//! 智能错误诊断集成测试 (方向 F)
//!
//! 验证:
//! 1. HeuristicErrorDiagnoser 分类准确性
//! 2. LlmErrorDiagnoser + Mock LLM 诊断流程
//! 3. LLM 不可用时的优雅降级
//! 4. HybridErrorDiagnoser 综合诊断
//! 5. ErrorHistory 持久化 (save/load)
//! 6. ErrorHistory 相似错误查询
//! 7. Orchestrator 集成: 诊断增强修复 prompt
//! 8. Orchestrator 集成: 错误历史记录
//! 9. Orchestrator 集成: 诊断关闭时行为不变
//! 10. MockErrorDiagnoser 测试

use async_trait::async_trait;
use forge::error_diagnosis::*;
use forge::extract::ExtractedFile;
use forge::llm_clarify::LlmClient;
use forge::memory::TaskStatus;
use forge::orchestrator::Orchestrator;
use forge::testrunner::{CompileError, TestResult};
use forge::traits::{ChatClient, ChatResult, FileExtractor, TestRunner};
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

// ===== MockLlmClient for LlmErrorDiagnoser =====

struct MockLlmClient {
    responses: Arc<Mutex<Vec<String>>>,
    available: bool,
}

impl MockLlmClient {
    fn new(responses: Vec<String>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses)),
            available: true,
        }
    }

    fn single(response: &str) -> Self {
        Self::new(vec![response.to_string()])
    }

    fn unavailable() -> Self {
        Self {
            responses: Arc::new(Mutex::new(vec![])),
            available: false,
        }
    }
}

#[async_trait]
impl LlmClient for MockLlmClient {
    async fn generate(&self, _prompt: &str) -> anyhow::Result<String> {
        if !self.available {
            return Err(anyhow::anyhow!("Mock LLM 不可用"));
        }
        let mut queue = self.responses.lock().unwrap();
        if queue.is_empty() {
            return Ok("CATEGORY: Unknown\nANALYSIS: \nFIX_GUIDANCE: ".to_string());
        }
        Ok(queue.remove(0))
    }

    async fn is_available(&self) -> bool {
        self.available
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
            "error".to_string()
        },
        exit_code: if success { 0 } else { 1 },
        errors,
        test_summary: None,
    }
}

fn make_error(code: Option<&str>, msg: &str, file: &str) -> CompileError {
    CompileError {
        file: file.to_string(),
        line: Some(10),
        column: Some(5),
        message: msg.to_string(),
        error_code: code.map(String::from),
    }
}

fn ef(path: &str, content: &str) -> ExtractedFile {
    ExtractedFile {
        path: path.to_string(),
        content: content.to_string(),
        language: String::new(),
    }
}

fn make_ctx() -> DiagnosisContext {
    DiagnosisContext {
        task_prompt: "创建 CLI 工具".to_string(),
        attempt: 2,
        max_attempts: 3,
        files_written: vec!["src/main.rs".to_string()],
    }
}

fn make_plan_json() -> &'static str {
    r#"```json
