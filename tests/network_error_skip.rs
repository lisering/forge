//! 网络错误跳过集成测试 — 第 38 项任务
//!
//! 验证 orchestrator 层面的网络错误处理:
//! 1. 网络错误重试: check/test 遇到网络错误时自动重试 (3 次, 30s 间隔)
//! 2. 网络错误跳过: 重试耗尽后跳过 AI 修复 (不消耗修复轮次)
//! 3. 网络错误恢复: 跳过后重新执行任务, 网络恢复后成功
//!
//! 使用 MockTestRunner 预编程网络错误结果, MockChat 预编程代码回复。
//! 使用 #[tokio::test(start_paused = true)] 自动推进时间 (30s sleep 立即完成)。

use async_trait::async_trait;
use forge::extract::ExtractedFile;
use forge::interaction::AutoApprove;
use forge::memory::TaskStatus;
use forge::orchestrator::Orchestrator;
use forge::testrunner::{CompileError, TestResult};
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

    fn sent_count(&self) -> usize {
        self.sent_messages.lock().unwrap().len()
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
            return Ok(make_success_result());
        }
        Ok(queue.remove(0))
    }

    fn test(&self, _dir: &Path) -> anyhow::Result<TestResult> {
        let mut queue = self.test_results.lock().unwrap();
        if queue.is_empty() {
            return Ok(make_success_result());
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

fn make_network_error_result() -> TestResult {
    TestResult {
        success: false,
        stdout: String::new(),
        stderr: r#"warning: spurious network error (2 tries remaining): [7] Couldn't connect to server (Failed to connect to 127.0.0.1 port 7890 after 0 ms: Couldn't connect to server)
error: failed to get `anyhow` as a dependency of package `calculator v0.1.0`
Caused by:
  unable to update registry `crates-io`
"#
        .to_string(),
        exit_code: 101,
        errors: vec![],
        test_summary: None,
    }
}

fn make_compile_error_result() -> TestResult {
    TestResult {
        success: false,
        stdout: String::new(),
        stderr: r#"error[E0308]: mismatched types
  --> src/main.rs:10:5
"#
        .to_string(),
        exit_code: 101,
        errors: vec![CompileError {
            file: "src/main.rs".to_string(),
            line: Some(10),
            column: Some(5),
            message: "mismatched types".to_string(),
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

const CODE_RESPONSE: &str = "```file:src/main.rs\nfn main() {}\n```";

// ============================================================================
//  测试用例
// ============================================================================

/// 测试 1: 网络错误重试成功 — check 遇到网络错误后重试成功, 不消耗 AI 修复轮次
///
/// 流程:
/// 1. AI 返回计划 JSON → 计划被批准
/// 2. AI 返回代码 → 提取文件 → 写入工作区
/// 3. cargo check → 网络错误 → 重试 (30s, 自动推进) → 成功
/// 4. cargo test → 成功
/// 5. 任务完成
///
/// 验证: AI 只被调用 2 次 (计划 + 代码), 没有修复轮次
#[tokio::test(start_paused = true)]
async fn test_network_error_retried_successfully() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![PLAN_JSON.to_string(), CODE_RESPONSE.to_string()]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            make_network_error_result(), // 第一次 check: 网络错误
            make_success_result(),       // 重试后: 成功
        ])
        .with_test_results(vec![make_success_result()]);

    let extractor = MockExtractor::new(vec![vec![ef("src/main.rs", "fn main() {}")]]);

    let mut orch = Orchestrator::new(
        &chat,
        runner,
        extractor,
        ws_dir,
        "创建一个 CLI 工具",
        3,  // max_rounds
        60, // timeout
    )
    .with_interaction(Box::new(AutoApprove));

    orch.run().await.unwrap();

    // 任务应完成
    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(
        task.status,
        TaskStatus::Completed,
        "任务应完成 (网络错误重试后成功)"
    );

    // AI 只被调用 2 次 (计划 + 代码), 没有修复轮次
    assert_eq!(
        chat.sent_count(),
        2,
        "AI 应只被调用 2 次 (计划 + 代码), 不应有修复轮次"
    );

    // 检查调用次数: 初始 1 次 + 重试 1 次 = 2 次
    // 注意: runner 的 check_call_count 无法从这里访问 (orchestrator 拥有了 runner)
    // 但通过 AI 调用次数可以间接验证
}

/// 测试 2: 网络错误跳过 — 重试耗尽后跳过 AI 修复, 重新执行任务后成功
///
/// 流程:
/// 1. AI 返回计划 JSON → 计划被批准
/// 2. AI 返回代码 → 提取文件 → 写入工作区
/// 3. cargo check → 网络错误 → 重试 3 次都失败 → 跳过 AI 修复 (network_error_skips=1)
/// 4. 重新执行任务: AI 返回代码 → 提取文件 → 写入工作区
/// 5. cargo check → 成功
/// 6. cargo test → 成功
/// 7. 任务完成
///
/// 验证: AI 被调用 3 次 (计划 + 代码 + 代码), 没有修复轮次 (第 3 次是重新执行, 不是修复)
#[tokio::test(start_paused = true)]
async fn test_network_error_skip_no_ai_repair() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        PLAN_JSON.to_string(),
        CODE_RESPONSE.to_string(),
        CODE_RESPONSE.to_string(), // 重新执行任务时 AI 返回代码
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            make_network_error_result(), // 初始 check: 网络错误
            make_network_error_result(), // 重试 1: 网络错误
            make_network_error_result(), // 重试 2: 网络错误
            make_network_error_result(), // 重试 3: 网络错误 (重试耗尽)
            make_success_result(),       // 跳过后重新执行: 成功
        ])
        .with_test_results(vec![make_success_result()]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/main.rs", "fn main() {}")], // 重新执行时提取的文件
    ]);

    let mut orch = Orchestrator::new(
        &chat,
        runner,
        extractor,
        ws_dir,
        "创建一个 CLI 工具",
        3,  // max_rounds
        60, // timeout
    )
    .with_interaction(Box::new(AutoApprove));

    orch.run().await.unwrap();

    // 任务应完成
    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(
        task.status,
        TaskStatus::Completed,
        "任务应完成 (网络错误跳过后重新执行成功)"
    );

    // AI 被调用 3 次 (计划 + 代码 + 代码重新执行)
    // 第 3 次是重新执行 (attempt 仍为 1), 不是修复轮次
    assert_eq!(
        chat.sent_count(),
        3,
        "AI 应被调用 3 次 (计划 + 代码 + 代码重新执行), 不应有修复轮次"
    );

    // 验证没有修复相关的决策日志
    let has_fix_decision = orch.memory.decisions.iter().any(|d| {
        d.decision.contains("编译失败,进入修复") || d.decision.contains("测试失败,进入修复")
    });
    assert!(
        !has_fix_decision,
        "不应有编译/测试失败进入修复的决策 (网络错误应跳过 AI 修复)"
    );
}

