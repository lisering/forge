//! LLM 增强自主追问集成测试 — 验证 HybridClarificationChecker 与 Orchestrator 集成
//!
//! 测试场景:
//! 1. LLM 检测到隐晦提问 → 触发追问 → AI 提供代码 → 任务成功
//! 2. LLM 不可用 → 优雅降级 → 正常流程不中断
//! 3. Hybrid: 启发式优先捕获明显提问 (LLM 不被调用)
//! 4. Hybrid: 启发式未检出 → LLM 兜底检测
//! 5. LLM 追问历史记录在 Memory 中
//! 6. Planning 阶段 LLM 追问
//! 7. LLM 追问后 AI 仍无代码 → 继续修复循环
//! 8. 超时 → 启发式优先检测 (不调用 LLM)
//!
//! **关键**: HybridClarificationChecker 在 planning 和 task 阶段都会调用 LLM
//! (当启发式未检测到问题时)。因此 MockLlmClient 的响应队列需要
//! 包含 planning 阶段的 "OK" 响应 + task 阶段的实际响应。

use async_trait::async_trait;
use forge::extract::ExtractedFile;
use forge::llm_clarify::{HybridClarificationChecker, LlmClient};
use forge::memory::{Memory, TaskStatus};
use forge::orchestrator::Orchestrator;
use forge::testrunner::{CompileError, TestResult};
use forge::traits::{ChatClient, ChatResult, FileExtractor, TestRunner};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;

// ============================================================================
//  Mock 实现 (复用自 clarification.rs 模式)
// ============================================================================

/// Mock ChatClient — 按顺序返回预编程回复
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
        let (text, timed_out) = if let Some(stripped) = text.strip_prefix("[TIMEOUT]") {
            (stripped.to_string(), true)
        } else {
            (text, false)
        };
        Ok(ChatResult { text, timed_out })
    }
}

/// Mock TestRunner
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

/// Mock FileExtractor
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

/// Mock LLM 客户端 — 按顺序返回预编程回复
#[derive(Clone)]
struct MockLlmClient {
    responses: Arc<Mutex<Vec<String>>>,
    call_count: Arc<Mutex<u32>>,
}

impl MockLlmClient {
    fn new(responses: Vec<String>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses)),
            call_count: Arc::new(Mutex::new(0)),
        }
    }

    fn unavailable() -> Self {
        Self {
            responses: Arc::new(Mutex::new(vec![])),
            call_count: Arc::new(Mutex::new(0)),
        }
    }

    fn call_count(&self) -> u32 {
        *self.call_count.lock().unwrap()
    }
}

#[async_trait]
impl LlmClient for MockLlmClient {
    async fn generate(&self, _prompt: &str) -> anyhow::Result<String> {
        let mut count = self.call_count.lock().unwrap();
        *count += 1;
        drop(count);

        let mut queue = self.responses.lock().unwrap();
        if queue.is_empty() {
            return Err(anyhow::anyhow!("Mock LLM 不可用"));
        }
        Ok(queue.remove(0))
    }

    async fn is_available(&self) -> bool {
        !self.responses.lock().unwrap().is_empty()
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

/// LLM "OK" 响应 (不需要追问)
fn llm_ok() -> String {
    "OK".to_string()
}

/// LLM "需要追问" 响应
fn llm_needs_clarification(reason: &str, follow_up: &str) -> String {
    format!("NEEDS_CLARIFICATION: {}\nFOLLOW_UP: {}", reason, follow_up)
}

// ============================================================================
//  测试用例
// ============================================================================

/// 测试 1: LLM 检测到隐晦提问 → 触发追问 → AI 提供代码 → 任务成功
///
/// AI 回复没有明显的提问标记 (启发式不会触发),
/// 但 LLM 能判断出 AI 在犹豫, 触发追问。
#[tokio::test]
async fn test_llm_detects_subtle_question_triggers_clarification() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        // planning 回复
        plan_json().to_string(),
        // task 回复 — 隐晦的不确定, 启发式不会触发
        "我觉得可以用 tokio，也可以考虑 async-std，各有优劣。".to_string(),
        // 追问后的回复 — 包含代码
        "```file:src/main.rs\nfn main() { println!(\"hello\"); }\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    // LLM: planning 说 OK, task 检测到犹豫
    let llm_client = MockLlmClient::new(vec![
        llm_ok(), // planning: LLM 说 OK
        llm_needs_clarification("AI 对技术方案表达了犹豫", "请选择最佳方案并直接输出代码。"), // task: LLM 检测到
    ]);

    let hybrid_checker = HybridClarificationChecker::new(llm_client);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10)
        .with_clarification(hybrid_checker);

