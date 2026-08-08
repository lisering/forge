//! 循环终止检测集成测试 (借鉴方向 3)
//!
//! 验证:
//! 1. LoopDetector 检测同一编译错误反复出现 → 触发策略改变
//! 2. 策略改变后仍死循环 → 建议跳过任务
//! 3. 禁用循环终止检测 (max_repeats=0) → 正常修复循环行为
//! 4. 不同错误不触发循环检测
//! 5. 任务间 LoopDetector 重置
//! 6. 策略 prompt 包含错误信息
//! 7. max_repeats=2 更快触发检测
//! 8. 循环终止与智能错误诊断共存

use async_trait::async_trait;
use forge::extract::ExtractedFile;
use forge::loop_detector::LoopDetector;
use forge::orchestrator::Orchestrator;
use forge::testrunner::{CompileError, TestResult};
use forge::traits::{ChatClient, ChatResult, FileExtractor, TestRunner};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;

// ============================================================================
//  Mock 实现
// ============================================================================

/// Mock ChatClient — 按顺序返回预编程回复, 记录所有收到的消息
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

/// 生成一个代码回复 (足够长以避免触发过短检测)
fn code_response() -> String {
    "以下是修复后的代码实现，包含了必要的修改和优化。\n```file:src/main.rs\nfn main() { let x = 1; println!(\"{}\", x); }\n```".to_string()
}

/// 生成一个成功的代码回复
fn success_code_response() -> String {
    "以下是完整的代码实现，已通过所有测试验证。\n```file:src/main.rs\nfn main() { println!(\"hello\"); }\n```".to_string()
}

// ============================================================================
//  测试用例
// ============================================================================

/// 测试 1: 循环终止检测触发策略改变 — 同一编译错误出现 3 次后改变策略
#[tokio::test]
async fn test_loop_detection_triggers_strategy_change() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        // planning phase
        r#"```json
[{"name":"阶段1","description":"d","tasks":[{"name":"任务1","prompt":"do it"}]}]
```"#
            .to_string(),
        // 4 次 execute_task 回复
        code_response(),
        code_response(),
        code_response(),
        code_response(),
    ]);

    // 同一编译错误反复出现
    let same_error = vec![make_compile_error(
        "src/main.rs",
        10,
        "mismatched types: expected usize, found i32",
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

    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 5, 10).with_loop_detection(3);

    orch.run().await.unwrap();

    // 验证: 任务失败 (被循环终止跳过)
    assert_eq!(
        orch.memory.phases[0].tasks[0].status,
        forge::memory::TaskStatus::Failed,
        "任务应因循环终止而失败"
    );

    // 验证: 尝试次数为 4 (第 4 次触发 should_skip)
    assert_eq!(
        orch.memory.phases[0].tasks[0].attempts, 4,
        "应在第 4 次尝试时跳过 (3 次记录 + 1 次策略改变后跳过)"
    );

    // 验证: 第 4 次发送的消息包含策略改变提示
    let sent = chat.sent_messages();
    assert!(
        sent.len() >= 5,
        "应至少发送 5 条消息 (planning + 4 attempts)"
    );

    let attempt4_msg = &sent[4]; // index 4 = 第 4 次 attempt
    assert!(
        attempt4_msg.contains("循环终止检测") || attempt4_msg.contains("换一种完全不同的方法"),
        "第 4 次修复 prompt 应包含策略改变提示, 实际: {}",
        &attempt4_msg[..attempt4_msg.len().min(200)]
    );
}

/// 测试 2: 循环终止检测跳过任务 — 策略改变后仍死循环 → 建议跳过
#[tokio::test]
async fn test_loop_detection_skips_task_early() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        // planning phase
        r#"```json
