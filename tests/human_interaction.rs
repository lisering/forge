//! 人工干预接口集成测试 — 方向 A
//!
//! 验证 HumanInteraction trait 在 Orchestrator 中的 4 个决策点:
//! 1. 计划确认 — confirm_planning: approve / reject
//! 2. 任务确认 — confirm_task: Execute / Skip / Abort
//! 3. 修复确认 — confirm_fix: continue / skip
//! 4. 需求变更确认 — confirm_requirement_change: process / skip
//!
//! 使用 MockInteraction 预编程响应, 无需 Chrome 环境。
//! 通过 memory 状态验证行为 (决策日志、任务状态)。

use async_trait::async_trait;
use forge::extract::ExtractedFile;
use forge::interaction::MockInteraction;
use forge::memory::TaskStatus;
use forge::orchestrator::Orchestrator;
use forge::testrunner::{CompileError, TestResult};
use forge::traits::{ChatClient, ChatResult, FileExtractor, TaskAction, TestRunner};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;

// ============================================================================
//  Mock 实现 (复用 orchestrator_dip.rs 的模式)
// ============================================================================

struct MockChat {
    responses: Arc<Mutex<Vec<String>>>,
    sent_messages: Arc<Mutex<Vec<String>>>,
}

impl MockChat {
    fn new(responses: Vec<String>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses)),
            sent_messages: Arc::new(Mutex::new(vec![])),
        }
    }
}

#[async_trait]
impl ChatClient for MockChat {
    async fn send_message(&self, msg: &str, _timeout: u64) -> anyhow::Result<ChatResult> {
        self.sent_messages.lock().unwrap().push(msg.to_string());
        let mut queue = self.responses.lock().unwrap();
        if queue.is_empty() {
            return Ok(ChatResult {
                text: "(empty)".to_string(),
                timed_out: false,
            });
        }
        Ok(ChatResult {
            text: queue.remove(0),
            timed_out: false,
        })
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

fn make_success_result() -> TestResult {
    TestResult {
        success: true,
        stdout: String::new(),
        stderr: String::new(),
        exit_code: 0,
        errors: vec![],
        test_summary: None,
    }
}

fn make_fail_result(file: &str, msg: &str) -> TestResult {
    TestResult {
        success: false,
        stdout: String::new(),
        stderr: "error".to_string(),
        exit_code: 1,
        errors: vec![CompileError {
            file: file.to_string(),
            line: Some(1),
            column: Some(1),
            message: msg.to_string(),
            error_code: Some("E0308".to_string()),
        }],
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

/// 简单的计划 JSON (单阶段单任务)
const PLAN_JSON: &str = r#"```json
[{"name":"初始化","description":"创建项目","tasks":[{"name":"创建main.rs","prompt":"创建main.rs"}]}]
```"#;

// ============================================================================
//  测试用例
// ============================================================================

/// 测试 1: 计划被批准 — 开发正常进行, 任务完成
#[tokio::test]
async fn test_plan_approved_development_proceeds() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        PLAN_JSON.to_string(),
        "```file:src/main.rs\nfn main() {}\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_success_result()])
        .with_test_results(vec![make_success_result()]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mock_interaction = MockInteraction::new().with_plan_response(true);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "创建一个 CLI 工具", 3, 60)
        .with_interaction(Box::new(mock_interaction));

    orch.run().await.unwrap();

    // 计划被批准 → 决策日志应有 "计划已确认"
    let has_plan_confirm = orch
        .memory
        .decisions
        .iter()
        .any(|d| d.decision == "计划已确认");
    assert!(has_plan_confirm, "决策日志应记录计划已确认");

    // 任务应完成
    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(task.status, TaskStatus::Completed);
}

/// 测试 2: 计划被拒绝 — 开发终止, 无任务执行
#[tokio::test]
async fn test_plan_rejected_development_aborts() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![PLAN_JSON.to_string()]);

    let runner = MockTestRunner::new();
    let extractor = MockExtractor::new(vec![]);

    let mock_interaction = MockInteraction::new().with_plan_response(false);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "创建一个 CLI 工具", 3, 60)
        .with_interaction(Box::new(mock_interaction));

    orch.run().await.unwrap();

    // 计划被拒绝 → 决策日志应有 "计划被人类拒绝"
    let has_reject = orch
        .memory
        .decisions
        .iter()
        .any(|d| d.decision == "计划被人类拒绝");
    assert!(has_reject, "决策日志应记录计划被拒绝");

    // 任务应保持 Pending (未执行)
    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(task.status, TaskStatus::Pending);
}

