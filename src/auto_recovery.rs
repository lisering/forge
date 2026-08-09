//! 自动恢复机制 — Chrome 断连后自动重连
//!
//! 当 `ConnectionMonitor` 检测到连接异常时, `AutoRecovery` 执行恢复策略:
//! - 指数退避重试 (等待 2^n 秒后重试)
//! - 最多 N 次重试, 超过后放弃
//! - 每次重试记录恢复事件到 DevTrace
//!
//! ## 恢复流程
//!
//! 1. 检测到连接异常 (ConnectionStatus != Connected)
//! 2. 等待退避时间 (2^attempt 秒, 上限 60s)
//! 3. 重新检查连接
//! 4. 如果恢复 → 重置监控器, 返回 Success
//! 5. 如果未恢复 → 增加退避时间, 重试
//! 6. 超过最大重试次数 → 返回 Failed
//!
//! ## 与现有机制的关系
//!
//! - **ConnectionMonitor**: 检测连接状态
//! - **AutoRecovery (本模块)**: 执行恢复策略
//! - **Orchestrator**: 调用 AutoRecovery, 恢复后从 Memory 断点续传
//! - **DevTrace**: 记录恢复事件 (TraceAction::Recovery)

use crate::connection_monitor::{ConnectionMonitor, ConnectionStatus, HealthLevel, RecoveryEvent};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

// ============================================================================
//  BackoffStrategy — 退避策略
// ============================================================================

/// 指数退避策略 — 计算每次重试的等待时间
///
/// 策略: `base * 2^attempt`, 上限 `max_delay`。
/// 例如 base=2, max=60:
/// - 第 1 次: 2s
/// - 第 2 次: 4s
/// - 第 3 次: 8s
/// - 第 4 次: 16s
/// - 第 5 次: 32s
/// - 第 6 次: 60s (上限)
/// - 第 7+ 次: 60s
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackoffStrategy {
    /// 基础延迟 (秒)
    pub base_secs: u64,
    /// 最大延迟 (秒)
    pub max_delay_secs: u64,
}

impl Default for BackoffStrategy {
    fn default() -> Self {
        Self {
            base_secs: 2,
            max_delay_secs: 60,
        }
    }
}

impl BackoffStrategy {
    /// 创建指数退避策略
    pub fn new(base_secs: u64, max_delay_secs: u64) -> Self {
        let clamped_base = base_secs.max(1);
        Self {
            base_secs: clamped_base,
            max_delay_secs: max_delay_secs.max(clamped_base),
        }
    }

    /// 计算第 attempt 次重试的等待时间 (秒)
    ///
    /// attempt 从 1 开始 (第 1 次重试等待 base * 2^1 = base * 2 秒)。
    pub fn delay_secs(&self, attempt: u32) -> u64 {
        let delay = self.base_secs.saturating_mul(2u64.saturating_pow(attempt));
        delay.min(self.max_delay_secs)
    }

    /// 计算从第 1 次到第 max_attempts 次的总等待时间 (秒)
    pub fn total_delay_secs(&self, max_attempts: u32) -> u64 {
        (1..=max_attempts).map(|a| self.delay_secs(a)).sum()
    }
}

// ============================================================================
//  RecoveryConfig — 恢复配置
// ============================================================================

/// 自动恢复配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryConfig {
    /// 最大重试次数 (默认 10)
    pub max_retries: u32,
    /// 退避策略
    pub backoff: BackoffStrategy,
    /// Chrome 调试端口
    pub port: u16,
}

impl Default for RecoveryConfig {
    fn default() -> Self {
        Self {
            max_retries: 10,
            backoff: BackoffStrategy::default(),
            port: 9222,
        }
    }
}

impl RecoveryConfig {
    /// 创建恢复配置
    pub fn new(port: u16, max_retries: u32) -> Self {
        Self {
            port,
            max_retries,
            backoff: BackoffStrategy::default(),
        }
    }

    /// 设置退避策略
    pub fn with_backoff(mut self, backoff: BackoffStrategy) -> Self {
        self.backoff = backoff;
        self
    }
}

// ============================================================================
//  RecoveryResult — 恢复结果
// ============================================================================

/// 自动恢复的结果
#[derive(Debug, Clone)]
pub enum RecoveryResult {
    /// 恢复成功 — Chrome 重新可达, 标签页存在
    ///
    /// `attempts` 表示用了多少次重试才成功 (0 = 第一次就成功)。
    Success {
        /// 重试次数 (0 = 立即成功, 1 = 重试 1 次后成功, ...)
        attempts: u32,
        /// 总耗时 (毫秒)
        total_duration_ms: u64,
    },
    /// 恢复失败 — 超过最大重试次数仍未恢复
    Failed {
        /// 尝试次数
        attempts: u32,
        /// 总耗时 (毫秒)
        total_duration_ms: u64,
        /// 最后的连接状态
        last_status: ConnectionStatus,
        /// 错误信息
        error: String,
    },
}

impl RecoveryResult {
    /// 是否恢复成功
    pub fn is_success(&self) -> bool {
        matches!(self, RecoveryResult::Success { .. })
    }

    /// 是否恢复失败
    pub fn is_failed(&self) -> bool {
        matches!(self, RecoveryResult::Failed { .. })
    }

    /// 获取重试次数
    pub fn attempts(&self) -> u32 {
        match self {
            RecoveryResult::Success { attempts, .. } => *attempts,
            RecoveryResult::Failed { attempts, .. } => *attempts,
        }
    }

    /// 获取总耗时 (毫秒)
    pub fn total_duration_ms(&self) -> u64 {
        match self {
            RecoveryResult::Success {
                total_duration_ms, ..
            } => *total_duration_ms,
            RecoveryResult::Failed {
                total_duration_ms, ..
            } => *total_duration_ms,
        }
    }
}

// ============================================================================
//  纯逻辑函数 — 自动恢复核心算法 (无副作用, 可独立测试)
// ===========================================================================
//
// 以下函数将自动恢复的核心决策逻辑提取为纯函数, 使得:
// 1. 可以在无 Chrome 环境下完全测试
// 2. 决策逻辑集中管理, 修改策略时只需改一处
// 3. 与 connection_monitor.rs 的纯函数协同, 形成完整的 24h 可靠性链路

/// 恢复动作 — `decide_recovery_action` 返回的决策
///
/// 表示在某次检查后应该执行的动作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryAction {
    /// 恢复成功 — 连接已恢复, 无需更多操作
    ///
    /// `attempts` 表示在第几次重试时成功 (0 = 立即成功)。
    Succeed {
        /// 成功时的重试次数
        attempts: u32,
    },
    /// 继续重试 — 等待退避时间后重试
    Retry {
        /// 下一次重试的编号 (从 1 开始)
        next_attempt: u32,
        /// 等待时间 (秒)
        delay_secs: u64,
    },
    /// 放弃恢复 — 已超过最大重试次数
    GiveUp {
        /// 总尝试次数
        attempts: u32,
    },
}

/// 恢复策略 — 根据 `ConnectionStatus` 选择不同的恢复方式
///
/// 由 [`select_recovery_strategy`] 根据连接状态计算。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryStrategy {
    /// 无需恢复 — 连接正常
    None,
    /// 简单重试 — 标签页关闭或检查超时, 重新检查即可
    SimpleRetry,
    /// WebSocket 重连 — WebSocket 连接异常, 需要重新建立连接
    WebSocketReconnect,
    /// Chrome 重启恢复 — Chrome 不可达, 需要等待 Chrome 重启
    ChromeRestart,
}

impl RecoveryStrategy {
    /// 中文描述
    pub fn description(&self) -> &'static str {
        match self {
            RecoveryStrategy::None => "无需恢复",
            RecoveryStrategy::SimpleRetry => "简单重试",
            RecoveryStrategy::WebSocketReconnect => "WebSocket 重连",
            RecoveryStrategy::ChromeRestart => "Chrome 重启恢复",
        }
    }

    /// 恢复难度等级 (0=无需, 1=简单, 2=中等, 3=困难)
    pub fn difficulty(&self) -> u8 {
        match self {
            RecoveryStrategy::None => 0,
            RecoveryStrategy::SimpleRetry => 1,
            RecoveryStrategy::WebSocketReconnect => 2,
            RecoveryStrategy::ChromeRestart => 3,
        }
    }
}

impl std::fmt::Display for RecoveryStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.description())
    }
}

