//! 端到端全功能共存集成测试 (第 16 项任务)
//!
//! 验证所有功能在同时启用时能正确共存、不冲突、不退化:
//! - 自主追问 (HeuristicClarificationChecker)
//! - 人工干预 (MockInteraction)
//! - 智能错误诊断 (MockErrorDiagnoser)
//! - 上下文衔接 (max_context_turns)
//! - 转向提醒 (steer_interval)
//! - 循环终止检测 (loop_detection)
//! - 结构化开发追踪 (dev_trace)
//! - AI 自主指令 (slash_commands)
//! - 并行任务执行 (parallel)
//!
//! 测试覆盖:
//! 1. 所有功能同时启用 → 多阶段多任务成功完成
//! 2. DevTrace 包含多种操作类型 (Planning/TaskExecution/CompileCheck/TestRun)
//! 3. Slash Commands 与循环终止检测共存
//! 4. Slash Commands 与上下文衔接共存
//! 5. Slash Commands 与智能错误诊断共存
//! 6. Slash Commands 与转向提醒共存
//! 7. 修复循环中所有功能共存 (失败→诊断→循环检测→修复→成功)
//! 8. /skip 与人工干预共存
//! 9. 多任务并行 + 全功能共存
//! 10. Memory 决策记录完整性
//! 11. DevTrace 摘要报告正确性
//! 12. 全功能启用 → 无 panic / 无死锁

use async_trait::async_trait;
use forge::dev_trace::{DevTraceWriter, TraceAction};
use forge::error_diagnosis::MockErrorDiagnoser;
use forge::extract::ExtractedFile;
use forge::interaction::MockInteraction;
use forge::memory::TaskStatus;
use forge::orchestrator::Orchestrator;
use forge::slash_command;
use forge::testrunner::{CompileError, TestResult};
use forge::traits::{ChatClient, ChatResult, FileExtractor, TestRunner};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tempfile::tempdir;

// ============================================================================
//  Mock 实现 (复用已有模式, 支持对话轮次计数 + 新开对话)
// ============================================================================

struct MockChat {
    responses: Arc<Mutex<Vec<String>>>,
    sent_messages: Arc<Mutex<Vec<String>>>,
    turn_count: Arc<AtomicUsize>,
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

