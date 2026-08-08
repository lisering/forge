//! Orchestrator DIP 集成测试 — 验证核心编排逻辑可在无 Chrome 环境下测试
//!
//! 通过 Mock 实现 ChatClient / TestRunner / FileExtractor trait,
//! 注入 Orchestrator 测试 execute_task 的修复循环、版本回滚、断点续传等核心逻辑。
//!
//! 这是 DIP 重构的核心价值: 不需要真实浏览器/Chrome 即可测试编排引擎。

use async_trait::async_trait;
use forge::extract::ExtractedFile;
use forge::memory::{Memory, Phase, PhaseStatus, Task, TaskStatus};
use forge::orchestrator::Orchestrator;
use forge::testrunner::{CompileError, TestResult};
use forge::traits::{ChatClient, ChatResult, FileExtractor, TestRunner};
use forge::workspace::Workspace;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;

// ============================================================================
//  Mock 实现
// ============================================================================

/// Mock ChatClient — 按顺序返回预编程回复
struct MockChat {
    /// 预编程回复队列 (按调用顺序弹出)
    responses: Arc<Mutex<Vec<String>>>,
    /// 记录所有收到的消息
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

/// Mock TestRunner — 按顺序返回预编程结果
struct MockTestRunner {
    /// check 结果队列
    check_results: Arc<Mutex<Vec<TestResult>>>,
    /// test 结果队列
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
    /// 文件列表队列 (每次 extract 弹出一个)
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

// ============================================================================
//  测试用例
// ============================================================================

/// 测试 1: 一次成功 — AI 返回正确代码,编译和测试都通过
#[tokio::test]
async fn test_execute_task_success_on_first_attempt() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        // planning phase 的回复
        r#"```json
[{"name":"初始化","description":"创建项目","tasks":[{"name":"创建main.rs","prompt":"创建main.rs"}]}]
```"#
            .to_string(),
        // execute_task 的回复 — 返回代码
        "```file:src/main.rs\nfn main() { println!(\"hello\"); }\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(
        &chat,
        runner,
        extractor,
        ws_dir,
        "test goal",
        3,  // max_rounds
        10, // timeout
    );

    orch.run().await.unwrap();

    // 验证任务完成
    assert_eq!(orch.memory.phases[0].tasks[0].status, TaskStatus::Completed);
    assert_eq!(orch.memory.phases[0].tasks[0].attempts, 1);
    assert!(orch.memory.phases[0].tasks[0]
        .files_written
        .contains(&"src/main.rs".to_string()));
}

/// 测试 2: 修复循环 — 第一次编译失败,第二次修复成功
#[tokio::test]
async fn test_execute_task_fix_loop_success() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        // planning phase 的回复
        r#"```json
[{"name":"阶段1","description":"d","tasks":[{"name":"任务1","prompt":"do it"}]}]
```"#
            .to_string(),
        // 第一次 execute_task — 返回有错误的代码
        "```file:src/main.rs\nfn main() { broken }\n```".to_string(),
        // 第二次 execute_task (修复轮) — 返回修复后的代码
        "```file:src/main.rs\nfn main() { println!(\"fixed\"); }\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            // 第一次 check 失败
            make_test_result(
                false,
                vec![make_compile_error("src/main.rs", 1, "syntax error")],
            ),
            // 第二次 check 成功
            make_test_result(true, vec![]),
        ])
        .with_test_results(vec![
            // 第二次的 test 成功 (第一次没到 test)
            make_test_result(true, vec![]),
        ]);

    let extractor = MockExtractor::new(vec![
        // 第一次 extract
        vec![ef("src/main.rs", "fn main() { broken }")],
        // 第二次 extract (修复后)
        vec![ef("src/main.rs", "fn main() { println!(\"fixed\"); }")],
    ]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10);

    orch.run().await.unwrap();