/// 恢复紧急度 — 根据 `HealthLevel` 评估恢复的紧迫程度
///
/// 由 [`assess_recovery_urgency`] 根据健康等级计算。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryUrgency {
    /// 无需恢复 — 连接健康
    None,
    /// 低紧急度 — 降级状态, 可延后恢复
    Low,
    /// 高紧急度 — 需要尽快恢复
    High,
    /// 严重紧急度 — 必须立即恢复
    Critical,
}

impl RecoveryUrgency {
    /// 中文描述
    pub fn description(&self) -> &'static str {
        match self {
            RecoveryUrgency::None => "无需恢复",
            RecoveryUrgency::Low => "低紧急度",
            RecoveryUrgency::High => "高紧急度",
            RecoveryUrgency::Critical => "严重紧急度",
        }
    }

    /// 是否需要立即恢复
    pub fn requires_immediate_recovery(&self) -> bool {
        matches!(self, RecoveryUrgency::Critical)
    }
}

impl std::fmt::Display for RecoveryUrgency {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.description())
    }
}

/// 决定恢复动作 — 根据连接状态和重试进度决定下一步
///
/// 在每次检查连接后调用, 返回应该执行的动作。
///
/// # 逻辑
/// - 连接正常 → `Succeed` (恢复成功)
/// - 连接异常且未超过最大重试次数 → `Retry` (等待退避时间后重试)
/// - 连接异常且已超过最大重试次数 → `GiveUp` (放弃恢复)
///
/// # 参数
/// - `is_connected`: 当前连接是否正常
/// - `attempt`: 当前重试次数 (0 = 第一次检查, 1 = 第一次重试, ...)
/// - `max_retries`: 最大重试次数
/// - `backoff`: 退避策略
///
/// # 示例
///
/// ```
/// use forge::auto_recovery::{decide_recovery_action, RecoveryAction, BackoffStrategy};
///
/// let backoff = BackoffStrategy::new(2, 60);
///
/// // 连接正常 — 立即成功
/// assert_eq!(
///     decide_recovery_action(true, 0, 10, &backoff),
///     RecoveryAction::Succeed { attempts: 0 }
/// );
///
/// // 连接异常, 第 0 次检查, 还有重试机会 — 重试
/// let action = decide_recovery_action(false, 0, 10, &backoff);
/// assert!(matches!(action, RecoveryAction::Retry { next_attempt: 1, .. }));
///
/// // 连接异常, 已达最大重试次数 — 放弃
/// assert_eq!(
///     decide_recovery_action(false, 10, 10, &backoff),
///     RecoveryAction::GiveUp { attempts: 10 }
/// );
/// ```
pub fn decide_recovery_action(
    is_connected: bool,
    attempt: u32,
    max_retries: u32,
    backoff: &BackoffStrategy,
) -> RecoveryAction {
    if is_connected {
        RecoveryAction::Succeed { attempts: attempt }
    } else if should_continue_retrying(attempt, max_retries) {
        let next_attempt = attempt + 1;
        let delay_secs = backoff.delay_secs(next_attempt);
        RecoveryAction::Retry {
            next_attempt,
            delay_secs,
        }
    } else {
        RecoveryAction::GiveUp {
            attempts: max_retries,
        }
    }
}

/// 判断是否应该继续重试
///
/// 当当前重试次数小于最大重试次数时, 还有重试机会。
///
/// # 参数
/// - `attempt`: 当前重试次数 (0 = 第一次检查)
/// - `max_retries`: 最大重试次数
///
/// # 返回值
/// `true` 表示还有重试机会
///
/// # 示例
///
/// ```
/// use forge::auto_recovery::should_continue_retrying;
///
/// assert!(should_continue_retrying(0, 10));  // 还有 10 次机会
/// assert!(should_continue_retrying(9, 10));  // 还有 1 次机会
/// assert!(!should_continue_retrying(10, 10)); // 已用完
/// assert!(!should_continue_retrying(11, 10)); // 已超过
/// ```
pub fn should_continue_retrying(attempt: u32, max_retries: u32) -> bool {
    attempt < max_retries
}

/// 计算完整的退避时间计划
///
/// 返回从第 1 次到第 max_attempts 次的每次等待时间列表。
///
/// # 参数
/// - `backoff`: 退避策略
/// - `max_attempts`: 最大重试次数
///
/// # 示例
///
/// ```
/// use forge::auto_recovery::{compute_backoff_schedule, BackoffStrategy};
///
/// let backoff = BackoffStrategy::new(2, 60);
/// let schedule = compute_backoff_schedule(&backoff, 4);
/// // attempt 1: 4, attempt 2: 8, attempt 3: 16, attempt 4: 32
/// assert_eq!(schedule, vec![4, 8, 16, 32]);
///
/// // 0 次重试 — 空列表
/// assert!(compute_backoff_schedule(&backoff, 0).is_empty());
/// ```
pub fn compute_backoff_schedule(backoff: &BackoffStrategy, max_attempts: u32) -> Vec<u64> {
    (1..=max_attempts).map(|a| backoff.delay_secs(a)).collect()
}

/// 估算最大恢复时间 (秒)
///
/// 计算从第 1 次到最后一次重试的退避等待总时间。
/// 不包含实际的连接检查时间, 仅计算 sleep 等待时间。
///
/// # 参数
/// - `config`: 恢复配置
///
/// # 示例
///
/// ```
/// use forge::auto_recovery::{estimate_max_recovery_secs, RecoveryConfig, BackoffStrategy};
///
/// let config = RecoveryConfig::new(9222, 3)
///     .with_backoff(BackoffStrategy::new(2, 60));
/// // 退避: 4 + 8 + 16 = 28 秒
/// assert_eq!(estimate_max_recovery_secs(&config), 28);
/// ```
pub fn estimate_max_recovery_secs(config: &RecoveryConfig) -> u64 {
    config.backoff.total_delay_secs(config.max_retries)
}

/// 根据连接状态选择恢复策略
///
/// 将 `ConnectionStatus` 映射到对应的 `RecoveryStrategy`。
/// 与 `connection_monitor::classify_connection_severity` 协同工作。
///
/// # 映射
/// - `Connected` → `None` (无需恢复)
/// - `TabClosed`, `CheckTimeout` → `SimpleRetry` (简单重试)
/// - `WebSocketError` → `WebSocketReconnect` (WebSocket 重连)
/// - `ChromeUnreachable` → `ChromeRestart` (Chrome 重启恢复)
///
/// # 示例
///
/// ```
/// use forge::auto_recovery::{select_recovery_strategy, RecoveryStrategy};
/// use forge::connection_monitor::ConnectionStatus;
///
/// assert_eq!(
///     select_recovery_strategy(&ConnectionStatus::Connected),
///     RecoveryStrategy::None
/// );
/// assert_eq!(
///     select_recovery_strategy(&ConnectionStatus::TabClosed),
///     RecoveryStrategy::SimpleRetry
/// );
/// assert_eq!(
///     select_recovery_strategy(&ConnectionStatus::ChromeUnreachable),
///     RecoveryStrategy::ChromeRestart
/// );
/// ```
pub fn select_recovery_strategy(status: &ConnectionStatus) -> RecoveryStrategy {
    match status {
        ConnectionStatus::Connected => RecoveryStrategy::None,
        ConnectionStatus::TabClosed | ConnectionStatus::CheckTimeout => {
            RecoveryStrategy::SimpleRetry
        }
        ConnectionStatus::WebSocketError(_) => RecoveryStrategy::WebSocketReconnect,
        ConnectionStatus::ChromeUnreachable => RecoveryStrategy::ChromeRestart,
    }
}

/// 根据健康等级评估恢复紧急度
///
/// 将 `connection_monitor::HealthLevel` 映射到 `RecoveryUrgency`。
/// 与 `connection_monitor::determine_health_level` 协同工作。
///
/// # 映射
/// - `Healthy` → `None` (无需恢复)
/// - `Degraded` → `Low` (低紧急度, 可延后)
/// - `Critical` → `Critical` (严重, 必须立即恢复)
///
/// # 示例
///
/// ```
/// use forge::auto_recovery::{assess_recovery_urgency, RecoveryUrgency};
/// use forge::connection_monitor::HealthLevel;
///
/// assert_eq!(
///     assess_recovery_urgency(&HealthLevel::Healthy),
///     RecoveryUrgency::None
/// );
/// assert_eq!(
///     assess_recovery_urgency(&HealthLevel::Degraded),
///     RecoveryUrgency::Low
/// );
/// assert_eq!(
///     assess_recovery_urgency(&HealthLevel::Critical),
///     RecoveryUrgency::Critical
/// );
/// ```
pub fn assess_recovery_urgency(health_level: &HealthLevel) -> RecoveryUrgency {
    match health_level {
        HealthLevel::Healthy => RecoveryUrgency::None,
        HealthLevel::Degraded => RecoveryUrgency::Low,
        HealthLevel::Critical => RecoveryUrgency::Critical,
    }
}