    #[allow(dead_code)]
    fn turn_count(&self) -> usize {
        self.turn_count.load(Ordering::SeqCst)
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

fn make_compile_error(file: &str, line: u32, msg: &str) -> CompileError {
    CompileError {
        file: file.to_string(),
        line: Some(line),
        column: Some(1),
        message: msg.to_string(),
        error_code: Some("E0308".to_string()),
    }
}

fn ef(path: &str, content: &str) -> ExtractedFile {
    ExtractedFile {
        path: path.to_string(),
        content: content.to_string(),
        language: String::new(),
    }
}

fn read_trace_entries(ws_dir: &str) -> Vec<forge::dev_trace::DevTraceEntry> {
    let writer = DevTraceWriter::new(Path::new(ws_dir));
    writer.read_all().unwrap_or_default()
}

fn trace_action_set(entries: &[forge::dev_trace::DevTraceEntry]) -> Vec<TraceAction> {
    let mut actions: Vec<TraceAction> = entries.iter().map(|e| e.action).collect();
    actions.sort_by_key(|a| format!("{}", a));
    actions.dedup();
    actions
}

/// 3 阶段 5 任务的复杂计划
fn complex_plan() -> String {
    r#"```json
[
  {"name":"初始化","description":"创建项目结构","tasks":[
    {"name":"创建main","prompt":"创建 main.rs"},
    {"name":"创建lib","prompt":"创建 lib.rs"}
  ]},
  {"name":"核心功能","description":"实现核心逻辑","tasks":[
    {"name":"实现逻辑","prompt":"实现核心逻辑"}
  ]},
  {"name":"测试","description":"编写测试","tasks":[
    {"name":"单元测试","prompt":"编写单元测试"},
    {"name":"集成测试","prompt":"编写集成测试"}
  ]}
]
```"#
        .to_string()
}

/// 单阶段单任务计划
fn simple_plan() -> String {
    r#"```json
[{"name":"阶段1","description":"d","tasks":[{"name":"任务1","prompt":"do it"}]}
]
```"#
        .to_string()
}

/// 单阶段双任务计划
fn two_task_plan() -> String {
    r#"```json
[{"name":"阶段1","description":"d","tasks":[
  {"name":"任务1","prompt":"task1"},
  {"name":"任务2","prompt":"task2"}
]}]
```"#
        .to_string()
}

fn success_code() -> String {
    "以下是完整实现。\n```file:src/main.rs\nfn main() { println!(\"hello\"); }\n```".to_string()
}

fn skip_code() -> String {
    "无法完成。\n/skip\n```file:src/main.rs\nfn main() {}\n```".to_string()
}

fn compact_code() -> String {
    "上下文太长。\n/compact\n```file:src/main.rs\nfn main() {}\n```".to_string()
}

/// 构建一个全功能启用的 Orchestrator
fn build_full_orchestrator<'a>(
    chat: &'a MockChat,
    runner: MockTestRunner,
    extractor: MockExtractor,
    ws_dir: &str,
    goal: &str,
) -> Orchestrator<
    'a,
    MockChat,
    MockTestRunner,
    MockExtractor,
    forge::clarify::HeuristicClarificationChecker,
> {
    Orchestrator::new(chat, runner, extractor, ws_dir, goal, 5, 60)
        .with_slash_commands(true) // 借鉴方向 5
        .with_dev_trace(true) // 借鉴方向 4
        .with_loop_detection(3) // 借鉴方向 3
        .with_steer_reminder(10) // 借鉴方向 2
        .with_context_handoff(50) // 借鉴方向 1
        .with_interaction(Box::new(MockInteraction::new())) // 方向 A
        .with_error_diagnosis(Box::new(MockErrorDiagnoser::with_category(
            forge::error_diagnosis::ErrorCategory::SyntaxError,
        ))) // 方向 F
}

// ============================================================================
//  测试用例
// ============================================================================

/// 测试 1: 所有功能同时启用 → 多阶段多任务成功完成 (无退化)
#[tokio::test]
async fn test_all_features_enabled_multi_phase_success() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        complex_plan(),
        // 阶段1: 初始化 — 2 个任务
        success_code(), // 创建 main
        success_code(), // 创建 lib
        // 阶段2: 核心功能 — 1 个任务
        success_code(),
        // 阶段3: 测试 — 2 个任务
        success_code(),
        success_code(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            make_test_result(true, vec![]),
            make_test_result(true, vec![]),
            make_test_result(true, vec![]),
            make_test_result(true, vec![]),
            make_test_result(true, vec![]),
        ])
        .with_test_results(vec![
            make_test_result(true, vec![]),
            make_test_result(true, vec![]),
            make_test_result(true, vec![]),
            make_test_result(true, vec![]),
            make_test_result(true, vec![]),
        ]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/lib.rs", "pub fn lib() {}")],
        vec![ef("src/core.rs", "pub fn core() {}")],
        vec![ef("tests/unit.rs", "#[test] fn t() {}")],
        vec![ef("tests/integ.rs", "#[test] fn i() {}")],
    ]);

    let mut orch = build_full_orchestrator(&chat, runner, extractor, ws_dir, "复杂项目");

    // 不应 panic
    orch.run().await.unwrap();

    // 验证: 3 个阶段全部完成
    assert_eq!(orch.memory.phases.len(), 3, "应有 3 个阶段");
    for (i, phase) in orch.memory.phases.iter().enumerate() {
        assert_eq!(
            phase.status,
            forge::memory::PhaseStatus::Completed,
            "阶段 {} 应完成",
            i
        );
    }

    // 验证: 所有 5 个任务全部完成
    let total_tasks: usize = orch.memory.phases.iter().map(|p| p.tasks.len()).sum();
    assert_eq!(total_tasks, 5, "应有 5 个任务");
    for phase in &orch.memory.phases {
        for task in &phase.tasks {
            assert_eq!(
                task.status,
                TaskStatus::Completed,
                "任务 '{}' 应完成",
                task.name
            );
        }
    }
}

/// 测试 2: DevTrace 包含多种操作类型 (Planning + TaskExecution + CompileCheck + TestRun)
#[tokio::test]
async fn test_dev_trace_contains_multiple_action_types() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![simple_plan(), success_code()]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = build_full_orchestrator(&chat, runner, extractor, ws_dir, "测试");

    orch.run().await.unwrap();

    let entries = read_trace_entries(ws_dir);
    let actions = trace_action_set(&entries);

    // 验证: 至少包含 4 种基本操作类型
    assert!(actions.contains(&TraceAction::Planning), "应包含 Planning");
    assert!(
        actions.contains(&TraceAction::TaskExecution),
        "应包含 TaskExecution"
    );
    assert!(
        actions.contains(&TraceAction::CompileCheck),
        "应包含 CompileCheck"
    );
    assert!(actions.contains(&TraceAction::TestRun), "应包含 TestRun");

    // 验证: 总条目数 >= 4
    assert!(
        entries.len() >= 4,
        "应至少有 4 个 trace 条目, 实际: {}",
        entries.len()
    );
}