    // 验证: 任务最终成功
    assert_eq!(orch.memory.phases[0].tasks[0].status, TaskStatus::Completed);
    assert_eq!(
        orch.memory.phases[0].tasks[0].attempts, 2,
        "应在第 2 次修复成功"
    );
}

/// 测试 3: 修复循环全部失败 — 达到 max_rounds 后标记失败
#[tokio::test]
async fn test_execute_task_all_attempts_fail() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        // planning phase
        r#"```json
[{"name":"p","description":"d","tasks":[{"name":"t","prompt":"do"}]}]
```"#
            .to_string(),
        // 3 次 execute_task 都返回代码
        "```file:src/main.rs\nfn main() { broken1 }\n```".to_string(),
        "```file:src/main.rs\nfn main() { broken2 }\n```".to_string(),
        "```file:src/main.rs\nfn main() { broken3 }\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            make_test_result(false, vec![make_compile_error("src/main.rs", 1, "error1")]),
            make_test_result(false, vec![make_compile_error("src/main.rs", 1, "error2")]),
            make_test_result(false, vec![make_compile_error("src/main.rs", 1, "error3")]),
        ])
        .with_test_results(vec![]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() { broken1 }")],
        vec![ef("src/main.rs", "fn main() { broken2 }")],
        vec![ef("src/main.rs", "fn main() { broken3 }")],
    ]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10);

    orch.run().await.unwrap();

    // 验证: 任务失败
    assert_eq!(orch.memory.phases[0].tasks[0].status, TaskStatus::Failed);
    assert_eq!(orch.memory.phases[0].tasks[0].attempts, 3, "应尝试 3 次");
}

/// 测试 4: AI 未返回代码文件 — 直接跳过,继续下一轮
#[tokio::test]
async fn test_execute_task_no_code_extracted() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        // planning phase
        r#"```json
[{"name":"p","description":"d","tasks":[{"name":"t","prompt":"do"}]}]
```"#
            .to_string(),
        // 第一次回复没有代码
        "I will help you with that.".to_string(),
        // 第二次回复有代码
        "```file:src/main.rs\nfn main() {}\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![
        vec![],                                  // 第一次 extract 返回空
        vec![ef("src/main.rs", "fn main() {}")], // 第二次有文件
    ]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10);

    orch.run().await.unwrap();

    // 验证: 最终成功 (第二次有代码)
    assert_eq!(orch.memory.phases[0].tasks[0].status, TaskStatus::Completed);
    assert_eq!(orch.memory.phases[0].tasks[0].attempts, 2);
}

/// 测试 5: 编译通过但测试失败 — 进入修复循环
#[tokio::test]
async fn test_execute_task_compile_ok_test_fail_then_fix() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        // planning phase
        r#"```json
[{"name":"p","description":"d","tasks":[{"name":"t","prompt":"do"}]}]
```"#
            .to_string(),
        // 第一次回复
        "```file:src/main.rs\nfn main() {}\n```".to_string(),
        // 修复回复
        "```file:src/main.rs\nfn main() { println!(\"fixed\"); }\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            make_test_result(true, vec![]), // 第一次 check 通过
            make_test_result(true, vec![]), // 第二次 check 通过
        ])
        .with_test_results(vec![
            make_test_result(false, vec![]), // 第一次 test 失败
            make_test_result(true, vec![]),  // 第二次 test 通过
        ]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/main.rs", "fn main() { println!(\"fixed\"); }")],
    ]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10);

    orch.run().await.unwrap();

    // 验证: 最终成功
    assert_eq!(orch.memory.phases[0].tasks[0].status, TaskStatus::Completed);
    assert_eq!(orch.memory.phases[0].tasks[0].attempts, 2);
}

