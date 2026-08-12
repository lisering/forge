#![allow(clippy::useless_vec)]

//! Steer Reminder 模块性能基准测试
//!
//! 测试目标:
//! 1. extract_phase_name + extract_task_name — 阶段/任务名称提取性能
//! 2. format_goal_line + format_phase_task_line + format_constraints_section — 格式化性能
//! 3. check_remind_needed — 提醒触发判断性能
//! 4. SteerReminder::to_prompt — 完整提醒 prompt 生成性能
//! 5. edge_cases — 边界条件性能

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use forge::memory::{Phase, PhaseStatus, Task, TaskStatus};
use forge::steer_reminder::{
    check_remind_needed, extract_phase_name, extract_task_name, format_constraints_section,
    format_goal_line, format_phase_task_line, SteerReminder,
};

/// 构建测试用 Phase
fn make_phase(id: usize, name: &str, task_count: usize) -> Phase {
    Phase {
        id,
        name: name.to_string(),
        description: format!("阶段 {id} 描述"),
        status: PhaseStatus::InProgress,
        tasks: (0..task_count)
            .map(|i| Task {
                id: format!("{id}-{i}"),
                phase_id: id,
                name: format!("任务{i}"),
                prompt: format!("创建任务{i}"),
                status: if i % 2 == 0 {
                    TaskStatus::Completed
                } else {
                    TaskStatus::Pending
                },
                result: Some(format!("任务{i}完成结果")),
                attempts: i as u32,
                files_written: vec![format!("src/file{i}.rs")],
                test_result: None,
                last_good_snapshot: None,
                clarifications: vec![],
                depends_on: vec![],
            })
            .collect(),
    }
}

/// 构建多阶段 Memory 数据
fn build_phases(phase_count: usize, tasks_per_phase: usize) -> Vec<Phase> {
    (0..phase_count)
        .map(|i| make_phase(i, &format!("阶段{i}"), tasks_per_phase))
        .collect()
}

/// 基准测试: extract_phase_name + extract_task_name
fn bench_extract_names(c: &mut Criterion) {
    let mut group = c.benchmark_group("extract_names");

    let sizes: Vec<usize> = vec![1, 5, 20, 100];

    for &size in &sizes {
        group.throughput(Throughput::Elements(size as u64));
        let phases = build_phases(size, 5);

        // extract_phase_name: 正常索引
        group.bench_with_input(
            BenchmarkId::new("phase_name_found", size),
            &phases,
            |b, phases| {
                b.iter(|| black_box(extract_phase_name(black_box(phases), black_box(size / 2))))
            },
        );

        // extract_phase_name: 越界索引
        group.bench_with_input(
            BenchmarkId::new("phase_name_oob", size),
            &phases,
            |b, phases| b.iter(|| black_box(extract_phase_name(black_box(phases), black_box(999)))),
        );

        // extract_task_name: 找到任务
        let task_id = format!("{}-2", size / 2);
        group.bench_with_input(
            BenchmarkId::new("task_name_found", size),
            &phases,
            |b, phases| {
                b.iter(|| {
                    black_box(extract_task_name(
                        black_box(phases),
                        black_box(&Some(task_id.clone())),
                    ))
                })
            },
        );

        // extract_task_name: None
        group.bench_with_input(
            BenchmarkId::new("task_name_none", size),
            &phases,
            |b, phases| {
                b.iter(|| black_box(extract_task_name(black_box(phases), black_box(&None))))
            },
        );
    }
    group.finish();
}

/// 基准测试: format_goal_line + format_phase_task_line + format_constraints_section
fn bench_format_functions(c: &mut Criterion) {
    let mut group = c.benchmark_group("format_functions");

    // format_goal_line
    let long_goal = "x".repeat(500);
    let goals: Vec<(&str, &str)> = vec![
        ("short", "构建计算器"),
        ("empty", ""),
        ("unicode", "🚀 构建 α & β 系统 — 测试用"),
        ("long", &long_goal),
    ];

    for (name, goal) in &goals {
        group.bench_function(format!("goal_line/{name}"), |b| {
            b.iter(|| black_box(format_goal_line(black_box(goal))))
        });
    }

    // format_phase_task_line
    let phase_task_cases: Vec<(&str, &str, &str)> = vec![
        ("both", "阶段A", "任务B"),
        ("phase_only", "阶段A", ""),
        ("task_only", "", "任务B"),
        ("both_empty", "", ""),
    ];

    for (name, phase, task) in &phase_task_cases {
        group.bench_function(format!("phase_task/{name}"), |b| {
            b.iter(|| black_box(format_phase_task_line(black_box(phase), black_box(task))))
        });
    }

    // format_constraints_section
    let constraint_sizes: Vec<usize> = vec![0, 3, 10, 50, 200];

    for &size in &constraint_sizes {
        group.throughput(Throughput::Elements(size as u64));
        let constraints: Vec<String> = (0..size).map(|i| format!("约束{i}")).collect();

        group.bench_with_input(
            BenchmarkId::new("constraints", size),
            &constraints,
            |b, constraints| {
                b.iter(|| black_box(format_constraints_section(black_box(constraints))))
            },
        );
    }
    group.finish();
}

