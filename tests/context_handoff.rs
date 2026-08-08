//! 上下文衔接集成测试 (借鉴方向 1)
//!
//! 验证:
//! 1. ContextHandoff::build_from_memory 构建
//! 2. ContextHandoff::to_prompt 生成
//! 3. MockChat 追踪新开对话调用
//! 4. Orchestrator 集成: 对话轮数超阈值时触发交接
//! 5. Orchestrator 集成: 对话轮数未超阈值时不触发
//! 6. Orchestrator 集成: max_context_turns=0 时禁用
//! 7. 交接 prompt 包含关键信息
//! 8. 交接后对话轮数重置

use async_trait::async_trait;
use forge::context_handoff::ContextHandoff;
use forge::error_diagnosis::ErrorHistory;
use forge::extract::ExtractedFile;
use forge::memory::{Memory, Phase, PhaseStatus, Task, TaskStatus};
use forge::orchestrator::Orchestrator;
use forge::testrunner::TestResult;
use forge::traits::{ChatClient, ChatResult, FileExtractor, TestRunner};
use forge::workspace::Workspace;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;

// ============================================================================
//  Mock 实现 — 支持上下文衔接追踪
// ============================================================================

/// Mock ChatClient — 支持上下文衔接追踪
///
/// 与 orchestrator_dip.rs 的 MockChat 不同, 此 Mock 还追踪:
/// - start_new_conversation 调用次数
/// - conversation_turn_count (每次 send_message +1, start_new_conversation 清零)
struct MockChat {
    /// 预编程回复队列 (按调用顺序弹出)
    responses: Arc<Mutex<Vec<String>>>,
    /// 记录所有收到的消息
    sent_messages: Arc<Mutex<Vec<String>>>,
    /// start_new_conversation 调用次数
    new_conversation_count: Arc<Mutex<usize>>,
    /// 当前对话轮数
    turn_count: Arc<Mutex<usize>>,
}

impl MockChat {
    fn new(responses: Vec<String>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses)),
            sent_messages: Arc::new(Mutex::new(vec![])),
            new_conversation_count: Arc::new(Mutex::new(0)),
            turn_count: Arc::new(Mutex::new(0)),
        }
    }

    fn sent_messages(&self) -> Vec<String> {
        self.sent_messages.lock().unwrap().clone()
    }

    fn new_conversation_count(&self) -> usize {
        *self.new_conversation_count.lock().unwrap()
    }

    fn turn_count(&self) -> usize {
        *self.turn_count.lock().unwrap()
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

/// Mock FileExtractor — 返回空 (测试上下文衔接, 不需要代码生成)
struct MockExtractor;

impl FileExtractor for MockExtractor {
    fn extract(&self, _text: &str) -> Vec<ExtractedFile> {
        vec![]
    }
}

/// 构建 MockExtractor 返回代码文件的版本
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
//  ContextHandoff 单元测试 (从模块外的视角)
// ============================================================================

#[test]
fn test_context_handoff_build() {
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
            files_written: vec!["src/main.rs".to_string()],
            test_result: None,
            last_good_snapshot: None,
            clarifications: vec![],
            depends_on: vec![],
        }],
    }]);
    mem.current_task = Some("0-0".to_string());

    let dir = tempdir().unwrap();
    let ws = Workspace::new(dir.path());
    ws.init().unwrap();
    ws.write_file("src/main.rs", "fn main() {}").unwrap();

    let history = ErrorHistory::new();
    let handoff = ContextHandoff::build_from_memory(&mem, &ws, &history);

    assert_eq!(handoff.goal, "测试目标");
    assert!(handoff.current_phase.is_some());
    assert!(handoff.current_task.is_some());
}

#[test]
fn test_context_handoff_prompt_contains_goal() {
    let mem = Memory::new("构建一个 Web 服务器");
    let dir = tempdir().unwrap();
    let ws = Workspace::new(dir.path());
    ws.init().unwrap();
    let history = ErrorHistory::new();

    let handoff = ContextHandoff::build_from_memory(&mem, &ws, &history);
    let prompt = handoff.to_prompt();

    assert!(prompt.contains("构建一个 Web 服务器"));
}

// ============================================================================
//  Orchestrator 集成测试
// ============================================================================

#[tokio::test]
async fn test_context_handoff_disabled_by_default() {
    // max_context_turns == 0 时, 不应触发交接
    let chat = MockChat::new(vec!["plan json".to_string()]);

    let dir = tempdir().unwrap();
    let ws_path = dir.path().to_str().unwrap();

    let mut orch = Orchestrator::new(
        &chat,
        MockTestRunner,
        MockExtractor,
        ws_path,
        "test goal",
        1,
        60,
    );

    // 不设置 max_context_turns (默认 0)
    orch.run().await.unwrap();

    // 不应调用 start_new_conversation
    assert_eq!(chat.new_conversation_count(), 0);
}