/// 测试 3: 任务被跳过 — 继续执行后续任务
#[tokio::test]
async fn test_task_skipped_continues_to_next() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let plan_json = r#"```json
[{"name":"初始化","description":"创建项目","tasks":[
  {"name":"任务1","prompt":"创建main.rs"},
  {"name":"任务2","prompt":"创建lib.rs"}
]}]
```"#;

    let chat = MockChat::new(vec![
        plan_json.to_string(),
        "```file:src/main.rs\nfn main() {}\n```".to_string(),
        "```file:src/lib.rs\npub fn hello() {}\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_success_result(), make_success_result()])
        .with_test_results(vec![make_success_result(), make_success_result()]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/lib.rs", "pub fn hello() {}")],
    ]);

    // 第一个任务跳过, 第二个执行
    let mock_interaction = MockInteraction::new()
        .with_plan_response(true)
        .with_task_responses(vec![TaskAction::Skip, TaskAction::Execute]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "创建一个 CLI 工具", 3, 60)
        .with_interaction(Box::new(mock_interaction));

    orch.run().await.unwrap();

    // 任务1被跳过 (status = Pending), 任务2执行 (status = Completed)
    let task1 = &orch.memory.phases[0].tasks[0];
    let task2 = &orch.memory.phases[0].tasks[1];
    assert_eq!(task1.status, TaskStatus::Pending, "任务1应被跳过");
    assert_eq!(task2.status, TaskStatus::Completed, "任务2应执行完成");

    // 决策日志应有 "任务被人类跳过"
    let has_skip = orch
        .memory
        .decisions
        .iter()
        .any(|d| d.decision == "任务被人类跳过");
    assert!(has_skip, "决策日志应记录任务被跳过");
}

/// 测试 4: 任务中止 — 整个开发终止
#[tokio::test]
async fn test_task_abort_terminates_development() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![PLAN_JSON.to_string()]);

    let runner = MockTestRunner::new();
    let extractor = MockExtractor::new(vec![]);

    let mock_interaction = MockInteraction::new()
        .with_plan_response(true)
        .with_task_response(TaskAction::Abort);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "创建一个 CLI 工具", 3, 60)
        .with_interaction(Box::new(mock_interaction));

    let result = orch.run().await;

    // 应该返回错误 (开发被中止)
    assert!(result.is_err(), "任务中止应返回错误");

    // 决策日志应有 "开发被人类中止"
    let has_abort = orch
        .memory
        .decisions
        .iter()
        .any(|d| d.decision == "开发被人类中止");
    assert!(has_abort, "决策日志应记录开发被中止");
}

/// 测试 5: 修复确认被拒绝 — 任务标记为失败
#[tokio::test]
async fn test_fix_rejected_task_fails() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        PLAN_JSON.to_string(),
        "```file:src/main.rs\nfn main() {}\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_fail_result("src/main.rs", "syntax error")])
        .with_test_results(vec![]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mock_interaction = MockInteraction::new()
        .with_plan_response(true)
        .with_fix_response(false); // 拒绝修复

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "创建一个 CLI 工具", 3, 60)
        .with_interaction(Box::new(mock_interaction));

    orch.run().await.unwrap();

    // 任务应该标记为失败
    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(task.status, TaskStatus::Failed);

    // 决策日志应有 "修复被人类跳过"
    let has_skip = orch
        .memory
        .decisions
        .iter()
        .any(|d| d.decision == "修复被人类跳过");
    assert!(has_skip, "决策日志应记录修复被跳过");
}

