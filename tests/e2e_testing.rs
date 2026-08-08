//! E2E 测试集成测试 — 验证 Orchestrator 在 cargo test 通过后运行 E2E 测试
//!
//! 测试场景:
//! 1. 有 E2E 测试用例 → 运行 E2E → 全部通过 → 任务完成
//! 2. 有 E2E 测试用例 → 运行 E2E → 部分失败 → 进入修复 → 修复后通过
//! 3. 无 E2E 测试用例 → 正常完成 (不运行 E2E)
//! 4. E2E 测试用例从 .forge/e2e_tests.json 加载

use async_trait::async_trait;
use forge::extract::ExtractedFile;
use forge::memory::TaskStatus;
use forge::orchestrator::Orchestrator;
use forge::testrunner::{CompileError, E2ETestCase, E2ETestResult, TestResult};
use forge::traits::{ChatClient, ChatResult, FileExtractor, TestRunner};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;

// ============================================================================
//  Mock 实现
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

    #[allow(dead_code)]
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

/// MockTestRunner — 支持 check/test/run_binary
struct MockTestRunner {
    check_results: Arc<Mutex<Vec<TestResult>>>,
    test_results: Arc<Mutex<Vec<TestResult>>>,
    e2e_results: Arc<Mutex<Vec<Vec<E2ETestResult>>>>,
    run_binary_calls: Arc<Mutex<u32>>,
}