[{"name":"阶段1","description":"d","tasks":[{"name":"任务1","prompt":"do it"}]}]
```"#
            .to_string(),
        // 4 次 execute_task 回复
        code_response(),
        code_response(),
        code_response(),
        code_response(),
    ]);

    let same_error = vec![make_compile_error(
        "src/lib.rs",
        5,
        "use of moved value: `x`",
        "E0382",
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

    // max_rounds=10, 但应在 attempt 4 就被跳过 (远早于 10)
    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 10, 10).with_loop_detection(3);

    orch.run().await.unwrap();

    // 验证: 任务失败
    assert_eq!(
        orch.memory.phases[0].tasks[0].status,
        forge::memory::TaskStatus::Failed
    );

    // 验证: 提前跳过 (远早于 max_rounds=10)
    assert_eq!(
        orch.memory.phases[0].tasks[0].attempts, 4,
        "应在第 4 次尝试时跳过, 而非等到 max_rounds=10"
    );

    // 验证: 决策日志包含循环终止
    let has_loop_decision = orch
        .memory
        .decisions
        .iter()
        .any(|d| d.decision.contains("循环终止") || d.decision.contains("跳过"));
    assert!(has_loop_decision, "决策日志应包含循环终止记录");
}

/// 测试 3: 禁用循环终止检测 — max_repeats=0 时正常修复循环
#[tokio::test]
async fn test_loop_detection_disabled() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        r#"```json
[{"name":"阶段1","description":"d","tasks":[{"name":"任务1","prompt":"do it"}]}]
```"#
            .to_string(),
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
        ])
        .with_test_results(vec![]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/main.rs", "fn main() {}")],
    ]);

    // loop_detection=0 (禁用), max_rounds=3
    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10).with_loop_detection(0);

    orch.run().await.unwrap();

    // 验证: 任务失败 (正常 max_rounds 耗尽)
    assert_eq!(
        orch.memory.phases[0].tasks[0].status,
        forge::memory::TaskStatus::Failed
    );
    assert_eq!(
        orch.memory.phases[0].tasks[0].attempts, 3,
        "应尝试 3 次 (max_rounds), 不因循环终止提前跳过"
    );

    // 验证: 没有策略改变 prompt
    let sent = chat.sent_messages();
    for msg in &sent {
        assert!(
            !msg.contains("循环终止检测"),
            "禁用循环检测时不应有策略改变 prompt"
        );
    }
}

/// 测试 4: 不同错误不触发循环检测
#[tokio::test]
async fn test_loop_detection_no_loop_different_errors() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        r#"```json
[{"name":"阶段1","description":"d","tasks":[{"name":"任务1","prompt":"do it"}]}]
```"#
            .to_string(),
        code_response(),
        code_response(),
        code_response(),
    ]);

    // 每次不同的错误 (不同 error_code, 不同消息, 不同文件)
    let runner = MockTestRunner::new()
        .with_check_results(vec![
            make_test_result(
                false,
                vec![make_compile_error("src/main.rs", 10, "type error", "E0308")],
            ),
            make_test_result(
                false,
                vec![make_compile_error(
                    "src/lib.rs",
                    20,
                    "borrow error",
                    "E0382",
                )],
            ),
            make_test_result(
                false,
                vec![make_compile_error("src/utils.rs", 30, "not found", "E0425")],
            ),
        ])
        .with_test_results(vec![]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/main.rs", "fn main() {}")],
    ]);

    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10).with_loop_detection(3);

    orch.run().await.unwrap();

    // 验证: 任务失败 (正常 max_rounds 耗尽, 非循环终止)
    assert_eq!(
        orch.memory.phases[0].tasks[0].status,
        forge::memory::TaskStatus::Failed
    );
    assert_eq!(
        orch.memory.phases[0].tasks[0].attempts, 3,
        "不同错误不应触发循环终止, 应正常尝试 3 次"
    );

    // 验证: 没有策略改变 prompt
    let sent = chat.sent_messages();
    for msg in &sent {
        assert!(
            !msg.contains("循环终止检测"),
            "不同错误不应触发循环终止检测"
        );
    }
}

/// 测试 5: 任务间 LoopDetector 重置
#[tokio::test]
async fn test_loop_detection_resets_between_tasks() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    // 2 个任务
    let chat = MockChat::new(vec![
        // planning phase
        r#"```json
