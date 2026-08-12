#![allow(clippy::useless_vec)]

//! workspace 性能基准测试
//!
//! 测试目标:
//! 1. workspace_init_and_write - 工作区初始化和文件写入
//! 2. workspace_read_and_list - 文件读取和列表
//! 3. snapshot_operations - 版本快照操作
//! 4. known_good_rollback - Known good 标记和回滚
//! 5. edge_cases - 边界场景 (空工作区/大量文件/tree输出)

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use forge::extract::ExtractedFile;
use forge::workspace::Workspace;
use tempfile::TempDir;

// ============================================================================
//  辅助函数
// ============================================================================

fn make_extracted_files(count: usize) -> Vec<ExtractedFile> {
    (0..count)
        .map(|i| ExtractedFile {
            path: format!("src/file_{i}.rs"),
            content: format!("// File {}\npub fn func_{}() {{}}\n", i, i),
            language: String::new(),
        })
        .collect()
}

fn make_workspace_with_files(count: usize) -> (TempDir, Workspace) {
    let dir = TempDir::new().unwrap();
    let ws = Workspace::new(dir.path());
    ws.init().unwrap();
    let files = make_extracted_files(count);
    ws.write_files(&files).unwrap();
    (dir, ws)
}

// ============================================================================
//  基准测试 1: workspace_init_and_write
// ============================================================================

fn bench_workspace_init_and_write(c: &mut Criterion) {
    let mut group = c.benchmark_group("workspace_init_and_write");

    // init
    group.bench_function("init", |b| {
        b.iter_with_setup(
            || TempDir::new().unwrap(),
            |dir| {
                let ws = Workspace::new(dir.path());
                ws.init().unwrap();
                black_box(ws);
            },
        )
    });

    // write_files 不同规模
    for size in [1, 10, 50] {
        group.bench_with_input(BenchmarkId::new("write_files", size), &size, |b, &size| {
            b.iter_with_setup(
                || {
                    let dir = TempDir::new().unwrap();
                    let ws = Workspace::new(dir.path());
                    ws.init().unwrap();
                    let files = make_extracted_files(size);
                    (dir, ws, files)
                },
                |(_dir, ws, files)| {
                    let written = ws.write_files(black_box(&files)).unwrap();
                    black_box(written);
                },
            )
        });
    }

    // write_file (单个)
    group.bench_function("write_single_file", |b| {
        b.iter_with_setup(
            || {
                let dir = TempDir::new().unwrap();
                let ws = Workspace::new(dir.path());
                ws.init().unwrap();
                (dir, ws)
            },
            |(_dir, ws)| {
                ws.write_file("src/main.rs", "fn main() {}").unwrap();
            },
        )
    });

    // write_files_with_snapshot
    group.bench_function("write_with_snapshot", |b| {
        b.iter_with_setup(
            || {
                let dir = TempDir::new().unwrap();
                let ws = Workspace::new(dir.path());
                ws.init().unwrap();
                // 先写入初始文件
                let initial_files = make_extracted_files(5);
                ws.write_files(&initial_files).unwrap();
                (dir, ws)
            },
            |(_dir, ws)| {
                let new_files = make_extracted_files(5);
                let (snap_id, written) = ws
                    .write_files_with_snapshot(black_box(&new_files), "bench")
                    .unwrap();
                black_box((snap_id, written));
            },
        )
    });

    group.finish();
}

// ============================================================================
//  基准测试 2: workspace_read_and_list
// ============================================================================