/// 测试 6: 修复确认通过 — 继续修复, 最终成功
#[tokio::test]
async fn test_fix_approved_continues_repair() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        PLAN_JSON.to_string(),
        "```file:src/main.rs\nfn main() {}\n```".to_string(),
        "```file:src/main.rs\nfn main() { fixed }\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            make_fail_result("src/main.rs", "syntax error"),
            make_success_result(),
        ])
        .with_test_results(vec![make_success_result()]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/main.rs", "fn main() { fixed }")],
    ]);

    let mock_interaction = MockInteraction::new()
        .with_plan_response(true)
        .with_fix_response(true);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "创建一个 CLI 工具", 3, 60)
        .with_interaction(Box::new(mock_interaction));

    orch.run().await.unwrap();

    // 任务应该完成
    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(task.status, TaskStatus::Completed);
    assert_eq!(task.attempts, 2, "应有 2 次尝试");
}

/// 测试 7: 需求变更被跳过 — 变更不处理, 标记为已处理
#[tokio::test]
async fn test_requirement_change_skipped() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        PLAN_JSON.to_string(),
        "```file:src/main.rs\nfn main() {}\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_success_result()])
        .with_test_results(vec![make_success_result()]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mock_interaction = MockInteraction::new()
        .with_plan_response(true)
        .with_change_response(false); // 拒绝需求变更

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "创建一个 CLI 工具", 3, 60)
        .with_interaction(Box::new(mock_interaction));

    // 添加需求变更
    orch.memory.add_requirement_change("添加用户认证", "test");

    orch.run().await.unwrap();

    // 变更被标记为已处理 (即使被拒绝)
    assert!(!orch.memory.has_pending_changes(), "变更应被标记为已处理");

    // 不应有新追加的阶段
    assert_eq!(orch.memory.phases.len(), 1, "不应有新阶段");
}

/// 测试 8: AutoApprove (默认) — 所有决策点自动通过, 行为与无交互一致
#[tokio::test]
async fn test_auto_approve_default_behavior() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        PLAN_JSON.to_string(),
        "```file:src/main.rs\nfn main() {}\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_success_result()])
        .with_test_results(vec![make_success_result()]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    // 不设置任何 MockInteraction — 使用默认的 AutoApprove
    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "创建一个 CLI 工具", 3, 60);

    orch.run().await.unwrap();

    // 任务应正常完成
    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(task.status, TaskStatus::Completed);

    // 决策日志应有 "计划已确认"
    let has_plan_confirm = orch
        .memory
        .decisions
        .iter()
        .any(|d| d.decision == "计划已确认");
    assert!(has_plan_confirm, "AutoApprove 应自动确认计划");
}

/// 测试 9: 需求变更被确认 — 变更被处理, 新阶段追加
#[tokio::test]
async fn test_requirement_change_approved() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let plan_json = r#"```json
[{"name":"初始化","description":"创建项目","tasks":[{"name":"任务1","prompt":"创建main.rs"}]}]
```"#;

    let replan_json = r#"```json
[{"name":"新功能","description":"需求变更新增","tasks":[{"name":"新任务","prompt":"创建新功能"}]}]
```"#;

    let chat = MockChat::new(vec![
        plan_json.to_string(),
        replan_json.to_string(),
        "```file:src/main.rs\nfn main() {}\n```".to_string(),
        "```file:src/feature.rs\npub fn feature() {}\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_success_result(), make_success_result()])
        .with_test_results(vec![make_success_result(), make_success_result()]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/feature.rs", "pub fn feature() {}")],
    ]);

    let mock_interaction = MockInteraction::new()
        .with_plan_response(true)
        .with_change_response(true); // 批准需求变更

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "创建一个 CLI 工具", 3, 60)
        .with_interaction(Box::new(mock_interaction));

    // 添加需求变更
    orch.memory.add_requirement_change("添加新功能", "test");

    orch.run().await.unwrap();

    // 应该有 2 个阶段 (原始 + 变更新增)
    assert_eq!(orch.memory.phases.len(), 2, "应有 2 个阶段");
    assert_eq!(orch.memory.phases[1].name, "新功能");
}