    orch.run().await.unwrap();

    // 验证: 任务成功
    assert_eq!(orch.memory.phases[0].tasks[0].status, TaskStatus::Completed);

    // 验证: 发送了 3 条消息 (planning + task + clarification)
    let sent = orch.chat.sent_messages();
    assert_eq!(
        sent.len(),
        3,
        "应发送 3 条消息: planning + task + clarification"
    );

    // 验证: 第 3 条是 LLM 生成的追问
    assert!(
        sent[2].contains("选择最佳方案") || sent[2].contains("代码"),
        "第 3 条消息应是 LLM 生成的追问: {}",
        sent[2]
    );

    // 验证: 追问历史已记录
    assert_eq!(
        orch.memory.phases[0].tasks[0].clarifications.len(),
        1,
        "应记录 1 次 LLM 追问"
    );
}

/// 测试 2: LLM 不可用 → 优雅降级 → 不追问 → 正常流程
#[tokio::test]
async fn test_llm_unavailable_graceful_degradation() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        plan_json().to_string(),
        // task 回复 — 正常代码, 无需追问
        "```file:src/main.rs\nfn main() { println!(\"hello\"); }\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    // LLM 不可用
    let llm_client = MockLlmClient::unavailable();
    let hybrid_checker = HybridClarificationChecker::new(llm_client);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10)
        .with_clarification(hybrid_checker);

    orch.run().await.unwrap();

    // 验证: 任务成功 (LLM 不可用不影响正常流程)
    assert_eq!(orch.memory.phases[0].tasks[0].status, TaskStatus::Completed);

    // 验证: 只发送了 2 条消息 (planning + task), 无追问
    let sent = orch.chat.sent_messages();
    assert_eq!(sent.len(), 2, "LLM 不可用不应影响正常流程");

    // 验证: 无追问历史
    assert_eq!(orch.memory.phases[0].tasks[0].clarifications.len(), 0);
}

/// 测试 3: Hybrid — 启发式优先捕获明显提问 (LLM 不被调用 for task)
///
/// AI 回复有明显提问标记 → 启发式捕获 → LLM 不被调用 (for task)
/// (LLM 仍会被调用 for planning, 返回 OK)
#[tokio::test]
async fn test_hybrid_heuristic_catches_obvious_question() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        plan_json().to_string(),
        // task 回复 — 明显的提问, 启发式会捕获
        "你希望用哪种框架？请告诉我。".to_string(),
        // 追问后的回复
        "```file:src/main.rs\nfn main() {}\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    // LLM: planning 说 OK; task 不应被调用 (启发式先捕获)
    let llm_client = MockLlmClient::new(vec![llm_ok()]); // 只有 planning 用

    let hybrid_checker = HybridClarificationChecker::new(llm_client.clone());

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10)
        .with_clarification(hybrid_checker);

    orch.run().await.unwrap();

    // 验证: 任务成功 (启发式捕获了提问, LLM 没被调用 for task)
    assert_eq!(orch.memory.phases[0].tasks[0].status, TaskStatus::Completed);

    // 验证: 发送了 3 条消息 (planning + task + heuristic clarification)
    let sent = orch.chat.sent_messages();
    assert_eq!(sent.len(), 3);

    // 验证: 追问来自启发式 (包含 file:路径)
    assert!(
        sent[2].contains("file:路径"),
        "追问应来自启发式 (包含 file:路径): {}",
        sent[2]
    );

    // 验证: LLM 只被调用 1 次 (planning), task 阶段未调用
    assert_eq!(
        llm_client.call_count(),
        1,
        "LLM 应只被调用 1 次 (planning), task 阶段启发式先捕获"
    );
}

/// 测试 4: Hybrid — 启发式未检出 → LLM 兜底检测
#[tokio::test]
async fn test_hybrid_llm_catches_after_heuristic_miss() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        plan_json().to_string(),
        // task 回复 — 没有明显提问标记, 启发式不会触发
        // 但回复中没有代码文件, LLM 应检测到
        "好的, 我来分析一下需求, 然后开始设计架构方案。".to_string(),
        // 追问后的回复
        "```file:src/main.rs\nfn main() {}\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    // LLM: planning 说 OK; task 检测到无代码
    let llm_client = MockLlmClient::new(vec![
        llm_ok(), // planning
        llm_needs_clarification(
            "AI 回复中没有代码文件",
            "请直接用 file:路径 格式输出所有代码文件。",
        ), // task
    ]);

    let hybrid_checker = HybridClarificationChecker::new(llm_client);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10)
        .with_clarification(hybrid_checker);

    orch.run().await.unwrap();

    // 验证: 任务成功
    assert_eq!(orch.memory.phases[0].tasks[0].status, TaskStatus::Completed);

    // 验证: 发送了 3 条消息 (planning + task + LLM clarification)
    let sent = orch.chat.sent_messages();
    assert_eq!(sent.len(), 3, "应发送 3 条消息");

    // 验证: 追问来自 LLM (包含 LLM 判断标记)
    assert!(
        orch.memory.phases[0].tasks[0]
            .clarifications
            .iter()
            .any(|c| c.contains("file:路径")),
        "LLM 追问应包含 file:路径 格式要求"
    );
}