/// 计算恢复成功率
///
/// 成功率 = 成功次数 / 总恢复次数。
/// 当总恢复次数为 0 时, 返回 1.0 (视为完全成功)。
///
/// # 参数
/// - `total_recoveries`: 总恢复次数
/// - `total_successes`: 总成功次数
///
/// # 返回值
/// 成功率, 范围 [0.0, 1.0]
///
/// # 示例
///
/// ```
/// use forge::auto_recovery::compute_recovery_success_rate;
///
/// // 零恢复 — 默认成功
/// assert!((compute_recovery_success_rate(0, 0) - 1.0).abs() < 0.001);
///
/// // 全部成功
/// assert!((compute_recovery_success_rate(10, 10) - 1.0).abs() < 0.001);
///
/// // 一半成功
/// assert!((compute_recovery_success_rate(10, 5) - 0.5).abs() < 0.001);
/// ```
pub fn compute_recovery_success_rate(total_recoveries: u64, total_successes: u64) -> f64 {
    if total_recoveries == 0 {
        return 1.0;
    }
    let successes = total_successes.min(total_recoveries);
    successes as f64 / total_recoveries as f64
}

/// 格式化恢复成功率为百分比字符串
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
/// use forge::auto_recovery::format_recovery_rate;
///
/// assert_eq!(format_recovery_rate(1.0), "100.0%");
/// assert_eq!(format_recovery_rate(0.0), "0.0%");
/// assert_eq!(format_recovery_rate(0.85), "85.0%");
/// ```
pub fn format_recovery_rate(rate: f64) -> String {
    let clamped = rate.clamp(0.0, 1.0);
    format!("{:.1}%", clamped * 100.0)
}

/// 构造恢复成功结果
///
/// # 参数
/// - `attempts`: 重试次数 (0 = 立即成功)
/// - `total_duration_ms`: 总耗时 (毫秒)
///
/// # 示例
///
/// ```
/// use forge::auto_recovery::{make_success_result, RecoveryResult};
///
/// let result = make_success_result(3, 15000);
/// assert!(result.is_success());
/// assert_eq!(result.attempts(), 3);
/// assert_eq!(result.total_duration_ms(), 15000);
/// ```
pub fn make_success_result(attempts: u32, total_duration_ms: u64) -> RecoveryResult {
    RecoveryResult::Success {
        attempts,
        total_duration_ms,
    }
}

/// 构造恢复失败结果
///
/// # 参数
/// - `attempts`: 尝试次数
/// - `total_duration_ms`: 总耗时 (毫秒)
/// - `last_status`: 最后的连接状态
/// - `error`: 错误信息
///
/// # 示例
///
/// ```
/// use forge::auto_recovery::{make_failed_result, RecoveryResult};
/// use forge::connection_monitor::ConnectionStatus;
///
/// let result = make_failed_result(
///     10, 60000,
///     ConnectionStatus::ChromeUnreachable,
///     "超过最大重试次数",
/// );
/// assert!(result.is_failed());
/// assert_eq!(result.attempts(), 10);
/// ```
pub fn make_failed_result(
    attempts: u32,
    total_duration_ms: u64,
    last_status: ConnectionStatus,
    error: &str,
) -> RecoveryResult {
    RecoveryResult::Failed {
        attempts,
        total_duration_ms,
        last_status,
        error: error.to_string(),
    }
}

/// 计算恢复效率
///
/// 恢复效率 = 1.0 - (attempts / max_retries)。
/// - 立即成功 (attempts=0) → 效率 1.0 (100%)
/// - 在最大重试次数时成功 → 效率 0.0 (0%)
///
/// # 参数
/// - `attempts`: 实际重试次数
/// - `max_retries`: 最大重试次数
///
/// # 返回值
/// 效率值, 范围 [0.0, 1.0]。当 max_retries=0 时返回 1.0。
///
/// # 示例
///
/// ```
/// use forge::auto_recovery::recovery_efficiency;
///
/// // 立即成功 — 100% 效率
/// assert!((recovery_efficiency(0, 10) - 1.0).abs() < 0.001);
///
/// // 在一半时成功 — 50% 效率
/// assert!((recovery_efficiency(5, 10) - 0.5).abs() < 0.001);
///
/// // 在最大次数时成功 — 0% 效率
/// assert!((recovery_efficiency(10, 10) - 0.0).abs() < 0.001);
/// ```
pub fn recovery_efficiency(attempts: u32, max_retries: u32) -> f64 {
    if max_retries == 0 {
        return 1.0;
    }
    let ratio = (attempts.min(max_retries)) as f64 / max_retries as f64;
    (1.0 - ratio).clamp(0.0, 1.0)
}

/// 获取 RecoveryResult::Failed 的错误信息 (辅助函数)
pub fn result_error(result: &RecoveryResult) -> String {
    match result {
        RecoveryResult::Failed { error, .. } => error.clone(),
        _ => String::new(),
    }
}

// ============================================================================
//  AutoRecovery — 自动恢复器
// ============================================================================

/// 自动恢复器 — Chrome 断连后自动重连
///
/// 使用指数退避策略, 在检测到连接异常后自动重试。
/// 每次重试通过 `ConnectionMonitor::check_connection()` 检查是否恢复。
///
/// ## 使用方式
///
/// ```ignore
/// let mut monitor = ConnectionMonitor::new(9222);
/// let mut recovery = AutoRecovery::new(RecoveryConfig::default());
///
/// let status = monitor.check_connection().await;
/// if status.needs_recovery() {
///     let result = recovery.recover(&mut monitor).await;
///     if result.is_success() {
///         // 从 Memory 断点续传
///     }
/// }
/// ```
pub struct AutoRecovery {
    /// 恢复配置
    config: RecoveryConfig,
    /// 恢复历史 (记录每次恢复的结果)
    recovery_history: Vec<RecoveryResult>,
    /// 总恢复次数
    pub total_recoveries: u64,
    /// 总成功次数
    pub total_successes: u64,
}

impl AutoRecovery {
    /// 创建自动恢复器
    pub fn new(config: RecoveryConfig) -> Self {
        Self {
            config,
            recovery_history: vec![],
            total_recoveries: 0,
            total_successes: 0,
        }
    }

    /// 使用默认配置创建 (端口 9222, 最多 10 次重试)
    pub fn with_port(port: u16) -> Self {
        Self::new(RecoveryConfig::new(port, 10))
    }

    /// 获取配置
    pub fn config(&self) -> &RecoveryConfig {
        &self.config
    }

    /// 获取恢复历史
    pub fn recovery_history(&self) -> &[RecoveryResult] {
        &self.recovery_history
    }

    /// 获取总恢复次数
    pub fn total_recoveries(&self) -> u64 {
        self.total_recoveries
    }

    /// 获取总成功次数
    pub fn total_successes(&self) -> u64 {
        self.total_successes
    }

    /// 成功率 — 委托 [`compute_recovery_success_rate`]
    pub fn success_rate(&self) -> f64 {
        compute_recovery_success_rate(self.total_recoveries, self.total_successes)
    }

    /// 获取当前恢复策略 — 委托 [`select_recovery_strategy`]
    pub fn recovery_strategy(&self, status: &ConnectionStatus) -> RecoveryStrategy {
        select_recovery_strategy(status)
    }

    /// 获取恢复紧急度 — 委托 [`assess_recovery_urgency`]
    pub fn recovery_urgency(&self, health_level: &HealthLevel) -> RecoveryUrgency {
        assess_recovery_urgency(health_level)
    }

    /// 估算最大恢复时间 (秒) — 委托 [`estimate_max_recovery_secs`]
    pub fn estimated_max_recovery_secs(&self) -> u64 {
        estimate_max_recovery_secs(&self.config)
    }

