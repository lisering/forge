//! 网站健康检查 — 检测聊天网站是否可用
//!
//! 在 24 小时不间断运行中, 网站可能出现以下问题:
//! - **未登录**: 会话过期, 重定向到登录页面
//! - **限流**: 请求过于频繁, 网站返回限流提示
//! - **维护中**: 网站正在进行维护
//! - **网络错误**: 页面加载失败
//!
//! 本模块通过 CDP 执行 JS 检测页面的 DOM 状态, 判断网站是否健康。
//! 当网站不健康时, 可触发多网站自动切换 (SiteFailover)。

use crate::browser::SiteType;
use crate::cdp::CdpSession;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

// ============================================================================
//  HealthSeverity — 健康状态严重程度 (与 connection_monitor::ConnectionSeverity 协同)
// ============================================================================

/// 网站健康状态的严重程度分类
///
/// 由 [`classify_health_severity`] 根据 [`SiteHealthStatus`] 计算。
/// 与 [`crate::connection_monitor::ConnectionSeverity`] 的区别:
/// - `ConnectionSeverity` 只看 CDP 连接层面 (Connected / TabClosed / ChromeUnreachable)
/// - `HealthSeverity` 看网站层面 (Healthy / RateLimited / UnderMaintenance)
///
/// 两者协同构成完整的 24h 可靠性链路:
/// `ConnectionSeverity` (CDP层) → `HealthSeverity` (网站层) → `RecoveryUrgency` (恢复层)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HealthSeverity {
    /// 信息 — 网站正常
    Info,
    /// 警告 — 可自动恢复的异常 (限流、未登录)
    Warning,
    /// 严重 — 需要人工干预的异常 (维护中、网络错误)
    Critical,
    /// 未知 — 无法判断, 需进一步检查
    Unknown,
}

impl HealthSeverity {
    /// 中文描述
    pub fn description(&self) -> &'static str {
        match self {
            HealthSeverity::Info => "信息",
            HealthSeverity::Warning => "警告",
            HealthSeverity::Critical => "严重",
            HealthSeverity::Unknown => "未知",
        }
    }

    /// 是否需要立即故障转移
    pub fn requires_immediate_failover(&self) -> bool {
        matches!(self, HealthSeverity::Critical)
    }
}

impl std::fmt::Display for HealthSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.description())
    }
}

// ============================================================================
//  SiteHealthStatus — 网站健康状态
// ============================================================================

/// 网站健康状态
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SiteHealthStatus {
    /// 健康 — 页面正常, 输入框可用
    Healthy,
    /// 未登录 — 重定向到登录页面或显示登录提示
    NotLoggedIn,
    /// 被限流 — 请求过于频繁, 网站返回限流提示
    RateLimited,
    /// 维护中 — 网站正在维护
    UnderMaintenance,
    /// 网络错误 — 页面加载失败或连接断开
    NetworkError,
    /// 未知状态 — 无法判断 (可能是页面结构变化)
    #[default]
    Unknown,
}

impl SiteHealthStatus {
    /// 是否健康 (可以正常发送消息)
    pub fn is_healthy(&self) -> bool {
        matches!(self, Self::Healthy)
    }

    /// 是否应该切换到其他网站
    pub fn should_failover(&self) -> bool {
        matches!(
            self,
            Self::NotLoggedIn | Self::RateLimited | Self::UnderMaintenance | Self::NetworkError
        )
    }

    /// 人类可读的描述
    pub fn description(&self) -> &str {
        match self {
            Self::Healthy => "健康 — 页面正常",
            Self::NotLoggedIn => "未登录 — 需要重新登录",
            Self::RateLimited => "被限流 — 请求过于频繁",
            Self::UnderMaintenance => "维护中 — 网站不可用",
            Self::NetworkError => "网络错误 — 页面加载失败",
            Self::Unknown => "未知状态 — 无法判断",
        }
    }
}

impl std::fmt::Display for SiteHealthStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.description())
    }
}

// ============================================================================
//  HealthCheckResult — 健康检查结果
// ============================================================================

/// 健康检查结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheckResult {
    /// 健康状态
    pub status: SiteHealthStatus,
    /// 检查时间戳 (秒)
    pub timestamp: u64,
    /// 检测到的提示信息 (如有)
    pub message: Option<String>,
    /// 当前页面 URL (检测时)
    pub current_url: Option<String>,
    /// 检查耗时 (毫秒)
    pub check_duration_ms: u64,
}

impl HealthCheckResult {
    /// 创建一个快速结果 (用于测试)
    pub fn new(status: SiteHealthStatus) -> Self {
        Self {
            status,
            timestamp: 0,
            message: None,
            current_url: None,
            check_duration_ms: 0,
        }
    }

    /// 是否健康
    pub fn is_healthy(&self) -> bool {
        self.status.is_healthy()
    }

    /// 是否应该切换到其他网站
    pub fn should_failover(&self) -> bool {
        self.status.should_failover()
    }
}

// ============================================================================
//  SiteHealthChecker — 网站健康检查器
// ============================================================================

/// 网站健康检查器
///
/// 通过 CDP 执行 JS 检测页面的 DOM 状态, 判断网站是否健康。
/// 支持多网站 (Z.ai / DeepSeek / Kimi / 通义千问), 每个网站有特定的检测逻辑。
pub struct SiteHealthChecker;

impl SiteHealthChecker {
    /// 检查网站健康状态
    ///
    /// 通过一次 CDP evaluate 调用检测多种健康指标:
    /// 1. 当前 URL (是否重定向到登录页)
    /// 2. 登录按钮/表单是否存在
    /// 3. 限流提示文本是否存在
    /// 4. 维护页面是否存在
    /// 5. 输入框是否可用 (健康的核心标志)
    pub async fn check(session: &CdpSession, site_type: SiteType) -> Result<HealthCheckResult> {
        let start = Instant::now();
        let js = Self::build_check_js(site_type);

        let result = session.evaluate_string(&js).await?;
        let check_duration_ms = start.elapsed().as_millis() as u64;

        // 解析 JSON 结果
        let parsed: HealthCheckJson =
            serde_json::from_str(&result).unwrap_or(HealthCheckJson::default());

        let status = Self::interpret_result(&parsed, site_type);
        let message = if status != SiteHealthStatus::Healthy {
            Some(parsed.message)
        } else {
            None
        };

        let current_url = if parsed.url.is_empty() {
            None
        } else {
            Some(parsed.url)
        };

        debug!(
            "健康检查: {} ({:?}) — 耗时 {}ms",
            status, site_type, check_duration_ms
        );

        Ok(HealthCheckResult {
            status,
            timestamp: start.elapsed().as_secs(),
            message,
            current_url,
            check_duration_ms,
        })
    }

    /// 构建健康检查的 JS 代码 — 一次 evaluate 检测所有指标
    fn build_check_js(site_type: SiteType) -> String {
        let login_url_patterns = Self::login_url_patterns(site_type);
        let login_text_patterns = Self::login_text_patterns(site_type);
        let rate_limit_patterns = Self::rate_limit_patterns(site_type);
        let maintenance_patterns = Self::maintenance_patterns(site_type);
        let input_selector = Self::input_selector(site_type);

        format!(
            r#"
            (() => {{
                const result = {{
                    url: window.location.href,
                    hasInput: false,
                    hasLoginButton: false,
                    hasRateLimit: false,
                    hasMaintenance: false,
                    message: '',
                }};

                // 1. 检查输入框是否存在且可用
                const input = document.querySelector('{}');
                if (input) {{
                    const rect = input.getBoundingClientRect();
                    const style = window.getComputedStyle(input);
                    result.hasInput = rect.width > 50 && rect.height > 15 &&
                        style.display !== 'none' && style.visibility !== 'hidden';
                }}

                // 2. 检查 URL 是否重定向到登录页面
                const loginUrlPatterns = {};
                const lowerUrl = result.url.toLowerCase();
                for (const pattern of loginUrlPatterns) {{
                    if (lowerUrl.includes(pattern)) {{
                        result.hasLoginButton = true;
                        result.message = 'URL 重定向到登录页: ' + pattern;
                        break;
                    }}
                }}

                // 3. 检查登录按钮/表单文本
                if (!result.hasLoginButton) {{
                    const loginTextPatterns = {};
                    const bodyText = (document.body?.innerText || '').toLowerCase();
                    for (const pattern of loginTextPatterns) {{
                        if (bodyText.includes(pattern)) {{
                            result.hasLoginButton = true;
                            result.message = '检测到登录提示: ' + pattern;
                            break;
                        }}
                    }}
                }}

                // 4. 检查限流提示
                const rateLimitPatterns = {};
                const bodyText2 = (document.body?.innerText || '').toLowerCase();
                for (const pattern of rateLimitPatterns) {{
                    if (bodyText2.includes(pattern)) {{
                        result.hasRateLimit = true;
                        result.message = '检测到限流提示: ' + pattern;
                        break;
                    }}
                }}

                // 5. 检查维护页面
                const maintenancePatterns = {};
                for (const pattern of maintenancePatterns) {{
                    if (bodyText2.includes(pattern)) {{
                        result.hasMaintenance = true;
                        result.message = '检测到维护提示: ' + pattern;
                        break;
                    }}
                }}

                return JSON.stringify(result);
            }})()
            "#,
            input_selector,
            Self::js_array(&login_url_patterns),
            Self::js_array(&login_text_patterns),
            Self::js_array(&rate_limit_patterns),
            Self::js_array(&maintenance_patterns),
        )
    }

    /// 解释检测结果 — 委托纯函数 [`interpret_health_json`]
    fn interpret_result(parsed: &HealthCheckJson, _site_type: SiteType) -> SiteHealthStatus {
        interpret_health_json(parsed)
    }

    // ===== 网站特定配置 =====

