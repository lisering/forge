//! Memory 模块性能基准测试
//!
//! 测试目标:
//! 1. Memory 构造/对话/决策操作
//! 2. build_context 上下文构建
//! 3. 序列化/反序列化 (save/load)
//! 4. 统计方法 (resume_point, all_phases_completed, task counts)
//! 5. 边界条件: 空记忆/大规模/需求变更/Unicode

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use forge::memory::{Memory, Phase, PhaseStatus, Task, TaskStatus};

/// 创建基础任务
fn make_task(id: &str, phase_id: usize, status: TaskStatus) -> Task {
    let is_completed = status == TaskStatus::Completed;
    Task {
        id: id.to_string(),
        phase_id,
        name: format!("任务_{}", id),
        prompt: format!("执行任务 {}", id),
        status,
        result: if is_completed {
            Some("完成".to_string())
        } else {
            None
        },
        attempts: if is_completed { 1 } else { 0 },
        files_written: if is_completed {
            vec![format!("src/{}.rs", id)]
        } else {
            vec![]
        },
        test_result: None,
        last_good_snapshot: None,
        clarifications: vec![],
        depends_on: vec![],
    }
}

/// 创建测试用阶段
fn make_phases(n: usize) -> Vec<Phase> {
    (0..n)
        .map(|i| Phase {
            id: i,
            name: format!("阶段{}", i),
            description: format!("阶段{}的描述", i),
            status: if i < n / 2 {
                PhaseStatus::Completed
            } else if i == n / 2 {
                PhaseStatus::InProgress
            } else {
                PhaseStatus::Pending
            },
            tasks: vec![
                make_task(&format!("{}-0", i), i, TaskStatus::Completed),
                make_task(&format!("{}-1", i), i, TaskStatus::Pending),
                make_task(&format!("{}-2", i), i, TaskStatus::Pending),
            ],
        })
        .collect()
}

/// 构建完整 Memory (含对话+决策+阶段)
fn make_full_memory(phases: usize, conversations: usize) -> Memory {
    let mut mem = Memory::new(&format!("开发目标: {}个阶段", phases));
    mem.set_phases(make_phases(phases));
    mem.current_phase = phases / 2;
    mem.current_task = Some(format!("{}-1", phases / 2));
    mem.workspace_files = (0..10).map(|i| format!("src/module_{}.rs", i)).collect();

    for i in 0..conversations {
        mem.add_conversation(
            if i % 2 == 0 { "user" } else { "assistant" },
            &format!("这是第{}轮对话, 包含一些上下文信息用于测试", i),
            Some(&format!("{}-{}", i % phases, i % 3)),
        );
    }

    for i in 0..(phases * 2) {
        mem.add_decision(
            i % phases,
            Some(&format!("{}-0", i % phases)),
            &format!("决策{}", i),
            &format!("原因{}", i),
        );
    }

    mem
}

/// 基准测试: Memory 构造/对话/决策操作
fn bench_memory_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_operations");

    // new
    group.bench_function("new", |b| {
        b.iter(|| black_box(Memory::new(black_box("开发目标"))))
    });

    // add_conversation 批量
    for size in [10, 100, 1_000] {
        group.throughput(Throughput::Elements(size as u64));
        group.bench_function(BenchmarkId::new("add_conversations", size), |b| {
            b.iter(|| {
                let mut mem = Memory::new("test");
                for i in 0..size {
                    mem.add_conversation(
                        "user",
                        &format!("对话内容 {}", i),
                        Some(&format!("0-{}", i % 3)),
                    );
                }
                black_box(mem.conversations.len())
            })
        });
    }

    // add_decision 批量
    for size in [10, 100, 500] {
        group.throughput(Throughput::Elements(size as u64));
        group.bench_function(BenchmarkId::new("add_decisions", size), |b| {
            b.iter(|| {
                let mut mem = Memory::new("test");
                for i in 0..size {
                    mem.add_decision(
                        i % 5,
                        Some(&format!("{}-0", i % 5)),
                        &format!("决策{}", i),
                        "原因",
                    );
                }
                black_box(mem.decisions.len())
            })
        });
    }

    // set_phases
    for &phases in &[3, 10, 50] {
        let phase_list = make_phases(phases);
        group.bench_function(BenchmarkId::new("set_phases", phases), |b| {
            b.iter(|| {
                let mut mem = Memory::new("test");
                mem.set_phases(black_box(phase_list.clone()));
                black_box(mem.phases.len())
            })
        });
    }

    group.finish();
}

/// 基准测试: build_context 上下文构建
fn bench_context_building(c: &mut Criterion) {
    let mut group = c.benchmark_group("context_building");

    for &(phases, convs) in &[(3, 10), (5, 50), (10, 100), (20, 500)] {
        let mem = make_full_memory(phases, convs);
        let label = format!("p{}_c{}", phases, convs);

        for &max_turns in &[5, 20, 100] {
            group.bench_function(format!("{}_turns{}", label, max_turns), |b| {
                b.iter(|| black_box(mem.build_context(black_box(max_turns))))
            });
        }
    }

    group.finish();
}

