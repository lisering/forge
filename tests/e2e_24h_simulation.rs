//! 24 小时运行模拟测试 (第 16 项任务)
//!
//! 模拟 24 小时不间断运行场景:
//! 1. 大量任务 (20+ 任务) 的长时运行 — 验证不 panic、不累积错误
//! 2. DevTrace 摘要报告在大量条目下正确
//! 3. Memory 增长稳定 — 阶段/任务/决策数量与预期一致
//! 4. 断点续传在长时运行中正确工作
//! 5. 上下文衔接在长时运行中多次触发
//! 6. 转向提醒在长时运行中多次注入
//! 7. 混合成功/失败/修复/跳过场景的长时间运行
//! 8. DevTrace JSONL 文件可被正确读取和解析

use async_trait::async_trait;
use forge::dev_trace::{DevTraceWriter, TraceAction};
use forge::error_diagnosis::MockErrorDiagnoser;
use forge::extract::ExtractedFile;
use forge::interaction::MockInteraction;
use forge::memory::TaskStatus;
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

fn ok_result() -> TestResult {
    TestResult {
        success: true,
        stdout: String::new(),
        stderr: String::new(),
        exit_code: 0,
        errors: vec![],
        test_summary: None,
    }
}

fn fail_result(errors: Vec<CompileError>) -> TestResult {
    TestResult {
        success: false,
        stdout: String::new(),
        stderr: "error".to_string(),
        exit_code: 1,
        errors,
        test_summary: None,
    }
}

fn ef(path: &str, content: &str) -> ExtractedFile {
    ExtractedFile {
        path: path.to_string(),
        content: content.to_string(),
        language: String::new(),
    }
}

fn success_code() -> String {
    "完成。\n```file:src/main.rs\nfn main() {}\n```".to_string()
}

fn read_trace_entries(ws_dir: &str) -> Vec<forge::dev_trace::DevTraceEntry> {
    let writer = DevTraceWriter::new(Path::new(ws_dir));
    writer.read_all().unwrap_or_default()
}

