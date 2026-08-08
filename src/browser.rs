//! 浏览器管理器 — 发现聊天标签页,自动探测页面元素

use crate::cdp::{self, CdpSession, TabInfo};
use crate::chat::TimeoutConfig;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::sync::atomic::AtomicUsize;
use tracing::{debug, info, warn};

// ============================================================================
//  SiteType — 多网站类型识别
// ============================================================================

/// AI 聊天网站类型 — 用于驱动网站特定行为
///
/// 通过 URL 自动检测, 影响:
/// - `extract_last_response`: 不同网站的 DOM 结构不同
/// - `start_new_conversation`: 新开对话时导航到不同 URL
/// - `get_assistant_count`: 不同网站的 AI 消息容器选择器不同
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SiteType {
    /// chat.z.ai — GLM 系列聊天
    /// DOM: textarea#chat-input + button#send-message-button + .chat-assistant
    Zai,
    /// chat.deepseek.com — DeepSeek 聊天
    /// DOM: textarea (无 id) + div.ds-button (非 button!) + [class*="markdown"]
    DeepSeek,
    /// kimi.moonshot.cn — Kimi 聊天 (待适配)
    Kimi,
    /// tongyi.aliyun.com — 通义千问 (待适配)
    Tongyi,
    /// claude.ai — Claude (ProseMirror 编辑器, Enter 发送)
    /// DOM: div.tiptap.ProseMirror (contenteditable) + Enter 发送
    Claude,
    /// 未知网站 — 使用通用策略 (fallback)
    #[default]
    Unknown,
}

impl SiteType {
    /// 从 URL 自动检测网站类型
    pub fn detect(url: &str) -> Self {
        let lower = url.to_lowercase();
        if lower.contains("chat.z.ai") || lower.contains("z.ai") {
            Self::Zai
        } else if lower.contains("deepseek.com") {
            Self::DeepSeek
        } else if lower.contains("kimi") || lower.contains("moonshot") {
            Self::Kimi
        } else if lower.contains("tongyi") || lower.contains("aliyun") {
            Self::Tongyi
        } else if lower.contains("claude.ai") || lower.contains("claude") {
            Self::Claude
        } else {
            Self::Unknown
        }
    }

    /// 获取新开对话的 URL
    pub fn new_conversation_url(&self) -> &str {
        match self {
            Self::Zai => "https://chat.z.ai/",
            Self::DeepSeek => "https://chat.deepseek.com/",
            Self::Kimi => "https://kimi.moonshot.cn/",
            Self::Tongyi => "https://tongyi.aliyun.com/",
            Self::Claude => "https://claude.ai/new",
            Self::Unknown => "https://chat.z.ai/",
        }
    }

    /// 获取页面就绪检测的 JS 条件 (新开对话后等待此条件为 true)
    pub fn page_ready_condition(&self) -> &str {
        match self {
            Self::Zai => {
                r#"(document.querySelector('#chat-input') ||
                   document.querySelector('textarea')) !== null"#
            }
            Self::DeepSeek => {
                r#"(document.querySelector('textarea') ||
                   document.querySelector('[class*="ds-scroll-area"]')) !== null"#
            }
            Self::Kimi => {
                r#"(document.querySelector('textarea') ||
                   document.querySelector('[contenteditable="true"]')) !== null"#
            }
            Self::Tongyi => {
                r#"(document.querySelector('textarea') ||
                   document.querySelector('[contenteditable="true"]')) !== null"#
            }
            Self::Claude => {
                r#"(document.querySelector('.ProseMirror') ||
                   document.querySelector('[contenteditable="true"]')) !== null"#
            }
            Self::Unknown => {
                r#"(document.querySelector('textarea') ||
                   document.querySelector('#chat-input')) !== null"#
            }
        }
    }

    /// 是否为已知网站 (非 Unknown)
    pub fn is_known(&self) -> bool {
        !matches!(self, Self::Unknown)
    }

    /// 网站显示名称
    pub fn display_name(&self) -> &str {
        match self {
            Self::Zai => "Z.ai",
            Self::DeepSeek => "DeepSeek",
            Self::Kimi => "Kimi",
            Self::Tongyi => "通义千问",
            Self::Claude => "Claude",
            Self::Unknown => "未知网站",
        }
    }
}