    /// 获取输入框选择器 (网站特定)
    fn input_selector(site_type: SiteType) -> &'static str {
        match site_type {
            SiteType::Zai => "#chat-input, textarea",
            SiteType::DeepSeek => r#"textarea, [class*="ds-scroll-area"]"#,
            SiteType::Kimi => r#"textarea, [contenteditable="true"]"#,
            SiteType::Tongyi => r#"textarea, [contenteditable="true"]"#,
            SiteType::Claude => r#".ProseMirror, [contenteditable="true"], textarea"#,
            SiteType::Unknown => "textarea, #chat-input",
        }
    }

    /// 登录页 URL 模式 (网站特定)
    fn login_url_patterns(site_type: SiteType) -> Vec<&'static str> {
        match site_type {
            SiteType::Zai => vec!["login", "signin", "auth"],
            SiteType::DeepSeek => vec!["login", "signin", "sign_in"],
            SiteType::Kimi => vec!["login", "signin", "auth"],
            SiteType::Tongyi => vec!["login", "signin", "auth"],
            SiteType::Claude => vec!["login", "signin", "auth", "oauth"],
            SiteType::Unknown => vec!["login", "signin", "auth"],
        }
    }

    /// 登录提示文本模式 (网站特定)
    fn login_text_patterns(_site_type: SiteType) -> Vec<&'static str> {
        // 通用登录提示文本
        vec![
            "请登录",
            "请先登录",
            "登录后",
            "sign in",
            "log in",
            "please login",
            "未登录",
            "需要登录",
            "登录账号",
            "扫码登录",
        ]
    }

    /// 限流提示文本模式
    fn rate_limit_patterns(_site_type: SiteType) -> Vec<&'static str> {
        vec![
            "too many requests",
            "rate limit",
            "频繁",
            "稍后再试",
            "请求过多",
            "限流",
            "稍后重试",
            "too frequent",
            "service unavailable",
            "服务繁忙",
            "try again later",
            // Z.ai 特定限流文本 (第 39 项改进)
            "请求过于频繁",
            "回复内容为空",
            "请稍后重试",
            "操作太频繁",
            "请稍后再试",
            "请稍候再试",
            "请求被限流",
            "request was throttled",
        ]
    }

    /// 检测 AI 回复文本中是否包含限流标志
    ///
    /// Z.ai 限流时, 回复内容可能是一句限流提示 (如 "回复内容为空，请稍后重试")
    /// 而非正常的代码回复。本方法检测这些标志, 用于在发送成功后主动触发故障切换。
    ///
    /// 返回 `Some(HealthCheckResult)` 如果检测到限流, `None` 如果回复正常。
    pub fn check_response_for_rate_limit(text: &str) -> Option<HealthCheckResult> {
        let lower = text.to_lowercase();
        let patterns = Self::rate_limit_patterns(SiteType::Unknown);
        for pattern in &patterns {
            if lower.contains(pattern) {
                return Some(HealthCheckResult {
                    status: SiteHealthStatus::RateLimited,
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                    message: Some(format!("AI 回复包含限流标志: {}", pattern)),
                    current_url: None,
                    check_duration_ms: 0,
                });
            }
        }
        None
    }

    /// 维护提示文本模式
    fn maintenance_patterns(_site_type: SiteType) -> Vec<&'static str> {
        vec![
            "维护中",
            "系统维护",
            "under maintenance",
            "maintenance",
            "升级中",
            "系统升级",
            "暂时无法访问",
        ]
    }

    /// 将 Rust 字符串数组转为 JS 数组字面量
    fn js_array(items: &[&str]) -> String {
        let items: Vec<String> = items
            .iter()
            .map(|s| format!("'{}'", s.replace('\'', "\\'")))
            .collect();
        format!("[{}]", items.join(", "))
    }
}

// ============================================================================
//  纯逻辑函数 — 健康检查核心算法 (无副作用, 可独立测试)
// ============================================================================
//
// 以下函数将健康检查的核心决策逻辑提取为纯函数, 使得:
// 1. 可以在无 Chrome 环境下完全测试
// 2. 决策逻辑集中管理, 修改策略时只需改一处
// 3. 与 connection_monitor.rs / auto_recovery.rs 的纯函数协同, 形成完整 24h 可靠性链路

/// 健康检查 JS 返回的 JSON 结构
///
/// 由 `SiteHealthChecker::build_check_js` 生成的 JS 代码返回,
/// 用于解析 CDP evaluate 结果。
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct HealthCheckJson {
    /// 当前页面 URL
    #[serde(default)]
    pub url: String,
    /// 输入框是否存在且可用
    #[serde(default, rename = "hasInput")]
    pub has_input: bool,
    /// 是否检测到登录按钮/登录提示
    #[serde(default, rename = "hasLoginButton")]
    pub has_login_button: bool,
    /// 是否检测到限流提示
    #[serde(default, rename = "hasRateLimit")]
    pub has_rate_limit: bool,
    /// 是否检测到维护提示
    #[serde(default, rename = "hasMaintenance")]
    pub has_maintenance: bool,
    /// 检测到的提示信息
    #[serde(default)]
    pub message: String,
}

impl HealthCheckJson {
    /// 创建一个新的 `HealthCheckJson` (所有字段为默认值)
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置 URL
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = url.into();
        self
    }

    /// 设置输入框是否可用
    pub fn with_input(mut self, has_input: bool) -> Self {
        self.has_input = has_input;
        self
    }

    /// 设置是否有登录提示
    pub fn with_login_button(mut self, has_login_button: bool) -> Self {
        self.has_login_button = has_login_button;
        self
    }

    /// 设置是否有限流提示
    pub fn with_rate_limit(mut self, has_rate_limit: bool) -> Self {
        self.has_rate_limit = has_rate_limit;
        self
    }

    /// 设置是否有维护提示
    pub fn with_maintenance(mut self, has_maintenance: bool) -> Self {
        self.has_maintenance = has_maintenance;
        self
    }

    /// 设置消息
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = message.into();
        self
    }
}

/// 解释健康检查 JSON 结果 — 根据各项指标判断健康状态
///
/// 这是 `SiteHealthChecker::interpret_result` 的纯函数版本, 可独立测试。
///
/// # 优先级
/// 维护 > 限流 > 未登录 > 有输入框(健康) > 未知
///
/// # 参数
/// - `parsed`: 健康检查 JS 返回的 JSON 结构
///
/// # 返回值
/// 健康状态
///
/// # 示例
///
/// ```
/// use forge::site_health::{interpret_health_json, HealthCheckJson, SiteHealthStatus};
///
/// // 有输入框 → 健康
/// let json = HealthCheckJson::default().with_input(true);
/// assert_eq!(interpret_health_json(&json), SiteHealthStatus::Healthy);
///
/// // 维护中 (最高优先级)
/// let json = HealthCheckJson::default().with_maintenance(true).with_input(true);
/// assert_eq!(interpret_health_json(&json), SiteHealthStatus::UnderMaintenance);
/// ```
pub fn interpret_health_json(parsed: &HealthCheckJson) -> SiteHealthStatus {
    // 优先级: 维护 > 限流 > 未登录 > 有输入框(健康) > 未知
    if parsed.has_maintenance {
        return SiteHealthStatus::UnderMaintenance;
    }

    if parsed.has_rate_limit {
        return SiteHealthStatus::RateLimited;
    }

    if parsed.has_login_button {
        // 如果有登录提示但输入框也存在, 可能只是页面有登录按钮 (不影响使用)
        if parsed.has_input {
            return SiteHealthStatus::Healthy;
        }
        return SiteHealthStatus::NotLoggedIn;
    }

    // 如果输入框存在且可用, 认为健康
    if parsed.has_input {
        return SiteHealthStatus::Healthy;
    }

    // 没有输入框, 也没有明确的异常标志
    SiteHealthStatus::Unknown
}

/// 分类健康状态的严重程度
///
/// 根据网站健康状态返回严重程度, 用于日志和决策。
///
/// # 映射
/// - `Healthy` → `Info`
/// - `RateLimited`, `NotLoggedIn` → `Warning` (可自动恢复)
/// - `UnderMaintenance`, `NetworkError` → `Critical` (需人工干预)
/// - `Unknown` → `Unknown`
///
/// # 示例
///
/// ```
/// use forge::site_health::{classify_health_severity, HealthSeverity, SiteHealthStatus};
///
/// assert_eq!(
///     classify_health_severity(&SiteHealthStatus::Healthy),
///     HealthSeverity::Info
/// );
/// assert_eq!(
///     classify_health_severity(&SiteHealthStatus::RateLimited),
///     HealthSeverity::Warning
/// );
/// assert_eq!(
///     classify_health_severity(&SiteHealthStatus::UnderMaintenance),
///     HealthSeverity::Critical
/// );
/// ```
pub fn classify_health_severity(status: &SiteHealthStatus) -> HealthSeverity {
    match status {
        SiteHealthStatus::Healthy => HealthSeverity::Info,
        SiteHealthStatus::RateLimited | SiteHealthStatus::NotLoggedIn => HealthSeverity::Warning,
        SiteHealthStatus::UnderMaintenance | SiteHealthStatus::NetworkError => {
            HealthSeverity::Critical
        }
        SiteHealthStatus::Unknown => HealthSeverity::Unknown,
    }
}

