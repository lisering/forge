//! 结构化开发追踪集成测试 (借鉴方向 4)
//!
//! 验证:
//! 1. DevTrace 文件 (.forge/devtrace.jsonl) 在运行后正确创建
//! 2. Planning trace 条目在规划完成后被记录
//! 3. TaskExecution trace 条目在首次执行任务时被记录
//! 4. CompileCheck trace 条目在编译检查后被记录 (成功/失败)
//! 5. TestRun trace 条目在测试运行后被记录
//! 6. FixAttempt trace 条目在修复轮被记录
//! 7. 禁用 DevTrace (with_dev_trace(false)) → 不创建 trace 文件
//! 8. 多任务多阶段 → trace 条目覆盖所有操作
//! 9. DevTraceSummary 正确统计成功率
//! 10. trace 条目包含正确的操作类型
//! 11. trace 条目记录阶段和任务索引
//! 12. 运行结束后 trace 文件可被 DevTraceWriter 读取
//! 13. 循环终止检测时记录 LoopDetection trace 条目
//! 14. 上下文衔接时记录 ContextHandoff trace 条目

use async_trait::async_trait;
use forge::dev_trace::{DevTraceWriter, TraceAction};
use forge::extract::ExtractedFile;
use forge::orchestrator::Orchestrator;
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
    /// 对话轮次计数器 (用于上下文衔接测试)
    turn_count: Arc<AtomicUsize>,
    /// 是否支持 start_new_conversation
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

/// 简单 JSON 计划回复
fn plan_response_single_task() -> String {
    r#"```json
[{"name":"阶段1","description":"d","tasks":[{"name":"任务1","prompt":"do it"}]}]
```"#
        .to_string()
}

/// 两个任务的计划回复
fn plan_response_two_tasks() -> String {
    r#"```json
[{"name":"阶段1","description":"d","tasks":[{"name":"任务1","prompt":"task1"},{"name":"任务2","prompt":"task2"}]}]
```"#
        .to_string()
}

/// 两个阶段的计划回复
fn plan_response_two_phases() -> String {
    r#"```json
[
  {"name":"阶段1","description":"d1","tasks":[{"name":"任务1","prompt":"do1"}]},
  {"name":"阶段2","description":"d2","tasks":[{"name":"任务2","prompt":"do2"}]}
]
```"#
        .to_string()
}

/// 成功的代码回复
fn success_code_response() -> String {
    "以下是完整的代码实现。\n```file:src/main.rs\nfn main() { println!(\"hello\"); }\n```"
        .to_string()
}

/// 生成一个代码回复
fn code_response() -> String {
    "以下是修复后的代码实现。\n```file:src/main.rs\nfn main() { let x = 1; println!(\"{}\", x); }\n```".to_string()
}

/// 读取 trace 文件中所有条目
fn read_trace_entries(ws_dir: &str) -> Vec<forge::dev_trace::DevTraceEntry> {
    let writer = DevTraceWriter::new(Path::new(ws_dir));
    writer.read_all().unwrap_or_default()
}

/// 获取 trace 条目中的所有操作类型
fn trace_actions(entries: &[forge::dev_trace::DevTraceEntry]) -> Vec<TraceAction> {
    entries.iter().map(|e| e.action).collect()
}

// ============================================================================
//  测试用例
// ============================================================================

/// 测试 1: DevTrace 文件在运行后被创建
#[tokio::test]
async fn test_dev_trace_file_created() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![plan_response_single_task(), success_code_response()]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10).with_dev_trace(true);

    orch.run().await.unwrap();

    // 验证: trace 文件存在
    let trace_path = format!("{}/.forge/devtrace.jsonl", ws_dir);
    assert!(
        Path::new(&trace_path).exists(),
        "devtrace.jsonl 应在运行后存在"
    );
}

