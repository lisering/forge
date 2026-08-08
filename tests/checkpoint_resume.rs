//! 集成测试 — 断点续传 (checkpoint & resume)
//!
//! 测试 Memory 持久化的完整生命周期:
//! 保存 → 加载 → 验证恢复点 → 与版本管理协同
//!
//! 参考 tests/version_management.rs 的 tempfile 隔离模式

use forge::memory::{Memory, Phase, PhaseStatus, Task, TaskStatus};
use forge::workspace::Workspace;
use tempfile::tempdir;

/// 创建带初始文件的临时工作区
fn make_ws() -> (tempfile::TempDir, Workspace) {
    let dir = tempdir().unwrap();
    let ws = Workspace::new(dir.path());
    ws.init().unwrap();
    ws.write_file(
        "Cargo.toml",
        "[package]\nname = \"testapp\"\nversion = \"0.1.0\"\nedition = \"2021\"",
    )
    .unwrap();
    ws.write_file(
        "src/main.rs",
        "fn main() {\n    println!(\"Hello, world!\");\n}",
    )
    .unwrap();
    (dir, ws)
}

/// 构建一个模拟执行中途的 Memory (阶段0已完成, 阶段1执行到一半)
fn make_mid_execution_memory() -> Memory {
    let mut mem = Memory::new("构建一个 CLI 计算器");
    mem.set_phases(vec![
        Phase {
            id: 0,
            name: "项目初始化".to_string(),
            description: "创建项目结构".to_string(),
            status: PhaseStatus::Completed,
            tasks: vec![Task {
                id: "0-0".to_string(),
                phase_id: 0,
                name: "初始化项目".to_string(),
                prompt: "创建 Cargo.toml 和 main.rs".to_string(),
                status: TaskStatus::Completed,
                result: Some("成功".to_string()),
                attempts: 1,
                files_written: vec!["Cargo.toml".to_string(), "src/main.rs".to_string()],
                test_result: Some("✅ 编译成功".to_string()),
                last_good_snapshot: Some(1),
                clarifications: vec![],
                depends_on: vec![],
            }],
        },
        Phase {
            id: 1,
            name: "功能实现".to_string(),
            description: "实现核心功能".to_string(),
            status: PhaseStatus::InProgress,
            tasks: vec![
                Task {
                    id: "1-0".to_string(),
                    phase_id: 1,
                    name: "解析输入".to_string(),
                    prompt: "实现参数解析".to_string(),
                    status: TaskStatus::Completed,
                    result: Some("成功".to_string()),
                    attempts: 2,
                    files_written: vec!["src/parser.rs".to_string()],
                    test_result: Some("✅ 测试通过".to_string()),
                    last_good_snapshot: Some(2),
                    clarifications: vec![],
                    depends_on: vec![],
                },
                Task {
                    id: "1-1".to_string(),
                    phase_id: 1,
                    name: "计算逻辑".to_string(),
                    prompt: "实现计算功能".to_string(),
                    status: TaskStatus::InProgress,
                    result: None,
                    attempts: 1,
                    files_written: vec!["src/calc.rs".to_string()],
                    test_result: Some("❌ 编译失败".to_string()),
                    last_good_snapshot: None,
                    clarifications: vec![],
                    depends_on: vec![],
                },
                Task {
                    id: "1-2".to_string(),
                    phase_id: 1,
                    name: "输出格式化".to_string(),
                    prompt: "格式化输出结果".to_string(),
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
        },
        Phase {
            id: 2,
            name: "测试和文档".to_string(),
            description: "完善测试和文档".to_string(),
            status: PhaseStatus::Pending,
            tasks: vec![Task {
                id: "2-0".to_string(),
                phase_id: 2,
                name: "编写测试".to_string(),
                prompt: "编写单元测试".to_string(),
                status: TaskStatus::Pending,
                result: None,
                attempts: 0,
                files_written: vec![],
                test_result: None,
                last_good_snapshot: None,
                clarifications: vec![],
                depends_on: vec![],
            }],
        },
    ]);

    mem.current_phase = 1;
    mem.current_task = Some("1-1".to_string());
    mem.workspace_files = vec![
        "Cargo.toml".to_string(),
        "src/main.rs".to_string(),
        "src/parser.rs".to_string(),
        "src/calc.rs".to_string(),
    ];

    mem.add_conversation("user", "请拆解目标", None);
    mem.add_conversation("assistant", "[阶段计划]", None);
    mem.add_conversation("user", "实现参数解析", Some("1-0"));
    mem.add_conversation("assistant", "fn parse() { }", Some("1-0"));
    mem.add_conversation("user", "实现计算功能", Some("1-1"));
    mem.add_conversation("assistant", "fn calc() { broken }", Some("1-1"));

    mem.add_decision(0, None, "完成拆解", "AI 返回计划");
    mem.add_decision(1, Some("1-0"), "任务完成", "测试通过");
    mem.add_decision(1, Some("1-1"), "编译失败", "需要修复");

    mem
}

/// 完整 save → load → resume_point 往返
#[test]
fn test_memory_save_load_roundtrip() {
    let (dir, _ws) = make_ws();
    let memory_path = dir.path().join(".forge").join("memory.json");

    let original = make_mid_execution_memory();
    original.save(&memory_path).unwrap();
    assert!(memory_path.exists());

    let loaded = Memory::load(&memory_path).unwrap();

    // 基本字段
    assert_eq!(loaded.goal, "构建一个 CLI 计算器");
    assert_eq!(loaded.current_phase, 1);
    assert_eq!(loaded.current_task, Some("1-1".to_string()));

    // 阶段状态
    assert_eq!(loaded.phases.len(), 3);
    assert_eq!(loaded.phases[0].status, PhaseStatus::Completed);
    assert_eq!(loaded.phases[1].status, PhaseStatus::InProgress);
    assert_eq!(loaded.phases[2].status, PhaseStatus::Pending);

    // 任务状态
    assert_eq!(loaded.phases[1].tasks.len(), 3);
    assert_eq!(loaded.phases[1].tasks[0].status, TaskStatus::Completed);
    assert_eq!(loaded.phases[1].tasks[1].status, TaskStatus::InProgress);
    assert_eq!(loaded.phases[1].tasks[2].status, TaskStatus::Pending);

    // files_written 保留
    assert_eq!(
        loaded.phases[1].tasks[0].files_written,
        vec!["src/parser.rs"]
    );
    assert_eq!(loaded.phases[1].tasks[1].files_written, vec!["src/calc.rs"]);

    // last_good_snapshot 保留
    assert_eq!(loaded.phases[0].tasks[0].last_good_snapshot, Some(1));
    assert_eq!(loaded.phases[1].tasks[0].last_good_snapshot, Some(2));
    assert_eq!(loaded.phases[1].tasks[1].last_good_snapshot, None);

    // 对话历史完整保留
    assert_eq!(loaded.conversations.len(), 6);
    assert_eq!(loaded.conversations[0].role, "user");
    assert_eq!(loaded.conversations[5].content, "fn calc() { broken }");
    assert_eq!(loaded.conversations[4].task_id, Some("1-1".to_string()));

    // 决策记录完整保留
    assert_eq!(loaded.decisions.len(), 3);
    assert_eq!(loaded.decisions[0].phase_id, 0);
    assert_eq!(loaded.decisions[2].decision, "编译失败");

    // workspace_files 保留
    assert_eq!(loaded.workspace_files.len(), 4);

    // forge_version 记录
    assert_eq!(loaded.forge_version, env!("CARGO_PKG_VERSION"));
}

/// 恢复后 resume_point 正确指向第一个未完成任务
#[test]
fn test_resume_point_after_load() {
    let (dir, _ws) = make_ws();
    let memory_path = dir.path().join(".forge").join("memory.json");

    let original = make_mid_execution_memory();
    original.save(&memory_path).unwrap();

    let loaded = Memory::load(&memory_path).unwrap();

    // 阶段0已完成, 阶段1的任务0已完成, 任务1是InProgress
    // resume_point 应指向 (1, 1)
    assert_eq!(loaded.resume_point(), Some((1, 1)));
}

/// 恢复后跳过已完成的阶段
#[test]
fn test_resume_skips_completed_phases() {
    let mem = make_mid_execution_memory();

    // 阶段0已完成, 应跳过
    let mut skipped_phases = Vec::new();
    for (idx, phase) in mem.phases.iter().enumerate() {
        if phase.status == PhaseStatus::Completed {
            skipped_phases.push(idx);
        }
    }
    assert_eq!(skipped_phases, vec![0], "阶段0应被跳过");

    // 阶段1和2不应跳过
    assert_eq!(mem.phases[1].status, PhaseStatus::InProgress);
    assert_eq!(mem.phases[2].status, PhaseStatus::Pending);
}

/// 恢复后跳过已完成的任务
#[test]
fn test_resume_skips_completed_tasks() {
    let mem = make_mid_execution_memory();

    // 阶段1中, 任务0已完成应跳过, 任务1(InProgress)和2(Pending)不跳过
    let phase1 = &mem.phases[1];
    let mut skipped_tasks = Vec::new();
    for (idx, task) in phase1.tasks.iter().enumerate() {
        if task.status == TaskStatus::Completed {
            skipped_tasks.push(idx);
        }
    }
    assert_eq!(skipped_tasks, vec![0], "任务0应被跳过");

    // 任务1是InProgress (需要重新执行), 任务2是Pending
    assert_eq!(phase1.tasks[1].status, TaskStatus::InProgress);
    assert_eq!(phase1.tasks[2].status, TaskStatus::Pending);
}

/// 所有阶段完成时的 resume_point
#[test]
fn test_resume_point_all_completed() {
    let mut mem = Memory::new("目标");
    mem.set_phases(vec![
        Phase {
            id: 0,
            name: "阶段1".to_string(),
            description: "".to_string(),
            status: PhaseStatus::Completed,
            tasks: vec![Task {
                id: "0-0".to_string(),
                phase_id: 0,
                name: "任务1".to_string(),
                prompt: "".to_string(),
                status: TaskStatus::Completed,
                result: None,
                attempts: 1,
                files_written: vec![],
                test_result: None,
                last_good_snapshot: None,
                clarifications: vec![],
                depends_on: vec![],
            }],
        },
        Phase {
            id: 1,
            name: "阶段2".to_string(),
            description: "".to_string(),
            status: PhaseStatus::Completed,
            tasks: vec![Task {
                id: "1-0".to_string(),
                phase_id: 1,
                name: "任务2".to_string(),
                prompt: "".to_string(),
                status: TaskStatus::Completed,
                result: None,
                attempts: 1,
                files_written: vec![],
                test_result: None,
                last_good_snapshot: None,
                clarifications: vec![],
                depends_on: vec![],
            }],
        },
    ]);

    assert_eq!(mem.resume_point(), None);
    assert!(mem.all_phases_completed());
    assert_eq!(mem.completed_task_count(), 2);
    assert_eq!(mem.total_task_count(), 2);
}

/// 断点续传与版本管理协同: 恢复时检查 known good 快照
#[test]
fn test_resume_with_version_management() {
    let (dir, ws) = make_ws();
    let memory_path = dir.path().join(".forge").join("memory.json");

    // 模拟已知良好状态: 保存快照
    let kg_id = ws.snapshot_all("known_good").unwrap();
    ws.save_known_good(kg_id).unwrap();

    // 保存 Memory (包含 last_good_snapshot 信息)
    let mut mem = make_mid_execution_memory();
    // 更新 known good 信息到 memory 中
    mem.phases[0].tasks[0].last_good_snapshot = Some(kg_id);
    mem.save(&memory_path).unwrap();

    // 模拟中断后恢复
    let loaded = Memory::load(&memory_path).unwrap();

    // 验证 known good 快照状态一致
    let ws_kg = ws.get_known_good_id();
    assert_eq!(ws_kg, Some(kg_id), "工作区的 known good 应保持");

    // 验证 memory 中的 last_good_snapshot 一致
    assert_eq!(
        loaded.phases[0].tasks[0].last_good_snapshot,
        Some(kg_id),
        "memory 中的 known good 应与工作区一致"
    );
}

/// 断点续传与版本管理协同: 恢复后工作区文件列表应更新
#[test]
fn test_resume_updates_workspace_files() {
    let (dir, ws) = make_ws();
    let memory_path = dir.path().join(".forge").join("memory.json");

    // 保存 Memory
    let mem = make_mid_execution_memory();
    mem.save(&memory_path).unwrap();

    // 模拟恢复后重新读取工作区文件列表
    let loaded = Memory::load(&memory_path).unwrap();
    let actual_files: Vec<String> = ws
        .list_files()
        .unwrap_or_default()
        .into_iter()
        .filter(|f| !f.starts_with("target/"))
        .collect();

    // 工作区实际文件可能与 memory 中记录的不同 (中断后可能有变更)
    // 恢复时应以工作区实际为准
    assert!(actual_files.contains(&"Cargo.toml".to_string()));
    assert!(actual_files.contains(&"src/main.rs".to_string()));

    // 但 memory 中的记录是中断时的快照
    assert!(loaded.workspace_files.contains(&"src/calc.rs".to_string()));
}

/// 多次保存覆盖: 最新状态覆盖旧状态
#[test]
fn test_multiple_saves_overwrite() {
    let (dir, _ws) = make_ws();
    let memory_path = dir.path().join(".forge").join("memory.json");

    // 第一次保存: 初始状态
    let mut mem = Memory::new("目标");
    mem.set_phases(vec![Phase {
        id: 0,
        name: "阶段1".to_string(),
        description: "".to_string(),
        status: PhaseStatus::Pending,
        tasks: vec![Task {
            id: "0-0".to_string(),
            phase_id: 0,
            name: "任务1".to_string(),
            prompt: "".to_string(),
            status: TaskStatus::Pending,
            result: None,
            attempts: 0,
            files_written: vec![],
            test_result: None,
            last_good_snapshot: None,
            clarifications: vec![],
            depends_on: vec![],
        }],
    }]);
    mem.save(&memory_path).unwrap();

    // 验证初始状态
    let loaded1 = Memory::load(&memory_path).unwrap();
    assert_eq!(loaded1.phases[0].status, PhaseStatus::Pending);
    assert_eq!(loaded1.phases[0].tasks[0].status, TaskStatus::Pending);
    assert_eq!(loaded1.conversations.len(), 0);

    // 第二次保存: 更新状态 (完成任务0, 添加对话)
    mem.phases[0].status = PhaseStatus::Completed;
    mem.phases[0].tasks[0].status = TaskStatus::Completed;
    mem.add_conversation("user", "执行任务1", Some("0-0"));
    mem.add_conversation("assistant", "完成任务", Some("0-0"));
    mem.save(&memory_path).unwrap();

    // 验证更新后状态
    let loaded2 = Memory::load(&memory_path).unwrap();
    assert_eq!(loaded2.phases[0].status, PhaseStatus::Completed);
    assert_eq!(loaded2.phases[0].tasks[0].status, TaskStatus::Completed);
    assert_eq!(loaded2.conversations.len(), 2);
    assert_eq!(loaded2.conversations[1].content, "完成任务");
    assert_eq!(
        loaded2.resume_point(),
        None,
        "所有任务完成后 resume_point 为 None"
    );
}

/// 空 Memory 的 save/load
#[test]
fn test_save_load_empty_memory() {
    let (dir, _ws) = make_ws();
    let memory_path = dir.path().join(".forge").join("memory.json");

    let empty = Memory::new("空目标");
    empty.save(&memory_path).unwrap();

    let loaded = Memory::load(&memory_path).unwrap();
    assert_eq!(loaded.goal, "空目标");
    assert!(loaded.phases.is_empty());
    assert!(loaded.conversations.is_empty());
    assert!(loaded.decisions.is_empty());
    assert_eq!(loaded.current_phase, 0);
    assert!(loaded.current_task.is_none());
    assert!(loaded.workspace_files.is_empty());
    assert_eq!(loaded.resume_point(), None);
    assert!(!loaded.all_phases_completed());
}

/// 损坏的 memory.json 加载失败
#[test]
fn test_load_corrupt_memory_fails() {
    let (dir, _ws) = make_ws();
    let memory_path = dir.path().join(".forge").join("memory.json");

    std::fs::write(&memory_path, "{ this is not valid json }").unwrap();

    assert!(Memory::load(&memory_path).is_err());
}

/// 不存在的 memory.json 加载失败
#[test]
fn test_load_nonexistent_memory_fails() {
    let (dir, _ws) = make_ws();
    let memory_path = dir.path().join(".forge").join("nonexistent.json");

    assert!(Memory::load(&memory_path).is_err());
}

/// memory.json 不污染文件列表
#[test]
fn test_memory_json_not_in_file_list() {
    let (_dir, ws) = make_ws();
    let memory_path = ws.root.join(".forge").join("memory.json");

    let mem = Memory::new("目标");
    mem.save(&memory_path).unwrap();

    let files = ws.list_files().unwrap();
    assert!(
        files.iter().all(|f| !f.starts_with(".forge/")),
        ".forge/ 不应在文件列表中"
    );
}

/// 版本号记录在 memory.json 中
#[test]
fn test_memory_json_records_version() {
    let (dir, _ws) = make_ws();
    let memory_path = dir.path().join(".forge").join("memory.json");

    let mem = Memory::new("测试");
    mem.save(&memory_path).unwrap();

    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&memory_path).unwrap()).unwrap();
    assert_eq!(
        json["forge_version"].as_str().unwrap(),
        env!("CARGO_PKG_VERSION")
    );
}

