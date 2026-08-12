//! DevTrace 模块性能基准测试
//!
//! 测试目标:
//! 1. DevTraceEntry 构造/序列化/反序列化
//! 2. DevTraceSummary 构建/报告生成/JSON 导出
//! 3. 纯函数: calculate_success_rate, group_entries_by_action, build_timeline, format_timeline_line
//! 4. IncrementalStats / ActionStats 操作
//! 5. 边界条件: 空数据/单条目/大规模/Unicode

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use forge::dev_trace::{
    build_timeline, calculate_success_rate, format_duration_human, format_success_rate_percent,
    format_timeline_line, group_entries_by_action, parse_jsonl_line, ActionStats, DevTraceEntry,
    DevTraceSummary, IncrementalStats, TimelineEntry, TraceAction,
};

/// 构建测试用 DevTraceEntry
fn make_entry(action: TraceAction, success: bool, idx: usize) -> DevTraceEntry {
    DevTraceEntry::new(
        action,
        Some(0),
        Some(idx),
        Some(&format!("task_{}", idx)),
        &format!("输入数据 {}", idx),
        &format!("输出结果 {}", idx),
        1000 + idx as u64,
        success,
        if success { None } else { Some("编译错误") },
    )
}

/// 构建多条目列表
fn make_entries(n: usize) -> Vec<DevTraceEntry> {
    let actions = [
        TraceAction::Planning,
        TraceAction::TaskExecution,
        TraceAction::FixAttempt,
        TraceAction::CompileCheck,
        TraceAction::TestRun,
        TraceAction::Clarification,
        TraceAction::Recovery,
        TraceAction::WebSearch,
        TraceAction::CacheTuning,
        TraceAction::SearchQuality,
    ];
    (0..n)
        .map(|i| make_entry(actions[i % actions.len()], i % 3 != 0, i))
        .collect()
}

/// 基准测试: DevTraceEntry 构造/序列化/反序列化
fn bench_entry_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("entry_operations");

    // 构造
    group.bench_function("new", |b| {
        b.iter(|| {
            for i in 0..100u32 {
                black_box(DevTraceEntry::new(
                    TraceAction::TaskExecution,
                    Some(0),
                    Some(i as usize),
                    Some("task"),
                    "input data",
                    "output result",
                    1000,
                    true,
                    None,
                ));
            }
        })
    });

    // 序列化为 JSONL
    let entry = make_entry(TraceAction::TaskExecution, true, 0);
    group.bench_function("to_jsonl", |b| {
        b.iter(|| black_box(entry.to_jsonl().unwrap()))
    });

    // 从 JSONL 反序列化
    let jsonl = entry.to_jsonl().unwrap();
    group.bench_function("from_jsonl", |b| {
        b.iter(|| black_box(DevTraceEntry::from_jsonl(black_box(&jsonl)).unwrap()))
    });

    // parse_jsonl_line (含空行/无效行)
    let lines: Vec<String> = (0..100)
        .map(|i| {
            if i % 5 == 0 {
                String::new()
            } else if i % 7 == 0 {
                "invalid json".to_string()
            } else {
                make_entry(TraceAction::TaskExecution, true, i)
                    .to_jsonl()
                    .unwrap()
            }
        })
        .collect();
    group.bench_function("parse_jsonl_line_batch", |b| {
        b.iter(|| {
            for line in &lines {
                black_box(parse_jsonl_line(black_box(line)));
            }
        })
    });

    group.finish();
}

/// 基准测试: DevTraceSummary 构建/报告生成/JSON 导出
fn bench_summary_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("summary_operations");

    let sizes = vec![10, 100, 500, 1_000];

    for size in sizes {
        group.throughput(Throughput::Elements(size as u64));
        let entries = make_entries(size);

        // from_entries
        group.bench_with_input(
            BenchmarkId::new("from_entries", size),
            &entries,
            |b, entries| b.iter(|| black_box(DevTraceSummary::from_entries(black_box(entries)))),
        );

        // to_report
        let summary = DevTraceSummary::from_entries(&entries);
        group.bench_with_input(
            BenchmarkId::new("to_report", size),
            &summary,
            |b, summary| b.iter(|| black_box(summary.to_report())),
        );

        // to_json (pretty)
        group.bench_with_input(BenchmarkId::new("to_json", size), &summary, |b, summary| {
            b.iter(|| black_box(summary.to_json().unwrap()))
        });

        // to_json_compact
        group.bench_with_input(
            BenchmarkId::new("to_json_compact", size),
            &summary,
            |b, summary| b.iter(|| black_box(summary.to_json_compact().unwrap())),
        );
    }

    group.finish();
}