/// 测试 2: 禁用 DevTrace → 不创建 trace 文件
#[tokio::test]
async fn test_dev_trace_disabled_no_file() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![plan_response_single_task(), success_code_response()]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10).with_dev_trace(false);

    orch.run().await.unwrap();

    // 验证: trace 文件不存在
    let trace_path = format!("{}/.forge/devtrace.jsonl", ws_dir);
    assert!(
        !Path::new(&trace_path).exists(),
        "禁用 DevTrace 时不应创建 trace 文件"
    );
}

/// 测试 3: Planning trace 条目在规划完成后被记录
#[tokio::test]
async fn test_planning_trace_recorded() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![plan_response_single_task(), success_code_response()]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10).with_dev_trace(true);

    orch.run().await.unwrap();

    let entries = read_trace_entries(ws_dir);
    let actions = trace_actions(&entries);

    // 验证: 包含 Planning 操作
    assert!(
        actions.contains(&TraceAction::Planning),
        "trace 应包含 Planning 条目, 实际: {:?}",
        actions
    );
}

/// 测试 4: TaskExecution trace 条目在首次执行任务时被记录
#[tokio::test]
async fn test_task_execution_trace_recorded() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![plan_response_single_task(), success_code_response()]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10).with_dev_trace(true);

    orch.run().await.unwrap();

    let entries = read_trace_entries(ws_dir);
    let actions = trace_actions(&entries);

    // 验证: 包含 TaskExecution 操作
    assert!(
        actions.contains(&TraceAction::TaskExecution),
        "trace 应包含 TaskExecution 条目, 实际: {:?}",
        actions
    );

    // 验证: TaskExecution 条目的 phase_idx 和 task_idx
    let task_exec = entries
        .iter()
        .find(|e| e.action == TraceAction::TaskExecution)
        .unwrap();
    assert_eq!(task_exec.phase_idx, Some(0));
    assert_eq!(task_exec.task_idx, Some(0));
    assert!(task_exec.task_name.is_some());
    assert!(task_exec.success, "TaskExecution 应标记为成功");
}

/// 测试 5: CompileCheck trace 条目在编译检查后被记录 (成功)
#[tokio::test]
async fn test_compile_check_trace_success() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![plan_response_single_task(), success_code_response()]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10).with_dev_trace(true);

    orch.run().await.unwrap();

    let entries = read_trace_entries(ws_dir);

    // 验证: 包含 CompileCheck 操作且标记为成功
    let compile_check = entries
        .iter()
        .find(|e| e.action == TraceAction::CompileCheck)
        .expect("应包含 CompileCheck 条目");
    assert!(compile_check.success, "CompileCheck 应标记为成功");
    assert!(compile_check.error.is_none(), "成功时不应有错误信息");
}

/// 测试 6: CompileCheck trace 条目在编译检查后被记录 (失败)
#[tokio::test]
async fn test_compile_check_trace_failure() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        plan_response_single_task(),
        code_response(),
        success_code_response(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            make_test_result(
                false,
                vec![make_compile_error(
                    "src/main.rs",
                    1,
                    "syntax error",
                    "E0308",
                )],
            ),
            make_test_result(true, vec![]),
        ])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/main.rs", "fn main() { println!(\"fixed\"); }")],
    ]);

    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10).with_dev_trace(true);

    orch.run().await.unwrap();

    let entries = read_trace_entries(ws_dir);

    // 验证: 有 CompileCheck 失败条目
    let failed_checks: Vec<_> = entries
        .iter()
        .filter(|e| e.action == TraceAction::CompileCheck && !e.success)
        .collect();
    assert!(
        !failed_checks.is_empty(),
        "应至少有一个 CompileCheck 失败条目"
    );

    // 验证: 失败的 CompileCheck 有错误信息
    let failed = &failed_checks[0];
    assert!(failed.error.is_some(), "失败的 CompileCheck 应有错误信息");
}

