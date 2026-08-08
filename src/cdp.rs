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
