#![allow(clippy::useless_vec)]

//! DevTrace 分析引擎性能基准测试
//!
//! 测试目标:
//! 1. compute_health_score - 健康度评分计算性能
//! 2. generate_recommendations - 建议生成性能
//! 3. analyze_dev_trace_summary - 综合分析性能
//! 4. generate_analysis_report - Markdown 报告生成性能

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use forge::dev_trace::{
    CacheFixCorrelation, CacheStatsSummary, DevTraceSummary, IncrementalStats, SearchQualityStats,
};
use forge::dev_trace_analyzer;

/// 构建最小 DevTraceSummary (仅基本字段)
fn build_minimal_summary(entries: usize) -> DevTraceSummary {
    DevTraceSummary {
        total_entries: entries,
        total_duration_ms: entries as u64 * 5000,
        by_action: std::collections::HashMap::new(),
        success_rate: 0.75,
        timeline: vec![],
        incremental_summary: None,
        cache_summary: None,
        cache_fix_correlation: None,
        cache_tuning_summary: None,
        search_quality_summary: None,
        search_quality_history_summary: None,
        cache_tuning_history_summary: None,
        memory_evaluation_summary: None,
        memory_evaluation_history_summary: None,
        evaluator_synergy_summary: None,
        evaluator_synergy_history_summary: None,
        synergy_score_history: None,
        fix_rate_history: None,
        search_diff_history: None,
        memory_diff_history: None,
        ttl_history_values: None,
        correlation_diff_history: None,
        health_score_history_summary: None,
        joint_decision_history_summary: None,
    }
}

/// 构建完整 DevTraceSummary (所有可选字段填充)
fn build_full_summary(entries: usize) -> DevTraceSummary {
    let mut summary = build_minimal_summary(entries);
    summary.success_rate = 0.68;

    // 缓存统计
    summary.cache_summary = Some(CacheStatsSummary {
        cache_hits: (entries as u32) / 3,
        cache_misses: (entries as u32) / 4,
        search_failures: (entries as u32) / 10,
        time_saved_ms: entries as u64 * 120,
    });

    // 缓存修复关联
    let mut corr = CacheFixCorrelation::new();
    for i in 0..entries {
        let success = i % 3 != 0;
        if i % 2 == 0 {
            corr.record_hit_check(success);
        } else {
            corr.record_miss_check(success);
        }
    }
    summary.cache_fix_correlation = Some(corr);

    // 搜索质量
    summary.search_quality_summary = Some(SearchQualityStats {
        checks_with_search: entries / 2,
        successes_with_search: entries / 3,
        checks_without_search: entries / 2,
        successes_without_search: entries / 4,
        total_searches: entries / 2,
        successful_searches: entries / 3,
        failed_searches: entries / 6,
    });

    // 增量发送
    summary.incremental_summary = Some(IncrementalStats::new());

    summary
}

/// 基准测试: compute_health_score
fn bench_compute_health_score(c: &mut Criterion) {
    let mut group = c.benchmark_group("compute_health_score");

    let sizes = vec![10, 100, 1_000, 10_000];

    for size in sizes {
        group.throughput(Throughput::Elements(size as u64));

        // 最小 summary
        let min_summary = build_minimal_summary(size);
        group.bench_with_input(
            BenchmarkId::new("minimal", size),
            &min_summary,
            |b, summary| {
                b.iter(|| black_box(dev_trace_analyzer::compute_health_score(black_box(summary))))
            },
        );

        // 完整 summary
        let full_summary = build_full_summary(size);
        group.bench_with_input(
            BenchmarkId::new("full", size),
            &full_summary,
            |b, summary| {
                b.iter(|| black_box(dev_trace_analyzer::compute_health_score(black_box(summary))))
            },
        );
    }
    group.finish();
}

/// 基准测试: generate_recommendations
fn bench_generate_recommendations(c: &mut Criterion) {
    let mut group = c.benchmark_group("generate_recommendations");

    let sizes = vec![10, 100, 1_000, 10_000];

    for size in sizes {
        group.throughput(Throughput::Elements(size as u64));
        let summary = build_full_summary(size);

        group.bench_with_input(
            BenchmarkId::new("recommendations", size),
            &summary,
            |b, summary| {
                b.iter(|| {
                    black_box(dev_trace_analyzer::generate_recommendations(black_box(
                        summary,
                    )))
                })
            },
        );
    }
    group.finish();
}

/// 基准测试: analyze_dev_trace_summary (完整分析流水线)
fn bench_analyze_dev_trace_summary(c: &mut Criterion) {
    let mut group = c.benchmark_group("analyze_dev_trace_summary");

    let sizes = vec![10, 100, 1_000, 10_000];

    for size in sizes {
        group.throughput(Throughput::Elements(size as u64));
        let summary = build_full_summary(size);

        group.bench_with_input(BenchmarkId::new("analyze", size), &summary, |b, summary| {
            b.iter(|| {
                black_box(dev_trace_analyzer::analyze_dev_trace_summary(black_box(
                    summary,
                )))
            })
        });
    }
    group.finish();
}

/// 基准测试: generate_analysis_report (Markdown 生成)
fn bench_generate_analysis_report(c: &mut Criterion) {
    let mut group = c.benchmark_group("generate_analysis_report");

    let sizes = vec![10, 100, 1_000, 10_000];

    for size in sizes {
        group.throughput(Throughput::Elements(size as u64));
        let summary = build_full_summary(size);
        let analysis = dev_trace_analyzer::analyze_dev_trace_summary(&summary);

        group.bench_with_input(
            BenchmarkId::new("report", size),
            &analysis,
            |b, analysis| {
                b.iter(|| {
                    black_box(dev_trace_analyzer::generate_analysis_report(black_box(
                        analysis,
                    )))
                })
            },
        );
    }
    group.finish();
}

/// 边界条件基准测试
fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("analyzer_edge_cases");

    // 空数据 (0 条目)
    let empty_summary = build_minimal_summary(0);
    group.bench_function("empty_summary", |b| {
        b.iter(|| {
            black_box(dev_trace_analyzer::analyze_dev_trace_summary(black_box(
                &empty_summary,
            )))
        })
    });

    // 单条目
    let single_summary = build_minimal_summary(1);
    group.bench_function("single_entry", |b| {
        b.iter(|| {
            black_box(dev_trace_analyzer::analyze_dev_trace_summary(black_box(
                &single_summary,
            )))
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
        .output_directory(std::path::Path::new("target/criterion/dev_trace_analyzer"))
}

criterion_group! {
    name = dev_trace_analyzer_benches;
    config = configure_criterion();
    targets =
        bench_compute_health_score,
        bench_generate_recommendations,
        bench_analyze_dev_trace_summary,
        bench_generate_analysis_report,
        bench_edge_cases,
}

criterion_main!(dev_trace_analyzer_benches);
