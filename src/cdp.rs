//! Chrome DevTools Protocol 底层连接
//!
//! 通过 HTTP 发现标签页，通过 WebSocket 发送 CDP 命令

use anyhow::{anyhow, bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{oneshot, Mutex};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, info, trace, warn};

/// 从 Chrome 调试端口获取的标签页信息
#[derive(Debug, Clone, Deserialize)]
pub struct TabInfo {
    pub id: String,
    #[serde(rename = "type")]
    pub tab_type: String,
    pub title: String,
    pub url: String,
    #[serde(rename = "webSocketDebuggerUrl")]
    pub ws_url: String,
}

/// 通过 HTTP 发现所有标签页
pub async fn discover_tabs(port: u16) -> Result<Vec<TabInfo>> {
    let url = format!("http://localhost:{}/json", port);
    debug!("发现标签页: {}", url);
    let resp = reqwest::get(&url)
        .await
        .context("无法连接 Chrome 调试端口")?;
    let tabs: Vec<TabInfo> = resp.json().await?;
    Ok(tabs.into_iter().filter(|t| t.tab_type == "page").collect())
}

/// 测试 Chrome 调试端口是否可达
pub async fn check_reachable(port: u16) -> Result<()> {
    let url = format!("http://localhost:{}/json/version", port);
    reqwest::get(&url).await.context(format!(
        "无法连接到 Chrome 调试端口 (端口 {}).请确保 Chrome 已用 --remote-debugging-port={} 启动",
        port, port
    ))?;
    Ok(())
}

/// WebSocket 流类型别名 (简化泛型)
type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsSink = futures_util::stream::SplitSink<WsStream, Message>;

/// 到单个标签页的 CDP WebSocket 会话
pub struct CdpSession {
    ws_write: Mutex<WsSink>,
    msg_id: Mutex<u32>,
    /// 等待 CDP 响应的 pending 请求 — 用 Arc 以便 spawn 的任务也能访问
    pending: Arc<Mutex<HashMap<u32, oneshot::Sender<Value>>>>,
}

