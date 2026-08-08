//! FailoverChatClient 集成测试
//!
//! 测试多网站自动切换 ChatClient 包装器的完整功能:
//! - 基本消息发送
//! - 健康检查间隔
//! - 多网站自动切换 (不健康/发送失败)
//! - 性能统计
//! - DevTrace 集成
//! - 冷却时间与最大失败次数

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use forge::browser::SiteType;
use forge::dev_trace::{DevTraceWriter, TraceAction};
use forge::failover_chat::FailoverChatClient;
use forge::site_health::{HealthCheckResult, SiteHealthStatus};
use forge::traits::{ChatClient, ChatResult, Failoverable};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tempfile::tempdir;

// ============================================================================
//  MockFailoverable — 集成测试用的 Failoverable 实现
// ============================================================================

/// 测试用的可故障切换客户端
///
/// 实现 `Failoverable` trait, 提供:
/// - 预编程回复队列 (按调用顺序弹出)
/// - 可配置的健康检查结果
/// - 对话轮数跟踪
/// - 发送消息记录 (用于断言)
/// - 健康检查调用记录 (用于断言)
struct MockFailoverable {
    /// 预编程回复队列
    responses: Arc<StdMutex<Vec<String>>>,
    /// 记录所有收到的消息
    sent_messages: Arc<StdMutex<Vec<String>>>,
    /// 健康检查结果队列 (每次调用弹出一个, 队列为空时使用 default_health)
    health_results: Arc<StdMutex<Vec<HealthCheckResult>>>,
    /// 默认健康检查结果
    default_health: Arc<StdMutex<HealthCheckResult>>,
    /// 网站类型
    site: SiteType,
    /// 对话轮数
    turn_count: AtomicUsize,
    /// 健康检查调用次数
    health_check_calls: AtomicUsize,
    /// 是否强制 send_message 失败
    force_error: Arc<StdMutex<bool>>,
    /// start_new_conversation 调用次数
    new_conversation_calls: AtomicUsize,
}

impl MockFailoverable {
    fn new(site: SiteType, responses: Vec<String>) -> Self {
        Self {
            responses: Arc::new(StdMutex::new(responses)),
            sent_messages: Arc::new(StdMutex::new(vec![])),
            health_results: Arc::new(StdMutex::new(vec![])),
            default_health: Arc::new(StdMutex::new(HealthCheckResult::new(
                SiteHealthStatus::Healthy,
            ))),
            site,
            turn_count: AtomicUsize::new(0),
            health_check_calls: AtomicUsize::new(0),
            force_error: Arc::new(StdMutex::new(false)),
            new_conversation_calls: AtomicUsize::new(0),
        }
    }

    /// 设置默认健康检查结果
    fn with_default_health(self, health: HealthCheckResult) -> Self {
        *self.default_health.lock().unwrap() = health;
        self
    }

    /// 设置健康检查结果队列
    #[allow(dead_code)]
    fn with_health_results(self, results: Vec<HealthCheckResult>) -> Self {
        *self.health_results.lock().unwrap() = results;
        self
    }

    /// 强制 send_message 返回错误
    fn with_force_error(self) -> Self {
        *self.force_error.lock().unwrap() = true;
        self
    }

    /// 获取收到的消息列表
    fn sent_messages(&self) -> Vec<String> {
        self.sent_messages.lock().unwrap().clone()
    }

    /// 获取健康检查调用次数
    fn health_check_call_count(&self) -> usize {
        self.health_check_calls.load(Ordering::Relaxed)
    }

    /// 获取当前对话轮数
    #[allow(dead_code)]
    fn turn(&self) -> usize {
        self.turn_count.load(Ordering::Relaxed)
    }

