//! 自动恢复集成测试 (第 17 项任务)
//!
//! 验证 Chrome 自动恢复机制与现有功能的共存:
//! - ConnectionMonitor 连接状态检测
//! - AutoRecovery 指数退避重试
//! - DevTrace Recovery 事件记录
//! - Orchestrator 集成 (禁用时向后兼容)
//! - 24h 可靠性场景模拟
//!
//! 注意: 测试环境中无 Chrome 运行, 因此主要测试:
//! 1. 模块在无 Chrome 环境下的行为 (检测到不可达)
//! 2. 禁用自动恢复时的向后兼容性
//! 3. DevTrace Recovery 事件记录
//! 4. 恢复摘要报告生成

use async_trait::async_trait;
use forge::auto_recovery::{AutoRecovery, BackoffStrategy, RecoveryConfig};
use forge::connection_monitor::{
    ConnectionMonitor, ConnectionStatus, MonitorConfig, RecoveryEvent,
};
use forge::dev_trace::{DevTraceWriter, TraceAction};
use forge::extract::ExtractedFile;
use forge::orchestrator::Orchestrator;
use forge::testrunner::TestResult;
use forge::traits::{ChatClient, ChatResult, FileExtractor, TestRunner};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tempfile::tempdir;

// ============================================================================
//  Mock 实现 (复用已有模式)
// ============================================================================

struct MockChat {
    responses: Arc<Mutex<Vec<String>>>,
    turn_count: Arc<AtomicUsize>,
}

impl MockChat {
    fn new(responses: Vec<String>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses)),
            turn_count: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[async_trait]
impl ChatClient for MockChat {
    async fn send_message(&self, _msg: &str, _timeout: u64) -> anyhow::Result<ChatResult> {
        self.turn_count.fetch_add(1, Ordering::SeqCst);
        let mut queue = self.responses.lock().unwrap();
        if queue.is_empty() {
            return Ok(ChatResult {
                text: "(empty)".to_string(),
                timed_out: false,
            });
        }
        let text = queue.remove(0);
        Ok(ChatResult {
            text,
            timed_out: false,
        })
    }

    fn conversation_turn_count(&self) -> usize {
        self.turn_count.load(Ordering::SeqCst)
    }
}

struct MockTestRunner;

impl TestRunner for MockTestRunner {
    fn check(&self, _dir: &Path) -> anyhow::Result<TestResult> {
        Ok(TestResult {
            success: true,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
            errors: vec![],
            test_summary: None,
        })
    }

    fn test(&self, _dir: &Path) -> anyhow::Result<TestResult> {
        Ok(TestResult {
            success: true,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
            errors: vec![],
            test_summary: None,
        })
    }
}

struct MockExtractor;

impl FileExtractor for MockExtractor {
    fn extract(&self, _text: &str) -> Vec<ExtractedFile> {
        vec![ExtractedFile {
            path: "src/main.rs".to_string(),
            content: "fn main() {}".to_string(),
            language: String::new(),
        }]
    }
}

// ============================================================================
//  辅助函数
// ============================================================================

