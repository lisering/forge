#![allow(clippy::useless_vec)]

//! Context Handoff 模块性能基准测试
//!
//! 测试目标:
//! 1. check_compaction_trigger — 上下文压缩触发判断性能
//! 2. format_sections — 各格式化函数性能
//! 3. build_summaries — Phase/Task 摘要构建性能
//! 4. should_trigger_handoff + calculate_tail_preserve — 交接触发+尾部保留性能
//! 5. edge_cases — 边界条件 (truncate/boundary/badge/is_workspace_file)

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use forge::context_handoff::{
    self, build_phase_summary, build_task_summary, calculate_tail_preserve,
    check_compaction_trigger, collect_completed_tasks, format_completed_tasks_section,
    format_error_code_badge, format_error_history_section, format_known_issues_section,
    format_phase_section, format_recent_errors_section, format_task_section,
    format_workspace_files_section, is_user_message_boundary, is_workspace_file_included,
    should_trigger_handoff, truncate_text, CompactionConfig, CompactionTrigger, ErrorSummary,
    FileSummary, PhaseSummary, TaskSummary,
};
use forge::memory::{Phase, PhaseStatus, Task, TaskStatus};

/// 构建测试用 Phase
fn make_phase(id: usize, task_count: usize) -> Phase {
    Phase {
        id,
        name: format!("阶段{id}"),
        description: format!("阶段 {id} 的描述文本"),
        status: PhaseStatus::InProgress,
        tasks: (0..task_count)
            .map(|i| Task {
                id: format!("{id}-{i}"),
                phase_id: id,
                name: format!("任务{i}"),
                prompt: format!("执行任务{i}"),
                status: if i % 3 == 0 {
                    TaskStatus::Completed
                } else {
                    TaskStatus::Pending
                },
                result: Some(format!("任务{i}完成")),
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

/// 基准测试: check_compaction_trigger
fn bench_check_compaction_trigger(c: &mut Criterion) {
    let mut group = c.benchmark_group("check_compaction_trigger");

    let config = CompactionConfig::default();

    // 各种 token 使用率
    let cases: Vec<(&str, usize, usize)> = vec![
        ("low_usage", 50000, 100000),
        ("mid_usage", 70000, 100000),
        ("near_soft", 85000, 100000),
        ("above_soft", 86000, 100000),
        ("near_hard", 92000, 100000),
        ("at_hard", 95000, 100000),
        ("over_limit", 110000, 100000),
        ("zero_window", 100, 0),
        ("small_context", 100, 200),
    ];

    for (name, current, window) in &cases {
        let config = config.clone();
        group.bench_function(name.to_string(), move |b| {
            b.iter(|| {
                black_box(check_compaction_trigger(
                    black_box(*current),
                    black_box(*window),
                    black_box(&config),
                ))
            })
        });
    }

    // 自定义配置
    let custom_config = CompactionConfig {
        soft_threshold_pct: 0.70,
        hard_threshold_tokens: 4096,
        tail_preserve_tokens: 30000,
        min_tokens_for_soft_trigger: 1024,
    };
    group.bench_function("custom_config", |b| {
        b.iter(|| {
            black_box(check_compaction_trigger(
                black_box(75000),
                black_box(100000),
                black_box(&custom_config),
            ))
        })
    });

    group.finish();
}

/// 基准测试: format_sections (各格式化函数)
fn bench_format_sections(c: &mut Criterion) {
    let mut group = c.benchmark_group("format_sections");

    // format_phase_section
    let phase = PhaseSummary {
        name: "功能实现".to_string(),
        description: "实现核心功能模块".to_string(),
        status: "InProgress".to_string(),
        task_count: 10,
        completed_count: 5,
    };
    group.bench_function("phase_section", |b| {
        b.iter(|| black_box(format_phase_section(black_box(&phase))))
    });

    // format_task_section
    let task = TaskSummary {
        id: "0-1".to_string(),
        name: "实现加法运算".to_string(),
        status: "Completed".to_string(),
        result: Some("加法运算实现完成".to_string()),
        files_written: vec!["src/add.rs".to_string(), "tests/add_test.rs".to_string()],
        attempts: 2,
    };
    group.bench_function("task_section", |b| {
        b.iter(|| black_box(format_task_section(black_box(&task))))
    });

    // format_completed_tasks_section
    let task_sizes: Vec<usize> = vec![0, 5, 10, 50, 200];
    for &size in &task_sizes {
        group.throughput(Throughput::Elements(size as u64));
        let tasks: Vec<TaskSummary> = (0..size)
            .map(|i| TaskSummary {
                id: format!("0-{i}"),
                name: format!("任务{i}"),
                status: "Completed".to_string(),
                result: None,
                files_written: vec![format!("src/file{i}.rs")],
                attempts: 1,
            })
            .collect();

        group.bench_with_input(
            BenchmarkId::new("completed_tasks", size),
            &tasks,
            |b, tasks| b.iter(|| black_box(format_completed_tasks_section(black_box(tasks)))),
        );
    }

    // format_workspace_files_section
    let file_sizes: Vec<usize> = vec![0, 5, 20, 100];
    for &size in &file_sizes {
        group.throughput(Throughput::Elements(size as u64));
        let files: Vec<FileSummary> = (0..size)
            .map(|i| FileSummary {
                path: format!("src/module{i}/file.rs"),
                size: 1024 * (i + 1),
            })
            .collect();

        group.bench_with_input(
            BenchmarkId::new("workspace_files", size),
            &files,
            |b, files| b.iter(|| black_box(format_workspace_files_section(black_box(files)))),
        );
    }

    // format_recent_errors_section
    let errors: Vec<ErrorSummary> = (0..10)
        .map(|i| ErrorSummary {
            file: format!("src/file{i}.rs"),
            message: format!("error: mismatched types at line {i}"),
            error_code: Some(format!("E{i:04}")),
        })
        .collect();
    group.bench_function("recent_errors", |b| {
        b.iter(|| black_box(format_recent_errors_section(black_box(&errors))))
    });

    // format_error_history_section + format_known_issues_section
    let history_summary =
        "  1. [TypeMismatch] E0308 (出现15次, 已修复)\n  2. [BorrowError] E0502 (出现8次, 未修复)";
    group.bench_function("error_history_section", |b| {
        b.iter(|| black_box(format_error_history_section(black_box(history_summary))))
    });

    let known_issues = "待完成任务 (3):\n  - [0-1] 任务A\n  - [0-2] 任务B\n  - [0-3] 任务C";
    group.bench_function("known_issues_section", |b| {
        b.iter(|| black_box(format_known_issues_section(black_box(known_issues))))
    });

    group.finish();
}

/// 基准测试: build_summaries (Phase/Task 摘要构建)
fn bench_build_summaries(c: &mut Criterion) {
    let mut group = c.benchmark_group("build_summaries");

    let sizes: Vec<usize> = vec![1, 5, 20, 100];

    for &size in &sizes {
        group.throughput(Throughput::Elements(size as u64));
        let phases: Vec<Phase> = (0..size).map(|i| make_phase(i, 5)).collect();

        // build_phase_summary
        group.bench_with_input(
            BenchmarkId::new("phase_summary", size),
            &phases,
            |b, phases| b.iter(|| black_box(build_phase_summary(black_box(&phases[0])))),
        );

        // build_task_summary
        group.bench_with_input(
            BenchmarkId::new("task_summary", size),
            &phases,
            |b, phases| b.iter(|| black_box(build_task_summary(black_box(&phases[0].tasks[0])))),
        );

        // collect_completed_tasks
        group.bench_with_input(
            BenchmarkId::new("collect_completed", size),
            &phases,
            |b, phases| b.iter(|| black_box(collect_completed_tasks(black_box(phases)))),
        );
    }
    group.finish();
}

/// 基准测试: should_trigger_handoff + calculate_tail_preserve
fn bench_handoff_triggers(c: &mut Criterion) {
    let mut group = c.benchmark_group("handoff_triggers");

    // should_trigger_handoff
    let thresholds: Vec<usize> = vec![10, 30, 100];
    for &threshold in &thresholds {
        for &count in &[0, 5, 10, 15, 50, 100] {
            group.bench_function(format!("trigger/t{threshold}_c{count}"), |b| {
                b.iter(|| {
                    black_box(should_trigger_handoff(
                        black_box(count),
                        black_box(threshold),
                    ))
                })
            });
        }
    }

    // calculate_tail_preserve
    let window_sizes: Vec<usize> = vec![10000, 100000, 1000000, 10000000];
    let max_tails: Vec<usize> = vec![50000, 100000];

    for &window in &window_sizes {
        for &max_tail in &max_tails {
            group.bench_function(format!("tail/w{window}_m{max_tail}"), |b| {
                b.iter(|| {
                    black_box(calculate_tail_preserve(
                        black_box(window),
                        black_box(max_tail),
                    ))
                })
            });
        }
    }

    group.finish();
}

/// 边界条件基准测试
fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_cases");

    // truncate_text
    let text_sizes: Vec<usize> = vec![10, 100, 1000, 10000];
    for &size in &text_sizes {
        let text = "x".repeat(size);
        group.bench_function(format!("truncate/{size}_half"), |b| {
            b.iter(|| black_box(truncate_text(black_box(&text), black_box(size / 2))))
        });
    }

    // truncate_text — Unicode
    let unicode_text = "你好世界测试".repeat(100);
    group.bench_function("truncate_unicode", |b| {
        b.iter(|| black_box(truncate_text(black_box(&unicode_text), black_box(50))))
    });

    // is_user_message_boundary
    let boundary_cases: Vec<(&str, &str)> = vec![
        ("zh_user", "用户: 请帮我创建一个计算器"),
        ("en_user", "User: Create a calculator"),
        ("assistant", "助手: 好的, 我来创建"),
        ("empty", ""),
        ("whitespace", "  用户: test"),
    ];
    for (name, msg) in &boundary_cases {
        group.bench_function(format!("user_boundary/{name}"), |b| {
            b.iter(|| black_box(is_user_message_boundary(black_box(msg))))
        });
    }

    // format_error_code_badge
    group.bench_function("badge_some", |b| {
        b.iter(|| black_box(format_error_code_badge(black_box(Some("E0308")))))
    });
    group.bench_function("badge_none", |b| {
        b.iter(|| black_box(format_error_code_badge(black_box(None))))
    });

    // is_workspace_file_included
    let path_cases: Vec<(&str, &str)> = vec![
        ("src", "src/main.rs"),
        ("target", "target/debug/output"),
        ("forge", ".forge/logs/trace.json"),
        ("root", "Cargo.toml"),
        ("empty", ""),
    ];
    for (name, path) in &path_cases {
        group.bench_function(format!("workspace_file/{name}"), |b| {
            b.iter(|| black_box(is_workspace_file_included(black_box(path))))
        });
    }

    // build_compaction_summary_prompt (返回静态字符串)
    group.bench_function("compaction_prompt", |b| {
        b.iter(|| black_box(context_handoff::build_compaction_summary_prompt()))
    });

    // 空列表格式化
    let empty_tasks: Vec<TaskSummary> = vec![];
    group.bench_function("format_empty_completed", |b| {
        b.iter(|| black_box(format_completed_tasks_section(black_box(&empty_tasks))))
    });

    let empty_files: Vec<FileSummary> = vec![];
    group.bench_function("format_empty_files", |b| {
        b.iter(|| black_box(format_workspace_files_section(black_box(&empty_files))))
    });

    let empty_errors: Vec<ErrorSummary> = vec![];
    group.bench_function("format_empty_errors", |b| {
        b.iter(|| black_box(format_recent_errors_section(black_box(&empty_errors))))
    });

    // CompactionTrigger 变体
    let _ = CompactionTrigger::Soft;
    group.bench_function("trigger_variant_access", |b| {
        b.iter(|| black_box(CompactionTrigger::None))
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
        .output_directory(std::path::Path::new("target/criterion/context_handoff"))
}

criterion_group! {
    name = context_handoff_benches;
    config = configure_criterion();
    targets =
        bench_check_compaction_trigger,
        bench_format_sections,
        bench_build_summaries,
        bench_handoff_triggers,
        bench_edge_cases,
}

criterion_main!(context_handoff_benches);