impl MockTestRunner {
    fn new() -> Self {
        Self {
            check_results: Arc::new(Mutex::new(vec![])),
            test_results: Arc::new(Mutex::new(vec![])),
            e2e_results: Arc::new(Mutex::new(vec![])),
            run_binary_calls: Arc::new(Mutex::new(0)),
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

    fn with_e2e_results(mut self, results: Vec<Vec<E2ETestResult>>) -> Self {
        self.e2e_results = Arc::new(Mutex::new(results));
        self
    }

    fn run_binary_call_count(&self) -> u32 {
        *self.run_binary_calls.lock().unwrap()
    }
}

impl Clone for MockTestRunner {
    fn clone(&self) -> Self {
        Self {
            check_results: Arc::clone(&self.check_results),
            test_results: Arc::clone(&self.test_results),
            e2e_results: Arc::clone(&self.e2e_results),
            run_binary_calls: Arc::clone(&self.run_binary_calls),
        }
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

    fn run_binary(
        &self,
        _dir: &Path,
        test_cases: &[E2ETestCase],
    ) -> anyhow::Result<Vec<E2ETestResult>> {
        *self.run_binary_calls.lock().unwrap() += 1;
        let mut queue = self.e2e_results.lock().unwrap();
        if queue.is_empty() {
            // 默认: 所有测试通过
            return Ok(test_cases
                .iter()
                .map(|tc| E2ETestResult {
                    test_case: tc.clone(),
                    stdout: tc.expected_stdout.clone().unwrap_or_default(),
                    stderr: String::new(),
                    exit_code: 0,
                    passed: true,
                })
                .collect());
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

fn plan_json() -> &'static str {
    r#"```json
[{"name":"初始化","description":"创建项目","tasks":[{"name":"创建main.rs","prompt":"创建main.rs"}]}]
```"#
}

fn make_e2e_case(name: &str, expected_stdout: &str) -> E2ETestCase {
    E2ETestCase {
        name: name.to_string(),
        stdin: None,
        args: vec![],
        expected_stdout: Some(expected_stdout.to_string()),
        expected_exit_code: Some(0),
    }
}

fn make_e2e_result(case: E2ETestCase, passed: bool) -> E2ETestResult {
    let stdout = if passed {
        case.expected_stdout.clone().unwrap_or_default()
    } else {
        "wrong output".to_string()
    };
    E2ETestResult {
        test_case: case,
        stdout,
        stderr: String::new(),
        exit_code: if passed { 0 } else { 1 },
        passed,
    }
}

/// 在工作区创建 .forge/e2e_tests.json
fn write_e2e_tests(ws_dir: &Path, cases: &[E2ETestCase]) {
    let forge_dir = ws_dir.join(".forge");
    std::fs::create_dir_all(&forge_dir).unwrap();
    let json = serde_json::to_string_pretty(cases).unwrap();
    std::fs::write(forge_dir.join("e2e_tests.json"), json).unwrap();
}

// ============================================================================
//  测试用例
// ============================================================================

/// 测试 1: 有 E2E 测试 → 运行 → 全部通过 → 任务完成
#[tokio::test]
async fn test_e2e_tests_pass_task_completes() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    // 创建 E2E 测试用例文件
    let e2e_cases = vec![make_e2e_case("test_hello", "hello world")];
    write_e2e_tests(dir.path(), &e2e_cases);

    let chat = MockChat::new(vec![
        plan_json().to_string(),
        "```file:src/main.rs\nfn main() { println!(\"hello world\"); }\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])])
        .with_e2e_results(vec![vec![make_e2e_result(
            make_e2e_case("test_hello", "hello world"),
            true,
        )]]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10);

    orch.run().await.unwrap();

    // 验证: 任务完成
    assert_eq!(orch.memory.phases[0].tasks[0].status, TaskStatus::Completed);
    assert_eq!(
        orch.memory.phases[0].tasks[0].result,
        Some("成功 (含 E2E 测试)".to_string())
    );
}

/// 测试 2: 有 E2E 测试 → 部分失败 → 修复后通过
#[tokio::test]
async fn test_e2e_tests_fail_then_fix() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let e2e_cases = vec![make_e2e_case("test_hello", "hello world")];
    write_e2e_tests(dir.path(), &e2e_cases);

    let chat = MockChat::new(vec![
        plan_json().to_string(),
        // 第一次回复
        "```file:src/main.rs\nfn main() { println!(\"hi\"); }\n```".to_string(),
        // 修复轮回复
        "```file:src/main.rs\nfn main() { println!(\"hello world\"); }\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            make_test_result(true, vec![]),
            make_test_result(true, vec![]),
        ])
        .with_test_results(vec![
            make_test_result(true, vec![]),
            make_test_result(true, vec![]),
        ])
        .with_e2e_results(vec![
            // 第一次 E2E: 失败
            vec![make_e2e_result(
                make_e2e_case("test_hello", "hello world"),
                false,
            )],
            // 第二次 E2E: 通过
            vec![make_e2e_result(
                make_e2e_case("test_hello", "hello world"),
                true,
            )],
        ]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() { println!(\"hi\"); }")],
        vec![ef(
            "src/main.rs",
            "fn main() { println!(\"hello world\"); }",
        )],
    ]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10);

    orch.run().await.unwrap();

    // 验证: 任务最终完成
    assert_eq!(orch.memory.phases[0].tasks[0].status, TaskStatus::Completed);
    assert_eq!(
        orch.memory.phases[0].tasks[0].attempts, 2,
        "应在第 2 次修复成功"
    );

    // 验证: 决策记录中有 E2E 测试失败
    let has_e2e_fail = orch
        .memory
        .decisions
        .iter()
        .any(|d| d.decision.contains("E2E"));
    assert!(has_e2e_fail, "应有 E2E 测试相关决策记录");
}

/// 测试 3: 无 E2E 测试用例 → 正常完成
#[tokio::test]
async fn test_no_e2e_tests_normal_completion() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    // 不创建 e2e_tests.json

    let chat = MockChat::new(vec![
        plan_json().to_string(),
        "```file:src/main.rs\nfn main() {}\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);
    let runner_clone = runner.clone();

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10);

    orch.run().await.unwrap();

    // 验证: 任务完成 (无 E2E 标记)
    assert_eq!(orch.memory.phases[0].tasks[0].status, TaskStatus::Completed);
    assert_eq!(
        orch.memory.phases[0].tasks[0].result,
        Some("成功".to_string())
    );

    // 验证: run_binary 未被调用
    assert_eq!(
        runner_clone.run_binary_call_count(),
        0,
        "无 E2E 测试时不应调用 run_binary"
    );
}

/// 测试 4: E2E 测试用例从 .forge/e2e_tests.json 加载
#[tokio::test]
async fn test_e2e_tests_loaded_from_workspace() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    // 创建包含多个用例的 e2e_tests.json
    let e2e_cases = vec![
        make_e2e_case("test1", "output1"),
        make_e2e_case("test2", "output2"),
    ];
    write_e2e_tests(dir.path(), &e2e_cases);

    let chat = MockChat::new(vec![
        plan_json().to_string(),
        "```file:src/main.rs\nfn main() {}\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);
    let runner_clone = runner.clone();

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10);

    orch.run().await.unwrap();

    // 验证: run_binary 被调用了 (有 E2E 测试)
    assert_eq!(
        runner_clone.run_binary_call_count(),
        1,
        "应调用 1 次 run_binary"
    );
}
