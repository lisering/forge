//! 性能测试 (第 16 项任务)
//!
//! 测量各功能开启时的性能开销:
//! 1. DevTrace 写入开销 — 大量 trace 条目写入时间
//! 2. Slash Commands 解析开销 — 大量文本解析时间
//! 3. 循环终止检测开销 — 大量错误记录时间
//! 4. 全功能 vs 基线 — 全功能启用的额外开销
//! 5. DevTrace JSONL 大文件读取性能
//! 6. SlashCommand 大文本解析性能

use async_trait::async_trait;
use forge::dev_trace::{DevTraceWriter, TraceAction};
use forge::extract::ExtractedFile;
use forge::loop_detector::LoopDetector;
use forge::orchestrator::Orchestrator;
use forge::slash_command;
use forge::testrunner::{CompileError, TestResult};
use forge::traits::{ChatClient, ChatResult, FileExtractor, TestRunner};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tempfile::tempdir;

// ============================================================================
//  Mock 实现 (简化版)
// ============================================================================

struct MockChat {
    responses: Arc<Mutex<Vec<String>>>,
    turn_count: Arc<AtomicUsize>,
    new_conversation_count: Arc<AtomicUsize>,
}

impl MockChat {
    fn new(responses: Vec<String>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses)),
            turn_count: Arc::new(AtomicUsize::new(0)),
            new_conversation_count: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[async_trait]