fn simple_plan() -> String {
    r#"```json
[{"name":"阶段1","description":"d","tasks":[{"name":"任务1","prompt":"do it"}]}
]
```"#
        .to_string()
}

fn success_code() -> String {
    "以下是完整实现。\n```file:src/main.rs\nfn main() { println!(\"hello\"); }\n```".to_string()
}

// ============================================================================
//  ConnectionStatus 测试
// ============================================================================

/// 测试 1: ConnectionStatus 枚举行为
#[test]
fn test_connection_status_variants() {
    assert!(ConnectionStatus::Connected.is_connected());
    assert!(!ConnectionStatus::ChromeUnreachable.is_connected());
    assert!(!ConnectionStatus::TabClosed.is_connected());
    assert!(!ConnectionStatus::WebSocketError("err".into()).is_connected());
    assert!(!ConnectionStatus::CheckTimeout.is_connected());

    assert!(!ConnectionStatus::Connected.needs_recovery());
    assert!(ConnectionStatus::ChromeUnreachable.needs_recovery());
    assert!(ConnectionStatus::TabClosed.needs_recovery());
    assert!(ConnectionStatus::WebSocketError("err".into()).needs_recovery());
    assert!(ConnectionStatus::CheckTimeout.needs_recovery());
}

/// 测试 2: ConnectionStatus 恢复难度排序
#[test]
fn test_recovery_difficulty_ordering() {
    assert!(ConnectionStatus::Connected.recovery_difficulty() == 0);
    assert!(ConnectionStatus::TabClosed.recovery_difficulty() == 1);
    assert!(ConnectionStatus::WebSocketError("e".into()).recovery_difficulty() == 2);
    assert!(ConnectionStatus::CheckTimeout.recovery_difficulty() == 2);
    assert!(ConnectionStatus::ChromeUnreachable.recovery_difficulty() == 3);
}

/// 测试 3: ConnectionStatus 序列化/反序列化
#[test]
fn test_connection_status_serde() {
    let statuses = vec![
        ConnectionStatus::Connected,
        ConnectionStatus::ChromeUnreachable,
        ConnectionStatus::TabClosed,
        ConnectionStatus::CheckTimeout,
        ConnectionStatus::WebSocketError("connection refused".to_string()),
    ];

    for status in &statuses {
        let json = serde_json::to_string(status).unwrap();
        let parsed: ConnectionStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(&parsed, status);
    }
}

// ============================================================================
//  ConnectionMonitor 测试
// ============================================================================

/// 测试 4: ConnectionMonitor 初始状态
#[test]
fn test_monitor_initial_state() {
    let monitor = ConnectionMonitor::new(9222);
    assert!(monitor.last_status().is_connected());
    assert_eq!(monitor.consecutive_failures(), 0);
    assert_eq!(monitor.total_checks(), 0);
    assert_eq!(monitor.total_failures(), 0);
    assert!(!monitor.is_chrome_crashed());
}

/// 测试 5: ConnectionMonitor 自定义配置
#[test]
fn test_monitor_custom_config() {
    let config = MonitorConfig {
        port: 9333,
        check_timeout_secs: 5,
        heartbeat_interval_secs: 15,
        max_consecutive_failures: 7,
    };
    let monitor = ConnectionMonitor::with_config(config);
    assert_eq!(monitor.config().port, 9333);
    assert_eq!(monitor.config().check_timeout_secs, 5);
    assert_eq!(monitor.config().max_consecutive_failures, 7);
}

/// 测试 6: ConnectionMonitor 无 Chrome 时检测到不可达
#[tokio::test]
async fn test_monitor_detects_no_chrome() {
    let mut monitor = ConnectionMonitor::new(19999);
    let status = monitor.check_connection().await;

    assert!(status.needs_recovery());
    assert_eq!(monitor.total_checks(), 1);
    assert_eq!(monitor.total_failures(), 1);
    assert_eq!(monitor.consecutive_failures(), 1);
}

/// 测试 7: ConnectionMonitor 连续失败计数
#[tokio::test]
async fn test_monitor_consecutive_failures() {
    let mut monitor = ConnectionMonitor::new(19999);

    for i in 1..=5 {
        let _ = monitor.check_connection().await;
        assert_eq!(monitor.consecutive_failures(), i);
        assert_eq!(monitor.total_failures(), i as u64);
    }

    assert_eq!(monitor.total_checks(), 5);
    assert!(monitor.is_chrome_crashed()); // 默认阈值 3, 已超过
}

/// 测试 8: ConnectionMonitor 恢复事件记录
#[test]
fn test_monitor_recovery_events() {
    let mut monitor = ConnectionMonitor::new(9222);

    // 记录成功恢复
    monitor.record_recovery_event(RecoveryEvent::new(
        0,
        ConnectionStatus::ChromeUnreachable,
        ConnectionStatus::Connected,
        "重试第3次成功",
        15000,
        true,
        None,
    ));

    // 记录失败恢复
    monitor.record_recovery_event(RecoveryEvent::new(
        0,
        ConnectionStatus::ChromeUnreachable,
        ConnectionStatus::ChromeUnreachable,
        "重试10次后放弃",
        60000,
        false,
        Some("超过最大重试次数"),
    ));

    assert_eq!(monitor.recovery_events().len(), 2);
    assert!(monitor.recovery_events()[0].success);
    assert!(!monitor.recovery_events()[1].success);
}

/// 测试 9: ConnectionMonitor 重置
#[test]
fn test_monitor_reset() {
    let mut monitor = ConnectionMonitor::new(9222);
    monitor.consecutive_failures = 10;
    monitor.last_status = ConnectionStatus::ChromeUnreachable;

    monitor.reset();

    assert_eq!(monitor.consecutive_failures(), 0);
    assert!(monitor.last_status().is_connected());
}

/// 测试 10: ConnectionMonitor 摘要报告
#[test]
fn test_monitor_summary_report() {
    let mut monitor = ConnectionMonitor::new(9222);
    monitor.total_checks = 1000;
    monitor.total_failures = 15;
    monitor.consecutive_failures = 2;

    let summary = monitor.summary();
    let report = summary.to_report();

    assert!(report.contains("连接监控报告"));
    assert!(report.contains("总检查次数: 1000"));
    assert!(report.contains("总失败次数: 15"));
    assert!((summary.success_rate - 0.985).abs() < 0.001);
}

// ============================================================================
//  BackoffStrategy 测试
// ============================================================================

/// 测试 11: 指数退避策略计算
#[test]
fn test_backoff_strategy_calculation() {
    let b = BackoffStrategy::new(2, 60);

    // 2 * 2^1 = 4
    assert_eq!(b.delay_secs(1), 4);
    // 2 * 2^2 = 8
    assert_eq!(b.delay_secs(2), 8);
    // 2 * 2^3 = 16
    assert_eq!(b.delay_secs(3), 16);
    // 2 * 2^4 = 32
    assert_eq!(b.delay_secs(4), 32);
    // 2 * 2^5 = 64 → capped at 60
    assert_eq!(b.delay_secs(5), 60);
}

/// 测试 12: 退避总时间计算
#[test]
fn test_backoff_total_time() {
    let b = BackoffStrategy::new(2, 60);
    // 4 + 8 + 16 + 32 = 60
    assert_eq!(b.total_delay_secs(4), 60);
    // 4 + 8 + 16 + 32 + 60 + 60 = 180
    assert_eq!(b.total_delay_secs(6), 180);
}

/// 测试 13: 自定义退避策略
#[test]
fn test_backoff_custom_strategy() {
    let b = BackoffStrategy::new(1, 10);
    assert_eq!(b.delay_secs(1), 2);
    assert_eq!(b.delay_secs(2), 4);
    assert_eq!(b.delay_secs(3), 8);
    assert_eq!(b.delay_secs(4), 10); // capped
}

// ============================================================================
//  AutoRecovery 测试
// ============================================================================

/// 测试 14: AutoRecovery 无 Chrome 时恢复失败
#[tokio::test]
async fn test_recovery_fails_without_chrome() {
    let config = RecoveryConfig::new(19999, 3);
    let mut recovery = AutoRecovery::new(config);
    let mut monitor = ConnectionMonitor::new(19999);

    let result = recovery.recover_no_wait(&mut monitor).await;

    assert!(result.is_failed());
    assert_eq!(result.attempts(), 3);
    assert_eq!(recovery.total_recoveries(), 1);
    assert_eq!(recovery.total_successes(), 0);
}

/// 测试 15: AutoRecovery 恢复历史记录
#[tokio::test]
async fn test_recovery_history() {
    let config = RecoveryConfig::new(19999, 2);
    let mut recovery = AutoRecovery::new(config);
    let mut monitor = ConnectionMonitor::new(19999);

    // 执行多次恢复
    for _ in 0..3 {
        let _ = recovery.recover_no_wait(&mut monitor).await;
    }

    assert_eq!(recovery.total_recoveries(), 3);
    assert_eq!(recovery.recovery_history().len(), 3);
    assert_eq!(recovery.total_successes(), 0); // 无 Chrome, 全部失败
}

/// 测试 16: AutoRecovery 恢复事件记录到 Monitor
#[tokio::test]
async fn test_recovery_records_to_monitor() {
    let config = RecoveryConfig::new(19999, 2);
    let mut recovery = AutoRecovery::new(config);
    let mut monitor = ConnectionMonitor::new(19999);

    let _ = recovery.recover_no_wait(&mut monitor).await;

    // Monitor 应有恢复事件
    let events = monitor.recovery_events();
    assert!(!events.is_empty());

    // 最后一个事件是失败的
    let last = events.last().unwrap();
    assert!(!last.success);
    assert!(last.duration_ms > 0);
}

/// 测试 17: AutoRecovery 摘要报告
#[test]
fn test_recovery_summary_report() {
    let mut recovery = AutoRecovery::new(RecoveryConfig::default());
    recovery.total_recoveries = 20;
    recovery.total_successes = 18;

    let summary = recovery.summary();
    let report = summary.to_report();

    assert!(report.contains("自动恢复报告"));
    assert!(report.contains("总恢复次数: 20"));
    assert!(report.contains("成功次数: 18"));
    assert!((summary.success_rate - 0.9).abs() < 0.001);
}

/// 测试 18: AutoRecovery 成功率计算
#[test]
fn test_recovery_success_rate() {
    let mut recovery = AutoRecovery::new(RecoveryConfig::default());

    // 空状态: 成功率 1.0
    assert!((recovery.success_rate() - 1.0).abs() < 0.001);

    recovery.total_recoveries = 10;
    recovery.total_successes = 7;
    assert!((recovery.success_rate() - 0.7).abs() < 0.001);
}

// ============================================================================
//  DevTrace Recovery 事件测试
// ============================================================================

/// 测试 19: DevTrace 可以记录 Recovery 事件
#[test]
fn test_devtrace_recovery_action() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
    let writer = DevTraceWriter::new(dir.path());

    writer
        .trace(
            TraceAction::Recovery,
            None,
            None,
            None,
            "连接检查",
            "Chrome 不可达",
            5000,
            false,
            Some("Chrome 调试端口不可达"),
        )
        .unwrap();

    let entries = writer.read_all().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].action, TraceAction::Recovery);
    assert!(!entries[0].success);
    assert!(entries[0].error.is_some());
}

