//! Chrome 连接监控 — 24h 可靠性强化
//!
//! 监控 Chrome 调试端口和 WebSocket 连接状态, 检测 Chrome 崩溃、标签页关闭等异常。
//! 当检测到连接异常时, 返回 `ConnectionStatus` 供 `AutoRecovery` 决策恢复策略。
//!
//! ## 核心设计
//!
//! 24 小时运行中, Chrome 可能因以下原因断连:
//! - Chrome 进程崩溃 (内存溢出、OOM)
//! - WebSocket 连接断开 (网络中断)
//! - 聊天标签页被关闭
//! - z.ai 服务端重启导致页面不可用
//!
//! `ConnectionMonitor` 通过以下方式检测:
//! 1. HTTP 探测 Chrome 调试端口 (`http://localhost:{port}/json/version`)
//! 2. CDP WebSocket ping (可选, 通过 `Runtime.evaluate` 探活)
//! 3. 标签页列表检查 (聊天标签页是否仍在)
//!
//! ## 与现有机制的关系
//!
//! - **CDP (cdp.rs)**: 底层 WebSocket 连接, 提供发送命令的能力
//! - **BrowserManager (browser.rs)**: 发现并管理聊天标签页
//! - **ConnectionMonitor (本模块)**: 监控连接状态, 不直接操作浏览器
//! - **AutoRecovery (auto_recovery.rs)**: 检测到断连后执行恢复策略

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

// ============================================================================
//  ConnectionStatus — 连接状态枚举
// =============================================================================

/// Chrome 连接状态 — 表示当前连接的健康程度
///
/// `ConnectionMonitor::check_connection()` 返回此枚举,
/// `AutoRecovery` 根据不同的状态选择不同的恢复策略。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionStatus {
    /// 连接正常 — Chrome 可达, 标签页存在
    Connected,
    /// Chrome 调试端口不可达 — Chrome 可能已崩溃或未启动
    ChromeUnreachable,
    /// Chrome 可达但聊天标签页已关闭 — 需要重新发现或创建标签页
    TabClosed,
    /// Chrome 可达、标签页存在, 但 WebSocket 连接异常 — CDP 命令失败
    WebSocketError(String),
    /// 连接检查超时 — 无法在限定时间内完成检查
    CheckTimeout,
}

impl ConnectionStatus {
    /// 是否连接正常
    pub fn is_connected(&self) -> bool {
        matches!(self, ConnectionStatus::Connected)
    }

    /// 是否需要恢复
    pub fn needs_recovery(&self) -> bool {
        !self.is_connected()
    }

    /// 状态的中文描述
    pub fn description(&self) -> &'static str {
        match self {
            ConnectionStatus::Connected => "连接正常",
            ConnectionStatus::ChromeUnreachable => "Chrome 不可达",
            ConnectionStatus::TabClosed => "标签页已关闭",
            ConnectionStatus::WebSocketError(_) => "WebSocket 连接异常",
            ConnectionStatus::CheckTimeout => "连接检查超时",
        }
    }

    /// 恢复难度等级 (1=简单, 3=困难)
    pub fn recovery_difficulty(&self) -> u8 {
        match self {
            ConnectionStatus::Connected => 0,
            ConnectionStatus::TabClosed => 1, // 重新发现标签页
            ConnectionStatus::WebSocketError(_) => 2, // 重新连接 WebSocket
            ConnectionStatus::ChromeUnreachable => 3, // 需要重启 Chrome
            ConnectionStatus::CheckTimeout => 2, // 不确定, 需重试
        }
    }
}

impl std::fmt::Display for ConnectionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionStatus::Connected => write!(f, "Connected"),
            ConnectionStatus::ChromeUnreachable => write!(f, "ChromeUnreachable"),
            ConnectionStatus::TabClosed => write!(f, "TabClosed"),
            ConnectionStatus::WebSocketError(msg) => {
                write!(f, "WebSocketError({})", msg)
            }
            ConnectionStatus::CheckTimeout => write!(f, "CheckTimeout"),
        }
    }
}

// ============================================================================
//  RecoveryEvent — 恢复事件记录
// ============================================================================

/// 一次连接检查/恢复事件的记录
///
/// 用于 DevTrace 记录和日志, 追踪 24 小时运行中的所有连接异常和恢复操作。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryEvent {
    /// 事件发生时间 (从启动开始的毫秒数)
    pub timestamp_ms: u64,
    /// 事件前的连接状态
    pub before_status: ConnectionStatus,
    /// 事件后的连接状态
    pub after_status: ConnectionStatus,
    /// 恢复策略描述 (如 "指数退避重试第3次")
    pub strategy: String,
    /// 恢复耗时 (毫秒)
    pub duration_ms: u64,
    /// 是否恢复成功
    pub success: bool,
    /// 错误信息 (如有)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl RecoveryEvent {
    /// 创建一条恢复事件
    pub fn new(
        timestamp_ms: u64,
        before: ConnectionStatus,
        after: ConnectionStatus,
        strategy: &str,
        duration_ms: u64,
        success: bool,
        error: Option<&str>,
    ) -> Self {
        Self {
            timestamp_ms,
            before_status: before,
            after_status: after,
            strategy: strategy.to_string(),
            duration_ms,
            success,
            error: error.map(String::from),
        }
    }
}

// ============================================================================
//  纯逻辑函数 — 连接监控核心算法 (无副作用, 可独立测试)
// ===========================================================================
//
// 以下函数将连接监控的核心决策逻辑提取为纯函数, 使得:
// 1. 可以在无 Chrome 环境下完全测试
// 2. 决策逻辑集中管理, 修改策略时只需改一处
// 3. 符合 DIP 原则 — ConnectionMonitor 委托纯函数, 不内联逻辑

/// 连接健康等级 — 综合评估连接状态
///
/// 由 [`determine_health_level`] 根据连接状态和连续失败次数计算。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HealthLevel {
    /// 健康 — 连接正常, 无失败
    Healthy,
    /// 降级 — 有异常但未崩溃, 需关注
    Degraded,
    /// 严重 — Chrome 崩溃或不可达, 需立即恢复
    Critical,
}

impl HealthLevel {
    /// 中文描述
    pub fn description(&self) -> &'static str {
        match self {
            HealthLevel::Healthy => "健康",
            HealthLevel::Degraded => "降级",
            HealthLevel::Critical => "严重",
        }
    }

    /// 是否需要立即干预
    pub fn requires_immediate_action(&self) -> bool {
        matches!(self, HealthLevel::Critical)
    }
}

impl std::fmt::Display for HealthLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.description())
    }
}

/// 连接状态严重程度 — 单次状态的严重性分类
///
/// 由 [`classify_connection_severity`] 根据 [`ConnectionStatus`] 计算。
/// 与 [`HealthLevel`] 的区别: 严重程度只看单次状态, 健康等级综合考虑历史。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionSeverity {
    /// 信息 — 正常状态
    Info,
    /// 警告 — 可恢复的异常
    Warning,
    /// 严重 — 需要立即处理的异常
    Critical,
}

impl ConnectionSeverity {
    /// 中文描述
    pub fn description(&self) -> &'static str {
        match self {
            ConnectionSeverity::Info => "信息",
            ConnectionSeverity::Warning => "警告",
            ConnectionSeverity::Critical => "严重",
        }
    }
}

impl std::fmt::Display for ConnectionSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.description())
    }
}

/// 计算连接监控成功率
///
/// 成功率 = 1.0 - (失败次数 / 总检查次数)。
/// 当总检查次数为 0 时, 返回 1.0 (视为完全成功)。
///
/// # 参数
/// - `total_checks`: 总检查次数
/// - `total_failures`: 总失败次数 (非 Connected 的检查)
///
/// # 返回值
/// 成功率, 范围 [0.0, 1.0]
///
/// # 示例
///
/// ```
/// use forge::connection_monitor::calculate_monitor_success_rate;
///
/// // 零检查 — 默认成功
/// assert!((calculate_monitor_success_rate(0, 0) - 1.0).abs() < 0.001);
///
/// // 全部成功
/// assert!((calculate_monitor_success_rate(100, 0) - 1.0).abs() < 0.001);
///
/// // 一半失败
/// assert!((calculate_monitor_success_rate(100, 50) - 0.5).abs() < 0.001);
/// ```
pub fn calculate_monitor_success_rate(total_checks: u64, total_failures: u64) -> f64 {
    if total_checks == 0 {
        return 1.0;
    }
    let failure_rate = total_failures as f64 / total_checks as f64;
    (1.0 - failure_rate).clamp(0.0, 1.0)
}