/// 基准测试: check_remind_needed
fn bench_check_remind_needed(c: &mut Criterion) {
    let mut group = c.benchmark_group("check_remind_needed");

    let intervals: Vec<usize> = vec![0, 1, 10, 100];
    let turns: Vec<usize> = vec![0, 1, 9, 10, 15, 50, 100];

    for &interval in &intervals {
        for &turn in &turns {
            group.bench_function(format!("interval_{interval}/turn_{turn}"), |b| {
                b.iter(|| black_box(check_remind_needed(black_box(interval), black_box(turn))))
            });
        }
    }
    group.finish();
}

/// 基准测试: SteerReminder::to_prompt (完整提醒 prompt 生成)
fn bench_to_prompt(c: &mut Criterion) {
    let mut group = c.benchmark_group("steer_reminder_to_prompt");

    let constraint_sizes: Vec<usize> = vec![0, 3, 10, 50];

    for &size in &constraint_sizes {
        group.throughput(Throughput::Elements(size as u64));
        let reminder = SteerReminder {
            goal: "构建一个高性能 Rust CLI 计算器工具".to_string(),
            current_phase: "功能实现".to_string(),
            current_task: "实现加减乘除运算".to_string(),
            constraints: (0..size).map(|i| format!("约束{i}")).collect(),
            interval: 10,
        };

        group.bench_function(format!("constraints_{size}"), |b| {
            b.iter(|| black_box(black_box(&reminder).to_prompt()))
        });
    }

    // 空状态 reminder
    let empty_reminder = SteerReminder {
        goal: String::new(),
        current_phase: String::new(),
        current_task: String::new(),
        constraints: vec![],
        interval: 0,
    };
    group.bench_function("empty_state", |b| {
        b.iter(|| black_box(black_box(&empty_reminder).to_prompt()))
    });

    group.finish();
}

/// 边界条件基准测试
fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_cases");

    // 空阶段列表
    let empty_phases: Vec<Phase> = vec![];
    group.bench_function("extract_phase_empty", |b| {
        b.iter(|| black_box(extract_phase_name(black_box(&empty_phases), black_box(0))))
    });

    group.bench_function("extract_task_empty", |b| {
        b.iter(|| {
            black_box(extract_task_name(
                black_box(&empty_phases),
                black_box(&None),
            ))
        })
    });

    // 大量约束 (1000 项)
    let large_constraints: Vec<String> = (0..1000).map(|i| format!("约束{i}")).collect();
    group.bench_function("format_large_constraints", |b| {
        b.iter(|| black_box(format_constraints_section(black_box(&large_constraints))))
    });

    // inject 方法 (不触发提醒)
    let reminder = SteerReminder {
        goal: "test".to_string(),
        current_phase: "阶段A".to_string(),
        current_task: "任务B".to_string(),
        constraints: vec![
            "遵循 SOLID 原则, 特别是 DIP (依赖倒置)".to_string(),
            "核心逻辑依赖 trait 抽象, 不依赖具体类型".to_string(),
            "每个新功能必须有配套的单元测试和集成测试 (TDD)".to_string(),
            "代码必须可编译、可测试".to_string(),
            "用 ```file:路径``` 格式输出所有文件".to_string(),
        ],
        interval: 10,
    };
    let long_msg = "x".repeat(10000);
    group.bench_function("inject_no_remind", |b| {
        b.iter(|| black_box(black_box(&reminder).inject(black_box(5), black_box(&long_msg))))
    });

    group.bench_function("inject_with_remind", |b| {
        b.iter(|| black_box(black_box(&reminder).inject(black_box(10), black_box(&long_msg))))
    });

    // should_remind 方法
    group.bench_function("should_remind_disabled", |b| {
        b.iter(|| {
            let r = SteerReminder {
                goal: String::new(),
                current_phase: String::new(),
                current_task: String::new(),
                constraints: vec![],
                interval: 0,
            };
            black_box(r.should_remind(black_box(100)))
        })
    });

    group.finish();
}

/// 配置基准测试参数
fn configure_criterion() -> Criterion {
    Criterion::default()
        .sample_size(50)
        .measurement_time(std::time::Duration::from_secs(5))
        .warm_up_time(std::time::Duration::from_secs(2))
        .nresamples(50_000)
        .noise_threshold(0.05)
        .output_directory(std::path::Path::new("target/criterion/steer_reminder"))
}

criterion_group! {
    name = steer_reminder_benches;
    config = configure_criterion();
    targets =
        bench_extract_names,
        bench_format_functions,
        bench_check_remind_needed,
        bench_to_prompt,
        bench_edge_cases,
}

criterion_main!(steer_reminder_benches);