/// 测试 20: DevTrace 多种恢复事件记录
#[test]
fn test_devtrace_multiple_recovery_events() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
    let writer = DevTraceWriter::new(dir.path());

    // 模拟一次完整的恢复周期
    // 1. 检测到断连
    writer
        .trace(
            TraceAction::Recovery,
            None,
            None,
            None,
            "连接检查",
            "ChromeUnreachable",
            100,
            false,
            Some("检测到 Chrome 不可达"),
        )
        .unwrap();

    // 2. 重试第1次
    writer
        .trace(
            TraceAction::Recovery,
            None,
            None,
            None,
            "自动恢复",
            "重试1次未成功",
            2000,
            false,
            Some("仍不可达"),
        )
        .unwrap();

    // 3. 重试第2次
    writer
        .trace(
            TraceAction::Recovery,
            None,
            None,
            None,
            "自动恢复",
            "重试2次未成功",
            4000,
            false,
            Some("仍不可达"),
        )
        .unwrap();

    // 4. 重试第3次成功
    writer
        .trace(
            TraceAction::Recovery,
            None,
            None,
            None,
            "自动恢复",
            "恢复成功 (3次)",
            8000,
            true,
            None,
        )
        .unwrap();

    let entries = writer.read_all().unwrap();
    assert_eq!(entries.len(), 4);

    // 验证事件类型和结果
    assert!(entries.iter().all(|e| e.action == TraceAction::Recovery));
    assert!(!entries[0].success);
    assert!(!entries[1].success);
    assert!(!entries[2].success);
    assert!(entries[3].success);

    // 验证 DevTrace 摘要
    let summary = writer.summary();
    let stats = summary.get_action_stats(TraceAction::Recovery).unwrap();
    assert_eq!(stats.count, 4);
    assert_eq!(stats.success_count, 1);
    assert!((stats.success_rate() - 0.25).abs() < 0.001);
}