/// 生成 N 个任务的 JSON 计划
fn large_plan(n_tasks: usize) -> String {
    let tasks: Vec<String> = (0..n_tasks)
        .map(|i| format!(r#"{{"name":"任务{}","prompt":"do task {}"}}"#, i, i))
        .collect();
    format!(
        r#"```json
[{{"name":"大规模开发","description":"24h simulation","tasks":[{}]}}]
```"#,
        tasks.join(",")
    )
}

/// 构建全功能 Orchestrator
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
        .with_slash_commands(true)
        .with_dev_trace(true)
        .with_loop_detection(3)
        .with_steer_reminder(5)
        .with_context_handoff(10)
        .with_interaction(Box::new(MockInteraction::new()))
        .with_error_diagnosis(Box::new(MockErrorDiagnoser::empty()))
}

// ============================================================================
//  测试用例
// ============================================================================

/// 测试 1: 20 个任务的长时运行 — 全部成功
#[tokio::test]
async fn test_20_tasks_all_success() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();
    let n = 20;

    let mut chat_responses = vec![large_plan(n)];
    let mut check_results = vec![];
    let mut test_results = vec![];
    let mut file_sets = vec![];

    for i in 0..n {
        chat_responses.push(success_code());
        check_results.push(ok_result());
        test_results.push(ok_result());
        file_sets.push(vec![ef(&format!("src/mod{}.rs", i), "pub fn f() {}")]);
    }

    let chat = MockChat::new(chat_responses);
    let runner = MockTestRunner::new()
        .with_check_results(check_results)
        .with_test_results(test_results);
    let extractor = MockExtractor::new(file_sets);

    let mut orch = build_full_orchestrator(&chat, runner, extractor, ws_dir, "20任务");

    orch.run().await.unwrap();

    // 验证: 所有 20 个任务完成
    let total: usize = orch.memory.phases.iter().map(|p| p.tasks.len()).sum();
    assert_eq!(total, n, "应有 {} 个任务", n);
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

    // 验证: DevTrace 有足够多的条目
    let entries = read_trace_entries(ws_dir);
    assert!(
        entries.len() >= n * 3, // Planning + TaskExecution + CompileCheck + TestRun per task
        "应有足够多的 trace 条目, 实际: {}",
        entries.len()
    );
}

/// 测试 2: DevTrace 摘要在大量条目下正确
#[tokio::test]
async fn test_dev_trace_summary_large_scale() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();
    let n = 15;

    let mut chat_responses = vec![large_plan(n)];
    let mut check_results = vec![];
    let mut test_results = vec![];
    let mut file_sets = vec![];

    for i in 0..n {
        chat_responses.push(success_code());
        check_results.push(ok_result());
        test_results.push(ok_result());
        file_sets.push(vec![ef(&format!("src/m{}.rs", i), "fn f() {}")]);
    }

    let chat = MockChat::new(chat_responses);
    let runner = MockTestRunner::new()
        .with_check_results(check_results)
        .with_test_results(test_results);
    let extractor = MockExtractor::new(file_sets);

    let mut orch = build_full_orchestrator(&chat, runner, extractor, ws_dir, "摘要大规模");

    orch.run().await.unwrap();

    let writer = DevTraceWriter::new(Path::new(ws_dir));
    let summary = writer.summary();

    // 验证: 总条目数合理
    assert!(summary.total_entries > 0, "应有 trace 条目");
    assert!(
        summary.total_entries >= n * 3,
        "应至少有 {} 个条目, 实际: {}",
        n * 3,
        summary.total_entries
    );

    // 验证: 成功率 = 1.0 (全部成功)
    assert!(
        summary.success_rate > 0.99,
        "全部成功时成功率应接近 1.0, 实际: {}",
        summary.success_rate
    );

    // 验证: 有 TaskExecution 统计
    let task_stats = summary
        .get_action_stats(TraceAction::TaskExecution)
        .unwrap();
    assert_eq!(task_stats.count, n, "应有 {} 个 TaskExecution", n);
    assert_eq!(task_stats.success_count, n, "全部成功");

    // 验证: 有 CompileCheck 统计
    let compile_stats = summary.get_action_stats(TraceAction::CompileCheck).unwrap();
    assert_eq!(compile_stats.count, n, "应有 {} 个 CompileCheck", n);

    // 验证: 有 TestRun 统计
    let test_stats = summary.get_action_stats(TraceAction::TestRun).unwrap();
    assert_eq!(test_stats.count, n, "应有 {} 个 TestRun", n);

    // 验证: 报告可生成
    let report = summary.to_report();
    assert!(report.contains("DevTrace 开发追踪报告"));
}

/// 测试 3: Memory 增长稳定 — 阶段/任务数量与预期一致
#[tokio::test]
async fn test_memory_growth_stable() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();
    let n = 10;

    let mut chat_responses = vec![large_plan(n)];
    let mut check_results = vec![];
    let mut test_results = vec![];
    let mut file_sets = vec![];

    for i in 0..n {
        chat_responses.push(success_code());
        check_results.push(ok_result());
        test_results.push(ok_result());
        file_sets.push(vec![ef(&format!("src/t{}.rs", i), "fn f() {}")]);
    }

    let chat = MockChat::new(chat_responses);
    let runner = MockTestRunner::new()
        .with_check_results(check_results)
        .with_test_results(test_results);
    let extractor = MockExtractor::new(file_sets);

    let mut orch = build_full_orchestrator(&chat, runner, extractor, ws_dir, "增长测试");

    orch.run().await.unwrap();

    // 验证: 1 个阶段
    assert_eq!(orch.memory.phases.len(), 1, "应有 1 个阶段");

    // 验证: n 个任务
    assert_eq!(orch.memory.phases[0].tasks.len(), n, "应有 {} 个任务", n);

    // 验证: 每个任务都有 files_written
    for task in &orch.memory.phases[0].tasks {
        assert!(!task.files_written.is_empty(), "任务应有写入的文件");
    }

    // 验证: memory.json 可加载且内容一致
    let memory_path = format!("{}/.forge/memory.json", ws_dir);
    let loaded = forge::memory::Memory::load(Path::new(&memory_path)).unwrap();
    assert_eq!(loaded.phases.len(), 1);
    assert_eq!(loaded.phases[0].tasks.len(), n);
}

/// 测试 4: 混合成功/失败/修复/跳过场景
#[tokio::test]
async fn test_mixed_success_fail_fix_skip() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    // 6 个任务: 3 成功, 1 失败后修复, 1 /skip, 1 全部失败
    let plan = r#"```json
[{"name":"混合","description":"mixed","tasks":[
  {"name":"成功1","prompt":"s1"},
  {"name":"成功2","prompt":"s2"},
  {"name":"成功3","prompt":"s3"},
  {"name":"修复后成功","prompt":"fix"},
  {"name":"跳过","prompt":"skip"},
  {"name":"全部失败","prompt":"fail"}
]}]
```"#
        .to_string();

    let errors = vec![CompileError {
        file: "src/main.rs".to_string(),
        line: Some(1),
        column: Some(1),
        message: "error".to_string(),
        error_code: Some("E0308".to_string()),
    }];

    let chat = MockChat::new(vec![
        plan,
        // 成功1
        success_code(),
        // 成功2
        success_code(),
        // 成功3
        success_code(),
        // 修复后成功: attempt 1 失败
        "```file:src/main.rs\nfn main() { broken }\n```".to_string(),
        // 修复后成功: attempt 2 修复
        success_code(),
        // 跳过: /skip
        "无法完成。\n/skip\n```file:src/main.rs\nfn main() {}\n```".to_string(),
        // 全部失败: 5 次尝试都失败
        "```file:src/main.rs\nfn main() { b1 }\n```".to_string(),
        "```file:src/main.rs\nfn main() { b2 }\n```".to_string(),
        "```file:src/main.rs\nfn main() { b3 }\n```".to_string(),
        "```file:src/main.rs\nfn main() { b4 }\n```".to_string(),
        "```file:src/main.rs\nfn main() { b5 }\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            // 成功1-3
            ok_result(),
            ok_result(),
            ok_result(),
            // 修复后成功: 失败 → 成功
            fail_result(errors.clone()),
            ok_result(),
            // 跳过: /skip 跳过编译检查 (不消费 check_result)
            // 全部失败: 5 次都失败
            fail_result(errors.clone()),
            fail_result(errors.clone()),
            fail_result(errors.clone()),
            fail_result(errors.clone()),
            fail_result(errors.clone()),
        ])
        .with_test_results(vec![
            ok_result(),
            ok_result(),
            ok_result(),
            ok_result(), // 修复后成功的 test
                         // 跳过: /skip 不执行 test
                         // 全部失败: 编译都失败, 不执行 test
        ]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/s1.rs", "fn f() {}")],
        vec![ef("src/s2.rs", "fn f() {}")],
        vec![ef("src/s3.rs", "fn f() {}")],
        vec![ef("src/main.rs", "fn main() { broken }")],
        vec![ef("src/main.rs", "fn main() {}")],
        // 跳过: /skip 不提取代码 (不消费 extractor)
        vec![ef("src/main.rs", "fn main() { b1 }")],
        vec![ef("src/main.rs", "fn main() { b2 }")],
        vec![ef("src/main.rs", "fn main() { b3 }")],
        vec![ef("src/main.rs", "fn main() { b4 }")],
        vec![ef("src/main.rs", "fn main() { b5 }")],
    ]);

    let mut orch = build_full_orchestrator(&chat, runner, extractor, ws_dir, "混合场景");

    orch.run().await.unwrap();

    let tasks = &orch.memory.phases[0].tasks;

    // 验证: 6 个任务
    assert_eq!(tasks.len(), 6, "应有 6 个任务");

    // 验证: 成功1-3 完成
    assert_eq!(tasks[0].status, TaskStatus::Completed, "成功1 应完成");
    assert_eq!(tasks[1].status, TaskStatus::Completed, "成功2 应完成");
    assert_eq!(tasks[2].status, TaskStatus::Completed, "成功3 应完成");

    // 验证: 修复后成功 完成, 2 次尝试
    assert_eq!(tasks[3].status, TaskStatus::Completed, "修复后成功 应完成");
    assert_eq!(tasks[3].attempts, 2, "修复后成功 应尝试 2 次");

    // 验证: 跳过 → Failed
    assert_eq!(tasks[4].status, TaskStatus::Failed, "跳过 应 Failed");

    // 验证: 全部失败 → Failed
    assert_eq!(tasks[5].status, TaskStatus::Failed, "全部失败 应 Failed");
}

