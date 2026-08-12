//! HTML 报告生成器性能基准测试
//!
//! 测试目标:
//! 1. Chart.js 图表生成 (line/bar/doughnut/gantt/line_raw/line_colored)
//! 2. HTML 报告生成 (generate_html_report + CSS + stat_card)
//! 3. 数据导出 (CSV/JSON: timeline + action_stats)
//! 4. 工具函数 (doughnut_colors, point_colors, extract_gantt_data, csv_escape_field)
//! 5. 边界条件: 空摘要/大规模/Unicode

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use forge::dev_trace::{ActionStats, DevTraceEntry, DevTraceSummary, TraceAction};
use forge::html_report::{
    csv_escape_field, extract_gantt_data, generate_action_stats_csv, generate_action_stats_json,
    generate_chart_js_bar, generate_chart_js_doughnut, generate_chart_js_gantt,
    generate_chart_js_line, generate_chart_js_line_colored, generate_chart_js_line_raw,
    generate_css_styles, generate_doughnut_colors, generate_html_report, generate_point_colors,
    generate_stat_card, generate_timeline_csv, generate_timeline_json,
};

use std::collections::HashMap;

/// 构建测试用 DevTraceEntry
fn make_entry(action: TraceAction, success: bool, idx: usize) -> DevTraceEntry {
    DevTraceEntry::new(
        action,
        Some(0),
        Some(idx),
        Some(&format!("task_{}", idx)),
        "input",
        "output",
        1000 + idx as u64,
        success,
        None,
    )
}

/// 构建测试用 DevTraceSummary
fn make_summary(entries: usize) -> DevTraceSummary {
    let actions = [
        TraceAction::TaskExecution,
        TraceAction::FixAttempt,
        TraceAction::CompileCheck,
        TraceAction::TestRun,
        TraceAction::Planning,
    ];
    let entry_list: Vec<DevTraceEntry> = (0..entries)
        .map(|i| make_entry(actions[i % actions.len()], i % 3 != 0, i))
        .collect();
    DevTraceSummary::from_entries(&entry_list)
}

/// 基准测试: Chart.js 图表生成
fn bench_chart_generation(c: &mut Criterion) {
    let mut group = c.benchmark_group("chart_generation");

    // 折线图
    let labels: Vec<String> = (0..50).map(|i| format!("S{}", i)).collect();
    let data: Vec<f64> = (0..50).map(|i| 0.5 + 0.01 * i as f64).collect();
    group.bench_function("chart_js_line_50", |b| {
        b.iter(|| {
            black_box(generate_chart_js_line(
                black_box("lineChart"),
                black_box("评分趋势"),
                black_box(&labels),
                black_box(&data),
                black_box("rgba(75, 192, 192, 1)"),
                black_box("评分"),
            ))
        })
    });

    // 柱状图
    let bar_data: Vec<f64> = (0..20).map(|i| (i + 1) as f64 * 5.0).collect();
    let bar_labels: Vec<String> = (0..20).map(|i| format!("类型{}", i)).collect();
    group.bench_function("chart_js_bar_20", |b| {
        b.iter(|| {
            black_box(generate_chart_js_bar(
                black_box("barChart"),
                black_box("操作统计"),
                black_box(&bar_labels),
                black_box(&bar_data),
                black_box("rgba(54, 162, 235, 0.8)"),
                black_box("次数"),
            ))
        })
    });

    // Doughnut 图
    let d_labels = vec!["命中".to_string(), "未命中".to_string(), "失败".to_string()];
    let d_data = vec![60.0, 30.0, 10.0];
    group.bench_function("chart_js_doughnut", |b| {
        b.iter(|| {
            black_box(generate_chart_js_doughnut(
                black_box("cachePie"),
                black_box("缓存命中率"),
                black_box(&d_labels),
                black_box(&d_data),
                black_box(&[]),
            ))
        })
    });

    // 甘特图
    let g_labels: Vec<String> = (0..10).map(|i| format!("Task {}", i)).collect();
    let g_data: Vec<Vec<f64>> = (0..10)
        .map(|i| vec![i as f64 * 5000.0, (i + 1) as f64 * 5000.0])
        .collect();
    group.bench_function("chart_js_gantt_10", |b| {
        b.iter(|| {
            black_box(generate_chart_js_gantt(
                black_box("ganttChart"),
                black_box("时间线甘特图"),
                black_box(&g_labels),
                black_box(&g_data),
                black_box(&[]),
            ))
        })
    });

    // line_raw (自动Y轴)
    group.bench_function("chart_js_line_raw_50", |b| {
        b.iter(|| {
            black_box(generate_chart_js_line_raw(
                black_box("rawChart"),
                black_box("TTL趋势"),
                black_box(&labels),
                black_box(&data),
                black_box("rgba(54, 162, 235, 1)"),
                black_box("秒"),
            ))
        })
    });

    // line_colored (颜色编码点)
    let mixed_data: Vec<f64> = (0..50)
        .map(|i| {
            if i % 2 == 0 {
                0.1 * i as f64
            } else {
                -0.1 * i as f64
            }
        })
        .collect();
    group.bench_function("chart_js_line_colored_50", |b| {
        b.iter(|| {
            black_box(generate_chart_js_line_colored(
                black_box("colorChart"),
                black_box("差值趋势"),
                black_box(&labels),
                black_box(&mixed_data),
                black_box("rgba(75, 192, 192, 1)"),
                black_box("差值"),
            ))
        })
    });

    group.finish();
}