    /// 执行自动恢复
    ///
    /// 使用指数退避策略重试连接:
    /// 1. 立即检查一次 (attempt 0)
    /// 2. 如果未恢复, 等待退避时间后重试
    /// 3. 重复直到成功或达到最大重试次数
    ///
    /// 注意: 此方法会 sleep 等待退避时间, 在测试中可使用 `recover_no_wait`。
    pub async fn recover(&mut self, monitor: &mut ConnectionMonitor) -> RecoveryResult {
        self.total_recoveries += 1;
        let start = Instant::now();
        let max_retries = self.config.max_retries;

        info!("🔄 开始自动恢复 (最多 {} 次重试)", max_retries);

        for attempt in 0..=max_retries {
            // 检查连接
            let status = monitor.check_connection().await;

            // 使用纯函数决策
            let action = decide_recovery_action(
                status.is_connected(),
                attempt,
                max_retries,
                &self.config.backoff,
            );

            match action {
                RecoveryAction::Succeed { attempts } => {
                    let duration = start.elapsed().as_millis() as u64;
                    info!("✅ 恢复成功 (第 {} 次重试, 耗时 {}ms)", attempts, duration);

                    monitor.record_recovery_event(RecoveryEvent::new(
                        0,
                        monitor.last_status().clone(),
                        ConnectionStatus::Connected,
                        &format!("自动恢复第 {} 次重试成功", attempts),
                        duration,
                        true,
                        None,
                    ));

                    monitor.reset();
                    self.total_successes += 1;

                    let result = make_success_result(attempts, duration);
                    self.recovery_history.push(result.clone());
                    return result;
                }
                RecoveryAction::Retry {
                    next_attempt,
                    delay_secs,
                } => {
                    warn!(
                        "⚠️ 第 {} 次检查未恢复 ({}), 等待 {}s 后重试...",
                        next_attempt,
                        status.description(),
                        delay_secs
                    );
                    tokio::time::sleep(Duration::from_secs(delay_secs)).await;
                }
                RecoveryAction::GiveUp { .. } => {
                    error!("❌ 自动恢复失败: 超过最大重试次数 {}", max_retries);
                }
            }
        }

        // 恢复失败
        let duration = start.elapsed().as_millis() as u64;
        let last_status = monitor.last_status().clone();

        monitor.record_recovery_event(RecoveryEvent::new(
            0,
            last_status.clone(),
            last_status.clone(),
            &format!("自动恢复失败 ({} 次重试)", max_retries),
            duration,
            false,
            Some("超过最大重试次数"),
        ));

        let result = make_failed_result(
            max_retries,
            duration,
            last_status,
            &format!("超过最大重试次数 {}", max_retries),
        );
        self.recovery_history.push(result.clone());
        result
    }

    /// 执行自动恢复 (不等待退避时间 — 用于测试)
    ///
    /// 与 `recover` 相同, 但不 sleep 等待退避时间。
    pub async fn recover_no_wait(&mut self, monitor: &mut ConnectionMonitor) -> RecoveryResult {
        self.total_recoveries += 1;
        let start = Instant::now();
        let max_retries = self.config.max_retries;

        for attempt in 0..=max_retries {
            let status = monitor.check_connection().await;

            // 使用纯函数决策
            let action = decide_recovery_action(
                status.is_connected(),
                attempt,
                max_retries,
                &self.config.backoff,
            );

            match action {
                RecoveryAction::Succeed { attempts } => {
                    let duration = start.elapsed().as_millis() as u64;
                    monitor.record_recovery_event(RecoveryEvent::new(
                        0,
                        monitor.last_status().clone(),
                        ConnectionStatus::Connected,
                        &format!("恢复第 {} 次", attempts),
                        duration,
                        true,
                        None,
                    ));
                    monitor.reset();
                    self.total_successes += 1;

                    let result = make_success_result(attempts, duration);
                    self.recovery_history.push(result.clone());
                    return result;
                }
                RecoveryAction::Retry { next_attempt, .. } => {
                    debug!(
                        "第 {} 次检查未恢复 ({}), 立即重试",
                        next_attempt,
                        status.description()
                    );
                }
                RecoveryAction::GiveUp { .. } => {}
            }
        }

        let duration = start.elapsed().as_millis() as u64;
        let last_status = monitor.last_status().clone();
        let result = make_failed_result(
            max_retries,
            duration,
            last_status.clone(),
            &format!("超过最大重试次数 {}", max_retries),
        );
        monitor.record_recovery_event(RecoveryEvent::new(
            0,
            last_status.clone(),
            last_status,
            "恢复失败",
            duration,
            false,
            Some(&result_error(&result)),
        ));
        self.recovery_history.push(result.clone());
        result
    }

    /// 生成恢复摘要
    pub fn summary(&self) -> AutoRecoverySummary {
        AutoRecoverySummary {
            total_recoveries: self.total_recoveries,
            total_successes: self.total_successes,
            success_rate: self.success_rate(),
            config: self.config.clone(),
        }
    }
}

// ============================================================================
//  AutoRecoverySummary — 恢复摘要
// ============================================================================

/// 自动恢复摘要
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoRecoverySummary {
    /// 总恢复次数
    pub total_recoveries: u64,
    /// 总成功次数
    pub total_successes: u64,
    /// 成功率
    pub success_rate: f64,
    /// 恢复配置
    pub config: RecoveryConfig,
}

impl AutoRecoverySummary {
    /// 生成可读报告 — 使用 [`format_recovery_rate`] 纯函数
    pub fn to_report(&self) -> String {
        let mut report = String::new();
        report.push_str("═══════════════════════════════════════════════════\n");
        report.push_str("  🔧 自动恢复报告\n");
        report.push_str("═══════════════════════════════════════════════════\n\n");

        report.push_str(&format!("  总恢复次数: {}\n", self.total_recoveries));
        report.push_str(&format!("  成功次数: {}\n", self.total_successes));
        report.push_str(&format!(
            "  成功率: {}\n",
            format_recovery_rate(self.success_rate)
        ));
        report.push_str(&format!("  最大重试: {}\n", self.config.max_retries));
        report.push_str(&format!(
            "  退避策略: base={}s max={}s\n",
            self.config.backoff.base_secs, self.config.backoff.max_delay_secs
        ));
        report
    }
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ===== BackoffStrategy =====

    #[test]
    fn test_backoff_default() {
        let b = BackoffStrategy::default();
        assert_eq!(b.base_secs, 2);
        assert_eq!(b.max_delay_secs, 60);
    }

    #[test]
    fn test_backoff_delay() {
        let b = BackoffStrategy::new(2, 60);
        // attempt 1: 2 * 2^1 = 4
        assert_eq!(b.delay_secs(1), 4);
        // attempt 2: 2 * 2^2 = 8
        assert_eq!(b.delay_secs(2), 8);
        // attempt 3: 2 * 2^3 = 16
        assert_eq!(b.delay_secs(3), 16);
        // attempt 4: 2 * 2^4 = 32
        assert_eq!(b.delay_secs(4), 32);
        // attempt 5: 2 * 2^5 = 64 → capped at 60
        assert_eq!(b.delay_secs(5), 60);
        // attempt 6: capped
        assert_eq!(b.delay_secs(6), 60);
    }

    #[test]
    fn test_backoff_delay_custom() {
        let b = BackoffStrategy::new(1, 10);
        // attempt 1: 1 * 2^1 = 2
        assert_eq!(b.delay_secs(1), 2);
        // attempt 2: 1 * 2^2 = 4
        assert_eq!(b.delay_secs(2), 4);
        // attempt 3: 1 * 2^3 = 8
        assert_eq!(b.delay_secs(3), 8);
        // attempt 4: 1 * 2^4 = 16 → capped at 10
        assert_eq!(b.delay_secs(4), 10);
    }

    #[test]
    fn test_backoff_total_delay() {
        let b = BackoffStrategy::new(2, 60);
        // 4 + 8 + 16 = 28
        assert_eq!(b.total_delay_secs(3), 28);
    }

    #[test]
    fn test_backoff_total_delay_capped() {
        let b = BackoffStrategy::new(2, 60);
        // 4 + 8 + 16 + 32 + 60 + 60 = 180
        assert_eq!(b.total_delay_secs(6), 180);
    }

    #[test]
    fn test_backoff_new_clamps() {
        let b = BackoffStrategy::new(0, 0);
        // base should be clamped to 1
        assert_eq!(b.base_secs, 1);
        // max should be at least base
        assert_eq!(b.max_delay_secs, 1);
    }

    // ===== RecoveryConfig =====