[{"name":"阶段1","description":"d","tasks":[{"name":"任务1","prompt":"task1"},{"name":"任务2","prompt":"task2"}]}]
```"#
            .to_string(),
        // 任务 1: 4 次回复 (将被循环终止跳过)
        code_response(),
        code_response(),
        code_response(),
        code_response(),
        // 任务 2: 1 次回复 (成功)
        success_code_response(),
    ]);

    let same_error = vec![make_compile_error(
        "src/main.rs",
        10,
        "mismatched types",
        "E0308",
    )];

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            // 任务 1: 4 次失败 (同一错误)
            make_test_result(false, same_error.clone()),
            make_test_result(false, same_error.clone()),
            make_test_result(false, same_error.clone()),
            make_test_result(false, same_error.clone()),
            // 任务 2: 1 次成功
            make_test_result(true, vec![]),
        ])
        .with_test_results(vec![
            // 任务 2: 测试成功
            make_test_result(true, vec![]),
        ]);

    let extractor = MockExtractor::new(vec![
        // 任务 1: 4 次文件提取
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/main.rs", "fn main() {}")],
        // 任务 2: 1 次文件提取
        vec![ef("src/main.rs", "fn main() { println!(\"hello\"); }")],
    ]);

    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 5, 10).with_loop_detection(3);

    orch.run().await.unwrap();

    // 验证: 任务 1 失败 (循环终止)
    assert_eq!(
        orch.memory.phases[0].tasks[0].status,
        forge::memory::TaskStatus::Failed,
        "任务 1 应因循环终止失败"
    );
    assert_eq!(orch.memory.phases[0].tasks[0].attempts, 4);

    // 验证: 任务 2 成功 (LoopDetector 已重置, 不受任务 1 影响)
    assert_eq!(
        orch.memory.phases[0].tasks[1].status,
        forge::memory::TaskStatus::Completed,
        "任务 2 应成功 (LoopDetector 已重置)"
    );
    assert_eq!(orch.memory.phases[0].tasks[1].attempts, 1);
}

/// 测试 6: 策略 prompt 包含错误信息
#[tokio::test]
async fn test_loop_detection_strategy_prompt_content() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        r#"```json
[{"name":"阶段1","description":"d","tasks":[{"name":"任务1","prompt":"do it"}]}]
```"#
            .to_string(),
        code_response(),
        code_response(),
        code_response(),
        code_response(),
    ]);

    let error_msg = "mismatched types: expected `usize`, found `i32`";
    let same_error = vec![make_compile_error("src/main.rs", 42, error_msg, "E0308")];

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

    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 5, 10).with_loop_detection(3);

    orch.run().await.unwrap();

    let sent = chat.sent_messages();
    let strategy_msg = &sent[4]; // 第 4 次 attempt 的消息

    // 验证: 包含循环终止标记
    assert!(
        strategy_msg.contains("循环终止检测"),
        "应包含循环终止检测标记"
    );

    // 验证: 包含错误信息
    assert!(
        strategy_msg.contains("mismatched types") || strategy_msg.contains("E0308"),
        "策略 prompt 应包含错误信息"
    );

    // 验证: 包含策略改变指导
    assert!(
        strategy_msg.contains("换一种完全不同的方法"),
        "应包含'换一种完全不同的方法'指导"
    );
}

/// 测试 7: max_repeats=2 更快触发检测
#[tokio::test]
async fn test_loop_detection_max_repeats_2() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        r#"```json
[{"name":"阶段1","description":"d","tasks":[{"name":"任务1","prompt":"do it"}]}]
```"#
            .to_string(),
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
        ])
        .with_test_results(vec![]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/main.rs", "fn main() {}")],
    ]);

    // max_repeats=2, max_rounds=10 → 应在 attempt 3 跳过
    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 10, 10).with_loop_detection(2);

    orch.run().await.unwrap();

    // 验证: 任务失败
    assert_eq!(
        orch.memory.phases[0].tasks[0].status,
        forge::memory::TaskStatus::Failed
    );

    // 验证: 在 attempt 3 跳过 (2 次记录 + 1 次策略改变后跳过)
    assert_eq!(
        orch.memory.phases[0].tasks[0].attempts, 3,
        "max_repeats=2 时应在第 3 次尝试时跳过"
    );
}

/// 测试 8: 循环终止检测与智能错误诊断共存
#[tokio::test]
async fn test_loop_detection_with_error_diagnosis() {
    use forge::error_diagnosis::{DiagnosisResult, ErrorCategory, MockErrorDiagnoser};

    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        r#"```json