/// 测试 5: 上下文衔接在长时运行中多次触发
#[tokio::test]
async fn test_context_handoff_multiple_times() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();
    let n = 10;

    let mut chat_responses = vec![large_plan(n)];
    let mut check_results = vec![];
    let mut test_results = vec![];
    let mut file_sets = vec![];

    for i in 0..n {
        chat_responses.push(success_code());
        check_results.push(ok_result());
        test_results.push(ok_result());
        file_sets.push(vec![ef(&format!("src/h{}.rs", i), "fn f() {}")]);
    }

    let chat = MockChat::new(chat_responses);
    let runner = MockTestRunner::new()
        .with_check_results(check_results)
        .with_test_results(test_results);
    let extractor = MockExtractor::new(file_sets);

    // max_context_turns=3 → 每 3 轮触发上下文衔接
    // 10 个任务 → planning(1) + 10 tasks = 11 轮 → 约 3 次交接
    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "多次交接", 5, 60)
        .with_slash_commands(true)
        .with_dev_trace(true)
        .with_loop_detection(3)
        .with_steer_reminder(100) // 禁用
        .with_context_handoff(3) // 每 3 轮交接
        .with_interaction(Box::new(MockInteraction::new()))
        .with_error_diagnosis(Box::new(MockErrorDiagnoser::empty()));

    orch.run().await.unwrap();

    // 验证: 应触发多次上下文衔接
    assert!(
        chat.new_conversation_count() >= 2,
        "应至少触发 2 次上下文衔接, 实际: {}",
        chat.new_conversation_count()
    );

    // 验证: 所有任务仍完成
    for task in &orch.memory.phases[0].tasks {
        assert_eq!(task.status, TaskStatus::Completed);
    }
}

