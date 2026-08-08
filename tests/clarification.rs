//! 自主提问能力集成测试 — 验证 Agent 自主追问的核心逻辑
//!
//! 测试场景:
//! 1. AI 回复含疑问 → Agent 自动追问 → AI 提供代码 → 任务成功
//! 2. AI 回复超时 → Agent 追问要求继续 → AI 完成 → 任务成功
//! 3. 正常回复 → 无需追问 → 正常流程
//! 4. 达到追问上限 → 不再追问 → 继续处理
//! 5. 追问历史记录在 Memory 中
//! 6. Planning 阶段追问
//! 7. MockClarificationChecker 精确控制测试

use async_trait::async_trait;
use forge::extract::ExtractedFile;
use forge::memory::{Memory, TaskStatus};
use forge::orchestrator::Orchestrator;
use forge::testrunner::{CompileError, TestResult};
use forge::traits::{
    ChatClient, ChatResult, ClarificationChecker, ClarificationContext, ClarificationResult,
    FileExtractor, TestRunner,
};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;

// ============================================================================
//  Mock 实现 (复用自 orchestrator_dip.rs 模式)
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
        // 检查是否标记为超时 (用 "[TIMEOUT]" 前缀)
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

/// MockClarificationChecker — 精确控制何时触发追问
struct MockClarificationChecker {
    /// 按顺序返回的检查结果队列
    results: Arc<Mutex<Vec<ClarificationResult>>>,
}

impl MockClarificationChecker {
    fn new(results: Vec<ClarificationResult>) -> Self {
        Self {
            results: Arc::new(Mutex::new(results)),
        }
    }

    /// 总是返回 "不需要追问"
    fn never_clarify() -> Self {
        Self::new(vec![ClarificationResult::no()])
    }
}

#[async_trait]
impl ClarificationChecker for MockClarificationChecker {
    async fn check(&self, _response: &str, _context: &ClarificationContext) -> ClarificationResult {
        let mut queue = self.results.lock().unwrap();
        if queue.is_empty() {
            return ClarificationResult::no();
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

// ============================================================================
//  测试用例
// ============================================================================

/// 测试 1: AI 回复含疑问 → Agent 自动追问 → AI 提供代码 → 任务成功
///
/// 流程:
/// 1. Planning: AI 返回计划 JSON (正常,无需追问)
/// 2. Task attempt 1: AI 回复 "你希望用哪种框架？" (触发追问)
/// 3. Clarification: Agent 发送追问 → AI 返回代码
/// 4. 代码编译测试通过 → 任务成功
#[tokio::test]
async fn test_clarification_on_question_in_response() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        // 1. planning 回复
        plan_json().to_string(),
        // 2. task 回复 — 包含疑问 (触发启发式追问)
        "你希望用哪种框架？请告诉我你的选择。".to_string(),
        // 3. 追问后的回复 — 包含代码
        "好的，我来创建项目。\n```file:src/main.rs\nfn main() { println!(\"hello\"); }\n```"
            .to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10);

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

    // 验证: 第 3 条是追问消息
    assert!(
        sent[2].contains("file:路径") || sent[2].contains("自行选择"),
        "第 3 条消息应是追问: {}",
        sent[2]
    );

    // 验证: 追问历史已记录
    assert_eq!(
        orch.memory.phases[0].tasks[0].clarifications.len(),
        1,
        "应记录 1 次追问"
    );
}

/// 测试 2: AI 回复超时 → Agent 追问要求继续 → AI 完成 → 任务成功
#[tokio::test]
async fn test_clarification_on_timeout() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        // planning 回复
        plan_json().to_string(),
        // task 回复 — 超时 (用 [TIMEOUT] 前缀标记)
        "[TIMEOUT]好的，我来创建```file:src/main.rs\nfn main() {".to_string(),
        // 追问后的完整回复
        "```file:src/main.rs\nfn main() { println!(\"hello\"); }\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10);

    orch.run().await.unwrap();

    // 验证: 任务成功
    assert_eq!(orch.memory.phases[0].tasks[0].status, TaskStatus::Completed);

    // 验证: 发送了 3 条消息 (planning + task + clarification)
    let sent = orch.chat.sent_messages();
    assert_eq!(sent.len(), 3);