/// 测试 21: TraceAction::all() 包含 Recovery + HealthCheck + SiteFailover + PerformanceStats + CacheTuning + SearchQuality + MemoryInjection + MemoryEvaluation
#[test]
fn test_trace_action_all_includes_recovery() {
    let all = TraceAction::all();
    assert!(all.contains(&TraceAction::Recovery));
    assert!(all.contains(&TraceAction::HealthCheck));
    assert!(all.contains(&TraceAction::SiteFailover));
    assert!(all.contains(&TraceAction::PerformanceStats));
    assert_eq!(all.len(), 22); // 22 种操作类型
}

/// 测试 22: Recovery 事件与其他 trace 事件共存
#[test]
fn test_recovery_coexists_with_other_traces() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
    let writer = DevTraceWriter::new(dir.path());

    // Planning
    writer
        .trace(
            TraceAction::Planning,
            None,
            None,
            None,
            "plan",
            "3 phases",
            5000,
            true,
            None,
        )
        .unwrap();

    // Task execution
    writer
        .trace(
            TraceAction::TaskExecution,
            Some(0),
            Some(0),
            Some("task1"),
            "exec",
            "done",
            3000,
            true,
            None,
        )
        .unwrap();

    // Recovery (断连)
    writer
        .trace(
            TraceAction::Recovery,
            None,
            None,
            None,
            "check",
            "ChromeUnreachable",
            100,
            false,
            Some("断连"),
        )
        .unwrap();

    // Recovery (恢复成功)
    writer
        .trace(
            TraceAction::Recovery,
            None,
            None,
            None,
            "recover",
            "Connected",
            8000,
            true,
            None,
        )
        .unwrap();

    // Compile check (恢复后继续)
    writer
        .trace(
            TraceAction::CompileCheck,
            Some(0),
            Some(0),
            Some("task1"),
            "cargo check",
            "ok",
            500,
            true,
            None,
        )
        .unwrap();

    let entries = writer.read_all().unwrap();
    assert_eq!(entries.len(), 5);

    // 验证操作类型多样性
    let actions: Vec<_> = entries.iter().map(|e| e.action).collect();
    assert!(actions.contains(&TraceAction::Planning));
    assert!(actions.contains(&TraceAction::TaskExecution));
    assert!(actions.contains(&TraceAction::Recovery));
    assert!(actions.contains(&TraceAction::CompileCheck));

    // 验证摘要
    let summary = writer.summary();
    assert!(summary.get_action_stats(TraceAction::Recovery).is_some());
    assert!(summary.get_action_stats(TraceAction::Planning).is_some());
}