/// 计算下次健康检查的延迟 (秒)
///
/// 根据当前健康状态决定下次检查间隔:
/// - `Healthy` → 使用基础间隔 (默认 60s)
/// - `RateLimited` → 缩短到基础间隔的 1/4 (快速恢复检测)
/// - `NotLoggedIn` → 缩短到基础间隔的 1/2
/// - `UnderMaintenance` → 延长到基础间隔的 2 倍 (维护通常需要时间)
/// - `NetworkError` → 缩短到基础间隔的 1/4 (快速重试)
/// - `Unknown` → 缩短到基础间隔的 1/2 (需要尽快确认状态)
///
/// # 参数
/// - `status`: 当前健康状态
/// - `base_interval_secs`: 基础检查间隔 (秒)
///
/// # 返回值
/// 下次检查延迟 (秒), 最小 1 秒
///
/// # 示例
///
/// ```
/// use forge::site_health::{compute_health_check_interval, SiteHealthStatus};
///
/// // 健康 → 60s
/// assert_eq!(compute_health_check_interval(&SiteHealthStatus::Healthy, 60), 60);
///
/// // 限流 → 15s (60/4)
/// assert_eq!(compute_health_check_interval(&SiteHealthStatus::RateLimited, 60), 15);
///
/// // 维护 → 120s (60*2)
/// assert_eq!(compute_health_check_interval(&SiteHealthStatus::UnderMaintenance, 60), 120);
/// ```
pub fn compute_health_check_interval(status: &SiteHealthStatus, base_interval_secs: u64) -> u64 {
    let multiplier = match status {
        SiteHealthStatus::Healthy => 1.0,
        SiteHealthStatus::RateLimited => 0.25,
        SiteHealthStatus::NotLoggedIn => 0.5,
        SiteHealthStatus::UnderMaintenance => 2.0,
        SiteHealthStatus::NetworkError => 0.25,
        SiteHealthStatus::Unknown => 0.5,
    };
    let result = (base_interval_secs as f64 * multiplier).round() as u64;
    result.max(1)
}

/// 格式化健康检查结果为单行文本 (用于日志和 DevTrace)
///
/// 生成如 `"[0] Z.ai: 健康 — 页面正常 (120ms)"` 的格式。
///
/// # 参数
/// - `tab_idx`: 标签页索引
/// - `site_type`: 网站类型
/// - `result`: 健康检查结果
///
/// # 示例
///
/// ```
/// use forge::site_health::{format_health_result_line, HealthCheckResult, SiteHealthStatus};
/// use forge::browser::SiteType;
///
/// let result = HealthCheckResult::new(SiteHealthStatus::Healthy);
/// let line = format_health_result_line(0, SiteType::Zai, &result);
/// assert!(line.contains("[0]"));
/// assert!(line.contains("Z.ai"));
/// assert!(line.contains("健康"));
/// ```
pub fn format_health_result_line(
    tab_idx: usize,
    site_type: SiteType,
    result: &HealthCheckResult,
) -> String {
    let msg = result
        .message
        .as_deref()
        .unwrap_or(result.status.description());
    format!(
        "[{}] {}: {} ({}ms)",
        tab_idx, site_type, msg, result.check_duration_ms
    )
}

/// 确定标签页的故障转移优先级
///
/// 根据健康状态返回一个数值, 数值越小优先级越高 (越应该被选中)。
///
/// # 优先级映射
/// - `Healthy` → 0 (最高优先级)
/// - `Unknown` → 1
/// - `NotLoggedIn` → 2
/// - `RateLimited` → 3
/// - `NetworkError` → 4
/// - `UnderMaintenance` → 5 (最低优先级)
///
/// # 示例
///
/// ```
/// use forge::site_health::{determine_failover_priority, SiteHealthStatus};
///
/// assert_eq!(determine_failover_priority(&SiteHealthStatus::Healthy), 0);
/// assert_eq!(determine_failover_priority(&SiteHealthStatus::RateLimited), 3);
/// assert_eq!(determine_failover_priority(&SiteHealthStatus::UnderMaintenance), 5);
/// ```
pub fn determine_failover_priority(status: &SiteHealthStatus) -> u8 {
    match status {
        SiteHealthStatus::Healthy => 0,
        SiteHealthStatus::Unknown => 1,
        SiteHealthStatus::NotLoggedIn => 2,
        SiteHealthStatus::RateLimited => 3,
        SiteHealthStatus::NetworkError => 4,
        SiteHealthStatus::UnderMaintenance => 5,
    }
}

/// 从多个健康检查结果中选择最佳健康标签页
///
/// 返回第一个健康标签页的索引。如果没有健康标签页,
/// 返回故障转移优先级最低的标签页索引。
/// 如果列表为空, 返回 `None`。
///
/// # 参数
/// - `results`: (标签页索引, 健康检查结果) 列表
///
/// # 返回值
/// `Some(best_tab_idx)` 或 `None`
///
/// # 示例
///
/// ```
/// use forge::site_health::{select_best_healthy_tab, HealthCheckResult, SiteHealthStatus};
///
/// let results = vec![
///     (0, HealthCheckResult::new(SiteHealthStatus::RateLimited)),
///     (1, HealthCheckResult::new(SiteHealthStatus::Healthy)),
///     (2, HealthCheckResult::new(SiteHealthStatus::Unknown)),
/// ];
/// assert_eq!(select_best_healthy_tab(&results), Some(1));
///
/// // 无健康标签页 → 返回优先级最低的
/// let results = vec![
///     (0, HealthCheckResult::new(SiteHealthStatus::RateLimited)),
///     (1, HealthCheckResult::new(SiteHealthStatus::UnderMaintenance)),
/// ];
/// assert_eq!(select_best_healthy_tab(&results), Some(0));
/// ```
pub fn select_best_healthy_tab(results: &[(usize, HealthCheckResult)]) -> Option<usize> {
    if results.is_empty() {
        return None;
    }

    // 优先返回第一个健康的标签页
    for (idx, result) in results {
        if result.is_healthy() {
            return Some(*idx);
        }
    }

    // 没有健康的, 返回故障转移优先级最低的
    results
        .iter()
        .min_by_key(|(_, r)| determine_failover_priority(&r.status))
        .map(|(idx, _)| *idx)
}

/// 判断是否应该跳过当前标签页 (基于健康历史)
///
/// 当连续不健康次数超过阈值时, 应跳过该标签页。
///
/// # 参数
/// - `consecutive_unhealthy`: 连续不健康次数
/// - `threshold`: 跳过阈值
///
/// # 示例
///
/// ```
/// use forge::site_health::should_skip_tab;
///
/// assert!(!should_skip_tab(0, 3));
/// assert!(!should_skip_tab(2, 3));
/// assert!(should_skip_tab(3, 3));
/// assert!(should_skip_tab(5, 3));
/// ```
pub fn should_skip_tab(consecutive_unhealthy: u32, threshold: u32) -> bool {
    consecutive_unhealthy >= threshold
}

/// 计算健康率
///
/// 健康率 = 健康检查次数 / 总健康检查次数。
/// 当总检查次数为 0 时, 返回 1.0 (视为完全健康)。
///
/// # 参数
/// - `total_checks`: 总健康检查次数
/// - `healthy_checks`: 健康检查通过次数
///
/// # 示例
///
/// ```
/// use forge::site_health::calculate_health_rate;
///
/// assert!((calculate_health_rate(0, 0) - 1.0).abs() < 0.001);
/// assert!((calculate_health_rate(100, 80) - 0.8).abs() < 0.001);
/// assert!((calculate_health_rate(100, 0) - 0.0).abs() < 0.001);
/// ```
pub fn calculate_health_rate(total_checks: u64, healthy_checks: u64) -> f64 {
    if total_checks == 0 {
        return 1.0;
    }
    (healthy_checks as f64 / total_checks as f64).clamp(0.0, 1.0)
}

/// 格式化健康率为百分比字符串
///
/// # 示例
///
/// ```
/// use forge::site_health::format_health_rate;
///
/// assert_eq!(format_health_rate(1.0), "100.0%");
/// assert_eq!(format_health_rate(0.8), "80.0%");
/// assert_eq!(format_health_rate(0.0), "0.0%");
/// ```
pub fn format_health_rate(rate: f64) -> String {
    format!("{:.1}%", rate * 100.0)
}

// ============================================================================
//  SiteFailover — 多网站自动切换
// ============================================================================

/// 多网站自动切换策略
///
/// 当主网站不健康时, 自动尝试其他可用的标签页。
/// 支持配置切换策略: 顺序切换 / 轮询 / 优先级。
#[derive(Debug, Clone)]
pub struct SiteFailover {
    /// 可用的标签页索引列表 (按优先级排序)
    pub available_tabs: Vec<usize>,
    /// 当前使用的标签页索引
    pub current_tab: usize,
    /// 已尝试过的标签页 (本次 failover 中)
    pub tried_tabs: Vec<usize>,
    /// 连续失败次数 (所有标签页)
    pub consecutive_failures: usize,
    /// 最大连续失败次数 (超过后放弃)
    pub max_consecutive_failures: usize,
    /// 切换冷却时间 (避免频繁切换)
    pub cooldown_secs: u64,
    /// 上次切换时间 (运行时使用, 不序列化)
    pub last_switch_time: Option<std::time::Instant>,
}

impl SiteFailover {
    /// 创建自动切换策略
    pub fn new(available_tabs: Vec<usize>, current_tab: usize) -> Self {
        Self {
            available_tabs,
            current_tab,
            tried_tabs: vec![current_tab],
            consecutive_failures: 0,
            max_consecutive_failures: 3,
            cooldown_secs: 30,
            last_switch_time: None,
        }
    }

    /// 设置最大连续失败次数
    pub fn with_max_failures(mut self, max: usize) -> Self {
        self.max_consecutive_failures = max;
        self
    }

    /// 设置切换冷却时间
    pub fn with_cooldown(mut self, secs: u64) -> Self {
        self.cooldown_secs = secs;
        self
    }

    /// 是否应该切换到其他标签页
    ///
    /// 当健康检查结果不健康时调用此方法。
    /// 返回 Some(new_tab) 表示应切换到新标签页, None 表示无法切换。
    pub fn should_switch(&self, health: &HealthCheckResult) -> Option<usize> {
        if health.is_healthy() {
            return None;
        }

        if !health.should_failover() {
            return None;
        }

        // 检查冷却时间
        if let Some(last_switch) = self.last_switch_time {
            if last_switch.elapsed() < Duration::from_secs(self.cooldown_secs) {
                warn!(
                    "切换冷却中 (剩余 {:?}), 暂不切换",
                    Duration::from_secs(self.cooldown_secs) - last_switch.elapsed()
                );
                return None;
            }
        }

        // 检查是否超过最大失败次数
        if self.consecutive_failures >= self.max_consecutive_failures {
            warn!(
                "连续失败 {} 次 (超过最大 {}), 放弃切换",
                self.consecutive_failures, self.max_consecutive_failures
            );
            return None;
        }

        // 找到下一个未尝试的标签页
        let next = self
            .available_tabs
            .iter()
            .find(|&&tab| !self.tried_tabs.contains(&tab) && tab != self.current_tab);

        next.copied()
    }