/// 测试 5: LLM 追问历史记录在 Memory 中, 可持久化
#[tokio::test]
async fn test_llm_clarification_recorded_in_memory() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        plan_json().to_string(),
        "我觉得可以用 tokio，也可以考虑 async-std。".to_string(),
        "```file:src/main.rs\nfn main() {}\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let llm_client = MockLlmClient::new(vec![
        llm_ok(),                                                           // planning
        llm_needs_clarification("AI 表达了犹豫", "请选择方案并输出代码。"), // task
    ]);

    let hybrid_checker = HybridClarificationChecker::new(llm_client);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10)
        .with_clarification(hybrid_checker);

    orch.run().await.unwrap();

    // 验证: memory.json 存在且可加载
    let memory_path = format!("{}/.forge/memory.json", ws_dir);
    let loaded = Memory::load(std::path::Path::new(&memory_path)).unwrap();

    // 验证: LLM 追问历史已持久化
    assert_eq!(loaded.phases[0].tasks[0].clarifications.len(), 1);
    assert!(
        !loaded.phases[0].tasks[0].clarifications[0].is_empty(),
        "追问内容不应为空"
    );

    // 验证: 决策记录中有 "自主追问"
    let has_clarification_decision = loaded.decisions.iter().any(|d| d.decision == "自主追问");
    assert!(has_clarification_decision, "决策记录应包含 '自主追问'");
}

/// 测试 6: Planning 阶段 LLM 追问
#[tokio::test]
async fn test_llm_clarification_in_planning_phase() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        // planning 回复 — AI 先说了一段分析, 没有给计划
        "让我分析一下需求。这个项目需要一个合适的架构设计。".to_string(),
        // 追问后的回复 — AI 给出计划
        plan_json().to_string(),
        // task 回复
        "```file:src/main.rs\nfn main() {}\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    // LLM: planning 检测到没有计划 → 追问; task 说 OK
    let llm_client = MockLlmClient::new(vec![
        llm_needs_clarification("AI 没有给出开发计划", "请直接给出开发计划的 JSON。"), // planning
        llm_ok(),                                                                      // task
    ]);

    let hybrid_checker = HybridClarificationChecker::new(llm_client);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10)
        .with_clarification(hybrid_checker);

    orch.run().await.unwrap();

    // 验证: 任务成功
    assert_eq!(orch.memory.phases[0].tasks[0].status, TaskStatus::Completed);

    // 验证: 发送了 3 条消息 (planning + planning_clarification + task)
    let sent = orch.chat.sent_messages();
    assert_eq!(
        sent.len(),
        3,
        "应发送 3 条消息: planning + clarification + task"
    );

    // 验证: 决策记录中有 planning 追问
    let has_planning_clarification = orch
        .memory
        .decisions
        .iter()
        .any(|d| d.decision == "planning 自主追问");
    assert!(has_planning_clarification, "应有 planning 追问决策记录");
}

/// 测试 7: LLM 追问后 AI 仍无代码 → 继续修复循环
#[tokio::test]
async fn test_llm_clarification_then_fix_loop() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        // planning
        plan_json().to_string(),
        // attempt 1: AI 没有给代码 → LLM 追问 → AI 仍无代码
        "好的, 我来分析一下这个项目的需求和架构设计。".to_string(),
        "好的, 我继续思考设计方案。".to_string(), // 追问后仍无代码 → continue
        // attempt 2: 修复轮 → AI 提供代码
        "```file:src/main.rs\nfn main() { println!(\"ok\"); }\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![
        vec![],                                  // attempt 1: 追问后仍无代码
        vec![ef("src/main.rs", "fn main() {}")], // attempt 2: 有代码
    ]);

    // LLM: planning OK, attempt1 检测到无代码, attempt2 OK
    let llm_client = MockLlmClient::new(vec![
        llm_ok(),                                                       // planning
        llm_needs_clarification("AI 没有输出代码", "请直接输出代码。"), // attempt 1
        llm_ok(),                                                       // attempt 2
    ]);

    let hybrid_checker = HybridClarificationChecker::new(llm_client);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10)
        .with_clarification(hybrid_checker);

    orch.run().await.unwrap();

    // 验证: 任务最终成功 (在第 2 次尝试)
    assert_eq!(orch.memory.phases[0].tasks[0].status, TaskStatus::Completed);
    assert_eq!(orch.memory.phases[0].tasks[0].attempts, 2);

    // 验证: 第 1 次尝试有 LLM 追问
    assert_eq!(
        orch.memory.phases[0].tasks[0].clarifications.len(),
        1,
        "应有 1 次 LLM 追问"
    );

    // 消息数: planning(1) + attempt1_prompt(1) + attempt1_clarification(1) + attempt2_fix_prompt(1) = 4
    let sent = orch.chat.sent_messages();
    assert_eq!(sent.len(), 4, "应发送 4 条消息");
}