/// 测试 3: 网络错误跳过次数耗尽 — 超过 MAX_NETWORK_ERROR_SKIPS 后进入正常 AI 修复
///
/// 流程:
/// 1. AI 返回计划 JSON → 计划被批准
/// 2. AI 返回代码 → 提取文件 → 写入工作区
/// 3. cargo check → 网络错误 → 重试 3 次都失败 → 跳过 (skip 1)
/// 4. 重新执行: AI 返回代码 → check → 网络错误 → 重试 3 次都失败 → 跳过 (skip 2)
/// 5. 重复直到 skip 5 次耗尽
/// 6. 进入正常 AI 修复: AI 返回修复代码 → check → 网络错误 → 修复失败
/// 7. 最终任务失败
///
/// 注意: 此测试需要大量 MockTestRunner 结果 (每次跳过需要 4 个网络错误结果)
/// 5 次跳过 × 4 结果 = 20 个网络错误结果 + 1 个修复尝试的结果
#[tokio::test(start_paused = true)]
async fn test_network_error_max_skips_then_ai_repair() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    // 每次跳过需要 4 个网络错误结果 (初始 + 3 次重试)
    // 5 次跳过后进入 AI 修复, 修复也遇到网络错误
    let mut check_results: Vec<TestResult> = vec![];
    for _ in 0..5 {
        // 5 次跳过, 每次 4 个网络错误
        for _ in 0..4 {
            check_results.push(make_network_error_result());
        }
    }
    // 第 6 次网络错误 (超过 MAX_NETWORK_ERROR_SKIPS=5), 进入正常 AI 修复
    // AI 修复时也遇到网络错误 (4 个网络错误)
    for _ in 0..4 {
        check_results.push(make_network_error_result());
    }
    // 最后一次 check 也失败 (网络错误)
    check_results.push(make_network_error_result());

    // AI 需要返回: 计划 + 代码 (6 次: 5 次跳过 + 1 次修复) + 代码 (修复)
    let mut chat_responses: Vec<String> = vec![PLAN_JSON.to_string()];
    for _ in 0..6 {
        chat_responses.push(CODE_RESPONSE.to_string());
    }

    let chat = MockChat::new(chat_responses);

    let runner = MockTestRunner::new()
        .with_check_results(check_results)
        .with_test_results(vec![]); // 不会到 test 阶段

    // MockExtractor 需要返回 6 组文件
    let mut file_sets: Vec<Vec<ExtractedFile>> = vec![];
    for _ in 0..6 {
        file_sets.push(vec![ef("src/main.rs", "fn main() {}")]);
    }
    let extractor = MockExtractor::new(file_sets);

    let mut orch = Orchestrator::new(
        &chat,
        runner,
        extractor,
        ws_dir,
        "创建一个 CLI 工具",
        3,  // max_rounds (3 次修复尝试)
        60, // timeout
    )
    .with_interaction(Box::new(AutoApprove));

    orch.run().await.unwrap();

    // 任务应失败 (网络错误持续, 修复轮次耗尽)
    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(
        task.status,
        TaskStatus::Failed,
        "任务应失败 (网络错误持续, 修复轮次耗尽)"
    );

    // 验证有修复相关的决策日志 (超过 MAX_NETWORK_ERROR_SKIPS 后进入 AI 修复)
    let has_fix_decision = orch
        .memory
        .decisions
        .iter()
        .any(|d| d.decision.contains("编译失败,进入修复"));
    assert!(
        has_fix_decision,
        "超过 MAX_NETWORK_ERROR_SKIPS 后应进入 AI 修复"
    );
}

