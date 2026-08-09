//! 并发故障转移测试 (Session 68)
//!
//! 测试多标签页同时故障场景:
//! - 所有标签页同时发送失败 → 返回错误
//! - 部分标签页健康检查失败 → 自动切换到下一个标签页
//! - 标签页从故障恢复 → 重新可用
//! - 循环切换 (所有标签页依次故障后恢复)
//! - 多标签页发送消息失败 + 健康检查失败组合

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use forge::browser::SiteType;
use forge::dev_trace::DevTraceWriter;
use forge::failover_chat::FailoverChatClient;
use forge::site_health::{HealthCheckResult, SiteHealthStatus};
use forge::traits::{ChatClient, ChatResult, Failoverable};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tempfile::tempdir;

// ============================================================================
//  DynamicMockFailoverable — 支持动态健康状态切换
// ============================================================================

struct DynamicMockFailoverable {
    responses: Arc<StdMutex<Vec<String>>>,
    sent_messages: Arc<StdMutex<Vec<String>>>,
    current_health: Arc<StdMutex<HealthCheckResult>>,
    site: SiteType,
    turn_count: AtomicUsize,
    health_check_calls: AtomicUsize,
    send_calls: AtomicUsize,
    force_error: Arc<StdMutex<bool>>,
    new_conversation_calls: AtomicUsize,
}

impl DynamicMockFailoverable {
    fn new(site: SiteType, responses: Vec<String>) -> Self {
        Self {
            responses: Arc::new(StdMutex::new(responses)),
            sent_messages: Arc::new(StdMutex::new(vec![])),
            current_health: Arc::new(StdMutex::new(HealthCheckResult::new(
                SiteHealthStatus::Healthy,
            ))),
            site,
            turn_count: AtomicUsize::new(0),
            health_check_calls: AtomicUsize::new(0),
            send_calls: AtomicUsize::new(0),
            force_error: Arc::new(StdMutex::new(false)),
            new_conversation_calls: AtomicUsize::new(0),
        }
    }

    fn set_health(&self, status: SiteHealthStatus) {
        *self.current_health.lock().unwrap() = HealthCheckResult::new(status);
    }

    fn set_force_error(&self, force: bool) {
        *self.force_error.lock().unwrap() = force;
    }

    #[allow(dead_code)]
    fn send_count(&self) -> usize {
        self.send_calls.load(Ordering::Relaxed)
    }

    fn health_check_count(&self) -> usize {
        self.health_check_calls.load(Ordering::Relaxed)
    }

    fn sent_messages(&self) -> Vec<String> {
        self.sent_messages.lock().unwrap().clone()
    }