    /// 记录切换
    pub fn record_switch(&mut self, new_tab: usize) {
        self.tried_tabs.push(new_tab);
        self.current_tab = new_tab;
        self.last_switch_time = Some(std::time::Instant::now());
        info!("🔄 网站切换: -> 标签页 {}", new_tab);
    }

    /// 记录成功 (重置失败计数)
    pub fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.tried_tabs.clear();
        self.tried_tabs.push(self.current_tab);
    }

    /// 记录失败
    pub fn record_failure(&mut self) {
        self.consecutive_failures += 1;
    }

    /// 是否所有标签页都已尝试
    pub fn all_tried(&self) -> bool {
        self.available_tabs
            .iter()
            .all(|tab| self.tried_tabs.contains(tab))
    }

    /// 重置 (在新的一轮 failover 开始时调用)
    pub fn reset(&mut self) {
        self.tried_tabs.clear();
        self.tried_tabs.push(self.current_tab);
        self.consecutive_failures = 0;
        self.last_switch_time = None;
    }
}

// ============================================================================
//  健康检查辅助函数
// ============================================================================

/// 执行健康检查并日志输出结果
pub async fn check_and_log(session: &CdpSession, site_type: SiteType) -> Result<HealthCheckResult> {
    let result = SiteHealthChecker::check(session, site_type).await?;

    if result.is_healthy() {
        info!("✅ 网站健康检查通过 [{}] — 输入框可用", site_type);
    } else {
        warn!(
            "⚠ 网站健康检查未通过 [{}]: {} — {}",
            site_type,
            result.status,
            result.message.as_deref().unwrap_or("无详细信息")
        );
    }

    Ok(result)
}

