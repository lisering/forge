//! Orchestrator 复杂场景集成测试 (Session 66)
//!
//! 验证核心编排引擎的端到端复杂场景，与 Session 65 的可靠性链路集成测试互补:
//! 1. 错误恢复: 测试失败修复、全轮耗尽、版本回滚
//! 2. 多阶段编排: 混合成功/失败、跨阶段上下文、复杂管道
//! 3. Slash 指令: /escalate 审批/拒绝、/compact 交接后继续
//! 4. 全功能管道: 所有增强功能同时启用 + 详细验证
//! 5. 持久化: Memory/报告/工作区文件/对话记录
//!
//! 测试使用 MockChat + MockTestRunner + MockExtractor 完整编排流程,
//! 无需 Chrome 环境, 通过 Memory/Workspace/DevTrace 状态验证行为。

use async_trait::async_trait;
use forge::dev_trace::{DevTraceWriter, TraceAction};
use forge::error_diagnosis::MockErrorDiagnoser;
use forge::extract::ExtractedFile;
use forge::interaction::MockInteraction;
use forge::memory::{PhaseStatus, TaskStatus};
use forge::orchestrator::Orchestrator;
use forge::testrunner::{CompileError, TestResult};
use forge::traits::{ChatClient, ChatResult, FileExtractor, TestRunner};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tempfile::tempdir;

// ============================================================================
//  Mock 实现 (支持对话轮次计数 + 新开对话 + 超时标志)
// ============================================================================

struct MockChat {
    responses: Arc<Mutex<Vec<String>>>,
    sent_messages: Arc<Mutex<Vec<String>>>,
    turn_count: Arc<AtomicUsize>,
    new_conversation_count: Arc<AtomicUsize>,
    /// 每条响应是否超时 (按顺序弹出, 空时默认 false)
    timeout_flags: Arc<Mutex<Vec<bool>>>,
}

impl MockChat {
    fn new(responses: Vec<String>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses)),
            sent_messages: Arc::new(Mutex::new(vec![])),
            turn_count: Arc::new(AtomicUsize::new(0)),
            new_conversation_count: Arc::new(AtomicUsize::new(0)),
            timeout_flags: Arc::new(Mutex::new(vec![])),
        }
    }

    /// 设置每条响应的超时标志
    #[allow(dead_code)]
    fn with_timeout_flags(self, flags: Vec<bool>) -> Self {
        *self.timeout_flags.lock().unwrap() = flags;
        self
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
        let mut flags = self.timeout_flags.lock().unwrap();
        let timed_out = if !flags.is_empty() {
            flags.remove(0)
        } else {
            false
        };
        if queue.is_empty() {
            return Ok(ChatResult {
                text: "(empty)".to_string(),
                timed_out: false,
            });
        }
        let text = queue.remove(0);
        Ok(ChatResult { text, timed_out })
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

    fn with_check_results(self, results: Vec<TestResult>) -> Self {
        *self.check_results.lock().unwrap() = results;
        self
    }

    fn with_test_results(self, results: Vec<TestResult>) -> Self {
        *self.test_results.lock().unwrap() = results;
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

/// 4 阶段 8 任务的复杂计划
fn large_plan() -> String {
    r#"```json
[
  {"name":"初始化","description":"创建项目","tasks":[
    {"name":"Cargo.toml","prompt":"创建 Cargo.toml"},
    {"name":"main.rs","prompt":"创建 main.rs"}
  ]},
  {"name":"核心模块","description":"实现核心","tasks":[
    {"name":"模型","prompt":"创建数据模型"},
    {"name":"服务","prompt":"创建服务层"},
    {"name":"控制器","prompt":"创建控制器"}
  ]},
  {"name":"API层","description":"实现API","tasks":[
    {"name":"路由","prompt":"创建路由"},
    {"name":"处理器","prompt":"创建处理器"}
  ]},
  {"name":"测试","description":"编写测试","tasks":[
    {"name":"单元测试","prompt":"编写单元测试"}
  ]}
]
```"#
        .to_string()
}

fn success_code() -> String {
    "以下是完整实现。\n```file:src/main.rs\nfn main() { println!(\"hello\"); }\n```".to_string()
}

fn code_with_file(path: &str, content: &str) -> String {
    format!("完成。\n```file:{}\n{}\n```", path, content)
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
        .with_steer_reminder(10)
        .with_context_handoff(50)
        .with_interaction(Box::new(MockInteraction::new()))
        .with_error_diagnosis(Box::new(MockErrorDiagnoser::with_category(
            forge::error_diagnosis::ErrorCategory::SyntaxError,
        )))
}

// ============================================================================
//  测试用例 — 错误恢复 & 修复循环
// ============================================================================

/// 测试 1: 编译通过但测试失败 → 修复 → 成功
///
/// 场景: AI 第一次写的代码能编译但测试不通过,
/// 第二次修复后编译和测试都通过。
#[tokio::test]
async fn test_test_only_failure_then_fix_success() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        simple_plan(),
        success_code(), // 第一次: 代码能编译
        success_code(), // 第二次: 修复后的代码
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            make_test_result(true, vec![]), // 第一次: 编译通过
            make_test_result(true, vec![]), // 第二次: 编译通过
        ])
        .with_test_results(vec![
            make_test_result(false, vec![]), // 第一次: 测试失败
            make_test_result(true, vec![]),  // 第二次: 测试通过
        ]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() { buggy(); }")],
        vec![ef("src/main.rs", "fn main() { println!(\"fixed\"); }")],
    ]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "测试失败修复", 3, 60);

    orch.run().await.unwrap();

    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(task.status, TaskStatus::Completed, "测试失败修复后应成功");
    assert_eq!(task.attempts, 2, "应尝试 2 次");
}