    // 验证: 追问消息包含"未完成"
    assert!(
        sent[2].contains("未完成"),
        "超时追问应包含'未完成': {}",
        sent[2]
    );

    // 验证: 追问历史记录
    assert_eq!(orch.memory.phases[0].tasks[0].clarifications.len(), 1);
}

/// 测试 3: 正常回复 → 无需追问 → 正常流程 (不发送额外消息)
#[tokio::test]
async fn test_no_clarification_for_normal_response() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        // planning 回复
        plan_json().to_string(),
        // task 回复 — 正常代码, 无疑问
        "好的，我来创建项目。\n```file:src/main.rs\nfn main() { println!(\"hello\"); }\n```"
            .to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10);

    orch.run().await.unwrap();

    // 验证: 任务成功
    assert_eq!(orch.memory.phases[0].tasks[0].status, TaskStatus::Completed);

    // 验证: 只发送了 2 条消息 (planning + task), 无追问
    let sent = orch.chat.sent_messages();
    assert_eq!(sent.len(), 2, "正常回复不应触发追问");

    // 验证: 无追问历史
    assert_eq!(orch.memory.phases[0].tasks[0].clarifications.len(), 0);
}

/// 测试 4: 达到追问上限 → 不再追问 → 继续处理
///
/// max_clarifications = 0 → 从不追问, 即使 AI 回复含疑问
#[tokio::test]
async fn test_clarification_max_reached() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        // planning 回复
        plan_json().to_string(),
        // attempt 1: AI 回复含疑问, 但 max=0 不追问 → 无代码 → continue
        "你希望用哪种框架？".to_string(),
        // attempt 2: AI 提供代码 → 成功
        "```file:src/main.rs\nfn main() { println!(\"fixed\"); }\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![
        vec![],                                  // attempt 1: 无代码
        vec![ef("src/main.rs", "fn main() {}")], // attempt 2: 有代码
    ]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10);

    // 设置 max_clarifications = 0 (禁止追问)
    orch.memory.max_clarifications = 0;

    orch.run().await.unwrap();

    // 验证: 任务最终成功
    assert_eq!(orch.memory.phases[0].tasks[0].status, TaskStatus::Completed);

    // 验证: 只发送了 3 条消息 (planning + attempt1 + attempt2), 无追问
    let sent = orch.chat.sent_messages();
    assert_eq!(sent.len(), 3, "应发送 3 条消息 (无追问)");

    // 验证: 无追问历史 (max=0 从未追问)
    assert_eq!(
        orch.memory.phases[0].tasks[0].clarifications.len(),
        0,
        "max=0 时不应有任何追问"
    );
}

/// 测试 5: 追问历史记录在 Memory 中, 可持久化
#[tokio::test]
async fn test_clarification_recorded_in_memory() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        plan_json().to_string(),
        "你希望用哪种框架？".to_string(),
        "```file:src/main.rs\nfn main() {}\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10);

    orch.run().await.unwrap();

    // 验证: memory.json 存在且可加载
    let memory_path = format!("{}/.forge/memory.json", ws_dir);
    let loaded = Memory::load(std::path::Path::new(&memory_path)).unwrap();

    // 验证: 追问历史已持久化
    assert_eq!(loaded.phases[0].tasks[0].clarifications.len(), 1);
    assert!(
        !loaded.phases[0].tasks[0].clarifications[0].is_empty(),
        "追问内容不应为空"
    );

    // 验证: 决策记录中有 "自主追问"
    let has_clarification_decision = loaded.decisions.iter().any(|d| d.decision == "自主追问");
    assert!(has_clarification_decision, "决策记录应包含 '自主追问'");

    // 验证: max_clarifications 已持久化
    assert_eq!(loaded.max_clarifications, 2);
}

/// 测试 6: Planning 阶段追问 — AI 在规划时提问, Agent 追问后获得计划
#[tokio::test]
async fn test_clarification_in_planning_phase() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        // planning 回复 — AI 先提问而不是给计划
        "你希望开发什么类型的应用？是 CLI 还是 Web？".to_string(),
        // 追问后的回复 — AI 给出计划
        plan_json().to_string(),
        // task 回复
        "```file:src/main.rs\nfn main() {}\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10);

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