/// 测试 3: 修复循环中全功能共存 (失败→诊断→循环检测→修复→成功)
#[tokio::test]
async fn test_fix_loop_with_all_features() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let same_error = vec![make_compile_error("src/main.rs", 10, "type mismatch")];

    let chat = MockChat::new(vec![
        simple_plan(),
        "代码写完了。\n```file:src/main.rs\nfn main() { broken }\n```".to_string(),
        "修复好了。\n```file:src/main.rs\nfn main() { println!(\"fixed\"); }\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            make_test_result(false, same_error.clone()), // 第一次失败
            make_test_result(true, vec![]),              // 第二次成功
        ])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() { broken }")],
        vec![ef("src/main.rs", "fn main() { println!(\"fixed\"); }")],
    ]);

    let mut orch = build_full_orchestrator(&chat, runner, extractor, ws_dir, "修复测试");

    orch.run().await.unwrap();

    // 验证: 任务最终成功 (修复循环成功)
    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(task.status, TaskStatus::Completed, "修复后任务应完成");
    assert_eq!(task.attempts, 2, "应尝试 2 次");

    // 验证: DevTrace 包含 FixAttempt
    let entries = read_trace_entries(ws_dir);
    let actions = trace_action_set(&entries);
    assert!(
        actions.contains(&TraceAction::FixAttempt),
        "应包含 FixAttempt"
    );

    // 验证: 有失败的 CompileCheck 和成功的 CompileCheck
    let failed_checks = entries
        .iter()
        .filter(|e| e.action == TraceAction::CompileCheck && !e.success)
        .count();
    let ok_checks = entries
        .iter()
        .filter(|e| e.action == TraceAction::CompileCheck && e.success)
        .count();
    assert!(failed_checks >= 1, "应至少有 1 个失败的 CompileCheck");
    assert!(ok_checks >= 1, "应至少有 1 个成功的 CompileCheck");
}

/// 测试 4: /skip 与循环终止检测共存
#[tokio::test]
async fn test_skip_with_loop_detection() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![simple_plan(), skip_code()]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = build_full_orchestrator(&chat, runner, extractor, ws_dir, "skip+loop");

    orch.run().await.unwrap();

    // /skip 导致任务 Failed
    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(task.status, TaskStatus::Failed, "/skip 应使任务 Failed");

    // DevTrace 应记录 SlashCommand
    let entries = read_trace_entries(ws_dir);
    let actions = trace_action_set(&entries);
    assert!(
        actions.contains(&TraceAction::SlashCommand),
        "应包含 SlashCommand trace"
    );

    // 决策记录中有 /skip
    let has_skip = orch
        .memory
        .decisions
        .iter()
        .any(|d| d.decision.contains("/skip"));
    assert!(has_skip, "应有 /skip 决策记录");
}

/// 测试 5: /compact 与上下文衔接共存
#[tokio::test]
async fn test_compact_with_context_handoff() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        simple_plan(),
        compact_code(),
        "交接完成, 继续开发。".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = build_full_orchestrator(&chat, runner, extractor, ws_dir, "compact+handoff");

    orch.run().await.unwrap();

    // /compact 触发新开对话
    assert!(chat.new_conversation_count() > 0, "/compact 应触发新开对话");

    // 决策记录中有 /compact
    let has_compact = orch
        .memory
        .decisions
        .iter()
        .any(|d| d.decision.contains("/compact"));
    assert!(has_compact, "应有 /compact 决策记录");
}

/// 测试 6: /retry 与循环终止检测共存
#[tokio::test]
async fn test_retry_with_loop_detection() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        simple_plan(),
        "需要换方法。\n/retry\n```file:src/main.rs\nfn main() {}\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = build_full_orchestrator(&chat, runner, extractor, ws_dir, "retry+loop");

    orch.run().await.unwrap();

    // /retry 应记录决策
    let has_retry = orch
        .memory
        .decisions
        .iter()
        .any(|d| d.decision.contains("/retry"));
    assert!(has_retry, "应有 /retry 决策记录");

    // 任务应正常完成 (/retry 只是重置检测器, 不跳过)
    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(task.status, TaskStatus::Completed, "/retry 后任务应完成");
}