/// 判断是否已达到 Chrome 崩溃阈值
///
/// 当连续失败次数 >= 最大允许失败次数时, 判定为 Chrome 崩溃。
///
/// # 参数
/// - `consecutive_failures`: 当前连续失败次数
/// - `max_consecutive_failures`: 配置的最大允许失败次数
///
/// # 返回值
/// `true` 表示已达到崩溃阈值
///
/// # 示例
///
/// ```
/// use forge::connection_monitor::is_chrome_crashed_status;
///
/// // 未达到阈值
/// assert!(!is_chrome_crashed_status(0, 3));
/// assert!(!is_chrome_crashed_status(2, 3));
///
/// // 恰好达到阈值 (边界)
/// assert!(is_chrome_crashed_status(3, 3));
///
/// // 超过阈值
/// assert!(is_chrome_crashed_status(5, 3));
/// ```
pub fn is_chrome_crashed_status(consecutive_failures: u32, max_consecutive_failures: u32) -> bool {
    consecutive_failures >= max_consecutive_failures
}

/// 确定连接健康等级
///
/// 综合考虑当前连接状态和连续失败历史, 返回整体健康评估。
///
/// # 逻辑
/// - **Healthy**: 连接正常且无连续失败
/// - **Critical**: Chrome 不可达, 或连续失败达到崩溃阈值
/// - **Degraded**: 其他异常状态 (标签页关闭、超时等), 但未达到崩溃阈值
///
/// # 参数
/// - `status`: 当前连接状态
/// - `consecutive_failures`: 当前连续失败次数
/// - `max_consecutive_failures`: 崩溃阈值
///
/// # 示例
///
/// ```
/// use forge::connection_monitor::{determine_health_level, HealthLevel, ConnectionStatus};
///
/// // 正常 — 健康
/// assert_eq!(
///     determine_health_level(&ConnectionStatus::Connected, 0, 3),
///     HealthLevel::Healthy
/// );
///
/// // Chrome 不可达 — 严重
/// assert_eq!(
///     determine_health_level(&ConnectionStatus::ChromeUnreachable, 0, 3),
///     HealthLevel::Critical
/// );
///
/// // 标签页关闭, 失败次数少 — 降级
/// assert_eq!(
///     determine_health_level(&ConnectionStatus::TabClosed, 1, 3),
///     HealthLevel::Degraded
/// );
/// ```
pub fn determine_health_level(
    status: &ConnectionStatus,
    consecutive_failures: u32,
    max_consecutive_failures: u32,
) -> HealthLevel {
    if status.is_connected() && consecutive_failures == 0 {
        HealthLevel::Healthy
    } else if is_chrome_crashed_status(consecutive_failures, max_consecutive_failures)
        || matches!(status, ConnectionStatus::ChromeUnreachable)
    {
        HealthLevel::Critical
    } else {
        HealthLevel::Degraded
    }
}

/// 分类连接状态的严重程度
///
/// 只根据单次状态分类, 不考虑历史失败次数。
///
/// # 映射
/// - `Connected` → `Info`
/// - `TabClosed`, `CheckTimeout` → `Warning`
/// - `ChromeUnreachable`, `WebSocketError` → `Critical`
///
/// # 示例
///
/// ```
/// use forge::connection_monitor::{classify_connection_severity, ConnectionSeverity, ConnectionStatus};
///
/// assert_eq!(
///     classify_connection_severity(&ConnectionStatus::Connected),
///     ConnectionSeverity::Info
/// );
/// assert_eq!(
///     classify_connection_severity(&ConnectionStatus::ChromeUnreachable),
///     ConnectionSeverity::Critical
/// );
/// ```
pub fn classify_connection_severity(status: &ConnectionStatus) -> ConnectionSeverity {
    match status {
        ConnectionStatus::Connected => ConnectionSeverity::Info,
        ConnectionStatus::TabClosed | ConnectionStatus::CheckTimeout => ConnectionSeverity::Warning,
        ConnectionStatus::ChromeUnreachable | ConnectionStatus::WebSocketError(_) => {
            ConnectionSeverity::Critical
        }
    }
}

/// 判断是否应该触发自动恢复
///
/// 当连接异常且已有至少 1 次连续失败, 或已达到 Chrome 崩溃阈值时, 应触发恢复。
///
/// # 参数
/// - `status`: 当前连接状态
/// - `consecutive_failures`: 当前连续失败次数
/// - `max_consecutive_failures`: 崩溃阈值
///
/// # 返回值
/// `true` 表示应触发自动恢复
///
/// # 示例
///
/// ```
/// use forge::connection_monitor::{should_trigger_recovery, ConnectionStatus};
///
/// // 连接正常 — 不需要恢复
/// assert!(!should_trigger_recovery(&ConnectionStatus::Connected, 0, 3));
///
/// // 标签页关闭, 已失败 1 次 — 需要恢复
/// assert!(should_trigger_recovery(&ConnectionStatus::TabClosed, 1, 3));
///
/// // 崩溃阈值 — 必须恢复
/// assert!(should_trigger_recovery(&ConnectionStatus::ChromeUnreachable, 3, 3));
/// ```
pub fn should_trigger_recovery(
    status: &ConnectionStatus,
    consecutive_failures: u32,
    max_consecutive_failures: u32,
) -> bool {
    if is_chrome_crashed_status(consecutive_failures, max_consecutive_failures) {
        return true;
    }
    status.needs_recovery() && consecutive_failures >= 1
}

/// 计算下次连接检查的延迟 (秒)
///
/// 根据当前状态和失败历史, 计算下一次检查应等待的时间。
///
/// # 策略
/// - 心跳间隔为 0: 禁用心跳, 返回 0
/// - 连接正常: 返回完整心跳间隔
/// - 连接异常: 缩短检查间隔 (更快检测恢复)
///   - 1 次失败: 间隔 / 2
///   - 2 次失败: 间隔 / 3
///   - 3+ 次失败: 间隔 / 4 (上限)
/// - 最小间隔为 1 秒
///
/// # 参数
/// - `last_status`: 上次检查的状态
/// - `consecutive_failures`: 当前连续失败次数
/// - `heartbeat_interval_secs`: 心跳间隔 (秒), 0 表示禁用
///
/// # 示例
///
/// ```
/// use forge::connection_monitor::{compute_next_check_delay, ConnectionStatus};
///
/// // 连接正常 — 完整间隔
/// assert_eq!(compute_next_check_delay(&ConnectionStatus::Connected, 0, 30), 30);
///
/// // 连接异常, 1 次失败 — 间隔减半
/// assert_eq!(compute_next_check_delay(&ConnectionStatus::TabClosed, 1, 30), 15);
///
/// // 心跳禁用
/// assert_eq!(compute_next_check_delay(&ConnectionStatus::Connected, 0, 0), 0);
/// ```
pub fn compute_next_check_delay(
    last_status: &ConnectionStatus,
    consecutive_failures: u32,
    heartbeat_interval_secs: u64,
) -> u64 {
    if heartbeat_interval_secs == 0 {
        return 0;
    }
    if last_status.is_connected() {
        return heartbeat_interval_secs;
    }
    let divisor = (1 + consecutive_failures.min(3) as u64).max(1);
    (heartbeat_interval_secs / divisor).max(1)
}

