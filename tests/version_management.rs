//! 集成测试 — 文件版本管理完整工作流
//!
//! 模拟 Orchestrator::execute_task 中的版本管理场景:
//! 写入文件 → 快照 → 破坏 → 回滚 → 验证恢复

use forge::extract::ExtractedFile;
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
    ws.write_file(
        "src/lib.rs",
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}",
    )
    .unwrap();
    (dir, ws)
}

fn ef(path: &str, content: &str) -> ExtractedFile {
    ExtractedFile {
        path: path.to_string(),
        content: content.to_string(),
        language: String::new(),
    }
}

/// 完整版本管理生命周期:
/// 初始写入 → known good → AI 破坏文件 → 回滚 → 验证恢复
#[test]
fn test_full_version_lifecycle() {
    let (_dir, ws) = make_ws();

    // 1. 初始状态: 3 个文件
    assert_eq!(ws.list_files().unwrap().len(), 3);

    // 2. 模拟 cargo check 通过 → 保存 known good
    let good_id = ws.snapshot_all("known_good").unwrap();
    ws.save_known_good(good_id).unwrap();
    assert_eq!(ws.get_known_good_id(), Some(good_id));

    // 3. 模拟 AI 修复尝试 (破坏 main.rs, 新增 broken.rs)
    let files = vec![
        ef("src/main.rs", "fn main() { broken syntax {{{"),
        ef("src/broken.rs", "this is broken"),
    ];
    let (snap_id, _) = ws
        .write_files_with_snapshot(&files, "pre_write_a2")
        .unwrap();
    assert!(snap_id.is_some(), "覆盖已有文件时应保存快照");

    // 4. 验证文件被破坏
    assert!(ws.read_file("src/main.rs").unwrap().contains("broken"));
    assert!(ws.root.join("src/broken.rs").exists());

    // 5. 回滚到 known good
    let rollback_result = ws.rollback_to_known_good().unwrap();
    assert_eq!(rollback_result, Some(good_id));

    // 6. 验证恢复
    assert_eq!(
        ws.read_file("src/main.rs").unwrap(),
        "fn main() {\n    println!(\"Hello, world!\");\n}"
    );
    assert_eq!(
        ws.read_file("src/lib.rs").unwrap(),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}"
    );
}

/// 多次 known good 更新，回滚到最新
#[test]
fn test_known_good_progression() {
    let (_dir, ws) = make_ws();

    // v1 known good
    let good1 = ws.snapshot_all("kg_v1").unwrap();
    ws.save_known_good(good1).unwrap();

    // 修改代码 (模拟 AI 修复成功)
    ws.write_file("src/main.rs", "fn main() {\n    println!(\"v2\");\n}")
        .unwrap();

    // v2 known good
    let good2 = ws.snapshot_all("kg_v2").unwrap();
    ws.save_known_good(good2).unwrap();

    // 再次修改 (失败)
    ws.write_file("src/main.rs", "broken").unwrap();
    ws.write_file("src/lib.rs", "also broken").unwrap();

    // 回滚 → 应恢复到 v2
    ws.rollback_to_known_good().unwrap();
    assert_eq!(
        ws.read_file("src/main.rs").unwrap(),
        "fn main() {\n    println!(\"v2\");\n}"
    );
    assert_eq!(
        ws.read_file("src/lib.rs").unwrap(),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}"
    );
}

/// 跨任务版本隔离
#[test]
fn test_cross_task_version_isolation() {
    let (_dir, ws) = make_ws();

    // 任务 A 成功完成
    let good_a = ws.snapshot_all("kg_taskA").unwrap();
    ws.save_known_good(good_a).unwrap();

    // 任务 B: 第一次尝试 check 通过
    ws.write_file("src/feature.rs", "pub fn feature() {}")
        .unwrap();
    let good_b1 = ws.snapshot_all("kg_taskB_a1").unwrap();
    ws.save_known_good(good_b1).unwrap();

    // 任务 B: 第二次尝试失败 (破坏了 feature.rs)
    let files = vec![
        ef("src/feature.rs", "broken feature"),
        ef("src/main.rs", "also broken"),
    ];
    ws.write_files_with_snapshot(&files, "pre_write_taskB_a2")
        .unwrap();

    // 回滚 → 应恢复到 good_b1 (任务 B 的 known good)
    ws.rollback_to_known_good().unwrap();

    assert_eq!(
        ws.read_file("src/feature.rs").unwrap(),
        "pub fn feature() {}"
    );
    assert_eq!(
        ws.read_file("src/main.rs").unwrap(),
        "fn main() {\n    println!(\"Hello, world!\");\n}"
    );
}

/// 无 known good 时的安全降级
#[test]
fn test_rollback_without_known_good() {
    let (_dir, ws) = make_ws();
    let result = ws.rollback_to_known_good().unwrap();
    assert_eq!(result, None, "无 known good 时返回 None，不报错");
}

