//! Orchestrator 模块性能基准测试
//!
//! 测试目标:
//! 1. build_fix_messages_with_memory 消息列表构建
//! 2. MemoryContextStats 操作 (注入统计)
//! 3. FixPromptBuilder 路径规范化 + 文件内容读取
//! 4. ContextBuilder 代码摘要 + 文件列表
//! 5. 边界条件: 空数据/大规模/Unicode/版本管理

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use forge::memory::{Memory, Phase, PhaseStatus, Task, TaskStatus};
use forge::orchestrator::{
    build_fix_messages_with_memory, ContextBuilder, FixPromptBuilder, MemoryContextStats,
    VersionManager,
};
use forge::workspace::Workspace;

/// 创建临时工作区并初始化
fn make_workspace() -> (tempfile::TempDir, Workspace) {
    let dir = tempfile::tempdir().unwrap();
    let ws = Workspace::new(dir.path());
    ws.init().unwrap();
    (dir, ws)
}

/// 创建带文件的临时工作区
fn make_workspace_with_files(n: usize) -> (tempfile::TempDir, Workspace) {
    let (dir, ws) = make_workspace();
    ws.write_file(
        "Cargo.toml",
        "[package]\nname = \"test\"\nversion = \"0.1.0\"",
    )
    .unwrap();
    for i in 0..n {
        ws.write_file(
            &format!("src/module_{}.rs", i),
            &format!(
                "//! Module {}\n\npub fn func_{}() -> i32 {{\n    {}\n}}\n",
                i,
                i,
                i + 1
            ),
        )
        .unwrap();
    }
    (dir, ws)
}

/// 创建测试用 Memory
fn make_memory(phases: usize) -> Memory {
    let mut mem = Memory::new("测试目标");
    let phase_list: Vec<Phase> = (0..phases)
        .map(|i| Phase {
            id: i,
            name: format!("阶段{}", i),
            description: format!("描述{}", i),
            status: PhaseStatus::InProgress,
            tasks: vec![Task {
                id: format!("{}-0", i),
                phase_id: i,
                name: "任务".to_string(),
                prompt: "prompt".to_string(),
                status: TaskStatus::InProgress,
                result: None,
                attempts: 1,
                files_written: vec![format!("src/module_{}.rs", i)],
                test_result: None,
                last_good_snapshot: None,
                clarifications: vec![],
                depends_on: vec![],
            }],
        })
        .collect();
    mem.set_phases(phase_list);
    mem.workspace_files = (0..phases)
        .map(|i| format!("src/module_{}.rs", i))
        .collect();
    mem
}

/// 基准测试: build_fix_messages_with_memory
fn bench_fix_messages(c: &mut Criterion) {
    let mut group = c.benchmark_group("fix_messages");

    // 基本场景
    for &mem_count in &[0, 1, 5, 20, 100] {
        let memory_messages: Vec<String> =
            (0..mem_count).map(|i| format!("历史消息 {}", i)).collect();
        let first_prompt = Some("首次尝试 prompt".to_string());
        let fix_prompt = "修复编译错误".to_string();

        group.bench_function(BenchmarkId::new("with_first_prompt", mem_count), |b| {
            b.iter(|| {
                black_box(build_fix_messages_with_memory(
                    black_box(&first_prompt),
                    black_box(&fix_prompt),
                    black_box(&memory_messages),
                ))
            })
        });

        let no_first: Option<String> = None;
        group.bench_function(BenchmarkId::new("without_first_prompt", mem_count), |b| {
            b.iter(|| {
                black_box(build_fix_messages_with_memory(
                    black_box(&no_first),
                    black_box(&fix_prompt),
                    black_box(&memory_messages),
                ))
            })
        });
    }

    // 批量测试
    for size in [10, 100, 1_000] {
        group.throughput(Throughput::Elements(size as u64));
        let messages: Vec<String> = (0..size).map(|i| format!("消息内容 {}", i)).collect();
        let first = Some("first".to_string());
        group.bench_function(BenchmarkId::new("batch", size), |b| {
            b.iter(|| {
                black_box(build_fix_messages_with_memory(
                    black_box(&first),
                    black_box("fix"),
                    black_box(&messages),
                ))
            })
        });
    }

    group.finish();
}