/// 基准测试: 纯函数
fn bench_pure_functions(c: &mut Criterion) {
    let mut group = c.benchmark_group("pure_functions");

    // calculate_success_rate
    for &(total, success) in &[(0, 0), (10, 7), (100, 85), (1_000, 950)] {
        group.bench_function(format!("calculate_success_rate_{}", total), |b| {
            b.iter(|| black_box(calculate_success_rate(black_box(total), black_box(success))))
        });
    }

    // format_duration_human
    for &ms in &[0, 1_000, 60_000, 3_600_000, 86_400_000] {
        group.bench_function(format!("format_duration_{}ms", ms), |b| {
            b.iter(|| black_box(format_duration_human(black_box(ms))))
        });
    }

    // format_success_rate_percent
    for &rate in &[0.0, 0.5, 0.75, 1.0] {
        group.bench_function(format!("format_rate_{}", rate), |b| {
            b.iter(|| black_box(format_success_rate_percent(black_box(rate))))
        });
    }

    // group_entries_by_action + build_timeline
    let sizes = vec![10, 100, 500, 1_000];
    for size in sizes {
        let entries = make_entries(size);
        group.throughput(Throughput::Elements(size as u64));

        group.bench_with_input(
            BenchmarkId::new("group_entries_by_action", size),
            &entries,
            |b, entries| b.iter(|| black_box(group_entries_by_action(black_box(entries)))),
        );

        group.bench_with_input(
            BenchmarkId::new("build_timeline", size),
            &entries,
            |b, entries| b.iter(|| black_box(build_timeline(black_box(entries), 100))),
        );
    }

    // format_timeline_line
    let timeline_entry = TimelineEntry {
        timestamp: chrono::Utc::now(),
        action: TraceAction::TaskExecution,
        task_name: Some("测试任务".to_string()),
        success: true,
        duration_ms: 3000,
    };
    group.bench_function("format_timeline_line", |b| {
        b.iter(|| black_box(format_timeline_line(black_box(&timeline_entry))))
    });

    group.finish();
}

/// 基准测试: IncrementalStats / ActionStats 操作
fn bench_stats_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("stats_operations");

    // IncrementalStats
    group.bench_function("incremental_stats_100", |b| {
        b.iter(|| {
            let mut stats = IncrementalStats::new();
            for i in 0..100u32 {
                stats.record(black_box((i + 1) as usize), black_box((i / 3) as usize));
            }
            black_box((
                stats.total_messages,
                stats.sent_messages,
                stats.skipped_messages,
                stats.saved_ratio(),
            ))
        })
    });

    // ActionStats
    group.bench_function("action_stats_100", |b| {
        b.iter(|| {
            let mut stats = ActionStats::new();
            for i in 0..100u32 {
                stats.record(black_box(1000 + i as u64), black_box(i % 3 != 0));
            }
            black_box((
                stats.count,
                stats.success_count,
                stats.total_duration_ms,
                stats.success_rate(),
                stats.avg_duration_ms(),
            ))
        })
    });

    // 批量 IncrementalStats
    for size in [10, 100, 1_000] {
        group.throughput(Throughput::Elements(size as u64));
        group.bench_function(BenchmarkId::new("incremental_batch", size), |b| {
            b.iter(|| {
                let mut stats = IncrementalStats::new();
                for i in 0..size {
                    stats.record(black_box(i + 1), black_box(i / 3));
                }
                black_box(stats.saved_ratio())
            })
        });
    }

    group.finish();
}

/// 边界条件基准测试
fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("dev_trace_edge_cases");

    // 空条目列表
    let empty_entries: Vec<DevTraceEntry> = vec![];
    group.bench_function("empty_entries", |b| {
        b.iter(|| {
            let summary = DevTraceSummary::from_entries(black_box(&empty_entries));
            black_box(summary.to_report());
        })
    });

    // 单条目
    let single = vec![make_entry(TraceAction::Planning, true, 0)];
    group.bench_function("single_entry", |b| {
        b.iter(|| {
            let summary = DevTraceSummary::from_entries(black_box(&single));
            black_box(summary.to_report());
        })
    });

    // 大规模 (2000 条目)
    let large = make_entries(2_000);
    group.bench_function("large_2000_entries", |b| {
        b.iter(|| {
            let summary = DevTraceSummary::from_entries(black_box(&large));
            black_box(summary.to_json_compact().unwrap());
        })
    });

    // Unicode 内容
    let unicode_entry = DevTraceEntry::new(
        TraceAction::TaskExecution,
        Some(0),
        Some(0),
        Some("Unicode任务🎉"),
        "输入: 你好世界 𝕏 ℝ ℂ",
        "输出: 成功 ✓ 失败 ✗",
        500,
        true,
        None,
    );
    let unicode_entries = vec![unicode_entry.clone(); 100];
    group.bench_function("unicode_100_entries", |b| {
        b.iter(|| {
            let summary = DevTraceSummary::from_entries(black_box(&unicode_entries));
            black_box(summary.to_report());
        })
    });

    // parse_jsonl_line 边界
    group.bench_function("parse_empty_line", |b| {
        b.iter(|| black_box(parse_jsonl_line(black_box(""))))
    });
    group.bench_function("parse_whitespace_line", |b| {
        b.iter(|| black_box(parse_jsonl_line(black_box("   "))))
    });
    group.bench_function("parse_invalid_json", |b| {
        b.iter(|| black_box(parse_jsonl_line(black_box("not json at all"))))
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
        .output_directory(std::path::Path::new("target/criterion/dev_trace"))
}

criterion_group! {
    name = dev_trace_benches;
    config = configure_criterion();
    targets =
        bench_entry_operations,
        bench_summary_operations,
        bench_pure_functions,
        bench_stats_operations,
        bench_edge_cases,
}

criterion_main!(dev_trace_benches);