/// 回滚恢复被删除的文件
#[test]
fn test_rollback_restores_deleted_file() {
    let (_dir, ws) = make_ws();
    let snap_id = ws.snapshot_all("before_del").unwrap();

    std::fs::remove_file(ws.root.join("src/lib.rs")).unwrap();
    assert!(ws.read_file("src/lib.rs").is_err());

    ws.rollback_to_snapshot(snap_id).unwrap();
    assert_eq!(
        ws.read_file("src/lib.rs").unwrap(),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}"
    );
}

/// 快照元数据完整性
#[test]
fn test_snapshot_metadata_integrity() {
    let (_dir, ws) = make_ws();
    let id = ws.snapshot_all("test_meta").unwrap();
    let snaps = ws.list_snapshots();

    assert_eq!(snaps.len(), 1);
    let snap = &snaps[0];
    assert_eq!(snap.id, id);
    assert_eq!(snap.label, "test_meta");
    assert_eq!(snap.file_count, 3);
    assert!(!snap.timestamp.is_empty());
    assert!(snap.path.exists());

    let meta_path = snap.path.join("_meta.json");
    assert!(meta_path.exists());
    let meta: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&meta_path).unwrap()).unwrap();
    assert_eq!(meta["id"], id);
    assert_eq!(meta["label"], "test_meta");
    assert_eq!(meta["file_count"], 3);
}

/// 多次写入快照序列
#[test]
fn test_multiple_write_snapshots_sequence() {
    let (_dir, ws) = make_ws();

    let files1 = vec![ef("src/main.rs", "version 1")];
    let (snap1, _) = ws.write_files_with_snapshot(&files1, "pw_1").unwrap();
    assert!(snap1.is_some());

    let files2 = vec![ef("src/main.rs", "version 2")];
    let (snap2, _) = ws.write_files_with_snapshot(&files2, "pw_2").unwrap();
    assert!(snap2.is_some());

    assert_ne!(snap1.unwrap(), snap2.unwrap(), "快照序号递增");

    // 回滚到第一次写入前的快照 → 恢复原始内容
    ws.rollback_to_snapshot(snap1.unwrap()).unwrap();
    assert_eq!(
        ws.read_file("src/main.rs").unwrap(),
        "fn main() {\n    println!(\"Hello, world!\");\n}"
    );
}

/// .forge 目录不污染文件列表
#[test]
fn test_forge_dir_not_in_file_list() {
    let (_dir, ws) = make_ws();

    ws.snapshot_all("snap1").unwrap();
    ws.snapshot_all("snap2").unwrap();
    ws.save_known_good(1).unwrap();

    let files = ws.list_files().unwrap();
    assert!(
        files.iter().all(|f| !f.starts_with(".forge/")),
        ".forge/ 不应在文件列表中"
    );
    assert_eq!(files.len(), 3, "应只有 3 个项目文件");
}

/// 模拟 execute_task 完整修复循环 (3 轮)
#[test]
fn test_simulated_execute_task_fix_loop() {
    let (_dir, ws) = make_ws();

    // === Attempt 1: 写入, check 通过, 测试失败 ===
    let files1 = vec![ef("src/main.rs", "fn main() {\n    println!(\"v1\");\n}")];
    let (_, _) = ws
        .write_files_with_snapshot(&files1, "pre_write_a1")
        .unwrap();
    // 模拟 check 通过 → 保存 known good
    let kg1 = ws.snapshot_all("kg_a1").unwrap();
    ws.save_known_good(kg1).unwrap();

    // === Attempt 2: 修复, check 失败 ===
    let files2 = vec![ef("src/main.rs", "broken syntax {{{")];
    let (_, _) = ws
        .write_files_with_snapshot(&files2, "pre_write_a2")
        .unwrap();
    // check 失败, 不保存 known good

    // === Attempt 3: 修复, check 仍失败 (最后一轮) ===
    let files3 = vec![ef("src/main.rs", "still broken }}}")];
    let (_, _) = ws
        .write_files_with_snapshot(&files3, "pre_write_a3")
        .unwrap();
    // check 仍失败, 达到 max_rounds → 回滚
    let rb = ws.rollback_to_known_good().unwrap();
    assert_eq!(rb, Some(kg1), "回滚到 attempt 1 的 known good");

    // 验证恢复到 attempt 1 的代码
    assert_eq!(
        ws.read_file("src/main.rs").unwrap(),
        "fn main() {\n    println!(\"v1\");\n}"
    );
}

/// pre-write 快照保存正确版本
#[test]
fn test_pre_write_snapshot_preserves_original() {
    let (_dir, ws) = make_ws();

    // 原始内容
    let original = ws.read_file("src/main.rs").unwrap();

    // 写入新版本 (自动保存 pre-write 快照)
    let files = vec![ef("src/main.rs", "fn main() { new_version }")];
    let (snap_id, _) = ws.write_files_with_snapshot(&files, "pre_write").unwrap();
    let sid = snap_id.expect("应有快照");

    // 验证当前内容是新版本
    assert_eq!(
        ws.read_file("src/main.rs").unwrap(),
        "fn main() { new_version }"
    );

    // 回滚到 pre-write 快照 → 恢复原始内容
    ws.rollback_to_snapshot(sid).unwrap();
    assert_eq!(ws.read_file("src/main.rs").unwrap(), original);
}