/// 测试 4: 编译错误 (非网络错误) 正常进入 AI 修复
///
/// 验证: 非网络错误的编译失败不受网络错误跳过逻辑影响, 正常进入 AI 修复
#[tokio::test(start_paused = true)]
async fn test_compile_error_normal_ai_repair() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        PLAN_JSON.to_string(),
        CODE_RESPONSE.to_string(),
        CODE_RESPONSE.to_string(), // 修复代码
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            make_compile_error_result(), // 第一次 check: 编译错误 (非网络错误)
            make_success_result(),       // 修复后: 成功
        ])
        .with_test_results(vec![make_success_result()]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/main.rs", "fn main() { fixed }")],
    ]);

    let mut orch = Orchestrator::new(
        &chat,
        runner,
        extractor,
        ws_dir,
        "创建一个 CLI 工具",
        3,  // max_rounds
        60, // timeout
    )
    .with_interaction(Box::new(AutoApprove));

    orch.run().await.unwrap();

    // 任务应完成 (修复后成功)
    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(
        task.status,
        TaskStatus::Completed,
        "任务应完成 (编译错误修复后成功)"
    );

    // AI 被调用 3 次 (计划 + 代码 + 修复代码)
    assert_eq!(
        chat.sent_count(),
        3,
        "AI 应被调用 3 次 (计划 + 代码 + 修复代码)"
    );

    // 验证有修复相关的决策日志
    let has_fix_decision = orch
        .memory
        .decisions
        .iter()
        .any(|d| d.decision.contains("编译失败,进入修复"));
    assert!(
        has_fix_decision,
        "编译错误应进入 AI 修复 (非网络错误不受跳过逻辑影响)"
    );
}

/// 测试 5: 测试失败为网络错误时跳过 AI 修复
///
/// 验证: cargo test 遇到网络错误时也跳过 AI 修复
#[tokio::test(start_paused = true)]
async fn test_network_error_in_test_skip_ai_repair() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        PLAN_JSON.to_string(),
        CODE_RESPONSE.to_string(),
        CODE_RESPONSE.to_string(), // 跳过后重新执行
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            make_success_result(), // 第一次 check: 成功
            make_success_result(), // 跳过后 check: 成功
        ])
        .with_test_results(vec![
            make_network_error_result(), // 第一次 test: 网络错误 → 重试 3 次都失败 → 跳过
            make_network_error_result(), // 重试 1
            make_network_error_result(), // 重试 2
            make_network_error_result(), // 重试 3
            make_success_result(),       // 跳过后 test: 成功
        ]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/main.rs", "fn main() {}")],
        vec![ef("src/main.rs", "fn main() {}")],
    ]);

    let mut orch = Orchestrator::new(
        &chat,
        runner,
        extractor,
        ws_dir,
        "创建一个 CLI 工具",
        3,  // max_rounds
        60, // timeout
    )
    .with_interaction(Box::new(AutoApprove));

    orch.run().await.unwrap();

    // 任务应完成 (测试网络错误跳过后重新执行成功)
    let task = &orch.memory.phases[0].tasks[0];
    assert_eq!(
        task.status,
        TaskStatus::Completed,
        "任务应完成 (测试网络错误跳过后重新执行成功)"
    );

    // AI 被调用 3 次 (计划 + 代码 + 代码重新执行)
    assert_eq!(
        chat.sent_count(),
        3,
        "AI 应被调用 3 次 (计划 + 代码 + 代码重新执行), 不应有修复轮次"
    );

    // 验证没有修复相关的决策日志
    let has_fix_decision = orch
        .memory
        .decisions
        .iter()
        .any(|d| d.decision.contains("测试失败,进入修复"));
    assert!(
        !has_fix_decision,
        "不应有测试失败进入修复的决策 (网络错误应跳过 AI 修复)"
    );
}