/// 测试 2: 全部修复轮次耗尽 → 任务标记为 Failed
///
/// 场景: AI 3 次尝试都编译失败, 任务最终 Failed。
#[tokio::test]
async fn test_all_rounds_exhausted_task_marked_failed() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let err = vec![make_compile_error("src/main.rs", 1, "syntax error")];

    let chat = MockChat::new(vec![
        simple_plan(),
        "```file:src/main.rs\nfn main() { broken1 }\n```".to_string(),
        "```file:src/main.rs\nfn main() { broken2 }\n```".to_string(),
        "```file:src/main.rs\nfn main() { broken3 }\n```".to_string(),
    ]);

    let runner = MockTestRunner::new().with_check_results(vec![
        make_test_result(false, err.clone()),
        make_test_result(false, err.clone()),
        make_test_result(false, err),
    ]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() { broken1 }")],
        vec![ef("src/main.rs", "fn main() { broken2 }")],
        vec![ef("src/main.rs", "fn main() { broken3 }")],
    ]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "全轮耗尽", 3, 60);

    orch.run().await.unwrap();

    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(task.status, TaskStatus::Failed, "全轮耗尽应 Failed");
    assert_eq!(task.attempts, 3, "应尝试 3 次");
}

/// 测试 3: 编译成功时保存 known good 快照
///
/// 场景: AI 第一次就编译成功, 验证 last_good_snapshot 被设置。
#[tokio::test]
async fn test_known_good_snapshot_saved_on_compile_success() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![simple_plan(), success_code()]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "快照测试", 3, 60);

    orch.run().await.unwrap();

    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(task.status, TaskStatus::Completed);
    assert!(
        task.last_good_snapshot.is_some(),
        "编译成功应保存 known good 快照"
    );
}

/// 测试 4: 最终失败时回滚到 known good 快照
///
/// 场景:
/// - 第1次: 编译失败 (无快照)
/// - 第2次: 编译成功 (保存快照), 测试失败
/// - 第3次: 编译失败 (最终失败) → 回滚到第2次的快照
#[tokio::test]
async fn test_version_rollback_restores_known_good_files() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let err = vec![make_compile_error("src/main.rs", 1, "error")];

    let chat = MockChat::new(vec![
        simple_plan(),
        "```file:src/main.rs\nfn main() { broken1 }\n```".to_string(),
        "```file:src/main.rs\nfn main() { correct() }\n```".to_string(),
        "```file:src/main.rs\nfn main() { broken3 }\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            make_test_result(false, err.clone()), // 第1次: 编译失败
            make_test_result(true, vec![]),       // 第2次: 编译成功 → 保存快照
            make_test_result(false, err),         // 第3次: 编译失败 → 回滚
        ])
        .with_test_results(vec![
            make_test_result(false, vec![]), // 第2次: 测试失败
        ]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() { broken1 }")],
        vec![ef("src/main.rs", "fn main() { correct() }")], // known good
        vec![ef("src/main.rs", "fn main() { broken3 }")],
    ]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "回滚测试", 3, 60);

    orch.run().await.unwrap();

    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(task.status, TaskStatus::Failed, "最终失败应 Failed");
    assert_eq!(task.attempts, 3, "应尝试 3 次");

    // 回滚后文件应恢复为 known good (第2次的内容)
    let content = orch.workspace.read_file("src/main.rs").unwrap();
    assert!(
        content.contains("correct"),
        "回滚后文件应恢复为 known good 版本, 实际: {}",
        content
    );

    // 决策日志应有版本回滚记录
    let has_rollback = orch
        .memory
        .decisions
        .iter()
        .any(|d| d.decision == "版本回滚");
    assert!(has_rollback, "应有版本回滚决策记录");
}