fn bench_workspace_read_and_list(c: &mut Criterion) {
    let mut group = c.benchmark_group("workspace_read_and_list");

    // read_file
    let (_dir, ws) = make_workspace_with_files(10);
    group.bench_function("read_file", |b| {
        b.iter(|| {
            let content = ws.read_file(black_box("src/file_0.rs")).unwrap();
            black_box(content);
        })
    });

    // list_files 不同规模
    for size in [10, 50, 100] {
        let (_dir, ws) = make_workspace_with_files(size);
        group.bench_with_input(BenchmarkId::new("list_files", size), &ws, |b, ws| {
            b.iter(|| {
                let files = ws.list_files().unwrap();
                black_box(files);
            })
        });
    }

    // is_rust_project
    let (_dir, ws) = make_workspace_with_files(5);
    group.bench_function("is_rust_project_false", |b| {
        b.iter(|| {
            let r = ws.is_rust_project();
            black_box(r);
        })
    });

    // tree
    let (_dir, ws) = make_workspace_with_files(20);
    group.bench_function("tree_20_files", |b| {
        b.iter(|| {
            let tree = ws.tree().unwrap();
            black_box(tree);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 3: snapshot_operations
// ============================================================================

fn bench_snapshot_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("snapshot_operations");

    // snapshot_files
    for size in [5, 20, 50] {
        let (_dir, ws) = make_workspace_with_files(size);
        let file_paths: Vec<String> = (0..size).map(|i| format!("src/file_{i}.rs")).collect();
        group.bench_with_input(
            BenchmarkId::new("snapshot_files", size),
            &file_paths,
            |b, file_paths| {
                b.iter(|| {
                    let id = ws.snapshot_files(black_box(file_paths), "bench").unwrap();
                    black_box(id);
                })
            },
        );
    }

    // snapshot_all
    let (_dir, ws) = make_workspace_with_files(20);
    group.bench_function("snapshot_all_20", |b| {
        b.iter(|| {
            let id = ws.snapshot_all("bench_all").unwrap();
            black_box(id);
        })
    });

    // list_snapshots
    let (_dir, ws) = make_workspace_with_files(10);
    // 创建多个快照
    for _ in 0..5 {
        ws.snapshot_all("bench").unwrap();
    }
    group.bench_function("list_snapshots_5", |b| {
        b.iter(|| {
            let snaps = ws.list_snapshots();
            black_box(snaps);
        })
    });

    // get_known_good_id (未设置)
    let (_dir, ws) = make_workspace_with_files(5);
    group.bench_function("get_known_good_none", |b| {
        b.iter(|| {
            let id = ws.get_known_good_id();
            black_box(id);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 4: known_good_rollback
// ============================================================================

fn bench_known_good_rollback(c: &mut Criterion) {
    let mut group = c.benchmark_group("known_good_rollback");

    // save_known_good
    let (_dir, ws) = make_workspace_with_files(10);
    let snap_id = ws.snapshot_all("kg").unwrap();
    group.bench_function("save_known_good", |b| {
        b.iter(|| {
            ws.save_known_good(black_box(snap_id)).unwrap();
        })
    });

    // get_known_good_id (已设置)
    group.bench_function("get_known_good_set", |b| {
        b.iter(|| {
            let id = ws.get_known_good_id();
            black_box(id);
        })
    });

    // clear_known_good
    group.bench_function("clear_known_good", |b| {
        b.iter(|| {
            ws.clear_known_good().unwrap();
            // 重新设置以供下次迭代
            ws.save_known_good(snap_id).unwrap();
        })
    });

    // rollback_to_snapshot
    let (_dir, ws) = make_workspace_with_files(10);
    let snap_id = ws.snapshot_all("rollback").unwrap();
    // 修改文件
    ws.write_file("src/file_0.rs", "modified").unwrap();
    group.bench_function("rollback_to_snapshot", |b| {
        b.iter_with_setup(
            || {
                // 每次迭代前重新修改文件
                ws.write_file("src/file_0.rs", "modified").unwrap();
            },
            |_| {
                ws.rollback_to_snapshot(black_box(snap_id)).unwrap();
            },
        )
    });

    // rollback_to_known_good
    let (_dir, ws) = make_workspace_with_files(10);
    let kg_id = ws.snapshot_all("kg").unwrap();
    ws.save_known_good(kg_id).unwrap();
    ws.write_file("src/file_0.rs", "broken").unwrap();
    group.bench_function("rollback_to_known_good", |b| {
        b.iter_with_setup(
            || {
                ws.write_file("src/file_0.rs", "broken").unwrap();
            },
            |_| {
                let result = ws.rollback_to_known_good().unwrap();
                black_box(result);
            },
        )
    });

    group.finish();
}

// ============================================================================
//  基准测试 5: edge_cases
// ============================================================================

fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("workspace_edge_cases");

    // 空工作区 list_files
    let dir = TempDir::new().unwrap();
    let ws = Workspace::new(dir.path());
    ws.init().unwrap();
    group.bench_function("list_files_empty", |b| {
        b.iter(|| {
            let files = ws.list_files().unwrap();
            assert!(files.is_empty());
            black_box(files);
        })
    });

    // 空工作区 tree
    group.bench_function("tree_empty", |b| {
        b.iter(|| {
            let tree = ws.tree().unwrap();
            black_box(tree);
        })
    });

    // 空工作区 list_snapshots
    group.bench_function("list_snapshots_empty", |b| {
        b.iter(|| {
            let snaps = ws.list_snapshots();
            assert!(snaps.is_empty());
            black_box(snaps);
        })
    });

    // rollback 不存在的快照
    group.bench_function("rollback_nonexistent", |b| {
        b.iter(|| {
            let result = ws.rollback_to_snapshot(black_box(999));
            assert!(result.is_err());
        })
    });

    // Workspace::new 不 init
    group.bench_function("new_without_init", |b| {
        b.iter(|| {
            let ws = Workspace::new(black_box("/tmp/forge_bench_nonexist"));
            black_box(ws);
        })
    });

    // 大量文件 (100)
    let (_dir, ws) = make_workspace_with_files(100);
    group.bench_function("list_files_100", |b| {
        b.iter(|| {
            let files = ws.list_files().unwrap();
            assert_eq!(files.len(), 100);
            black_box(files);
        })
    });

    group.finish();
}

// ============================================================================
//  配置 & 入口
// ============================================================================

fn configure_criterion() -> Criterion {
    Criterion::default()
        .sample_size(30)
        .measurement_time(std::time::Duration::from_secs(5))
        .warm_up_time(std::time::Duration::from_secs(2))
        .nresamples(50_000)
        .noise_threshold(0.05)
        .output_directory(std::path::Path::new("target/criterion/workspace"))
}

criterion_group! {
    name = workspace_benches;
    config = configure_criterion();
    targets = bench_workspace_init_and_write,
        bench_workspace_read_and_list,
        bench_snapshot_operations,
        bench_known_good_rollback,
        bench_edge_cases,
}

criterion_main!(workspace_benches);