/// 测试 7: 使用 MockClarificationChecker 精确控制追问
#[tokio::test]
async fn test_with_mock_clarification_checker() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        plan_json().to_string(),
        // 这条回复看起来正常,但 MockChecker 会强制触发追问
        "Here is the code:\n```file:src/main.rs\nfn main() {}\n```".to_string(),
        // 追问后的回复
        "```file:src/main.rs\nfn main() { println!(\"clarified\"); }\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    // 使用 MockClarificationChecker — 强制触发一次追问
    let mock_checker = MockClarificationChecker::new(vec![ClarificationResult::yes(
        "请直接输出代码。",
        "Mock 触发",
    )]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10)
        .with_clarification(mock_checker);

    orch.run().await.unwrap();

    // 验证: 任务成功
    assert_eq!(orch.memory.phases[0].tasks[0].status, TaskStatus::Completed);

    // 验证: 发送了 3 条消息 (planning + task + clarification)
    let sent = orch.chat.sent_messages();
    assert_eq!(sent.len(), 3, "Mock 应触发 1 次追问");

    // 验证: 追问消息是 MockChecker 的消息
    assert!(sent[2].contains("请直接输出代码"), "追问应为 Mock 消息");
}

/// 测试 8: MockClarificationChecker 从不追问 — 等价于无追问
#[tokio::test]
async fn test_with_mock_checker_never_clarify() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        plan_json().to_string(),
        "```file:src/main.rs\nfn main() {}\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mock_checker = MockClarificationChecker::never_clarify();

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10)
        .with_clarification(mock_checker);

    orch.run().await.unwrap();

    // 验证: 任务成功
    assert_eq!(orch.memory.phases[0].tasks[0].status, TaskStatus::Completed);

    // 验证: 只发送了 2 条消息 (无追问)
    let sent = orch.chat.sent_messages();
    assert_eq!(sent.len(), 2, "Mock 不追问时不应有额外消息");

    // 验证: 无追问历史
    assert_eq!(orch.memory.phases[0].tasks[0].clarifications.len(), 0);
}

/// 测试 9: 追问后 AI 仍未提供代码 → 继续修复循环
#[tokio::test]
async fn test_clarification_then_fix_loop() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        // planning
        plan_json().to_string(),
        // attempt 1: AI 提问 → 追问 → AI 仍未提供代码
        "你希望用什么框架？".to_string(),
        "我还需要更多信息。".to_string(), // 追问后仍无代码 → continue
        // attempt 2: 修复轮 → AI 提供代码
        "```file:src/main.rs\nfn main() { println!(\"ok\"); }\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    // 注意: attempt 1 追问后无代码 → extractor 返回空 → continue (不消耗 check/test)
    // attempt 2 有代码 → 编译测试成功
    let extractor = MockExtractor::new(vec![
        vec![],                                  // attempt 1: 追问后仍无代码
        vec![ef("src/main.rs", "fn main() {}")], // attempt 2: 有代码
    ]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10);

    orch.run().await.unwrap();

    // 验证: 任务最终成功 (在第 2 次尝试)
    assert_eq!(orch.memory.phases[0].tasks[0].status, TaskStatus::Completed);
    assert_eq!(orch.memory.phases[0].tasks[0].attempts, 2);

    // 验证: 第 1 次尝试有追问, 第 2 次无追问 (代码正常)
    // 消息数: planning(1) + attempt1_prompt(1) + attempt1_clarification(1) + attempt2_fix_prompt(1) = 4
    let sent = orch.chat.sent_messages();
    assert_eq!(sent.len(), 4, "应发送 4 条消息");

    // 验证: 追问历史有 1 条
    assert_eq!(orch.memory.phases[0].tasks[0].clarifications.len(), 1);
}

/// 测试 10: 追问消息包含 file:路径 格式要求
#[tokio::test]
async fn test_clarification_message_contains_format_instruction() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        plan_json().to_string(),
        "你希望用哪种框架？".to_string(),
        "```file:src/main.rs\nfn main() {}\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![make_test_result(true, vec![])])
        .with_test_results(vec![make_test_result(true, vec![])]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10);

    orch.run().await.unwrap();

    let sent = orch.chat.sent_messages();
    let clarification_msg = &sent[2]; // 第 3 条是追问

    // 追问消息应包含文件格式要求
    assert!(
        clarification_msg.contains("file:路径"),
        "追问消息应包含 file:路径 格式要求: {}",
        clarification_msg
    );
}