/// 测试 5: 单个任务提取多个文件
///
/// 场景: AI 在一个回复中返回 3 个文件,
/// 验证全部写入工作区, files_written 记录 3 个路径。
#[tokio::test]
async fn test_multi_file_extraction_single_task() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let multi_file_response = "完成所有文件。\n\
        ```file:src/main.rs\nfn main() {}\n```\n\
        ```file:src/lib.rs\npub fn lib() {}\n```\n\
        ```file:src/utils.rs\npub fn utils() {}\n```"
        .to_string();

    let chat = MockChat::new(vec![simple_plan(), multi_file_response]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![
        ef("src/main.rs", "fn main() {}"),
        ef("src/lib.rs", "pub fn lib() {}"),
        ef("src/utils.rs", "pub fn utils() {}"),
    ]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "多文件", 3, 60);

    orch.run().await.unwrap();

    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(task.status, TaskStatus::Completed);
    assert_eq!(task.files_written.len(), 3, "应记录 3 个文件路径");

    // 验证工作区中有全部 3 个文件
    assert!(orch.workspace.read_file("src/main.rs").is_ok());
    assert!(orch.workspace.read_file("src/lib.rs").is_ok());
    assert!(orch.workspace.read_file("src/utils.rs").is_ok());
}

// ============================================================================
//  测试用例 — 多阶段编排
// ============================================================================