// ============================================================================
//  Orchestrator 集成测试 (禁用自动恢复 → 向后兼容)
// ============================================================================

/// 测试 23: Orchestrator 默认禁用自动恢复 (向后兼容)
#[tokio::test]
async fn test_orchestrator_default_no_auto_recovery() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![simple_plan(), success_code()]);
    let orch = Orchestrator::new(&chat, MockTestRunner, MockExtractor, ws_dir, "test", 3, 60);

    // 默认不启用自动恢复
    assert!(orch.connection_monitor.is_none());
    assert!(orch.auto_recovery.is_none());
}

/// 测试 24: Orchestrator 启用自动恢复
#[tokio::test]
async fn test_orchestrator_with_auto_recovery() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![simple_plan(), success_code()]);
    let orch = Orchestrator::new(&chat, MockTestRunner, MockExtractor, ws_dir, "test", 3, 60)
        .with_auto_recovery(9222, 10);

    // 启用后字段不为 None
    assert!(orch.connection_monitor.is_some());
    assert!(orch.auto_recovery.is_some());
}

/// 测试 25: 启用自动恢复但无 Chrome → 任务失败 (不 panic)
#[tokio::test]
async fn test_orchestrator_auto_recovery_no_chrome_fails_gracefully() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![simple_plan(), success_code()]);
    let mut orch = Orchestrator::new(&chat, MockTestRunner, MockExtractor, ws_dir, "test", 3, 60)
        .with_auto_recovery(19999, 2); // 端口 19999 无 Chrome, 最多 2 次重试

    // 运行应该返回错误 (而非 panic)
    let result = orch.run().await;
    assert!(result.is_err(), "无 Chrome 时应返回错误");
}