impl CdpSession {
    /// 连接到标签页的 WebSocket
    pub async fn connect(ws_url: &str) -> Result<Self> {
        debug!("连接 CDP: {}", ws_url);
        let (ws_stream, _) = connect_async(ws_url).await.context("WebSocket 连接失败")?;

        let (write, mut read) = ws_stream.split();
        let pending: Arc<Mutex<HashMap<u32, oneshot::Sender<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let session = Self {
            ws_write: Mutex::new(write),
            msg_id: Mutex::new(0),
            pending: pending.clone(),
        };

        // 启动接收循环
        tokio::spawn(async move {
            while let Some(msg) = read.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        let value: Value = match serde_json::from_str(&text) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                        // 如果有 id,说明是对某个请求的响应
                        if let Some(id) = value.get("id").and_then(|v| v.as_u64()) {
                            let id = id as u32;
                            let mut pending_map = pending.lock().await;
                            if let Some(sender) = pending_map.remove(&id) {
                                let _ = sender.send(value);
                            }
                        } else {
                            // 事件,暂时忽略
                            trace!("CDP 事件: {}", value);
                        }
                    }
                    Ok(Message::Close(_)) => {
                        warn!("CDP WebSocket 已关闭");
                        break;
                    }
                    Err(e) => {
                        warn!("CDP WebSocket 错误: {}", e);
                        break;
                    }
                    _ => {}
                }
            }
        });

        Ok(session)
    }

    /// 发送 CDP 命令并等待响应
    pub async fn send_command(&self, method: &str, params: Value) -> Result<Value> {
        let id = {
            let mut msg_id = self.msg_id.lock().await;
            *msg_id += 1;
            *msg_id
        };

        let command = json!({
            "id": id,
            "method": method,
            "params": params,
        });

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(id, tx);
        }

        // 发送命令
        {
            let mut write = self.ws_write.lock().await;
            write
                .send(Message::Text(command.to_string()))
                .await
                .context("发送 CDP 命令失败")?;
        }

        // 等待响应 (超时 30 秒)
        let result = tokio::time::timeout(Duration::from_secs(30), rx)
            .await
            .map_err(|_| anyhow!("CDP 命令超时 (30s): {}", method))?
            .map_err(|_| anyhow!("CDP 响应通道关闭"))?;

        // 检查错误
        if let Some(error) = result.get("error") {
            bail!("CDP 命令失败: {} - {}", method, error);
        }

        Ok(result.get("result").cloned().unwrap_or(Value::Null))
    }

    /// 在页面中执行 JavaScript 并返回结果
    pub async fn evaluate(&self, expression: &str) -> Result<Value> {
        let result = self
            .send_command(
                "Runtime.evaluate",
                json!({
                    "expression": expression,
                    "returnByValue": true,
                    "awaitPromise": true,
                }),
            )
            .await?;

        // 检查异常
        if let Some(exception) = result.get("exceptionDetails") {
            bail!("JS 执行异常: {}", exception);
        }

        let value = result
            .get("result")
            .and_then(|r| r.get("value"))
            .cloned()
            .unwrap_or(Value::Null);

        Ok(value)
    }

    /// 在页面中执行 JavaScript,返回字符串结果
    pub async fn evaluate_string(&self, expression: &str) -> Result<String> {
        let value = self.evaluate(expression).await?;
        match value {
            Value::String(s) => Ok(s),
            Value::Null => Ok(String::new()),
            other => Ok(other.to_string()),
        }
    }

    /// 在页面中执行 JavaScript,返回可能很长的字符串结果 (分块提取)
    ///
    /// 当 AI 回复很长 (如 10000+ 字符的 JSON 规划), CDP `Runtime.evaluate`
    /// 的 `returnByValue: true` 可能因为 WebSocket 消息大小限制导致截断。
    ///
    /// 本方法通过以下策略避免截断:
    /// 1. 先执行 JS 获取文本长度
    /// 2. 如果长度 > chunk_size (默认 50000), 分块提取
    /// 3. 每块通过 substring 截取, 单独返回
    /// 4. 拼接所有块
    ///
    /// 对于短文本 (<= chunk_size), 直接 evaluate_string 一次完成。
    pub async fn evaluate_string_long(&self, expression: &str) -> Result<String> {
        const CHUNK_SIZE: usize = 50000; // 50KB per chunk

        // 先执行表达式获取结果 (可能被截断)
        let first_try = self.evaluate_string(expression).await?;

        // 如果结果较短, 直接返回
        if first_try.len() < CHUNK_SIZE {
            return Ok(first_try);
        }

        // 结果较长, 可能被截断 — 使用分块提取
        // 1. 先获取真实长度
        let length_js = format!(
            "(() => {{ let r = ({}); return r ? r.length : 0; }})()",
            expression
        );
        let total_len = self
            .evaluate_string(&length_js)
            .await?
            .parse::<usize>()
            .unwrap_or(0);

        if total_len == 0 {
            return Ok(first_try);
        }

        // 2. 如果真实长度 <= first_try 长度, 说明没截断
        if total_len <= first_try.len() {
            return Ok(first_try);
        }

        // 3. 分块提取
        debug!(
            "分块提取长文本 (总长 {} 字符, 分 {} 块)",
            total_len,
            total_len.div_ceil(CHUNK_SIZE)
        );
        let mut result = String::with_capacity(total_len);
        let mut offset = 0usize;
        while offset < total_len {
            let end = (offset + CHUNK_SIZE).min(total_len);
            let chunk_js = format!(
                "(() => {{ let r = ({}); return r ? r.substring({}, {}) : ''; }})()",
                expression, offset, end
            );
            let chunk = self.evaluate_string(&chunk_js).await?;
            if chunk.is_empty() {
                warn!("分块提取: 块 {}-{} 返回空, 停止", offset, end);
                break;
            }
            result.push_str(&chunk);
            offset = end;
        }

        info!("✅ 分块提取完成 ({} 字符)", result.len());
        Ok(result)
    }

    /// 通过 CDP Input.insertText 插入文本 (模拟真实打字,所有框架兼容)
    pub async fn insert_text(&self, text: &str) -> Result<()> {
        self.send_command(
            "Input.insertText",
            json!({
                "text": text
            }),
        )
        .await?;
        Ok(())
    }

    /// 通过 CDP Input.dispatchKeyEvent 发送真实键盘事件
    pub async fn press_key(&self, key: &str, code: &str, key_code: u32) -> Result<()> {
        // keyDown
        self.send_command(
            "Input.dispatchKeyEvent",
            json!({
                "type": "keyDown",
                "key": key,
                "code": code,
                "windowsVirtualKeyCode": key_code,
                "nativeVirtualKeyCode": key_code,
            }),
        )
        .await?;

        // char (for printable keys)
        if key.len() == 1 {
            self.send_command(
                "Input.dispatchKeyEvent",
                json!({
                    "type": "char",
                    "key": key,
                    "code": code,
                    "windowsVirtualKeyCode": key_code,
                    "nativeVirtualKeyCode": key_code,
                    "text": key,
                }),
            )
            .await?;
        }

        // keyUp
        self.send_command(
            "Input.dispatchKeyEvent",
            json!({
                "type": "keyUp",
                "key": key,
                "code": code,
                "windowsVirtualKeyCode": key_code,
                "nativeVirtualKeyCode": key_code,
            }),
        )
        .await?;

        Ok(())
    }

    /// 通过 CDP Input.dispatchKeyEvent 发送文本 (逐字符)
    pub async fn type_text(&self, text: &str) -> Result<()> {
        for ch in text.chars() {
            if ch == '\n' {
                self.press_key("Enter", "Enter", 13).await?;
            } else {
                let s = ch.to_string();
                self.send_command(
                    "Input.dispatchKeyEvent",
                    json!({
                        "type": "char",
                        "key": s,
                        "text": s,
                    }),
                )
                .await?;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok(())
    }

    /// 通过 CDP Input.dispatchKeyEvent 发送 Enter 键 (用于提交消息)
    pub async fn press_enter(&self) -> Result<()> {
        self.press_key("Enter", "Enter", 13).await
    }

    /// 通过 CDP 聚焦元素
    pub async fn focus(&self, selector: &str) -> Result<()> {
        self.evaluate(&format!(
            "document.querySelector('{}')?.focus()",
            selector.replace('\'', "\\'")
        ))
        .await?;
        Ok(())
    }

    /// 通过 CDP 上传文件到 `<input type="file">` 元素
    ///
    /// 使用 `DOM.setFileInputFiles` CDP 命令将本地文件设置到文件输入元素。
    /// 流程:
    /// 1. 启用 DOM 域
    /// 2. 获取文档根节点
    /// 3. 通过选择器查找 `<input type="file">` 元素
    /// 4. 设置文件路径
    /// 5. 触发 `change` 事件通知前端框架
    ///
    /// # 参数
    /// - `selector`: 文件输入元素的 CSS 选择器 (如 `input[type="file"]`)
    /// - `file_paths`: 要上传的文件绝对路径列表
    ///
    /// # 多网站支持
    /// - Z.ai: `input[type="file"]` (隐藏的文件输入, 最多 10 个文件)
    /// - DeepSeek: `input[type="file"]` (上传按钮触发)
    /// - 通用: 所有使用 `<input type="file">` 的网站
    pub async fn set_file_input_files(&self, selector: &str, file_paths: &[&str]) -> Result<()> {
        // 1. 启用 DOM 域
        self.send_command("DOM.enable", json!({})).await?;

        // 2. 获取文档根节点
        let doc_result = self
            .send_command("DOM.getDocument", json!({ "depth": 0 }))
            .await?;
        let root_node_id = doc_result
            .get("root")
            .and_then(|r| r.get("nodeId"))
            .and_then(|n| n.as_u64())
            .ok_or_else(|| anyhow!("无法获取文档根节点 nodeId"))? as i64;

        // 3. 查找文件输入元素
        let query_result = self
            .send_command(
                "DOM.querySelector",
                json!({
                    "nodeId": root_node_id,
                    "selector": selector
                }),
            )
            .await?;

        let node_id = query_result
            .get("nodeId")
            .and_then(|n| n.as_u64())
            .ok_or_else(|| anyhow!("无法找到文件输入元素: {}", selector))?
            as i64;

        if node_id == 0 {
            bail!("文件输入元素不存在: {}", selector);
        }

        // 4. 构建文件路径数组
        let files: Vec<&str> = file_paths.to_vec();

        // 5. 设置文件
        self.send_command(
            "DOM.setFileInputFiles",
            json!({
                "nodeId": node_id,
                "files": files
            }),
        )
        .await?;

        // 6. 触发 change 事件 (通知 Svelte/React 等框架)
        self.evaluate(&format!(
            r#"
            (() => {{
                let input = document.querySelector('{}');
                if (input) {{
                    input.dispatchEvent(new Event('change', {{ bubbles: true }}));
                    input.dispatchEvent(new Event('input', {{ bubbles: true }}));
                }}
            }})()
            "#,
            selector.replace('\'', "\\'")
        ))
        .await?;

        debug!("文件上传完成: {} 个文件 -> {}", files.len(), selector);
        Ok(())
    }

    /// 等待直到条件为真 (轮询)
    pub async fn wait_for_condition(
        &self,
        condition_js: &str,
        timeout_ms: u64,
        poll_interval_ms: u64,
    ) -> Result<()> {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);

        loop {
            if tokio::time::Instant::now() > deadline {
                bail!("等待条件超时 ({}ms): {}", timeout_ms, condition_js);
            }

            let result = self
                .evaluate(&format!(
                    "(() => {{ try {{ return {}; }} catch(e) {{ return false; }} }})()",
                    condition_js
                ))
                .await?;

            let is_true = match &result {
                Value::Bool(b) => *b,
                Value::String(s) => !s.is_empty(),
                Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
                Value::Null => false,
                _ => true,
            };

            if is_true {
                return Ok(());
            }

            tokio::time::sleep(Duration::from_millis(poll_interval_ms)).await;
        }
    }
}