    #[test]
    fn test_recovery_config_default() {
        let c = RecoveryConfig::default();
        assert_eq!(c.port, 9222);
        assert_eq!(c.max_retries, 10);
    }

    #[test]
    fn test_recovery_config_new() {
        let c = RecoveryConfig::new(9333, 5);
        assert_eq!(c.port, 9333);
        assert_eq!(c.max_retries, 5);
    }

    #[test]
    fn test_recovery_config_with_backoff() {
        let c = RecoveryConfig::new(9222, 10).with_backoff(BackoffStrategy::new(5, 120));
        assert_eq!(c.backoff.base_secs, 5);
        assert_eq!(c.backoff.max_delay_secs, 120);
    }

    // ===== RecoveryResult =====

    #[test]
    fn test_recovery_result_success() {
        let r = RecoveryResult::Success {
            attempts: 3,
            total_duration_ms: 10000,
        };
        assert!(r.is_success());
        assert!(!r.is_failed());
        assert_eq!(r.attempts(), 3);
        assert_eq!(r.total_duration_ms(), 10000);
    }

    #[test]
    fn test_recovery_result_failed() {
        let r = RecoveryResult::Failed {
            attempts: 10,
            total_duration_ms: 60000,
            last_status: ConnectionStatus::ChromeUnreachable,
            error: "超过最大重试次数".to_string(),
        };
        assert!(!r.is_success());
        assert!(r.is_failed());
        assert_eq!(r.attempts(), 10);
        assert_eq!(r.total_duration_ms(), 60000);
    }

    // ===== AutoRecovery =====

    #[test]
    fn test_auto_recovery_new() {
        let r = AutoRecovery::new(RecoveryConfig::default());
        assert_eq!(r.config().port, 9222);
        assert_eq!(r.config().max_retries, 10);
        assert_eq!(r.total_recoveries(), 0);
        assert_eq!(r.total_successes(), 0);
        assert!((r.success_rate() - 1.0).abs() < 0.001); // 默认成功率 1.0
    }

    #[test]
    fn test_auto_recovery_with_port() {
        let r = AutoRecovery::with_port(9333);
        assert_eq!(r.config().port, 9333);
    }

    #[tokio::test]
    async fn test_recover_no_wait_no_chrome() {
        // 在无 Chrome 环境下测试恢复失败
        let config = RecoveryConfig::new(19999, 3); // 3 次重试
        let mut recovery = AutoRecovery::new(config);
        let mut monitor = ConnectionMonitor::new(19999);

        let result = recovery.recover_no_wait(&mut monitor).await;

        assert!(result.is_failed());
        assert_eq!(result.attempts(), 3);
        assert_eq!(recovery.total_recoveries(), 1);
        assert_eq!(recovery.total_successes(), 0);
        assert_eq!(recovery.recovery_history().len(), 1);
    }

    #[tokio::test]
    async fn test_recover_records_history() {
        let config = RecoveryConfig::new(19999, 2);
        let mut recovery = AutoRecovery::new(config);
        let mut monitor = ConnectionMonitor::new(19999);

        // 第一次恢复
        let _ = recovery.recover_no_wait(&mut monitor).await;
        // 第二次恢复
        let _ = recovery.recover_no_wait(&mut monitor).await;

        assert_eq!(recovery.total_recoveries(), 2);
        assert_eq!(recovery.recovery_history().len(), 2);
    }

    #[tokio::test]
    async fn test_recover_updates_monitor_events() {
        let config = RecoveryConfig::new(19999, 2);
        let mut recovery = AutoRecovery::new(config);
        let mut monitor = ConnectionMonitor::new(19999);

        let _ = recovery.recover_no_wait(&mut monitor).await;

        // 应该有恢复事件记录
        assert!(!monitor.recovery_events().is_empty());
        let events = monitor.recovery_events();
        // 最后一个事件应该是失败的
        let last = events.last().unwrap();
        assert!(!last.success);
    }

    #[test]
    fn test_auto_recovery_summary() {
        let mut recovery = AutoRecovery::new(RecoveryConfig::default());
        recovery.total_recoveries = 10;
        recovery.total_successes = 8;

        let summary = recovery.summary();
        assert_eq!(summary.total_recoveries, 10);
        assert_eq!(summary.total_successes, 8);
        assert!((summary.success_rate - 0.8).abs() < 0.001);
    }

    #[test]
    fn test_auto_recovery_summary_report() {
        let mut recovery = AutoRecovery::new(RecoveryConfig::default());
        recovery.total_recoveries = 5;
        recovery.total_successes = 4;

        let summary = recovery.summary();
        let report = summary.to_report();

        assert!(report.contains("自动恢复报告"));
        assert!(report.contains("总恢复次数: 5"));
        assert!(report.contains("成功次数: 4"));
        assert!(report.contains("退避策略"));
    }

    #[test]
    fn test_auto_recovery_success_rate_empty() {
        let recovery = AutoRecovery::new(RecoveryConfig::default());
        assert!((recovery.success_rate() - 1.0).abs() < 0.001);
    }

    // ===== BackoffStrategy 边界测试 =====

    #[test]
    fn test_backoff_delay_attempt_0() {
        let b = BackoffStrategy::new(2, 60);
        // attempt 0: 2 * 2^0 = 2
        assert_eq!(b.delay_secs(0), 2);
    }

    #[test]
    fn test_backoff_large_attempt() {
        let b = BackoffStrategy::new(2, 60);
        // 非常大的 attempt 应该被 cap
        assert_eq!(b.delay_secs(100), 60);
    }

    #[test]
    fn test_backoff_total_delay_zero() {
        let b = BackoffStrategy::new(2, 60);
        assert_eq!(b.total_delay_secs(0), 0);
    }

    // ===== decide_recovery_action 测试 =====

    #[test]
    fn test_decide_action_immediate_success() {
        let backoff = BackoffStrategy::new(2, 60);
        // attempt 0, 连接正常 — 立即成功
        let action = decide_recovery_action(true, 0, 10, &backoff);
        assert_eq!(action, RecoveryAction::Succeed { attempts: 0 });
    }

    #[test]
    fn test_decide_action_success_after_retries() {
        let backoff = BackoffStrategy::new(2, 60);
        // attempt 3, 连接正常 — 在第 3 次重试时成功
        let action = decide_recovery_action(true, 3, 10, &backoff);
        assert_eq!(action, RecoveryAction::Succeed { attempts: 3 });
    }

    #[test]
    fn test_decide_action_retry_first() {
        let backoff = BackoffStrategy::new(2, 60);
        // attempt 0, 连接异常 — 第一次重试, delay = 2*2^1 = 4
        let action = decide_recovery_action(false, 0, 10, &backoff);
        assert_eq!(
            action,
            RecoveryAction::Retry {
                next_attempt: 1,
                delay_secs: 4
            }
        );
    }

    #[test]
    fn test_decide_action_retry_middle() {
        let backoff = BackoffStrategy::new(2, 60);
        // attempt 5, 连接异常 — 第 6 次重试, delay = 2*2^6 = 128 → capped 60
        let action = decide_recovery_action(false, 5, 10, &backoff);
        assert_eq!(
            action,
            RecoveryAction::Retry {
                next_attempt: 6,
                delay_secs: 60
            }
        );
    }

    #[test]
    fn test_decide_action_give_up_at_max() {
        let backoff = BackoffStrategy::new(2, 60);
        // attempt = max_retries, 连接异常 — 放弃
        let action = decide_recovery_action(false, 10, 10, &backoff);
        assert_eq!(action, RecoveryAction::GiveUp { attempts: 10 });
    }

    #[test]
    fn test_decide_action_give_up_beyond_max() {
        let backoff = BackoffStrategy::new(2, 60);
        // attempt > max_retries, 连接异常 — 放弃
        let action = decide_recovery_action(false, 15, 10, &backoff);
        assert_eq!(action, RecoveryAction::GiveUp { attempts: 10 });
    }

    #[test]
    fn test_decide_action_zero_max_retries_immediate_fail() {
        let backoff = BackoffStrategy::new(2, 60);
        // max_retries=0, attempt=0, 连接异常 — 立即放弃
        let action = decide_recovery_action(false, 0, 0, &backoff);
        assert_eq!(action, RecoveryAction::GiveUp { attempts: 0 });
    }

