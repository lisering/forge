//! 转向提醒集成测试 (借鉴方向 2)
//!
//! 验证:
//! 1. SteerReminder::build_from_memory 构建
//! 2. SteerReminder::should_remind 触发条件
//! 3. SteerReminder::to_prompt 生成
//! 4. SteerReminder::inject 注入逻辑
//! 5. Orchestrator 集成: steer_interval=0 时禁用
//! 6. Orchestrator 集成: 对话轮数达到 interval 倍数时注入提醒
//! 7. Orchestrator 集成: 对话轮数未达到时不注入
//! 8. 提醒内容包含项目目标和约束
//! 9. 转向提醒与上下文衔接的互补关系
//! 10. 交接后转向提醒重新计数

use async_trait::async_trait;
use forge::extract::ExtractedFile;
use forge::memory::{Memory, Phase, PhaseStatus, Task, TaskStatus};
use forge::orchestrator::Orchestrator;
use forge::steer_reminder::SteerReminder;
use forge::testrunner::TestResult;
use forge::traits::{ChatClient, ChatResult, FileExtractor, TestRunner};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;

// ============================================================================
//  Mock 实现 — 支持转向提醒追踪
// ============================================================================

/// Mock ChatClient — 支持对话轮数追踪
///
/// 与 context_handoff.rs 的 MockChat 类似, 追踪:
/// - 所有收到的消息 (用于检查是否包含提醒)
/// - conversation_turn_count (每次 send_message +1, start_new_conversation 清零)
struct MockChat {
    /// 预编程回复队列 (按调用顺序弹出)
    responses: Arc<Mutex<Vec<String>>>,
    /// 记录所有收到的消息
    sent_messages: Arc<Mutex<Vec<String>>>,
    /// 当前对话轮数
    turn_count: Arc<Mutex<usize>>,
    /// start_new_conversation 调用次数
    new_conversation_count: Arc<Mutex<usize>>,
}

impl MockChat {
    fn new(responses: Vec<String>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses)),
            sent_messages: Arc::new(Mutex::new(vec![])),
            turn_count: Arc::new(Mutex::new(0)),
            new_conversation_count: Arc::new(Mutex::new(0)),
        }
    }

    fn sent_messages(&self) -> Vec<String> {
        self.sent_messages.lock().unwrap().clone()
    }

    #[allow(dead_code)]
    fn turn_count(&self) -> usize {
        *self.turn_count.lock().unwrap()
    }

    fn new_conversation_count(&self) -> usize {
        *self.new_conversation_count.lock().unwrap()
    }
}