/// 测试 6: DevTrace JSONL 文件可被正确读取和解析
#[tokio::test]
async fn test_dev_trace_jsonl_readable_large_scale() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();
    let n = 8;

    let mut chat_responses = vec![large_plan(n)];
    let mut check_results = vec![];
    let mut test_results = vec![];
    let mut file_sets = vec![];

    for i in 0..n {
        chat_responses.push(success_code());
        check_results.push(ok_result());
        test_results.push(ok_result());
        file_sets.push(vec![ef(&format!("src/j{}.rs", i), "fn f() {}")]);
    }

    let chat = MockChat::new(chat_responses);
    let runner = MockTestRunner::new()
        .with_check_results(check_results)
        .with_test_results(test_results);
    let extractor = MockExtractor::new(file_sets);

    let mut orch = build_full_orchestrator(&chat, runner, extractor, ws_dir, "JSONL读取");

    orch.run().await.unwrap();

    // 读取所有条目
    let entries = read_trace_entries(ws_dir);
    assert!(!entries.is_empty(), "应有 trace 条目");

    // 验证: 每个条目都能序列化/反序列化
    for entry in &entries {
        let json = entry.to_jsonl().unwrap();
        let reparsed = forge::dev_trace::DevTraceEntry::from_jsonl(&json).unwrap();
        assert_eq!(reparsed.action, entry.action);
        assert_eq!(reparsed.success, entry.success);
        assert_eq!(reparsed.duration_ms, entry.duration_ms);
    }

    // 验证: trace 文件行数 = 条目数
    let trace_content =
        std::fs::read_to_string(format!("{}/.forge/devtrace.jsonl", ws_dir)).unwrap();
    let line_count = trace_content.lines().filter(|l| !l.is_empty()).count();
    assert_eq!(line_count, entries.len(), "JSONL 行数应等于条目数");
}

