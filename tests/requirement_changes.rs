//! 需求变更处理集成测试 — 验证 Orchestrator 在阶段间检查需求变更并重新规划
//!
//! 测试场景:
//! 1. 有需求变更 → 阶段间检测 → AI 重新规划 → 追加新阶段 → 执行新阶段
//! 2. 无需求变更 → 正常流程
//! 3. 需求变更文件加载 → 变更被添加到 Memory
//! 4. 需求变更持久化 → save/load 往返
//! 5. 变更处理后标记为已处理 (不重复触发)

use async_trait::async_trait;
use forge::extract::ExtractedFile;
use forge::memory::TaskStatus;
use forge::orchestrator::Orchestrator;
use forge::testrunner::{CompileError, TestResult};
use forge::traits::{ChatClient, ChatResult, FileExtractor, TestRunner};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;

// ============================================================================
//  Mock 实现 (复用自 e2e_testing.rs 模式)
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

    fn sent_messages(&self) -> Vec<String> {
        self.sent_messages.lock().unwrap().clone()
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
        let text = queue.remove(0);
        Ok(ChatResult {
            text,
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
            "failed".to_string()
        },
        exit_code: if success { 0 } else { 1 },
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

fn plan_json_1phase() -> &'static str {
    r#"```json
[{"name":"阶段1","description":"d","tasks":[{"name":"任务1","prompt":"do1"}]}]
```"#
}

fn plan_json_new_phase() -> &'static str {
    r#"```json
[{"name":"新阶段","description":"需求变更新增","tasks":[{"name":"新任务","prompt":"do new"}]}]
```"#
}

// ============================================================================
//  测试用例
// ============================================================================

/// 测试 1: 有需求变更 → 阶段间检测 → AI 重新规划 → 追加新阶段 → 执行新阶段
///
/// 流程:
/// 1. Planning: AI 返回 1 阶段 1 任务
/// 2. 阶段1 执行成功
/// 3. 阶段间检查: 有需求变更 → 发送重新规划请求
/// 4. AI 返回新阶段 JSON → 追加到计划
/// 5. 执行新阶段 → 成功
#[tokio::test]
async fn test_requirement_change_triggers_replan() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        // 1. planning
        plan_json_1phase().to_string(),
        // 2. 重新规划回复 (追加新阶段) — 在阶段0执行前触发
        plan_json_new_phase().to_string(),
        // 3. 阶段1 task 回复
        "```file:src/main.rs\nfn main() {}\n```".to_string(),
        // 4. 新阶段 task 回复
        "```file:src/lib.rs\npub fn lib() {}\n```".to_string(),
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

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10);

    // 添加需求变更 (在运行前)
    orch.memory.add_requirement_change("添加日志功能", "cli");

    orch.run().await.unwrap();

    // 验证: 原始阶段 + 新阶段 = 2 个阶段
    assert_eq!(orch.memory.phases.len(), 2, "应有 2 个阶段 (原始 + 新增)");
    assert_eq!(orch.memory.phases[0].name, "阶段1");
    assert_eq!(orch.memory.phases[1].name, "新阶段");
    assert_eq!(
        orch.memory.phases[0].status,
        forge::memory::PhaseStatus::Completed
    );
    assert_eq!(
        orch.memory.phases[1].status,
        forge::memory::PhaseStatus::Completed
    );

    // 验证: 新阶段的任务也完成了
    assert_eq!(orch.memory.phases[1].tasks[0].status, TaskStatus::Completed);
    assert_eq!(orch.memory.phases[1].tasks[0].name, "新任务");

    // 验证: 决策记录中有需求变更重新规划
    let has_replan = orch
        .memory
        .decisions
        .iter()
        .any(|d| d.decision == "需求变更重新规划");
    assert!(has_replan, "应有需求变更重新规划决策记录");

    // 验证: 需求变更已标记为已处理
    assert!(!orch.memory.has_pending_changes(), "需求变更应已处理");

    // 验证: 发送了 4 条消息 (planning + task1 + replan + task2)
    let sent = orch.chat.sent_messages();
    assert_eq!(sent.len(), 4, "应发送 4 条消息");

    // 第 3 条应是重新规划 prompt (包含需求变更内容)
    assert!(
        sent[2].contains("需求变更") || sent[2].contains("变更"),
        "第 3 条消息应包含需求变更: {}",
        sent[2]
    );
    assert!(
        sent[2].contains("添加日志功能"),
        "重新规划 prompt 应包含变更内容"
    );
}

/// 测试 2: 无需求变更 → 正常流程 (不触发重新规划)
#[tokio::test]
async fn test_no_changes_no_replan() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        plan_json_1phase().to_string(),
        "```file:src/main.rs\nfn main() {}\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10);

    // 不添加需求变更

    orch.run().await.unwrap();

    // 验证: 只有 1 个阶段
    assert_eq!(orch.memory.phases.len(), 1);

    // 验证: 没有需求变更重新规划决策
    let has_replan = orch
        .memory
        .decisions
        .iter()
        .any(|d| d.decision == "需求变更重新规划");
    assert!(!has_replan, "无需求变更时不应触发重新规划");

    // 验证: 只发送了 2 条消息 (planning + task)
    let sent = orch.chat.sent_messages();
    assert_eq!(sent.len(), 2);
}