[{"name":"阶段1","description":"d","tasks":[{"name":"任务1","prompt":"do it"}]}]
```"#
}

// ============================================================================
//  测试用例
// ============================================================================

// ===== 1. HeuristicErrorDiagnoser 分类 =====

#[tokio::test]
async fn test_heuristic_classifies_type_error() {
    let diagnoser = HeuristicErrorDiagnoser::new();
    let errors = vec![make_error(Some("E0308"), "mismatched types", "src/main.rs")];
    let result = diagnoser
        .diagnose(&errors, "feedback", &make_ctx(), &ErrorHistory::new())
        .await;

    assert_eq!(result.category, ErrorCategory::TypeError);
    assert!(!result.fix_guidance.is_empty());
    assert!(result.fix_guidance.contains("类型"));
    assert_eq!(result.source, "heuristic");
}

#[tokio::test]
async fn test_heuristic_classifies_borrow_error() {
    let diagnoser = HeuristicErrorDiagnoser::new();
    let errors = vec![make_error(
        Some("E0382"),
        "use of moved value",
        "src/main.rs",
    )];
    let result = diagnoser
        .diagnose(&errors, "feedback", &make_ctx(), &ErrorHistory::new())
        .await;

    assert_eq!(result.category, ErrorCategory::BorrowError);
    assert!(result.fix_guidance.contains("借用"));
}

#[tokio::test]
async fn test_heuristic_classifies_import_error() {
    let diagnoser = HeuristicErrorDiagnoser::new();
    let errors = vec![make_error(
        Some("E0432"),
        "unresolved import",
        "src/main.rs",
    )];
    let result = diagnoser
        .diagnose(&errors, "feedback", &make_ctx(), &ErrorHistory::new())
        .await;

    assert_eq!(result.category, ErrorCategory::ImportError);
}

#[tokio::test]
async fn test_heuristic_classifies_lifetime_error() {
    let diagnoser = HeuristicErrorDiagnoser::new();
    let errors = vec![make_error(Some("E0106"), "missing lifetime", "src/main.rs")];
    let result = diagnoser
        .diagnose(&errors, "feedback", &make_ctx(), &ErrorHistory::new())
        .await;

    assert_eq!(result.category, ErrorCategory::LifetimeError);
}

// ===== 2. LlmErrorDiagnoser 诊断 =====

#[tokio::test]
async fn test_llm_diagnose_returns_analysis() {
    let client = MockLlmClient::single(
        "CATEGORY: TypeError\n\
         ANALYSIS: 变量类型不匹配\n\
         FIX_GUIDANCE: 将 i32 改为 usize",
    );
    let diagnoser = LlmErrorDiagnoser::new(client);
    let errors = vec![make_error(Some("E0308"), "mismatched types", "src/main.rs")];
    let result = diagnoser
        .diagnose(&errors, "feedback", &make_ctx(), &ErrorHistory::new())
        .await;

    assert_eq!(result.category, ErrorCategory::TypeError);
    assert!(result.analysis.contains("类型不匹配"));
    assert!(result.fix_guidance.contains("usize"));
    assert_eq!(result.source, "llm");
    assert!(result.confidence > 0.8);
}

// ===== 3. LLM 不可用优雅降级 =====

#[tokio::test]
async fn test_llm_unavailable_degrades_to_heuristic() {
    let client = MockLlmClient::unavailable();
    let diagnoser = LlmErrorDiagnoser::new(client);
    let errors = vec![make_error(Some("E0308"), "mismatched types", "src/main.rs")];
    let result = diagnoser
        .diagnose(&errors, "feedback", &make_ctx(), &ErrorHistory::new())
        .await;

    assert_eq!(result.source, "heuristic_fallback");
    assert_eq!(result.category, ErrorCategory::TypeError);
}

#[tokio::test]
async fn test_llm_unparsable_degrades_to_heuristic() {
    let client = MockLlmClient::single("完全无法解析的文本");
    let diagnoser = LlmErrorDiagnoser::new(client);
    let errors = vec![make_error(Some("E0308"), "mismatched types", "src/main.rs")];
    let result = diagnoser
        .diagnose(&errors, "feedback", &make_ctx(), &ErrorHistory::new())
        .await;

    assert_eq!(result.source, "heuristic_fallback");
}

// ===== 4. HybridErrorDiagnoser 综合 =====

#[tokio::test]
async fn test_hybrid_combines_heuristic_and_llm() {
    let client = MockLlmClient::single(
        "CATEGORY: BorrowError\n\
         ANALYSIS: 所有权已转移\n\
         FIX_GUIDANCE: 使用 clone() 复制",
    );
    let diagnoser = HybridErrorDiagnoser::new(client);
    let errors = vec![make_error(
        Some("E0382"),
        "use of moved value",
        "src/main.rs",
    )];
    let result = diagnoser
        .diagnose(&errors, "feedback", &make_ctx(), &ErrorHistory::new())
        .await;

    assert_eq!(result.category, ErrorCategory::BorrowError);
    assert!(result.fix_guidance.contains("clone"));
    assert_eq!(result.source, "hybrid");
    assert!(result.confidence > 0.85);
}

#[tokio::test]
async fn test_hybrid_with_history_provides_suggestions() {
    let mut history = ErrorHistory::new();
    let err1 = make_error(Some("E0308"), "mismatched types", "src/main.rs");
    history.record(&err1, ErrorCategory::TypeError, true);

    let client = MockLlmClient::single(
        "CATEGORY: TypeError\n\
         ANALYSIS: 类型不匹配\n\
         FIX_GUIDANCE: 修改类型",
    );
    let diagnoser = HybridErrorDiagnoser::new(client);
    let errors = vec![make_error(Some("E0308"), "mismatched types", "src/main.rs")];
    let result = diagnoser
        .diagnose(&errors, "feedback", &make_ctx(), &history)
        .await;

    assert!(!result.similar_patterns.is_empty(), "应找到历史相似错误");
    assert!(
        result.fix_guidance.contains("相似历史错误") || result.fix_guidance.contains("修改类型"),
        "指导应包含历史建议或 LLM 建议"
    );
}

// ===== 5. ErrorHistory 持久化 =====

#[test]
fn test_error_history_save_and_load() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("error_history.json");

    let mut h = ErrorHistory::new();
    h.history_path = Some(path.clone());
    let err = make_error(Some("E0308"), "mismatched types", "src/main.rs");
    h.record(&err, ErrorCategory::TypeError, true);
    h.save().unwrap();

    let loaded = ErrorHistory::load(&path).unwrap();
    assert_eq!(loaded.patterns.len(), 1);
    assert_eq!(loaded.patterns[0].category, ErrorCategory::TypeError);
    assert!(loaded.patterns[0].last_fix_succeeded);
}

#[test]
fn test_error_history_load_nonexistent() {
    let h = ErrorHistory::load(std::path::Path::new("/nonexistent/error_history.json"));
    assert!(h.is_ok());
    assert!(h.unwrap().patterns.is_empty());
}

#[test]
fn test_error_history_load_from_workspace() {
    let dir = tempdir().unwrap();
    let forge_dir = dir.path().join(".forge");
    std::fs::create_dir_all(&forge_dir).unwrap();

    let mut h = ErrorHistory::new();
    h.history_path = Some(forge_dir.join("error_history.json"));
    let err = make_error(Some("E0382"), "use of moved", "src/main.rs");
    h.record(&err, ErrorCategory::BorrowError, false);
    h.save().unwrap();

    let loaded = ErrorHistory::load_from_workspace(dir.path());
    assert_eq!(loaded.patterns.len(), 1);
}

// ===== 6. ErrorHistory 相似查询 =====

#[test]
fn test_error_history_find_similar_by_code() {
    let mut h = ErrorHistory::new();
    let err1 = make_error(Some("E0308"), "mismatched: usize vs i32", "src/main.rs");
    h.record(&err1, ErrorCategory::TypeError, true);

    let query = make_error(Some("E0308"), "mismatched: String vs &str", "src/main.rs");
    let found = h.find_similar(&query);
    assert_eq!(found.len(), 1);
    assert!(found[0].last_fix_succeeded);
}

#[test]
fn test_error_history_find_successful_patterns() {
    let mut h = ErrorHistory::new();
    let err1 = make_error(Some("E0308"), "mismatched types", "src/main.rs");
    h.record(&err1, ErrorCategory::TypeError, true);

    let err2 = make_error(Some("E0382"), "use of moved", "src/main.rs");
    h.record(&err2, ErrorCategory::BorrowError, false);

    let query = make_error(Some("E0308"), "mismatched types", "src/main.rs");
    let successful = h.find_successful_patterns(&query);
    assert_eq!(successful.len(), 1);
    assert!(successful[0].last_fix_succeeded);
}

#[test]
fn test_error_history_summary() {
    let mut h = ErrorHistory::new();
    assert_eq!(h.summary(), "(无历史错误)");

    let err = make_error(Some("E0308"), "test", "src/main.rs");
    h.record(&err, ErrorCategory::TypeError, true);
    h.record(&err, ErrorCategory::TypeError, true);
    assert!(h.summary().contains("2 次出现"));
    assert!(h.summary().contains("1 个已修复"));
}

// ===== 7. Orchestrator 集成: 诊断增强修复 prompt =====

#[tokio::test]
async fn test_orchestrator_diagnosis_enhances_fix_prompt() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        make_plan_json().to_string(),
        // 第一次 attempt — 有错误的代码
        "```file:src/main.rs\nfn main() { broken }\n```".to_string(),
        // 第二次 attempt (修复轮) — 修复后的代码
        "```file:src/main.rs\nfn main() { println!(\"fixed\"); }\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            make_test_result(
                false,
                vec![make_error(Some("E0308"), "mismatched types", "src/main.rs")],
            ),
            make_test_result(true, vec![]),
        ])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() { broken }")],
        vec![ef("src/main.rs", "fn main() { println!(\"fixed\"); }")],
    ]);

    // 使用 MockErrorDiagnoser 注入诊断
    let mock_diagnoser = MockErrorDiagnoser::with_category(ErrorCategory::TypeError);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10)
        .with_error_diagnosis(Box::new(mock_diagnoser));

    orch.run().await.unwrap();

    // 验证任务最终完成
    assert_eq!(orch.memory.phases[0].tasks[0].status, TaskStatus::Completed);

    // 验证修复 prompt 包含诊断信息
    let sent = chat.sent_messages();
    // sent[0] = planning prompt, sent[1] = task prompt, sent[2] = fix prompt
    assert!(sent.len() >= 3, "应至少发送 3 条消息");
    let fix_prompt = &sent[2];
    assert!(
        fix_prompt.contains("🔍") || fix_prompt.contains("错误诊断"),
        "修复 prompt 应包含诊断信息: {}",
        fix_prompt
    );
}