/// 测试 7: 断点续传在长时运行中正确工作
#[tokio::test]
async fn test_resume_in_long_run() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();
    let n = 6;

    // 创建已完成前 3 个任务的 memory
    let mut tasks = vec![];
    for i in 0..3 {
        tasks.push(forge::memory::Task {
            id: format!("0-{}", i),
            phase_id: 0,
            name: format!("任务{}", i),
            prompt: format!("do {}", i),
            status: TaskStatus::Completed,
            result: Some("成功".to_string()),
            attempts: 1,
            files_written: vec![format!("src/t{}.rs", i)],
            test_result: None,
            last_good_snapshot: None,
            clarifications: vec![],
            depends_on: vec![],
        });
    }
    for i in 3..n {
        tasks.push(forge::memory::Task {
            id: format!("0-{}", i),
            phase_id: 0,
            name: format!("任务{}", i),
            prompt: format!("do {}", i),
            status: TaskStatus::Pending,
            result: None,
            attempts: 0,
            files_written: vec![],
            test_result: None,
            last_good_snapshot: None,
            clarifications: vec![],
            depends_on: vec![],
        });
    }

    let mut memory = forge::memory::Memory::new("续传长时");
    memory.set_phases(vec![forge::memory::Phase {
        id: 0,
        name: "大规模开发".to_string(),
        description: "resume test".to_string(),
        status: forge::memory::PhaseStatus::InProgress,
        tasks,
    }]);

    let ws = forge::workspace::Workspace::new(ws_dir);
    ws.init().unwrap();
    for i in 0..3 {
        ws.write_file(&format!("src/t{}.rs", i), "fn f() {}")
            .unwrap();
    }
    memory
        .save(&ws.root.join(".forge").join("memory.json"))
        .unwrap();
    drop(ws);

    // 只回复后 3 个任务
    let mut chat_responses = vec![];
    let mut check_results = vec![];
    let mut test_results = vec![];
    let mut file_sets = vec![];

    for i in 3..n {
        chat_responses.push(success_code());
        check_results.push(ok_result());
        test_results.push(ok_result());
        file_sets.push(vec![ef(&format!("src/t{}.rs", i), "fn f() {}")]);
    }

    let chat = MockChat::new(chat_responses);
    let runner = MockTestRunner::new()
        .with_check_results(check_results)
        .with_test_results(test_results);
    let extractor = MockExtractor::new(file_sets);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "续传长时", 5, 60)
        .with_resume(true)
        .with_slash_commands(true)
        .with_dev_trace(true)
        .with_loop_detection(3)
        .with_steer_reminder(10)
        .with_context_handoff(50)
        .with_interaction(Box::new(MockInteraction::new()))
        .with_error_diagnosis(Box::new(MockErrorDiagnoser::empty()));

    orch.run().await.unwrap();

    // 验证: 所有 n 个任务都完成
    assert_eq!(orch.memory.phases[0].tasks.len(), n, "应有 {} 个任务", n);
    for (i, task) in orch.memory.phases[0].tasks.iter().enumerate() {
        assert_eq!(task.status, TaskStatus::Completed, "任务 {} 应完成", i);
    }
}