/// 基准测试: HTML 报告生成
fn bench_report_generation(c: &mut Criterion) {
    let mut group = c.benchmark_group("report_generation");

    let sizes = vec![10, 100, 500];

    for size in sizes {
        group.throughput(Throughput::Elements(size as u64));
        let summary = make_summary(size);

        // generate_html_report (完整 HTML)
        group.bench_with_input(
            BenchmarkId::new("generate_html_report", size),
            &summary,
            |b, summary| b.iter(|| black_box(generate_html_report(black_box(summary)))),
        );
    }

    // CSS 样式
    group.bench_function("generate_css_styles", |b| {
        b.iter(|| black_box(generate_css_styles()))
    });

    // 统计卡片
    group.bench_function("generate_stat_card", |b| {
        b.iter(|| {
            black_box(generate_stat_card(
                black_box("总条目数"),
                black_box("12345"),
                black_box(None),
                black_box("blue"),
            ))
        })
    });

    group.finish();
}

/// 基准测试: 数据导出 (CSV/JSON)
fn bench_data_export(c: &mut Criterion) {
    let mut group = c.benchmark_group("data_export");

    let sizes = vec![10, 100, 500];

    for size in sizes {
        group.throughput(Throughput::Elements(size as u64));
        let summary = make_summary(size);

        // timeline CSV
        group.bench_with_input(
            BenchmarkId::new("timeline_csv", size),
            &summary,
            |b, summary| b.iter(|| black_box(generate_timeline_csv(black_box(&summary.timeline)))),
        );

        // timeline JSON
        group.bench_with_input(
            BenchmarkId::new("timeline_json", size),
            &summary,
            |b, summary| b.iter(|| black_box(generate_timeline_json(black_box(&summary.timeline)))),
        );

        // action_stats CSV
        group.bench_with_input(
            BenchmarkId::new("action_stats_csv", size),
            &summary,
            |b, summary| {
                b.iter(|| black_box(generate_action_stats_csv(black_box(&summary.by_action))))
            },
        );

        // action_stats JSON
        group.bench_with_input(
            BenchmarkId::new("action_stats_json", size),
            &summary,
            |b, summary| {
                b.iter(|| black_box(generate_action_stats_json(black_box(&summary.by_action))))
            },
        );
    }

    // csv_escape_field
    let test_fields = vec![
        "simple",
        "with,comma",
        "with\"quote",
        "with\nnewline",
        "混合,中英文\"引号\n换行",
        "no special chars at all just a long string for testing",
    ];
    group.bench_function("csv_escape_batch", |b| {
        b.iter(|| {
            for field in &test_fields {
                black_box(csv_escape_field(black_box(field)));
            }
        })
    });

    group.finish();
}