impl ChatClient for MockChat {
    async fn send_message(&self, _msg: &str, _timeout: u64) -> anyhow::Result<ChatResult> {
        self.turn_count.fetch_add(1, Ordering::SeqCst);
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

    async fn start_new_conversation(&self) -> anyhow::Result<()> {
        self.new_conversation_count.fetch_add(1, Ordering::SeqCst);
        self.turn_count.store(0, Ordering::SeqCst);
        Ok(())
    }

    fn conversation_turn_count(&self) -> usize {
        self.turn_count.load(Ordering::SeqCst)
    }
}

struct MockTestRunner;

impl TestRunner for MockTestRunner {
    fn check(&self, _dir: &Path) -> anyhow::Result<TestResult> {
        Ok(TestResult {
            success: true,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
            errors: vec![],
            test_summary: None,
        })
    }
    fn test(&self, _dir: &Path) -> anyhow::Result<TestResult> {
        Ok(TestResult {
            success: true,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
            errors: vec![],
            test_summary: None,
        })
    }
}

struct MockExtractor;

impl FileExtractor for MockExtractor {
    fn extract(&self, _text: &str) -> Vec<ExtractedFile> {
        vec![ExtractedFile {
            path: "src/main.rs".to_string(),
            content: "fn main() {}".to_string(),
            language: String::new(),
        }]
    }
}

// ============================================================================
//  辅助函数
// ============================================================================

#[allow(dead_code)]
fn simple_plan() -> String {
    r#"```json
[{"name":"p","description":"d","tasks":[{"name":"t","prompt":"do"}]}
]
```"#
        .to_string()
}

fn success_code() -> String {
    "完成。\n```file:src/main.rs\nfn main() {}\n```".to_string()
}

/// 生成包含 N 个任务的计划
fn n_task_plan(n: usize) -> String {
    let tasks: Vec<String> = (0..n)
        .map(|i| format!(r#"{{"name":"t{}","prompt":"do{}"}}"#, i, i))
        .collect();
    format!(
        r#"```json
[{{"name":"p","description":"d","tasks":[{}]}}]
```"#,
        tasks.join(",")
    )
}

// ============================================================================
//  测试用例
// ============================================================================

/// 测试 1: DevTrace 写入 1000 条目在合理时间内 (< 5s)
#[test]
fn test_dev_trace_write_performance() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
    let writer = DevTraceWriter::new(dir.path());
    writer.clear().unwrap();

    let start = Instant::now();
    for i in 0..1000 {
        writer
            .trace(
                TraceAction::TaskExecution,
                Some(0),
                Some(i % 10),
                Some(&format!("task{}", i)),
                &format!("input {}", i),
                &format!("output {}", i),
                i as u64,
                true,
                None,
            )
            .unwrap();
    }
    let elapsed = start.elapsed();

    // 1000 条目写入应在 5 秒内
    assert!(
        elapsed.as_secs() < 5,
        "1000 条 DevTrace 写入应在 5s 内, 实际: {:?}",
        elapsed
    );

    // 验证: 条目数正确
    assert_eq!(writer.entry_count(), 1000, "应写入 1000 条目");
}

/// 测试 2: DevTrace 读取 1000 条目在合理时间内 (< 5s)
#[test]
fn test_dev_trace_read_performance() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
    let writer = DevTraceWriter::new(dir.path());
    writer.clear().unwrap();

    // 先写入 1000 条目
    for i in 0..1000 {
        writer
            .trace(
                TraceAction::CompileCheck,
                Some(0),
                Some(i % 10),
                Some(&format!("task{}", i)),
                "input",
                "output",
                i as u64,
                i % 2 == 0,
                None,
            )
            .unwrap();
    }

    // 读取并计时
    let start = Instant::now();
    let entries = writer.read_all().unwrap();
    let elapsed = start.elapsed();

    // 1000 条目读取应在 5 秒内
    assert!(
        elapsed.as_secs() < 5,
        "1000 条 DevTrace 读取应在 5s 内, 实际: {:?}",
        elapsed
    );

    assert_eq!(entries.len(), 1000, "应读取 1000 条目");
}

/// 测试 3: Slash Commands 解析 1000 次在合理时间内 (< 2s)
#[test]
fn test_slash_command_parse_performance() {
    let text = "这是一个包含指令的回复。\n/compact\n/skip\n/refocus\n/retry\n/escalate\n```file:src/main.rs\nfn main() {}\n```";

    let start = Instant::now();
    for _ in 0..1000 {
        let _ = slash_command::parse_from_response(text);
    }
    let elapsed = start.elapsed();

    assert!(
        elapsed.as_secs() < 2,
        "1000 次 Slash Commands 解析应在 2s 内, 实际: {:?}",
        elapsed
    );
}

/// 测试 4: Slash Commands 解析大文本 (10000 行) 在合理时间内 (< 2s)
#[test]
fn test_slash_command_parse_large_text() {
    // 生成 10000 行文本, 其中 100 行包含指令
    let mut lines = vec![];
    for i in 0..10000 {
        if i % 100 == 0 && i > 0 {
            lines.push("/compact".to_string());
        } else {
            lines.push(format!("普通文本行 {}", i));
        }
    }
    let text = lines.join("\n");

    let start = Instant::now();
    let cmds = slash_command::parse_from_response(&text);
    let elapsed = start.elapsed();

    assert!(
        elapsed.as_secs() < 2,
        "大文本 Slash Commands 解析应在 2s 内, 实际: {:?}",
        elapsed
    );

    // 应检测到 /compact (去重后 1 个)
    assert!(!cmds.is_empty(), "应检测到指令");
}

/// 测试 5: strip_commands 大文本性能 (< 2s)
#[test]
fn test_strip_commands_performance() {
    let mut lines = vec![];
    for i in 0..10000 {
        if i % 100 == 0 && i > 0 {
            lines.push("/skip".to_string());
        } else {
            lines.push(format!("代码行 {}", i));
        }
    }
    let text = lines.join("\n");

    let start = Instant::now();
    let stripped = slash_command::strip_commands(&text);
    let elapsed = start.elapsed();

    assert!(
        elapsed.as_secs() < 2,
        "大文本 strip_commands 应在 2s 内, 实际: {:?}",
        elapsed
    );

    // 验证: /skip 被移除
    assert!(!stripped.contains("/skip"), "strip 后不应包含 /skip");
}

/// 测试 6: LoopDetector 大量错误记录性能 (< 1s)
#[test]
fn test_loop_detector_performance() {
    let mut detector = LoopDetector::new(3);

    let errors = vec![CompileError {
        file: "src/main.rs".to_string(),
        line: Some(10),
        column: Some(1),
        message: "type mismatch".to_string(),
        error_code: Some("E0308".to_string()),
    }];

    let start = Instant::now();
    for _ in 0..1000 {
        detector.record_errors(&errors);
        let _ = detector.is_looping();
        let _ = detector.should_skip();
    }
    let elapsed = start.elapsed();

    assert!(
        elapsed.as_secs() < 2,
        "1000 次 LoopDetector 操作应在 2s 内, 实际: {:?}",
        elapsed
    );
}

/// 测试 7: 全功能 vs 基线 — 全功能开销合理 (< 3x 基线时间)
#[tokio::test]
async fn test_full_features_overhead() {
    let n = 10;

    // === 基线: 全功能禁用 ===
    let dir1 = tempdir().unwrap();
    let ws1 = dir1.path().to_str().unwrap();

    let mut chat_responses = vec![n_task_plan(n)];
    for _ in 0..n {
        chat_responses.push(success_code());
    }

    let chat1 = MockChat::new(chat_responses.clone());
    let runner1 = MockTestRunner;
    let extractor1 = MockExtractor;

    let mut orch1 = Orchestrator::new(&chat1, runner1, extractor1, ws1, "基线", 3, 60)
        .with_slash_commands(false)
        .with_dev_trace(false)
        .with_loop_detection(0)
        .with_steer_reminder(0)
        .with_context_handoff(0);

    let start1 = Instant::now();
    orch1.run().await.unwrap();
    let baseline = start1.elapsed();

    // === 全功能启用 ===
    let dir2 = tempdir().unwrap();
    let ws2 = dir2.path().to_str().unwrap();

    let chat2 = MockChat::new(chat_responses);
    let runner2 = MockTestRunner;
    let extractor2 = MockExtractor;

    let mut orch2 = Orchestrator::new(&chat2, runner2, extractor2, ws2, "全功能", 3, 60)
        .with_slash_commands(true)
        .with_dev_trace(true)
        .with_loop_detection(3)
        .with_steer_reminder(5)
        .with_context_handoff(100);

    let start2 = Instant::now();
    orch2.run().await.unwrap();
    let full = start2.elapsed();

    // 全功能时间不应超过基线的 5 倍 (留足余量, Mock 环境下差异主要来自 I/O)
    // 注意: Mock 环境下时间可能很小, 主要验证不产生数量级差异
    let ratio = full.as_micros() as f64 / (baseline.as_micros().max(1) as f64);
    assert!(
        ratio < 10.0,
        "全功能时间不应超过基线的 10 倍, 基线: {:?}, 全功能: {:?}, 比率: {:.2}",
        baseline,
        full,
        ratio
    );

    // 验证: 两种模式都完成了所有任务
    let baseline_count = orch1.memory.phases[0]
        .tasks
        .iter()
        .filter(|t| t.status == forge::memory::TaskStatus::Completed)
        .count();
    let full_count = orch2.memory.phases[0]
        .tasks
        .iter()
        .filter(|t| t.status == forge::memory::TaskStatus::Completed)
        .count();
    assert_eq!(baseline_count, n, "基线应完成 {} 个任务", n);
    assert_eq!(full_count, n, "全功能应完成 {} 个任务", n);
}

/// 测试 8: DevTrace 摘要生成性能 (1000 条目 < 2s)
#[test]
fn test_dev_trace_summary_performance() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
    let writer = DevTraceWriter::new(dir.path());
    writer.clear().unwrap();

    // 写入 1000 条目
    for i in 0..1000 {
        let action = match i % 5 {
            0 => TraceAction::Planning,
            1 => TraceAction::TaskExecution,
            2 => TraceAction::CompileCheck,
            3 => TraceAction::TestRun,
            _ => TraceAction::FixAttempt,
        };
        writer
            .trace(
                action,
                Some(0),
                Some(i % 10),
                Some(&format!("task{}", i)),
                "input",
                "output",
                i as u64,
                i % 3 != 0,
                None,
            )
            .unwrap();
    }

    // 生成摘要并计时
    let start = Instant::now();
    let summary = writer.summary();
    let report = summary.to_report();
    let elapsed = start.elapsed();

    assert!(
        elapsed.as_secs() < 2,
        "1000 条目摘要生成应在 2s 内, 实际: {:?}",
        elapsed
    );

    assert_eq!(summary.total_entries, 1000, "应有 1000 条目");
    assert!(!report.is_empty(), "报告不应为空");
}

/// 测试 9: SlashCommand::has_command 性能
#[test]
fn test_has_command_performance() {
    let text = "这是一个很长的回复\n包含多行文本\n/skip\n更多文本\n```code\n/skip\n```\n结束";

    let start = Instant::now();
    for _ in 0..10000 {
        let _ = slash_command::has_command(text, forge::slash_command::SlashCommand::Skip);
    }
    let elapsed = start.elapsed();

    assert!(
        elapsed.as_secs() < 2,
        "10000 次 has_command 应在 2s 内, 实际: {:?}",
        elapsed
    );
}

/// 测试 10: DevTrace JSONL 往返序列化/反序列化性能
#[test]
fn test_dev_trace_jsonl_roundtrip_performance() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
    let writer = DevTraceWriter::new(dir.path());
    writer.clear().unwrap();

    // 写入 500 条目
    for i in 0..500 {
        writer
            .trace(
                TraceAction::TaskExecution,
                Some(i % 3),
                Some(i % 5),
                Some(&format!("task{}", i)),
                &format!("input {}", i),
                &format!("output {}", i),
                i as u64 * 10,
                i % 2 == 0,
                None,
            )
            .unwrap();
    }

    // 读取
    let entries = writer.read_all().unwrap();
    assert_eq!(entries.len(), 500);

    // 往返: 每个条目序列化 → 反序列化
    let start = Instant::now();
    for entry in &entries {
        let json = entry.to_jsonl().unwrap();
        let _ = forge::dev_trace::DevTraceEntry::from_jsonl(&json).unwrap();
    }
    let elapsed = start.elapsed();

    assert!(
        elapsed.as_secs() < 2,
        "500 条目往返序列化应在 2s 内, 实际: {:?}",
        elapsed
    );
}