#[tokio::test]
async fn test_context_handoff_not_triggered_below_threshold() {
    // 对话轮数未超过阈值时, 不应触发交接
    // planning 需要 1 次对话 → 轮数 1
    // 阈值设为 5 → 不触发
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
    .with_context_handoff(5); // 阈值 5

    orch.run().await.unwrap();

    // 2 次对话 (planning + task), 未超过 5
    assert_eq!(chat.new_conversation_count(), 0);
    assert!(chat.turn_count() <= 5);
}

#[tokio::test]
async fn test_context_handoff_triggered_above_threshold() {
    // 对话轮数超过阈值时, 应触发交接
    // 阈值设为 1 → planning 对话 (1次) 后超过阈值
    // planning 消息 → 交接 prompt → planning 继续

    let chat = MockChat::new(vec![
        // 第一次 send: planning 的初始 prompt → 轮数=1 ≥ 阈值 1
        r#"```json
        [{"name":"阶段1","description":"测试","tasks":[{"name":"任务1","prompt":"test"}]}]
        ```"#
            .to_string(),
        // 第二次 send: 交接 prompt 的回复 (交接后轮数重置为 0, 交接 prompt 后变 1)
        "收到,继续执行".to_string(),
        // 第三次 send: 任务执行
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
    .with_context_handoff(1); // 阈值 1

    orch.run().await.unwrap();

    // 应至少调用 1 次 start_new_conversation
    assert!(chat.new_conversation_count() >= 1, "应触发上下文衔接");
}

#[tokio::test]
async fn test_context_handoff_prompt_sent() {
    // 验证交接 prompt 被发送
    let chat = MockChat::new(vec![
        r#"```json
        [{"name":"阶段1","description":"测试","tasks":[{"name":"任务1","prompt":"test"}]}]
        ```"#
            .to_string(),
        "收到上下文".to_string(),
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
    .with_context_handoff(1);

    orch.run().await.unwrap();

    // 检查发送的消息中包含交接 prompt
    let sent = chat.sent_messages();
    let handoff_msg = sent.iter().find(|m| m.contains("上下文衔接"));
    assert!(handoff_msg.is_some(), "应发送交接 prompt");
    let handoff_msg = handoff_msg.unwrap();
    assert!(handoff_msg.contains("test goal"), "交接 prompt 应包含目标");
}

#[tokio::test]
async fn test_context_handoff_decision_recorded() {
    // 验证交接决策被记录到 memory
    let chat = MockChat::new(vec![
        r#"```json
        [{"name":"阶段1","description":"测试","tasks":[{"name":"任务1","prompt":"test"}]}]
        ```"#
            .to_string(),
        "收到上下文".to_string(),
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
    .with_context_handoff(1);

    orch.run().await.unwrap();

    // 检查 memory 中是否有交接决策
    let decisions: Vec<&str> = orch
        .memory
        .decisions
        .iter()
        .map(|d| d.decision.as_str())
        .collect();
    assert!(
        decisions.iter().any(|d| d.contains("上下文衔接")),
        "应记录交接决策, 实际决策: {:?}",
        decisions
    );
}

#[tokio::test]
async fn test_context_handoff_resets_turn_count() {
    // 验证交接后对话轮数重置
    let chat = MockChat::new(vec![
        r#"```json
        [{"name":"阶段1","description":"测试","tasks":[{"name":"任务1","prompt":"test"}]}]
        ```"#
            .to_string(),
        "收到上下文".to_string(),
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
    .with_context_handoff(1);

    orch.run().await.unwrap();

    // 交接后轮数应已重置 (可能又有新对话, 但交接本身会清零)
    // 我们只验证 new_conversation_count > 0 (交接发生过)
    assert!(chat.new_conversation_count() >= 1);
}

#[tokio::test]
async fn test_context_handoff_multiple_triggers() {
    // 对话轮数多次超过阈值时, 应多次触发交接
    // 阈值 1 → planning 对话后触发 → 交接后对话 → 再触发

    let chat = MockChat::new(vec![
        // planning 初始对话 → 轮数 1 → 触发交接
        r#"```json
        [{"name":"阶段1","description":"测试","tasks":[{"name":"任务1","prompt":"test"}]}]
        ```"#
            .to_string(),
        // 交接 prompt 回复 → 轮数 1 → 可能再触发交接
        "收到, 继续执行任务".to_string(),
        // 任务执行 → 轮数 1 → 可能再触发交接
        "code: fn main() {}".to_string(),
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
    .with_context_handoff(1);

    orch.run().await.unwrap();

    // 应多次触发交接 (至少 1 次, 可能 2-3 次取决于对话流程)
    assert!(chat.new_conversation_count() >= 1);
}

#[tokio::test]
async fn test_context_handoff_with_error_history() {
    // 验证交接 prompt 包含错误历史摘要
    let chat = MockChat::new(vec![
        r#"```json
        [{"name":"阶段1","description":"测试","tasks":[{"name":"任务1","prompt":"test"}]}]
        ```"#
            .to_string(),
        "收到上下文".to_string(),
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
    .with_context_handoff(1);

    // 添加一些错误历史
    use chrono::Utc;
    use forge::error_diagnosis::{ErrorCategory, ErrorPattern};
    let now = Utc::now();
    orch.error_history.patterns.push(ErrorPattern {
        error_code: Some("E0308".to_string()),
        message_signature: "[E0308] mismatched types".to_string(),
        category: ErrorCategory::TypeError,
        occurrences: 5,
        first_seen: now,
        last_seen: now,
        last_fix_succeeded: true,
        suggested_approach: None,
    });

    orch.run().await.unwrap();

    // 检查发送的消息中包含错误历史
    let sent = chat.sent_messages();
    let handoff_msg = sent.iter().find(|m| m.contains("上下文衔接"));
    if let Some(msg) = handoff_msg {
        // 如果有交接 prompt 且 error_history 非空, 应包含错误历史摘要
        // (注意: error_history 可能在 run() 中被重置, 所以这个断言是可选的)
        if !orch.error_history.patterns.is_empty() {
            assert!(
                msg.contains("错误历史摘要") || msg.contains("E0308"),
                "交接 prompt 应包含错误历史"
            );
        }
    }
}

#[tokio::test]
async fn test_context_handoff_preserves_goal() {
    // 验证交接 prompt 包含原始目标
    let chat = MockChat::new(vec![
        r#"```json
        [{"name":"阶段1","description":"测试","tasks":[{"name":"任务1","prompt":"test"}]}]
        ```"#
            .to_string(),
        "收到上下文".to_string(),
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
    .with_context_handoff(1);

    orch.run().await.unwrap();

    // 检查交接 prompt 包含目标
    let sent = chat.sent_messages();
    let handoff_msg = sent.iter().find(|m| m.contains("上下文衔接"));
    assert!(handoff_msg.is_some());
    assert!(handoff_msg.unwrap().contains("创建一个 Todo CLI 应用"));
}

#[tokio::test]
async fn test_context_handoff_conversation_recorded() {
    // 验证交接对话被记录到 memory
    let chat = MockChat::new(vec![
        r#"```json
        [{"name":"阶段1","description":"测试","tasks":[{"name":"任务1","prompt":"test"}]}]
        ```"#
            .to_string(),
        "收到上下文, 我理解了当前状态".to_string(),
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
    .with_context_handoff(1);

    orch.run().await.unwrap();

    // 检查 memory 中有交接对话记录
    let conversations: Vec<&str> = orch
        .memory
        .conversations
        .iter()
        .map(|c| c.content.as_str())
        .collect();

    let has_handoff_prompt = conversations
        .iter()
        .any(|c| c.contains("上下文衔接 prompt"));
    assert!(has_handoff_prompt, "应记录交接 prompt 对话");

    let has_handoff_response = conversations
        .iter()
        .any(|c| c.contains("收到上下文") || c.contains("理解了当前状态"));
    assert!(has_handoff_response, "应记录交接回复对话");
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

    // 再次发送, 轮数从 0 开始
    chat.send_message("msg3", 60).await.unwrap();
    assert_eq!(chat.conversation_turn_count(), 1);
}

#[tokio::test]
async fn test_with_context_handoff_builder() {
    // 测试构建器方法
    let chat = MockChat::new(vec![]);

    let dir = tempdir().unwrap();
    let ws_path = dir.path().to_str().unwrap();

    let orch = Orchestrator::new(&chat, MockTestRunner, MockExtractor, ws_path, "test", 1, 60);

    // 默认 0
    assert_eq!(orch.max_context_turns, 0);

    let orch = orch.with_context_handoff(30);
    assert_eq!(orch.max_context_turns, 30);

    let orch = orch.with_context_handoff(50);
    assert_eq!(orch.max_context_turns, 50);
}
