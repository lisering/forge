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

    /// 解释检测结果 — 根据各项指标判断健康状态
    fn interpret_result(parsed: &HealthCheckJson, _site_type: SiteType) -> SiteHealthStatus {
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

/// 健康检查 JS 返回的 JSON 结构 (内部使用)
#[derive(Debug, Default, Deserialize)]
struct HealthCheckJson {
    #[serde(default)]
    url: String,
    #[serde(default, rename = "hasInput")]
    has_input: bool,
    #[serde(default, rename = "hasLoginButton")]
    has_login_button: bool,
    #[serde(default, rename = "hasRateLimit")]
    has_rate_limit: bool,
    #[serde(default, rename = "hasMaintenance")]
    has_maintenance: bool,
    #[serde(default)]
    message: String,
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
}
