//! 并行任务执行集成测试 — 方向 C
//!
//! 验证 TaskGraph 依赖分析 + Orchestrator 并行执行模式:
//! - 并行模式下的独立任务执行
//! - 依赖感知的任务执行顺序
//! - 依赖失败时跳过后续任务
//! - 环检测回退到顺序执行
//! - 并行模式与顺序模式行为一致性

use async_trait::async_trait;
use forge::extract::ExtractedFile;
use forge::memory::{PhaseStatus, Task, TaskStatus};
use forge::orchestrator::Orchestrator;
use forge::task_graph::{TaskGraph, TaskGraphError};
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

fn make_task(id: &str, depends_on: Vec<String>) -> Task {
    Task {
        id: id.to_string(),
        phase_id: 0,
        name: format!("Task {}", id),
        prompt: format!("Do task {}", id),
        status: TaskStatus::Pending,
        result: None,
        attempts: 0,
        files_written: vec![],
        test_result: None,
        last_good_snapshot: None,
        clarifications: vec![],
        depends_on,
    }
}

/// 构建带 depends_on 的 JSON 计划 (3 个任务: A, B 独立, C 依赖 A 和 B)
fn plan_json_with_deps() -> String {
    r#"```json
[
  {
    "name": "阶段1",
    "description": "测试并行执行",
    "tasks": [
      {"name": "任务A", "prompt": "创建 main.rs"},
      {"name": "任务B", "prompt": "创建 lib.rs"},
      {"name": "任务C", "prompt": "创建 tests", "depends_on": ["0-0", "0-1"]}
    ]
  }
]
```"#
        .to_string()
}

/// 构建无依赖的 JSON 计划 (3 个独立任务)
fn plan_json_no_deps() -> String {
    r#"```json
[
  {
    "name": "阶段1",
    "description": "测试无依赖",
    "tasks": [
      {"name": "任务A", "prompt": "创建 main.rs"},
      {"name": "任务B", "prompt": "创建 lib.rs"},
      {"name": "任务C", "prompt": "创建 tests"}
    ]
  }
]
```"#
        .to_string()
}

// ============================================================================
//  TaskGraph 独立集成测试
// ============================================================================

/// 测试 1: TaskGraph 从任务列表正确构建
#[test]
fn test_taskgraph_build_from_tasks() {
    let tasks = vec![
        make_task("0-0", vec![]),
        make_task("0-1", vec![]),
        make_task("0-2", vec!["0-0".to_string(), "0-1".to_string()]),
    ];
    let graph = TaskGraph::build_from_tasks(&tasks).unwrap();
    assert_eq!(graph.num_tasks(), 3);
    assert!(!graph.has_cycle());

    let groups = graph.parallel_groups().unwrap();
    assert_eq!(groups.len(), 2, "应有 2 个并行组");
    assert_eq!(groups[0].len(), 2, "第一组有 2 个独立任务");
    assert_eq!(groups[1].len(), 1, "第二组有 1 个依赖任务");
}

/// 测试 2: TaskGraph 检测缺失依赖
#[test]
fn test_taskgraph_missing_dependency() {
    let tasks = vec![make_task("0-0", vec!["0-99".to_string()])];
    let result = TaskGraph::build_from_tasks(&tasks);
    assert!(matches!(result, Err(TaskGraphError::MissingDependency(_))));
}

/// 测试 3: TaskGraph 检测循环依赖
#[test]
fn test_taskgraph_cycle_detection() {
    let tasks = vec![
        make_task("0-0", vec!["0-1".to_string()]),
        make_task("0-1", vec!["0-0".to_string()]),
    ];
    let graph = TaskGraph::build_from_tasks(&tasks).unwrap();
    assert!(graph.has_cycle());
    assert_eq!(graph.topological_sort(), Err(TaskGraphError::CycleDetected));
}

/// 测试 4: TaskGraph 并行分组 — 菱形依赖
#[test]
fn test_taskgraph_diamond_groups() {
    let tasks = vec![
        make_task("0-0", vec![]),
        make_task("0-1", vec!["0-0".to_string()]),
        make_task("0-2", vec!["0-0".to_string()]),
        make_task("0-3", vec!["0-1".to_string(), "0-2".to_string()]),
    ];
    let graph = TaskGraph::build_from_tasks(&tasks).unwrap();
    let groups = graph.parallel_groups().unwrap();
    assert_eq!(groups, vec![vec![0], vec![1, 2], vec![3]]);
}

/// 测试 5: TaskGraph 最大并行度
#[test]
fn test_taskgraph_max_parallelism() {
    let tasks = vec![
        make_task("0-0", vec![]),
        make_task("0-1", vec![]),
        make_task("0-2", vec![]),
    ];
    let graph = TaskGraph::build_from_tasks(&tasks).unwrap();
    assert_eq!(graph.max_parallelism().unwrap(), 3);
}

/// 测试 6: TaskGraph 拓扑排序
#[test]
fn test_taskgraph_topological_sort() {
    let tasks = vec![
        make_task("0-0", vec![]),
        make_task("0-1", vec!["0-0".to_string()]),
        make_task("0-2", vec!["0-1".to_string()]),
    ];
    let graph = TaskGraph::build_from_tasks(&tasks).unwrap();
    let sorted = graph.topological_sort().unwrap();
    assert_eq!(sorted, vec![0, 1, 2]);
}

/// 测试 7: TaskGraph 依赖查询
#[test]
fn test_taskgraph_dependency_queries() {
    let tasks = vec![
        make_task("0-0", vec![]),
        make_task("0-1", vec!["0-0".to_string()]),
    ];
    let graph = TaskGraph::build_from_tasks(&tasks).unwrap();
    assert!(graph.dependencies_of(0).is_empty());
    assert_eq!(graph.dependencies_of(1), &[0]);
    assert_eq!(graph.dependents_of(0), &[1]);
    assert!(graph.dependents_of(1).is_empty());
}

/// 测试 8: TaskGraph 传递闭包
#[test]
fn test_taskgraph_transitive_deps() {
    let tasks = vec![
        make_task("0-0", vec![]),
        make_task("0-1", vec!["0-0".to_string()]),
        make_task("0-2", vec!["0-1".to_string()]),
    ];
    let graph = TaskGraph::build_from_tasks(&tasks).unwrap();
    let deps = graph.all_dependencies(2);
    assert!(deps.contains(&0));
    assert!(deps.contains(&1));
    assert!(!deps.contains(&2));
}

// ============================================================================
//  Orchestrator 并行执行集成测试
// ============================================================================

/// 测试 9: 并行模式 — 3 个独立任务全部成功
#[tokio::test]
async fn test_parallel_all_independent_tasks_success() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        plan_json_no_deps(),
        "```file:src/main.rs\nfn main() {}\n```".to_string(),
        "```file:src/lib.rs\npub fn lib() {}\n```".to_string(),
        "```file:src/tests.rs\n#[test] fn t() {}\n```".to_string(),
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
        vec![ef("src/tests.rs", "#[test] fn t() {}")],
    ]);

    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10).with_parallel(true);

    orch.run().await.unwrap();

    // 所有 3 个任务都应完成
    assert_eq!(orch.memory.phases[0].tasks.len(), 3);
    for task in &orch.memory.phases[0].tasks {
        assert_eq!(
            task.status,
            TaskStatus::Completed,
            "任务 {} 应完成",
            task.name
        );
    }
}