/// 格式化运行时长为人类可读字符串
///
/// 格式: `X.Ym (Z.Wh)` (分钟和小时)
///
/// # 参数
/// - `secs`: 运行时长 (秒)
///
/// # 示例
///
/// ```
/// use forge::connection_monitor::format_uptime;
///
/// assert_eq!(format_uptime(0), "0.0m (0.0h)");
/// assert_eq!(format_uptime(60), "1.0m (0.0h)");
/// assert_eq!(format_uptime(3600), "60.0m (1.0h)");
/// ```
pub fn format_uptime(secs: u64) -> String {
    let minutes = secs as f64 / 60.0;
    let hours = secs as f64 / 3600.0;
    format!("{:.1}m ({:.1}h)", minutes, hours)
}

/// 格式化成功率为百分比字符串
///
/// 格式: `XX.X%` (保留一位小数)
/// 输入值会被 clamp 到 [0.0, 1.0] 范围。
///
/// # 参数
/// - `rate`: 成功率 (0.0 ~ 1.0)
///
/// # 示例
///
/// ```
/// use forge::connection_monitor::format_monitor_success_rate;
///
/// assert_eq!(format_monitor_success_rate(1.0), "100.0%");
/// assert_eq!(format_monitor_success_rate(0.0), "0.0%");
/// assert_eq!(format_monitor_success_rate(0.95), "95.0%");
/// ```
pub fn format_monitor_success_rate(rate: f64) -> String {
    let clamped = rate.clamp(0.0, 1.0);
    format!("{:.1}%", clamped * 100.0)
}

/// 格式化恢复事件为报告行
///
/// 格式: `  ✅ [Xs] 状态A → 状态B | 策略 (Yms)`
///
/// # 参数
/// - `event`: 恢复事件
///
/// # 示例
///
/// ```
/// use forge::connection_monitor::{format_recovery_event_line, RecoveryEvent, ConnectionStatus};
///
/// let event = RecoveryEvent::new(
///     5000,
///     ConnectionStatus::ChromeUnreachable,
///     ConnectionStatus::Connected,
///     "重试第3次",
///     15000,
///     true,
///     None,
/// );
/// let line = format_recovery_event_line(&event);
/// assert!(line.contains("✅"));
/// assert!(line.contains("重试第3次"));
/// ```
pub fn format_recovery_event_line(event: &RecoveryEvent) -> String {
    let status_icon = if event.success { "✅" } else { "❌" };
    format!(
        "  {} [{:.0}s] {} → {} | {} ({}ms)\n",
        status_icon,
        event.timestamp_ms as f64 / 1000.0,
        event.before_status.description(),
        event.after_status.description(),
        event.strategy,
        event.duration_ms,
    )
}

// ============================================================================
//  ConnectionMonitor — 连接监控器
// ============================================================================

/// 连接监控配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorConfig {
    /// Chrome 调试端口 (默认 9222)
    pub port: u16,
    /// 连接检查超时 (秒, 默认 10)
    pub check_timeout_secs: u64,
    /// 心跳间隔 (秒, 默认 30) — 0 表示禁用心跳
    pub heartbeat_interval_secs: u64,
    /// 连续失败多少次后判定为 Chrome 崩溃 (默认 3)
    pub max_consecutive_failures: u32,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            port: 9222,
            check_timeout_secs: 10,
            heartbeat_interval_secs: 30,
            max_consecutive_failures: 3,
        }
    }
}

/// Chrome 连接监控器
///
/// 通过 HTTP 探测 Chrome 调试端口, 检查连接状态。
/// 维护连续失败计数, 当超过阈值时判定为 Chrome 崩溃。
///
/// ## 使用方式
///
/// ```ignore
/// let monitor = ConnectionMonitor::new(9222);
/// let status = monitor.check_connection().await;
/// if status.needs_recovery() {
///     // 触发 AutoRecovery
/// }
/// ```
pub struct ConnectionMonitor {
    /// 监控配置
    config: MonitorConfig,
    /// 连续失败次数
    pub consecutive_failures: u32,
    /// 上次成功检查的时间
    last_successful_check: Option<Instant>,
    /// 上次状态
    pub last_status: ConnectionStatus,
    /// 总检查次数
    pub total_checks: u64,
    /// 总失败次数 (非 Connected)
    pub total_failures: u64,
    /// 恢复事件历史
    recovery_events: Vec<RecoveryEvent>,
    /// 启动时间 (用于计算 timestamp_ms)
    start_time: Instant,
}

impl ConnectionMonitor {
    /// 创建连接监控器
    pub fn new(port: u16) -> Self {
        Self {
            config: MonitorConfig {
                port,
                ..Default::default()
            },
            consecutive_failures: 0,
            last_successful_check: None,
            last_status: ConnectionStatus::Connected,
            total_checks: 0,
            total_failures: 0,
            recovery_events: vec![],
            start_time: Instant::now(),
        }
    }

    /// 创建带配置的连接监控器
    pub fn with_config(config: MonitorConfig) -> Self {
        Self {
            config,
            consecutive_failures: 0,
            last_successful_check: None,
            last_status: ConnectionStatus::Connected,
            total_checks: 0,
            total_failures: 0,
            recovery_events: vec![],
            start_time: Instant::now(),
        }
    }

    /// 获取当前配置
    pub fn config(&self) -> &MonitorConfig {
        &self.config
    }

    /// 获取上次状态
    pub fn last_status(&self) -> &ConnectionStatus {
        &self.last_status
    }

    /// 获取连续失败次数
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    /// 获取总检查次数
    pub fn total_checks(&self) -> u64 {
        self.total_checks
    }

    /// 获取总失败次数
    pub fn total_failures(&self) -> u64 {
        self.total_failures
    }

    /// 获取恢复事件历史
    pub fn recovery_events(&self) -> &[RecoveryEvent] {
        &self.recovery_events
    }

    /// 是否判定为 Chrome 崩溃 (连续失败超过阈值)
    pub fn is_chrome_crashed(&self) -> bool {
        is_chrome_crashed_status(
            self.consecutive_failures,
            self.config.max_consecutive_failures,
        )
    }

    /// 获取当前健康等级
    ///
    /// 综合考虑连接状态和连续失败历史, 返回 [`HealthLevel`]。
    pub fn health_level(&self) -> HealthLevel {
        determine_health_level(
            &self.last_status,
            self.consecutive_failures,
            self.config.max_consecutive_failures,
        )
    }

    /// 获取当前连接状态严重程度
    ///
    /// 只根据上次检查状态分类, 返回 [`ConnectionSeverity`]。
    pub fn severity(&self) -> ConnectionSeverity {
        classify_connection_severity(&self.last_status)
    }

    /// 是否应该触发自动恢复
    ///
    /// 当连接异常且已有至少 1 次连续失败, 或已达到 Chrome 崩溃阈值时返回 `true`。
    pub fn should_trigger_recovery(&self) -> bool {
        should_trigger_recovery(
            &self.last_status,
            self.consecutive_failures,
            self.config.max_consecutive_failures,
        )
    }

    /// 计算下次检查的延迟 (秒)
    ///
    /// 连接正常时返回完整心跳间隔; 异常时根据失败次数缩短间隔。
    pub fn next_check_delay_secs(&self) -> u64 {
        compute_next_check_delay(
            &self.last_status,
            self.consecutive_failures,
            self.config.heartbeat_interval_secs,
        )
    }

    /// 距上次成功检查的时长
    pub fn time_since_last_success(&self) -> Option<Duration> {
        self.last_successful_check.map(|t| t.elapsed())
    }