// ===== 8. Orchestrator 集成: 错误历史记录 =====

#[tokio::test]
async fn test_orchestrator_records_error_history() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        make_plan_json().to_string(),
        "```file:src/main.rs\nfn main() { broken }\n```".to_string(),
        "```file:src/main.rs\nfn main() { println!(\"fixed\"); }\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            make_test_result(
                false,
                vec![make_error(Some("E0308"), "mismatched types", "src/main.rs")],
            ),
            make_test_result(true, vec![]),
        ])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() { broken }")],
        vec![ef("src/main.rs", "fn main() { println!(\"fixed\"); }")],
    ]);

    let mock_diagnoser = MockErrorDiagnoser::with_category(ErrorCategory::TypeError);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10)
        .with_error_diagnosis(Box::new(mock_diagnoser));

    orch.run().await.unwrap();

    // 验证错误历史被记录
    assert!(
        !orch.error_history.patterns.is_empty(),
        "错误历史应包含至少一个模式"
    );
    let pattern = &orch.error_history.patterns[0];
    assert_eq!(pattern.category, ErrorCategory::TypeError);
    assert_eq!(pattern.occurrences, 1);
}

// ===== 9. Orchestrator 集成: 诊断关闭时行为不变 =====

#[tokio::test]
async fn test_orchestrator_without_diagnosis_works_normally() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        make_plan_json().to_string(),
        "```file:src/main.rs\nfn main() { println!(\"hello\"); }\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    // 不启用诊断
    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10);

    orch.run().await.unwrap();

    // 正常完成
    assert_eq!(orch.memory.phases[0].tasks[0].status, TaskStatus::Completed);
    // 错误历史为空
    assert!(orch.error_history.patterns.is_empty());
    // 诊断器为 None
    assert!(orch.error_diagnoser.is_none());
}

