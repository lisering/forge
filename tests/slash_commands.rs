//! AI 自主指令 (Slash Commands) 集成测试 (借鉴方向 5)
//!
//! 验证:
//! 1. /skip 指令 → 任务被跳过 (标记为 Failed)
//! 2. /compact 指令 → 触发上下文衔接 (新开对话)
//! 3. /refocus 指令 → 注入转向提醒
//! 4. /retry 指令 → 重置循环终止检测器
//! 5. /escalate 指令 → 触发人工干预
//! 6. 多个指令同时出现 → 逐个执行
//! 7. 禁用 slash commands → 指令被忽略
//! 8. 代码块内的指令不被检测
//! 9. SlashCommand + DevTrace 共存 → trace 记录 SlashCommand 条目
//! 10. 指令不影响正常代码提取

use async_trait::async_trait;
use forge::extract::ExtractedFile;
use forge::orchestrator::Orchestrator;
use forge::slash_command::{self, SlashCommand, SlashCommandAction};
use forge::testrunner::{CompileError, TestResult};
use forge::traits::{ChatClient, ChatResult, FileExtractor, TestRunner};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tempfile::tempdir;

// ============================================================================
//  Mock 实现
// ============================================================================

/// Mock ChatClient — 按顺序返回预编程回复, 记录所有消息, 支持对话轮次计数
struct MockChat {
    responses: Arc<Mutex<Vec<String>>>,
    sent_messages: Arc<Mutex<Vec<String>>>,
    /// 对话轮次计数器
    turn_count: Arc<AtomicUsize>,
    /// 新开对话次数
    new_conversation_count: Arc<AtomicUsize>,
}