/// 测试 26: 禁用自动恢复 → 正常运行 (向后兼容)
#[tokio::test]
async fn test_orchestrator_no_auto_recovery_works() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![simple_plan(), success_code()]);
    let mut orch = Orchestrator::new(&chat, MockTestRunner, MockExtractor, ws_dir, "test", 3, 60);

    // 不启用自动恢复, 应正常运行
    let result = orch.run().await;
    assert!(result.is_ok(), "禁用自动恢复时应正常运行");
}

/// 测试 27: 自动恢复 + DevTrace 共存
#[tokio::test]
async fn test_auto_recovery_with_devtrace() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![simple_plan(), success_code()]);
    let mut orch = Orchestrator::new(&chat, MockTestRunner, MockExtractor, ws_dir, "test", 3, 60)
        .with_dev_trace(true)
        .with_auto_recovery(19999, 2);

    // 运行会因无 Chrome 而失败, 但 DevTrace 应记录 Recovery 事件
    let _ = orch.run().await;

    // 检查 DevTrace
    let writer = DevTraceWriter::new(Path::new(ws_dir));
    let entries = writer.read_all().unwrap_or_default();

    // 应该有 Recovery 类型的 trace 条目
    let recovery_entries: Vec<_> = entries
        .iter()
        .filter(|e| e.action == TraceAction::Recovery)
        .collect();
    assert!(!recovery_entries.is_empty(), "应有 Recovery trace 条目");
}

/// 测试 28: 自动恢复 + 全功能共存 (DevTrace 记录多种事件类型)
#[tokio::test]
async fn test_auto_recovery_with_all_features() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![simple_plan(), success_code()]);
    let mut orch = Orchestrator::new(&chat, MockTestRunner, MockExtractor, ws_dir, "test", 3, 60)
        .with_slash_commands(true)
        .with_dev_trace(true)
        .with_loop_detection(3)
        .with_steer_reminder(10)
        .with_context_handoff(50)
        .with_auto_recovery(19999, 2);

    // 运行会因无 Chrome 而失败
    let _ = orch.run().await;

    // DevTrace 应有 Recovery 条目
    let writer = DevTraceWriter::new(Path::new(ws_dir));
    let entries = writer.read_all().unwrap_or_default();
    let has_recovery = entries.iter().any(|e| e.action == TraceAction::Recovery);
    assert!(has_recovery, "全功能 + 自动恢复应有 Recovery trace");
}

// ============================================================================
//  24h 可靠性场景测试
// ============================================================================

/// 测试 29: 模拟 24h 运行中多次断连/恢复
#[test]
fn test_24h_multiple_disconnects() {
    let mut monitor = ConnectionMonitor::new(9222);

    // 模拟 24 小时运行中 5 次断连/恢复
    for i in 0..5 {
        // 断连
        monitor.consecutive_failures = 3;
        monitor.last_status = ConnectionStatus::ChromeUnreachable;

        // 记录恢复事件
        monitor.record_recovery_event(RecoveryEvent::new(
            0,
            ConnectionStatus::ChromeUnreachable,
            ConnectionStatus::Connected,
            &format!("24h 运行中第 {} 次恢复", i + 1),
            10000 + i as u64 * 1000,
            true,
            None,
        ));

        // 恢复成功
        monitor.reset();
    }

    assert_eq!(monitor.recovery_events().len(), 5);
    assert!(monitor.recovery_events().iter().all(|e| e.success));
}