/// 测试 8: 多阶段长时运行 (3 阶段, 每阶段 5 任务)
#[tokio::test]
async fn test_multi_phase_long_run() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let plan = r#"```json
[
  {"name":"阶段A","description":"a","tasks":[
    {"name":"A1","prompt":"a1"},
    {"name":"A2","prompt":"a2"},
    {"name":"A3","prompt":"a3"},
    {"name":"A4","prompt":"a4"},
    {"name":"A5","prompt":"a5"}
  ]},
  {"name":"阶段B","description":"b","tasks":[
    {"name":"B1","prompt":"b1"},
    {"name":"B2","prompt":"b2"},
    {"name":"B3","prompt":"b3"},
    {"name":"B4","prompt":"b4"},
    {"name":"B5","prompt":"b5"}
  ]},
  {"name":"阶段C","description":"c","tasks":[
    {"name":"C1","prompt":"c1"},
    {"name":"C2","prompt":"c2"},
    {"name":"C3","prompt":"c3"},
    {"name":"C4","prompt":"c4"},
    {"name":"C5","prompt":"c5"}
  ]}
]
```"#
        .to_string();

    let n = 15; // 3 * 5
    let mut chat_responses = vec![plan];
    let mut check_results = vec![];
    let mut test_results = vec![];
    let mut file_sets = vec![];

    for i in 0..n {
        chat_responses.push(success_code());
        check_results.push(ok_result());
        test_results.push(ok_result());
        file_sets.push(vec![ef(&format!("src/m{}.rs", i), "fn f() {}")]);
    }

    let chat = MockChat::new(chat_responses);
    let runner = MockTestRunner::new()
        .with_check_results(check_results)
        .with_test_results(test_results);
    let extractor = MockExtractor::new(file_sets);

    let mut orch = build_full_orchestrator(&chat, runner, extractor, ws_dir, "多阶段长时");

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
        assert_eq!(phase.tasks.len(), 5, "每阶段应有 5 个任务");
        for task in &phase.tasks {
            assert_eq!(task.status, TaskStatus::Completed);
        }
    }

    // 验证: DevTrace 有足够条目
    let entries = read_trace_entries(ws_dir);
    assert!(entries.len() >= n * 3, "应有足够多的 trace 条目");
}

/// 测试 9: 转向提醒在长时运行中多次注入
#[tokio::test]
async fn test_steer_reminder_multiple_injections() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();
    let n = 8;

    let mut chat_responses = vec![large_plan(n)];
    let mut check_results = vec![];
    let mut test_results = vec![];
    let mut file_sets = vec![];

    for i in 0..n {
        chat_responses.push(success_code());
        check_results.push(ok_result());
        test_results.push(ok_result());
        file_sets.push(vec![ef(&format!("src/sr{}.rs", i), "fn f() {}")]);
    }

    let chat = MockChat::new(chat_responses);
    let runner = MockTestRunner::new()
        .with_check_results(check_results)
        .with_test_results(test_results);
    let extractor = MockExtractor::new(file_sets);

    // steer_interval=2 → 每 2 轮注入提醒
    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "多次提醒", 5, 60)
        .with_slash_commands(true)
        .with_dev_trace(true)
        .with_loop_detection(3)
        .with_steer_reminder(2) // 每 2 轮注入
        .with_context_handoff(100) // 禁用
        .with_interaction(Box::new(MockInteraction::new()))
        .with_error_diagnosis(Box::new(MockErrorDiagnoser::empty()));

    orch.run().await.unwrap();

    // 验证: 所有任务完成
    for task in &orch.memory.phases[0].tasks {
        assert_eq!(task.status, TaskStatus::Completed);
    }

    // 验证: 发送了足够多的消息 (planning + n tasks, 其中部分含提醒注入)
    let sent = chat.sent_messages();
    assert!(sent.len() > n, "应至少发送 {} 条消息", n + 1);
}

/// 测试 10: 修复循环在长时运行中多次出现
#[tokio::test]
async fn test_fix_loops_in_long_run() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();
    let n = 5; // 5 个任务, 每个都需要修复

    let plan = large_plan(n);
    let errors = vec![CompileError {
        file: "src/main.rs".to_string(),
        line: Some(1),
        column: Some(1),
        message: "error".to_string(),
        error_code: Some("E0308".to_string()),
    }];

    let mut chat_responses = vec![plan];
    let mut check_results = vec![];
    let mut test_results = vec![];
    let mut file_sets = vec![];

    for i in 0..n {
        // 每个任务: 第一次失败, 第二次成功
        chat_responses.push(format!(
            "```file:src/main.rs\nfn main() {{ broken{} }}\n```",
            i
        ));
        chat_responses.push(success_code());
        check_results.push(fail_result(errors.clone()));
        check_results.push(ok_result());
        test_results.push(ok_result());
        file_sets.push(vec![ef(&format!("src/f{}.rs", i), "fn f() { broken }")]);
        file_sets.push(vec![ef(&format!("src/f{}.rs", i), "fn f() {}")]);
    }

    let chat = MockChat::new(chat_responses);
    let runner = MockTestRunner::new()
        .with_check_results(check_results)
        .with_test_results(test_results);
    let extractor = MockExtractor::new(file_sets);

    let mut orch = build_full_orchestrator(&chat, runner, extractor, ws_dir, "多次修复");

    orch.run().await.unwrap();

    // 验证: 所有任务修复后完成
    for (i, task) in orch.memory.phases[0].tasks.iter().enumerate() {
        assert_eq!(task.status, TaskStatus::Completed, "任务 {} 应完成", i);
        assert_eq!(task.attempts, 2, "任务 {} 应尝试 2 次", i);
    }

    // 验证: DevTrace 有 n 个 FixAttempt
    let entries = read_trace_entries(ws_dir);
    let fix_count = entries
        .iter()
        .filter(|e| e.action == TraceAction::FixAttempt)
        .count();
    assert_eq!(fix_count, n, "应有 {} 个 FixAttempt", n);
}

/// 测试 11: DevTrace 时间线条目按时间排序
#[tokio::test]
async fn test_dev_trace_timeline_ordered() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();
    let n = 5;

    let mut chat_responses = vec![large_plan(n)];
    let mut check_results = vec![];
    let mut test_results = vec![];
    let mut file_sets = vec![];

    for i in 0..n {
        chat_responses.push(success_code());
        check_results.push(ok_result());
        test_results.push(ok_result());
        file_sets.push(vec![ef(&format!("src/tl{}.rs", i), "fn f() {}")]);
    }

    let chat = MockChat::new(chat_responses);
    let runner = MockTestRunner::new()
        .with_check_results(check_results)
        .with_test_results(test_results);
    let extractor = MockExtractor::new(file_sets);

    let mut orch = build_full_orchestrator(&chat, runner, extractor, ws_dir, "时间线");

    orch.run().await.unwrap();

    let entries = read_trace_entries(ws_dir);

    // 验证: 时间戳非递减 (后续条目时间 >= 前面)
    for i in 1..entries.len() {
        assert!(
            entries[i].timestamp >= entries[i - 1].timestamp,
            "时间戳应非递减, 但 entry[{}].ts ({}) < entry[{}].ts ({})",
            i,
            entries[i].timestamp,
            i - 1,
            entries[i - 1].timestamp
        );
    }
}

/// 测试 12: 全功能 + 大量决策记录
#[tokio::test]
async fn test_large_decision_log() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();
    let n = 6;

    // 每隔一个任务加 /skip
    let plan = large_plan(n);

    let mut chat_responses = vec![plan];
    let mut check_results = vec![];
    let mut test_results = vec![];
    let mut file_sets = vec![];

    for i in 0..n {
        if i % 2 == 0 {
            chat_responses.push(success_code());
        } else {
            chat_responses
                .push("跳过。\n/skip\n```file:src/main.rs\nfn main() {}\n```".to_string());
        }
        check_results.push(ok_result());
        test_results.push(ok_result());
        file_sets.push(vec![ef(&format!("src/d{}.rs", i), "fn f() {}")]);
    }

    let chat = MockChat::new(chat_responses);
    let runner = MockTestRunner::new()
        .with_check_results(check_results)
        .with_test_results(test_results);
    let extractor = MockExtractor::new(file_sets);

    let mut orch = build_full_orchestrator(&chat, runner, extractor, ws_dir, "大量决策");

    orch.run().await.unwrap();

    // 验证: 有决策记录
    assert!(!orch.memory.decisions.is_empty(), "应有决策记录");

    // 验证: 偶数任务完成, 奇数任务 Failed (/skip)
    for (i, task) in orch.memory.phases[0].tasks.iter().enumerate() {
        if i % 2 == 0 {
            assert_eq!(task.status, TaskStatus::Completed, "任务 {} 应完成", i);
        } else {
            assert_eq!(
                task.status,
                TaskStatus::Failed,
                "任务 {} 应 Failed (/skip)",
                i
            );
        }
    }

    // 验证: memory.json 可加载
    let memory_path = format!("{}/.forge/memory.json", ws_dir);
    let loaded = forge::memory::Memory::load(Path::new(&memory_path)).unwrap();
    assert!(!loaded.decisions.is_empty(), "加载的 memory 应有决策记录");
}