impl MockChat {
    fn new(responses: Vec<String>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses)),
            sent_messages: Arc::new(Mutex::new(vec![])),
            turn_count: Arc::new(AtomicUsize::new(0)),
            new_conversation_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    #[allow(dead_code)]
    fn sent_messages(&self) -> Vec<String> {
        self.sent_messages.lock().unwrap().clone()
    }

    fn new_conversation_count(&self) -> usize {
        self.new_conversation_count.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ChatClient for MockChat {
    async fn send_message(&self, msg: &str, _timeout: u64) -> anyhow::Result<ChatResult> {
        self.sent_messages.lock().unwrap().push(msg.to_string());
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

    async fn start_new_conversation(&self) -> anyhow::Result<()> {
        self.new_conversation_count.fetch_add(1, Ordering::SeqCst);
        self.turn_count.store(0, Ordering::SeqCst);
        Ok(())
    }

    fn conversation_turn_count(&self) -> usize {
        self.turn_count.load(Ordering::SeqCst)
    }
}

/// Mock TestRunner — 按顺序返回预编程结果
struct MockTestRunner {
    check_results: Arc<Mutex<Vec<TestResult>>>,
    test_results: Arc<Mutex<Vec<TestResult>>>,
}

impl MockTestRunner {
    fn new() -> Self {
        Self {
            check_results: Arc::new(Mutex::new(vec![])),
            test_results: Arc::new(Mutex::new(vec![])),
        }
    }

    fn with_check_results(mut self, results: Vec<TestResult>) -> Self {
        self.check_results = Arc::new(Mutex::new(results));
        self
    }

    fn with_test_results(mut self, results: Vec<TestResult>) -> Self {
        self.test_results = Arc::new(Mutex::new(results));
        self
    }
}

impl TestRunner for MockTestRunner {
    fn check(&self, _dir: &Path) -> anyhow::Result<TestResult> {
        let mut queue = self.check_results.lock().unwrap();
        if queue.is_empty() {
            return Ok(TestResult {
                success: true,
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 0,
                errors: vec![],
                test_summary: None,
            });
        }
        Ok(queue.remove(0))
    }

    fn test(&self, _dir: &Path) -> anyhow::Result<TestResult> {
        let mut queue = self.test_results.lock().unwrap();
        if queue.is_empty() {
            return Ok(TestResult {
                success: true,
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 0,
                errors: vec![],
                test_summary: None,
            });
        }
        Ok(queue.remove(0))
    }
}

/// Mock FileExtractor — 按顺序返回预编程文件列表
struct MockExtractor {
    file_sets: Arc<Mutex<Vec<Vec<ExtractedFile>>>>,
}

impl MockExtractor {
    fn new(file_sets: Vec<Vec<ExtractedFile>>) -> Self {
        Self {
            file_sets: Arc::new(Mutex::new(file_sets)),
        }
    }
}

impl FileExtractor for MockExtractor {
    fn extract(&self, _text: &str) -> Vec<ExtractedFile> {
        let mut queue = self.file_sets.lock().unwrap();
        if queue.is_empty() {
            return vec![];
        }
        queue.remove(0)
    }
}

// ============================================================================
//  辅助函数
// ============================================================================

fn make_test_result(success: bool, errors: Vec<CompileError>) -> TestResult {
    TestResult {
        success,
        stdout: String::new(),
        stderr: if success {
            String::new()
        } else {
            "compilation failed".to_string()
        },
        exit_code: if success { 0 } else { 1 },
        errors,
        test_summary: None,
    }
}

fn make_compile_error(file: &str, line: u32, msg: &str, code: &str) -> CompileError {
    CompileError {
        file: file.to_string(),
        line: Some(line),
        column: Some(1),
        message: msg.to_string(),
        error_code: Some(code.to_string()),
    }
}

fn ef(path: &str, content: &str) -> ExtractedFile {
    ExtractedFile {
        path: path.to_string(),
        content: content.to_string(),
        language: String::new(),
    }
}

/// 生成一个带 /skip 指令的代码回复
fn skip_response() -> String {
    "这个任务无法完成, 因为缺少必要的依赖。\n\n/skip\n\n```file:src/main.rs\nfn main() {}\n```"
        .to_string()
}

/// 生成一个带 /compact 指令的代码回复
fn compact_response() -> String {
    "代码已写完, 但上下文太长了。\n\n/compact\n\n```file:src/main.rs\nfn main() { println!(\"hello\"); }\n```"
        .to_string()
}

/// 生成一个带 /refocus 指令的代码回复
fn refocus_response() -> String {
    "已完成, 但需要重新聚焦。\n\n/refocus\n\n```file:src/main.rs\nfn main() { println!(\"hello\"); }\n```"
        .to_string()
}

/// 生成一个带 /retry 指令的代码回复
fn retry_response() -> String {
    "需要用不同方法重试。\n\n/retry\n\n```file:src/main.rs\nfn main() { println!(\"hello\"); }\n```"
        .to_string()
}

/// 生成一个普通成功代码回复
fn success_code_response() -> String {
    "以下是完整的代码实现，已通过所有测试验证。\n```file:src/main.rs\nfn main() { println!(\"hello\"); }\n```"
        .to_string()
}

/// 生成 planning 阶段回复
fn planning_response() -> String {
    r#"```json
[{"name":"阶段1","description":"d","tasks":[{"name":"任务1","prompt":"do it"}]}]
```"#
        .to_string()
}

// ============================================================================
//  测试用例
// ============================================================================

/// 测试 1: /skip 指令 → 任务被跳过
#[tokio::test]
async fn test_skip_command_skips_task() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![planning_response(), skip_response()]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "测试目标", 3, 60)
        .with_slash_commands(true);

    orch.run().await.unwrap();

    // 任务应被标记为 Failed (因为 /skip)
    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(
        task.status,
        forge::memory::TaskStatus::Failed,
        "/skip 指令应导致任务被标记为 Failed"
    );

    // 决策中应记录 /skip
    let decisions: Vec<_> = orch
        .memory
        .decisions
        .iter()
        .filter(|d| d.decision.contains("/skip"))
        .collect();
    assert!(!decisions.is_empty(), "应有 /skip 决策记录");
}

/// 测试 2: /compact 指令 → 触发上下文衔接 (新开对话)
#[tokio::test]
async fn test_compact_command_triggers_handoff() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        planning_response(),
        compact_response(),
        // 上下文衔接后 AI 的回复
        "交接完成, 继续开发。".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "测试目标", 3, 60)
        .with_slash_commands(true)
        .with_context_handoff(100); // 设置高阈值, 确保是 /compact 触发而非自动

    orch.run().await.unwrap();

    // 应触发新开对话
    assert!(
        chat.new_conversation_count() > 0,
        "/compact 应触发新开对话 (上下文衔接)"
    );

    // 决策中应记录 /compact
    let decisions: Vec<_> = orch
        .memory
        .decisions
        .iter()
        .filter(|d| d.decision.contains("/compact"))
        .collect();
    assert!(!decisions.is_empty(), "应有 /compact 决策记录");
}

/// 测试 3: /refocus 指令 → 注入转向提醒
#[tokio::test]
async fn test_refocus_command_injects_reminder() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        planning_response(),
        refocus_response(),
        // refocus 发送转向提醒后的 AI 回复
        "收到提醒, 继续聚焦。".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "测试目标", 3, 60)
        .with_slash_commands(true);

    orch.run().await.unwrap();

    // 决策中应记录 /refocus
    let decisions: Vec<_> = orch
        .memory
        .decisions
        .iter()
        .filter(|d| d.decision.contains("/refocus"))
        .collect();
    assert!(!decisions.is_empty(), "应有 /refocus 决策记录");

    // 应有多条发送消息 (包括 refocus 的转向提醒)
    let sent = chat.sent_messages();
    assert!(
        sent.len() >= 3,
        "应至少发送 3 条消息 (planning + task + refocus)"
    );
}

/// 测试 4: /retry 指令 → 重置循环终止检测器
#[tokio::test]
async fn test_retry_command_resets_loop_detector() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![planning_response(), retry_response()]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "测试目标", 3, 60)
        .with_slash_commands(true)
        .with_loop_detection(3); // 启用循环终止检测

    orch.run().await.unwrap();

    // 决策中应记录 /retry
    let decisions: Vec<_> = orch
        .memory
        .decisions
        .iter()
        .filter(|d| d.decision.contains("/retry"))
        .collect();
    assert!(!decisions.is_empty(), "应有 /retry 决策记录");
}

/// 测试 5: 禁用 slash commands → 指令被忽略
#[tokio::test]
async fn test_disabled_slash_commands_ignored() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        planning_response(),
        skip_response(), // 包含 /skip 但应被忽略
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "测试目标", 3, 60)
        .with_slash_commands(false); // 禁用

    orch.run().await.unwrap();

    // 任务应完成 (不是 Failed), 因为 /skip 被忽略
    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(
        task.status,
        forge::memory::TaskStatus::Completed,
        "禁用 slash commands 时 /skip 应被忽略, 任务正常完成"
    );

    // 不应有 /skip 决策
    let decisions: Vec<_> = orch
        .memory
        .decisions
        .iter()
        .filter(|d| d.decision.contains("/skip"))
        .collect();
    assert!(decisions.is_empty(), "不应有 /skip 决策记录");
}

/// 测试 6: 代码块内的指令不被检测
#[tokio::test]
async fn test_commands_in_code_blocks_ignored() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    // /skip 在代码块内 → 不应被检测
    let response =
        "代码写好了\n```\n/skip\n```\n```file:src/main.rs\nfn main() { println!(\"hello\"); }\n```"
            .to_string();

    let chat = MockChat::new(vec![planning_response(), response]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "测试目标", 3, 60)
        .with_slash_commands(true);

    orch.run().await.unwrap();

    // 任务应完成 (代码块内的 /skip 被忽略)
    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(
        task.status,
        forge::memory::TaskStatus::Completed,
        "代码块内的 /skip 应被忽略"
    );
}

/// 测试 7: 多个指令同时出现
#[tokio::test]
async fn test_multiple_commands() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    // /retry + /refocus + /skip → /skip 最后执行, 任务被跳过
    let response =
        "需要调整策略。\n\n/retry\n/refocus\n/skip\n\n```file:src/main.rs\nfn main() {}\n```"
            .to_string();

    let chat = MockChat::new(vec![
        planning_response(),
        response,
        // refocus 发送转向提醒后的回复
        "收到提醒。".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "测试目标", 3, 60)
        .with_slash_commands(true)
        .with_loop_detection(3);

    orch.run().await.unwrap();

    // 应有所有三个指令的决策记录
    for cmd in &["retry", "refocus", "skip"] {
        let decisions: Vec<_> = orch
            .memory
            .decisions
            .iter()
            .filter(|d| d.decision.contains(cmd))
            .collect();
        assert!(!decisions.is_empty(), "应有 /{} 决策记录", cmd);
    }

    // /skip 最终导致任务失败
    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(
        task.status,
        forge::memory::TaskStatus::Failed,
        "/skip 最终应导致任务被跳过"
    );
}

/// 测试 8: SlashCommand + DevTrace 共存
#[tokio::test]
async fn test_slash_command_with_dev_trace() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![planning_response(), skip_response()]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "测试目标", 3, 60)
        .with_slash_commands(true)
        .with_dev_trace(true);

    orch.run().await.unwrap();

    // DevTrace 文件应存在
    let trace_path = dir.path().join(".forge").join("devtrace.jsonl");
    assert!(trace_path.exists(), "DevTrace 文件应存在");

    // 读取 trace 条目
    let trace_writer = forge::dev_trace::DevTraceWriter::new(dir.path());
    let entries = trace_writer.read_all().unwrap();

    // 应包含 SlashCommand trace 条目
    let slash_entries: Vec<_> = entries
        .iter()
        .filter(|e| e.action == forge::dev_trace::TraceAction::SlashCommand)
        .collect();
    assert!(!slash_entries.is_empty(), "应包含 SlashCommand trace 条目");
}

/// 测试 9: 无指令时正常完成
#[tokio::test]
async fn test_no_commands_normal_completion() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![planning_response(), success_code_response()]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "测试目标", 3, 60)
        .with_slash_commands(true);

    orch.run().await.unwrap();

    // 任务应正常完成
    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(
        task.status,
        forge::memory::TaskStatus::Completed,
        "无指令时任务应正常完成"
    );
}