/// 测试 30: 模拟 24h 运行中混合成功/失败恢复
#[test]
fn test_24h_mixed_recovery_results() {
    let mut monitor = ConnectionMonitor::new(9222);

    // 3 次成功恢复
    for i in 0..3 {
        monitor.record_recovery_event(RecoveryEvent::new(
            0,
            ConnectionStatus::ChromeUnreachable,
            ConnectionStatus::Connected,
            &format!("成功恢复 #{}", i + 1),
            5000,
            true,
            None,
        ));
    }

    // 2 次失败恢复
    for i in 0..2 {
        monitor.record_recovery_event(RecoveryEvent::new(
            0,
            ConnectionStatus::ChromeUnreachable,
            ConnectionStatus::ChromeUnreachable,
            &format!("恢复失败 #{}", i + 1),
            60000,
            false,
            Some("Chrome 未在 60s 内恢复"),
        ));
    }

    assert_eq!(monitor.recovery_events().len(), 5);

    let summary = monitor.summary();
    assert_eq!(summary.recovery_events.len(), 5);

    let report = summary.to_report();
    assert!(report.contains("恢复事件"));
    assert!(report.contains("成功恢复"));
    assert!(report.contains("恢复失败"));
}

/// 测试 31: 模拟 Chrome 标签页关闭后恢复
#[test]
fn test_tab_closed_recovery() {
    let mut monitor = ConnectionMonitor::new(9222);

    // 标签页关闭
    monitor.last_status = ConnectionStatus::TabClosed;
    monitor.consecutive_failures = 1;

    // 记录恢复 (重新发现标签页)
    monitor.record_recovery_event(RecoveryEvent::new(
        0,
        ConnectionStatus::TabClosed,
        ConnectionStatus::Connected,
        "重新发现聊天标签页",
        2000,
        true,
        None,
    ));

    monitor.reset();
    assert!(monitor.last_status().is_connected());
    assert_eq!(monitor.recovery_events().len(), 1);
}

/// 测试 32: 模拟 WebSocket 断连后恢复
#[test]
fn test_websocket_error_recovery() {
    let mut monitor = ConnectionMonitor::new(9222);

    // WebSocket 错误
    monitor.last_status = ConnectionStatus::WebSocketError("connection reset".to_string());
    monitor.consecutive_failures = 2;

    // 记录恢复 (重新连接 WebSocket)
    monitor.record_recovery_event(RecoveryEvent::new(
        0,
        ConnectionStatus::WebSocketError("connection reset".to_string()),
        ConnectionStatus::Connected,
        "重新连接 WebSocket",
        5000,
        true,
        None,
    ));

    monitor.reset();
    assert!(monitor.last_status().is_connected());
}

/// 测试 33: 退避策略在 24h 运行中的总等待时间
#[test]
fn test_24h_total_recovery_wait_time() {
    let b = BackoffStrategy::new(2, 60);

    // 10 次重试的总等待时间
    let total = b.total_delay_secs(10);
    // 4 + 8 + 16 + 32 + 60 + 60 + 60 + 60 + 60 + 60 = 420
    assert_eq!(total, 420);

    // 应在 7 分钟内完成 (420s = 7min)
    assert!(total <= 420, "10 次重试总等待时间应 <= 7 分钟");
}