impl std::fmt::Display for SiteType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

/// 一个被发现的聊天标签页
pub struct ChatTab {
    pub info: TabInfo,
    pub session: CdpSession,
    pub elements: ChatElements,
    pub title: String,
    pub url: String,
    /// 网站类型 — 自动检测, 驱动网站特定行为
    pub site_type: SiteType,
    /// 当前对话轮数 — 用于上下文衔接 (借鉴方向 1)
    ///
    /// 每次 `send_message` 后 +1, `start_new_conversation` 后清零。
    /// 使用 AtomicUsize 实现线程安全的原子操作。
    pub turn_count: AtomicUsize,
    /// 流式响应超时配置 (24h 可靠性) — 可配置的三阶段超时 + 卡死检测
    ///
    /// 默认使用 `TimeoutConfig::default()` (Phase1=10s, Phase2=60s, Phase3=30s, Stuck=120s)。
    /// 可通过 CLI `--stuck-threshold` 等参数自定义。
    pub timeout_config: TimeoutConfig,
}

/// 探测到的聊天页面元素选择器
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatElements {
    pub input_selector: String,
    pub send_selector: String,
    pub ai_message_selector: String,
    pub stop_button_selector: Option<String>,
}

impl Default for ChatElements {
    fn default() -> Self {
        Self {
            input_selector: "textarea".to_string(),
            send_selector: "button[type='submit']".to_string(),
            ai_message_selector: ".chat-assistant, [class*='assistant']".to_string(),
            stop_button_selector: None,
        }
    }
}

/// 浏览器管理器
pub struct BrowserManager {
    pub port: u16,
    pub tabs: Vec<ChatTab>,
}

impl BrowserManager {
    pub fn new(port: u16) -> Self {
        Self { port, tabs: vec![] }
    }

    /// 连接到 Chrome,发现所有聊天标签页
    pub async fn discover_and_connect(&mut self) -> Result<()> {
        cdp::check_reachable(self.port).await?;
        info!("Chrome 调试端口 {} 可达", self.port);

        let all_tabs = cdp::discover_tabs(self.port).await?;
        info!("发现 {} 个标签页", all_tabs.len());

        for tab_info in &all_tabs {
            if !Self::looks_like_chat(&tab_info.url, &tab_info.title) {
                debug!("跳过非聊天标签: {} ({})", tab_info.title, tab_info.url);
                continue;
            }

            info!("连接聊天标签: {} ({})", tab_info.title, tab_info.url);

            match CdpSession::connect(&tab_info.ws_url).await {
                Ok(session) => {
                    // 先检测网站类型 — Unknown 网站跳过元素探测 (避免 30s 超时)
                    let site_type = SiteType::detect(&tab_info.url);
                    info!("网站类型: {} ({})", site_type, tab_info.url);

                    let elements = if site_type.is_known() {
                        match Self::probe_elements(&session).await {
                            Ok(e) => {
                                debug!("探测到元素: {:?}", e);
                                e
                            }
                            Err(e) => {
                                warn!("元素探测失败,使用默认值: {}", e);
                                ChatElements::default()
                            }
                        }
                    } else {
                        debug!("未知网站, 跳过元素探测, 使用默认值");
                        ChatElements::default()
                    };

                    self.tabs.push(ChatTab {
                        info: tab_info.clone(),
                        session,
                        elements,
                        title: tab_info.title.clone(),
                        url: tab_info.url.clone(),
                        site_type,
                        turn_count: AtomicUsize::new(0),
                        timeout_config: TimeoutConfig::default(),
                    });
                }
                Err(e) => {
                    warn!("连接标签页失败 {}: {}", tab_info.url, e);
                }
            }
        }

        if self.tabs.is_empty() {
            bail!(
                "没有发现聊天标签页。请在 Chrome 中打开聊天网页 (如 https://chat.z.ai)\n\
                 提示: Chrome 需要用 --remote-debugging-port={} 启动",
                self.port
            );
        }

        info!("成功连接 {} 个聊天标签页", self.tabs.len());
        Ok(())
    }

