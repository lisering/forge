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
        self.consecutive_failures >= self.config.max_consecutive_failures
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
            success_rate: if self.total_checks == 0 {
                1.0
            } else {
                1.0 - (self.total_failures as f64 / self.total_checks as f64)
            },
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
            "  运行时长: {:.1}m ({:.1}h)\n",
            self.uptime_secs as f64 / 60.0,
            self.uptime_secs as f64 / 3600.0
        ));
        report.push_str(&format!("  总检查次数: {}\n", self.total_checks));
        report.push_str(&format!("  总失败次数: {}\n", self.total_failures));
        report.push_str(&format!("  成功率: {:.1}%\n", self.success_rate * 100.0));
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
                let status = if event.success { "✅" } else { "❌" };
                report.push_str(&format!(
                    "  {} [{:.0}s] {} → {} | {} ({}ms)\n",
                    status,
                    event.timestamp_ms as f64 / 1000.0,
                    event.before_status.description(),
                    event.after_status.description(),
                    event.strategy,
                    event.duration_ms,
                ));
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
}