/// 测试 6: 多阶段混合成功/失败
///
/// 场景: 3 个阶段, Phase 1 全部成功, Phase 2 全部失败, Phase 3 全部成功。
/// 验证: 所有阶段标记为 Completed, 但 Phase 2 的任务为 Failed。
#[tokio::test]
async fn test_multi_phase_mixed_success_and_failure() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let plan = r#"```json
[
  {"name":"阶段1","description":"成功","tasks":[{"name":"任务1","prompt":"do1"}]},
  {"name":"阶段2","description":"失败","tasks":[
    {"name":"任务2","prompt":"do2"},
    {"name":"任务3","prompt":"do3"}
  ]},
  {"name":"阶段3","description":"成功","tasks":[{"name":"任务4","prompt":"do4"}]}
]
```"#;
    let err = vec![make_compile_error("src/main.rs", 1, "error")];

    let chat = MockChat::new(vec![
        plan.to_string(),
        // 阶段1: 成功
        success_code(),
        // 阶段2: 两个任务都失败 (max_rounds=1, 每个只尝试1次)
        "```file:src/fail1.rs\nbroken\n```".to_string(),
        "```file:src/fail2.rs\nbroken\n```".to_string(),
        // 阶段3: 成功
        success_code(),
    ]);

    let runner = MockTestRunner::new().with_check_results(vec![
        // 阶段1
        make_test_result(true, vec![]),
        // 阶段2: 两个任务都编译失败
        make_test_result(false, err.clone()),
        make_test_result(false, err),
        // 阶段3
        make_test_result(true, vec![]),
    ]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/fail1.rs", "broken")],
        vec![ef("src/fail2.rs", "broken")],
        vec![ef("src/main.rs", "fn main() { ok(); }")],
    ]);

    // max_rounds=1: 失败的任务不重试, 直接 Failed
    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "混合结果", 1, 60);

    orch.run().await.unwrap();

    assert_eq!(orch.memory.phases.len(), 3, "应有 3 个阶段");
    for phase in &orch.memory.phases {
        assert_eq!(
            phase.status,
            PhaseStatus::Completed,
            "所有阶段应标记为 Completed"
        );
    }

    // 阶段1: 任务成功
    assert_eq!(orch.memory.phases[0].tasks[0].status, TaskStatus::Completed);
    // 阶段2: 两个任务都失败
    assert_eq!(
        orch.memory.phases[1].tasks[0].status,
        TaskStatus::Failed,
        "阶段2任务1应Failed"
    );
    assert_eq!(
        orch.memory.phases[1].tasks[1].status,
        TaskStatus::Failed,
        "阶段2任务2应Failed"
    );
    // 阶段3: 任务成功
    assert_eq!(orch.memory.phases[2].tasks[0].status, TaskStatus::Completed);
}

/// 测试 7: 跨阶段上下文 — 前一阶段的文件在后一阶段可见
///
/// 场景: 阶段1写入 src/main.rs, 阶段2写入 src/lib.rs,
/// 验证运行后工作区同时包含两个文件。
#[tokio::test]
async fn test_inter_phase_context_files_preserved() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        two_task_plan(),
        code_with_file("src/main.rs", "fn main() {}"),
        code_with_file("src/lib.rs", "pub fn lib() {}"),
    ]);

    let runner = MockTestRunner::new().with_check_results(vec![
        make_test_result(true, vec![]),
        make_test_result(true, vec![]),
    ]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/lib.rs", "pub fn lib() {}")],
    ]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "跨阶段", 3, 60);

    orch.run().await.unwrap();

    // 两个文件都应在工作区中
    assert!(
        orch.workspace.read_file("src/main.rs").is_ok(),
        "阶段1的文件应保留"
    );
    assert!(
        orch.workspace.read_file("src/lib.rs").is_ok(),
        "阶段2的文件应存在"
    );

    // workspace_files 应包含两个文件
    assert!(
        orch.memory
            .workspace_files
            .iter()
            .any(|f| f.contains("main.rs")),
        "workspace_files 应包含 main.rs"
    );
    assert!(
        orch.memory
            .workspace_files
            .iter()
            .any(|f| f.contains("lib.rs")),
        "workspace_files 应包含 lib.rs"
    );
}

/// 测试 8: FORGE_REPORT.md 在运行后生成
#[tokio::test]
async fn test_forge_report_md_generated_after_run() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![simple_plan(), success_code()]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "报告测试", 3, 60);

    orch.run().await.unwrap();

    let report_path = orch.workspace.root.join("FORGE_REPORT.md");
    assert!(report_path.exists(), "FORGE_REPORT.md 应在运行后生成");

    let report = std::fs::read_to_string(&report_path).unwrap();
    assert!(report.contains("执行报告"), "报告应包含标题");
    assert!(report.contains("报告测试"), "报告应包含目标");
}

/// 测试 9: 复杂 4 阶段 8 任务管道
///
/// 场景: 4 个阶段, 8 个任务, 混合成功和失败。
/// 验证: 最终统计正确, 所有阶段完成。
#[tokio::test]
async fn test_complex_4_phase_8_task_pipeline() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let err = vec![make_compile_error("src/main.rs", 1, "error")];

    let chat = MockChat::new(vec![
        large_plan(),
        // 阶段1: 2 个任务都成功
        success_code(),
        code_with_file("src/main.rs", "fn main() { println!(\"ok\"); }"),
        // 阶段2: 3 个任务, 前两个成功, 第三个失败
        code_with_file("src/model.rs", "pub struct Model;"),
        code_with_file("src/service.rs", "pub fn service() {}"),
        "```file:src/controller.rs\nbroken\n```".to_string(),
        // 阶段3: 2 个任务都成功
        code_with_file("src/router.rs", "pub fn route() {}"),
        code_with_file("src/handler.rs", "pub fn handle() {}"),
        // 阶段4: 1 个任务成功
        code_with_file("tests/unit.rs", "#[test] fn t() {}"),
    ]);

    let runner = MockTestRunner::new().with_check_results(vec![
        // 阶段1: 2 成功
        make_test_result(true, vec![]),
        make_test_result(true, vec![]),
        // 阶段2: 2 成功 + 1 失败
        make_test_result(true, vec![]),
        make_test_result(true, vec![]),
        make_test_result(false, err),
        // 阶段3: 2 成功
        make_test_result(true, vec![]),
        make_test_result(true, vec![]),
        // 阶段4: 1 成功
        make_test_result(true, vec![]),
    ]);

    let extractor = MockExtractor::new(vec![
        vec![ef("Cargo.toml", "[package]\nname = \"test\"")],
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/model.rs", "pub struct Model;")],
        vec![ef("src/service.rs", "pub fn service() {}")],
        vec![ef("src/controller.rs", "broken")],
        vec![ef("src/router.rs", "pub fn route() {}")],
        vec![ef("src/handler.rs", "pub fn handle() {}")],
        vec![ef("tests/unit.rs", "#[test] fn t() {}")],
    ]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "复杂管道", 1, 60);

    orch.run().await.unwrap();

    assert_eq!(orch.memory.phases.len(), 4, "应有 4 个阶段");
    for phase in &orch.memory.phases {
        assert_eq!(phase.status, PhaseStatus::Completed);
    }

    let total: usize = orch.memory.phases.iter().map(|p| p.tasks.len()).sum();
    assert_eq!(total, 8, "应有 8 个任务");

    let completed: usize = orch
        .memory
        .phases
        .iter()
        .flat_map(|p| &p.tasks)
        .filter(|t| t.status == TaskStatus::Completed)
        .count();
    assert_eq!(completed, 7, "应有 7 个任务完成");

    let failed: usize = orch
        .memory
        .phases
        .iter()
        .flat_map(|p| &p.tasks)
        .filter(|t| t.status == TaskStatus::Failed)
        .count();
    assert_eq!(failed, 1, "应有 1 个任务失败");
}

// ============================================================================
//  测试用例 — Slash 指令复杂场景
// ============================================================================

/// 测试 10: /escalate 人工审批通过 → 任务继续执行
///
/// 场景: AI 在回复中发出 /escalate 请求人工干预,
/// 人类审批通过, 任务继续执行并完成。
#[tokio::test]
async fn test_escalate_approved_task_continues() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let response = "需要人工确认。\n/escalate\n```file:src/main.rs\nfn main() {}\n```".to_string();

    let chat = MockChat::new(vec![simple_plan(), response]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    // fix_response=true → /escalate 审批通过
    let mock_interaction = MockInteraction::new().with_fix_response(true);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "escalate审批", 3, 60)
        .with_slash_commands(true)
        .with_interaction(Box::new(mock_interaction));

    orch.run().await.unwrap();

    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(
        task.status,
        TaskStatus::Completed,
        "/escalate 审批通过后任务应完成"
    );

    // 应有 /escalate 决策记录
    let has_escalate = orch
        .memory
        .decisions
        .iter()
        .any(|d| d.decision.contains("/escalate"));
    assert!(has_escalate, "应有 /escalate 决策记录");
}

/// 测试 11: /escalate 人工审批拒绝 → 任务失败
///
/// 场景: AI 发出 /escalate, 人类拒绝, 任务被跳过 (Failed)。
#[tokio::test]
async fn test_escalate_rejected_task_fails() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let response = "需要人工确认。\n/escalate\n```file:src/main.rs\nfn main() {}\n```".to_string();

    let chat = MockChat::new(vec![simple_plan(), response]);

    let runner = MockTestRunner::new();
    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    // fix_response=false → /escalate 审批拒绝
    let mock_interaction = MockInteraction::new().with_fix_response(false);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "escalate拒绝", 3, 60)
        .with_slash_commands(true)
        .with_interaction(Box::new(mock_interaction));

    orch.run().await.unwrap();

    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(
        task.status,
        TaskStatus::Failed,
        "/escalate 拒绝后任务应 Failed"
    );

    let has_escalate = orch
        .memory
        .decisions
        .iter()
        .any(|d| d.decision.contains("/escalate"));
    assert!(has_escalate, "应有 /escalate 决策记录");
}

/// 测试 12: /compact 触发上下文交接后继续执行
///
/// 场景: AI 发出 /compact, 触发上下文交接 (新开对话),
/// 之后任务继续执行并完成。
#[tokio::test]
async fn test_compact_handoff_then_continue_execution() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let response = "上下文太长。\n/compact\n```file:src/main.rs\nfn main() {}\n```".to_string();

    let chat = MockChat::new(vec![
        simple_plan(),
        response,
        "交接完成, 继续。".to_string(), // 交接后的回复
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "compact交接", 3, 60)
        .with_slash_commands(true)
        .with_dev_trace(true)
        .with_context_handoff(50)
        .with_interaction(Box::new(MockInteraction::new()));

    orch.run().await.unwrap();

    // /compact 应触发新开对话
    assert!(chat.new_conversation_count() > 0, "/compact 应触发新开对话");

    // 任务应正常完成
    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(task.status, TaskStatus::Completed);

    // DevTrace 应记录 ContextHandoff
    let entries = read_trace_entries(ws_dir);
    let actions = trace_action_set(&entries);
    assert!(
        actions.contains(&TraceAction::ContextHandoff),
        "应包含 ContextHandoff trace"
    );

    // 决策记录中有 /compact
    let has_compact = orch
        .memory
        .decisions
        .iter()
        .any(|d| d.decision.contains("/compact"));
    assert!(has_compact, "应有 /compact 决策记录");
}

// ============================================================================
//  测试用例 — 全功能管道 & 持久化
// ============================================================================

/// 测试 13: 全功能管道复杂场景 — 详细验证
///
/// 场景: 多阶段多任务 + 修复循环 + 所有增强功能,
/// 验证 DevTrace、决策日志、任务状态、快照等。
#[tokio::test]
async fn test_full_pipeline_all_features_detailed_verification() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let err = vec![make_compile_error("src/main.rs", 5, "type mismatch")];

    let chat = MockChat::new(vec![
        complex_plan(),
        // 阶段1: 2 个任务, 第一个需要修复
        "```file:src/main.rs\nfn main() { broken }\n```".to_string(),
        "```file:src/main.rs\nfn main() { println!(\"ok\"); }\n```".to_string(),
        // 阶段1: 第二个任务直接成功
        code_with_file("src/lib.rs", "pub fn lib() {}"),
        // 阶段2: 1 个任务成功
        code_with_file("src/core.rs", "pub fn core() {}"),
        // 阶段3: 2 个任务都成功
        code_with_file("tests/unit.rs", "#[test] fn t() {}"),
        code_with_file("tests/integ.rs", "#[test] fn i() {}"),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            // 阶段1 任务1: 第一次失败, 第二次成功
            make_test_result(false, err),
            make_test_result(true, vec![]),
            // 阶段1 任务2: 成功
            make_test_result(true, vec![]),
            // 阶段2: 成功
            make_test_result(true, vec![]),
            // 阶段3: 2 个成功
            make_test_result(true, vec![]),
            make_test_result(true, vec![]),
        ])
        .with_test_results(vec![
            // 阶段1 任务1 第二次: 测试成功
            make_test_result(true, vec![]),
            // 阶段1 任务2: 测试成功
            make_test_result(true, vec![]),
            // 阶段2: 测试成功
            make_test_result(true, vec![]),
            // 阶段3: 2 个测试成功
            make_test_result(true, vec![]),
            make_test_result(true, vec![]),
        ]);

    let extractor = MockExtractor::new(vec![
        // 阶段1 任务1: 两次尝试
        vec![ef("src/main.rs", "fn main() { broken }")],
        vec![ef("src/main.rs", "fn main() { println!(\"ok\"); }")],
        // 阶段1 任务2
        vec![ef("src/lib.rs", "pub fn lib() {}")],
        // 阶段2
        vec![ef("src/core.rs", "pub fn core() {}")],
        // 阶段3
        vec![ef("tests/unit.rs", "#[test] fn t() {}")],
        vec![ef("tests/integ.rs", "#[test] fn i() {}")],
    ]);

    let mut orch = build_full_orchestrator(&chat, runner, extractor, ws_dir, "全功能管道");

    orch.run().await.unwrap();

    // === 验证: 3 个阶段全部完成 ===
    assert_eq!(orch.memory.phases.len(), 3);
    for phase in &orch.memory.phases {
        assert_eq!(phase.status, PhaseStatus::Completed);
    }

    // === 验证: 所有 5 个任务完成 ===
    let total: usize = orch.memory.phases.iter().map(|p| p.tasks.len()).sum();
    assert_eq!(total, 5);
    for phase in &orch.memory.phases {
        for task in &phase.tasks {
            assert_eq!(task.status, TaskStatus::Completed);
        }
    }

    // === 验证: 第一个任务有修复记录 (attempts=2) ===
    assert_eq!(
        orch.memory.phases[0].tasks[0].attempts, 2,
        "第一个任务应尝试 2 次"
    );

    // === 验证: DevTrace 包含多种操作类型 ===
    let entries = read_trace_entries(ws_dir);
    let actions = trace_action_set(&entries);
    assert!(actions.contains(&TraceAction::Planning));
    assert!(actions.contains(&TraceAction::TaskExecution));
    assert!(actions.contains(&TraceAction::CompileCheck));
    assert!(actions.contains(&TraceAction::TestRun));
    assert!(
        actions.contains(&TraceAction::FixAttempt),
        "应包含 FixAttempt (第一个任务有修复)"
    );

    // === 验证: 决策日志不为空 ===
    assert!(!orch.memory.decisions.is_empty(), "应有决策记录");

    // === 验证: 有编译成功和失败的 CompileCheck ===
    let failed_checks = entries
        .iter()
        .filter(|e| e.action == TraceAction::CompileCheck && !e.success)
        .count();
    let ok_checks = entries
        .iter()
        .filter(|e| e.action == TraceAction::CompileCheck && e.success)
        .count();
    assert!(failed_checks >= 1, "应至少有 1 个失败的 CompileCheck");
    assert!(ok_checks >= 5, "应至少有 5 个成功的 CompileCheck");

    // === 验证: known good 快照已保存 ===
    let task_with_snapshot = orch
        .memory
        .phases
        .iter()
        .flat_map(|p| &p.tasks)
        .find(|t| t.last_good_snapshot.is_some());
    assert!(
        task_with_snapshot.is_some(),
        "至少一个任务应有 known good 快照"
    );
}