    #[test]
    fn test_decide_action_zero_max_retries_success() {
        let backoff = BackoffStrategy::new(2, 60);
        // max_retries=0, attempt=0, 连接正常 — 成功
        let action = decide_recovery_action(true, 0, 0, &backoff);
        assert_eq!(action, RecoveryAction::Succeed { attempts: 0 });
    }

    #[test]
    fn test_decide_action_success_takes_priority() {
        let backoff = BackoffStrategy::new(2, 60);
        // 即使 attempt >= max_retries, 连接正常也算成功
        let action = decide_recovery_action(true, 10, 10, &backoff);
        assert_eq!(action, RecoveryAction::Succeed { attempts: 10 });
    }

    // ===== should_continue_retrying 测试 =====

    #[test]
    fn test_should_continue_retrying_has_attempts_left() {
        assert!(should_continue_retrying(0, 10));
        assert!(should_continue_retrying(5, 10));
        assert!(should_continue_retrying(9, 10));
    }

    #[test]
    fn test_should_continue_retrying_at_max() {
        assert!(!should_continue_retrying(10, 10));
    }

    #[test]
    fn test_should_continue_retrying_beyond_max() {
        assert!(!should_continue_retrying(11, 10));
        assert!(!should_continue_retrying(100, 10));
    }

    #[test]
    fn test_should_continue_retrying_zero_max() {
        // max=0: 0 < 0 = false, 无重试机会
        assert!(!should_continue_retrying(0, 0));
    }

    #[test]
    fn test_should_continue_retrying_u32_max() {
        assert!(should_continue_retrying(0, u32::MAX));
        assert!(!should_continue_retrying(u32::MAX, u32::MAX));
    }

    // ===== compute_backoff_schedule 测试 =====

    #[test]
    fn test_compute_backoff_schedule_basic() {
        let backoff = BackoffStrategy::new(2, 60);
        let schedule = compute_backoff_schedule(&backoff, 4);
        assert_eq!(schedule, vec![4, 8, 16, 32]);
    }

    #[test]
    fn test_compute_backoff_schedule_capped() {
        let backoff = BackoffStrategy::new(2, 60);
        let schedule = compute_backoff_schedule(&backoff, 6);
        // 4, 8, 16, 32, 60, 60
        assert_eq!(schedule, vec![4, 8, 16, 32, 60, 60]);
    }

    #[test]
    fn test_compute_backoff_schedule_empty() {
        let backoff = BackoffStrategy::new(2, 60);
        assert!(compute_backoff_schedule(&backoff, 0).is_empty());
    }

    #[test]
    fn test_compute_backoff_schedule_single() {
        let backoff = BackoffStrategy::new(2, 60);
        let schedule = compute_backoff_schedule(&backoff, 1);
        assert_eq!(schedule, vec![4]);
    }

    #[test]
    fn test_compute_backoff_schedule_large() {
        let backoff = BackoffStrategy::new(1, 10);
        let schedule = compute_backoff_schedule(&backoff, 10);
        // All capped at 10 after attempt 4
        // 2, 4, 8, 10, 10, 10, 10, 10, 10, 10
        assert_eq!(schedule, vec![2, 4, 8, 10, 10, 10, 10, 10, 10, 10]);
    }

    #[test]
    fn test_compute_backoff_schedule_matches_total() {
        let backoff = BackoffStrategy::new(2, 60);
        let schedule = compute_backoff_schedule(&backoff, 5);
        let total: u64 = schedule.iter().sum();
        assert_eq!(total, backoff.total_delay_secs(5));
    }

    // ===== estimate_max_recovery_secs 测试 =====

    #[test]
    fn test_estimate_max_recovery_basic() {
        let config = RecoveryConfig::new(9222, 3).with_backoff(BackoffStrategy::new(2, 60));
        // 4 + 8 + 16 = 28
        assert_eq!(estimate_max_recovery_secs(&config), 28);
    }

    #[test]
    fn test_estimate_max_recovery_default() {
        let config = RecoveryConfig::default();
        // default: base=2, max=60, retries=10
        // 4 + 8 + 16 + 32 + 60 + 60 + 60 + 60 + 60 + 60 = 420
        assert_eq!(estimate_max_recovery_secs(&config), 420);
    }

    #[test]
    fn test_estimate_max_recovery_zero_retries() {
        let config = RecoveryConfig::new(9222, 0);
        assert_eq!(estimate_max_recovery_secs(&config), 0);
    }

    #[test]
    fn test_estimate_max_recovery_single_retry() {
        let config = RecoveryConfig::new(9222, 1).with_backoff(BackoffStrategy::new(2, 60));
        assert_eq!(estimate_max_recovery_secs(&config), 4);
    }

    // ===== select_recovery_strategy 测试 =====

    #[test]
    fn test_select_strategy_connected() {
        assert_eq!(
            select_recovery_strategy(&ConnectionStatus::Connected),
            RecoveryStrategy::None
        );
    }

    #[test]
    fn test_select_strategy_tab_closed() {
        assert_eq!(
            select_recovery_strategy(&ConnectionStatus::TabClosed),
            RecoveryStrategy::SimpleRetry
        );
    }

    #[test]
    fn test_select_strategy_check_timeout() {
        assert_eq!(
            select_recovery_strategy(&ConnectionStatus::CheckTimeout),
            RecoveryStrategy::SimpleRetry
        );
    }

    #[test]
    fn test_select_strategy_websocket_error() {
        assert_eq!(
            select_recovery_strategy(&ConnectionStatus::WebSocketError("err".to_string())),
            RecoveryStrategy::WebSocketReconnect
        );
    }

    #[test]
    fn test_select_strategy_chrome_unreachable() {
        assert_eq!(
            select_recovery_strategy(&ConnectionStatus::ChromeUnreachable),
            RecoveryStrategy::ChromeRestart
        );
    }

    #[test]
    fn test_select_strategy_all_statuses() {
        // 确保所有状态都有映射
        let statuses = vec![
            ConnectionStatus::Connected,
            ConnectionStatus::ChromeUnreachable,
            ConnectionStatus::TabClosed,
            ConnectionStatus::WebSocketError("e".to_string()),
            ConnectionStatus::CheckTimeout,
        ];
        for status in &statuses {
            // 所有状态都应返回有效策略 (不 panic)
            let _strategy = select_recovery_strategy(status);
        }
        // Connected → None, 其他 → 非 None
        assert_eq!(
            select_recovery_strategy(&ConnectionStatus::Connected),
            RecoveryStrategy::None
        );
        assert_ne!(
            select_recovery_strategy(&ConnectionStatus::ChromeUnreachable),
            RecoveryStrategy::None
        );
        assert_ne!(
            select_recovery_strategy(&ConnectionStatus::TabClosed),
            RecoveryStrategy::None
        );
        assert_ne!(
            select_recovery_strategy(&ConnectionStatus::WebSocketError("e".to_string())),
            RecoveryStrategy::None
        );
        assert_ne!(
            select_recovery_strategy(&ConnectionStatus::CheckTimeout),
            RecoveryStrategy::None
        );
    }

    // ===== RecoveryStrategy 方法测试 =====

    #[test]
    fn test_recovery_strategy_description() {
        assert_eq!(RecoveryStrategy::None.description(), "无需恢复");
        assert_eq!(RecoveryStrategy::SimpleRetry.description(), "简单重试");
        assert_eq!(
            RecoveryStrategy::WebSocketReconnect.description(),
            "WebSocket 重连"
        );
        assert_eq!(
            RecoveryStrategy::ChromeRestart.description(),
            "Chrome 重启恢复"
        );
    }

    #[test]
    fn test_recovery_strategy_difficulty() {
        assert_eq!(RecoveryStrategy::None.difficulty(), 0);
        assert_eq!(RecoveryStrategy::SimpleRetry.difficulty(), 1);
        assert_eq!(RecoveryStrategy::WebSocketReconnect.difficulty(), 2);
        assert_eq!(RecoveryStrategy::ChromeRestart.difficulty(), 3);
    }

    #[test]
    fn test_recovery_strategy_display() {
        assert_eq!(RecoveryStrategy::None.to_string(), "无需恢复");
        assert_eq!(RecoveryStrategy::SimpleRetry.to_string(), "简单重试");
    }