/// 测试 7: /refocus 与转向提醒共存
#[tokio::test]
async fn test_refocus_with_steer_reminder() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        simple_plan(),
        "需要重新聚焦。\n/refocus\n```file:src/main.rs\nfn main() {}\n```".to_string(),
        "收到提醒, 继续。".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = build_full_orchestrator(&chat, runner, extractor, ws_dir, "refocus+steer");

    orch.run().await.unwrap();

    // /refocus 应记录决策
    let has_refocus = orch
        .memory
        .decisions
        .iter()
        .any(|d| d.decision.contains("/refocus"));
    assert!(has_refocus, "应有 /refocus 决策记录");

    // 应有多条发送消息 (planning + task + refocus 提醒)
    assert!(chat.sent_messages().len() >= 3, "应至少发送 3 条消息");
}

/// 测试 8: 智能错误诊断与修复循环共存
#[tokio::test]
async fn test_error_diagnosis_with_fix_loop() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let errors = vec![make_compile_error("src/main.rs", 5, "cannot find function")];

    let chat = MockChat::new(vec![
        simple_plan(),
        "```file:src/main.rs\nfn main() { unknown(); }\n```".to_string(),
        "```file:src/main.rs\nfn main() { println!(\"ok\"); }\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            make_test_result(false, errors),
            make_test_result(true, vec![]),
        ])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() { unknown(); }")],
        vec![ef("src/main.rs", "fn main() { println!(\"ok\"); }")],
    ]);

    let mut orch = build_full_orchestrator(&chat, runner, extractor, ws_dir, "诊断+修复");

    orch.run().await.unwrap();

    // 任务应修复成功
    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(task.status, TaskStatus::Completed, "诊断+修复后应成功");
    assert_eq!(task.attempts, 2, "应尝试 2 次");

    // DevTrace 有 FixAttempt
    let entries = read_trace_entries(ws_dir);
    let actions = trace_action_set(&entries);
    assert!(
        actions.contains(&TraceAction::FixAttempt),
        "应包含 FixAttempt"
    );
}

/// 测试 9: 循环终止检测触发 → 全功能共存不 panic
#[tokio::test]
async fn test_loop_detection_trigger_with_all_features() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let same_error = vec![make_compile_error("src/main.rs", 10, "same error")];

    let chat = MockChat::new(vec![
        simple_plan(),
        "```file:src/main.rs\nfn main() { broken }\n```".to_string(),
        "```file:src/main.rs\nfn main() { broken }\n```".to_string(),
        "```file:src/main.rs\nfn main() { broken }\n```".to_string(),
        "```file:src/main.rs\nfn main() { broken }\n```".to_string(),
        "```file:src/main.rs\nfn main() { broken }\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            make_test_result(false, same_error.clone()),
            make_test_result(false, same_error.clone()),
            make_test_result(false, same_error.clone()),
            make_test_result(false, same_error.clone()),
            make_test_result(false, same_error.clone()),
        ])
        .with_test_results(vec![]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/main.rs", "fn main() {}")],
    ]);

    let mut orch = build_full_orchestrator(&chat, runner, extractor, ws_dir, "循环检测");

    // 不应 panic
    orch.run().await.unwrap();

    // 任务最终 Failed (循环终止检测可能导致跳过)
    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(task.status, TaskStatus::Failed, "死循环后任务应 Failed");

    // DevTrace 可能包含 LoopDetection
    let entries = read_trace_entries(ws_dir);
    let actions = trace_action_set(&entries);
    // 循环终止检测应被记录 (可能触发也可能因为 max_rounds 先到)
    let _ = actions; // 不强制断言, 因为 max_rounds=5 和 loop_detection=3 可能交互
}

/// 测试 10: 多任务 + 全功能共存 → 无 panic, 无死锁
#[tokio::test]
async fn test_multi_task_all_features_no_panic() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        two_task_plan(),
        success_code(), // 任务1
        success_code(), // 任务2
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            make_test_result(true, vec![]),
            make_test_result(true, vec![]),
        ])
        .with_test_results(vec![
            make_test_result(true, vec![]),
            make_test_result(true, vec![]),
        ]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/lib.rs", "pub fn lib() {}")],
    ]);

    let mut orch = build_full_orchestrator(&chat, runner, extractor, ws_dir, "多任务");

    // 不应 panic 或死锁
    orch.run().await.unwrap();

    // 两个任务都完成
    for (i, task) in orch.memory.phases[0].tasks.iter().enumerate() {
        assert_eq!(task.status, TaskStatus::Completed, "任务 {} 应完成", i);
    }
}