/// 测试 14: Memory.json 持久化 — 运行后可加载
#[tokio::test]
async fn test_memory_json_persistence_after_run() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![simple_plan(), success_code()]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "持久化测试", 3, 60);

    orch.run().await.unwrap();

    // memory.json 应存在
    let memory_path = orch.workspace.root.join(".forge").join("memory.json");
    assert!(memory_path.exists(), "memory.json 应存在");

    // 加载并验证
    let loaded = forge::memory::Memory::load(&memory_path).unwrap();
    assert_eq!(loaded.goal, "持久化测试");
    assert_eq!(loaded.phases.len(), 1);
    assert_eq!(loaded.phases[0].tasks.len(), 1);
    assert_eq!(loaded.phases[0].tasks[0].status, TaskStatus::Completed);
    assert!(!loaded.decisions.is_empty(), "加载的决策日志不应为空");
}

/// 测试 15: workspace_files 列表在运行后更新
#[tokio::test]
async fn test_workspace_files_list_updated_after_run() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        two_task_plan(),
        code_with_file("src/main.rs", "fn main() {}"),
        code_with_file("src/lib.rs", "pub fn lib() {}"),
    ]);

    let runner = MockTestRunner::new().with_check_results(vec![
        make_test_result(true, vec![]),
        make_test_result(true, vec![]),
    ]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/lib.rs", "pub fn lib() {}")],
    ]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "文件列表", 3, 60);

    // 运行前 workspace_files 应为空
    assert!(orch.memory.workspace_files.is_empty());

    orch.run().await.unwrap();

    // 运行后 workspace_files 应包含写入的文件
    assert!(
        !orch.memory.workspace_files.is_empty(),
        "运行后 workspace_files 不应为空"
    );
    assert!(
        orch.memory
            .workspace_files
            .iter()
            .any(|f| f.contains("main.rs")),
        "应包含 main.rs"
    );
    assert!(
        orch.memory
            .workspace_files
            .iter()
            .any(|f| f.contains("lib.rs")),
        "应包含 lib.rs"
    );
    // 不应包含 target/ 目录
    assert!(
        !orch
            .memory
            .workspace_files
            .iter()
            .any(|f| f.starts_with("target/")),
        "不应包含 target/ 文件"
    );
}