    #[test]
    fn test_recovery_strategy_serde() {
        let strategy = RecoveryStrategy::ChromeRestart;
        let json = serde_json::to_string(&strategy).unwrap();
        let parsed: RecoveryStrategy = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, strategy);
    }

    // ===== assess_recovery_urgency 测试 =====

    #[test]
    fn test_assess_urgency_healthy() {
        assert_eq!(
            assess_recovery_urgency(&HealthLevel::Healthy),
            RecoveryUrgency::None
        );
    }

    #[test]
    fn test_assess_urgency_degraded() {
        assert_eq!(
            assess_recovery_urgency(&HealthLevel::Degraded),
            RecoveryUrgency::Low
        );
    }

    #[test]
    fn test_assess_urgency_critical() {
        assert_eq!(
            assess_recovery_urgency(&HealthLevel::Critical),
            RecoveryUrgency::Critical
        );
    }

    #[test]
    fn test_assess_urgency_all_levels() {
        let levels = vec![
            HealthLevel::Healthy,
            HealthLevel::Degraded,
            HealthLevel::Critical,
        ];
        for level in &levels {
            let urgency = assess_recovery_urgency(level);
            // 确保所有级别都有映射
            assert!(matches!(
                urgency,
                RecoveryUrgency::None | RecoveryUrgency::Low | RecoveryUrgency::Critical
            ));
        }
    }

    // ===== RecoveryUrgency 方法测试 =====

    #[test]
    fn test_recovery_urgency_description() {
        assert_eq!(RecoveryUrgency::None.description(), "无需恢复");
        assert_eq!(RecoveryUrgency::Low.description(), "低紧急度");
        assert_eq!(RecoveryUrgency::High.description(), "高紧急度");
        assert_eq!(RecoveryUrgency::Critical.description(), "严重紧急度");
    }

    #[test]
    fn test_recovery_urgency_requires_immediate() {
        assert!(!RecoveryUrgency::None.requires_immediate_recovery());
        assert!(!RecoveryUrgency::Low.requires_immediate_recovery());
        assert!(!RecoveryUrgency::High.requires_immediate_recovery());
        assert!(RecoveryUrgency::Critical.requires_immediate_recovery());
    }

    #[test]
    fn test_recovery_urgency_display() {
        assert_eq!(RecoveryUrgency::None.to_string(), "无需恢复");
        assert_eq!(RecoveryUrgency::Low.to_string(), "低紧急度");
    }

    #[test]
    fn test_recovery_urgency_serde() {
        let urgency = RecoveryUrgency::Critical;
        let json = serde_json::to_string(&urgency).unwrap();
        let parsed: RecoveryUrgency = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, urgency);
    }

    // ===== compute_recovery_success_rate 测试 =====

    #[test]
    fn test_compute_success_rate_zero_recoveries() {
        assert!((compute_recovery_success_rate(0, 0) - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_compute_success_rate_all_success() {
        assert!((compute_recovery_success_rate(10, 10) - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_compute_success_rate_half() {
        assert!((compute_recovery_success_rate(10, 5) - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_compute_success_rate_all_failed() {
        assert!((compute_recovery_success_rate(10, 0) - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_compute_success_rate_single_success() {
        assert!((compute_recovery_success_rate(1, 1) - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_compute_success_rate_single_failure() {
        assert!((compute_recovery_success_rate(1, 0) - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_compute_success_rate_successes_exceed_recoveries() {
        // successes > recoveries — 应 clamp
        let rate = compute_recovery_success_rate(5, 10);
        assert!((rate - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_compute_success_rate_large_numbers() {
        let rate = compute_recovery_success_rate(u64::MAX, u64::MAX / 2);
        assert!((rate - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_compute_success_rate_consistency_with_method() {
        // 验证 AutoRecovery::success_rate() 委托纯函数
        let mut recovery = AutoRecovery::new(RecoveryConfig::default());
        recovery.total_recoveries = 20;
        recovery.total_successes = 15;

        let expected = compute_recovery_success_rate(20, 15);
        assert!((recovery.success_rate() - expected).abs() < 0.001);
    }

    // ===== format_recovery_rate 测试 =====

    #[test]
    fn test_format_recovery_rate_full() {
        assert_eq!(format_recovery_rate(1.0), "100.0%");
    }

    #[test]
    fn test_format_recovery_rate_zero() {
        assert_eq!(format_recovery_rate(0.0), "0.0%");
    }

    #[test]
    fn test_format_recovery_rate_half() {
        assert_eq!(format_recovery_rate(0.5), "50.0%");
    }

    #[test]
    fn test_format_recovery_rate_85() {
        assert_eq!(format_recovery_rate(0.85), "85.0%");
    }

    #[test]
    fn test_format_recovery_rate_third() {
        assert_eq!(format_recovery_rate(1.0 / 3.0), "33.3%");
    }

    #[test]
    fn test_format_recovery_rate_negative_clamped() {
        assert_eq!(format_recovery_rate(-0.1), "0.0%");
    }

    #[test]
    fn test_format_recovery_rate_above_one_clamped() {
        assert_eq!(format_recovery_rate(1.5), "100.0%");
    }

    // ===== make_success_result 测试 =====

    #[test]
    fn test_make_success_result_basic() {
        let result = make_success_result(3, 15000);
        assert!(result.is_success());
        assert!(!result.is_failed());
        assert_eq!(result.attempts(), 3);
        assert_eq!(result.total_duration_ms(), 15000);
    }

    #[test]
    fn test_make_success_result_immediate() {
        let result = make_success_result(0, 100);
        assert!(result.is_success());
        assert_eq!(result.attempts(), 0);
        assert_eq!(result.total_duration_ms(), 100);
    }

    #[test]
    fn test_make_success_result_zero_duration() {
        let result = make_success_result(5, 0);
        assert!(result.is_success());
        assert_eq!(result.total_duration_ms(), 0);
    }

    #[test]
    fn test_make_success_result_large_values() {
        let result = make_success_result(u32::MAX, u64::MAX);
        assert!(result.is_success());
        assert_eq!(result.attempts(), u32::MAX);
        assert_eq!(result.total_duration_ms(), u64::MAX);
    }

    // ===== make_failed_result 测试 =====

    #[test]
    fn test_make_failed_result_basic() {
        let result = make_failed_result(
            10,
            60000,
            ConnectionStatus::ChromeUnreachable,
            "超过最大重试次数",
        );
        assert!(result.is_failed());
        assert!(!result.is_success());
        assert_eq!(result.attempts(), 10);
        assert_eq!(result.total_duration_ms(), 60000);
    }

    #[test]
    fn test_make_failed_result_zero_retries() {
        let result = make_failed_result(0, 0, ConnectionStatus::TabClosed, "无重试机会");
        assert!(result.is_failed());
        assert_eq!(result.attempts(), 0);
        assert_eq!(result.total_duration_ms(), 0);
    }

    #[test]
    fn test_make_failed_result_empty_error() {
        let result = make_failed_result(5, 1000, ConnectionStatus::CheckTimeout, "");
        assert!(result.is_failed());
        // 验证空错误信息被正确处理
        assert_eq!(result_error(&result), "");
    }

    #[test]
    fn test_make_failed_result_all_statuses() {
        let statuses = vec![
            ConnectionStatus::ChromeUnreachable,
            ConnectionStatus::TabClosed,
            ConnectionStatus::WebSocketError("e".to_string()),
            ConnectionStatus::CheckTimeout,
        ];
        for status in &statuses {
            let result = make_failed_result(1, 100, status.clone(), "test");
            assert!(result.is_failed());
        }
    }

    // ===== result_error 测试 =====

    #[test]
    fn test_result_error_failed() {
        let result = make_failed_result(3, 5000, ConnectionStatus::TabClosed, "测试错误");
        assert_eq!(result_error(&result), "测试错误");
    }

    #[test]
    fn test_result_error_success() {
        let result = make_success_result(0, 100);
        assert_eq!(result_error(&result), "");
    }

    // ===== recovery_efficiency 测试 =====

    #[test]
    fn test_recovery_efficiency_immediate_success() {
        assert!((recovery_efficiency(0, 10) - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_recovery_efficiency_halfway() {
        assert!((recovery_efficiency(5, 10) - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_recovery_efficiency_at_max() {
        assert!((recovery_efficiency(10, 10) - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_recovery_efficiency_zero_max_retries() {
        assert!((recovery_efficiency(0, 0) - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_recovery_efficiency_beyond_max_clamped() {
        // attempts > max_retries — 应 clamp 到 0
        assert!((recovery_efficiency(15, 10) - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_recovery_efficiency_large_max() {
        let rate = recovery_efficiency(1, u32::MAX);
        // 1 / u32::MAX ≈ 0, efficiency ≈ 1.0
        assert!((rate - 1.0).abs() < 0.001);
    }

    // ===== AutoRecovery 新方法测试 (委托纯函数) =====

    #[test]
    fn test_auto_recovery_recovery_strategy() {
        let recovery = AutoRecovery::new(RecoveryConfig::default());

        assert_eq!(
            recovery.recovery_strategy(&ConnectionStatus::Connected),
            RecoveryStrategy::None
        );
        assert_eq!(
            recovery.recovery_strategy(&ConnectionStatus::TabClosed),
            RecoveryStrategy::SimpleRetry
        );
        assert_eq!(
            recovery.recovery_strategy(&ConnectionStatus::ChromeUnreachable),
            RecoveryStrategy::ChromeRestart
        );
    }

    #[test]
    fn test_auto_recovery_recovery_urgency() {
        let recovery = AutoRecovery::new(RecoveryConfig::default());

        assert_eq!(
            recovery.recovery_urgency(&HealthLevel::Healthy),
            RecoveryUrgency::None
        );
        assert_eq!(
            recovery.recovery_urgency(&HealthLevel::Degraded),
            RecoveryUrgency::Low
        );
        assert_eq!(
            recovery.recovery_urgency(&HealthLevel::Critical),
            RecoveryUrgency::Critical
        );
    }

    #[test]
    fn test_auto_recovery_estimated_max_recovery_secs() {
        let config = RecoveryConfig::new(9222, 3).with_backoff(BackoffStrategy::new(2, 60));
        let recovery = AutoRecovery::new(config);

        // 4 + 8 + 16 = 28
        assert_eq!(recovery.estimated_max_recovery_secs(), 28);
    }

    #[test]
    fn test_auto_recovery_estimated_max_recovery_default() {
        let recovery = AutoRecovery::new(RecoveryConfig::default());
        // default: 4 + 8 + 16 + 32 + 60 + 60 + 60 + 60 + 60 + 60 = 420
        assert_eq!(recovery.estimated_max_recovery_secs(), 420);
    }

    // ===== summary 一致性 (使用纯函数) =====

    #[test]
    fn test_summary_uses_pure_function() {
        let mut recovery = AutoRecovery::new(RecoveryConfig::default());
        recovery.total_recoveries = 30;
        recovery.total_successes = 25;

        let summary = recovery.summary();
        let expected = compute_recovery_success_rate(30, 25);
        assert!((summary.success_rate - expected).abs() < 0.001);
    }

    // ===== to_report 一致性 (使用纯函数) =====

    #[test]
    fn test_report_uses_format_recovery_rate() {
        let mut recovery = AutoRecovery::new(RecoveryConfig::default());
        recovery.total_recoveries = 100;
        recovery.total_successes = 95;

        let summary = recovery.summary();
        let report = summary.to_report();
        assert!(report.contains(&format_recovery_rate(summary.success_rate)));
    }

    #[test]
    fn test_report_contains_strategy_info() {
        let recovery = AutoRecovery::new(RecoveryConfig::default());
        let summary = recovery.summary();
        let report = summary.to_report();

        assert!(report.contains("退避策略"));
        assert!(report.contains("base=2s"));
        assert!(report.contains("max=60s"));
    }

    // ===== 协同测试 (与 connection_monitor 纯函数) =====

    #[test]
    fn test_synergy_strategy_matches_severity() {
        // select_recovery_strategy 和 classify_connection_severity 应协同工作
        use crate::connection_monitor::classify_connection_severity;

        // Connected → None strategy + Info severity
        let status = ConnectionStatus::Connected;
        assert_eq!(select_recovery_strategy(&status), RecoveryStrategy::None);
        assert_eq!(classify_connection_severity(&status).description(), "信息");

        // ChromeUnreachable → ChromeRestart strategy + Critical severity
        let status = ConnectionStatus::ChromeUnreachable;
        assert_eq!(
            select_recovery_strategy(&status),
            RecoveryStrategy::ChromeRestart
        );
        assert_eq!(classify_connection_severity(&status).description(), "严重");
    }

    #[test]
    fn test_synergy_urgency_matches_health() {
        // assess_recovery_urgency 和 determine_health_level 应协同工作
        use crate::connection_monitor::determine_health_level;

        let status = ConnectionStatus::ChromeUnreachable;
        let health = determine_health_level(&status, 3, 3);
        let urgency = assess_recovery_urgency(&health);

        // ChromeUnreachable + max failures → Critical → Critical urgency
        assert_eq!(urgency, RecoveryUrgency::Critical);
        assert!(urgency.requires_immediate_recovery());
    }

    #[test]
    fn test_synergy_action_uses_monitor_status() {
        // decide_recovery_action 可以直接使用 ConnectionMonitor 的状态
        let backoff = BackoffStrategy::default();

        // 模拟 ConnectionMonitor 检测到 TabClosed
        let status = ConnectionStatus::TabClosed;
        let action = decide_recovery_action(status.is_connected(), 0, 10, &backoff);
        assert!(matches!(action, RecoveryAction::Retry { .. }));

        // 模拟 ConnectionMonitor 检测到 Connected
        let status = ConnectionStatus::Connected;
        let action = decide_recovery_action(status.is_connected(), 3, 10, &backoff);
        assert_eq!(action, RecoveryAction::Succeed { attempts: 3 });
    }

    #[test]
    fn test_synergy_full_recovery_pipeline() {
        // 模拟完整的恢复决策管道:
        // ConnectionStatus → HealthLevel → RecoveryUrgency → RecoveryStrategy → RecoveryAction
        use crate::connection_monitor::{classify_connection_severity, determine_health_level};

        let status = ConnectionStatus::ChromeUnreachable;
        let failures = 3;
        let max_failures = 3;

        // 1. 判定健康等级
        let health = determine_health_level(&status, failures, max_failures);
        assert_eq!(health, HealthLevel::Critical);

        // 2. 评估紧急度
        let urgency = assess_recovery_urgency(&health);
        assert_eq!(urgency, RecoveryUrgency::Critical);
        assert!(urgency.requires_immediate_recovery());

        // 3. 选择恢复策略
        let strategy = select_recovery_strategy(&status);
        assert_eq!(strategy, RecoveryStrategy::ChromeRestart);
        assert_eq!(strategy.difficulty(), 3);

        // 4. 分类严重程度
        let severity = classify_connection_severity(&status);
        assert_eq!(severity.description(), "严重");

        // 5. 决定恢复动作 (第一次检查, 还未恢复)
        let backoff = BackoffStrategy::default();
        let action = decide_recovery_action(false, 0, 10, &backoff);
        assert!(matches!(action, RecoveryAction::Retry { .. }));
    }

    // ===== RecoveryAction 边界测试 =====

    #[test]
    fn test_recovery_action_eq() {
        let backoff = BackoffStrategy::new(2, 60);

        // 相同条件 → 相同动作
        assert_eq!(
            decide_recovery_action(true, 0, 10, &backoff),
            decide_recovery_action(true, 0, 10, &backoff)
        );

        // 不同条件 → 不同动作
        assert_ne!(
            decide_recovery_action(true, 0, 10, &backoff),
            decide_recovery_action(false, 0, 10, &backoff)
        );
    }

    #[test]
    fn test_recovery_action_retry_delay_progression() {
        let backoff = BackoffStrategy::new(2, 60);

        // 验证退避时间递增
        let a1 = decide_recovery_action(false, 0, 10, &backoff);
        let a2 = decide_recovery_action(false, 1, 10, &backoff);
        let a3 = decide_recovery_action(false, 2, 10, &backoff);

        if let RecoveryAction::Retry { delay_secs: d1, .. } = a1 {
            if let RecoveryAction::Retry { delay_secs: d2, .. } = a2 {
                if let RecoveryAction::Retry { delay_secs: d3, .. } = a3 {
                    assert!(d1 < d2);
                    assert!(d2 < d3);
                }
            }
        }
    }

    #[test]
    fn test_recovery_action_give_up_attempts_always_max() {
        let backoff = BackoffStrategy::new(2, 60);

        // GiveUp 的 attempts 始终是 max_retries, 不论 attempt
        let a1 = decide_recovery_action(false, 10, 10, &backoff);
        let a2 = decide_recovery_action(false, 20, 10, &backoff);

        assert_eq!(a1, RecoveryAction::GiveUp { attempts: 10 });
        assert_eq!(a2, RecoveryAction::GiveUp { attempts: 10 });
    }
}