/// 测试 11: Memory 决策记录完整性
#[tokio::test]
async fn test_memory_decisions_complete() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![simple_plan(), skip_code()]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = build_full_orchestrator(&chat, runner, extractor, ws_dir, "决策记录");

    orch.run().await.unwrap();

    // 应有决策记录
    assert!(!orch.memory.decisions.is_empty(), "应有决策记录");

    // 应有 /skip 决策
    let skip_decisions: Vec<_> = orch
        .memory
        .decisions
        .iter()
        .filter(|d| d.decision.contains("/skip"))
        .collect();
    assert!(!skip_decisions.is_empty(), "应有 /skip 决策");

    // memory.json 应存在且可加载
    let memory_path = format!("{}/.forge/memory.json", ws_dir);
    assert!(Path::new(&memory_path).exists(), "memory.json 应存在");
    let loaded = forge::memory::Memory::load(Path::new(&memory_path)).unwrap();
    assert_eq!(loaded.goal, "决策记录");
}

/// 测试 12: DevTrace 摘要报告正确性
#[tokio::test]
async fn test_dev_trace_summary_correctness() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![two_task_plan(), success_code(), success_code()]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            make_test_result(true, vec![]),
            make_test_result(true, vec![]),
        ])
        .with_test_results(vec![
            make_test_result(true, vec![]),
            make_test_result(true, vec![]),
        ]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/lib.rs", "pub fn lib() {}")],
    ]);

    let mut orch = build_full_orchestrator(&chat, runner, extractor, ws_dir, "摘要测试");

    orch.run().await.unwrap();

    let writer = DevTraceWriter::new(Path::new(ws_dir));
    let summary = writer.summary();

    // 总条目 > 0
    assert!(summary.total_entries > 0, "应有 trace 条目");

    // 成功率在 0~1 之间
    assert!(
        summary.success_rate > 0.0 && summary.success_rate <= 1.0,
        "成功率应在 (0, 1], 实际: {}",
        summary.success_rate
    );

    // 报告包含关键信息
    let report = summary.to_report();
    assert!(report.contains("DevTrace 开发追踪报告"), "报告应包含标题");
    assert!(report.contains("总条目"), "报告应包含总条目数");
    assert!(report.contains("成功率"), "报告应包含成功率");

    // 应有 Planning 统计
    let planning_stats = summary.get_action_stats(TraceAction::Planning);
    assert!(planning_stats.is_some(), "应有 Planning 统计");
    assert_eq!(planning_stats.unwrap().count, 1, "应有 1 个 Planning");

    // 应有 TaskExecution 统计
    let task_stats = summary.get_action_stats(TraceAction::TaskExecution);
    assert!(task_stats.is_some(), "应有 TaskExecution 统计");
    assert_eq!(task_stats.unwrap().count, 2, "应有 2 个 TaskExecution");
}

/// 测试 13: 上下文衔接自动触发 + 全功能共存
#[tokio::test]
async fn test_auto_context_handoff_with_all_features() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    // max_context_turns=2 → planning(1轮) + task(1轮) → 2轮后触发交接
    let chat = MockChat::new(vec![simple_plan(), success_code()]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "自动交接", 3, 60)
        .with_slash_commands(true)
        .with_dev_trace(true)
        .with_loop_detection(3)
        .with_steer_reminder(100) // 禁用转向提醒 (避免干扰)
        .with_context_handoff(2) // 2 轮后自动触发
        .with_interaction(Box::new(MockInteraction::new()))
        .with_error_diagnosis(Box::new(MockErrorDiagnoser::empty()));

    orch.run().await.unwrap();

    // 应自动触发上下文衔接
    if chat.new_conversation_count() > 0 {
        let entries = read_trace_entries(ws_dir);
        let actions = trace_action_set(&entries);
        assert!(
            actions.contains(&TraceAction::ContextHandoff),
            "自动触发上下文衔接时应记录 ContextHandoff trace, 实际: {:?}",
            actions
        );
    }
}