// ============================================================================
//  纯逻辑函数 — 从 CdpSession 提取的可测试函数
// ============================================================================

/// 将 CDP 响应中的 `Value` 转换为布尔值 (用于 `wait_for_condition` 轮询)
///
/// 转换规则:
/// - `Bool(b)` → b
/// - `String(s)` → !s.is_empty()
/// - `Number(n)` → n != 0.0
/// - `Null` → false
/// - 其他 (Array/Object) → true (非空容器视为真)
pub fn value_as_bool(result: &Value) -> bool {
    match result {
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::Null => false,
        _ => true,
    }
}

/// 判断按键是否为可打印字符 (决定 `press_key` 是否发送 char 事件)
///
/// CDP `Input.dispatchKeyEvent` 的 `char` 事件仅对单字节可打印字符发送。
/// 多字节键名 (如 "Enter", "Tab", "Escape") 不需要 char 事件。
pub fn is_printable_key(key: &str) -> bool {
    key.len() == 1
}

/// 判断 `evaluate_string_long` 是否需要对结果进行分块提取
///
/// 当首次 `evaluate_string` 返回的长度 >= CHUNK_SIZE 时, 可能被截断, 需要分块。
pub fn needs_chunking(first_result_len: usize, chunk_size: usize) -> bool {
    first_result_len >= chunk_size
}