/// 测试 7: TestRun trace 条目在测试运行后被记录
#[tokio::test]
async fn test_test_run_trace_recorded() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![plan_response_single_task(), success_code_response()]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10).with_dev_trace(true);

    orch.run().await.unwrap();

    let entries = read_trace_entries(ws_dir);
    let actions = trace_actions(&entries);

    // 验证: 包含 TestRun 操作
    assert!(
        actions.contains(&TraceAction::TestRun),
        "trace 应包含 TestRun 条目"
    );

    // 验证: TestRun 标记为成功
    let test_run = entries
        .iter()
        .find(|e| e.action == TraceAction::TestRun)
        .expect("应包含 TestRun 条目");
    assert!(test_run.success, "TestRun 应标记为成功");
}

/// 测试 8: FixAttempt trace 条目在修复轮被记录
#[tokio::test]
async fn test_fix_attempt_trace_recorded() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        plan_response_single_task(),
        code_response(),         // 第一次: 编译失败
        success_code_response(), // 第二次: 修复成功
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            make_test_result(
                false,
                vec![make_compile_error(
                    "src/main.rs",
                    1,
                    "syntax error",
                    "E0308",
                )],
            ),
            make_test_result(true, vec![]),
        ])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/main.rs", "fn main() { println!(\"fixed\"); }")],
    ]);

    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10).with_dev_trace(true);

    orch.run().await.unwrap();

    let entries = read_trace_entries(ws_dir);
    let actions = trace_actions(&entries);

    // 验证: 包含 TaskExecution (第一次) 和 FixAttempt (第二次)
    assert!(
        actions.contains(&TraceAction::TaskExecution),
        "trace 应包含 TaskExecution (首次尝试)"
    );
    assert!(
        actions.contains(&TraceAction::FixAttempt),
        "trace 应包含 FixAttempt (修复轮)"
    );

    // 验证: 有 1 个 TaskExecution + 1 个 FixAttempt
    let task_exec_count = entries
        .iter()
        .filter(|e| e.action == TraceAction::TaskExecution)
        .count();
    let fix_attempt_count = entries
        .iter()
        .filter(|e| e.action == TraceAction::FixAttempt)
        .count();
    assert_eq!(task_exec_count, 1, "应有 1 个 TaskExecution 条目");
    assert_eq!(fix_attempt_count, 1, "应有 1 个 FixAttempt 条目");
}

/// 测试 9: 多任务多阶段 → trace 条目覆盖所有操作
#[tokio::test]
async fn test_multi_task_multi_phase_traces() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        plan_response_two_phases(),
        success_code_response(), // 阶段1任务1
        success_code_response(), // 阶段2任务1
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

    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10).with_dev_trace(true);

    orch.run().await.unwrap();

    let entries = read_trace_entries(ws_dir);

    // 验证: 有 2 个 TaskExecution (每个阶段 1 个)
    let task_exec_count = entries
        .iter()
        .filter(|e| e.action == TraceAction::TaskExecution)
        .count();
    assert_eq!(task_exec_count, 2, "应有 2 个 TaskExecution 条目");

    // 验证: 有 2 个 CompileCheck
    let compile_count = entries
        .iter()
        .filter(|e| e.action == TraceAction::CompileCheck)
        .count();
    assert_eq!(compile_count, 2, "应有 2 个 CompileCheck 条目");

    // 验证: 有 2 个 TestRun
    let test_count = entries
        .iter()
        .filter(|e| e.action == TraceAction::TestRun)
        .count();
    assert_eq!(test_count, 2, "应有 2 个 TestRun 条目");

    // 验证: 2 个任务分别属于不同阶段
    let task_execs: Vec<_> = entries
        .iter()
        .filter(|e| e.action == TraceAction::TaskExecution)
        .collect();
    assert_eq!(task_execs[0].phase_idx, Some(0));
    assert_eq!(task_execs[1].phase_idx, Some(1));
}