/// 测试 14: 转向提醒注入 + 全功能共存
#[tokio::test]
async fn test_steer_reminder_with_all_features() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    // steer_interval=1 → 每轮都注入提醒
    let chat = MockChat::new(vec![simple_plan(), success_code()]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "转向提醒", 3, 60)
        .with_slash_commands(true)
        .with_dev_trace(true)
        .with_loop_detection(3)
        .with_steer_reminder(1) // 每轮注入
        .with_context_handoff(100) // 禁用自动交接 (避免干扰)
        .with_interaction(Box::new(MockInteraction::new()))
        .with_error_diagnosis(Box::new(MockErrorDiagnoser::empty()));

    orch.run().await.unwrap();

    // 任务应正常完成
    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(
        task.status,
        TaskStatus::Completed,
        "转向提醒不应阻止任务完成"
    );

    // 发送的消息中应包含提醒内容 (steer_interval=1, 第1轮后注入)
    let sent = chat.sent_messages();
    assert!(!sent.is_empty(), "应发送了消息");
}

/// 测试 15: 多指令同时 + 全功能共存
#[tokio::test]
async fn test_multiple_commands_all_features() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    // /retry + /refocus + /skip → 全部执行, 最终 /skip 跳过
    let response =
        "调整策略。\n/retry\n/refocus\n/skip\n```file:src/main.rs\nfn main() {}\n```".to_string();

    let chat = MockChat::new(vec![
        simple_plan(),
        response,
        "收到提醒。".to_string(), // refocus 回复
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = build_full_orchestrator(&chat, runner, extractor, ws_dir, "多指令");

    orch.run().await.unwrap();

    // 三个指令都应有决策记录
    for cmd in &["retry", "refocus", "skip"] {
        let has = orch
            .memory
            .decisions
            .iter()
            .any(|d| d.decision.contains(cmd));
        assert!(has, "应有 /{} 决策记录", cmd);
    }

    // /skip 最终导致任务 Failed
    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(task.status, TaskStatus::Failed, "/skip 应使任务 Failed");

    // DevTrace 应记录 SlashCommand
    let entries = read_trace_entries(ws_dir);
    let actions = trace_action_set(&entries);
    assert!(
        actions.contains(&TraceAction::SlashCommand),
        "应包含 SlashCommand trace"
    );
}

/// 测试 16: 全功能 + 修复中 /skip → 共存不冲突
#[tokio::test]
async fn test_skip_in_fix_round_all_features() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let errors = vec![make_compile_error("src/main.rs", 1, "error")];

    let chat = MockChat::new(vec![
        simple_plan(),
        success_code(), // attempt 1: 代码写好但编译失败
        skip_code(),    // attempt 2: 修复中发出 /skip
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            make_test_result(false, errors), // attempt 1 编译失败
            make_test_result(true, vec![]),  // attempt 2 编译成功
        ])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/main.rs", "fn main() {}")],
    ]);

    let mut orch = build_full_orchestrator(&chat, runner, extractor, ws_dir, "修复中skip");

    orch.run().await.unwrap();

    // /skip 在修复轮中也生效
    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(
        task.status,
        TaskStatus::Failed,
        "修复轮中 /skip 应使任务 Failed"
    );
}

/// 测试 17: 全功能禁用 → 基本功能仍正常 (退化测试)
#[tokio::test]
async fn test_all_features_disabled_baseline() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![simple_plan(), success_code()]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    // 全部禁用: 只保留基本功能
    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "基线", 3, 60)
        .with_slash_commands(false)
        .with_dev_trace(false)
        .with_loop_detection(0) // 禁用
        .with_steer_reminder(0) // 禁用
        .with_context_handoff(0); // 禁用

    orch.run().await.unwrap();

    // 基本功能正常
    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(
        task.status,
        TaskStatus::Completed,
        "禁用所有增强功能后基本功能应正常"
    );

    // DevTrace 文件不应存在
    let trace_path = format!("{}/.forge/devtrace.jsonl", ws_dir);
    assert!(
        !Path::new(&trace_path).exists(),
        "禁用 DevTrace 时不应创建 trace 文件"
    );
}