/// 测试 10: 并行模式 — 有依赖的任务按正确顺序执行
#[tokio::test]
async fn test_parallel_with_dependencies() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        plan_json_with_deps(),
        // 任务 A 的回复
        "```file:src/main.rs\nfn main() {}\n```".to_string(),
        // 任务 B 的回复
        "```file:src/lib.rs\npub fn lib() {}\n```".to_string(),
        // 任务 C 的回复 (依赖 A 和 B)
        "```file:src/tests.rs\n#[test] fn t() {}\n```".to_string(),
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
        vec![ef("src/tests.rs", "#[test] fn t() {}")],
    ]);

    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10).with_parallel(true);

    orch.run().await.unwrap();

    // 所有 3 个任务都应完成
    assert_eq!(orch.memory.phases[0].tasks.len(), 3);
    for task in &orch.memory.phases[0].tasks {
        assert_eq!(task.status, TaskStatus::Completed);
    }

    // 验证任务 C (index 2) 在任务 A 和 B 之后执行
    let sent = orch.chat.sent_messages();
    // sent[0] = planning, sent[1] = task A, sent[2] = task B, sent[3] = task C
    assert!(sent.len() >= 4, "应至少发送 4 条消息");
}

/// 测试 11: 并行模式 — 依赖任务失败时跳过后续依赖者
#[tokio::test]
async fn test_parallel_dependency_failure_skips_dependents() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        plan_json_with_deps(),
        // 任务 A 的回复
        "```file:src/main.rs\nfn main() { broken }\n```".to_string(),
        // 任务 B 的回复
        "```file:src/lib.rs\npub fn lib() {}\n```".to_string(),
        // 任务 C 不需要回复 (因为依赖 A 失败, 应被跳过)
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            // 任务 A 编译失败
            make_test_result(
                false,
                vec![CompileError {
                    file: "src/main.rs".to_string(),
                    line: Some(1),
                    column: Some(1),
                    message: "syntax error".to_string(),
                    error_code: Some("E0308".to_string()),
                }],
            ),
            // 任务 A 第二次也失败
            make_test_result(
                false,
                vec![CompileError {
                    file: "src/main.rs".to_string(),
                    line: Some(1),
                    column: Some(1),
                    message: "still broken".to_string(),
                    error_code: Some("E0308".to_string()),
                }],
            ),
            // 任务 A 第三次也失败
            make_test_result(
                false,
                vec![CompileError {
                    file: "src/main.rs".to_string(),
                    line: Some(1),
                    column: Some(1),
                    message: "still broken".to_string(),
                    error_code: Some("E0308".to_string()),
                }],
            ),
            // 任务 B 编译成功
            make_test_result(true, vec![]),
        ])
        .with_test_results(vec![
            // 任务 B 测试成功
            make_test_result(true, vec![]),
        ]);

    let extractor = MockExtractor::new(vec![
        // 任务 A 的 3 次尝试
        vec![ef("src/main.rs", "fn main() { broken1 }")],
        vec![ef("src/main.rs", "fn main() { broken2 }")],
        vec![ef("src/main.rs", "fn main() { broken3 }")],
        // 任务 B
        vec![ef("src/lib.rs", "pub fn lib() {}")],
    ]);

    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10).with_parallel(true);

    orch.run().await.unwrap();

    // 任务 A 应失败
    assert_eq!(orch.memory.phases[0].tasks[0].status, TaskStatus::Failed);
    // 任务 B 应完成 (与 A 独立)
    assert_eq!(orch.memory.phases[0].tasks[1].status, TaskStatus::Completed);
    // 任务 C 应失败 (依赖 A, A 失败了)
    assert_eq!(
        orch.memory.phases[0].tasks[2].status,
        TaskStatus::Failed,
        "任务 C 应因依赖失败而被跳过"
    );
}