    /// 检查 Chrome 连接状态
    ///
    /// 通过 HTTP 探测 Chrome 调试端口:
    /// 1. 检查 `http://localhost:{port}/json/version` 是否可达
    /// 2. 如果可达, 获取标签页列表, 检查是否有聊天标签页
    ///
    /// 返回 `ConnectionStatus`, 同时更新内部计数器。
    pub async fn check_connection(&mut self) -> ConnectionStatus {
        self.total_checks += 1;
        let check_start = Instant::now();

        let status = self.do_check().await;

        // 更新计数器
        if status.is_connected() {
            self.consecutive_failures = 0;
            self.last_successful_check = Some(Instant::now());
        } else {
            self.consecutive_failures += 1;
            self.total_failures += 1;
        }

        // 更新最后状态
        let prev_status = self.last_status.clone();
        self.last_status = status.clone();

        // 如果状态发生变化, 记录日志
        if prev_status != status {
            if status.is_connected() {
                info!("✅ 连接恢复: {} -> {}", prev_status, status);
            } else {
                warn!(
                    "⚠️ 连接异常: {} -> {} (连续失败 {} 次)",
                    prev_status, status, self.consecutive_failures
                );
            }
        }

        debug!(
            "连接检查完成: {} (耗时 {}ms, 总检查 {} 次)",
            status,
            check_start.elapsed().as_millis(),
            self.total_checks
        );

        status
    }

    /// 内部检查逻辑
    async fn do_check(&self) -> ConnectionStatus {
        let timeout = Duration::from_secs(self.config.check_timeout_secs);

        // 1. 检查 Chrome 调试端口是否可达
        let check_result =
            tokio::time::timeout(timeout, crate::cdp::check_reachable(self.config.port)).await;

        match check_result {
            Err(_) => {
                // 检查超时
                warn!("连接检查超时 ({}s)", self.config.check_timeout_secs);
                ConnectionStatus::CheckTimeout
            }
            Ok(Err(e)) => {
                // Chrome 不可达
                debug!("Chrome 不可达: {}", e);
                ConnectionStatus::ChromeUnreachable
            }
            Ok(Ok(())) => {
                // Chrome 可达, 检查标签页
                let tabs_result =
                    tokio::time::timeout(timeout, crate::cdp::discover_tabs(self.config.port))
                        .await;

                match tabs_result {
                    Err(_) => ConnectionStatus::CheckTimeout,
                    Ok(Err(e)) => {
                        warn!("获取标签页列表失败: {}", e);
                        ConnectionStatus::WebSocketError(e.to_string())
                    }
                    Ok(Ok(tabs)) => {
                        // 检查是否有聊天标签页
                        let has_chat_tab = tabs.iter().any(|t| {
                            crate::browser::BrowserManager::looks_like_chat(&t.url, &t.title)
                        });

                        if has_chat_tab {
                            ConnectionStatus::Connected
                        } else {
                            warn!("聊天标签页不存在 (共 {} 个标签页)", tabs.len());
                            ConnectionStatus::TabClosed
                        }
                    }
                }
            }
        }
    }

    /// 记录一次恢复事件
    pub fn record_recovery_event(&mut self, event: RecoveryEvent) {
        let timestamp_ms = self.start_time.elapsed().as_millis() as u64;
        let mut event = event;
        event.timestamp_ms = timestamp_ms;
        self.recovery_events.push(event);
    }

    /// 重置监控器 (恢复成功后调用)
    pub fn reset(&mut self) {
        self.consecutive_failures = 0;
        self.last_status = ConnectionStatus::Connected;
        self.last_successful_check = Some(Instant::now());
        debug!("连接监控器已重置");
    }

    /// 生成监控摘要报告
    pub fn summary(&self) -> ConnectionMonitorSummary {
        ConnectionMonitorSummary {
            total_checks: self.total_checks,
            total_failures: self.total_failures,
            success_rate: calculate_monitor_success_rate(self.total_checks, self.total_failures),
            consecutive_failures: self.consecutive_failures,
            last_status: self.last_status.clone(),
            recovery_events: self.recovery_events.clone(),
            uptime_secs: self.start_time.elapsed().as_secs(),
        }
    }
}

// ============================================================================
//  ConnectionMonitorSummary — 监控摘要
// ============================================================================

/// 连接监控摘要 — 24 小时运行后的连接健康概览
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionMonitorSummary {
    /// 总检查次数
    pub total_checks: u64,
    /// 总失败次数
    pub total_failures: u64,
    /// 成功率 (0.0 ~ 1.0)
    pub success_rate: f64,
    /// 当前连续失败次数
    pub consecutive_failures: u32,
    /// 最后状态
    pub last_status: ConnectionStatus,
    /// 恢复事件历史
    pub recovery_events: Vec<RecoveryEvent>,
    /// 运行时长 (秒)
    pub uptime_secs: u64,
}

impl ConnectionMonitorSummary {
    /// 生成可读报告
    pub fn to_report(&self) -> String {
        let mut report = String::new();
        report.push_str("═══════════════════════════════════════════════════\n");
        report.push_str("  📡 连接监控报告\n");
        report.push_str("═══════════════════════════════════════════════════\n\n");

        report.push_str(&format!(
            "  运行时长: {}\n",
            format_uptime(self.uptime_secs)
        ));
        report.push_str(&format!("  总检查次数: {}\n", self.total_checks));
        report.push_str(&format!("  总失败次数: {}\n", self.total_failures));
        report.push_str(&format!(
            "  成功率: {}\n",
            format_monitor_success_rate(self.success_rate)
        ));
        report.push_str(&format!("  当前连续失败: {}\n", self.consecutive_failures));
        report.push_str(&format!(
            "  最后状态: {}\n\n",
            self.last_status.description()
        ));

        if !self.recovery_events.is_empty() {
            report.push_str(&format!(
                "  ── 恢复事件 ({} 次) ──\n",
                self.recovery_events.len()
            ));
            for event in &self.recovery_events {
                report.push_str(&format_recovery_event_line(event));
            }
        } else {
            report.push_str("  ── 恢复事件: 无 ──\n");
        }

        report
    }
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ===== ConnectionStatus =====

    #[test]
    fn test_status_is_connected() {
        assert!(ConnectionStatus::Connected.is_connected());
        assert!(!ConnectionStatus::ChromeUnreachable.is_connected());
        assert!(!ConnectionStatus::TabClosed.is_connected());
        assert!(!ConnectionStatus::WebSocketError("err".to_string()).is_connected());
        assert!(!ConnectionStatus::CheckTimeout.is_connected());
    }

    #[test]
    fn test_status_needs_recovery() {
        assert!(!ConnectionStatus::Connected.needs_recovery());
        assert!(ConnectionStatus::ChromeUnreachable.needs_recovery());
        assert!(ConnectionStatus::TabClosed.needs_recovery());
        assert!(ConnectionStatus::WebSocketError("err".to_string()).needs_recovery());
        assert!(ConnectionStatus::CheckTimeout.needs_recovery());
    }

    #[test]
    fn test_status_description() {
        assert_eq!(ConnectionStatus::Connected.description(), "连接正常");
        assert_eq!(
            ConnectionStatus::ChromeUnreachable.description(),
            "Chrome 不可达"
        );
        assert_eq!(ConnectionStatus::TabClosed.description(), "标签页已关闭");
        assert_eq!(
            ConnectionStatus::WebSocketError("e".to_string()).description(),
            "WebSocket 连接异常"
        );
        assert_eq!(ConnectionStatus::CheckTimeout.description(), "连接检查超时");
    }

    #[test]
    fn test_status_recovery_difficulty() {
        assert_eq!(ConnectionStatus::Connected.recovery_difficulty(), 0);
        assert_eq!(ConnectionStatus::TabClosed.recovery_difficulty(), 1);
        assert_eq!(
            ConnectionStatus::WebSocketError("e".to_string()).recovery_difficulty(),
            2
        );
        assert_eq!(ConnectionStatus::ChromeUnreachable.recovery_difficulty(), 3);
        assert_eq!(ConnectionStatus::CheckTimeout.recovery_difficulty(), 2);
    }

    #[test]
    fn test_status_display() {
        assert_eq!(ConnectionStatus::Connected.to_string(), "Connected");
        assert_eq!(
            ConnectionStatus::ChromeUnreachable.to_string(),
            "ChromeUnreachable"
        );
        assert_eq!(ConnectionStatus::TabClosed.to_string(), "TabClosed");
        assert_eq!(ConnectionStatus::CheckTimeout.to_string(), "CheckTimeout");
        assert!(ConnectionStatus::WebSocketError("err".to_string())
            .to_string()
            .contains("err"));
    }