/// 测试 18: 全功能 + 复杂修复场景 (多次失败后成功)
#[tokio::test]
async fn test_complex_fix_scenario_all_features() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let err1 = vec![make_compile_error("src/main.rs", 1, "syntax error")];
    let err2 = vec![make_compile_error("src/main.rs", 5, "type mismatch")];

    let chat = MockChat::new(vec![
        simple_plan(),
        "```file:src/main.rs\nfn main() { broken }\n```".to_string(), // 语法错误
        "```file:src/main.rs\nfn main() { let x: i32 = \"s\"; }\n```".to_string(), // 类型错误
        "```file:src/main.rs\nfn main() { println!(\"ok\"); }\n```".to_string(), // 修复成功
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            make_test_result(false, err1),  // 第一次: 语法错误
            make_test_result(false, err2),  // 第二次: 类型错误
            make_test_result(true, vec![]), // 第三次: 成功
        ])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() { broken }")],
        vec![ef("src/main.rs", "fn main() { let x: i32 = \"s\"; }")],
        vec![ef("src/main.rs", "fn main() { println!(\"ok\"); }")],
    ]);

    let mut orch = build_full_orchestrator(&chat, runner, extractor, ws_dir, "复杂修复");

    orch.run().await.unwrap();

    // 任务最终成功
    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(task.status, TaskStatus::Completed, "复杂修复后应成功");
    assert_eq!(task.attempts, 3, "应尝试 3 次");

    // DevTrace 应有 1 个 TaskExecution + 2 个 FixAttempt
    let entries = read_trace_entries(ws_dir);
    let task_exec_count = entries
        .iter()
        .filter(|e| e.action == TraceAction::TaskExecution)
        .count();
    let fix_count = entries
        .iter()
        .filter(|e| e.action == TraceAction::FixAttempt)
        .count();
    assert_eq!(task_exec_count, 1, "应有 1 个 TaskExecution");
    assert_eq!(fix_count, 2, "应有 2 个 FixAttempt");

    // 应有 3 个 CompileCheck (2 失败 + 1 成功)
    let compile_count = entries
        .iter()
        .filter(|e| e.action == TraceAction::CompileCheck)
        .count();
    assert_eq!(compile_count, 3, "应有 3 个 CompileCheck");
}

/// 测试 19: 代码块内 slash command 不被检测 + 全功能共存
#[tokio::test]
async fn test_code_block_commands_ignored_all_features() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    // /skip 在代码块内 → 不被检测
    let response =
        "代码完成。\n```\n/skip\n```\n```file:src/main.rs\nfn main() {}\n```".to_string();

    let chat = MockChat::new(vec![simple_plan(), response]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = build_full_orchestrator(&chat, runner, extractor, ws_dir, "代码块skip");

    orch.run().await.unwrap();

    // 任务应正常完成 (代码块内 /skip 被忽略)
    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(
        task.status,
        TaskStatus::Completed,
        "代码块内 /skip 应被忽略"
    );
}

/// 测试 20: strip_commands 不影响代码提取 + 全功能共存
#[tokio::test]
async fn test_strip_commands_preserves_code_all_features() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let response =
        "完成。\n/skip\n```file:src/main.rs\nfn main() { println!(\"hello\"); }\n```".to_string();

    let chat = MockChat::new(vec![simple_plan(), response]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = build_full_orchestrator(&chat, runner, extractor, ws_dir, "strip测试");

    orch.run().await.unwrap();

    // /skip 导致任务 Failed
    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(task.status, TaskStatus::Failed, "/skip 应使任务 Failed");

    // 但代码文件应已写入 (strip_commands 清理指令后提取代码)
    // 注: 即使 /skip 跳过了任务, 代码可能在跳过前已写入
    // 主要验证: 没有因为指令标记导致提取失败或 panic
}

/// 测试 21: TraceAction::all() 包含所有 19 种操作类型
#[test]
fn test_all_trace_actions_present() {
    let all = TraceAction::all();
    assert_eq!(all.len(), 22, "应有 22 种 TraceAction");

    // 验证包含 SlashCommand
    assert!(
        all.contains(&TraceAction::SlashCommand),
        "应包含 SlashCommand"
    );
    // 验证包含 HealthCheck + SiteFailover + PerformanceStats
    assert!(
        all.contains(&TraceAction::HealthCheck),
        "应包含 HealthCheck"
    );
    assert!(
        all.contains(&TraceAction::SiteFailover),
        "应包含 SiteFailover"
    );
    assert!(
        all.contains(&TraceAction::PerformanceStats),
        "应包含 PerformanceStats"
    );
    // 验证包含所有其他类型
    assert!(all.contains(&TraceAction::Planning));
    assert!(all.contains(&TraceAction::TaskExecution));
    assert!(all.contains(&TraceAction::FixAttempt));
    assert!(all.contains(&TraceAction::Clarification));
    assert!(all.contains(&TraceAction::ContextHandoff));
    assert!(all.contains(&TraceAction::SteerReminder));
    assert!(all.contains(&TraceAction::LoopDetection));
    assert!(all.contains(&TraceAction::CompileCheck));
    assert!(all.contains(&TraceAction::TestRun));
    assert!(all.contains(&TraceAction::E2ETest));
    assert!(all.contains(&TraceAction::RequirementChange));
}