    fn new_conversation_count(&self) -> usize {
        self.new_conversation_calls.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl Failoverable for DynamicMockFailoverable {
    fn site_type(&self) -> SiteType {
        self.site
    }

    async fn health_check(&self) -> Result<HealthCheckResult> {
        self.health_check_calls.fetch_add(1, Ordering::Relaxed);
        Ok(self.current_health.lock().unwrap().clone())
    }
}

#[async_trait]
impl ChatClient for DynamicMockFailoverable {
    async fn send_message(&self, msg: &str, _timeout: u64) -> Result<ChatResult> {
        self.send_calls.fetch_add(1, Ordering::Relaxed);
        self.sent_messages.lock().unwrap().push(msg.to_string());
        self.turn_count.fetch_add(1, Ordering::Relaxed);

        if *self.force_error.lock().unwrap() {
            return Err(anyhow!("forced send error"));
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

fn make_trace_writer() -> (tempfile::TempDir, DevTraceWriter) {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
    let writer = DevTraceWriter::new(dir.path());
    (dir, writer)
}

// ============================================================================
//  并发故障场景测试
// ============================================================================

/// 所有标签页发送都失败 (force_error) → 应返回错误
#[tokio::test]
async fn test_all_tabs_send_error_returns_error() {
    let tab0 = DynamicMockFailoverable::new(SiteType::Zai, vec!["zai resp".to_string()]);
    let tab1 = DynamicMockFailoverable::new(SiteType::DeepSeek, vec!["ds resp".to_string()]);
    let tab2 = DynamicMockFailoverable::new(SiteType::Kimi, vec!["kimi resp".to_string()]);

    // 所有标签页发送都失败
    tab0.set_force_error(true);
    tab1.set_force_error(true);
    tab2.set_force_error(true);

    let client = FailoverChatClient::new(vec![&tab0, &tab1, &tab2], 0, 3, 0, 0);

    let result = client.send_message("hello", 60).await;
    assert!(result.is_err(), "All tabs send_error should return error");
}

/// tab0 不健康 → 自动切换到 tab1 (不检查 tab1 健康) → tab1 发送成功
#[tokio::test]
async fn test_health_check_failure_triggers_failover() {
    let tab0 = DynamicMockFailoverable::new(SiteType::Zai, vec!["zai resp".to_string()]);
    let tab1 = DynamicMockFailoverable::new(SiteType::DeepSeek, vec!["ds resp".to_string()]);

    // tab0 不健康, tab1 健康
    tab0.set_health(SiteHealthStatus::NotLoggedIn);

    let client = FailoverChatClient::new(vec![&tab0, &tab1], 0, 3, 0, 0);

    let result = client.send_message("hello", 60).await.unwrap();
    assert_eq!(result.text, "ds resp");
    assert_eq!(client.current_tab_index(), 1, "Should switch to tab1");
}

/// 所有标签页发送失败 + 不健康 → 应返回错误
#[tokio::test]
async fn test_all_tabs_unhealthy_and_send_error() {
    let tab0 = DynamicMockFailoverable::new(SiteType::Zai, vec![]);
    let tab1 = DynamicMockFailoverable::new(SiteType::DeepSeek, vec![]);
    let tab2 = DynamicMockFailoverable::new(SiteType::Kimi, vec![]);

    tab0.set_health(SiteHealthStatus::NotLoggedIn);
    tab1.set_health(SiteHealthStatus::RateLimited);
    tab2.set_health(SiteHealthStatus::UnderMaintenance);

    tab0.set_force_error(true);
    tab1.set_force_error(true);
    tab2.set_force_error(true);

    let client = FailoverChatClient::new(vec![&tab0, &tab1, &tab2], 0, 3, 0, 0);

    let result = client.send_message("hello", 60).await;
    assert!(
        result.is_err(),
        "All tabs unhealthy + send_error should fail"
    );

    // 验证 tab0 的健康检查被调用
    assert!(
        tab0.health_check_count() >= 1,
        "tab0 health check should be called"
    );
}

/// tab0 发送失败 → 切换到 tab1 → tab1 发送成功
#[tokio::test]
async fn test_send_error_triggers_failover() {
    let tab0 = DynamicMockFailoverable::new(SiteType::Zai, vec![]);
    let tab1 = DynamicMockFailoverable::new(SiteType::DeepSeek, vec!["ds resp".to_string()]);

    tab0.set_force_error(true);

    let client = FailoverChatClient::new(vec![&tab0, &tab1], 0, 3, 0, 0);

    let result = client.send_message("hello", 60).await.unwrap();
    assert_eq!(result.text, "ds resp");
    assert_eq!(
        client.current_tab_index(),
        1,
        "Should failover to tab1 after send error"
    );
}

/// tab0 恢复后仍继续使用 tab1 (不自动切回)
#[tokio::test]
async fn test_tab_recovery_after_failover() {
    let tab0 = DynamicMockFailoverable::new(
        SiteType::Zai,
        vec!["zai resp 1".to_string(), "zai resp 2".to_string()],
    );
    let tab1 = DynamicMockFailoverable::new(SiteType::DeepSeek, vec!["ds resp".to_string()]);

    tab0.set_health(SiteHealthStatus::RateLimited);

    let client = FailoverChatClient::new(vec![&tab0, &tab1], 0, 3, 0, 0);

    // 第一次: tab0 不健康 → 切换到 tab1
    let r1 = client.send_message("msg1", 60).await.unwrap();
    assert_eq!(r1.text, "ds resp");
    assert_eq!(client.current_tab_index(), 1);

    // tab0 恢复健康
    tab0.set_health(SiteHealthStatus::Healthy);

    // 第二次: 继续在 tab1 (不会自动切回)
    tab1.responses.lock().unwrap().push("ds resp 2".to_string());
    let r2 = client.send_message("msg2", 60).await.unwrap();
    assert_eq!(r2.text, "ds resp 2");
    assert_eq!(client.current_tab_index(), 1);
}

/// 循环切换: tab0不健康→切换tab1→tab1不健康→切换tab2
#[tokio::test]
async fn test_circular_failover() {
    let tab0 = DynamicMockFailoverable::new(SiteType::Zai, vec!["zai resp".to_string()]);
    let tab1 = DynamicMockFailoverable::new(SiteType::DeepSeek, vec!["ds resp".to_string()]);
    let tab2 = DynamicMockFailoverable::new(SiteType::Kimi, vec!["kimi resp".to_string()]);

    // tab0 不健康 → 切换
    tab0.set_health(SiteHealthStatus::UnderMaintenance);

    let client = FailoverChatClient::new(vec![&tab0, &tab1, &tab2], 0, 3, 0, 0);

    // 第一次: tab0 不健康 → 切换到某个可用标签页
    let r1 = client.send_message("msg1", 60).await.unwrap();
    // 应该切换到 tab1 或 tab2 (取决于 FailoverChatClient 的切换策略)
    assert!(
        r1.text == "ds resp" || r1.text == "kimi resp",
        "Should get response from a failover tab: got={}",
        r1.text
    );
    assert!(
        client.current_tab_index() >= 1,
        "Should have switched away from tab0"
    );
}

/// 健康检查失败 → 切换 → 统计跟踪
#[tokio::test]
async fn test_failover_stats_track_all_tabs() {
    let tab0 = DynamicMockFailoverable::new(SiteType::Zai, vec![]);
    let tab1 = DynamicMockFailoverable::new(SiteType::DeepSeek, vec!["ds resp".to_string()]);

    tab0.set_health(SiteHealthStatus::NotLoggedIn);

    let client = FailoverChatClient::new(vec![&tab0, &tab1], 0, 3, 0, 0);

    client.send_message("msg1", 60).await.unwrap();

    let stats = client.get_stats().await;
    assert_eq!(stats.len(), 2, "Should have stats for all tabs");

    // tab0: 健康检查失败 → 0 sends
    assert_eq!(stats[0].0, 0);
    assert_eq!(stats[0].2.total_sends, 0, "tab0 should have 0 sends");

    // tab1: 成功发送
    assert_eq!(stats[1].0, 1);
    assert_eq!(stats[1].2.total_sends, 1);
    assert_eq!(stats[1].2.success_count, 1);
}

/// DevTrace 记录故障切换事件
#[tokio::test]
async fn test_failover_with_devtrace() {
    let (dir, writer) = make_trace_writer();

    let tab0 = DynamicMockFailoverable::new(SiteType::Zai, vec![]);
    let tab1 = DynamicMockFailoverable::new(SiteType::DeepSeek, vec!["ds resp".to_string()]);

    tab0.set_health(SiteHealthStatus::NotLoggedIn);

    let client = FailoverChatClient::new(vec![&tab0, &tab1], 0, 3, 0, 0);
    client.set_dev_trace(writer);

    let result = client.send_message("hello", 60).await.unwrap();
    assert_eq!(result.text, "ds resp");
    assert_eq!(client.current_tab_index(), 1);

    let trace_path = dir.path().join(".forge/devtrace.jsonl");
    assert!(trace_path.exists(), "DevTrace file should exist");

    let content = std::fs::read_to_string(&trace_path).unwrap();
    assert!(
        content.contains("Z.ai") || content.contains("DeepSeek") || content.contains("failover"),
        "DevTrace should contain failover event"
    );
}

/// max_failures 限制: 所有标签页发送失败, 超过 max_failures 返回错误
#[tokio::test]
async fn test_max_failures_exhausted() {
    let tab0 = DynamicMockFailoverable::new(SiteType::Zai, vec![]);
    let tab1 = DynamicMockFailoverable::new(SiteType::DeepSeek, vec![]);

    tab0.set_force_error(true);
    tab1.set_force_error(true);

    let client = FailoverChatClient::new(vec![&tab0, &tab1], 0, 1, 0, 0);

    let result = client.send_message("hello", 60).await;
    assert!(
        result.is_err(),
        "Should fail when max_failures is exhausted"
    );
}

/// 消息顺序保持: 同一标签页连续发送
#[tokio::test]
async fn test_failover_preserves_message_order() {
    let tab0 = DynamicMockFailoverable::new(
        SiteType::Zai,
        vec!["zai resp 1".to_string(), "zai resp 2".to_string()],
    );
    let tab1 = DynamicMockFailoverable::new(
        SiteType::DeepSeek,
        vec!["ds resp 1".to_string(), "ds resp 2".to_string()],
    );

    let client = FailoverChatClient::new(vec![&tab0, &tab1], 0, 3, 30, 0);

    let r1 = client.send_message("msg1", 60).await.unwrap();
    assert_eq!(r1.text, "zai resp 1");
    assert_eq!(tab0.sent_messages(), vec!["msg1".to_string()]);

    let r2 = client.send_message("msg2", 60).await.unwrap();
    assert_eq!(r2.text, "zai resp 2");
    assert_eq!(
        tab0.sent_messages(),
        vec!["msg1".to_string(), "msg2".to_string()]
    );
    assert_eq!(client.current_tab_index(), 0);
}

/// start_new_conversation 委托给当前标签页
#[tokio::test]
async fn test_failover_new_conversation_delegates() {
    let tab0 = DynamicMockFailoverable::new(SiteType::Zai, vec!["zai resp".to_string()]);
    let tab1 = DynamicMockFailoverable::new(SiteType::DeepSeek, vec!["ds resp".to_string()]);

    tab0.set_health(SiteHealthStatus::NotLoggedIn);

    let client = FailoverChatClient::new(vec![&tab0, &tab1], 0, 3, 0, 0);

    client.send_message("msg1", 60).await.unwrap();
    assert_eq!(client.current_tab_index(), 1);

    client.start_new_conversation().await.unwrap();
    assert_eq!(tab1.new_conversation_count(), 1);
    assert_eq!(tab0.new_conversation_count(), 0);
}

/// 跨标签页对话轮数累计
#[tokio::test]
async fn test_failover_turn_count_increments() {
    let tab0 = DynamicMockFailoverable::new(SiteType::Zai, vec!["zai resp".to_string()]);

    let client = FailoverChatClient::new(vec![&tab0], 0, 3, 30, 0);

    assert_eq!(client.conversation_turn_count(), 0);
    client.send_message("msg1", 60).await.unwrap();
    assert_eq!(client.conversation_turn_count(), 1);
}

/// 多标签页全不健康 + force_error → 快速失败不挂起
#[tokio::test]
async fn test_all_fail_does_not_hang() {
    let tab0 = DynamicMockFailoverable::new(SiteType::Zai, vec![]);
    let tab1 = DynamicMockFailoverable::new(SiteType::DeepSeek, vec![]);
    let tab2 = DynamicMockFailoverable::new(SiteType::Kimi, vec![]);

    for tab in [&tab0, &tab1, &tab2] {
        tab.set_health(SiteHealthStatus::NotLoggedIn);
        tab.set_force_error(true);
    }

    let client = FailoverChatClient::new(vec![&tab0, &tab1, &tab2], 0, 3, 0, 0);

    // 使用 timeout 确保不挂起
    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(5),
        client.send_message("hello", 60),
    )
    .await;

    assert!(result.is_ok(), "Should not hang");
    assert!(
        result.unwrap().is_err(),
        "All tabs failing should return error"
    );
}