/// 测试 10: /compact 与循环终止检测共存
#[tokio::test]
async fn test_compact_with_loop_detection() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        planning_response(),
        compact_response(),
        "交接完成。".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "测试目标", 3, 60)
        .with_slash_commands(true)
        .with_loop_detection(3)
        .with_context_handoff(100);

    orch.run().await.unwrap();

    // 两者都应有决策记录
    assert!(chat.new_conversation_count() > 0, "/compact 应触发新开对话");
}

/// 测试 11: SlashCommand 解析器基础测试 (集成层面验证)
#[test]
fn test_parse_from_ai_response() {
    let response = "代码完成\n/skip\n/compact\n```\n/refocus\n```";
    let cmds = slash_command::parse_from_response(response);
    assert_eq!(cmds.len(), 2);
    assert!(cmds.contains(&SlashCommand::Skip));
    assert!(cmds.contains(&SlashCommand::Compact));
    // /refocus 在代码块内, 不应被检测
    assert!(!cmds.contains(&SlashCommand::Refocus));
}

/// 测试 12: strip_commands 清理后不影响代码提取
#[test]
fn test_strip_commands_preserves_code() {
    let response = "代码完成\n/skip\n```file:src/main.rs\nfn main() { println!(\"hello\"); }\n```";
    let stripped = slash_command::strip_commands(response);
    assert!(!stripped.contains("/skip"));
    assert!(stripped.contains("fn main()"));
    assert!(stripped.contains("```file:src/main.rs"));
}