/// 测试 8: 超时 → 启发式优先检测 (不调用 LLM for task)
#[tokio::test]
async fn test_hybrid_timeout_triggers_heuristic_not_llm() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        plan_json().to_string(),
        // task 回复 — 超时
        "[TIMEOUT]好的，我来创建```file:src/main.rs\nfn main() {".to_string(),
        // 追问后的完整回复
        "```file:src/main.rs\nfn main() { println!(\"hello\"); }\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    // LLM: planning OK; task 不应被调用 (超时由启发式处理)
    let llm_client = MockLlmClient::new(vec![llm_ok()]); // 只有 planning 用

    let hybrid_checker = HybridClarificationChecker::new(llm_client.clone());

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10)
        .with_clarification(hybrid_checker);

    orch.run().await.unwrap();

    // 验证: 任务成功
    assert_eq!(orch.memory.phases[0].tasks[0].status, TaskStatus::Completed);

    // 验证: 发送了 3 条消息 (planning + task + timeout clarification)
    let sent = orch.chat.sent_messages();
    assert_eq!(sent.len(), 3);

    // 验证: 追问包含"未完成" (来自启发式的超时追问)
    assert!(
        sent[2].contains("未完成"),
        "超时追问应来自启发式 (包含'未完成'): {}",
        sent[2]
    );

    // 验证: LLM 只被调用 1 次 (planning), task 阶段超时由启发式处理
    assert_eq!(
        llm_client.call_count(),
        1,
        "LLM 应只被调用 1 次 (planning), task 超时由启发式处理"
    );
}

/// 测试 9: LLM 生成的追问消息包含 file:路径 格式要求
#[tokio::test]
async fn test_llm_clarification_message_contains_format_instruction() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        plan_json().to_string(),
        "我觉得可以用 tokio。".to_string(),
        "```file:src/main.rs\nfn main() {}\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    // LLM 没有提供 FOLLOW_UP, 应使用默认模板 (包含 file:路径)
    let llm_client = MockLlmClient::new(vec![
        llm_ok(),                                         // planning
        "NEEDS_CLARIFICATION: AI 回复不完整".to_string(), // task: no FOLLOW_UP
    ]);

    let hybrid_checker = HybridClarificationChecker::new(llm_client);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10)
        .with_clarification(hybrid_checker);

    orch.run().await.unwrap();

    let sent = orch.chat.sent_messages();
    let clarification_msg = &sent[2]; // 第 3 条是追问

    // 默认追问应包含 file:路径 格式要求
    assert!(
        clarification_msg.contains("file:路径"),
        "默认追问应包含 file:路径 格式要求: {}",
        clarification_msg
    );
}

/// 测试 10: 启发式优先于 LLM — 启发式检测到提问时 LLM 不被调用 (for task)
#[tokio::test]
async fn test_heuristic_priority_over_llm_for_task() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        plan_json().to_string(),
        // 明显的提问, 启发式会触发
        "你希望用哪种框架？".to_string(),
        // 追问后的回复
        "```file:src/main.rs\nfn main() {}\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    // LLM: planning 说 OK; task 不应被调用 (启发式先捕获)
    let llm_client = MockLlmClient::new(vec![llm_ok()]); // 只有 planning 用

    let hybrid_checker = HybridClarificationChecker::new(llm_client.clone());

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10)
        .with_clarification(hybrid_checker);

    orch.run().await.unwrap();

    // 验证: 任务成功
    assert_eq!(orch.memory.phases[0].tasks[0].status, TaskStatus::Completed);

    // 验证: 追问来自启发式
    assert_eq!(
        orch.memory.phases[0].tasks[0].clarifications.len(),
        1,
        "应有 1 次启发式追问"
    );

    // 验证: LLM 只被调用 1 次 (planning), task 阶段启发式先捕获
    assert_eq!(
        llm_client.call_count(),
        1,
        "LLM 应只被调用 1 次 (planning), task 启发式先捕获"
    );
}