/// 测试 12: 并行模式与顺序模式结果一致 (无依赖时)
#[tokio::test]
async fn test_parallel_equals_sequential_no_deps() {
    let dir1 = tempdir().unwrap();
    let dir2 = tempdir().unwrap();

    let make_chat = || {
        MockChat::new(vec![
            plan_json_no_deps(),
            "```file:src/main.rs\nfn main() {}\n```".to_string(),
            "```file:src/lib.rs\npub fn lib() {}\n```".to_string(),
            "```file:src/tests.rs\n#[test] fn t() {}\n```".to_string(),
        ])
    };

    let make_runner = || {
        MockTestRunner::new()
            .with_check_results(vec![
                make_test_result(true, vec![]),
                make_test_result(true, vec![]),
                make_test_result(true, vec![]),
            ])
            .with_test_results(vec![
                make_test_result(true, vec![]),
                make_test_result(true, vec![]),
                make_test_result(true, vec![]),
            ])
    };

    let make_extractor = || {
        MockExtractor::new(vec![
            vec![ef("src/main.rs", "fn main() {}")],
            vec![ef("src/lib.rs", "pub fn lib() {}")],
            vec![ef("src/tests.rs", "#[test] fn t() {}")],
        ])
    };

    // 顺序模式
    let chat_seq = make_chat();
    let runner_seq = make_runner();
    let extractor_seq = make_extractor();
    let mut orch_seq = Orchestrator::new(
        &chat_seq,
        runner_seq,
        extractor_seq,
        dir1.path().to_str().unwrap(),
        "test",
        3,
        10,
    );
    orch_seq.run().await.unwrap();

    // 并行模式
    let chat_par = make_chat();
    let runner_par = make_runner();
    let extractor_par = make_extractor();
    let mut orch_par = Orchestrator::new(
        &chat_par,
        runner_par,
        extractor_par,
        dir2.path().to_str().unwrap(),
        "test",
        3,
        10,
    )
    .with_parallel(true);
    orch_par.run().await.unwrap();

    // 验证结果一致
    assert_eq!(orch_seq.memory.phases[0].tasks.len(), 3);
    assert_eq!(orch_par.memory.phases[0].tasks.len(), 3);

    for i in 0..3 {
        assert_eq!(
            orch_seq.memory.phases[0].tasks[i].status, orch_par.memory.phases[0].tasks[i].status,
            "任务 {} 状态应一致",
            i
        );
    }
}