/// 基准测试: MemoryContextStats 操作
fn bench_memory_context_stats(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_context_stats");

    // 构造 + record
    for &injections in &[1, 10, 100, 1_000] {
        group.bench_function(BenchmarkId::new("record_injections", injections), |b| {
            b.iter(|| {
                let mut stats = MemoryContextStats::new();
                for i in 0..injections {
                    stats.record_injection(
                        black_box(5 + i as usize % 10),
                        black_box(i as usize % 3),
                    );
                }
                black_box(stats.has_data())
            })
        });
    }

    // 统计方法
    let mut stats = MemoryContextStats::new();
    for i in 0..100usize {
        stats.record_injection(5 + i % 10, i % 3);
    }
    group.bench_function("avg_messages_100", |b| {
        b.iter(|| black_box(stats.avg_messages_per_injection()))
    });
    group.bench_function("skip_rate_100", |b| b.iter(|| black_box(stats.skip_rate())));
    group.bench_function("to_summary_100", |b| {
        b.iter(|| black_box(stats.to_summary()))
    });

    // 空统计
    let empty_stats = MemoryContextStats::new();
    group.bench_function("empty_avg", |b| {
        b.iter(|| black_box(empty_stats.avg_messages_per_injection()))
    });
    group.bench_function("empty_skip_rate", |b| {
        b.iter(|| black_box(empty_stats.skip_rate()))
    });
    group.bench_function("empty_to_summary", |b| {
        b.iter(|| black_box(empty_stats.to_summary()))
    });

    group.finish();
}

/// 基准测试: FixPromptBuilder 路径规范化 + 文件内容读取
fn bench_fix_prompt_builder(c: &mut Criterion) {
    let mut group = c.benchmark_group("fix_prompt_builder");

    let (dir, ws) = make_workspace_with_files(20);
    let _ = &dir;

    // normalize_error_path
    let relative_paths: Vec<String> = (0..20).map(|i| format!("src/module_{}.rs", i)).collect();
    let absolute_paths: Vec<String> = relative_paths
        .iter()
        .map(|p| format!("{}/{}", ws.root.display(), p))
        .collect();
    let external_paths: Vec<String> = (0..10)
        .map(|i| format!("/Users/.../.cargo/registry/src/package_{}/lib.rs", i))
        .collect();

    group.bench_function("normalize_relative_20", |b| {
        b.iter(|| {
            for p in &relative_paths {
                black_box(FixPromptBuilder::normalize_error_path(
                    black_box(&ws),
                    black_box(p),
                ));
            }
        })
    });
    group.bench_function("normalize_absolute_20", |b| {
        b.iter(|| {
            for p in &absolute_paths {
                black_box(FixPromptBuilder::normalize_error_path(
                    black_box(&ws),
                    black_box(p),
                ));
            }
        })
    });
    group.bench_function("normalize_external_10", |b| {
        b.iter(|| {
            for p in &external_paths {
                black_box(FixPromptBuilder::normalize_error_path(
                    black_box(&ws),
                    black_box(p),
                ));
            }
        })
    });

    // get_files_full_content
    for &n in &[1, 5, 20] {
        let paths: Vec<String> = (0..n).map(|i| format!("src/module_{}.rs", i)).collect();
        group.bench_function(BenchmarkId::new("get_files_content", n), |b| {
            b.iter(|| {
                black_box(FixPromptBuilder::get_files_full_content(
                    black_box(&ws),
                    black_box(&paths),
                ))
            })
        });
    }

    // 不存在的文件
    let nonexistent: Vec<String> = (0..10)
        .map(|i| format!("src/nonexistent_{}.rs", i))
        .collect();
    group.bench_function("get_files_nonexistent_10", |b| {
        b.iter(|| {
            black_box(FixPromptBuilder::get_files_full_content(
                black_box(&ws),
                black_box(&nonexistent),
            ))
        })
    });

    group.finish();
}