    /// 获取 start_new_conversation 调用次数
    fn new_conversation_count(&self) -> usize {
        self.new_conversation_calls.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl Failoverable for MockFailoverable {
    fn site_type(&self) -> SiteType {
        self.site
    }

    async fn health_check(&self) -> Result<HealthCheckResult> {
        self.health_check_calls.fetch_add(1, Ordering::Relaxed);
        let results = self.health_results.lock().unwrap();
        if !results.is_empty() {
            Ok(results[0].clone())
        } else {
            Ok(self.default_health.lock().unwrap().clone())
        }
    }
}

#[async_trait]
impl ChatClient for MockFailoverable {
    async fn send_message(&self, msg: &str, _timeout: u64) -> Result<ChatResult> {
        self.sent_messages.lock().unwrap().push(msg.to_string());
        self.turn_count.fetch_add(1, Ordering::Relaxed);

        if *self.force_error.lock().unwrap() {
            return Err(anyhow!("mock send_message error"));
        }

        let mut queue = self.responses.lock().unwrap();
        let text = if queue.is_empty() {
            "(empty)".to_string()
        } else {
            queue.remove(0)
        };
        Ok(ChatResult {
            text,
            timed_out: false,
        })
    }

    async fn start_new_conversation(&self) -> Result<()> {
        self.new_conversation_calls.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn conversation_turn_count(&self) -> usize {
        self.turn_count.load(Ordering::Relaxed)
    }
}

// ============================================================================
//  辅助函数
// ============================================================================

/// 创建两个 MockFailoverable (Z.ai + DeepSeek)
fn make_two_mocks() -> (MockFailoverable, MockFailoverable) {
    let tab0 = MockFailoverable::new(SiteType::Zai, vec!["response from Zai".to_string()]);
    let tab1 = MockFailoverable::new(
        SiteType::DeepSeek,
        vec!["response from DeepSeek".to_string()],
    );
    (tab0, tab1)
}

/// 创建临时 DevTraceWriter
fn make_trace_writer() -> (tempfile::TempDir, DevTraceWriter) {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
    let writer = DevTraceWriter::new(dir.path());
    (dir, writer)
}

// ============================================================================
//  基本功能测试
// ============================================================================

#[tokio::test]
async fn test_integration_basic_send_message() {
    let (tab0, _tab1) = make_two_mocks();
    let client = FailoverChatClient::new(vec![&tab0], 0, 3, 30, 0);

    let result = client.send_message("hello", 60).await.unwrap();
    assert_eq!(result.text, "response from Zai");
    assert!(!result.timed_out);
    assert_eq!(tab0.sent_messages(), vec!["hello".to_string()]);
}

#[tokio::test]
async fn test_integration_turn_count_increment() {
    let (tab0, _tab1) = make_two_mocks();
    let client = FailoverChatClient::new(vec![&tab0], 0, 3, 30, 0);

    assert_eq!(client.conversation_turn_count(), 0);
    client.send_message("msg1", 60).await.unwrap();
    assert_eq!(client.conversation_turn_count(), 1);
    client.send_message("msg2", 60).await.unwrap();
    assert_eq!(client.conversation_turn_count(), 2);
}

#[tokio::test]
async fn test_integration_start_new_conversation_delegates() {
    let (tab0, _tab1) = make_two_mocks();
    let client = FailoverChatClient::new(vec![&tab0], 0, 3, 30, 0);

    client.start_new_conversation().await.unwrap();
    assert_eq!(tab0.new_conversation_count(), 1);
}

#[tokio::test]
async fn test_integration_tab_count_and_index() {
    let (tab0, tab1) = make_two_mocks();
    let client = FailoverChatClient::new(vec![&tab0, &tab1], 0, 3, 30, 5);
    assert_eq!(client.tab_count(), 2);
    assert_eq!(client.current_tab_index(), 0);
}

// ============================================================================
//  健康检查间隔测试
// ============================================================================

#[tokio::test]
async fn test_integration_health_check_interval_skips() {
    let (tab0, _tab1) = make_two_mocks();
    let client = FailoverChatClient::new(vec![&tab0], 0, 3, 30, 10);

    client.send_message("hello", 60).await.unwrap();
    assert_eq!(
        tab0.health_check_call_count(),
        0,
        "interval=10, turn=0 → 跳过"
    );
}

#[tokio::test]
async fn test_integration_health_check_interval_zero_always_checks() {
    let (tab0, _tab1) = make_two_mocks();
    let client = FailoverChatClient::new(vec![&tab0], 0, 3, 30, 0);

    client.send_message("hello", 60).await.unwrap();
    assert_eq!(tab0.health_check_call_count(), 1);
    client.send_message("hello2", 60).await.unwrap();
    assert_eq!(tab0.health_check_call_count(), 2);
}

#[tokio::test]
async fn test_integration_health_check_interval_triggers_after_n_turns() {
    let (tab0, _tab1) = make_two_mocks();
    let client = FailoverChatClient::new(vec![&tab0], 0, 3, 30, 2);

    client.send_message("msg1", 60).await.unwrap();
    assert_eq!(tab0.health_check_call_count(), 0, "turn=0, 0 < 2 → 跳过");

    client.send_message("msg2", 60).await.unwrap();
    assert_eq!(tab0.health_check_call_count(), 0, "turn=1, 1 < 2 → 跳过");

    client.send_message("msg3", 60).await.unwrap();
    assert_eq!(tab0.health_check_call_count(), 1, "turn=2, 2 >= 2 → 检查");
}

// ============================================================================
//  多网站自动切换测试
// ============================================================================

#[tokio::test]
async fn test_integration_failover_on_unhealthy() {
    let tab0 = MockFailoverable::new(SiteType::Zai, vec!["zai response".to_string()])
        .with_default_health(HealthCheckResult::new(SiteHealthStatus::NotLoggedIn));
    let tab1 = MockFailoverable::new(SiteType::DeepSeek, vec!["deepseek response".to_string()])
        .with_default_health(HealthCheckResult::new(SiteHealthStatus::Healthy));

    let client = FailoverChatClient::new(vec![&tab0, &tab1], 0, 3, 0, 0);

    let result = client.send_message("hello", 60).await.unwrap();
    assert_eq!(result.text, "deepseek response");
    assert_eq!(client.current_tab_index(), 1, "应切换到 tab1");
    assert_eq!(tab1.sent_messages(), vec!["hello".to_string()]);
}

#[tokio::test]
async fn test_integration_failover_on_send_error() {
    let tab0 = MockFailoverable::new(SiteType::Zai, vec![])
        .with_default_health(HealthCheckResult::new(SiteHealthStatus::Healthy))
        .with_force_error();
    let tab1 = MockFailoverable::new(SiteType::DeepSeek, vec!["deepseek response".to_string()])
        .with_default_health(HealthCheckResult::new(SiteHealthStatus::Healthy));

    let client = FailoverChatClient::new(vec![&tab0, &tab1], 0, 3, 0, 0);

    let result = client.send_message("hello", 60).await.unwrap();
    assert_eq!(result.text, "deepseek response");
    assert_eq!(client.current_tab_index(), 1);
    assert_eq!(tab0.sent_messages(), vec!["hello".to_string()]);
    assert_eq!(tab1.sent_messages(), vec!["hello".to_string()]);
}

#[tokio::test]
async fn test_integration_no_switch_when_healthy() {
    let tab0 = MockFailoverable::new(SiteType::Zai, vec!["zai response".to_string()])
        .with_default_health(HealthCheckResult::new(SiteHealthStatus::Healthy));
    let tab1 = MockFailoverable::new(SiteType::DeepSeek, vec![]);

    let client = FailoverChatClient::new(vec![&tab0, &tab1], 0, 3, 30, 0);

    let result = client.send_message("hello", 60).await.unwrap();
    assert_eq!(result.text, "zai response");
    assert_eq!(client.current_tab_index(), 0);
    assert!(tab1.sent_messages().is_empty());
}

#[tokio::test]
async fn test_integration_no_available_tabs_to_switch() {
    let tab0 = MockFailoverable::new(SiteType::Zai, vec![]).with_force_error();

    let client = FailoverChatClient::new(vec![&tab0], 0, 3, 0, 0);

    let result = client.send_message("hello", 60).await;
    assert!(result.is_err());
    assert_eq!(client.current_tab_index(), 0);
}

#[tokio::test]
async fn test_integration_failover_success_resets_failure_count() {
    let tab0 = MockFailoverable::new(
        SiteType::Zai,
        vec!["response1".to_string(), "response2".to_string()],
    )
    .with_default_health(HealthCheckResult::new(SiteHealthStatus::Healthy));

    let client = FailoverChatClient::new(vec![&tab0], 0, 3, 30, 0);

    client.send_message("msg1", 60).await.unwrap();
    client.send_message("msg2", 60).await.unwrap();

    let stats = client.get_stats().await;
    assert_eq!(stats[0].2.success_count, 2);
}

#[tokio::test]
async fn test_integration_failover_multiple_switches() {
    // 三个标签页, 前两个不健康, 第三个健康
    let tab0 = MockFailoverable::new(SiteType::Zai, vec![])
        .with_default_health(HealthCheckResult::new(SiteHealthStatus::NotLoggedIn));
    let tab1 = MockFailoverable::new(SiteType::DeepSeek, vec![])
        .with_default_health(HealthCheckResult::new(SiteHealthStatus::RateLimited));
    let tab2 = MockFailoverable::new(SiteType::Kimi, vec!["kimi response".to_string()])
        .with_default_health(HealthCheckResult::new(SiteHealthStatus::Healthy));

    let client = FailoverChatClient::new(vec![&tab0, &tab1, &tab2], 0, 3, 0, 0);

    let result = client.send_message("hello", 60).await.unwrap();
    // tab0 不健康 → 切换到 tab1 → tab1.send_message 成功 (但返回 "(empty)")
    assert_eq!(result.text, "(empty)");
    assert_eq!(client.current_tab_index(), 1);
}

// ============================================================================
//  性能统计测试
// ============================================================================

#[tokio::test]
async fn test_integration_stats_after_successful_send() {
    let (tab0, _tab1) = make_two_mocks();
    let client = FailoverChatClient::new(vec![&tab0], 0, 3, 30, 0);

    client.send_message("hello", 60).await.unwrap();

    let stats = client.get_stats().await;
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].0, 0);
    assert_eq!(stats[0].1, SiteType::Zai);
    assert_eq!(stats[0].2.total_sends, 1);
    assert_eq!(stats[0].2.success_count, 1);
    assert_eq!(stats[0].2.failure_count, 0);
}

#[tokio::test]
async fn test_integration_stats_after_failed_send() {
    let tab0 = MockFailoverable::new(SiteType::Zai, vec![]).with_force_error();
    let client = FailoverChatClient::new(vec![&tab0], 0, 3, 0, 0);

    let _ = client.send_message("hello", 60).await;

    let stats = client.get_stats().await;
    assert_eq!(stats[0].2.total_sends, 1);
    assert_eq!(stats[0].2.failure_count, 1);
    assert_eq!(stats[0].2.success_count, 0);
}

#[tokio::test]
async fn test_integration_stats_after_failover() {
    let tab0 = MockFailoverable::new(SiteType::Zai, vec![])
        .with_default_health(HealthCheckResult::new(SiteHealthStatus::NotLoggedIn));
    let tab1 = MockFailoverable::new(SiteType::DeepSeek, vec!["deepseek response".to_string()])
        .with_default_health(HealthCheckResult::new(SiteHealthStatus::Healthy));

    let client = FailoverChatClient::new(vec![&tab0, &tab1], 0, 3, 0, 0);

    client.send_message("hello", 60).await.unwrap();

    let stats = client.get_stats().await;
    // tab0: 健康检查不健康, failover_from +1
    assert_eq!(stats[0].2.failover_from_count, 1);
    assert_eq!(stats[0].2.health_checks, 1);
    assert_eq!(stats[0].2.healthy_count, 0);
    // tab1: 被切换到, 成功发送
    assert_eq!(stats[1].2.failover_to_count, 1);
    assert_eq!(stats[1].2.success_count, 1);
}

// ============================================================================
//  DevTrace 集成测试
// ============================================================================

#[tokio::test]
async fn test_integration_devtrace_health_check_written() {
    let (_dir, writer) = make_trace_writer();
    let (tab0, _tab1) = make_two_mocks();
    let client = FailoverChatClient::new(vec![&tab0], 0, 3, 30, 0);
    client.set_dev_trace(writer.clone());

    client.send_message("hello", 60).await.unwrap();

    let entries = writer.read_all().unwrap();
    let has_health = entries.iter().any(|e| e.action == TraceAction::HealthCheck);
    assert!(has_health, "DevTrace 应包含 HealthCheck 条目");

    let health_entry = entries
        .iter()
        .find(|e| e.action == TraceAction::HealthCheck)
        .unwrap();
    assert!(health_entry.input_summary.contains("Z.ai"));
}

#[tokio::test]
async fn test_integration_devtrace_failover_written() {
    let (_dir, writer) = make_trace_writer();
    let tab0 = MockFailoverable::new(SiteType::Zai, vec![])
        .with_default_health(HealthCheckResult::new(SiteHealthStatus::NotLoggedIn));
    let tab1 = MockFailoverable::new(SiteType::DeepSeek, vec!["response".to_string()])
        .with_default_health(HealthCheckResult::new(SiteHealthStatus::Healthy));

    let client = FailoverChatClient::new(vec![&tab0, &tab1], 0, 3, 0, 0);
    client.set_dev_trace(writer.clone());

    client.send_message("hello", 60).await.unwrap();

    let entries = writer.read_all().unwrap();
    let has_failover = entries
        .iter()
        .any(|e| e.action == TraceAction::SiteFailover);
    assert!(has_failover, "DevTrace 应包含 SiteFailover 条目");

    let failover_entry = entries
        .iter()
        .find(|e| e.action == TraceAction::SiteFailover)
        .unwrap();
    assert!(failover_entry.input_summary.contains("Z.ai"));
    assert!(failover_entry.input_summary.contains("DeepSeek"));
    assert!(failover_entry.success);
}

#[tokio::test]
async fn test_integration_devtrace_failover_failure_written() {
    let (_dir, writer) = make_trace_writer();
    let tab0 = MockFailoverable::new(SiteType::Zai, vec![]).with_force_error();

    let client = FailoverChatClient::new(vec![&tab0], 0, 3, 0, 0);
    client.set_dev_trace(writer.clone());

    let _ = client.send_message("hello", 60).await;

    let entries = writer.read_all().unwrap();
    let failover_entries: Vec<_> = entries
        .iter()
        .filter(|e| e.action == TraceAction::SiteFailover)
        .collect();
    assert!(!failover_entries.is_empty());
    assert!(!failover_entries[0].success);
    assert_eq!(failover_entries[0].output_summary, "无法切换");
}

#[tokio::test]
async fn test_integration_devtrace_performance_stats_written() {
    let (_dir, writer) = make_trace_writer();
    let (tab0, _tab1) = make_two_mocks();
    let client = FailoverChatClient::new(vec![&tab0], 0, 3, 30, 0);
    client.set_dev_trace(writer.clone());

    client.send_message("hello", 60).await.unwrap();
    client.write_final_trace().await;

    let entries = writer.read_all().unwrap();
    let has_perf = entries
        .iter()
        .any(|e| e.action == TraceAction::PerformanceStats);
    assert!(has_perf, "DevTrace 应包含 PerformanceStats 条目");

    let perf_entry = entries
        .iter()
        .find(|e| e.action == TraceAction::PerformanceStats)
        .unwrap();
    assert!(perf_entry.input_summary.contains("Z.ai"));
    assert!(perf_entry.output_summary.contains("发送:1"));
    assert!(perf_entry.output_summary.contains("成功:1"));
}

#[tokio::test]
async fn test_integration_devtrace_no_writer_no_crash() {
    let (tab0, _tab1) = make_two_mocks();
    let client = FailoverChatClient::new(vec![&tab0], 0, 3, 30, 0);

    // 不调用 set_dev_trace
    client.send_message("hello", 60).await.unwrap();
    client.write_final_trace().await;
    // 不应 panic
}

#[tokio::test]
async fn test_integration_devtrace_multiple_sends_accumulate() {
    let (_dir, writer) = make_trace_writer();
    let tab0 = MockFailoverable::new(
        SiteType::Zai,
        vec!["r1".to_string(), "r2".to_string(), "r3".to_string()],
    )
    .with_default_health(HealthCheckResult::new(SiteHealthStatus::Healthy));

    let client = FailoverChatClient::new(vec![&tab0], 0, 3, 30, 0);
    client.set_dev_trace(writer.clone());

    for i in 0..3 {
        client.send_message(&format!("msg{}", i), 60).await.unwrap();
    }

    let entries = writer.read_all().unwrap();
    let health_count = entries
        .iter()
        .filter(|e| e.action == TraceAction::HealthCheck)
        .count();
    assert_eq!(health_count, 3, "应有 3 个 HealthCheck 条目");
}

#[tokio::test]
async fn test_integration_devtrace_shared_writer_with_orchestrator() {
    let (_dir, writer) = make_trace_writer();

    // 模拟 Orchestrator 写入 Planning
    writer
        .trace(
            TraceAction::Planning,
            None,
            None,
            None,
            "拆解目标",
            "3阶段5任务",
            5000,
            true,
            None,
        )
        .unwrap();

    // FailoverChatClient 写入 HealthCheck + SiteFailover
    let tab0 = MockFailoverable::new(SiteType::Zai, vec!["response".to_string()])
        .with_default_health(HealthCheckResult::new(SiteHealthStatus::RateLimited));
    let tab1 = MockFailoverable::new(SiteType::DeepSeek, vec!["deepseek response".to_string()])
        .with_default_health(HealthCheckResult::new(SiteHealthStatus::Healthy));

    let client = FailoverChatClient::new(vec![&tab0, &tab1], 0, 3, 0, 0);
    client.set_dev_trace(writer.clone());

    client.send_message("hello", 60).await.unwrap();
    client.write_final_trace().await;

    // Orchestrator 写入 TaskExecution
    writer
        .trace(
            TraceAction::TaskExecution,
            Some(0),
            Some(0),
            Some("初始化"),
            "创建项目",
            "main.rs",
            3000,
            true,
            None,
        )
        .unwrap();

    // 验证所有条目在同一文件中
    let entries = writer.read_all().unwrap();
    let actions: Vec<_> = entries.iter().map(|e| e.action).collect();
    assert!(actions.contains(&TraceAction::Planning));
    assert!(actions.contains(&TraceAction::HealthCheck));
    assert!(actions.contains(&TraceAction::SiteFailover));
    assert!(actions.contains(&TraceAction::PerformanceStats));
    assert!(actions.contains(&TraceAction::TaskExecution));
}

// ============================================================================
//  边界条件测试
// ============================================================================

#[test]
#[should_panic(expected = "至少一个标签页")]
fn test_integration_empty_tabs_panics() {
    let _ = FailoverChatClient::<MockFailoverable>::new(vec![], 0, 3, 30, 5);
}

#[test]
#[should_panic(expected = "初始标签页索引超出范围")]
fn test_integration_index_out_of_range_panics() {
    let (tab0, _tab1) = make_two_mocks();
    let _ = FailoverChatClient::new(vec![&tab0], 5, 3, 30, 5);
}

#[tokio::test]
async fn test_integration_single_tab_no_failover_available() {
    let tab0 = MockFailoverable::new(SiteType::Zai, vec![])
        .with_default_health(HealthCheckResult::new(SiteHealthStatus::NotLoggedIn));

    let client = FailoverChatClient::new(vec![&tab0], 0, 3, 0, 0);

    // 只有一个标签页, 不健康但无法切换 → 仍然尝试发送
    let result = client.send_message("hello", 60).await;
    // tab0 的 responses 为空, 会返回 "(empty)" 或错误
    // 健康检查不健康 → 尝试切换 → 无可用标签页 → 继续用当前标签页发送
    // tab0.send_message → responses 为空 → "(empty)"
    assert!(result.is_ok());
    assert_eq!(result.unwrap().text, "(empty)");
}

#[tokio::test]
async fn test_integration_all_tabs_unhealthy() {
    let tab0 = MockFailoverable::new(SiteType::Zai, vec!["zai resp".to_string()])
        .with_default_health(HealthCheckResult::new(SiteHealthStatus::NotLoggedIn));
    let tab1 = MockFailoverable::new(SiteType::DeepSeek, vec!["ds resp".to_string()])
        .with_default_health(HealthCheckResult::new(SiteHealthStatus::RateLimited));

    let client = FailoverChatClient::new(vec![&tab0, &tab1], 0, 3, 0, 0);

    // tab0 不健康 → 切换到 tab1 → tab1 也不健康 → 但仍发送
    let result = client.send_message("hello", 60).await.unwrap();
    assert_eq!(client.current_tab_index(), 1);
    // tab1 发送成功
    assert_eq!(result.text, "ds resp");
}

#[tokio::test]
async fn test_integration_cooldown_prevents_rapid_switching() {
    let tab0 = MockFailoverable::new(SiteType::Zai, vec![])
        .with_default_health(HealthCheckResult::new(SiteHealthStatus::NotLoggedIn))
        .with_force_error();
    let tab1 = MockFailoverable::new(SiteType::DeepSeek, vec!["ds resp".to_string()])
        .with_default_health(HealthCheckResult::new(SiteHealthStatus::Healthy));

    // cooldown = 0 允许立即切换
    let client = FailoverChatClient::new(vec![&tab0, &tab1], 0, 3, 0, 0);

    // 第一次发送: tab0 不健康 → 切换到 tab1 → 成功
    let result = client.send_message("hello", 60).await.unwrap();
    assert_eq!(result.text, "ds resp");
    assert_eq!(client.current_tab_index(), 1);
}
