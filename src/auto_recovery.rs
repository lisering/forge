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

use crate::connection_monitor::{ConnectionMonitor, ConnectionStatus, RecoveryEvent};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

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

    /// 成功率
    pub fn success_rate(&self) -> f64 {
        if self.total_recoveries == 0 {
            return 1.0;
        }
        self.total_successes as f64 / self.total_recoveries as f64
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

            if status.is_connected() {
                let duration = start.elapsed().as_millis() as u64;
                info!("✅ 恢复成功 (第 {} 次重试, 耗时 {}ms)", attempt, duration);

                // 记录恢复事件
                monitor.record_recovery_event(RecoveryEvent::new(
                    0,
                    monitor.last_status().clone(),
                    ConnectionStatus::Connected,
                    &format!("自动恢复第 {} 次重试成功", attempt),
                    duration,
                    true,
                    None,
                ));

                monitor.reset();
                self.total_successes += 1;

                let result = RecoveryResult::Success {
                    attempts: attempt,
                    total_duration_ms: duration,
                };
                self.recovery_history.push(result.clone());
                return result;
            }

            // 未恢复, 记录日志
            if attempt < max_retries {
                let delay = self.config.backoff.delay_secs(attempt + 1);
                warn!(
                    "⚠️ 第 {} 次检查未恢复 ({}), 等待 {}s 后重试...",
                    attempt + 1,
                    status.description(),
                    delay
                );
                tokio::time::sleep(Duration::from_secs(delay)).await;
            } else {
                error!("❌ 自动恢复失败: 超过最大重试次数 {}", max_retries);
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

        let result = RecoveryResult::Failed {
            attempts: max_retries,
            total_duration_ms: duration,
            last_status,
            error: format!("超过最大重试次数 {}", max_retries),
        };
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

            if status.is_connected() {
                let duration = start.elapsed().as_millis() as u64;
                monitor.record_recovery_event(RecoveryEvent::new(
                    0,
                    monitor.last_status().clone(),
                    ConnectionStatus::Connected,
                    &format!("恢复第 {} 次", attempt),
                    duration,
                    true,
                    None,
                ));
                monitor.reset();
                self.total_successes += 1;

                let result = RecoveryResult::Success {
                    attempts: attempt,
                    total_duration_ms: duration,
                };
                self.recovery_history.push(result.clone());
                return result;
            }

            // 不等待, 立即重试
            if attempt < max_retries {
                debug!(
                    "第 {} 次检查未恢复 ({}), 立即重试",
                    attempt + 1,
                    status.description()
                );
            }
        }

        let duration = start.elapsed().as_millis() as u64;
        let last_status = monitor.last_status().clone();
        let result = RecoveryResult::Failed {
            attempts: max_retries,
            total_duration_ms: duration,
            last_status: last_status.clone(),
            error: format!("超过最大重试次数 {}", max_retries),
        };
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

/// 获取 RecoveryResult::Failed 的错误信息 (辅助函数)
fn result_error(result: &RecoveryResult) -> String {
    match result {
        RecoveryResult::Failed { error, .. } => error.clone(),
        _ => String::new(),
    }
}

use tracing::debug;

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
    /// 生成可读报告
    pub fn to_report(&self) -> String {
        let mut report = String::new();
        report.push_str("═══════════════════════════════════════════════════\n");
        report.push_str("  🔧 自动恢复报告\n");
        report.push_str("═══════════════════════════════════════════════════\n\n");

        report.push_str(&format!("  总恢复次数: {}\n", self.total_recoveries));
        report.push_str(&format!("  成功次数: {}\n", self.total_successes));
        report.push_str(&format!("  成功率: {:.1}%\n", self.success_rate * 100.0));
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
}