/// 测试 34: 连接监控 + 自动恢复 + DevTrace 联合报告
#[test]
fn test_combined_reports() {
    let mut monitor = ConnectionMonitor::new(9222);
    monitor.total_checks = 500;
    monitor.total_failures = 8;

    let mut recovery = AutoRecovery::new(RecoveryConfig::default());
    recovery.total_recoveries = 8;
    recovery.total_successes = 7;

    let monitor_report = monitor.summary().to_report();
    let recovery_report = recovery.summary().to_report();

    assert!(monitor_report.contains("连接监控报告"));
    assert!(monitor_report.contains("总检查次数: 500"));
    assert!(recovery_report.contains("自动恢复报告"));
    assert!(recovery_report.contains("总恢复次数: 8"));
    assert!(recovery_report.contains("成功次数: 7"));
}

/// 测试 35: RecoveryEvent 序列化 (用于持久化)
#[test]
fn test_recovery_event_persistence() {
    let event = RecoveryEvent::new(
        123456789,
        ConnectionStatus::ChromeUnreachable,
        ConnectionStatus::Connected,
        "指数退避第5次成功",
        30000,
        true,
        None,
    );

    let json = serde_json::to_string(&event).unwrap();
    let parsed: RecoveryEvent = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed.timestamp_ms, 123456789);
    assert_eq!(parsed.before_status, ConnectionStatus::ChromeUnreachable);
    assert_eq!(parsed.after_status, ConnectionStatus::Connected);
    assert_eq!(parsed.strategy, "指数退避第5次成功");
    assert_eq!(parsed.duration_ms, 30000);
    assert!(parsed.success);
}

/// 测试 36: ConnectionStatus Display 格式
#[test]
fn test_connection_status_display_format() {
    assert_eq!(ConnectionStatus::Connected.to_string(), "Connected");
    assert_eq!(
        ConnectionStatus::ChromeUnreachable.to_string(),
        "ChromeUnreachable"
    );
    assert_eq!(ConnectionStatus::TabClosed.to_string(), "TabClosed");
    assert_eq!(ConnectionStatus::CheckTimeout.to_string(), "CheckTimeout");
    let ws_err = ConnectionStatus::WebSocketError("timeout".to_string());
    assert!(ws_err.to_string().contains("timeout"));
}

/// 测试 37: RecoveryConfig 构建器链
#[test]
fn test_recovery_config_builder_chain() {
    let config = RecoveryConfig::new(9222, 15).with_backoff(BackoffStrategy::new(3, 120));

    assert_eq!(config.port, 9222);
    assert_eq!(config.max_retries, 15);
    assert_eq!(config.backoff.base_secs, 3);
    assert_eq!(config.backoff.max_delay_secs, 120);
}

/// 测试 38: AutoRecovery with_port 便捷构造
#[test]
fn test_auto_recovery_with_port() {
    let recovery = AutoRecovery::with_port(9333);
    assert_eq!(recovery.config().port, 9333);
    assert_eq!(recovery.config().max_retries, 10); // 默认 10 次
}

/// 测试 39: 多次恢复后历史记录完整
#[tokio::test]
async fn test_multiple_recovery_history_complete() {
    let config = RecoveryConfig::new(19999, 1);
    let mut recovery = AutoRecovery::new(config);
    let mut monitor = ConnectionMonitor::new(19999);

    // 执行 5 次恢复
    for i in 0..5 {
        let result = recovery.recover_no_wait(&mut monitor).await;
        assert!(result.is_failed(), "第 {} 次恢复应失败", i + 1);
    }

    assert_eq!(recovery.recovery_history().len(), 5);
    assert_eq!(recovery.total_recoveries(), 5);
    assert_eq!(recovery.total_successes(), 0);

    // 每次恢复的 attempts 应该都是 1 (max_retries=1)
    for result in recovery.recovery_history() {
        assert_eq!(result.attempts(), 1);
    }
}

/// 测试 40: MonitorConfig 默认值合理
#[test]
fn test_monitor_config_defaults_reasonable() {
    let config = MonitorConfig::default();
    assert!(config.check_timeout_secs >= 5, "检查超时至少 5s");
    assert!(config.heartbeat_interval_secs >= 10, "心跳间隔至少 10s");
    assert!(config.max_consecutive_failures >= 2, "连续失败阈值至少 2");
}