/// 基准测试: 工具函数
fn bench_utility_functions(c: &mut Criterion) {
    let mut group = c.benchmark_group("utility_functions");

    // generate_doughnut_colors
    for &count in &[3, 10, 50, 100] {
        group.bench_function(BenchmarkId::new("doughnut_colors", count), |b| {
            b.iter(|| black_box(generate_doughnut_colors(black_box(count))))
        });
    }

    // generate_point_colors
    let pos_data: Vec<f64> = (0..100).map(|i| 0.1 * i as f64).collect();
    let neg_data: Vec<f64> = (0..100).map(|i| -0.1 * i as f64).collect();
    let mixed_data: Vec<f64> = (0..100)
        .map(|i| {
            if i % 2 == 0 {
                0.1 * i as f64
            } else {
                -0.1 * i as f64
            }
        })
        .collect();

    group.bench_function("point_colors_positive_100", |b| {
        b.iter(|| black_box(generate_point_colors(black_box(&pos_data))))
    });
    group.bench_function("point_colors_negative_100", |b| {
        b.iter(|| black_box(generate_point_colors(black_box(&neg_data))))
    });
    group.bench_function("point_colors_mixed_100", |b| {
        b.iter(|| black_box(generate_point_colors(black_box(&mixed_data))))
    });

    // extract_gantt_data (需要 TimelineEntry, 不是 DevTraceEntry)
    let timeline_entries: Vec<forge::dev_trace::TimelineEntry> = (0..100)
        .map(|i| {
            let entry = make_entry(TraceAction::TaskExecution, i % 3 != 0, i);
            forge::dev_trace::TimelineEntry::from_entry(&entry)
        })
        .collect();
    group.bench_function("extract_gantt_data_100", |b| {
        b.iter(|| black_box(extract_gantt_data(black_box(&timeline_entries))))
    });

    group.finish();
}

/// 边界条件基准测试
fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("html_report_edge_cases");

    // 空摘要
    let empty_summary = DevTraceSummary::empty();
    group.bench_function("empty_summary", |b| {
        b.iter(|| black_box(generate_html_report(black_box(&empty_summary))))
    });

    // 空数据图表
    let empty_labels: Vec<String> = vec![];
    let empty_data: Vec<f64> = vec![];
    group.bench_function("empty_chart_line", |b| {
        b.iter(|| {
            black_box(generate_chart_js_line(
                black_box("empty"),
                black_box("空"),
                black_box(&empty_labels),
                black_box(&empty_data),
                black_box("rgba(0,0,0,1)"),
                black_box("Y"),
            ))
        })
    });
    group.bench_function("empty_chart_doughnut", |b| {
        b.iter(|| {
            black_box(generate_chart_js_doughnut(
                black_box("emptyD"),
                black_box("空"),
                black_box(&empty_labels),
                black_box(&empty_data),
                black_box(&[]),
            ))
        })
    });

    // 大规模摘要 (1000 条目)
    let large_summary = make_summary(1_000);
    group.bench_function("large_1000_summary", |b| {
        b.iter(|| black_box(generate_html_report(black_box(&large_summary))))
    });

    // 空时间线 CSV/JSON
    group.bench_function("empty_timeline_csv", |b| {
        b.iter(|| black_box(generate_timeline_csv(black_box(&[]))))
    });
    group.bench_function("empty_timeline_json", |b| {
        b.iter(|| black_box(generate_timeline_json(black_box(&[]))))
    });

    // 空 by_action CSV/JSON
    let empty_by_action: HashMap<TraceAction, ActionStats> = HashMap::new();
    group.bench_function("empty_action_stats_csv", |b| {
        b.iter(|| black_box(generate_action_stats_csv(black_box(&empty_by_action))))
    });
    group.bench_function("empty_action_stats_json", |b| {
        b.iter(|| black_box(generate_action_stats_json(black_box(&empty_by_action))))
    });

    // csv_escape_field 边界
    group.bench_function("csv_escape_empty", |b| {
        b.iter(|| black_box(csv_escape_field(black_box(""))))
    });
    group.bench_function("csv_escape_unicode", |b| {
        b.iter(|| black_box(csv_escape_field(black_box("你好,世界\"引号\n换行🎉"))))
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
        .output_directory(std::path::Path::new("target/criterion/html_report"))
}

criterion_group! {
    name = html_report_benches;
    config = configure_criterion();
    targets =
        bench_chart_generation,
        bench_report_generation,
        bench_data_export,
        bench_utility_functions,
        bench_edge_cases,
}

criterion_main!(html_report_benches);