/// 基准测试: ContextBuilder 代码摘要 + 文件列表
fn bench_context_builder(c: &mut Criterion) {
    let mut group = c.benchmark_group("context_builder");

    // get_current_code_summary
    for &n in &[1, 5, 10, 20] {
        let (dir, ws) = make_workspace_with_files(n);
        let _ = &dir;
        group.bench_function(BenchmarkId::new("code_summary", n), |b| {
            b.iter(|| black_box(ContextBuilder::get_current_code_summary(black_box(&ws))))
        });
    }

    // get_project_file_list
    for &n in &[1, 10, 50, 200] {
        let mem = make_memory(n);
        group.bench_function(BenchmarkId::new("file_list", n), |b| {
            b.iter(|| black_box(ContextBuilder::get_project_file_list(black_box(&mem))))
        });
    }

    // 空项目
    let (dir, empty_ws) = make_workspace();
    let _ = &dir;
    group.bench_function("code_summary_empty", |b| {
        b.iter(|| {
            black_box(ContextBuilder::get_current_code_summary(black_box(
                &empty_ws,
            )))
        })
    });

    let empty_mem = Memory::new("空");
    group.bench_function("file_list_empty", |b| {
        b.iter(|| black_box(ContextBuilder::get_project_file_list(black_box(&empty_mem))))
    });

    group.finish();
}

/// 边界条件基准测试
fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("orchestrator_edge_cases");

    // build_fix_messages_with_memory 边界
    group.bench_function("fix_messages_empty_all", |b| {
        b.iter(|| {
            black_box(build_fix_messages_with_memory(
                black_box(&None),
                black_box(""),
                black_box(&[]),
            ))
        })
    });

    group.bench_function("fix_messages_empty_memory", |b| {
        b.iter(|| {
            black_box(build_fix_messages_with_memory(
                black_box(&Some("first".to_string())),
                black_box("fix"),
                black_box(&[]),
            ))
        })
    });

    // 大量历史消息
    let large_messages: Vec<String> = (0..10_000)
        .map(|i| format!("消息{}: 这是一段较长的历史消息内容用于测试性能", i))
        .collect();
    let large_first = Some("首次 prompt".to_string());
    group.bench_function("fix_messages_10k_history", |b| {
        b.iter(|| {
            black_box(build_fix_messages_with_memory(
                black_box(&large_first),
                black_box("修复"),
                black_box(&large_messages),
            ))
        })
    });

    // Unicode 消息
    let unicode_messages: Vec<String> = (0..100)
        .map(|i| format!("历史消息{}: 你好世界🎉 𝕏 ℝ ℂ ✓ ✗", i))
        .collect();
    group.bench_function("fix_messages_unicode_100", |b| {
        b.iter(|| {
            black_box(build_fix_messages_with_memory(
                black_box(&Some("首次🎯".to_string())),
                black_box("修复✗"),
                black_box(&unicode_messages),
            ))
        })
    });

    // MemoryContextStats 边界
    group.bench_function("stats_empty_to_summary", |b| {
        b.iter(|| black_box(MemoryContextStats::new().to_summary()))
    });

    // VersionManager 操作
    let (dir, ws) = make_workspace_with_files(5);
    let _ = &dir;
    group.bench_function("save_known_good", |b| {
        b.iter(|| black_box(VersionManager::save_known_good(black_box(&ws)).unwrap()))
    });

    // rollback (需要先保存 known good)
    let _ = VersionManager::save_known_good(&ws).unwrap();
    group.bench_function("rollback_to_known_good", |b| {
        b.iter(|| black_box(VersionManager::rollback_to_known_good(black_box(&ws)).unwrap()))
    });

    // normalize_error_path 边界
    group.bench_function("normalize_empty_path", |b| {
        b.iter(|| {
            black_box(FixPromptBuilder::normalize_error_path(
                black_box(&ws),
                black_box(""),
            ))
        })
    });
    group.bench_function("normalize_whitespace_path", |b| {
        b.iter(|| {
            black_box(FixPromptBuilder::normalize_error_path(
                black_box(&ws),
                black_box("   "),
            ))
        })
    });

    group.finish();
}

/// 配置基准测试参数
fn configure_criterion() -> Criterion {
    Criterion::default()
        .sample_size(30)
        .measurement_time(std::time::Duration::from_secs(5))
        .warm_up_time(std::time::Duration::from_secs(2))
        .nresamples(30_000)
        .noise_threshold(0.05)
        .output_directory(std::path::Path::new("target/criterion/orchestrator"))
}

criterion_group! {
    name = orchestrator_benches;
    config = configure_criterion();
    targets =
        bench_fix_messages,
        bench_memory_context_stats,
        bench_fix_prompt_builder,
        bench_context_builder,
        bench_edge_cases,
}

criterion_main!(orchestrator_benches);