/// 测试 6: 版本回滚 — 最终失败时回滚到 known good
#[tokio::test]
async fn test_version_rollback_on_final_failure() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    // 先写入一个正确的文件,建立 known good
    let ws = Workspace::new(ws_dir);
    ws.init().unwrap();
    ws.write_file("src/main.rs", "fn main() { println!(\"original\"); }")
        .unwrap();
    ws.write_file("Cargo.toml", "[package]\nname = \"test\"")
        .unwrap();
    let good_id = ws.snapshot_all("known_good").unwrap();
    ws.save_known_good(good_id).unwrap();
    drop(ws);

    let chat = MockChat::new(vec![
        // planning phase
        r#"```json
[{"name":"p","description":"d","tasks":[{"name":"t","prompt":"do"}]}]
```"#
            .to_string(),
        // 2 次都返回破坏性代码
        "```file:src/main.rs\nfn main() { broken1 }\n```".to_string(),
        "```file:src/main.rs\nfn main() { broken2 }\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            make_test_result(false, vec![make_compile_error("src/main.rs", 1, "err1")]),
            make_test_result(false, vec![make_compile_error("src/main.rs", 1, "err2")]),
        ])
        .with_test_results(vec![]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() { broken1 }")],
        vec![ef("src/main.rs", "fn main() { broken2 }")],
    ]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 2, 10);

    orch.run().await.unwrap();

    // 验证: 任务失败
    assert_eq!(orch.memory.phases[0].tasks[0].status, TaskStatus::Failed);

    // 验证: 文件回滚到 known good (原始内容)
    let content = std::fs::read_to_string(format!("{}/src/main.rs", ws_dir)).unwrap();
    assert_eq!(
        content, "fn main() { println!(\"original\"); }",
        "应回滚到 known good"
    );
}

/// 测试 7: 增量修复 — 验证修复轮发送的是增量 prompt
#[tokio::test]
async fn test_incremental_fix_prompt_sent() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        // planning phase
        r#"```json
[{"name":"p","description":"d","tasks":[{"name":"t","prompt":"do"}]}]
```"#
            .to_string(),
        // 第一次回复
        "```file:src/main.rs\nfn main() { broken }\n```".to_string(),
        // 第二次回复 (修复轮)
        "```file:src/main.rs\nfn main() { println!(\"fixed\"); }\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            make_test_result(
                false,
                vec![make_compile_error("src/main.rs", 1, "syntax error")],
            ),
            make_test_result(true, vec![]),
        ])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() { broken }")],
        vec![ef("src/main.rs", "fn main() { println!(\"fixed\"); }")],
    ]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10);

    orch.run().await.unwrap();

    let sent = orch.chat.sent_messages();

    // 第 1 条: planning prompt
    // 第 2 条: 第一次 execute_task 的完整 prompt (含上下文)
    // 第 3 条: 修复轮 prompt (应包含"编译/测试错误"和"出错的文件")
    assert!(sent.len() >= 3, "应至少发送 3 条消息");
    let fix_prompt = &sent[2];
    assert!(
        fix_prompt.contains("编译/测试错误") || fix_prompt.contains("测试错误"),
        "修复轮 prompt 应包含错误信息"
    );
}