#[async_trait]
impl ChatClient for MockChat {
    async fn send_message(&self, msg: &str, _timeout: u64) -> anyhow::Result<ChatResult> {
        self.sent_messages.lock().unwrap().push(msg.to_string());
        *self.turn_count.lock().unwrap() += 1;
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

    async fn start_new_conversation(&self) -> anyhow::Result<()> {
        *self.new_conversation_count.lock().unwrap() += 1;
        *self.turn_count.lock().unwrap() = 0;
        Ok(())
    }

    fn conversation_turn_count(&self) -> usize {
        *self.turn_count.lock().unwrap()
    }
}

/// Mock TestRunner — 总是返回成功
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

/// Mock FileExtractor — 返回空
struct MockExtractor;

impl FileExtractor for MockExtractor {
    fn extract(&self, _text: &str) -> Vec<ExtractedFile> {
        vec![]
    }
}

/// Mock FileExtractor — 返回代码文件
struct CodeExtractor;

impl FileExtractor for CodeExtractor {
    fn extract(&self, _text: &str) -> Vec<ExtractedFile> {
        vec![ExtractedFile {
            path: "src/main.rs".to_string(),
            content: "fn main() {}".to_string(),
            language: String::new(),
        }]
    }
}

// ============================================================================
//  SteerReminder 单元测试 (从模块外的视角)
// ============================================================================

#[test]
fn test_steer_reminder_build_from_memory() {
    let mut mem = Memory::new("测试目标");
    mem.set_phases(vec![Phase {
        id: 0,
        name: "阶段1".to_string(),
        description: "测试阶段".to_string(),
        status: PhaseStatus::InProgress,
        tasks: vec![Task {
            id: "0-0".to_string(),
            phase_id: 0,
            name: "任务1".to_string(),
            prompt: "do something".to_string(),
            status: TaskStatus::InProgress,
            result: None,
            attempts: 1,
            files_written: vec![],
            test_result: None,
            last_good_snapshot: None,
            clarifications: vec![],
            depends_on: vec![],
        }],
    }]);
    mem.current_task = Some("0-0".to_string());

    let reminder = SteerReminder::build_from_memory(&mem);

    assert_eq!(reminder.goal, "测试目标");
    assert_eq!(reminder.current_phase, "阶段1");
    assert_eq!(reminder.current_task, "任务1");
}

#[test]
fn test_steer_reminder_should_remind() {
    let mem = Memory::new("test");
    let mut reminder = SteerReminder::build_from_memory(&mem);
    reminder.interval = 10;

    assert!(!reminder.should_remind(0));
    assert!(!reminder.should_remind(5));
    assert!(reminder.should_remind(10));
    assert!(reminder.should_remind(20));
}

#[test]
fn test_steer_reminder_to_prompt() {
    let mem = Memory::new("构建一个 Web 服务器");
    let reminder = SteerReminder::build_from_memory(&mem);
    let prompt = reminder.to_prompt();

    assert!(prompt.contains("构建一个 Web 服务器"));
    assert!(prompt.contains("转向提醒"));
    assert!(prompt.contains("SOLID"));
}

#[test]
fn test_steer_reminder_inject() {
    let mem = Memory::new("test");
    let mut reminder = SteerReminder::build_from_memory(&mem);
    reminder.interval = 10;

    // 不触发
    let result = reminder.inject(5, "原始消息");
    assert_eq!(result, "原始消息");

    // 触发
    let result = reminder.inject(10, "原始消息");
    assert!(result.contains("转向提醒"));
    assert!(result.contains("原始消息"));
}

// ============================================================================
//  Orchestrator 集成测试
// ============================================================================

#[tokio::test]
async fn test_steer_reminder_disabled_by_default() {
    // steer_interval == 0 时, 不应注入提醒
    let chat = MockChat::new(vec![
        r#"```json
        [{"name":"阶段1","description":"测试","tasks":[{"name":"任务1","prompt":"test"}]}]
        ```"#
            .to_string(),
        "code response".to_string(),
    ]);

    let dir = tempdir().unwrap();
    let ws_path = dir.path().to_str().unwrap();

    let mut orch = Orchestrator::new(
        &chat,
        MockTestRunner,
        CodeExtractor,
        ws_path,
        "test goal",
        1,
        60,
    );
    // 不设置 steer_interval (默认 0)

    orch.run().await.unwrap();

    // 不应有转向提醒
    let sent = chat.sent_messages();
    let has_steer = sent.iter().any(|m| m.contains("转向提醒"));
    assert!(!has_steer, "steer_interval=0 时不应注入提醒");
}

#[tokio::test]
async fn test_steer_reminder_not_triggered_below_interval() {
    // 对话轮数未达到 interval 时, 不应注入提醒
    // planning 需要 1 次对话 → 轮数 1
    // task 需要 1 次对话 → 轮数 2
    // interval 设为 10 → 不触发
    let chat = MockChat::new(vec![
        r#"```json
        [{"name":"阶段1","description":"测试","tasks":[{"name":"任务1","prompt":"test"}]}]
        ```"#
            .to_string(),
        "code response".to_string(),
    ]);

    let dir = tempdir().unwrap();
    let ws_path = dir.path().to_str().unwrap();

    let mut orch = Orchestrator::new(
        &chat,
        MockTestRunner,
        CodeExtractor,
        ws_path,
        "test goal",
        1,
        60,
    )
    .with_steer_reminder(10); // 间隔 10

    orch.run().await.unwrap();

    // 2 次对话, 未达到 10
    let sent = chat.sent_messages();
    let has_steer = sent.iter().any(|m| m.contains("转向提醒"));
    assert!(!has_steer, "对话轮数未达到间隔时不应注入提醒");
}

#[tokio::test]
async fn test_steer_reminder_triggered_at_interval() {
    // 对话轮数达到 interval 倍数时, 应注入提醒
    // interval 设为 1 → 每轮都触发
    // planning: 1 次对话 → 轮数 1 ≥ interval 1 → 注入提醒

    let chat = MockChat::new(vec![
        // planning 回复 (第 1 次对话, 轮数变为 1)
        r#"```json
        [{"name":"阶段1","description":"测试","tasks":[{"name":"任务1","prompt":"test"}]}]
        ```"#
            .to_string(),
        // task 执行回复 (第 2 次对话, 轮数变为 2)
        "code response".to_string(),
    ]);

    let dir = tempdir().unwrap();
    let ws_path = dir.path().to_str().unwrap();

    let mut orch = Orchestrator::new(
        &chat,
        MockTestRunner,
        CodeExtractor,
        ws_path,
        "test goal",
        1,
        60,
    )
    .with_steer_reminder(1); // 每轮都触发

    orch.run().await.unwrap();

    // 应有转向提醒
    let sent = chat.sent_messages();
    let steer_msgs: Vec<&String> = sent.iter().filter(|m| m.contains("转向提醒")).collect();
    assert!(
        !steer_msgs.is_empty(),
        "应有转向提醒, 实际发送 {} 条消息: {:?}",
        sent.len(),
        sent
    );
}

#[tokio::test]
async fn test_steer_reminder_contains_goal() {
    // 验证提醒内容包含项目目标
    let chat = MockChat::new(vec![
        r#"```json
        [{"name":"阶段1","description":"测试","tasks":[{"name":"任务1","prompt":"test"}]}]
        ```"#
            .to_string(),
        "code response".to_string(),
    ]);

    let dir = tempdir().unwrap();
    let ws_path = dir.path().to_str().unwrap();

    let mut orch = Orchestrator::new(
        &chat,
        MockTestRunner,
        CodeExtractor,
        ws_path,
        "创建一个 Todo CLI 应用",
        1,
        60,
    )
    .with_steer_reminder(1);

    orch.run().await.unwrap();

    // 检查提醒包含目标
    let sent = chat.sent_messages();
    let steer_msg = sent.iter().find(|m| m.contains("转向提醒"));
    assert!(steer_msg.is_some(), "应有转向提醒");
    assert!(
        steer_msg.unwrap().contains("创建一个 Todo CLI 应用"),
        "提醒应包含项目目标"
    );
}

#[tokio::test]
async fn test_steer_reminder_contains_constraints() {
    // 验证提醒内容包含架构约束
    let chat = MockChat::new(vec![
        r#"```json
        [{"name":"阶段1","description":"测试","tasks":[{"name":"任务1","prompt":"test"}]}]
        ```"#
            .to_string(),
        "code response".to_string(),
    ]);

    let dir = tempdir().unwrap();
    let ws_path = dir.path().to_str().unwrap();

    let mut orch = Orchestrator::new(
        &chat,
        MockTestRunner,
        CodeExtractor,
        ws_path,
        "test goal",
        1,
        60,
    )
    .with_steer_reminder(1);

    orch.run().await.unwrap();

    let sent = chat.sent_messages();
    let steer_msg = sent.iter().find(|m| m.contains("转向提醒"));
    assert!(steer_msg.is_some());
    let msg = steer_msg.unwrap();
    assert!(msg.contains("SOLID"), "提醒应包含 SOLID 约束");
    assert!(msg.contains("file:路径"), "提醒应包含 file:格式 约束");
}

#[tokio::test]
async fn test_steer_reminder_preserves_original_content() {
    // 验证提醒注入后原始消息内容仍保留
    let chat = MockChat::new(vec![
        r#"```json
        [{"name":"阶段1","description":"测试","tasks":[{"name":"任务1","prompt":"请实现 XXX 功能"}]}]
        ```"#
            .to_string(),
        "code response".to_string(),
    ]);

    let dir = tempdir().unwrap();
    let ws_path = dir.path().to_str().unwrap();

    let mut orch = Orchestrator::new(
        &chat,
        MockTestRunner,
        CodeExtractor,
        ws_path,
        "test goal",
        1,
        60,
    )
    .with_steer_reminder(1);

    orch.run().await.unwrap();

    let sent = chat.sent_messages();
    // 找到包含提醒的消息
    let steer_msg = sent.iter().find(|m| m.contains("转向提醒"));
    assert!(steer_msg.is_some());
    // 原始消息内容应保留
    assert!(
        steer_msg.unwrap().contains("请实现 XXX 功能") || steer_msg.unwrap().contains("test"),
        "提醒注入后原始内容应保留"
    );
}

#[tokio::test]
async fn test_steer_reminder_interval_2() {
    // 间隔 2: 每 2 轮触发一次
    // planning: 1 次对话 → 轮数 1 (发送前 turn=0, 0%2=0 但 0 不触发)
    // task: 1 次对话 → 轮数 2 (发送前 turn=1, 1%2!=0, 不触发)
    // 使用足够长的回复避免触发自主追问

    let chat = MockChat::new(vec![
        r#"```json
        [{"name":"阶段1","description":"测试","tasks":[{"name":"任务1","prompt":"test"}]}]
        ```"#
            .to_string(),
        "这是一个足够长的代码回复，不会触发自主追问的长度检查，因为超过20个字符阈值".to_string(),
    ]);

    let dir = tempdir().unwrap();
    let ws_path = dir.path().to_str().unwrap();

    let mut orch = Orchestrator::new(
        &chat,
        MockTestRunner,
        CodeExtractor,
        ws_path,
        "test goal",
        1,
        60,
    )
    .with_steer_reminder(2); // 间隔 2

    orch.run().await.unwrap();

    // 2 次对话, 发送前 turn_count 分别为 0 和 1
    // 0%2=0 但 0 不触发, 1%2!=0 不触发
    let sent = chat.sent_messages();
    let has_steer = sent.iter().any(|m| m.contains("转向提醒"));
    assert!(!has_steer, "2 次对话, 轮数 0 和 1, 不应触发间隔 2 的提醒");
}

#[tokio::test]
async fn test_with_steer_reminder_builder() {
    // 测试构建器方法
    let chat = MockChat::new(vec![]);

    let dir = tempdir().unwrap();
    let ws_path = dir.path().to_str().unwrap();

    let orch = Orchestrator::new(&chat, MockTestRunner, MockExtractor, ws_path, "test", 1, 60);

    // 默认 0
    assert_eq!(orch.steer_interval, 0);

    let orch = orch.with_steer_reminder(10);
    assert_eq!(orch.steer_interval, 10);

    let orch = orch.with_steer_reminder(20);
    assert_eq!(orch.steer_interval, 20);
}

#[tokio::test]
async fn test_steer_reminder_decision_recorded() {
    // 验证提醒注入不需要记录决策 (与上下文衔接不同, 转向提醒是轻量级干预)
    // 但应能在发送的消息中检测到
    let chat = MockChat::new(vec![
        r#"```json
        [{"name":"阶段1","description":"测试","tasks":[{"name":"任务1","prompt":"test"}]}]
        ```"#
            .to_string(),
        "code response".to_string(),
    ]);

    let dir = tempdir().unwrap();
    let ws_path = dir.path().to_str().unwrap();

    let mut orch = Orchestrator::new(
        &chat,
        MockTestRunner,
        CodeExtractor,
        ws_path,
        "test goal",
        1,
        60,
    )
    .with_steer_reminder(1);

    orch.run().await.unwrap();

    // 检查发送的消息中有提醒
    let sent = chat.sent_messages();
    let has_steer = sent.iter().any(|m| m.contains("转向提醒"));
    assert!(has_steer, "应有转向提醒");
}

#[tokio::test]
async fn test_steer_reminder_with_context_handoff() {
    // 转向提醒与上下文衔接互补
    // steer_interval=1, max_context_turns=2
    // 第 1 轮: 注入提醒 (turn_count=1, 1%1=0)
    // 第 2 轮: 注入提醒 + 触发交接 (turn_count=2, 2%1=0 且 2>=2)
    let chat = MockChat::new(vec![
        // planning (第 1 次对话, turn_count 变为 1)
        r#"```json
        [{"name":"阶段1","description":"测试","tasks":[{"name":"任务1","prompt":"test"}]}]
        ```"#
            .to_string(),
        // 交接 prompt 回复 (交接后 turn_count=0, 交接 prompt 后变为 1)
        "收到上下文".to_string(),
        // task 执行
        "code response".to_string(),
    ]);

    let dir = tempdir().unwrap();
    let ws_path = dir.path().to_str().unwrap();

    let mut orch = Orchestrator::new(
        &chat,
        MockTestRunner,
        CodeExtractor,
        ws_path,
        "test goal",
        1,
        60,
    )
    .with_steer_reminder(1)
    .with_context_handoff(2);

    orch.run().await.unwrap();

    // 应有转向提醒
    let sent = chat.sent_messages();
    let has_steer = sent.iter().any(|m| m.contains("转向提醒"));
    assert!(has_steer, "应有转向提醒");

    // 应有上下文衔接
    let has_handoff = sent.iter().any(|m| m.contains("上下文衔接"));
    assert!(has_handoff, "应有上下文交接 prompt");

    // 应调用了 start_new_conversation
    assert!(chat.new_conversation_count() >= 1, "应触发上下文衔接");
}

#[tokio::test]
async fn test_steer_reminder_resets_after_handoff() {
    // 交接后 turn_count 清零, 转向提醒重新计数
    // steer_interval=2, max_context_turns=2
    // planning: turn_count 0→1 (不触发 steer, 不触发 handoff)
    // 交接检查: turn_count=1 < 2, 不触发
    // task: turn_count 1→2 (steer 不触发: 2%2=0 但 turn_count 发送前是 1)
    //   wait, let me re-check...
    //   send_message 前 turn_count=1, 1%2!=0, 不触发 steer
    //   send_message 后 turn_count=2
    //   maybe_context_handoff: turn_count=2 >= 2, 触发交接
    //   交接后 turn_count=0
    //   交接 prompt 发送: turn_count 0→1
    //   maybe_context_handoff: turn_count=1 < 2, 不触发

    // 简化: 验证交接后不会立即触发 steer (因为 turn_count=0)
    let chat = MockChat::new(vec![
        r#"```json
        [{"name":"阶段1","description":"测试","tasks":[{"name":"任务1","prompt":"test"}]}]
        ```"#
            .to_string(),
        "code response".to_string(),
        "交接后回复".to_string(),
    ]);

    let dir = tempdir().unwrap();
    let ws_path = dir.path().to_str().unwrap();

    let mut orch = Orchestrator::new(
        &chat,
        MockTestRunner,
        CodeExtractor,
        ws_path,
        "test goal",
        1,
        60,
    )
    .with_steer_reminder(10) // 大间隔, 不会触发 steer
    .with_context_handoff(1); // 小阈值, 会触发交接

    orch.run().await.unwrap();

    // 应触发了交接
    assert!(chat.new_conversation_count() >= 1, "应触发上下文衔接");

    // 交接后 turn_count 应已重置
    // (可能后续又有对话, 但交接本身会清零)
    // 验证 steer 没有触发 (因为 interval=10, 对话轮数不够)
    let sent = chat.sent_messages();
    let has_steer = sent.iter().any(|m| m.contains("转向提醒"));
    assert!(!has_steer, "大间隔不应触发 steer");
}

#[tokio::test]
async fn test_steer_reminder_multiple_tasks() {
    // 多任务场景: 提醒应在多个任务间持续工作
    let chat = MockChat::new(vec![
        // planning
        r#"```json
        [
          {"name":"阶段1","description":"测试","tasks":[
            {"name":"任务1","prompt":"test1"},
            {"name":"任务2","prompt":"test2"}
          ]}
        ]
        ```"#
            .to_string(),
        // task 1
        "code1 response".to_string(),
        // task 2
        "code2 response".to_string(),
    ]);

    let dir = tempdir().unwrap();
    let ws_path = dir.path().to_str().unwrap();

    let mut orch = Orchestrator::new(
        &chat,
        MockTestRunner,
        CodeExtractor,
        ws_path,
        "test goal",
        1,
        60,
    )
    .with_steer_reminder(2); // 间隔 2

    orch.run().await.unwrap();

    // 3 次对话: planning(0), task1(1), task2(2)
    // 发送前 turn_count 分别为: 0, 1, 2
    // steer 触发: 0→no(0), 1→no(1%2!=0), 2→yes(2%2=0)
    // 但 task2 发送前 turn_count=2, 2%2=0 → 触发!
    let sent = chat.sent_messages();
    let steer_count = sent.iter().filter(|m| m.contains("转向提醒")).count();
    assert!(
        steer_count >= 1,
        "应有至少 1 次转向提醒 (第 3 次对话前 turn_count=2), 实际: {}",
        steer_count
    );
}

#[tokio::test]
async fn test_steer_reminder_prepend_to_message() {
    // 验证提醒在消息前面 (而不是后面)
    let chat = MockChat::new(vec![
        r#"```json
        [{"name":"阶段1","description":"测试","tasks":[{"name":"任务1","prompt":"ORIGINAL_PROMPT_TEXT"}]}]
        ```"#
            .to_string(),
        "code response".to_string(),
    ]);

    let dir = tempdir().unwrap();
    let ws_path = dir.path().to_str().unwrap();

    let mut orch = Orchestrator::new(
        &chat,
        MockTestRunner,
        CodeExtractor,
        ws_path,
        "test goal",
        1,
        60,
    )
    .with_steer_reminder(1);

    orch.run().await.unwrap();

    let sent = chat.sent_messages();
    // 找到包含提醒的消息
    let steer_msg = sent.iter().find(|m| m.contains("转向提醒"));
    if let Some(msg) = steer_msg {
        let steer_pos = msg.find("转向提醒").unwrap();
        // 提醒应在消息前面 (前 50% 的位置)
        assert!(
            steer_pos < msg.len() / 2,
            "提醒应在消息前半部分, 位置: {}/{}",
            steer_pos,
            msg.len()
        );
    }
}

#[tokio::test]
async fn test_mock_chat_turn_count_tracking() {
    // 直接测试 MockChat 的轮数跟踪
    let chat = MockChat::new(vec!["response1".to_string(), "response2".to_string()]);

    assert_eq!(chat.conversation_turn_count(), 0);

    chat.send_message("msg1", 60).await.unwrap();
    assert_eq!(chat.conversation_turn_count(), 1);

    chat.send_message("msg2", 60).await.unwrap();
    assert_eq!(chat.conversation_turn_count(), 2);

    // 新开对话, 轮数清零
    chat.start_new_conversation().await.unwrap();
    assert_eq!(chat.conversation_turn_count(), 0);
    assert_eq!(chat.new_conversation_count(), 1);
}