/// 批量检查多个标签页的健康状态
///
/// 返回 (tab_index, HealthCheckResult) 列表, 按 healthy 优先排序。
pub async fn check_all_tabs(tabs: &[(&CdpSession, SiteType)]) -> Vec<(usize, HealthCheckResult)> {
    let mut results = Vec::new();

    for (idx, (session, site_type)) in tabs.iter().enumerate() {
        match SiteHealthChecker::check(session, *site_type).await {
            Ok(result) => {
                results.push((idx, result));
            }
            Err(e) => {
                warn!("标签页 {} 健康检查失败: {}", idx, e);
                results.push((
                    idx,
                    HealthCheckResult {
                        status: SiteHealthStatus::NetworkError,
                        timestamp: 0,
                        message: Some(e.to_string()),
                        current_url: None,
                        check_duration_ms: 0,
                    },
                ));
            }
        }
    }

    // 按健康状态排序: Healthy 优先
    results.sort_by_key(|(_, r)| !r.is_healthy());
    results
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ===== SiteHealthStatus 测试 =====

    #[test]
    fn test_site_health_status_is_healthy() {
        assert!(SiteHealthStatus::Healthy.is_healthy());
        assert!(!SiteHealthStatus::NotLoggedIn.is_healthy());
        assert!(!SiteHealthStatus::RateLimited.is_healthy());
        assert!(!SiteHealthStatus::UnderMaintenance.is_healthy());
        assert!(!SiteHealthStatus::NetworkError.is_healthy());
        assert!(!SiteHealthStatus::Unknown.is_healthy());
    }

    #[test]
    fn test_site_health_status_should_failover() {
        assert!(!SiteHealthStatus::Healthy.should_failover());
        assert!(SiteHealthStatus::NotLoggedIn.should_failover());
        assert!(SiteHealthStatus::RateLimited.should_failover());
        assert!(SiteHealthStatus::UnderMaintenance.should_failover());
        assert!(SiteHealthStatus::NetworkError.should_failover());
        assert!(!SiteHealthStatus::Unknown.should_failover());
    }

    #[test]
    fn test_site_health_status_description() {
        assert!(SiteHealthStatus::Healthy.description().contains("健康"));
        assert!(SiteHealthStatus::NotLoggedIn.description().contains("登录"));
        assert!(SiteHealthStatus::RateLimited.description().contains("限流"));
        assert!(SiteHealthStatus::UnderMaintenance
            .description()
            .contains("维护"));
        assert!(SiteHealthStatus::NetworkError
            .description()
            .contains("网络"));
        assert!(SiteHealthStatus::Unknown.description().contains("未知"));
    }

    #[test]
    fn test_site_health_status_display() {
        let s = format!("{}", SiteHealthStatus::Healthy);
        assert!(s.contains("健康"));
    }

    #[test]
    fn test_site_health_status_default() {
        assert_eq!(SiteHealthStatus::default(), SiteHealthStatus::Unknown);
    }

    #[test]
    fn test_site_health_status_serde() {
        let json = serde_json::to_string(&SiteHealthStatus::RateLimited).unwrap();
        let parsed: SiteHealthStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, SiteHealthStatus::RateLimited);
    }

    // ===== HealthCheckResult 测试 =====

    #[test]
    fn test_health_check_result_new() {
        let result = HealthCheckResult::new(SiteHealthStatus::Healthy);
        assert!(result.is_healthy());
        assert!(!result.should_failover());
    }

    #[test]
    fn test_health_check_result_failover() {
        let result = HealthCheckResult::new(SiteHealthStatus::RateLimited);
        assert!(!result.is_healthy());
        assert!(result.should_failover());
    }

    #[test]
    fn test_health_check_result_serde() {
        let result = HealthCheckResult {
            status: SiteHealthStatus::NotLoggedIn,
            timestamp: 12345,
            message: Some("test".to_string()),
            current_url: Some("https://example.com/login".to_string()),
            check_duration_ms: 100,
        };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: HealthCheckResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.status, SiteHealthStatus::NotLoggedIn);
        assert_eq!(parsed.message, Some("test".to_string()));
    }

    // ===== SiteHealthChecker JS 构建测试 =====

    #[test]
    fn test_build_check_js_contains_input_selector() {
        let js = SiteHealthChecker::build_check_js(SiteType::Zai);
        assert!(js.contains("chat-input"), "JS 应包含 Z.ai 的 #chat-input");
    }

    #[test]
    fn test_build_check_js_contains_deepseek_input() {
        let js = SiteHealthChecker::build_check_js(SiteType::DeepSeek);
        assert!(
            js.contains("ds-scroll-area"),
            "JS 应包含 DeepSeek 的 ds-scroll-area"
        );
    }

    #[test]
    fn test_build_check_js_contains_login_patterns() {
        let js = SiteHealthChecker::build_check_js(SiteType::Zai);
        assert!(js.contains("login"), "JS 应包含 login 检测");
        assert!(js.contains("signin"), "JS 应包含 signin 检测");
    }

    #[test]
    fn test_build_check_js_contains_rate_limit_patterns() {
        let js = SiteHealthChecker::build_check_js(SiteType::DeepSeek);
        assert!(js.contains("频繁"), "JS 应包含限流检测");
        assert!(js.contains("稍后再试"), "JS 应包含限流检测");
    }

    #[test]
    fn test_build_check_js_contains_maintenance_patterns() {
        let js = SiteHealthChecker::build_check_js(SiteType::Unknown);
        assert!(js.contains("维护"), "JS 应包含维护检测");
        assert!(js.contains("maintenance"), "JS 应包含 maintenance 检测");
    }

    #[test]
    fn test_build_check_js_returns_json() {
        let js = SiteHealthChecker::build_check_js(SiteType::Zai);
        assert!(js.contains("JSON.stringify"), "JS 应返回 JSON 字符串");
        assert!(js.contains("hasInput"), "JS 应检测 hasInput");
        assert!(js.contains("hasLoginButton"), "JS 应检测 hasLoginButton");
        assert!(js.contains("hasRateLimit"), "JS 应检测 hasRateLimit");
        assert!(js.contains("hasMaintenance"), "JS 应检测 hasMaintenance");
    }

    #[test]
    fn test_build_check_js_all_site_types() {
        for site in [
            SiteType::Zai,
            SiteType::DeepSeek,
            SiteType::Kimi,
            SiteType::Tongyi,
            SiteType::Claude,
            SiteType::Unknown,
        ] {
            let js = SiteHealthChecker::build_check_js(site);
            assert!(!js.is_empty(), "{} 的 JS 不应为空", site);
            assert!(
                js.contains("querySelector"),
                "{} 的 JS 应包含 querySelector",
                site
            );
        }
    }

    // ===== interpret_result 测试 =====

    #[test]
    fn test_interpret_result_healthy_with_input() {
        let parsed = HealthCheckJson {
            url: "https://chat.z.ai/".to_string(),
            has_input: true,
            has_login_button: false,
            has_rate_limit: false,
            has_maintenance: false,
            message: String::new(),
        };
        assert_eq!(
            SiteHealthChecker::interpret_result(&parsed, SiteType::Zai),
            SiteHealthStatus::Healthy
        );
    }

    #[test]
    fn test_interpret_result_not_logged_in() {
        let parsed = HealthCheckJson {
            url: "https://chat.z.ai/login".to_string(),
            has_input: false,
            has_login_button: true,
            has_rate_limit: false,
            has_maintenance: false,
            message: "URL 重定向到登录页".to_string(),
        };
        assert_eq!(
            SiteHealthChecker::interpret_result(&parsed, SiteType::Zai),
            SiteHealthStatus::NotLoggedIn
        );
    }

    #[test]
    fn test_interpret_result_rate_limited() {
        let parsed = HealthCheckJson {
            url: "https://chat.deepseek.com/".to_string(),
            has_input: true,
            has_login_button: false,
            has_rate_limit: true,
            has_maintenance: false,
            message: "频繁".to_string(),
        };
        assert_eq!(
            SiteHealthChecker::interpret_result(&parsed, SiteType::DeepSeek),
            SiteHealthStatus::RateLimited
        );
    }

    #[test]
    fn test_interpret_result_maintenance() {
        let parsed = HealthCheckJson {
            url: "https://chat.z.ai/".to_string(),
            has_input: false,
            has_login_button: false,
            has_rate_limit: false,
            has_maintenance: true,
            message: "维护中".to_string(),
        };
        assert_eq!(
            SiteHealthChecker::interpret_result(&parsed, SiteType::Zai),
            SiteHealthStatus::UnderMaintenance
        );
    }

    #[test]
    fn test_interpret_result_unknown_no_input() {
        let parsed = HealthCheckJson {
            url: "https://chat.z.ai/".to_string(),
            has_input: false,
            has_login_button: false,
            has_rate_limit: false,
            has_maintenance: false,
            message: String::new(),
        };
        assert_eq!(
            SiteHealthChecker::interpret_result(&parsed, SiteType::Zai),
            SiteHealthStatus::Unknown
        );
    }

    #[test]
    fn test_interpret_result_healthy_with_login_and_input() {
        // 页面有登录按钮但输入框也可用 → 健康 (登录按钮可能是导航栏中的)
        let parsed = HealthCheckJson {
            url: "https://chat.z.ai/".to_string(),
            has_input: true,
            has_login_button: true,
            has_rate_limit: false,
            has_maintenance: false,
            message: String::new(),
        };
        assert_eq!(
            SiteHealthChecker::interpret_result(&parsed, SiteType::Zai),
            SiteHealthStatus::Healthy
        );
    }

    #[test]
    fn test_interpret_result_maintenance_overrides_rate_limit() {
        // 维护优先级高于限流
        let parsed = HealthCheckJson {
            url: "https://chat.z.ai/".to_string(),
            has_input: false,
            has_login_button: false,
            has_rate_limit: true,
            has_maintenance: true,
            message: String::new(),
        };
        assert_eq!(
            SiteHealthChecker::interpret_result(&parsed, SiteType::Zai),
            SiteHealthStatus::UnderMaintenance
        );
    }

    // ===== 网站特定配置测试 =====

    #[test]
    fn test_input_selector_zai() {
        assert!(SiteHealthChecker::input_selector(SiteType::Zai).contains("chat-input"));
    }

    #[test]
    fn test_input_selector_deepseek() {
        assert!(SiteHealthChecker::input_selector(SiteType::DeepSeek).contains("ds-scroll-area"));
    }

    #[test]
    fn test_input_selector_kimi() {
        assert!(SiteHealthChecker::input_selector(SiteType::Kimi).contains("contenteditable"));
    }

    #[test]
    fn test_input_selector_tongyi() {
        assert!(SiteHealthChecker::input_selector(SiteType::Tongyi).contains("contenteditable"));
    }

    #[test]
    fn test_input_selector_claude() {
        assert!(SiteHealthChecker::input_selector(SiteType::Claude).contains("ProseMirror"));
    }

    #[test]
    fn test_login_url_patterns() {
        for site in [
            SiteType::Zai,
            SiteType::DeepSeek,
            SiteType::Kimi,
            SiteType::Tongyi,
            SiteType::Claude,
        ] {
            let patterns = SiteHealthChecker::login_url_patterns(site);
            assert!(
                patterns.iter().any(|p| p.contains("login")),
                "{} 应包含 login",
                site
            );
        }
    }

    #[test]
    fn test_rate_limit_patterns_contain_key_terms() {
        let patterns = SiteHealthChecker::rate_limit_patterns(SiteType::Unknown);
        assert!(patterns.iter().any(|p| p.contains("频繁")));
        assert!(patterns.iter().any(|p| p.contains("稍后再试")));
    }

    #[test]
    fn test_rate_limit_patterns_contain_zai_specific() {
        // Z.ai 特定限流文本 (第 39 项改进)
        let patterns = SiteHealthChecker::rate_limit_patterns(SiteType::Unknown);
        assert!(
            patterns.iter().any(|p| p.contains("请求过于频繁")),
            "应包含 '请求过于频繁'"
        );
        assert!(
            patterns.iter().any(|p| p.contains("回复内容为空")),
            "应包含 '回复内容为空'"
        );
        assert!(
            patterns.iter().any(|p| p.contains("请稍后重试")),
            "应包含 '请稍后重试'"
        );
    }

    // ===== check_response_for_rate_limit 测试 (第 39 项改进) =====

    #[test]
    fn test_check_response_rate_limit_detected() {
        // Z.ai 限流回复: "回复内容为空，请稍后重试"
        let result = SiteHealthChecker::check_response_for_rate_limit("回复内容为空，请稍后重试");
        assert!(result.is_some(), "应检测到限流");
        let health = result.unwrap();
        assert_eq!(health.status, SiteHealthStatus::RateLimited);
        assert!(health.message.as_ref().unwrap().contains("限流"));
    }

    #[test]
    fn test_check_response_rate_limit_empty_text() {
        let result = SiteHealthChecker::check_response_for_rate_limit("");
        assert!(result.is_none(), "空文本不应检测到限流");
    }

    #[test]
    fn test_check_response_rate_limit_normal_code() {
        // 正常的代码回复不应被判定为限流
        let text = "file:src/main.rs\nfn main() { println!(\"hello\"); }";
        let result = SiteHealthChecker::check_response_for_rate_limit(text);
        assert!(result.is_none(), "正常代码回复不应检测到限流");
    }

    #[test]
    fn test_check_response_rate_limit_zai_patterns() {
        // 测试各种 Z.ai 限流文本
        let test_cases = vec![
            "请求过于频繁",
            "回复内容为空，请稍后重试",
            "操作太频繁，请稍候再试",
            "request was throttled",
            "try again later",
        ];
        for case in test_cases {
            let result = SiteHealthChecker::check_response_for_rate_limit(case);
            assert!(result.is_some(), "应检测到限流: {}", case);
            assert_eq!(result.unwrap().status, SiteHealthStatus::RateLimited);
        }
    }

    #[test]
    fn test_check_response_rate_limit_normal_long_text() {
        // 长篇代码回复不应被误判为限流
        let text = "file:Cargo.toml\n[package]\nname = \"test\"\nversion = \"0.1.0\"\n\nfile:src/main.rs\nfn main() {\n    println!(\"Hello, world!\");\n}\n";
        let result = SiteHealthChecker::check_response_for_rate_limit(text);
        assert!(result.is_none(), "正常的长代码回复不应检测到限流");
    }

    #[test]
    fn test_maintenance_patterns_contain_key_terms() {
        let patterns = SiteHealthChecker::maintenance_patterns(SiteType::Unknown);
        assert!(patterns.iter().any(|p| p.contains("维护")));
        assert!(patterns.iter().any(|p| p.contains("maintenance")));
    }

    // ===== SiteFailover 测试 =====

    #[test]
    fn test_site_failover_new() {
        let failover = SiteFailover::new(vec![0, 1, 2], 0);
        assert_eq!(failover.current_tab, 0);
        assert_eq!(failover.available_tabs, vec![0, 1, 2]);
        assert_eq!(failover.tried_tabs, vec![0]);
        assert_eq!(failover.consecutive_failures, 0);
    }

    #[test]
    fn test_site_failover_should_switch_healthy() {
        let failover = SiteFailover::new(vec![0, 1, 2], 0);
        let health = HealthCheckResult::new(SiteHealthStatus::Healthy);
        assert!(failover.should_switch(&health).is_none());
    }

    #[test]
    fn test_site_failover_should_switch_rate_limited() {
        let failover = SiteFailover::new(vec![0, 1, 2], 0);
        let health = HealthCheckResult::new(SiteHealthStatus::RateLimited);
        // 应返回 Some(1) — 下一个可用标签页
        assert_eq!(failover.should_switch(&health), Some(1));
    }

    #[test]
    fn test_site_failover_should_switch_not_logged_in() {
        let failover = SiteFailover::new(vec![0, 1], 0);
        let health = HealthCheckResult::new(SiteHealthStatus::NotLoggedIn);
        assert_eq!(failover.should_switch(&health), Some(1));
    }

    #[test]
    fn test_site_failover_should_switch_unknown_no_failover() {
        let failover = SiteFailover::new(vec![0, 1], 0);
        let health = HealthCheckResult::new(SiteHealthStatus::Unknown);
        // Unknown 不触发 failover
        assert!(failover.should_switch(&health).is_none());
    }

    #[test]
    fn test_site_failover_record_switch() {
        let mut failover = SiteFailover::new(vec![0, 1, 2], 0);
        failover.record_switch(1);
        assert_eq!(failover.current_tab, 1);
        assert!(failover.tried_tabs.contains(&1));
        assert!(failover.last_switch_time.is_some());
    }

    #[test]
    fn test_site_failover_record_success_resets() {
        let mut failover = SiteFailover::new(vec![0, 1, 2], 0);
        failover.record_failure();
        failover.record_failure();
        failover.record_success();
        assert_eq!(failover.consecutive_failures, 0);
        assert_eq!(failover.tried_tabs.len(), 1); // 只包含当前 tab
    }

    #[test]
    fn test_site_failover_record_failure_increments() {
        let mut failover = SiteFailover::new(vec![0, 1], 0);
        failover.record_failure();
        assert_eq!(failover.consecutive_failures, 1);
        failover.record_failure();
        assert_eq!(failover.consecutive_failures, 2);
    }

    #[test]
    fn test_site_failover_all_tried() {
        let mut failover = SiteFailover::new(vec![0, 1, 2], 0);
        assert!(!failover.all_tried());
        failover.record_switch(1);
        failover.record_switch(2);
        assert!(failover.all_tried());
    }

    #[test]
    fn test_site_failover_reset() {
        let mut failover = SiteFailover::new(vec![0, 1, 2], 0);
        failover.record_switch(1);
        failover.record_failure();
        failover.reset();
        assert_eq!(failover.consecutive_failures, 0);
        assert_eq!(failover.tried_tabs.len(), 1);
        assert!(failover.last_switch_time.is_none());
    }

    #[test]
    fn test_site_failover_max_consecutive_failures() {
        let failover = SiteFailover::new(vec![0, 1], 0).with_max_failures(2);
        let health = HealthCheckResult::new(SiteHealthStatus::RateLimited);

        // 第一次失败: 可以切换
        let mut f = failover.clone();
        assert!(f.should_switch(&health).is_some());
        f.record_failure();

        // 第二次失败: 仍可以切换 (1 < 2)
        assert!(f.should_switch(&health).is_some());
        f.record_failure();

        // 第三次失败: 超过最大, 不能切换
        assert!(f.should_switch(&health).is_none());
    }

    #[test]
    fn test_site_failover_with_max_failures() {
        let failover = SiteFailover::new(vec![0, 1], 0).with_max_failures(5);
        assert_eq!(failover.max_consecutive_failures, 5);
    }

    #[test]
    fn test_site_failover_with_cooldown() {
        let failover = SiteFailover::new(vec![0, 1], 0).with_cooldown(60);
        assert_eq!(failover.cooldown_secs, 60);
    }

    #[test]
    fn test_site_failover_no_available_tabs() {
        // 只有一个标签页, 无法切换
        let failover = SiteFailover::new(vec![0], 0);
        let health = HealthCheckResult::new(SiteHealthStatus::RateLimited);
        assert!(failover.should_switch(&health).is_none());
    }

    #[test]
    fn test_site_failover_tried_all_available() {
        // 所有标签页都已尝试
        let mut failover = SiteFailover::new(vec![0, 1], 0);
        failover.record_switch(1);
        let health = HealthCheckResult::new(SiteHealthStatus::RateLimited);
        // 没有更多标签页可切换
        assert!(failover.should_switch(&health).is_none());
    }

    // ===== js_array 测试 =====

    #[test]
    fn test_js_array_empty() {
        let result = SiteHealthChecker::js_array(&[]);
        assert_eq!(result, "[]");
    }

    #[test]
    fn test_js_array_single() {
        let result = SiteHealthChecker::js_array(&["login"]);
        assert_eq!(result, "['login']");
    }

    #[test]
    fn test_js_array_multiple() {
        let result = SiteHealthChecker::js_array(&["login", "signin"]);
        assert_eq!(result, "['login', 'signin']");
    }

    #[test]
    fn test_js_array_escape_quotes() {
        let result = SiteHealthChecker::js_array(&["it's"]);
        assert_eq!(result, "['it\\'s']");
    }

    // ===== HealthCheckJson 测试 =====

    #[test]
    fn test_health_check_json_default() {
        let json = HealthCheckJson::default();
        assert!(!json.has_input);
        assert!(!json.has_login_button);
        assert!(!json.has_rate_limit);
        assert!(!json.has_maintenance);
        assert!(json.url.is_empty());
        assert!(json.message.is_empty());
    }

    #[test]
    fn test_health_check_json_deserialize() {
        let json_str = r#"{
            "url": "https://chat.z.ai/",
            "hasInput": true,
            "hasLoginButton": false,
            "hasRateLimit": false,
            "hasMaintenance": false,
            "message": ""
        }"#;
        let parsed: HealthCheckJson = serde_json::from_str(json_str).unwrap();
        assert!(parsed.has_input);
        assert_eq!(parsed.url, "https://chat.z.ai/");
    }

    #[test]
    fn test_health_check_json_deserialize_with_defaults() {
        // 缺少字段时应使用默认值
        let json_str = r#"{"url": "test"}"#;
        let parsed: HealthCheckJson = serde_json::from_str(json_str).unwrap();
        assert_eq!(parsed.url, "test");
        assert!(!parsed.has_input);
    }

    // ===== HealthCheckJson builder 测试 =====

    #[test]
    fn test_health_check_json_new() {
        let json = HealthCheckJson::new();
        assert!(!json.has_input);
        assert!(json.url.is_empty());
    }

    #[test]
    fn test_health_check_json_builder_chain() {
        let json = HealthCheckJson::new()
            .with_url("https://chat.z.ai/")
            .with_input(true)
            .with_login_button(false)
            .with_rate_limit(false)
            .with_maintenance(false)
            .with_message("ok");
        assert_eq!(json.url, "https://chat.z.ai/");
        assert!(json.has_input);
        assert_eq!(json.message, "ok");
    }

    #[test]
    fn test_health_check_json_clone() {
        let json = HealthCheckJson::new().with_input(true).with_url("test");
        let cloned = json.clone();
        assert_eq!(json, cloned);
    }

    #[test]
    fn test_health_check_json_partial_eq() {
        let a = HealthCheckJson::new().with_input(true);
        let b = HealthCheckJson::new().with_input(true);
        let c = HealthCheckJson::new().with_input(false);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    // ===== interpret_health_json 纯函数测试 =====

    #[test]
    fn test_interpret_health_json_healthy_with_input() {
        let json = HealthCheckJson::new().with_input(true);
        assert_eq!(interpret_health_json(&json), SiteHealthStatus::Healthy);
    }

    #[test]
    fn test_interpret_health_json_maintenance_highest_priority() {
        let json = HealthCheckJson::new()
            .with_maintenance(true)
            .with_rate_limit(true)
            .with_login_button(true)
            .with_input(true);
        assert_eq!(
            interpret_health_json(&json),
            SiteHealthStatus::UnderMaintenance
        );
    }

    #[test]
    fn test_interpret_health_json_rate_limit_over_login() {
        let json = HealthCheckJson::new()
            .with_rate_limit(true)
            .with_login_button(true)
            .with_input(false);
        assert_eq!(interpret_health_json(&json), SiteHealthStatus::RateLimited);
    }

    #[test]
    fn test_interpret_health_json_not_logged_in_no_input() {
        let json = HealthCheckJson::new()
            .with_login_button(true)
            .with_input(false);
        assert_eq!(interpret_health_json(&json), SiteHealthStatus::NotLoggedIn);
    }

    #[test]
    fn test_interpret_health_json_login_with_input_healthy() {
        let json = HealthCheckJson::new()
            .with_login_button(true)
            .with_input(true);
        assert_eq!(interpret_health_json(&json), SiteHealthStatus::Healthy);
    }

    #[test]
    fn test_interpret_health_json_unknown_no_indicators() {
        let json = HealthCheckJson::new();
        assert_eq!(interpret_health_json(&json), SiteHealthStatus::Unknown);
    }

    #[test]
    fn test_interpret_health_json_empty_url() {
        let json = HealthCheckJson::new().with_url("").with_input(true);
        assert_eq!(interpret_health_json(&json), SiteHealthStatus::Healthy);
    }

    #[test]
    fn test_interpret_health_json_all_false() {
        let json = HealthCheckJson::new()
            .with_input(false)
            .with_login_button(false)
            .with_rate_limit(false)
            .with_maintenance(false);
        assert_eq!(interpret_health_json(&json), SiteHealthStatus::Unknown);
    }

    #[test]
    fn test_interpret_health_json_consistent_with_interpret_result() {
        // 验证纯函数与 SiteHealthChecker::interpret_result 一致
        for has_input in [true, false] {
            for has_login in [true, false] {
                for has_rate in [true, false] {
                    for has_maint in [true, false] {
                        let json = HealthCheckJson::new()
                            .with_input(has_input)
                            .with_login_button(has_login)
                            .with_rate_limit(has_rate)
                            .with_maintenance(has_maint);
                        let pure = interpret_health_json(&json);
                        let method = SiteHealthChecker::interpret_result(&json, SiteType::Zai);
                        assert_eq!(
                            pure, method,
                            "不一致: input={}, login={}, rate={}, maint={}",
                            has_input, has_login, has_rate, has_maint
                        );
                    }
                }
            }
        }
    }

    // ===== HealthSeverity 测试 =====

    #[test]
    fn test_health_severity_description() {
        assert_eq!(HealthSeverity::Info.description(), "信息");
        assert_eq!(HealthSeverity::Warning.description(), "警告");
        assert_eq!(HealthSeverity::Critical.description(), "严重");
        assert_eq!(HealthSeverity::Unknown.description(), "未知");
    }

    #[test]
    fn test_health_severity_requires_immediate_failover() {
        assert!(!HealthSeverity::Info.requires_immediate_failover());
        assert!(!HealthSeverity::Warning.requires_immediate_failover());
        assert!(HealthSeverity::Critical.requires_immediate_failover());
        assert!(!HealthSeverity::Unknown.requires_immediate_failover());
    }

    #[test]
    fn test_health_severity_display() {
        assert_eq!(HealthSeverity::Info.to_string(), "信息");
        assert_eq!(HealthSeverity::Warning.to_string(), "警告");
        assert_eq!(HealthSeverity::Critical.to_string(), "严重");
        assert_eq!(HealthSeverity::Unknown.to_string(), "未知");
    }

    #[test]
    fn test_health_severity_serde() {
        let sev = HealthSeverity::Critical;
        let json = serde_json::to_string(&sev).unwrap();
        let parsed: HealthSeverity = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, sev);
    }

    // ===== classify_health_severity 测试 =====

    #[test]
    fn test_classify_health_severity_healthy() {
        assert_eq!(
            classify_health_severity(&SiteHealthStatus::Healthy),
            HealthSeverity::Info
        );
    }

    #[test]
    fn test_classify_health_severity_rate_limited() {
        assert_eq!(
            classify_health_severity(&SiteHealthStatus::RateLimited),
            HealthSeverity::Warning
        );
    }

    #[test]
    fn test_classify_health_severity_not_logged_in() {
        assert_eq!(
            classify_health_severity(&SiteHealthStatus::NotLoggedIn),
            HealthSeverity::Warning
        );
    }

    #[test]
    fn test_classify_health_severity_under_maintenance() {
        assert_eq!(
            classify_health_severity(&SiteHealthStatus::UnderMaintenance),
            HealthSeverity::Critical
        );
    }

    #[test]
    fn test_classify_health_severity_network_error() {
        assert_eq!(
            classify_health_severity(&SiteHealthStatus::NetworkError),
            HealthSeverity::Critical
        );
    }

    #[test]
    fn test_classify_health_severity_unknown() {
        assert_eq!(
            classify_health_severity(&SiteHealthStatus::Unknown),
            HealthSeverity::Unknown
        );
    }

    #[test]
    fn test_classify_health_severity_all_variants() {
        // 遍历所有变体, 确保无 panic
        for status in [
            SiteHealthStatus::Healthy,
            SiteHealthStatus::NotLoggedIn,
            SiteHealthStatus::RateLimited,
            SiteHealthStatus::UnderMaintenance,
            SiteHealthStatus::NetworkError,
            SiteHealthStatus::Unknown,
        ] {
            let sev = classify_health_severity(&status);
            assert!(!sev.description().is_empty());
        }
    }

    // ===== compute_health_check_interval 测试 =====

    #[test]
    fn test_compute_health_check_interval_healthy() {
        assert_eq!(
            compute_health_check_interval(&SiteHealthStatus::Healthy, 60),
            60
        );
    }

    #[test]
    fn test_compute_health_check_interval_rate_limited() {
        // 60 * 0.25 = 15
        assert_eq!(
            compute_health_check_interval(&SiteHealthStatus::RateLimited, 60),
            15
        );
    }

    #[test]
    fn test_compute_health_check_interval_not_logged_in() {
        // 60 * 0.5 = 30
        assert_eq!(
            compute_health_check_interval(&SiteHealthStatus::NotLoggedIn, 60),
            30
        );
    }

    #[test]
    fn test_compute_health_check_interval_under_maintenance() {
        // 60 * 2.0 = 120
        assert_eq!(
            compute_health_check_interval(&SiteHealthStatus::UnderMaintenance, 60),
            120
        );
    }

    #[test]
    fn test_compute_health_check_interval_network_error() {
        // 60 * 0.25 = 15
        assert_eq!(
            compute_health_check_interval(&SiteHealthStatus::NetworkError, 60),
            15
        );
    }

    #[test]
    fn test_compute_health_check_interval_unknown() {
        // 60 * 0.5 = 30
        assert_eq!(
            compute_health_check_interval(&SiteHealthStatus::Unknown, 60),
            30
        );
    }

    #[test]
    fn test_compute_health_check_interval_min_1_second() {
        // 即使 base_interval 很小, 最小返回 1
        assert_eq!(
            compute_health_check_interval(&SiteHealthStatus::RateLimited, 1),
            1
        );
    }

    #[test]
    fn test_compute_health_check_interval_zero_base() {
        // base=0 → 0*0.25=0 → max(0,1)=1
        assert_eq!(
            compute_health_check_interval(&SiteHealthStatus::Healthy, 0),
            1
        );
    }

    #[test]
    fn test_compute_health_check_interval_large_base() {
        // base=3600, maintenance → 7200
        assert_eq!(
            compute_health_check_interval(&SiteHealthStatus::UnderMaintenance, 3600),
            7200
        );
    }

    #[test]
    fn test_compute_health_check_interval_ordering() {
        // 维护 > 健康 > 未登录/未知 > 限流/网络错误
        let base = 60u64;
        let maintenance = compute_health_check_interval(&SiteHealthStatus::UnderMaintenance, base);
        let healthy = compute_health_check_interval(&SiteHealthStatus::Healthy, base);
        let rate_limited = compute_health_check_interval(&SiteHealthStatus::RateLimited, base);

        assert!(maintenance > healthy);
        assert!(healthy > rate_limited);
    }

    // ===== format_health_result_line 测试 =====

    #[test]
    fn test_format_health_result_line_healthy() {
        let result = HealthCheckResult::new(SiteHealthStatus::Healthy);
        let line = format_health_result_line(0, SiteType::Zai, &result);
        assert!(line.contains("[0]"));
        assert!(line.contains("Z.ai"));
        assert!(line.contains("健康"));
        assert!(line.contains("0ms"));
    }

    #[test]
    fn test_format_health_result_line_with_message() {
        let result = HealthCheckResult {
            status: SiteHealthStatus::RateLimited,
            timestamp: 0,
            message: Some("请求过于频繁".to_string()),
            current_url: Some("https://chat.z.ai".to_string()),
            check_duration_ms: 150,
        };
        let line = format_health_result_line(1, SiteType::DeepSeek, &result);
        assert!(line.contains("[1]"));
        assert!(line.contains("DeepSeek"));
        assert!(line.contains("请求过于频繁"));
        assert!(line.contains("150ms"));
    }

    #[test]
    fn test_format_health_result_line_no_message() {
        let result = HealthCheckResult::new(SiteHealthStatus::Unknown);
        let line = format_health_result_line(2, SiteType::Kimi, &result);
        // 无 message 时使用 description
        assert!(line.contains("未知"));
    }

    #[test]
    fn test_format_health_result_line_large_index() {
        let result = HealthCheckResult::new(SiteHealthStatus::Healthy);
        let line = format_health_result_line(999, SiteType::Claude, &result);
        assert!(line.contains("[999]"));
    }

    #[test]
    fn test_format_health_result_line_all_site_types() {
        let result = HealthCheckResult::new(SiteHealthStatus::Healthy);
        for (idx, site) in [
            SiteType::Zai,
            SiteType::DeepSeek,
            SiteType::Kimi,
            SiteType::Tongyi,
            SiteType::Claude,
            SiteType::Unknown,
        ]
        .iter()
        .enumerate()
        {
            let line = format_health_result_line(idx, *site, &result);
            assert!(!line.is_empty());
            assert!(line.contains(&format!("[{}]", idx)));
        }
    }

    // ===== determine_failover_priority 测试 =====

    #[test]
    fn test_determine_failover_priority_healthy() {
        assert_eq!(determine_failover_priority(&SiteHealthStatus::Healthy), 0);
    }

    #[test]
    fn test_determine_failover_priority_unknown() {
        assert_eq!(determine_failover_priority(&SiteHealthStatus::Unknown), 1);
    }

    #[test]
    fn test_determine_failover_priority_not_logged_in() {
        assert_eq!(
            determine_failover_priority(&SiteHealthStatus::NotLoggedIn),
            2
        );
    }

    #[test]
    fn test_determine_failover_priority_rate_limited() {
        assert_eq!(
            determine_failover_priority(&SiteHealthStatus::RateLimited),
            3
        );
    }

    #[test]
    fn test_determine_failover_priority_network_error() {
        assert_eq!(
            determine_failover_priority(&SiteHealthStatus::NetworkError),
            4
        );
    }

    #[test]
    fn test_determine_failover_priority_under_maintenance() {
        assert_eq!(
            determine_failover_priority(&SiteHealthStatus::UnderMaintenance),
            5
        );
    }

    #[test]
    fn test_determine_failover_priority_ordering() {
        // Healthy < Unknown < NotLoggedIn < RateLimited < NetworkError < UnderMaintenance
        let healthy = determine_failover_priority(&SiteHealthStatus::Healthy);
        let unknown = determine_failover_priority(&SiteHealthStatus::Unknown);
        let not_logged = determine_failover_priority(&SiteHealthStatus::NotLoggedIn);
        let rate = determine_failover_priority(&SiteHealthStatus::RateLimited);
        let network = determine_failover_priority(&SiteHealthStatus::NetworkError);
        let maintenance = determine_failover_priority(&SiteHealthStatus::UnderMaintenance);

        assert!(healthy < unknown);
        assert!(unknown < not_logged);
        assert!(not_logged < rate);
        assert!(rate < network);
        assert!(network < maintenance);
    }

    // ===== select_best_healthy_tab 测试 =====

    #[test]
    fn test_select_best_healthy_tab_empty() {
        let results: Vec<(usize, HealthCheckResult)> = vec![];
        assert_eq!(select_best_healthy_tab(&results), None);
    }

    #[test]
    fn test_select_best_healthy_tab_first_healthy() {
        let results = vec![
            (0, HealthCheckResult::new(SiteHealthStatus::RateLimited)),
            (1, HealthCheckResult::new(SiteHealthStatus::Healthy)),
            (2, HealthCheckResult::new(SiteHealthStatus::Healthy)),
        ];
        assert_eq!(select_best_healthy_tab(&results), Some(1));
    }

    #[test]
    fn test_select_best_healthy_tab_no_healthy_returns_lowest_priority() {
        let results = vec![
            (
                0,
                HealthCheckResult::new(SiteHealthStatus::UnderMaintenance),
            ),
            (1, HealthCheckResult::new(SiteHealthStatus::RateLimited)),
            (2, HealthCheckResult::new(SiteHealthStatus::NetworkError)),
        ];
        // RateLimited (priority 3) < NetworkError (4) < UnderMaintenance (5)
        assert_eq!(select_best_healthy_tab(&results), Some(1));
    }

    #[test]
    fn test_select_best_healthy_tab_single_healthy() {
        let results = vec![(5, HealthCheckResult::new(SiteHealthStatus::Healthy))];
        assert_eq!(select_best_healthy_tab(&results), Some(5));
    }

    #[test]
    fn test_select_best_healthy_tab_single_unhealthy() {
        let results = vec![(3, HealthCheckResult::new(SiteHealthStatus::RateLimited))];
        assert_eq!(select_best_healthy_tab(&results), Some(3));
    }

    #[test]
    fn test_select_best_healthy_tab_all_unknown() {
        let results = vec![
            (0, HealthCheckResult::new(SiteHealthStatus::Unknown)),
            (1, HealthCheckResult::new(SiteHealthStatus::Unknown)),
        ];
        // 所有优先级相同, 返回第一个 (min_by_key 在相等时返回第一个)
        assert_eq!(select_best_healthy_tab(&results), Some(0));
    }

    #[test]
    fn test_select_best_healthy_tab_mixed() {
        let results = vec![
            (0, HealthCheckResult::new(SiteHealthStatus::NotLoggedIn)),
            (1, HealthCheckResult::new(SiteHealthStatus::Unknown)),
            (2, HealthCheckResult::new(SiteHealthStatus::Healthy)),
            (3, HealthCheckResult::new(SiteHealthStatus::RateLimited)),
        ];
        // 有健康的 → 返回 2
        assert_eq!(select_best_healthy_tab(&results), Some(2));
    }

    // ===== should_skip_tab 测试 =====

    #[test]
    fn test_should_skip_tab_below_threshold() {
        assert!(!should_skip_tab(0, 3));
        assert!(!should_skip_tab(1, 3));
        assert!(!should_skip_tab(2, 3));
    }

    #[test]
    fn test_should_skip_tab_at_threshold() {
        assert!(should_skip_tab(3, 3));
    }

    #[test]
    fn test_should_skip_tab_above_threshold() {
        assert!(should_skip_tab(4, 3));
        assert!(should_skip_tab(10, 3));
        assert!(should_skip_tab(100, 3));
    }

    #[test]
    fn test_should_skip_tab_zero_threshold() {
        // threshold=0: 0 >= 0 → true
        assert!(should_skip_tab(0, 0));
    }

    #[test]
    fn test_should_skip_tab_u32_max() {
        assert!(!should_skip_tab(0, u32::MAX));
        assert!(should_skip_tab(u32::MAX, u32::MAX));
    }

    // ===== calculate_health_rate 测试 =====

    #[test]
    fn test_calculate_health_rate_zero_checks() {
        assert!((calculate_health_rate(0, 0) - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_calculate_health_rate_all_healthy() {
        assert!((calculate_health_rate(100, 100) - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_calculate_health_rate_half_healthy() {
        assert!((calculate_health_rate(100, 50) - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_calculate_health_rate_none_healthy() {
        assert!((calculate_health_rate(100, 0) - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_calculate_health_rate_more_healthy_than_checks() {
        // healthy > total → clamped to 1.0
        assert!((calculate_health_rate(10, 20) - 1.0).abs() < 0.001);
    }

    // ===== format_health_rate 测试 =====

    #[test]
    fn test_format_health_rate_full() {
        assert_eq!(format_health_rate(1.0), "100.0%");
    }

    #[test]
    fn test_format_health_rate_zero() {
        assert_eq!(format_health_rate(0.0), "0.0%");
    }

    #[test]
    fn test_format_health_rate_half() {
        assert_eq!(format_health_rate(0.5), "50.0%");
    }

    #[test]
    fn test_format_health_rate_decimal() {
        assert_eq!(format_health_rate(0.833), "83.3%");
    }

    // ===== 协同测试: site_health ↔ connection_monitor ↔ auto_recovery =====

    #[test]
    fn test_synergy_severity_mapping() {
        // 网站健康状态 → 健康严重程度 → 与连接严重程度协同
        use crate::connection_monitor::{classify_connection_severity, ConnectionSeverity};

        // 网站健康 → HealthSeverity::Info
        // 对应 CDP 连接正常 → ConnectionSeverity::Info
        let site_healthy = classify_health_severity(&SiteHealthStatus::Healthy);
        let conn_healthy = ConnectionSeverity::Info;
        assert_eq!(site_healthy, HealthSeverity::Info);
        assert_eq!(
            conn_healthy,
            classify_connection_severity(&crate::connection_monitor::ConnectionStatus::Connected)
        );

        // 网络错误 → HealthSeverity::Critical
        // 对应 CDP ChromeUnreachable → ConnectionSeverity::Critical
        let site_critical = classify_health_severity(&SiteHealthStatus::NetworkError);
        assert_eq!(site_critical, HealthSeverity::Critical);
        assert!(site_critical.requires_immediate_failover());
    }

    #[test]
    fn test_synergy_health_to_recovery_pipeline() {
        // 完整管道: SiteHealthStatus → HealthSeverity → RecoveryUrgency
        use crate::auto_recovery::{assess_recovery_urgency, RecoveryUrgency};
        use crate::connection_monitor::HealthLevel;

        // 网站维护中 → Critical → 需要立即故障转移
        let severity = classify_health_severity(&SiteHealthStatus::UnderMaintenance);
        assert_eq!(severity, HealthSeverity::Critical);
        assert!(severity.requires_immediate_failover());

        // 对应连接健康等级 Critical
        let health_level = HealthLevel::Critical;
        let urgency = assess_recovery_urgency(&health_level);
        assert_eq!(urgency, RecoveryUrgency::Critical);
        assert!(urgency.requires_immediate_recovery());
    }

    #[test]
    fn test_synergy_health_rate_with_monitor_rate() {
        // site_health 的 health_rate 与 connection_monitor 的 success_rate 应该是协同的
        use crate::connection_monitor::calculate_monitor_success_rate;

        // 80% 健康率 (网站) = 80% 成功率 (连接)
        let site_rate = calculate_health_rate(100, 80);
        let conn_rate = calculate_monitor_success_rate(100, 20); // 20 failures → 80% success
        assert!((site_rate - conn_rate).abs() < 0.001);
        assert!((site_rate - 0.8).abs() < 0.001);
    }

    #[test]
    fn test_synergy_failover_with_should_failover_decision() {
        // site_health 的 should_failover 与 failover_chat 的 should_failover_decision 协同
        use crate::failover_chat::should_failover_decision;

        for status in [
            SiteHealthStatus::Healthy,
            SiteHealthStatus::NotLoggedIn,
            SiteHealthStatus::RateLimited,
            SiteHealthStatus::UnderMaintenance,
            SiteHealthStatus::NetworkError,
            SiteHealthStatus::Unknown,
        ] {
            let result = HealthCheckResult::new(status.clone());
            let from_status = result.should_failover();
            let from_decision = should_failover_decision(&result);
            assert_eq!(
                from_status, from_decision,
                "should_failover 不一致: {:?}",
                status
            );
        }
    }

    #[test]
    fn test_synergy_interval_with_monitor_delay() {
        // site_health 的检查间隔与 connection_monitor 的检查延迟协同
        use crate::connection_monitor::compute_next_check_delay;

        // 网站健康 → 基础间隔 60s
        let site_interval = compute_health_check_interval(&SiteHealthStatus::Healthy, 60);
        // 连接正常, heartbeat=60 → 60s
        let conn_delay = compute_next_check_delay(
            &crate::connection_monitor::ConnectionStatus::Connected,
            0,
            60,
        );
        assert_eq!(site_interval, 60);
        assert_eq!(conn_delay, 60);
        assert_eq!(site_interval, conn_delay);
    }

    #[test]
    fn test_synergy_skip_tab_with_continue_retrying() {
        // site_health 的 should_skip_tab 与 auto_recovery 的 should_continue_retrying 协同
        use crate::auto_recovery::should_continue_retrying;

        // 跳过阈值 = 重试上限
        let skip_threshold = 3u32;
        let max_retries = 3u32;

        // 未达到阈值 → 不跳过, 继续重试
        for i in 0..skip_threshold {
            assert!(!should_skip_tab(i, skip_threshold), "不应跳过: {}", i);
            assert!(
                should_continue_retrying(i, max_retries),
                "应继续重试: {}",
                i
            );
        }

        // 达到阈值 → 跳过, 不再重试
        assert!(should_skip_tab(skip_threshold, skip_threshold));
        assert!(!should_continue_retrying(skip_threshold, max_retries));
    }

    #[test]
    fn test_synergy_full_24h_pipeline() {
        // 模拟完整的 24h 可靠性链路:
        // 1. CDP 连接状态 → ConnectionSeverity
        // 2. 网站健康状态 → HealthSeverity
        // 3. 综合评估 → RecoveryUrgency
        // 4. 故障转移决策 → select_best_healthy_tab
        use crate::auto_recovery::{assess_recovery_urgency, RecoveryUrgency};
        use crate::connection_monitor::{
            classify_connection_severity, determine_health_level, ConnectionStatus, HealthLevel,
        };

        // 场景: Chrome 连接正常, 但主网站限流
        let conn_status = ConnectionStatus::Connected;
        let conn_severity = classify_connection_severity(&conn_status);
        assert_eq!(conn_severity.description(), "信息"); // CDP 层正常

        let site_status = SiteHealthStatus::RateLimited;
        let site_severity = classify_health_severity(&site_status);
        assert_eq!(site_severity, HealthSeverity::Warning); // 网站层警告

        // 连接层健康 (因为 CDP 正常)
        let health_level = determine_health_level(&conn_status, 0, 3);
        assert_eq!(health_level, HealthLevel::Healthy);
        let urgency = assess_recovery_urgency(&health_level);
        assert_eq!(urgency, RecoveryUrgency::None); // 不需要恢复

        // 但网站层需要故障转移
        assert!(site_status.should_failover());
        assert!(!site_severity.requires_immediate_failover()); // 限流不是 Critical

        // 选择最佳标签页
        let results = vec![
            (0, HealthCheckResult::new(SiteHealthStatus::RateLimited)),
            (1, HealthCheckResult::new(SiteHealthStatus::Healthy)),
        ];
        let best = select_best_healthy_tab(&results);
        assert_eq!(best, Some(1)); // 切换到健康的标签页 1
    }

    #[test]
    fn test_synergy_maintenance_critical_pipeline() {
        // 场景: 网站维护中 + Chrome 不可达 → 双重 Critical
        use crate::connection_monitor::{
            classify_connection_severity, determine_health_level, ConnectionStatus, HealthLevel,
        };

        let conn_status = ConnectionStatus::ChromeUnreachable;
        let site_status = SiteHealthStatus::UnderMaintenance;

        // 两层都是 Critical
        let conn_severity = classify_connection_severity(&conn_status);
        let site_severity = classify_health_severity(&site_status);

        assert_eq!(conn_severity.description(), "严重");
        assert_eq!(site_severity, HealthSeverity::Critical);
        assert!(site_severity.requires_immediate_failover());

        // 连接层也是 Critical
        let health_level = determine_health_level(&conn_status, 3, 3);
        assert_eq!(health_level, HealthLevel::Critical);
    }
}
