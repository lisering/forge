#![allow(clippy::useless_vec)]

//! 搜索质量评估模块性能基准测试
//!
//! 测试目标:
//! 1. has_sufficient_search_data - 数据充分性检查性能
//! 2. should_disable_search - 禁用决策性能
//! 3. compute_search_quality_decision - 完整质量决策性能
//! 4. evaluate_and_apply - 评估器一步评估性能
//! 5. edge_cases - 边界条件性能

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use forge::dev_trace::SearchQualityStats;
use forge::search_quality;

/// 构建测试用 SearchQualityStats
fn build_stats(
    with_checks: usize,
    with_success_rate: f64,
    without_checks: usize,
    without_success_rate: f64,
) -> SearchQualityStats {
    let mut stats = SearchQualityStats::new();
    let with_successes = (with_checks as f64 * with_success_rate) as usize;
    for i in 0..with_checks {
        stats.record_with_search(i < with_successes);
    }
    let without_successes = (without_checks as f64 * without_success_rate) as usize;
    for i in 0..without_checks {
        stats.record_without_search(i < without_successes);
    }
    stats
}

/// 基准测试: has_sufficient_search_data
fn bench_has_sufficient_data(c: &mut Criterion) {
    let mut group = c.benchmark_group("has_sufficient_search_data");

    let test_cases = vec![
        ("empty", 0, 0, 5),
        ("partial", 2, 1, 5),
        ("sufficient", 3, 3, 5),
        ("large", 100, 100, 5),
    ];

    for (name, with, without, min) in &test_cases {
        let stats = build_stats(*with, 0.5, *without, 0.5);
        let min_val = *min;
        group.bench_function(*name, |b| {
            b.iter(|| {
                black_box(search_quality::has_sufficient_search_data(
                    black_box(&stats),
                    black_box(min_val),
                ))
            })
        });
    }
    group.finish();
}

/// 基准测试: should_disable_search
fn bench_should_disable_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("should_disable_search");

    let configs = vec![
        ("default", search_quality::SearchQualityConfig::default()),
        ("strict", search_quality::SearchQualityConfig::strict()),
        ("lenient", search_quality::SearchQualityConfig::lenient()),
    ];

    let stats_harmful = build_stats(50, 0.2, 50, 0.8);
    let stats_beneficial = build_stats(50, 0.8, 50, 0.2);
    let stats_neutral = build_stats(50, 0.5, 50, 0.5);

    for (name, config) in &configs {
        let cfg = config.clone();
        let stats = stats_harmful.clone();
        group.bench_function(format!("{name}/harmful"), move |b| {
            b.iter(|| {
                black_box(search_quality::should_disable_search(
                    black_box(&stats),
                    black_box(&cfg),
                ))
            })
        });

        let cfg = config.clone();
        let stats = stats_beneficial.clone();
        group.bench_function(format!("{name}/beneficial"), move |b| {
            b.iter(|| {
                black_box(search_quality::should_disable_search(
                    black_box(&stats),
                    black_box(&cfg),
                ))
            })
        });

        let cfg = config.clone();
        let stats = stats_neutral.clone();
        group.bench_function(format!("{name}/neutral"), move |b| {
            b.iter(|| {
                black_box(search_quality::should_disable_search(
                    black_box(&stats),
                    black_box(&cfg),
                ))
            })
        });
    }
    group.finish();
}

/// 基准测试: compute_search_quality_decision
fn bench_compute_decision(c: &mut Criterion) {
    let mut group = c.benchmark_group("compute_search_quality_decision");

    let sizes: Vec<usize> = vec![5, 50, 500, 5_000];
    let config = search_quality::SearchQualityConfig::default();

    for size in &sizes {
        group.throughput(Throughput::Elements(*size as u64));
        let stats = build_stats(*size, 0.3, *size, 0.7);
        let cfg = config.clone();

        group.bench_function(format!("harmful_{}", size), move |b| {
            b.iter(|| {
                black_box(search_quality::compute_search_quality_decision(
                    black_box(&stats),
                    black_box(&cfg),
                ))
            })
        });
    }

    for size in &sizes {
        group.throughput(Throughput::Elements(*size as u64));
        let stats = build_stats(*size, 0.7, *size, 0.3);
        let cfg = config.clone();

        group.bench_function(format!("beneficial_{}", size), move |b| {
            b.iter(|| {
                black_box(search_quality::compute_search_quality_decision(
                    black_box(&stats),
                    black_box(&cfg),
                ))
            })
        });
    }
    group.finish();
}

/// 基准测试: evaluate_and_apply (完整评估流程)
fn bench_evaluate_and_apply(c: &mut Criterion) {
    let mut group = c.benchmark_group("evaluate_and_apply");

    let sizes: Vec<usize> = vec![5, 50, 500];
    let config = search_quality::SearchQualityConfig::default();

    for size in &sizes {
        group.throughput(Throughput::Elements(*size as u64));
        let stats = build_stats(*size, 0.4, *size, 0.6);
        let cfg = config.clone();

        group.bench_function(format!("eval_{}", size), move |b| {
            b.iter_batched(
                || search_quality::SearchQualityEvaluator::new(cfg.clone()),
                |mut evaluator| black_box(evaluator.evaluate_and_apply(black_box(&stats))),
                criterion::BatchSize::SmallInput,
            )
        });
    }
    group.finish();
}

/// 边界条件基准测试
fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_cases");

    let config = search_quality::SearchQualityConfig::default();

    // 空统计
    let empty_stats = SearchQualityStats::new();
    group.bench_function("empty_stats", |b| {
        b.iter(|| {
            black_box(search_quality::compute_search_quality_decision(
                black_box(&empty_stats),
                black_box(&config),
            ))
        })
    });

    // 极小样本 (1 with, 1 without)
    let tiny_stats = build_stats(1, 1.0, 1, 0.0);
    group.bench_function("tiny_sample", |b| {
        b.iter(|| {
            black_box(search_quality::compute_search_quality_decision(
                black_box(&tiny_stats),
                black_box(&config),
            ))
        })
    });

    // 完全相同修复率
    let identical_stats = build_stats(100, 0.5, 100, 0.5);
    group.bench_function("identical_rates", |b| {
        b.iter(|| {
            black_box(search_quality::compute_search_quality_decision(
                black_box(&identical_stats),
                black_box(&config),
            ))
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
        .output_directory(std::path::Path::new("target/criterion/search_quality"))
}

criterion_group! {
    name = search_quality_benches;
    config = configure_criterion();
    targets =
        bench_has_sufficient_data,
        bench_should_disable_search,
        bench_compute_decision,
        bench_evaluate_and_apply,
        bench_edge_cases,
}

criterion_main!(search_quality_benches);