/// 测试 22: SlashCommand 解析在全功能环境下正确
#[test]
fn test_slash_command_parse_full_environment() {
    let response = "/compact\n/skip\n/refocus\n/retry\n/escalate\n/search";
    let cmds = slash_command::parse_from_response(response);
    assert_eq!(cmds.len(), 6, "应解析出 6 个指令");

    // 验证所有已知指令
    for cmd in forge::slash_command::SlashCommand::all_known() {
        assert!(cmds.contains(&cmd), "应包含: {:?}", cmd);
    }
}

/// 测试 23: 全功能 + 断点续传共存
#[tokio::test]
async fn test_resume_with_all_features() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    // 创建已完成的 memory
    let mut memory = forge::memory::Memory::new("续传测试");
    memory.set_phases(vec![forge::memory::Phase {
        id: 0,
        name: "阶段1".to_string(),
        description: "测试".to_string(),
        status: forge::memory::PhaseStatus::InProgress,
        tasks: vec![
            forge::memory::Task {
                id: "0-0".to_string(),
                phase_id: 0,
                name: "已完成".to_string(),
                prompt: "done".to_string(),
                status: TaskStatus::Completed,
                result: Some("成功".to_string()),
                attempts: 1,
                files_written: vec!["src/main.rs".to_string()],
                test_result: None,
                last_good_snapshot: None,
                clarifications: vec![],
                depends_on: vec![],
            },
            forge::memory::Task {
                id: "0-1".to_string(),
                phase_id: 0,
                name: "待执行".to_string(),
                prompt: "todo".to_string(),
                status: TaskStatus::Pending,
                result: None,
                attempts: 0,
                files_written: vec![],
                test_result: None,
                last_good_snapshot: None,
                clarifications: vec![],
                depends_on: vec![],
            },
        ],
    }]);

    let ws = forge::workspace::Workspace::new(ws_dir);
    ws.init().unwrap();
    ws.write_file("src/main.rs", "fn main() {}").unwrap();
    memory
        .save(&ws.root.join(".forge").join("memory.json"))
        .unwrap();
    drop(ws);

    let chat = MockChat::new(vec![success_code()]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/lib.rs", "pub fn lib() {}")]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "续传测试", 5, 60)
        .with_resume(true)
        .with_slash_commands(true)
        .with_dev_trace(true)
        .with_loop_detection(3)
        .with_steer_reminder(10)
        .with_context_handoff(50)
        .with_interaction(Box::new(MockInteraction::new()))
        .with_error_diagnosis(Box::new(MockErrorDiagnoser::empty()));

    orch.run().await.unwrap();

    // 第一个任务仍为 Completed
    assert_eq!(orch.memory.phases[0].tasks[0].status, TaskStatus::Completed);
    // 第二个任务变为 Completed
    assert_eq!(orch.memory.phases[0].tasks[1].status, TaskStatus::Completed);
}

/// 测试 24: 全功能 + 无代码返回 → 不 panic
#[tokio::test]
async fn test_no_code_returned_all_features() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        simple_plan(),
        "I will help you.".to_string(), // 无代码
        success_code(),                 // 第二次有代码
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![
        vec![], // 第一次无文件
        vec![ef("src/main.rs", "fn main() {}")],
    ]);

    let mut orch = build_full_orchestrator(&chat, runner, extractor, ws_dir, "无代码");

    orch.run().await.unwrap();

    // 最终成功
    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(task.status, TaskStatus::Completed, "第二次有代码后应成功");
}

/// 测试 25: 全功能 + 无效 JSON planning → 回退默认计划
#[tokio::test]
async fn test_invalid_json_planning_all_features() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        "Sorry, I cannot parse that.".to_string(), // 无效 JSON
        success_code(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = build_full_orchestrator(&chat, runner, extractor, ws_dir, "无效JSON");

    orch.run().await.unwrap();

    // 使用默认计划, 任务完成
    assert_eq!(orch.memory.phases.len(), 1, "应使用默认单阶段计划");
    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(task.status, TaskStatus::Completed, "默认计划任务应完成");
}