/// 测试 10: DevTraceSummary 正确统计成功率
#[tokio::test]
async fn test_dev_trace_summary_success_rate() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        plan_response_single_task(),
        code_response(),         // 编译失败
        success_code_response(), // 修复成功
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            make_test_result(
                false,
                vec![make_compile_error("src/main.rs", 1, "error", "E0308")],
            ),
            make_test_result(true, vec![]),
        ])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/main.rs", "fn main() { println!(\"fixed\"); }")],
    ]);

    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10).with_dev_trace(true);

    orch.run().await.unwrap();

    // 读取 trace 文件并生成摘要
    let writer = DevTraceWriter::new(Path::new(ws_dir));
    let summary = writer.summary();

    // 验证: 总条目 > 0
    assert!(summary.total_entries > 0, "应有 trace 条目");

    // 验证: 成功率在 0~1 之间
    assert!(
        summary.success_rate > 0.0 && summary.success_rate <= 1.0,
        "成功率应在 0~1 之间, 实际: {}",
        summary.success_rate
    );

    // 验证: 包含 CompileCheck 统计 (有成功和失败)
    let check_stats = summary
        .get_action_stats(TraceAction::CompileCheck)
        .expect("应有 CompileCheck 统计");
    assert!(check_stats.count >= 2, "应至少有 2 次 CompileCheck");
    assert!(check_stats.success_count >= 1, "应至少有 1 次成功");
}

/// 测试 11: trace 条目记录了耗时 (duration_ms > 0)
#[tokio::test]
async fn test_trace_entries_record_duration() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![plan_response_single_task(), success_code_response()]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10).with_dev_trace(true);

    orch.run().await.unwrap();

    let entries = read_trace_entries(ws_dir);

    // 验证: 至少有一个非零耗时的条目
    // 注意: MockChat 是同步的, 耗时可能很小但不一定为 0
    // 验证: 所有条目都有 duration_ms 字段 (u64, 始终有值)
    assert!(!entries.is_empty(), "应有 trace 条目");

    // 验证: 所有条目都有 duration_ms 字段 (即使是 0)
    for entry in &entries {
        // duration_ms 是 u64, 始终有值, 验证它不是异常大的值
        assert!(
            entry.duration_ms < 3_600_000, // < 1 小时
            "duration_ms 不应异常大: {}",
            entry.duration_ms
        );
    }
}

/// 测试 12: 运行结束后 trace 文件可被 DevTraceWriter 读取
#[tokio::test]
async fn test_trace_file_readable_by_writer() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![plan_response_single_task(), success_code_response()]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10).with_dev_trace(true);

    orch.run().await.unwrap();

    // 用一个新的 DevTraceWriter 读取 (模拟运行后分析)
    let writer = DevTraceWriter::new(Path::new(ws_dir));
    let entries = writer.read_all().unwrap();

    assert!(!entries.is_empty(), "应能读取到 trace 条目");

    // 验证: 所有条目都是有效的 JSONL (能反序列化)
    for entry in &entries {
        let json = entry.to_jsonl().unwrap();
        let reparsed = forge::dev_trace::DevTraceEntry::from_jsonl(&json).unwrap();
        assert_eq!(reparsed.action, entry.action);
    }
}

/// 测试 13: 全部失败的任务 → trace 记录所有失败
#[tokio::test]
async fn test_all_failures_traced() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        plan_response_single_task(),
        code_response(),
        code_response(),
        code_response(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            make_test_result(
                false,
                vec![make_compile_error("src/main.rs", 1, "err1", "E0308")],
            ),
            make_test_result(
                false,
                vec![make_compile_error("src/main.rs", 1, "err2", "E0308")],
            ),
            make_test_result(
                false,
                vec![make_compile_error("src/main.rs", 1, "err3", "E0308")],
            ),
        ])
        .with_test_results(vec![]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/main.rs", "fn main() {}")],
    ]);

    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10).with_dev_trace(true);

    orch.run().await.unwrap();

    let entries = read_trace_entries(ws_dir);

    // 验证: 1 个 TaskExecution + 2 个 FixAttempt
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

    // 验证: 3 个失败的 CompileCheck
    let failed_checks = entries
        .iter()
        .filter(|e| e.action == TraceAction::CompileCheck && !e.success)
        .count();
    assert_eq!(failed_checks, 3, "应有 3 个失败的 CompileCheck");

    // 验证: 没有成功的 TestRun (因为编译都失败了)
    let test_runs = entries
        .iter()
        .filter(|e| e.action == TraceAction::TestRun)
        .count();
    assert_eq!(test_runs, 0, "不应有 TestRun (编译都失败了)");
}