/// 模拟完整断点续传场景:
/// 1. 阶段0完成 → 保存
/// 2. 阶段1任务0完成 → 保存
/// 3. 阶段1任务1中断 (InProgress) → 保存
/// 4. 恢复 → resume_point 指向 (1, 1)
/// 5. 任务1重试完成 → 保存
/// 6. 恢复 → resume_point 指向 (1, 2)
#[test]
fn test_full_checkpoint_resume_lifecycle() {
    let (dir, _ws) = make_ws();
    let memory_path = dir.path().join(".forge").join("memory.json");

    // Step 1: 阶段0完成
    let mut mem = Memory::new("目标");
    mem.set_phases(vec![
        Phase {
            id: 0,
            name: "阶段0".to_string(),
            description: "".to_string(),
            status: PhaseStatus::Completed,
            tasks: vec![Task {
                id: "0-0".to_string(),
                phase_id: 0,
                name: "任务0-0".to_string(),
                prompt: "".to_string(),
                status: TaskStatus::Completed,
                result: None,
                attempts: 1,
                files_written: vec![],
                test_result: None,
                last_good_snapshot: None,
                clarifications: vec![],
                depends_on: vec![],
            }],
        },
        Phase {
            id: 1,
            name: "阶段1".to_string(),
            description: "".to_string(),
            status: PhaseStatus::InProgress,
            tasks: vec![
                Task {
                    id: "1-0".to_string(),
                    phase_id: 1,
                    name: "任务1-0".to_string(),
                    prompt: "".to_string(),
                    status: TaskStatus::Completed,
                    result: None,
                    attempts: 1,
                    files_written: vec![],
                    test_result: None,
                    last_good_snapshot: None,
                    clarifications: vec![],
                    depends_on: vec![],
                },
                Task {
                    id: "1-1".to_string(),
                    phase_id: 1,
                    name: "任务1-1".to_string(),
                    prompt: "".to_string(),
                    status: TaskStatus::InProgress,
                    result: None,
                    attempts: 1,
                    files_written: vec![],
                    test_result: None,
                    last_good_snapshot: None,
                    clarifications: vec![],
                    depends_on: vec![],
                },
                Task {
                    id: "1-2".to_string(),
                    phase_id: 1,
                    name: "任务1-2".to_string(),
                    prompt: "".to_string(),
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
        },
    ]);
    mem.save(&memory_path).unwrap();

    // Step 4: 恢复 → resume_point 指向 (1, 1)
    let loaded = Memory::load(&memory_path).unwrap();
    assert_eq!(loaded.resume_point(), Some((1, 1)));

    // Step 5: 任务1-1重试完成
    let mut mem = loaded;
    mem.phases[1].tasks[1].status = TaskStatus::Completed;
    mem.save(&memory_path).unwrap();

    // Step 6: 恢复 → resume_point 指向 (1, 2)
    let loaded2 = Memory::load(&memory_path).unwrap();
    assert_eq!(loaded2.resume_point(), Some((1, 2)));

    // Step 7: 任务1-2完成, 阶段1完成
    let mut mem = loaded2;
    mem.phases[1].tasks[2].status = TaskStatus::Completed;
    mem.phases[1].status = PhaseStatus::Completed;
    mem.save(&memory_path).unwrap();

    // Step 8: 恢复 → resume_point 为 None (全部完成)
    let loaded3 = Memory::load(&memory_path).unwrap();
    assert_eq!(loaded3.resume_point(), None);
    assert!(loaded3.all_phases_completed());
    assert_eq!(loaded3.completed_task_count(), 4);
    assert_eq!(loaded3.total_task_count(), 4);
}

/// memory.json 保存的对话历史包含完整内容 (不只是摘要)
#[test]
fn test_saved_conversations_have_full_content() {
    let (dir, _ws) = make_ws();
    let memory_path = dir.path().join(".forge").join("memory.json");

    let mut mem = Memory::new("测试");
    let long_content = "这是一段很长的内容".repeat(20);
    mem.add_conversation("assistant", &long_content, Some("0-0"));
    mem.save(&memory_path).unwrap();

    let loaded = Memory::load(&memory_path).unwrap();
    assert_eq!(loaded.conversations.len(), 1);
    // 完整内容保留 (不只是前100字摘要)
    assert_eq!(loaded.conversations[0].content, long_content);
    // 摘要只有前100字
    assert_eq!(
        loaded.conversations[0].summary,
        "这是一段很长的内容"
            .repeat(20)
            .chars()
            .take(100)
            .collect::<String>()
    );
}