    /// 启发式判断是否是聊天页面
    ///
    /// 注意: "ai" 关键词已移除 — 太宽泛, 会匹配 YouTube/GitHub 等非聊天页面
    /// (如 "AIProg", "OpenMOSS" 等), 导致 30s 元素探测超时
    pub fn looks_like_chat(url: &str, title: &str) -> bool {
        let combined = format!("{} {}", url, title).to_lowercase();
        const KEYWORDS: &[&str] = &[
            "chat", "gpt", "claude", "gemini", "z.ai", "glm", "deepseek", "kimi", "moonshot",
            "qwen", "tongyi", "aliyun", "doubao", "豆包", "文心", "ernie", "yiyan", "对话", "助手",
            "poe",
        ];
        // 排除官网首页等非聊天页面
        const EXCLUDE_PATTERNS: &[&str] = &[
            "www.deepseek.com/", // DeepSeek 官网首页 (不是 chat.deepseek.com)
        ];
        // 先检查排除模式
        if EXCLUDE_PATTERNS
            .iter()
            .any(|pat| url.to_lowercase().contains(pat))
        {
            return false;
        }
        KEYWORDS.iter().any(|kw| combined.contains(kw))
    }

    /// 自动探测页面的聊天元素 — 一次性探测所有元素
    async fn probe_elements(session: &CdpSession) -> Result<ChatElements> {
        // 一次性探测所有元素,减少 CDP 往返
        let result = session.evaluate_string(
            r#"
            (() => {
                const result = {
                    input: '',
                    send: '',
                    ai_message: '',
                    stop_button: '',
                };

                // === 1. 找输入框 ===
                const inputCandidates = [
                    ...document.querySelectorAll('textarea'),
                    ...document.querySelectorAll('[contenteditable="true"]'),
                    ...document.querySelectorAll('[role="textbox"]'),
                ];
                let bestInput = null;
                let bestInputScore = 0;
                for (const el of inputCandidates) {
                    const rect = el.getBoundingClientRect();
                    const style = window.getComputedStyle(el);
                    if (style.display === 'none' || style.visibility === 'hidden') continue;
                    if (rect.width < 50 || rect.height < 15) continue;
                    // 打分: 面积 (封顶 50000) + 标签偏好 + ID 偏好
                    let score = Math.min(rect.width * rect.height, 50000);
                    // 强偏好: textarea 标签 (比 div[role=textbox] 更可靠, 支持 insertText)
                    if (el.tagName === 'TEXTAREA') score += 50000;
                    // 强偏好: id 含 "input" (如 Z.ai 的 #chat-input)
                    if ((el.id || '').toLowerCase().includes('input')) score += 30000;
                    // 排除: contentEditable=false 的元素 (如 CodeMirror .cm-content)
                    if (el.contentEditable === 'false') score -= 40000;
                    if (score > bestInputScore) {
                        bestInputScore = score;
                        bestInput = el;
                    }
                }
                if (bestInput) {
                    if (bestInput.id) {
                        result.input = '#' + bestInput.id;
                    } else if (bestInput.className && typeof bestInput.className === 'string') {
                        const firstClass = bestInput.className.split(' ').filter(Boolean)[0];
                        if (firstClass) result.input = '.' + firstClass;
                    } else {
                        result.input = bestInput.tagName.toLowerCase();
                    }
                }

                // === 2. 找发送按钮 — 策略: 找输入框附近的提交按钮 ===
                // 先找输入框的父容器,再在容器内找按钮
                if (bestInput) {
                    // 向上查找输入框的容器 (最多 5 层)
                    let container = bestInput;
                    for (let i = 0; i < 5; i++) {
                        if (container.parentElement) {
                            container = container.parentElement;
                        }
                    }

                    // 在容器内找按钮 (包括 div 按钮 — DeepSeek 使用 div.ds-button)
                    const buttons = [
                        ...container.querySelectorAll('button'),
                        ...container.querySelectorAll('[class*="ds-button"]'),
                        ...container.querySelectorAll('[role="button"]'),
                    ].filter((v, i, a) => a.indexOf(v) === i); // 去重
                    let bestBtn = null;
                    let bestBtnScore = 0;
                    for (const btn of buttons) {
                        const rect = btn.getBoundingClientRect();
                        const style = window.getComputedStyle(btn);
                        if (style.display === 'none' || style.visibility === 'hidden') continue;
                        if (rect.width < 15 || rect.height < 15) continue;

                        const cls = (btn.className || '').toLowerCase();
                        const ariaLabel = (btn.getAttribute('aria-label') || '').toLowerCase();
                        const text = (btn.textContent || '').toLowerCase().trim();
                        const type = (btn.type || '').toLowerCase();
                        const tag = btn.tagName.toLowerCase();

                        let score = 0;
                        // 强偏好: id 含 "send" (如 Z.ai 的 #send-message-button)
                        if ((btn.id || '').toLowerCase().includes('send')) score += 50000;
                        // 强偏好: class 含 "sendMessageButton"
                        if (cls.includes('sendmessagebutton')) score += 30000;
                        // 强偏好: submit 类型
                        if (type === 'submit') score += 1000;
                        // 强偏好: 圆形按钮 (bg-black, rounded-full)
                        if (cls.includes('bg-black') || cls.includes('rounded-full')) score += 800;
                        if (cls.includes('bg-white') && cls.includes('rounded')) score += 600;
                        // DeepSeek: ds-button--primary + filled + circle
                        if (cls.includes('ds-button--primary') || cls.includes('ds-button--filled')) score += 900;
                        if (cls.includes('ds-button--circle')) score += 300;
                        // 偏好: send 相关
                        if (cls.includes('send')) score += 500;
                        if (ariaLabel.includes('send') || text.includes('发送')) score += 500;
                        // 排除: copy-code-button (Z.ai 代码复制按钮, class 含 bg-none + copy-code)
                        if (cls.includes('copy-code') || cls.includes('copy-response')) score -= 5000;
                        if (cls.includes('regenerate')) score -= 5000;
                        // 偏好: 在输入框右侧
                        const inputRect = bestInput.getBoundingClientRect();
                        if (Math.abs(rect.left - inputRect.right) < 100) score += 300;
                        // 偏好: 小按钮 (非 sidebar)
                        if (rect.width < 60 && rect.height < 60) score += 200;
                        // 排除: sidebar 按钮
                        if (cls.includes('sidebar')) score -= 500;
                        if (cls.includes('chat') && !cls.includes('send')) score -= 200;
                        // 排除: 登录/验证码按钮
                        if (text.includes('登录') || text.includes('验证码')) score -= 1000;
                        if (text.includes('新聊天') || text.includes('chat')) score -= 300;

                        if (score > bestBtnScore) {
                            bestBtnScore = score;
                            bestBtn = btn;
                        }
                    }

                    if (bestBtn) {
                        if (bestBtn.id) {
                            result.send = '#' + bestBtn.id;
                        } else {
                            // 构建更精确的选择器: tag + class 组合
                            const btnTag = bestBtn.tagName.toLowerCase();
                            const classes = (bestBtn.className || '').split(' ').filter(Boolean);
                            // 优先选择有意义的 class (排除通用布局 class 和哈希 class)
                            const significantClasses = classes.filter(c =>
                                !['flex', 'items-center', 'justify-center', 'p-2', 'p-1', 'p-0.5', 'p-1.5'].includes(c) &&
                                !c.startsWith('hover:') && !c.startsWith('dark:') && !c.startsWith('disabled:') &&
                                !c.startsWith('_') // 排除 DeepSeek 哈希 class
                            );
                            // 优先选择更具体的 class (含 -- 修饰符, 如 ds-button--primary)
                            const specificClass = significantClasses.find(c => c.includes('--')) || significantClasses[0];
                            if (specificClass) {
                                // 使用 [class*=] 选择器以确保匹配包含该 class 的元素
                                result.send = btnTag + '[class*="' + specificClass + '"]';
                            } else if (btnTag === 'button') {
                                result.send = 'button[type=submit]';
                            } else {
                                result.send = btnTag;
                            }
                        }
                    }
                }

                // 如果上面没找到, fallback: 找页面底部的按钮 (包括 div 按钮)
                if (!result.send) {
                    const allBtns = [
                        ...document.querySelectorAll('button'),
                        ...document.querySelectorAll('[class*="ds-button"]'),
                        ...document.querySelectorAll('[role="button"]'),
                    ].filter((v, i, a) => a.indexOf(v) === i);
                    let bestBtn = null;
                    let bestScore = 0;
                    for (const btn of allBtns) {
                        const rect = btn.getBoundingClientRect();
                        if (rect.width < 20 || rect.height < 20) continue;
                        const cls = (btn.className || '').toLowerCase();
                        let score = rect.bottom;
                        if (btn.type === 'submit') score += 5000;
                        if (cls.includes('bg-black') || cls.includes('rounded-full')) score += 800;
                        if (cls.includes('ds-button--primary') || cls.includes('ds-button--filled')) score += 900;
                        if (cls.includes('sidebar') || cls.includes('chat')) score -= 500;
                        if (score > bestScore) {
                            bestScore = score;
                            bestBtn = btn;
                        }
                    }
                    if (bestBtn) {
                        if (bestBtn.id) result.send = '#' + bestBtn.id;
                        else if (bestBtn.tagName.toLowerCase() === 'button') result.send = 'button[type=submit]';
                        else {
                            // div 按钮 fallback
                            const cls = (bestBtn.className || '').split(' ').filter(c => !c.startsWith('_'))[0];
                            result.send = cls ? bestBtn.tagName.toLowerCase() + '[class*="' + cls + '"]' : bestBtn.tagName.toLowerCase();
                        }
                    }
                }

                // === 3. 找 AI 消息容器 ===
                const msgSelectors = [
                    '.chat-assistant',
                    '[data-testid="assistant-message"]',
                    '.assistant-message',
                    '[class*="chat-assistant"]',
                    '[class*="assistant-message"]',
                    '[class*="bot-message"]',
                    '[class*="model-response"]',
                    '[class*="ai-message"]',
                ];
                for (const sel of msgSelectors) {
                    const el = document.querySelector(sel);
                    if (el && el.getBoundingClientRect().height > 0) {
                        result.ai_message = sel;
                        break;
                    }
                }
                if (!result.ai_message) {
                    // fallback: 找包含 markdown 内容的 div
                    const candidates = document.querySelectorAll('[class*="markdown"]');
                    for (const el of candidates) {
                        if (el.className.includes('assistant') || el.className.includes('chat-assistant')) {
                            result.ai_message = '.' + el.className.split(' ').filter(Boolean)[0];
                            break;
                        }
                    }
                }
                if (!result.ai_message) {
                    result.ai_message = '.chat-assistant, [class*="assistant"]';
                }

                // === 4. 找停止按钮 ===
                const stopSelectors = [
                    '[data-testid="stop-button"]',
                    'button[aria-label*="stop" i]',
                    'button[aria-label*="停止"]',
                    '[class*="stop-button"]',
                    '[class*="stop_generating"]',
                ];
                for (const sel of stopSelectors) {
                    if (document.querySelector(sel)) {
                        result.stop_button = sel;
                        break;
                    }
                }

                return JSON.stringify(result);
            })()
            "#,
        ).await?;

        // 解析 JSON 结果
        let parsed: serde_json::Value =
            serde_json::from_str(&result).unwrap_or(serde_json::json!({}));

        let input_selector = parsed
            .get("input")
            .and_then(|v| v.as_str())
            .unwrap_or("textarea")
            .to_string();
        let send_selector = parsed
            .get("send")
            .and_then(|v| v.as_str())
            .unwrap_or("button[type=submit]")
            .to_string();
        let ai_message_selector = parsed
            .get("ai_message")
            .and_then(|v| v.as_str())
            .unwrap_or(".chat-assistant, [class*='assistant']")
            .to_string();
        let stop_button_selector = parsed
            .get("stop_button")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        Ok(ChatElements {
            input_selector,
            send_selector,
            ai_message_selector,
            stop_button_selector,
        })
    }
}