/// 测试 3: 需求变更从文件加载
#[tokio::test]
async fn test_load_changes_from_file() {
    let dir = tempdir().unwrap();
    let changes_path = dir.path().join("changes.txt");
    std::fs::write(
        &changes_path,
        "# 注释\n添加用户认证\n\n支持多语言\n# 注释2\n优化性能\n",
    )
    .unwrap();

    let chat = MockChat::new(vec![]);
    let mut orch = Orchestrator::new(
        &chat,
        MockTestRunner::new(),
        MockExtractor::new(vec![]),
        dir.path().to_str().unwrap(),
        "test",
        3,
        10,
    );

    orch.memory.load_changes_from_file(&changes_path);

    // 验证: 3 个变更被加载 (注释和空行被跳过)
    assert!(orch.memory.has_pending_changes());
    assert_eq!(orch.memory.pending_requirement_changes.len(), 3);
    assert_eq!(
        orch.memory.pending_requirement_changes[0].description,
        "添加用户认证"
    );
    assert_eq!(
        orch.memory.pending_requirement_changes[1].description,
        "支持多语言"
    );
    assert_eq!(
        orch.memory.pending_requirement_changes[2].description,
        "优化性能"
    );
}

/// 测试 4: 需求变更持久化 → save/load 往返
#[tokio::test]
async fn test_changes_persisted_save_load() {
    let dir = tempdir().unwrap();
    let ws = forge::workspace::Workspace::new(dir.path());
    ws.init().unwrap();

    let mut mem = forge::memory::Memory::new("test goal");
    mem.add_requirement_change("持久化变更1", "file");
    mem.add_requirement_change("持久化变更2", "cli");

    let mem_path = dir.path().join(".forge").join("memory.json");
    mem.save(&mem_path).unwrap();

    let loaded = forge::memory::Memory::load(&mem_path).unwrap();

    assert!(loaded.has_pending_changes());
    assert_eq!(loaded.pending_requirement_changes.len(), 2);
    assert_eq!(
        loaded.pending_requirement_changes[0].description,
        "持久化变更1"
    );
    assert_eq!(loaded.pending_requirement_changes[0].source, "file");
    assert_eq!(
        loaded.pending_requirement_changes[1].description,
        "持久化变更2"
    );
    assert_eq!(loaded.pending_requirement_changes[1].source, "cli");
}

/// 测试 5: 变更处理后不重复触发
#[tokio::test]
async fn test_changes_not_triggered_twice() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    // 2 个阶段, 在第一个阶段前有变更
    // 变更应只触发一次 (第一次处理后, 后续不再触发)
    let chat = MockChat::new(vec![
        // planning: 2 个阶段
        r#"```json
[
  {"name":"阶段1","description":"d1","tasks":[{"name":"t1","prompt":"do1"}]},
  {"name":"阶段2","description":"d2","tasks":[{"name":"t2","prompt":"do2"}]}
]
```"#
            .to_string(),
        // 重新规划回复 (追加新阶段) — 在阶段0执行前触发
        plan_json_new_phase().to_string(),
        // 阶段1 task
        "```file:src/main.rs\nfn main() {}\n```".to_string(),
        // 新阶段 task
        "```file:src/lib.rs\npub fn lib() {}\n```".to_string(),
        // 原阶段2 task
        "```file:src/mod.rs\npub mod m {}\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            make_test_result(true, vec![]),
            make_test_result(true, vec![]),
            make_test_result(true, vec![]),
        ])
        .with_test_results(vec![
            make_test_result(true, vec![]),
            make_test_result(true, vec![]),
            make_test_result(true, vec![]),
        ]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/lib.rs", "pub fn lib() {}")],
        vec![ef("src/mod.rs", "pub mod m {}")],
    ]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10);

    // 添加一个需求变更
    orch.memory.add_requirement_change("新功能需求", "cli");

    orch.run().await.unwrap();

    // 验证: 原始 2 阶段 + 新增 1 阶段 = 3 阶段
    assert_eq!(orch.memory.phases.len(), 3, "应有 3 个阶段");

    // 验证: 需求变更只触发了一次重新规划
    let replan_count = orch
        .memory
        .decisions
        .iter()
        .filter(|d| d.decision == "需求变更重新规划")
        .count();
    assert_eq!(replan_count, 1, "需求变更应只触发一次重新规划");

    // 验证: 变更已标记为已处理
    assert!(!orch.memory.has_pending_changes());
}