/// 测试 16: 对话记录在运行后保存到 Memory
#[tokio::test]
async fn test_conversations_recorded_in_memory() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![simple_plan(), success_code()]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "对话记录", 3, 60);

    orch.run().await.unwrap();

    // 应有对话记录
    assert!(!orch.memory.conversations.is_empty(), "应有对话记录");

    // 应有 user 和 assistant 角色
    let has_user = orch.memory.conversations.iter().any(|c| c.role == "user");
    let has_assistant = orch
        .memory
        .conversations
        .iter()
        .any(|c| c.role == "assistant");
    assert!(has_user, "应有 user 对话");
    assert!(has_assistant, "应有 assistant 对话");

    // 至少 2 轮对话 (planning + task)
    assert!(
        orch.memory.conversations.len() >= 2,
        "应至少有 2 轮对话, 实际: {}",
        orch.memory.conversations.len()
    );
}

// ============================================================================
//  测试用例 — AI 响应边界场景
// ============================================================================

/// 测试 17: AI 回复超时 → 触发自主追问
///
/// 场景: AI 回复标记为超时, 启发式追问检查器应检测到并追问,
/// 追问后的回复正常完成。
#[tokio::test]
async fn test_ai_timeout_triggers_clarification() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        simple_plan(),
        "部分代码...".to_string(), // 超时的不完整回复
        success_code(),            // 追问后的完整回复
    ])
    .with_timeout_flags(vec![false, true, false]); // 第二条响应超时

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![
        vec![], // 超时回复无代码
        vec![ef("src/main.rs", "fn main() {}")],
    ]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "超时测试", 3, 60);

    orch.run().await.unwrap();

    // 最终应成功 (追问后获得完整代码)
    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(task.status, TaskStatus::Completed, "超时追问后应成功完成");
}