// ============================================================================
//  单元测试 — SiteType + looks_like_chat + probe_elements 逻辑
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ===== SiteType::detect 测试 =====

    #[test]
    fn test_site_type_detect_zai() {
        assert_eq!(SiteType::detect("https://chat.z.ai/"), SiteType::Zai);
        assert_eq!(
            SiteType::detect("https://chat.z.ai/chat/abc123"),
            SiteType::Zai
        );
        assert_eq!(SiteType::detect("https://z.ai/some/path"), SiteType::Zai);
    }

    #[test]
    fn test_site_type_detect_deepseek() {
        assert_eq!(
            SiteType::detect("https://chat.deepseek.com/"),
            SiteType::DeepSeek
        );
        assert_eq!(
            SiteType::detect("https://chat.deepseek.com/chat/session1"),
            SiteType::DeepSeek
        );
        assert_eq!(
            SiteType::detect("https://deepseek.com/"),
            SiteType::DeepSeek
        );
    }

    #[test]
    fn test_site_type_detect_kimi() {
        assert_eq!(
            SiteType::detect("https://kimi.moonshot.cn/"),
            SiteType::Kimi
        );
        assert_eq!(
            SiteType::detect("https://kimi.moonshot.cn/chat/123"),
            SiteType::Kimi
        );
        assert_eq!(SiteType::detect("https://moonshot.cn/"), SiteType::Kimi);
    }

    #[test]
    fn test_site_type_detect_tongyi() {
        assert_eq!(
            SiteType::detect("https://tongyi.aliyun.com/"),
            SiteType::Tongyi
        );
        assert_eq!(
            SiteType::detect("https://tongyi.aliyun.com/qianwen/"),
            SiteType::Tongyi
        );
        assert_eq!(SiteType::detect("https://aliyun.com/bot"), SiteType::Tongyi);
    }

    #[test]
    fn test_site_type_detect_claude() {
        assert_eq!(SiteType::detect("https://claude.ai/"), SiteType::Claude);
        assert_eq!(SiteType::detect("https://claude.ai/new"), SiteType::Claude);
        assert_eq!(
            SiteType::detect("https://claude.ai/chat/abc123"),
            SiteType::Claude
        );
    }

    #[test]
    fn test_site_type_detect_unknown() {
        assert_eq!(SiteType::detect("https://example.com/"), SiteType::Unknown);
        assert_eq!(
            SiteType::detect("https://www.google.com/"),
            SiteType::Unknown
        );
        assert_eq!(SiteType::detect(""), SiteType::Unknown);
    }

    #[test]
    fn test_site_type_detect_case_insensitive() {
        assert_eq!(SiteType::detect("HTTPS://CHAT.Z.AI/"), SiteType::Zai);
        assert_eq!(
            SiteType::detect("https://Chat.DeepSeek.COM/"),
            SiteType::DeepSeek
        );
        assert_eq!(
            SiteType::detect("HTTPS://Kimi.Moonshot.CN/"),
            SiteType::Kimi
        );
    }

    // ===== SiteType 方法测试 =====

    #[test]
    fn test_site_type_new_conversation_url() {
        assert_eq!(SiteType::Zai.new_conversation_url(), "https://chat.z.ai/");
        assert_eq!(
            SiteType::DeepSeek.new_conversation_url(),
            "https://chat.deepseek.com/"
        );
        assert_eq!(
            SiteType::Kimi.new_conversation_url(),
            "https://kimi.moonshot.cn/"
        );
        assert_eq!(
            SiteType::Tongyi.new_conversation_url(),
            "https://tongyi.aliyun.com/"
        );
        assert_eq!(
            SiteType::Claude.new_conversation_url(),
            "https://claude.ai/new"
        );
        assert_eq!(
            SiteType::Unknown.new_conversation_url(),
            "https://chat.z.ai/"
        );
    }

    #[test]
    fn test_site_type_page_ready_condition() {
        // 所有条件应包含 textarea 检测 (通用回退)
        for site in [
            SiteType::Zai,
            SiteType::DeepSeek,
            SiteType::Kimi,
            SiteType::Tongyi,
            SiteType::Unknown,
        ] {
            let cond = site.page_ready_condition();
            assert!(
                cond.contains("textarea") || cond.contains("chat-input"),
                "{} 的 page_ready_condition 应包含 textarea 或 chat-input",
                site
            );
        }
    }

    #[test]
    fn test_site_type_page_ready_condition_zai() {
        let cond = SiteType::Zai.page_ready_condition();
        assert!(cond.contains("#chat-input"));
    }

    #[test]
    fn test_site_type_page_ready_condition_deepseek() {
        let cond = SiteType::DeepSeek.page_ready_condition();
        assert!(cond.contains("ds-scroll-area") || cond.contains("textarea"));
    }

    #[test]
    fn test_site_type_page_ready_condition_claude() {
        let cond = SiteType::Claude.page_ready_condition();
        assert!(cond.contains("ProseMirror") || cond.contains("contenteditable"));
    }

    #[test]
    fn test_site_type_is_known() {
        assert!(SiteType::Zai.is_known());
        assert!(SiteType::DeepSeek.is_known());
        assert!(SiteType::Kimi.is_known());
        assert!(SiteType::Tongyi.is_known());
        assert!(SiteType::Claude.is_known());
        assert!(!SiteType::Unknown.is_known());
    }

    #[test]
    fn test_site_type_display_name() {
        assert_eq!(SiteType::Zai.display_name(), "Z.ai");
        assert_eq!(SiteType::DeepSeek.display_name(), "DeepSeek");
        assert_eq!(SiteType::Kimi.display_name(), "Kimi");
        assert_eq!(SiteType::Tongyi.display_name(), "通义千问");
        assert_eq!(SiteType::Claude.display_name(), "Claude");
        assert_eq!(SiteType::Unknown.display_name(), "未知网站");
    }

    #[test]
    fn test_site_type_display() {
        assert_eq!(format!("{}", SiteType::Zai), "Z.ai");
        assert_eq!(format!("{}", SiteType::DeepSeek), "DeepSeek");
        assert_eq!(format!("{}", SiteType::Unknown), "未知网站");
    }

    #[test]
    fn test_site_type_default() {
        assert_eq!(SiteType::default(), SiteType::Unknown);
    }

    #[test]
    fn test_site_type_clone_copy() {
        let site = SiteType::Zai;
        let cloned = site;
        assert_eq!(site, cloned);
    }

    #[test]
    fn test_site_type_serde() {
        let json = serde_json::to_string(&SiteType::DeepSeek).unwrap();
        let parsed: SiteType = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, SiteType::DeepSeek);
    }

    #[test]
    fn test_site_type_debug() {
        let debug = format!("{:?}", SiteType::Zai);
        assert!(debug.contains("Zai"));
    }

    // ===== looks_like_chat 测试 =====

    #[test]
    fn test_looks_like_chat_zai() {
        assert!(BrowserManager::looks_like_chat(
            "https://chat.z.ai/",
            "Z.ai Chat"
        ));
        assert!(BrowserManager::looks_like_chat(
            "https://chat.z.ai/chat/123",
            "聊天"
        ));
    }

    #[test]
    fn test_looks_like_chat_deepseek() {
        assert!(BrowserManager::looks_like_chat(
            "https://chat.deepseek.com/",
            "DeepSeek"
        ));
        assert!(BrowserManager::looks_like_chat(
            "https://chat.deepseek.com/some",
            "AI Chat"
        ));
    }

    #[test]
    fn test_looks_like_chat_kimi() {
        assert!(BrowserManager::looks_like_chat(
            "https://kimi.moonshot.cn/",
            "Kimi"
        ));
        assert!(BrowserManager::looks_like_chat(
            "https://moonshot.cn/chat",
            "对话助手"
        ));
    }

    #[test]
    fn test_looks_like_chat_tongyi() {
        assert!(BrowserManager::looks_like_chat(
            "https://tongyi.aliyun.com/",
            "通义千问"
        ));
        assert!(BrowserManager::looks_like_chat(
            "https://aliyun.com/bot",
            "AI"
        ));
    }

    #[test]
    fn test_looks_like_chat_claude() {
        assert!(BrowserManager::looks_like_chat(
            "https://claude.ai/",
            "Claude"
        ));
        assert!(BrowserManager::looks_like_chat(
            "https://claude.ai/new",
            "Claude AI"
        ));
    }

    #[test]
    fn test_looks_like_chat_other_ai() {
        assert!(BrowserManager::looks_like_chat(
            "https://chatgpt.com/",
            "ChatGPT"
        ));
        assert!(BrowserManager::looks_like_chat(
            "https://claude.ai/",
            "Claude"
        ));
        assert!(BrowserManager::looks_like_chat(
            "https://gemini.google.com/",
            "Gemini"
        ));
        assert!(BrowserManager::looks_like_chat(
            "https://poe.com/",
            "Poe AI"
        ));
    }

    #[test]
    fn test_looks_like_chat_negative() {
        assert!(!BrowserManager::looks_like_chat(
            "https://google.com/",
            "Google"
        ));
        assert!(!BrowserManager::looks_like_chat(
            "https://github.com/",
            "GitHub"
        ));
        assert!(!BrowserManager::looks_like_chat(
            "https://example.com/",
            "Example"
        ));
    }

    #[test]
    fn test_looks_like_chat_chinese_keywords() {
        assert!(BrowserManager::looks_like_chat(
            "https://doubao.com/",
            "豆包"
        ));
        assert!(BrowserManager::looks_like_chat(
            "https://yiyan.baidu.com/",
            "文心一言"
        ));
        assert!(BrowserManager::looks_like_chat(
            "https://some.com/",
            "AI助手"
        ));
    }

    #[test]
    fn test_looks_like_chat_no_ai_false_positive() {
        // "ai" 已移除 — 不应匹配 YouTube/GitHub 等非聊天页面中仅含 "AI" 的标题
        assert!(!BrowserManager::looks_like_chat(
            "https://www.youtube.com/watch?v=123",
            "AIProg Tutorial"
        ));
        assert!(!BrowserManager::looks_like_chat(
            "https://github.com/OpenMOSS/MOSS-TTS",
            "MOSS-TTS"
        ));
        assert!(!BrowserManager::looks_like_chat(
            "https://example.com/",
            "Some AI Tool"
        ));
        // 但 AI 聊天网站仍应匹配 (通过其他关键词)
        assert!(BrowserManager::looks_like_chat(
            "https://claude.ai/",
            "Claude"
        )); // "claude"
        assert!(BrowserManager::looks_like_chat(
            "https://chat.z.ai/",
            "Z.ai"
        )); // "z.ai" + "chat"
    }

    #[test]
    fn test_looks_like_chat_still_matches_chat_sites() {
        // 确保移除 "ai" 后所有已知聊天网站仍被正确识别
        assert!(BrowserManager::looks_like_chat(
            "https://chat.z.ai/",
            "Z.ai"
        ));
        assert!(BrowserManager::looks_like_chat(
            "https://chat.deepseek.com/",
            "DeepSeek"
        ));
        assert!(BrowserManager::looks_like_chat(
            "https://kimi.moonshot.cn/",
            "Kimi"
        ));
        assert!(BrowserManager::looks_like_chat(
            "https://tongyi.aliyun.com/",
            "通义千问"
        ));
        assert!(BrowserManager::looks_like_chat(
            "https://claude.ai/",
            "Claude"
        ));
        assert!(BrowserManager::looks_like_chat(
            "https://chatgpt.com/",
            "ChatGPT"
        ));
    }

    // ===== ChatElements 默认值测试 =====

    #[test]
    fn test_chat_elements_default() {
        let elements = ChatElements::default();
        assert_eq!(elements.input_selector, "textarea");
        assert_eq!(elements.send_selector, "button[type='submit']");
        assert!(elements.ai_message_selector.contains("assistant"));
        assert!(elements.stop_button_selector.is_none());
    }

    #[test]
    fn test_chat_elements_serde() {
        let elements = ChatElements {
            input_selector: "#chat-input".to_string(),
            send_selector: "#send-button".to_string(),
            ai_message_selector: ".chat-assistant".to_string(),
            stop_button_selector: Some(".stop-btn".to_string()),
        };
        let json = serde_json::to_string(&elements).unwrap();
        let parsed: ChatElements = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.input_selector, "#chat-input");
        assert_eq!(parsed.send_selector, "#send-button");
        assert_eq!(parsed.ai_message_selector, ".chat-assistant");
        assert_eq!(parsed.stop_button_selector, Some(".stop-btn".to_string()));
    }
}