// ===== 10. MockErrorDiagnoser =====

#[tokio::test]
async fn test_mock_error_diagnoser_returns_preset() {
    let diagnoser = MockErrorDiagnoser::with_category(ErrorCategory::BorrowError);
    let result = diagnoser
        .diagnose(&[], "feedback", &make_ctx(), &ErrorHistory::new())
        .await;

    assert_eq!(result.category, ErrorCategory::BorrowError);
    assert_eq!(result.source, "mock");
    assert!(!result.fix_guidance.is_empty());
}

#[tokio::test]
async fn test_mock_error_diagnoser_empty() {
    let diagnoser = MockErrorDiagnoser::empty();
    let result = diagnoser
        .diagnose(&[], "feedback", &make_ctx(), &ErrorHistory::new())
        .await;

    assert!(!result.has_guidance());
    assert_eq!(result.confidence, 0.0);
}

// ===== 11. ErrorCategory 分类 =====

#[test]
fn test_error_category_from_error_code() {
    assert_eq!(
        ErrorCategory::from_error_code("E0308"),
        ErrorCategory::TypeError
    );
    assert_eq!(
        ErrorCategory::from_error_code("E0382"),
        ErrorCategory::BorrowError
    );
    assert_eq!(
        ErrorCategory::from_error_code("E0425"),
        ErrorCategory::MissingItem
    );
    assert_eq!(
        ErrorCategory::from_error_code("E0432"),
        ErrorCategory::ImportError
    );
    assert_eq!(
        ErrorCategory::from_error_code("E0106"),
        ErrorCategory::LifetimeError
    );
    assert_eq!(
        ErrorCategory::from_error_code("E0502"),
        ErrorCategory::BorrowError
    );
}

#[test]
fn test_error_category_from_message() {
    assert_eq!(
        ErrorCategory::from_message("mismatched types"),
        ErrorCategory::TypeError
    );
    assert_eq!(
        ErrorCategory::from_message("cannot borrow `x` as mutable"),
        ErrorCategory::BorrowError
    );
    assert_eq!(
        ErrorCategory::from_message("unresolved import"),
        ErrorCategory::ImportError
    );
}

#[test]
fn test_error_category_display() {
    assert_eq!(format!("{}", ErrorCategory::TypeError), "类型错误");
    assert_eq!(format!("{}", ErrorCategory::BorrowError), "借用/所有权错误");
}

// ===== 12. 多错误诊断 =====

#[tokio::test]
async fn test_heuristic_diagnose_multiple_errors() {
    let diagnoser = HeuristicErrorDiagnoser::new();
    let errors = vec![
        make_error(Some("E0308"), "mismatched types", "src/main.rs"),
        make_error(Some("E0425"), "cannot find `x`", "src/main.rs"),
        make_error(Some("E0382"), "use of moved value", "src/lib.rs"),
    ];
    let result = diagnoser
        .diagnose(&errors, "feedback", &make_ctx(), &ErrorHistory::new())
        .await;

    // 以第一个错误为主分类
    assert_eq!(result.category, ErrorCategory::TypeError);
    assert!(result.analysis.contains("src/main.rs"));
}