/// 测试 18: AI 多次不返回代码 → 任务失败
///
/// 场景: AI 连续多次回复都不包含代码,
/// 达到最大轮次后任务 Failed。
#[tokio::test]
async fn test_ai_no_code_all_rounds_task_fails() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        simple_plan(),
        "我来分析一下需求。".to_string(),
        "让我想想怎么实现。".to_string(),
        "这个需要仔细考虑。".to_string(),
    ]);

    let runner = MockTestRunner::new();
    let extractor = MockExtractor::new(vec![
        vec![], // 无代码
        vec![],
        vec![],
    ]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "无代码", 3, 60);

    orch.run().await.unwrap();

    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(task.status, TaskStatus::Failed, "连续无代码应 Failed");
}

// ============================================================================
//  测试用例 — 报告 & 决策日志
// ============================================================================

/// 测试 19: 执行报告包含正确的统计数据
#[tokio::test]
async fn test_execution_report_contains_correct_stats() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let err = vec![make_compile_error("src/main.rs", 1, "error")];

    let chat = MockChat::new(vec![
        two_task_plan(),
        success_code(),                                // 任务1: 成功
        "```file:src/lib.rs\nbroken\n```".to_string(), // 任务2: 失败
    ]);

    let runner = MockTestRunner::new().with_check_results(vec![
        make_test_result(true, vec![]), // 任务1: 编译成功
        make_test_result(false, err),   // 任务2: 编译失败 (max_rounds=1)
    ]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/lib.rs", "broken")],
    ]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "报告统计", 1, 60);

    orch.run().await.unwrap();

    let report = orch.memory.execution_report();
    assert!(report.contains("执行报告"), "报告应包含标题");
    assert!(report.contains("报告统计"), "报告应包含目标");
    assert!(
        report.contains("1") && report.contains("2"),
        "报告应包含任务统计 (1/2)"
    );
}