/// 测试 8: 断点续传 — 已完成的任务被跳过
#[tokio::test]
async fn test_resume_skips_completed_tasks() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    // 创建一个有已完成任务的 memory
    let mut memory = Memory::new("test goal");
    memory.set_phases(vec![Phase {
        id: 0,
        name: "阶段1".to_string(),
        description: "测试".to_string(),
        status: PhaseStatus::InProgress,
        tasks: vec![
            Task {
                id: "0-0".to_string(),
                phase_id: 0,
                name: "已完成任务".to_string(),
                prompt: "do something".to_string(),
                status: TaskStatus::Completed,
                result: Some("成功".to_string()),
                attempts: 1,
                files_written: vec!["src/main.rs".to_string()],
                test_result: None,
                last_good_snapshot: None,
                clarifications: vec![],
                depends_on: vec![],
            },
            Task {
                id: "0-1".to_string(),
                phase_id: 0,
                name: "待执行任务".to_string(),
                prompt: "do another".to_string(),
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

    // 保存 memory
    let ws = Workspace::new(ws_dir);
    ws.init().unwrap();
    ws.write_file("src/main.rs", "fn main() {}").unwrap();
    memory
        .save(&ws.root.join(".forge").join("memory.json"))
        .unwrap();
    drop(ws);

    // Mock chat: 只需要回复待执行任务
    let chat = MockChat::new(vec![
        // 待执行任务的回复
        "```file:src/lib.rs\npub fn lib() {}\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/lib.rs", "pub fn lib() {}")]]);

    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test goal", 3, 10).with_resume(true);

    orch.run().await.unwrap();

    // 验证: 第一个任务仍为 Completed
    assert_eq!(orch.memory.phases[0].tasks[0].status, TaskStatus::Completed);
    // 验证: 第二个任务变为 Completed
    assert_eq!(orch.memory.phases[0].tasks[1].status, TaskStatus::Completed);

    // chat 只被调用 1 次 (跳过了已完成任务的 planning 和 execute)
    let sent = orch.chat.sent_messages();
    assert_eq!(sent.len(), 1, "应只发送 1 条消息 (待执行任务)");
}

/// 测试 9: 多阶段 — 验证阶段间正确流转
#[tokio::test]
async fn test_multi_phase_execution() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        // planning phase — 2 个阶段各 1 个任务
        r#"```json
[
  {"name":"阶段1","description":"d1","tasks":[{"name":"任务1","prompt":"do1"}]},
  {"name":"阶段2","description":"d2","tasks":[{"name":"任务2","prompt":"do2"}]}
]
```"#
            .to_string(),
        // 阶段1任务1的回复
        "```file:src/main.rs\nfn main() {}\n```".to_string(),
        // 阶段2任务1的回复
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

    orch.run().await.unwrap();

    // 验证: 两个阶段都完成
    assert_eq!(orch.memory.phases.len(), 2);
    assert_eq!(orch.memory.phases[0].status, PhaseStatus::Completed);
    assert_eq!(orch.memory.phases[1].status, PhaseStatus::Completed);
    assert_eq!(orch.memory.phases[0].tasks[0].status, TaskStatus::Completed);
    assert_eq!(orch.memory.phases[1].tasks[0].status, TaskStatus::Completed);
}

/// 测试 10: planning phase 返回无效 JSON — 使用默认计划
#[tokio::test]
async fn test_planning_invalid_json_fallback() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        // 无效 JSON
        "Sorry, I cannot help with that.".to_string(),
        // 默认任务的回复
        "```file:src/main.rs\nfn main() {}\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10);

    orch.run().await.unwrap();

    // 验证: 使用了默认计划 (1 个阶段 1 个任务)
    assert_eq!(orch.memory.phases.len(), 1, "应使用默认单阶段计划");
    assert_eq!(orch.memory.phases[0].tasks[0].status, TaskStatus::Completed);
}

/// 测试 11: memory.json 在执行过程中被保存
#[tokio::test]
async fn test_memory_saved_during_execution() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        r#"```json
[{"name":"p","description":"d","tasks":[{"name":"t","prompt":"do"}]}]
```"#
            .to_string(),
        "```file:src/main.rs\nfn main() {}\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10);

    orch.run().await.unwrap();

    // 验证: memory.json 存在
    let memory_path = format!("{}/.forge/memory.json", ws_dir);
    assert!(
        std::path::Path::new(&memory_path).exists(),
        "memory.json 应存在"
    );

    // 验证: 可以加载
    let loaded = Memory::load(std::path::Path::new(&memory_path)).unwrap();
    assert_eq!(loaded.goal, "test");
    assert!(!loaded.phases.is_empty());
}

/// 测试 12: known good 快照在编译通过时被保存
#[tokio::test]
async fn test_known_good_saved_on_compile_success() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        r#"```json
[{"name":"p","description":"d","tasks":[{"name":"t","prompt":"do"}]}]
```"#
            .to_string(),
        "```file:src/main.rs\nfn main() {}\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10);

    orch.run().await.unwrap();

    // 验证: known good 快照被记录
    assert!(
        orch.memory.phases[0].tasks[0].last_good_snapshot.is_some(),
        "编译通过后应保存 known good 快照"
    );

    // 验证: workspace 中有 known good 标记
    let ws = Workspace::new(ws_dir);
    assert!(ws.get_known_good_id().is_some());
}