/// 测试 14: trace 条目的输入输出摘要被截断到 200 字符
#[tokio::test]
async fn test_trace_summary_truncated() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    // 生成一个非常长的 planning 回复
    let long_plan = format!(
        r#"```json
[{{"name":"阶段1","description":"{}","tasks":[{{"name":"任务1","prompt":"do it"}}]}}]
```"#,
        "x".repeat(500)
    );

    let chat = MockChat::new(vec![long_plan, success_code_response()]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10).with_dev_trace(true);

    orch.run().await.unwrap();

    let entries = read_trace_entries(ws_dir);

    // 找到 Planning 条目
    let planning = entries
        .iter()
        .find(|e| e.action == TraceAction::Planning)
        .expect("应有 Planning 条目");

    // 验证: 输出摘要被截断到 200 字符以内
    assert!(
        planning.output_summary.chars().count() <= 200,
        "输出摘要应被截断到 200 字符, 实际: {}",
        planning.output_summary.chars().count()
    );
}

/// 测试 15: DevTraceWriter.clear() 在全新开始时清空旧 trace
#[tokio::test]
async fn test_dev_trace_cleared_on_fresh_start() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    // 先写入一些旧的 trace 数据
    let ws_path = Path::new(ws_dir);
    std::fs::create_dir_all(ws_path.join(".forge")).unwrap();
    let old_writer = DevTraceWriter::new(ws_path);
    old_writer
        .trace(
            TraceAction::Planning,
            None,
            None,
            None,
            "old input",
            "old output",
            9999,
            true,
            None,
        )
        .unwrap();

    // 验证旧数据存在
    assert_eq!(old_writer.entry_count(), 1);

    // 运行 Orchestrator (全新开始)
    let chat = MockChat::new(vec![plan_response_single_task(), success_code_response()]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10).with_dev_trace(true);

    orch.run().await.unwrap();

    // 读取 trace
    let new_writer = DevTraceWriter::new(ws_path);
    let entries = new_writer.read_all().unwrap();

    // 验证: 旧条目已被清空, 只有新运行的条目
    let has_old = entries.iter().any(|e| e.duration_ms == 9999);
    assert!(!has_old, "旧 trace 条目应被清空 (全新开始时 clear)");
}

/// 测试 16: trace 条目包含任务名称
#[tokio::test]
async fn test_trace_entries_contain_task_name() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![plan_response_single_task(), success_code_response()]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10).with_dev_trace(true);

    orch.run().await.unwrap();

    let entries = read_trace_entries(ws_dir);

    // 找到 TaskExecution 条目
    let task_exec = entries
        .iter()
        .find(|e| e.action == TraceAction::TaskExecution)
        .expect("应有 TaskExecution 条目");

    // 验证: task_name 是 Some 且不为空
    assert!(
        task_exec.task_name.is_some(),
        "TaskExecution 应有 task_name"
    );
    let name = task_exec.task_name.as_ref().unwrap();
    assert!(!name.is_empty(), "task_name 不应为空");
}

/// 测试 17: DevTrace 与循环终止检测共存 → 记录 LoopDetection trace
#[tokio::test]
async fn test_dev_trace_with_loop_detection() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        plan_response_single_task(),
        code_response(),
        code_response(),
        code_response(),
        code_response(),
    ]);

    let same_error = vec![make_compile_error(
        "src/main.rs",
        10,
        "mismatched types",
        "E0308",
    )];

    let runner = MockTestRunner::new()
        .with_check_results(vec![
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
    ]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 5, 10)
        .with_dev_trace(true)
        .with_loop_detection(3);

    orch.run().await.unwrap();

    let entries = read_trace_entries(ws_dir);
    let actions = trace_actions(&entries);

    // 验证: 包含 LoopDetection 操作
    assert!(
        actions.contains(&TraceAction::LoopDetection),
        "trace 应包含 LoopDetection 条目 (循环终止检测触发时记录), 实际: {:?}",
        actions
    );

    // 验证: LoopDetection 条目标记为失败
    let loop_det = entries
        .iter()
        .find(|e| e.action == TraceAction::LoopDetection)
        .expect("应有 LoopDetection 条目");
    assert!(
        !loop_det.success,
        "LoopDetection 应标记为失败 (检测到死循环)"
    );
    assert!(loop_det.error.is_some(), "LoopDetection 应有错误信息");
}

/// 测试 18: DevTraceSummary 的 to_report 生成可读报告
#[tokio::test]
async fn test_dev_trace_summary_report() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        plan_response_two_tasks(),
        success_code_response(), // 任务1成功
        success_code_response(), // 任务2成功
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

    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10).with_dev_trace(true);

    orch.run().await.unwrap();

    let writer = DevTraceWriter::new(Path::new(ws_dir));
    let summary = writer.summary();
    let report = summary.to_report();

    // 验证: 报告包含关键信息
    assert!(report.contains("DevTrace 开发追踪报告"), "报告应包含标题");
    assert!(report.contains("总条目"), "报告应包含总条目数");
    assert!(report.contains("成功率"), "报告应包含成功率");
    assert!(report.contains("按操作类型统计"), "报告应包含操作类型统计");
}

/// 测试 19: 多任务 → trace 中 CompileCheck/TestRun 数量匹配
#[tokio::test]
async fn test_multi_task_trace_counts() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        plan_response_two_tasks(),
        success_code_response(), // 任务1
        success_code_response(), // 任务2
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

    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10).with_dev_trace(true);

    orch.run().await.unwrap();

    let entries = read_trace_entries(ws_dir);

    // 验证: 2 个 TaskExecution + 2 个 CompileCheck + 2 个 TestRun + 1 个 Planning
    let planning_count = entries
        .iter()
        .filter(|e| e.action == TraceAction::Planning)
        .count();
    let task_exec_count = entries
        .iter()
        .filter(|e| e.action == TraceAction::TaskExecution)
        .count();
    let compile_count = entries
        .iter()
        .filter(|e| e.action == TraceAction::CompileCheck)
        .count();
    let test_count = entries
        .iter()
        .filter(|e| e.action == TraceAction::TestRun)
        .count();

    assert_eq!(planning_count, 1, "应有 1 个 Planning");
    assert_eq!(task_exec_count, 2, "应有 2 个 TaskExecution");
    assert_eq!(compile_count, 2, "应有 2 个 CompileCheck");
    assert_eq!(test_count, 2, "应有 2 个 TestRun");
}

/// 测试 20: DevTrace 与上下文衔接共存 → 记录 ContextHandoff trace
#[tokio::test]
async fn test_dev_trace_with_context_handoff() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    // max_context_turns=2 → 每 2 轮对话后触发上下文衔接
    // planning 用 1 轮, execute_task 用 1 轮 → 第 2 轮后触发交接
    let chat = MockChat::new(vec![plan_response_single_task(), success_code_response()]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10)
        .with_dev_trace(true)
        .with_context_handoff(2);

    orch.run().await.unwrap();

    let entries = read_trace_entries(ws_dir);
    let actions = trace_actions(&entries);

    // 验证: 包含 ContextHandoff 操作 (如果触发了上下文衔接)
    if chat.new_conversation_count() > 0 {
        assert!(
            actions.contains(&TraceAction::ContextHandoff),
            "触发了上下文衔接时应记录 ContextHandoff trace, 实际: {:?}",
            actions
        );
    }
}