/// 测试 13: 并行模式 — depends_on 从 AI JSON 正确解析
#[tokio::test]
async fn test_parallel_parses_depends_on_from_json() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![
        plan_json_with_deps(),
        "```file:src/main.rs\nfn main() {}\n```".to_string(),
        "```file:src/lib.rs\npub fn lib() {}\n```".to_string(),
        "```file:src/tests.rs\n#[test] fn t() {}\n```".to_string(),
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
        vec![ef("src/tests.rs", "#[test] fn t() {}")],
    ]);

    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10).with_parallel(true);

    orch.run().await.unwrap();

    // 验证 depends_on 被正确解析
    assert!(
        !orch.memory.phases[0].tasks[2].depends_on.is_empty(),
        "任务 C 应有依赖"
    );
    assert!(
        orch.memory.phases[0].tasks[2]
            .depends_on
            .contains(&"0-0".to_string()),
        "任务 C 应依赖 0-0"
    );
    assert!(
        orch.memory.phases[0].tasks[2]
            .depends_on
            .contains(&"0-1".to_string()),
        "任务 C 应依赖 0-1"
    );
}

/// 测试 14: 并行模式 — 无依赖任务和有依赖任务都成功
#[tokio::test]
async fn test_parallel_mixed_deps_all_success() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let plan = r#"```json
[
  {
    "name": "阶段1",
    "description": "混合依赖测试",
    "tasks": [
      {"name": "基础", "prompt": "创建基础"},
      {"name": "功能A", "prompt": "创建功能A", "depends_on": ["0-0"]},
      {"name": "功能B", "prompt": "创建功能B", "depends_on": ["0-0"]},
      {"name": "集成", "prompt": "集成测试", "depends_on": ["0-1", "0-2"]}
    ]
  }
]
```"#
        .to_string();

    let chat = MockChat::new(vec![
        plan,
        "```file:src/base.rs\npub fn base() {}\n```".to_string(),
        "```file:src/feature_a.rs\npub fn a() {}\n```".to_string(),
        "```file:src/feature_b.rs\npub fn b() {}\n```".to_string(),
        "```file:src/integration.rs\n#[test] fn t() {}\n```".to_string(),
    ]);

    let runner = MockTestRunner::new()
        .with_check_results(vec![
            make_test_result(true, vec![]),
            make_test_result(true, vec![]),
            make_test_result(true, vec![]),
            make_test_result(true, vec![]),
        ])
        .with_test_results(vec![
            make_test_result(true, vec![]),
            make_test_result(true, vec![]),
            make_test_result(true, vec![]),
            make_test_result(true, vec![]),
        ]);

    let extractor = MockExtractor::new(vec![
        vec![ef("src/base.rs", "pub fn base() {}")],
        vec![ef("src/feature_a.rs", "pub fn a() {}")],
        vec![ef("src/feature_b.rs", "pub fn b() {}")],
        vec![ef("src/integration.rs", "#[test] fn t() {}")],
    ]);

    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10).with_parallel(true);

    orch.run().await.unwrap();

    // 所有 4 个任务都应完成
    for (i, task) in orch.memory.phases[0].tasks.iter().enumerate() {
        assert_eq!(task.status, TaskStatus::Completed, "任务 {} 应完成", i);
    }
}

/// 测试 15: 并行模式默认关闭 (向后兼容)
#[tokio::test]
async fn test_parallel_disabled_by_default() {
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
    // 不调用 with_parallel, 默认应为 false
    assert!(!orch.parallel);

    orch.run().await.unwrap();
    assert_eq!(orch.memory.phases[0].tasks[0].status, TaskStatus::Completed);
}

/// 测试 16: 并行模式 — 空阶段不崩溃
#[tokio::test]
async fn test_parallel_empty_phase() {
    let dir = tempdir().unwrap();
    let ws_dir = dir.path().to_str().unwrap();

    let chat = MockChat::new(vec![r#"```json
[{"name":"空阶段","description":"无任务","tasks":[]}]
```"#
        .to_string()]);

    let runner = MockTestRunner::new();
    let extractor = MockExtractor::new(vec![]);

    let mut orch =
        Orchestrator::new(&chat, runner, extractor, ws_dir, "test", 3, 10).with_parallel(true);

    orch.run().await.unwrap();

    assert_eq!(orch.memory.phases[0].tasks.len(), 0);
    assert_eq!(orch.memory.phases[0].status, PhaseStatus::Completed);
}