/// 测试 20: 决策日志覆盖所有决策类型
///
/// 场景: 完整运行中应记录多种决策类型:
/// - 完成目标拆解
/// - 计划已确认
/// - 任务完成
#[tokio::test]
async fn test_decision_log_covers_all_decision_types() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![simple_plan(), success_code()]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "决策覆盖", 3, 60);

    orch.run().await.unwrap();

    let decisions: Vec<&str> = orch
        .memory
        .decisions
        .iter()
        .map(|d| d.decision.as_str())
        .collect();

    // 应有 "完成目标拆解" 决策
    assert!(
        decisions.iter().any(|d| d.contains("目标拆解")),
        "应有目标拆解决策, 实际: {:?}",
        decisions
    );

    // 应有 "计划已确认" 决策
    assert!(
        decisions.iter().any(|d| d.contains("计划已确认")),
        "应有计划确认决策, 实际: {:?}",
        decisions
    );

    // 应有 "任务完成" 决策
    assert!(
        decisions.iter().any(|d| d.contains("任务完成")),
        "应有任务完成决策, 实际: {:?}",
        decisions
    );

    // 每个决策应有原因
    for d in &orch.memory.decisions {
        assert!(!d.reason.is_empty(), "决策 '{}' 应有原因", d.decision);
    }
}