/// 判断首次结果是否完整 (真实长度 <= 首次返回长度, 说明没截断)
pub fn is_result_complete(total_len: usize, first_result_len: usize) -> bool {
    total_len == 0 || total_len <= first_result_len
}

/// 构建 JS 表达式: 获取表达式的结果长度
pub fn build_length_js(expression: &str) -> String {
    format!(
        "(() => {{ let r = ({}); return r ? r.length : 0; }})()",
        expression
    )
}

/// 构建 JS 表达式: 分块提取 substring
pub fn build_chunk_js(expression: &str, offset: usize, end: usize) -> String {
    format!(
        "(() => {{ let r = ({}); return r ? r.substring({}, {}) : ''; }})()",
        expression, offset, end
    )
}

/// 构建轮询条件 JS (try-catch 包装)
pub fn build_condition_js(condition_js: &str) -> String {
    format!(
        "(() => {{ try {{ return {}; }} catch(e) {{ return false; }} }})()",
        condition_js
    )
}

/// 构建聚焦元素 JS
pub fn build_focus_js(selector: &str) -> String {
    format!(
        "document.querySelector('{}')?.focus()",
        selector.replace('\'', "\\'")
    )
}

/// 从 CDP 响应中提取结果 (检查错误)
pub fn extract_result(response: &Value, method: &str) -> Result<Value> {
    if let Some(error) = response.get("error") {
        bail!("CDP 命令失败: {} - {}", method, error);
    }
    Ok(response.get("result").cloned().unwrap_or(Value::Null))
}