    #[test]
    fn test_status_serde() {
        let status = ConnectionStatus::ChromeUnreachable;
        let json = serde_json::to_string(&status).unwrap();
        let parsed: ConnectionStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, status);
    }

    #[test]
    fn test_status_eq() {
        assert_eq!(ConnectionStatus::Connected, ConnectionStatus::Connected);
        assert_ne!(ConnectionStatus::Connected, ConnectionStatus::TabClosed);
        assert_eq!(
            ConnectionStatus::WebSocketError("a".to_string()),
            ConnectionStatus::WebSocketError("a".to_string())
        );
        assert_ne!(
            ConnectionStatus::WebSocketError("a".to_string()),
            ConnectionStatus::WebSocketError("b".to_string())
        );
    }

    // ===== RecoveryEvent =====

    #[test]
    fn test_recovery_event_new() {
        let event = RecoveryEvent::new(
            5000,
            ConnectionStatus::ChromeUnreachable,
            ConnectionStatus::Connected,
            "指数退避重试第3次",
            15000,
            true,
            None,
        );
        assert_eq!(event.timestamp_ms, 5000);
        assert_eq!(event.before_status, ConnectionStatus::ChromeUnreachable);
        assert_eq!(event.after_status, ConnectionStatus::Connected);
        assert_eq!(event.strategy, "指数退避重试第3次");
        assert_eq!(event.duration_ms, 15000);
        assert!(event.success);
        assert!(event.error.is_none());
    }

    #[test]
    fn test_recovery_event_with_error() {
        let event = RecoveryEvent::new(
            10000,
            ConnectionStatus::CheckTimeout,
            ConnectionStatus::ChromeUnreachable,
            "等待 Chrome 重启",
            30000,
            false,
            Some("Chrome 未在 30s 内恢复"),
        );
        assert!(!event.success);
        assert_eq!(event.error, Some("Chrome 未在 30s 内恢复".to_string()));
    }

    #[test]
    fn test_recovery_event_serde() {
        let event = RecoveryEvent::new(
            1000,
            ConnectionStatus::TabClosed,
            ConnectionStatus::Connected,
            "重新发现标签页",
            500,
            true,
            None,
        );
        let json = serde_json::to_string(&event).unwrap();
        let parsed: RecoveryEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.before_status, event.before_status);
        assert_eq!(parsed.after_status, event.after_status);
        assert_eq!(parsed.strategy, event.strategy);
    }

    // ===== MonitorConfig =====

    #[test]
    fn test_config_default() {
        let config = MonitorConfig::default();
        assert_eq!(config.port, 9222);
        assert_eq!(config.check_timeout_secs, 10);
        assert_eq!(config.heartbeat_interval_secs, 30);
        assert_eq!(config.max_consecutive_failures, 3);
    }

    #[test]
    fn test_config_custom() {
        let config = MonitorConfig {
            port: 9333,
            check_timeout_secs: 5,
            heartbeat_interval_secs: 15,
            max_consecutive_failures: 5,
        };
        assert_eq!(config.port, 9333);
        assert_eq!(config.check_timeout_secs, 5);
    }

    // ===== ConnectionMonitor =====

    #[test]
    fn test_monitor_new() {
        let monitor = ConnectionMonitor::new(9222);
        assert_eq!(monitor.config().port, 9222);
        assert_eq!(monitor.consecutive_failures(), 0);
        assert_eq!(monitor.total_checks(), 0);
        assert_eq!(monitor.total_failures(), 0);
        assert!(!monitor.is_chrome_crashed());
        assert!(monitor.last_status().is_connected());
    }

    #[test]
    fn test_monitor_with_config() {
        let config = MonitorConfig {
            port: 9333,
            check_timeout_secs: 5,
            heartbeat_interval_secs: 10,
            max_consecutive_failures: 7,
        };
        let monitor = ConnectionMonitor::with_config(config);
        assert_eq!(monitor.config().port, 9333);
        assert_eq!(monitor.config().check_timeout_secs, 5);
        assert_eq!(monitor.config().max_consecutive_failures, 7);
    }

    #[test]
    fn test_monitor_is_chrome_crashed() {
        let mut monitor = ConnectionMonitor::new(9222);
        // 默认 max_consecutive_failures = 3
        assert!(!monitor.is_chrome_crashed());

        // 模拟 2 次失败 — 未达到阈值
        monitor.consecutive_failures = 2;
        assert!(!monitor.is_chrome_crashed());

        // 模拟 3 次失败 — 达到阈值
        monitor.consecutive_failures = 3;
        assert!(monitor.is_chrome_crashed());
    }

    #[test]
    fn test_monitor_reset() {
        let mut monitor = ConnectionMonitor::new(9222);
        monitor.consecutive_failures = 5;
        monitor.last_status = ConnectionStatus::ChromeUnreachable;

        monitor.reset();

        assert_eq!(monitor.consecutive_failures(), 0);
        assert!(monitor.last_status().is_connected());
    }

    #[test]
    fn test_monitor_record_recovery_event() {
        let mut monitor = ConnectionMonitor::new(9222);
        // 等待至少 1ms 确保 start_time.elapsed() > 0
        std::thread::sleep(std::time::Duration::from_millis(2));

        let event = RecoveryEvent::new(
            0,
            ConnectionStatus::ChromeUnreachable,
            ConnectionStatus::Connected,
            "重试第3次成功",
            20000,
            true,
            None,
        );
        monitor.record_recovery_event(event);

        assert_eq!(monitor.recovery_events().len(), 1);
        assert!(monitor.recovery_events()[0].success);
        assert_eq!(monitor.recovery_events()[0].strategy, "重试第3次成功");
        // timestamp_ms 应被覆盖为实际时间
        assert!(monitor.recovery_events()[0].timestamp_ms > 0);
    }

    #[test]
    fn test_monitor_record_multiple_events() {
        let mut monitor = ConnectionMonitor::new(9222);

        for i in 0..5 {
            let event = RecoveryEvent::new(
                0,
                ConnectionStatus::TabClosed,
                ConnectionStatus::Connected,
                &format!("恢复 #{}", i),
                1000 * (i + 1) as u64,
                true,
                None,
            );
            monitor.record_recovery_event(event);
        }

        assert_eq!(monitor.recovery_events().len(), 5);
        // 验证时间戳递增
        for i in 1..5 {
            assert!(
                monitor.recovery_events()[i].timestamp_ms
                    >= monitor.recovery_events()[i - 1].timestamp_ms
            );
        }
    }

    #[test]
    fn test_monitor_summary_empty() {
        let monitor = ConnectionMonitor::new(9222);
        let summary = monitor.summary();
        assert_eq!(summary.total_checks, 0);
        assert_eq!(summary.total_failures, 0);
        assert_eq!(summary.success_rate, 1.0);
        assert_eq!(summary.consecutive_failures, 0);
        assert!(summary.last_status.is_connected());
        assert!(summary.recovery_events.is_empty());
    }

    #[test]
    fn test_monitor_summary_with_failures() {
        let mut monitor = ConnectionMonitor::new(9222);

        // 模拟内部状态
        monitor.total_checks = 100;
        monitor.total_failures = 5;
        monitor.consecutive_failures = 2;
        monitor.last_status = ConnectionStatus::TabClosed;

        let summary = monitor.summary();
        assert_eq!(summary.total_checks, 100);
        assert_eq!(summary.total_failures, 5);
        assert!((summary.success_rate - 0.95).abs() < 0.001);
        assert_eq!(summary.consecutive_failures, 2);
        assert_eq!(summary.last_status, ConnectionStatus::TabClosed);
    }

    #[test]
    fn test_monitor_summary_report() {
        let mut monitor = ConnectionMonitor::new(9222);
        monitor.total_checks = 50;
        monitor.total_failures = 3;
        monitor.consecutive_failures = 1;
        monitor.last_status = ConnectionStatus::Connected;

        let event = RecoveryEvent::new(
            0,
            ConnectionStatus::ChromeUnreachable,
            ConnectionStatus::Connected,
            "指数退避第2次",
            10000,
            true,
            None,
        );
        monitor.record_recovery_event(event);

        let summary = monitor.summary();
        let report = summary.to_report();

        assert!(report.contains("连接监控报告"));
        assert!(report.contains("总检查次数: 50"));
        assert!(report.contains("总失败次数: 3"));
        assert!(report.contains("恢复事件"));
        assert!(report.contains("指数退避第2次"));
    }

    #[test]
    fn test_monitor_summary_report_no_events() {
        let monitor = ConnectionMonitor::new(9222);
        let summary = monitor.summary();
        let report = summary.to_report();
        assert!(report.contains("恢复事件: 无"));
    }

    // ===== 连接检查测试 (真实 HTTP 探测 — 在无 Chrome 时返回非 Connected) =====

    #[tokio::test]
    async fn test_check_connection_no_chrome() {
        // 在测试环境中 Chrome 不在 9222 端口运行
        // 使用一个不太可能运行的端口
        let mut monitor = ConnectionMonitor::new(19999);

        let status = monitor.check_connection().await;

        // 应该返回 ChromeUnreachable 或 CheckTimeout
        assert!(status.needs_recovery());
        assert_eq!(monitor.total_checks(), 1);
        assert_eq!(monitor.total_failures(), 1);
        assert_eq!(monitor.consecutive_failures(), 1);
    }

    #[tokio::test]
    async fn test_check_connection_updates_counters() {
        let mut monitor = ConnectionMonitor::new(19999);

        // 第一次检查
        let _ = monitor.check_connection().await;
        assert_eq!(monitor.total_checks(), 1);
        assert_eq!(monitor.consecutive_failures(), 1);

        // 第二次检查
        let _ = monitor.check_connection().await;
        assert_eq!(monitor.total_checks(), 2);
        assert_eq!(monitor.consecutive_failures(), 2);
        assert_eq!(monitor.total_failures(), 2);
    }

    #[tokio::test]
    async fn test_check_connection_short_timeout() {
        // 使用极短超时确保超时
        let config = MonitorConfig {
            port: 19999,
            check_timeout_secs: 1,
            heartbeat_interval_secs: 0,
            max_consecutive_failures: 10,
        };
        let mut monitor = ConnectionMonitor::with_config(config);

        let status = monitor.check_connection().await;
        // 端口不可达, 应该很快返回 ChromeUnreachable
        assert!(status.needs_recovery());
    }

    // ===== 时间_since_last_success 测试 =====

    #[test]
    fn test_time_since_last_success_none() {
        let monitor = ConnectionMonitor::new(9222);
        assert!(monitor.time_since_last_success().is_none());
    }

    #[test]
    fn test_time_since_last_success_some() {
        let mut monitor = ConnectionMonitor::new(9222);
        monitor.last_successful_check = Some(Instant::now() - Duration::from_secs(5));
        let elapsed = monitor.time_since_last_success().unwrap();
        assert!(elapsed.as_secs() >= 4); // 允许微小偏差
    }

    // ===== calculate_monitor_success_rate =====

    #[test]
    fn test_calculate_monitor_success_rate_zero_checks() {
        assert!((calculate_monitor_success_rate(0, 0) - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_calculate_monitor_success_rate_all_success() {
        assert!((calculate_monitor_success_rate(100, 0) - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_calculate_monitor_success_rate_all_failed() {
        assert!((calculate_monitor_success_rate(100, 100) - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_calculate_monitor_success_rate_half() {
        assert!((calculate_monitor_success_rate(100, 50) - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_calculate_monitor_success_rate_one_failure() {
        assert!((calculate_monitor_success_rate(1000, 1) - 0.999).abs() < 0.001);
    }

    #[test]
    fn test_calculate_monitor_success_rate_third() {
        let expected = 1.0 - 1.0 / 3.0;
        assert!((calculate_monitor_success_rate(3, 1) - expected).abs() < 0.001);
    }

    #[test]
    fn test_calculate_monitor_success_rate_large_numbers() {
        let rate = calculate_monitor_success_rate(u64::MAX, u64::MAX / 2);
        assert!((rate - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_calculate_monitor_success_rate_failures_exceed_checks() {
        // failures > checks — 应 clamp 到 0
        let rate = calculate_monitor_success_rate(10, 20);
        assert!((rate - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_calculate_monitor_success_rate_single_check_success() {
        assert!((calculate_monitor_success_rate(1, 0) - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_calculate_monitor_success_rate_single_check_failure() {
        assert!((calculate_monitor_success_rate(1, 1) - 0.0).abs() < 0.001);
    }

    // ===== is_chrome_crashed_status =====

    #[test]
    fn test_is_chrome_crashed_status_zero_failures() {
        assert!(!is_chrome_crashed_status(0, 3));
    }

    #[test]
    fn test_is_chrome_crashed_status_below_threshold() {
        assert!(!is_chrome_crashed_status(1, 3));
        assert!(!is_chrome_crashed_status(2, 3));
    }

    #[test]
    fn test_is_chrome_crashed_status_at_threshold() {
        // 边界: 恰好等于阈值
        assert!(is_chrome_crashed_status(3, 3));
    }

    #[test]
    fn test_is_chrome_crashed_status_above_threshold() {
        assert!(is_chrome_crashed_status(4, 3));
        assert!(is_chrome_crashed_status(100, 3));
    }

    #[test]
    fn test_is_chrome_crashed_status_zero_max() {
        // max=0: 0 >= 0 → true (任何状态都算崩溃)
        assert!(is_chrome_crashed_status(0, 0));
    }

    #[test]
    fn test_is_chrome_crashed_status_u32_max() {
        assert!(!is_chrome_crashed_status(0, u32::MAX));
        assert!(is_chrome_crashed_status(u32::MAX, u32::MAX));
    }

    // ===== determine_health_level =====

    #[test]
    fn test_health_level_healthy() {
        assert_eq!(
            determine_health_level(&ConnectionStatus::Connected, 0, 3),
            HealthLevel::Healthy
        );
    }

    #[test]
    fn test_health_level_connected_with_failures() {
        // Connected 但有失败历史 → Degraded
        assert_eq!(
            determine_health_level(&ConnectionStatus::Connected, 1, 3),
            HealthLevel::Degraded
        );
    }

    #[test]
    fn test_health_level_chrome_unreachable_always_critical() {
        // ChromeUnreachable 始终 Critical
        assert_eq!(
            determine_health_level(&ConnectionStatus::ChromeUnreachable, 0, 3),
            HealthLevel::Critical
        );
        assert_eq!(
            determine_health_level(&ConnectionStatus::ChromeUnreachable, 1, 3),
            HealthLevel::Critical
        );
    }

    #[test]
    fn test_health_level_tab_closed_degraded() {
        assert_eq!(
            determine_health_level(&ConnectionStatus::TabClosed, 1, 3),
            HealthLevel::Degraded
        );
    }

    #[test]
    fn test_health_level_tab_closed_critical() {
        // TabClosed + 达到崩溃阈值 → Critical
        assert_eq!(
            determine_health_level(&ConnectionStatus::TabClosed, 3, 3),
            HealthLevel::Critical
        );
    }

    #[test]
    fn test_health_level_check_timeout_degraded() {
        assert_eq!(
            determine_health_level(&ConnectionStatus::CheckTimeout, 1, 3),
            HealthLevel::Degraded
        );
    }

    #[test]
    fn test_health_level_websocket_error_degraded() {
        assert_eq!(
            determine_health_level(&ConnectionStatus::WebSocketError("err".to_string()), 1, 3),
            HealthLevel::Degraded
        );
    }

    #[test]
    fn test_health_level_crashed_via_threshold() {
        // 任何状态 + failures >= max → Critical
        assert_eq!(
            determine_health_level(&ConnectionStatus::CheckTimeout, 5, 3),
            HealthLevel::Critical
        );
        assert_eq!(
            determine_health_level(&ConnectionStatus::WebSocketError("e".to_string()), 3, 3),
            HealthLevel::Critical
        );
    }

    #[test]
    fn test_health_level_description() {
        assert_eq!(HealthLevel::Healthy.description(), "健康");
        assert_eq!(HealthLevel::Degraded.description(), "降级");
        assert_eq!(HealthLevel::Critical.description(), "严重");
    }

    #[test]
    fn test_health_level_requires_immediate_action() {
        assert!(!HealthLevel::Healthy.requires_immediate_action());
        assert!(!HealthLevel::Degraded.requires_immediate_action());
        assert!(HealthLevel::Critical.requires_immediate_action());
    }

    #[test]
    fn test_health_level_display() {
        assert_eq!(HealthLevel::Healthy.to_string(), "健康");
        assert_eq!(HealthLevel::Degraded.to_string(), "降级");
        assert_eq!(HealthLevel::Critical.to_string(), "严重");
    }

    #[test]
    fn test_health_level_serde() {
        let level = HealthLevel::Critical;
        let json = serde_json::to_string(&level).unwrap();
        let parsed: HealthLevel = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, level);
    }

    // ===== classify_connection_severity =====

    #[test]
    fn test_classify_severity_connected() {
        assert_eq!(
            classify_connection_severity(&ConnectionStatus::Connected),
            ConnectionSeverity::Info
        );
    }

    #[test]
    fn test_classify_severity_tab_closed() {
        assert_eq!(
            classify_connection_severity(&ConnectionStatus::TabClosed),
            ConnectionSeverity::Warning
        );
    }

    #[test]
    fn test_classify_severity_check_timeout() {
        assert_eq!(
            classify_connection_severity(&ConnectionStatus::CheckTimeout),
            ConnectionSeverity::Warning
        );
    }

    #[test]
    fn test_classify_severity_chrome_unreachable() {
        assert_eq!(
            classify_connection_severity(&ConnectionStatus::ChromeUnreachable),
            ConnectionSeverity::Critical
        );
    }

    #[test]
    fn test_classify_severity_websocket_error() {
        assert_eq!(
            classify_connection_severity(&ConnectionStatus::WebSocketError("err".to_string())),
            ConnectionSeverity::Critical
        );
    }

    #[test]
    fn test_severity_description() {
        assert_eq!(ConnectionSeverity::Info.description(), "信息");
        assert_eq!(ConnectionSeverity::Warning.description(), "警告");
        assert_eq!(ConnectionSeverity::Critical.description(), "严重");
    }

    #[test]
    fn test_severity_display() {
        assert_eq!(ConnectionSeverity::Info.to_string(), "信息");
        assert_eq!(ConnectionSeverity::Warning.to_string(), "警告");
        assert_eq!(ConnectionSeverity::Critical.to_string(), "严重");
    }

    #[test]
    fn test_severity_serde() {
        let sev = ConnectionSeverity::Warning;
        let json = serde_json::to_string(&sev).unwrap();
        let parsed: ConnectionSeverity = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, sev);
    }

    // ===== should_trigger_recovery =====

    #[test]
    fn test_should_trigger_recovery_connected() {
        assert!(!should_trigger_recovery(&ConnectionStatus::Connected, 0, 3));
    }

    #[test]
    fn test_should_trigger_recovery_first_failure() {
        // 第一次失败 — 应触发 (consecutive_failures >= 1)
        assert!(should_trigger_recovery(&ConnectionStatus::TabClosed, 1, 3));
    }

    #[test]
    fn test_should_trigger_recovery_zero_failures_non_connected() {
        // 非 Connected 但 0 次失败 — 不触发
        assert!(!should_trigger_recovery(&ConnectionStatus::TabClosed, 0, 3));
    }

    #[test]
    fn test_should_trigger_recovery_crashed() {
        // 崩溃 — 无论什么状态都必须触发
        assert!(should_trigger_recovery(
            &ConnectionStatus::ChromeUnreachable,
            3,
            3
        ));
        assert!(should_trigger_recovery(
            &ConnectionStatus::CheckTimeout,
            5,
            3
        ));
    }

    #[test]
    fn test_should_trigger_recovery_all_non_connected_statuses() {
        // 所有非 Connected 状态 + 1 次失败 → 触发
        assert!(should_trigger_recovery(
            &ConnectionStatus::ChromeUnreachable,
            1,
            3
        ));
        assert!(should_trigger_recovery(&ConnectionStatus::TabClosed, 1, 3));
        assert!(should_trigger_recovery(
            &ConnectionStatus::WebSocketError("e".to_string()),
            1,
            3
        ));
        assert!(should_trigger_recovery(
            &ConnectionStatus::CheckTimeout,
            1,
            3
        ));
    }

    // ===== compute_next_check_delay =====

    #[test]
    fn test_compute_next_check_delay_connected() {
        assert_eq!(
            compute_next_check_delay(&ConnectionStatus::Connected, 0, 30),
            30
        );
    }

    #[test]
    fn test_compute_next_check_delay_disabled() {
        assert_eq!(
            compute_next_check_delay(&ConnectionStatus::Connected, 0, 0),
            0
        );
        assert_eq!(
            compute_next_check_delay(&ConnectionStatus::TabClosed, 1, 0),
            0
        );
    }

    #[test]
    fn test_compute_next_check_delay_one_failure() {
        // 30 / (1 + 1) = 15
        assert_eq!(
            compute_next_check_delay(&ConnectionStatus::TabClosed, 1, 30),
            15
        );
    }

    #[test]
    fn test_compute_next_check_delay_two_failures() {
        // 30 / (1 + 2) = 10
        assert_eq!(
            compute_next_check_delay(&ConnectionStatus::TabClosed, 2, 30),
            10
        );
    }

    #[test]
    fn test_compute_next_check_delay_three_failures() {
        // 30 / (1 + 3) = 7
        assert_eq!(
            compute_next_check_delay(&ConnectionStatus::TabClosed, 3, 30),
            7
        );
    }

    #[test]
    fn test_compute_next_check_delay_capped_failures() {
        // 4+ 次失败: divisor 上限 4 → 30 / 4 = 7
        assert_eq!(
            compute_next_check_delay(&ConnectionStatus::TabClosed, 4, 30),
            7
        );
        assert_eq!(
            compute_next_check_delay(&ConnectionStatus::TabClosed, 100, 30),
            7
        );
    }

    #[test]
    fn test_compute_next_check_delay_min_one_second() {
        // 极小间隔 — 最小 1 秒
        assert_eq!(
            compute_next_check_delay(&ConnectionStatus::TabClosed, 1, 1),
            1
        );
        assert_eq!(
            compute_next_check_delay(&ConnectionStatus::TabClosed, 3, 2),
            1
        );
    }

    #[test]
    fn test_compute_next_check_delay_all_non_connected_statuses() {
        // 所有非 Connected 状态都应缩短间隔
        assert_eq!(
            compute_next_check_delay(&ConnectionStatus::ChromeUnreachable, 1, 30),
            15
        );
        assert_eq!(
            compute_next_check_delay(&ConnectionStatus::WebSocketError("e".to_string()), 1, 30),
            15
        );
        assert_eq!(
            compute_next_check_delay(&ConnectionStatus::CheckTimeout, 1, 30),
            15
        );
    }

    // ===== format_uptime =====

    #[test]
    fn test_format_uptime_zero() {
        assert_eq!(format_uptime(0), "0.0m (0.0h)");
    }

    #[test]
    fn test_format_uptime_one_second() {
        assert_eq!(format_uptime(1), "0.0m (0.0h)");
    }

    #[test]
    fn test_format_uptime_one_minute() {
        assert_eq!(format_uptime(60), "1.0m (0.0h)");
    }

    #[test]
    fn test_format_uptime_one_hour() {
        assert_eq!(format_uptime(3600), "60.0m (1.0h)");
    }

    #[test]
    fn test_format_uptime_24_hours() {
        assert_eq!(format_uptime(86400), "1440.0m (24.0h)");
    }

    #[test]
    fn test_format_uptime_90_seconds() {
        assert_eq!(format_uptime(90), "1.5m (0.0h)");
    }

    // ===== format_monitor_success_rate =====

    #[test]
    fn test_format_monitor_success_rate_zero() {
        assert_eq!(format_monitor_success_rate(0.0), "0.0%");
    }

    #[test]
    fn test_format_monitor_success_rate_full() {
        assert_eq!(format_monitor_success_rate(1.0), "100.0%");
    }

    #[test]
    fn test_format_monitor_success_rate_half() {
        assert_eq!(format_monitor_success_rate(0.5), "50.0%");
    }

    #[test]
    fn test_format_monitor_success_rate_95() {
        assert_eq!(format_monitor_success_rate(0.95), "95.0%");
    }

    #[test]
    fn test_format_monitor_success_rate_third() {
        assert_eq!(format_monitor_success_rate(1.0 / 3.0), "33.3%");
    }

    #[test]
    fn test_format_monitor_success_rate_negative_clamped() {
        assert_eq!(format_monitor_success_rate(-0.1), "0.0%");
    }

    #[test]
    fn test_format_monitor_success_rate_above_one_clamped() {
        assert_eq!(format_monitor_success_rate(1.5), "100.0%");
    }

    // ===== format_recovery_event_line =====

    #[test]
    fn test_format_recovery_event_line_success() {
        let event = RecoveryEvent::new(
            5000,
            ConnectionStatus::ChromeUnreachable,
            ConnectionStatus::Connected,
            "重试第3次",
            15000,
            true,
            None,
        );
        let line = format_recovery_event_line(&event);
        assert!(line.contains("✅"));
        assert!(line.contains("重试第3次"));
        assert!(line.contains("Chrome 不可达"));
        assert!(line.contains("连接正常"));
        assert!(line.contains("15000ms"));
        assert!(line.contains("[5s]"));
    }

    #[test]
    fn test_format_recovery_event_line_failure() {
        let event = RecoveryEvent::new(
            10000,
            ConnectionStatus::CheckTimeout,
            ConnectionStatus::ChromeUnreachable,
            "等待 Chrome 重启",
            30000,
            false,
            Some("Chrome 未恢复"),
        );
        let line = format_recovery_event_line(&event);
        assert!(line.contains("❌"));
        assert!(line.contains("等待 Chrome 重启"));
        assert!(line.contains("连接检查超时"));
        assert!(line.contains("Chrome 不可达"));
        assert!(line.contains("30000ms"));
    }

    #[test]
    fn test_format_recovery_event_line_zero_duration() {
        let event = RecoveryEvent::new(
            0,
            ConnectionStatus::TabClosed,
            ConnectionStatus::Connected,
            "立即恢复",
            0,
            true,
            None,
        );
        let line = format_recovery_event_line(&event);
        assert!(line.contains("✅"));
        assert!(line.contains("0ms"));
    }

    #[test]
    fn test_format_recovery_event_line_large_timestamp() {
        let event = RecoveryEvent::new(
            3_600_000, // 1 hour in ms
            ConnectionStatus::ChromeUnreachable,
            ConnectionStatus::Connected,
            "长时间恢复",
            5000,
            true,
            None,
        );
        let line = format_recovery_event_line(&event);
        assert!(line.contains("[3600s]"));
    }

    // ===== ConnectionMonitor 新方法 (委托纯函数) =====

    #[test]
    fn test_monitor_health_level_healthy() {
        let monitor = ConnectionMonitor::new(9222);
        assert_eq!(monitor.health_level(), HealthLevel::Healthy);
    }

    #[test]
    fn test_monitor_health_level_degraded() {
        let mut monitor = ConnectionMonitor::new(9222);
        monitor.last_status = ConnectionStatus::TabClosed;
        monitor.consecutive_failures = 1;
        assert_eq!(monitor.health_level(), HealthLevel::Degraded);
    }

    #[test]
    fn test_monitor_health_level_critical() {
        let mut monitor = ConnectionMonitor::new(9222);
        monitor.last_status = ConnectionStatus::ChromeUnreachable;
        monitor.consecutive_failures = 3;
        assert_eq!(monitor.health_level(), HealthLevel::Critical);
    }

    #[test]
    fn test_monitor_severity() {
        let monitor = ConnectionMonitor::new(9222);
        assert_eq!(monitor.severity(), ConnectionSeverity::Info);

        let mut monitor = ConnectionMonitor::new(9222);
        monitor.last_status = ConnectionStatus::TabClosed;
        assert_eq!(monitor.severity(), ConnectionSeverity::Warning);

        let mut monitor = ConnectionMonitor::new(9222);
        monitor.last_status = ConnectionStatus::ChromeUnreachable;
        assert_eq!(monitor.severity(), ConnectionSeverity::Critical);
    }

    #[test]
    fn test_monitor_should_trigger_recovery() {
        let monitor = ConnectionMonitor::new(9222);
        assert!(!monitor.should_trigger_recovery());

        let mut monitor = ConnectionMonitor::new(9222);
        monitor.last_status = ConnectionStatus::TabClosed;
        monitor.consecutive_failures = 1;
        assert!(monitor.should_trigger_recovery());

        let mut monitor = ConnectionMonitor::new(9222);
        monitor.last_status = ConnectionStatus::ChromeUnreachable;
        monitor.consecutive_failures = 3;
        assert!(monitor.should_trigger_recovery());
    }

    #[test]
    fn test_monitor_next_check_delay() {
        let monitor = ConnectionMonitor::new(9222);
        // Connected, heartbeat=30 → 30
        assert_eq!(monitor.next_check_delay_secs(), 30);

        let mut monitor = ConnectionMonitor::new(9222);
        monitor.last_status = ConnectionStatus::TabClosed;
        monitor.consecutive_failures = 1;
        // Not connected, 1 failure, heartbeat=30 → 30/2=15
        assert_eq!(monitor.next_check_delay_secs(), 15);
    }

    #[test]
    fn test_monitor_next_check_delay_disabled() {
        let config = MonitorConfig {
            port: 9222,
            check_timeout_secs: 10,
            heartbeat_interval_secs: 0,
            max_consecutive_failures: 3,
        };
        let monitor = ConnectionMonitor::with_config(config);
        assert_eq!(monitor.next_check_delay_secs(), 0);
    }

    // ===== summary 一致性 (使用纯函数) =====

    #[test]
    fn test_summary_uses_pure_function() {
        let mut monitor = ConnectionMonitor::new(9222);
        monitor.total_checks = 200;
        monitor.total_failures = 10;

        let summary = monitor.summary();
        let expected = calculate_monitor_success_rate(200, 10);
        assert!((summary.success_rate - expected).abs() < 0.001);
    }

    // ===== to_report 一致性 (使用纯函数) =====

    #[test]
    fn test_report_uses_format_uptime() {
        let mut monitor = ConnectionMonitor::new(9222);
        monitor.total_checks = 10;
        monitor.total_failures = 1;
        monitor.consecutive_failures = 1;
        monitor.last_status = ConnectionStatus::Connected;

        let summary = monitor.summary();
        let report = summary.to_report();
        assert!(report.contains(&format_uptime(summary.uptime_secs)));
    }

    #[test]
    fn test_report_uses_format_success_rate() {
        let mut monitor = ConnectionMonitor::new(9222);
        monitor.total_checks = 100;
        monitor.total_failures = 5;

        let summary = monitor.summary();
        let report = summary.to_report();
        assert!(report.contains(&format_monitor_success_rate(summary.success_rate)));
    }

    #[test]
    fn test_report_uses_format_recovery_event_line() {
        let mut monitor = ConnectionMonitor::new(9222);
        let event = RecoveryEvent::new(
            0,
            ConnectionStatus::ChromeUnreachable,
            ConnectionStatus::Connected,
            "测试恢复",
            5000,
            true,
            None,
        );
        monitor.record_recovery_event(event);

        let summary = monitor.summary();
        let report = summary.to_report();
        assert!(report.contains("测试恢复"));
        assert!(report.contains("5000ms"));
    }
}