/// 测试 13: /skip 在修复轮中也生效
#[tokio::test]
async fn test_skip_in_fix_round() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let same_error = vec![make_compile_error(
        "src/main.rs",
        10,
        "type mismatch",
        "E0308",
    )];

    let chat = MockChat::new(vec![
        planning_response(),
        // attempt 1: 正常代码
        success_code_response(),
        // attempt 2: 修复中发出 /skip
        skip_response(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            make_test_result(false, same_error.clone()), // attempt 1 编译失败
            make_test_result(true, vec![]),              // attempt 2 编译成功
        ])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/main.rs", "fn main() {}")],
    ]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "测试目标", 5, 60)
        .with_slash_commands(true);

    orch.run().await.unwrap();

    // 任务应被跳过 (Failed)
    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(
        task.status,
        forge::memory::TaskStatus::Failed,
        "修复轮中的 /skip 应导致任务被跳过"
    );
}

/// 测试 14: SlashCommandAction 枚举行为
#[test]
fn test_slash_command_action() {
    assert!(!SlashCommandAction::Continue.should_skip());
    assert!(SlashCommandAction::SkipTask.should_skip());
}

/// 测试 15: 所有已知指令解析
#[test]
fn test_all_known_commands_parsed() {
    let response = "/compact\n/skip\n/refocus\n/retry\n/escalate";
    let cmds = slash_command::parse_from_response(response);
    assert_eq!(cmds.len(), 5);
    for cmd in SlashCommand::all_known() {
        assert!(cmds.contains(&cmd), "应包含指令: {:?}", cmd);
    }
}