[{"name":"阶段1","description":"d","tasks":[{"name":"任务1","prompt":"do it"}]}]
```"#
            .to_string(),
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

    let diagnoser = Box::new(MockErrorDiagnoser::new(DiagnosisResult {
        category: ErrorCategory::TypeError,
        analysis: "类型不匹配分析".to_string(),
        fix_guidance: "修改类型声明".to_string(),
        similar_patterns: vec![],
        confidence: 0.9,
        source: "mock".to_string(),
    }));

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 5, 10)
        .with_loop_detection(3)
        .with_error_diagnosis(diagnoser);

    orch.run().await.unwrap();

    // 验证: 任务失败 (循环终止)
    assert_eq!(
        orch.memory.phases[0].tasks[0].status,
        forge::memory::TaskStatus::Failed
    );
    assert_eq!(orch.memory.phases[0].tasks[0].attempts, 4);

    // 验证: 第 4 次消息同时包含错误诊断和循环终止
    let sent = chat.sent_messages();
    let attempt4_msg = &sent[4];

    assert!(
        attempt4_msg.contains("循环终止检测"),
        "应包含循环终止检测 (与错误诊断共存)"
    );

    // 检查是否有错误诊断信息 (可能在之前的修复 prompt 中)
    let has_diagnosis = sent
        .iter()
        .any(|m| m.contains("🔍") || m.contains("错误诊断"));
    assert!(has_diagnosis, "应有错误诊断信息 (与循环终止共存)");
}

/// 测试 9: LoopDetector 单元测试 — is_looping 和 should_skip 的时序
#[test]
fn test_loop_detector_isolation() {
    let mut detector = LoopDetector::new(3);
    let errors = vec![make_compile_error("src/main.rs", 10, "error", "E0308")];

    // 1 轮: 不死循环
    detector.record_errors(&errors);
    assert!(!detector.is_looping());
    assert!(!detector.should_skip());

    // 2 轮: 不死循环
    detector.record_errors(&errors);
    assert!(!detector.is_looping());
    assert!(!detector.should_skip());

    // 3 轮: 死循环, 但不应跳过 (策略未改变)
    detector.record_errors(&errors);
    assert!(detector.is_looping());
    assert!(!detector.should_skip());

    // 改变策略
    let prompt = detector.loop_strategy_prompt();
    assert!(prompt.contains("换一种完全不同的方法"));
    assert!(detector.strategy_changed);

    // 策略改变后仍死循环 → 应跳过
    assert!(detector.should_skip());

    // 重置后恢复初始状态
    detector.reset();
    assert!(!detector.is_looping());
    assert!(!detector.should_skip());
    assert!(!detector.strategy_changed);
    assert_eq!(detector.round_count(), 0);
}

/// 测试 10: 文件路径维度触发循环检测
#[tokio::test]
async fn test_loop_detection_by_file_path() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        r#"```json
[{"name":"阶段1","description":"d","tasks":[{"name":"任务1","prompt":"do it"}]}]
```"#
            .to_string(),
        code_response(),
        code_response(),
        code_response(),
        code_response(),
    ]);

    // 不同 error_code 和消息, 但相同文件路径
    let runner = MockTestRunner::new()
        .with_check_results(vec![
            make_test_result(
                false,
                vec![make_compile_error("src/main.rs", 10, "type error", "E0308")],
            ),
            make_test_result(
                false,
                vec![make_compile_error(
                    "src/main.rs",
                    20,
                    "borrow error",
                    "E0382",
                )],
            ),
            make_test_result(
                false,
                vec![make_compile_error("src/main.rs", 30, "not found", "E0425")],
            ),
            make_test_result(
                false,
                vec![make_compile_error(
                    "src/main.rs",
                    40,
                    "syntax error",
                    "E0004",
                )],
            ),
        ])
        .with_test_results(vec![]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/main.rs", "fn main() {}")],
    ]);

    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 5, 10).with_loop_detection(3);

    orch.run().await.unwrap();

    // 验证: 任务失败 (文件路径维度触发循环检测)
    assert_eq!(
        orch.memory.phases[0].tasks[0].status,
        forge::memory::TaskStatus::Failed
    );
    assert_eq!(
        orch.memory.phases[0].tasks[0].attempts, 4,
        "文件路径重复应触发循环终止 (src/main.rs 连续 3+ 次出错)"
    );
}

/// 测试 11: 循环终止后第二个 prompt 是"建议跳过"
#[tokio::test]
async fn test_loop_detection_second_strategy_is_skip() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    // max_rounds=6, loop_detection=3
    // Attempt 1-3: 记录错误, 第 3 次后 is_looping=true
    // Attempt 4: 策略改变 (换方法), 仍失败, should_skip=true → 跳过
    // 所以只需要 4 次回复
    let chat = MockChat::new(vec![
        r#"```json
[{"name":"阶段1","description":"d","tasks":[{"name":"任务1","prompt":"do it"}]}]
```"#
            .to_string(),
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

    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 6, 10).with_loop_detection(3);

    orch.run().await.unwrap();

    let sent = chat.sent_messages();
    let attempt4_msg = &sent[4];

    // 第 4 次修复 prompt 应包含"换方法"策略 (首次策略改变)
    assert!(
        attempt4_msg.contains("换一种完全不同的方法"),
        "第 4 次应使用'换方法'策略 (首次策略改变)"
    );
}