/// 基准测试: 序列化/反序列化 (save/load)
fn bench_serialization(c: &mut Criterion) {
    let mut group = c.benchmark_group("serialization");

    for &(phases, convs) in &[(3, 10), (5, 50), (10, 200)] {
        let mem = make_full_memory(phases, convs);
        let label = format!("p{}_c{}", phases, convs);

        // 序列化 (to_string_pretty 内部)
        let json = serde_json::to_string_pretty(&mem).unwrap();
        group.bench_function(format!("{}_serialize", label), |b| {
            b.iter(|| black_box(serde_json::to_string_pretty(black_box(&mem)).unwrap()))
        });

        // 反序列化
        group.bench_function(format!("{}_deserialize", label), |b| {
            b.iter(|| black_box(serde_json::from_str::<Memory>(black_box(&json)).unwrap()))
        });

        // save + load 往返 (使用 TempDir)
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("memory_{}.json", label));
        group.bench_function(format!("{}_save_load_roundtrip", label), |b| {
            b.iter(|| {
                mem.save(black_box(&path)).unwrap();
                let loaded = Memory::load(black_box(&path)).unwrap();
                black_box(loaded.conversations.len())
            })
        });
    }

    group.finish();
}

/// 基准测试: 统计方法
fn bench_statistics(c: &mut Criterion) {
    let mut group = c.benchmark_group("statistics");

    for &phases in &[3, 10, 50] {
        let mem = make_full_memory(phases, phases * 10);

        // resume_point
        group.bench_function(BenchmarkId::new("resume_point", phases), |b| {
            b.iter(|| black_box(mem.resume_point()))
        });

        // all_phases_completed
        group.bench_function(BenchmarkId::new("all_phases_completed", phases), |b| {
            b.iter(|| black_box(mem.all_phases_completed()))
        });

        // completed_task_count
        group.bench_function(BenchmarkId::new("completed_task_count", phases), |b| {
            b.iter(|| black_box(mem.completed_task_count()))
        });

        // total_task_count
        group.bench_function(BenchmarkId::new("total_task_count", phases), |b| {
            b.iter(|| black_box(mem.total_task_count()))
        });
    }

    group.finish();
}

/// 边界条件基准测试
fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_edge_cases");

    // 空 Memory
    let empty_mem = Memory::new("空目标");
    group.bench_function("empty_build_context", |b| {
        b.iter(|| black_box(empty_mem.build_context(black_box(10))))
    });
    group.bench_function("empty_resume_point", |b| {
        b.iter(|| black_box(empty_mem.resume_point()))
    });
    group.bench_function("empty_serialize", |b| {
        b.iter(|| black_box(serde_json::to_string_pretty(black_box(&empty_mem)).unwrap()))
    });

    // 大规模 Memory (50 阶段, 1000 对话)
    let large_mem = make_full_memory(50, 1_000);
    group.bench_function("large_build_context", |b| {
        b.iter(|| black_box(large_mem.build_context(black_box(100))))
    });
    group.bench_function("large_resume_point", |b| {
        b.iter(|| black_box(large_mem.resume_point()))
    });

    // 需求变更
    let mut change_mem = Memory::new("需求变更测试");
    change_mem.set_phases(make_phases(5));
    group.bench_function("add_100_requirement_changes", |b| {
        b.iter(|| {
            let mut mem = change_mem.clone();
            for i in 0..100 {
                mem.add_requirement_change(
                    &format!("变更{}: 添加功能模块{}", i, i),
                    if i % 2 == 0 { "cli" } else { "file" },
                );
            }
            black_box(mem.has_pending_changes())
        })
    });

    // pending_changes_summary
    let mut change_mem2 = Memory::new("测试");
    for i in 0..50 {
        change_mem2.add_requirement_change(&format!("需求{}", i), "cli");
    }
    group.bench_function("pending_changes_summary_50", |b| {
        b.iter(|| black_box(change_mem2.pending_changes_summary()))
    });

    // Unicode 内容
    let mut unicode_mem = Memory::new("开发🎉目标: 支持𝕏多语言");
    unicode_mem.set_phases(make_phases(3));
    for i in 0..20 {
        unicode_mem.add_conversation("user", &format!("用户输入{}: 你好世界🚀", i), None);
        unicode_mem.add_conversation("assistant", &format!("AI回复{}: 成功✓ 完成✗", i), None);
    }
    group.bench_function("unicode_build_context", |b| {
        b.iter(|| black_box(unicode_mem.build_context(black_box(10))))
    });
    group.bench_function("unicode_serialize_deserialize", |b| {
        b.iter(|| {
            let json = serde_json::to_string_pretty(black_box(&unicode_mem)).unwrap();
            let loaded: Memory = serde_json::from_str(black_box(&json)).unwrap();
            black_box(loaded.conversations.len())
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
        .output_directory(std::path::Path::new("target/criterion/memory"))
}

criterion_group! {
    name = memory_benches;
    config = configure_criterion();
    targets =
        bench_memory_operations,
        bench_context_building,
        bench_serialization,
        bench_statistics,
        bench_edge_cases,
}

criterion_main!(memory_benches);