/// 从 `Runtime.evaluate` 结果中提取值
pub fn extract_evaluate_value(result: &Value) -> Result<Value> {
    if let Some(exception) = result.get("exceptionDetails") {
        bail!("JS 执行异常: {}", exception);
    }
    Ok(result
        .get("result")
        .and_then(|r| r.get("value"))
        .cloned()
        .unwrap_or(Value::Null))
}

/// 将 `Value` 转换为字符串 (用于 `evaluate_string`)
pub fn value_to_string(value: Value) -> String {
    match value {
        Value::String(s) => s,
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// 从 DOM 响应中提取 nodeId
pub fn extract_node_id(response: &Value, error_msg: &str) -> Result<i64> {
    response
        .get("nodeId")
        .and_then(|n| n.as_u64())
        .ok_or_else(|| anyhow!("{}", error_msg))?
        .try_into()
        .map_err(|_| anyhow!("nodeId 超出 i64 范围: {}", error_msg))
}

/// 从文档响应中提取根节点 nodeId
pub fn extract_root_node_id(response: &Value) -> Result<i64> {
    response
        .get("root")
        .and_then(|r| r.get("nodeId"))
        .and_then(|n| n.as_u64())
        .ok_or_else(|| anyhow!("无法获取文档根节点 nodeId"))?
        .try_into()
        .map_err(|_| anyhow!("root nodeId 超出 i64 范围"))
}

/// 构建 CDP 命令 JSON
pub fn build_command(id: u32, method: &str, params: Value) -> Value {
    json!({
        "id": id,
        "method": method,
        "params": params,
    })
}

/// 从 WebSocket 文本消息中提取 CDP 响应 ID
///
/// 如果消息是 CDP 响应 (包含 `id` 字段), 返回对应的 id。
/// 如果是事件 (无 `id`), 返回 None。
pub fn extract_response_id(text: &str) -> Option<u32> {
    let value: Value = serde_json::from_str(text).ok()?;
    value.get("id").and_then(|v| v.as_u64()).map(|id| id as u32)
}

/// 构建文件上传 change 事件触发 JS
pub fn build_file_change_js(selector: &str) -> String {
    let escaped = selector.replace('\'', "\\'");
    format!(
        r#"
        (() => {{
            let input = document.querySelector('{}');
            if (input) {{
                input.dispatchEvent(new Event('change', {{ bubbles: true }}));
                input.dispatchEvent(new Event('input', {{ bubbles: true }}));
            }}
        }})()
        "#,
        escaped
    )
}

/// 从 browser version 响应中提取 webSocketDebuggerUrl
pub fn extract_browser_ws_url(version_response: &Value) -> Result<&str> {
    version_response
        .get("webSocketDebuggerUrl")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("无法获取 browser WebSocket URL"))
}

/// 从 Target.createTarget 响应中提取 targetId
pub fn extract_target_id(response: &Value) -> Result<&str> {
    response
        .get("result")
        .and_then(|r| r.get("targetId"))
        .and_then(|t| t.as_str())
        .ok_or_else(|| anyhow::anyhow!("无法获取 targetId"))
}

/// 通过 browser-level CDP 创建新标签页
pub async fn create_tab(port: u16, url: &str) -> Result<TabInfo> {
    // 先获取 browser 的 ws url
    let version_url = format!("http://localhost:{}/json/version", port);
    let resp: serde_json::Value = reqwest::get(&version_url).await?.json().await?;
    let browser_ws = resp
        .get("webSocketDebuggerUrl")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("无法获取 browser WebSocket URL"))?;

    // 连接 browser ws
    let (ws_stream, _) = connect_async(browser_ws).await?;
    let (mut write, mut read) = ws_stream.split();

    // 发 Target.createTarget
    let cmd = serde_json::json!({
        "id": 1,
        "method": "Target.createTarget",
        "params": { "url": url }
    });
    write.send(Message::Text(cmd.to_string())).await?;

    // 读响应
    while let Some(msg) = read.next().await {
        if let Ok(Message::Text(text)) = msg {
            let v: serde_json::Value = serde_json::from_str(&text)?;
            if v.get("id").and_then(|i| i.as_u64()) == Some(1) {
                let target_id = v
                    .get("result")
                    .and_then(|r| r.get("targetId"))
                    .and_then(|t| t.as_str())
                    .ok_or_else(|| anyhow::anyhow!("无法获取 targetId"))?;

                // 获取新标签页信息
                let tabs = discover_tabs(port).await?;
                let tab = tabs
                    .into_iter()
                    .find(|t| t.id == target_id)
                    .ok_or_else(|| anyhow::anyhow!("标签页已创建但未在列表中找到"))?;
                return Ok(tab);
            }
        }
    }
    anyhow::bail!("创建标签页失败: 无响应")
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ===== TabInfo 反序列化 =====

    #[test]
    fn test_tab_info_deserialize() {
        let json_str = r#"{
            "id": "ABC123",
            "type": "page",
            "title": "Chat",
            "url": "https://chat.z.ai",
            "webSocketDebuggerUrl": "ws://localhost:9222/devtools/page/ABC123"
        }"#;
        let tab: TabInfo = serde_json::from_str(json_str).unwrap();
        assert_eq!(tab.id, "ABC123");
        assert_eq!(tab.tab_type, "page");
        assert_eq!(tab.title, "Chat");
        assert_eq!(tab.url, "https://chat.z.ai");
        assert_eq!(tab.ws_url, "ws://localhost:9222/devtools/page/ABC123");
    }

    #[test]
    fn test_tab_info_type_filter() {
        let tabs_json = r#"[
            {"id":"1","type":"page","title":"A","url":"http://a","webSocketDebuggerUrl":"ws://1"},
            {"id":"2","type":"background_page","title":"B","url":"http://b","webSocketDebuggerUrl":"ws://2"},
            {"id":"3","type":"page","title":"C","url":"http://c","webSocketDebuggerUrl":"ws://3"}
        ]"#;
        let tabs: Vec<TabInfo> = serde_json::from_str(tabs_json).unwrap();
        let pages: Vec<_> = tabs.into_iter().filter(|t| t.tab_type == "page").collect();
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].id, "1");
        assert_eq!(pages[1].id, "3");
    }

    #[test]
    fn test_tab_info_missing_fields() {
        // TabInfo 需要 id 字段，缺少时报错
        let json_str = r#"{"type":"page"}"#;
        let result: Result<TabInfo, _> = serde_json::from_str(json_str);
        assert!(result.is_err());
    }

    // ===== value_as_bool =====

    #[test]
    fn test_value_as_bool_true() {
        assert!(value_as_bool(&json!(true)));
    }

    #[test]
    fn test_value_as_bool_false() {
        assert!(!value_as_bool(&json!(false)));
    }

    #[test]
    fn test_value_as_bool_non_empty_string() {
        assert!(value_as_bool(&json!("hello")));
    }

    #[test]
    fn test_value_as_bool_empty_string() {
        assert!(!value_as_bool(&json!("")));
    }

    #[test]
    fn test_value_as_bool_nonzero_number() {
        assert!(value_as_bool(&json!(42)));
        assert!(value_as_bool(&json!(-1)));
    }

    #[test]
    fn test_value_as_bool_zero() {
        assert!(!value_as_bool(&json!(0)));
        assert!(!value_as_bool(&json!(0.0)));
    }

    #[test]
    fn test_value_as_bool_null() {
        assert!(!value_as_bool(&Value::Null));
    }

    #[test]
    fn test_value_as_bool_array() {
        assert!(value_as_bool(&json!([])));
        assert!(value_as_bool(&json!([1, 2])));
    }

    #[test]
    fn test_value_as_bool_object() {
        assert!(value_as_bool(&json!({})));
        assert!(value_as_bool(&json!({"key": "val"})));
    }

    // ===== is_printable_key =====

    #[test]
    fn test_is_printable_key_single_char() {
        assert!(is_printable_key("a"));
        assert!(is_printable_key("Z"));
        assert!(is_printable_key("1"));
    }

    #[test]
    fn test_is_printable_key_multi_char() {
        assert!(!is_printable_key("Enter"));
        assert!(!is_printable_key("Tab"));
        assert!(!is_printable_key("Escape"));
        assert!(!is_printable_key("Shift"));
    }

    // ===== needs_chunking =====

    #[test]
    fn test_needs_chunking_short_result() {
        assert!(!needs_chunking(100, 50000));
    }

    #[test]
    fn test_needs_chunking_exact_boundary() {
        assert!(needs_chunking(50000, 50000));
    }

    #[test]
    fn test_needs_chunking_long_result() {
        assert!(needs_chunking(100000, 50000));
    }

    #[test]
    fn test_needs_chunking_zero_length() {
        assert!(!needs_chunking(0, 50000));
    }

    // ===== is_result_complete =====

    #[test]
    fn test_is_result_complete_zero_total() {
        assert!(is_result_complete(0, 100));
    }

    #[test]
    fn test_is_result_complete_equal() {
        assert!(is_result_complete(100, 100));
    }

    #[test]
    fn test_is_result_complete_shorter() {
        assert!(is_result_complete(50, 100));
    }

    #[test]
    fn test_is_result_complete_longer() {
        assert!(!is_result_complete(200, 100));
    }

    // ===== build_length_js =====

    #[test]
    fn test_build_length_js() {
        let js = build_length_js("document.body.innerText");
        assert!(js.contains("document.body.innerText"));
        assert!(js.contains("r.length"));
    }

    #[test]
    fn test_build_length_js_returns_zero_on_null() {
        let js = build_length_js("null");
        assert!(js.contains("r ? r.length : 0"));
    }

    // ===== build_chunk_js =====

    #[test]
    fn test_build_chunk_js() {
        let js = build_chunk_js("document.body.innerText", 0, 50000);
        assert!(js.contains("substring(0, 50000)"));
    }

    #[test]
    fn test_build_chunk_js_with_offset() {
        let js = build_chunk_js("getResult()", 50000, 100000);
        assert!(js.contains("substring(50000, 100000)"));
    }

    // ===== build_condition_js =====

    #[test]
    fn test_build_condition_js() {
        let js = build_condition_js("document.querySelector('#btn')");
        assert!(js.contains("try"));
        assert!(js.contains("catch"));
        assert!(js.contains("document.querySelector('#btn')"));
    }

    // ===== build_focus_js =====

    #[test]
    fn test_build_focus_js() {
        let js = build_focus_js("#input");
        assert!(js.contains("document.querySelector"));
        assert!(js.contains("#input"));
        assert!(js.contains("focus()"));
    }

    #[test]
    fn test_build_focus_js_escapes_quotes() {
        let js = build_focus_js("input[name='test']");
        assert!(js.contains("input[name=\\'test\\']"));
    }

    // ===== extract_result =====

    #[test]
    fn test_extract_result_success() {
        let response = json!({
            "id": 1,
            "result": {"value": 42}
        });
        let result = extract_result(&response, "Runtime.evaluate").unwrap();
        assert_eq!(result["value"], json!(42));
    }

    #[test]
    fn test_extract_result_null_result() {
        let response = json!({"id": 1});
        let result = extract_result(&response, "Test").unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn test_extract_result_error() {
        let response = json!({
            "id": 1,
            "error": {"message": "Method not found"}
        });
        let result = extract_result(&response, "Unknown.method");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unknown.method"));
    }

    // ===== extract_evaluate_value =====

    #[test]
    fn test_extract_evaluate_value_success() {
        let result = json!({
            "result": {"value": "hello"}
        });
        let value = extract_evaluate_value(&result).unwrap();
        assert_eq!(value, json!("hello"));
    }

    #[test]
    fn test_extract_evaluate_value_exception() {
        let result = json!({
            "result": {"value": null},
            "exceptionDetails": {"text": "SyntaxError"}
        });
        let value = extract_evaluate_value(&result);
        assert!(value.is_err());
        assert!(value.unwrap_err().to_string().contains("JS 执行异常"));
    }

    #[test]
    fn test_extract_evaluate_value_missing_value() {
        let result = json!({"result": {}});
        let value = extract_evaluate_value(&result).unwrap();
        assert_eq!(value, Value::Null);
    }

    #[test]
    fn test_extract_evaluate_value_missing_result() {
        let result = json!({});
        let value = extract_evaluate_value(&result).unwrap();
        assert_eq!(value, Value::Null);
    }

    // ===== value_to_string =====

    #[test]
    fn test_value_to_string_string() {
        assert_eq!(value_to_string(json!("hello")), "hello");
    }

    #[test]
    fn test_value_to_string_null() {
        assert_eq!(value_to_string(Value::Null), "");
    }

    #[test]
    fn test_value_to_string_number() {
        assert_eq!(value_to_string(json!(42)), "42");
    }

    #[test]
    fn test_value_to_string_bool() {
        assert_eq!(value_to_string(json!(true)), "true");
    }

    #[test]
    fn test_value_to_string_array() {
        let result = value_to_string(json!([1, 2, 3]));
        assert!(result.contains("1"));
        assert!(result.contains("2"));
        assert!(result.contains("3"));
    }

    // ===== extract_node_id =====

    #[test]
    fn test_extract_node_id_success() {
        let response = json!({"nodeId": 42});
        let id = extract_node_id(&response, "not found").unwrap();
        assert_eq!(id, 42);
    }

    #[test]
    fn test_extract_node_id_missing() {
        let response = json!({});
        let result = extract_node_id(&response, "element not found");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("element not found"));
    }

    #[test]
    fn test_extract_node_id_zero() {
        let response = json!({"nodeId": 0});
        let id = extract_node_id(&response, "err").unwrap();
        assert_eq!(id, 0);
    }

    // ===== extract_root_node_id =====

    #[test]
    fn test_extract_root_node_id_success() {
        let response = json!({"root": {"nodeId": 1}});
        let id = extract_root_node_id(&response).unwrap();
        assert_eq!(id, 1);
    }

    #[test]
    fn test_extract_root_node_id_missing() {
        let response = json!({});
        let result = extract_root_node_id(&response);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("无法获取文档根节点"));
    }

    // ===== build_command =====

    #[test]
    fn test_build_command() {
        let cmd = build_command(1, "Runtime.evaluate", json!({"expression": "1+1"}));
        assert_eq!(cmd["id"], 1);
        assert_eq!(cmd["method"], "Runtime.evaluate");
        assert_eq!(cmd["params"]["expression"], "1+1");
    }

    #[test]
    fn test_build_command_empty_params() {
        let cmd = build_command(5, "DOM.enable", json!({}));
        assert_eq!(cmd["id"], 5);
        assert_eq!(cmd["method"], "DOM.enable");
    }

    // ===== extract_response_id =====

    #[test]
    fn test_extract_response_id_with_id() {
        let text = r#"{"id": 42, "result": {"value": "ok"}}"#;
        assert_eq!(extract_response_id(text), Some(42));
    }

    #[test]
    fn test_extract_response_id_event_no_id() {
        let text = r#"{"method": "Page.frameNavigated", "params": {}}"#;
        assert_eq!(extract_response_id(text), None);
    }

    #[test]
    fn test_extract_response_id_invalid_json() {
        assert_eq!(extract_response_id("not json"), None);
    }

    #[test]
    fn test_extract_response_id_empty_string() {
        assert_eq!(extract_response_id(""), None);
    }

    #[test]
    fn test_extract_response_id_large_id() {
        let text = r#"{"id": 999999, "result": null}"#;
        assert_eq!(extract_response_id(text), Some(999999));
    }

    // ===== build_file_change_js =====

    #[test]
    fn test_build_file_change_js() {
        let js = build_file_change_js("input[type='file']");
        assert!(js.contains("change"));
        assert!(js.contains("input"));
        assert!(js.contains("dispatchEvent"));
    }

    #[test]
    fn test_build_file_change_js_escapes_quotes() {
        let js = build_file_change_js("input[name='test']");
        assert!(js.contains("input[name=\\'test\\']"));
    }

    // ===== extract_browser_ws_url =====

    #[test]
    fn test_extract_browser_ws_url_success() {
        let response = json!({
            "webSocketDebuggerUrl": "ws://localhost:9222/devtools/browser/abc"
        });
        let url = extract_browser_ws_url(&response).unwrap();
        assert_eq!(url, "ws://localhost:9222/devtools/browser/abc");
    }

    #[test]
    fn test_extract_browser_ws_url_missing() {
        let response = json!({});
        let result = extract_browser_ws_url(&response);
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_browser_ws_url_not_string() {
        let response = json!({"webSocketDebuggerUrl": 123});
        let result = extract_browser_ws_url(&response);
        assert!(result.is_err());
    }

    // ===== extract_target_id =====

    #[test]
    fn test_extract_target_id_success() {
        let response = json!({
            "result": {"targetId": "target-123"}
        });
        let id = extract_target_id(&response).unwrap();
        assert_eq!(id, "target-123");
    }

    #[test]
    fn test_extract_target_id_missing() {
        let response = json!({});
        let result = extract_target_id(&response);
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_target_id_no_result_key() {
        let response = json!({"id": 1});
        let result = extract_target_id(&response);
        assert!(result.is_err());
    }
}
