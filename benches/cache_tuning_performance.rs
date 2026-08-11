#![allow(clippy::useless_vec)]

//! 缓存调优模块性能基准测试
//!
//! 测试目标:
//! 1. should_disable_cache - 禁用决策性能
//! 2. compute_new_ttl - TTL 计算性能
//! 3. make_tuning_decision - 完整调优决策性能
//! 4. extract_ttl_trajectory - TTL 轨迹提取性能
//! 5. extract_correlation_diffs - 关联差值提取性能

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use forge::cache_tuning;
use forge::dev_trace::CacheFixCorrelation;
use forge::search_cache::CacheStats;

/// 构建测试用 CacheFixCorrelation
fn build_correlation(
    hit_checks: usize,
    miss_checks: usize,
    hit_success_rate: f64,
) -> CacheFixCorrelation {
    let mut corr = CacheFixCorrelation::new();
    for i in 0..hit_checks {
        corr.record_hit_check((i as f64 / hit_checks.max(1) as f64) < hit_success_rate);
    }
    for i in 0..miss_checks {
        corr.record_miss_check((i as f64 / miss_checks.max(1) as f64) < hit_success_rate * 0.8);
    }
    corr
}

/// 基准测试: should_disable_cache
fn bench_should_disable_cache(c: &mut Criterion) {
    let mut group = c.benchmark_group("should_disable_cache");

    let corr = build_correlation(100, 100, 0.3);
    let default_config = cache_tuning::CacheTuningConfig::default();
    let aggressive_config = cache_tuning::CacheTuningConfig::aggressive();
    let conservative_config = cache_tuning::CacheTuningConfig::conservative();

    group.bench_function("default", |b| {
        b.iter(|| {
            black_box(cache_tuning::should_disable_cache(
                black_box(&corr),
                black_box(&default_config),
            ))
        })
    });

    group.bench_function("aggressive", |b| {
        b.iter(|| {
            black_box(cache_tuning::should_disable_cache(
                black_box(&corr),
                black_box(&aggressive_config),
            ))
        })
    });

    group.bench_function("conservative", |b| {
        b.iter(|| {
            black_box(cache_tuning::should_disable_cache(
                black_box(&corr),
                black_box(&conservative_config),
            ))
        })
    });

    group.finish();
}

/// 基准测试: compute_new_ttl
fn bench_compute_new_ttl(c: &mut Criterion) {
    let mut group = c.benchmark_group("compute_new_ttl");

    let ttls: Vec<u64> = vec![300, 1800, 3600, 7200];
    let config = cache_tuning::CacheTuningConfig::default();

    for ttl in &ttls {
        let corr = build_correlation(50, 50, 0.4);
        let ttl_val = *ttl;
        let cfg = config.clone();
        group.bench_function(format!("ttl_{}", ttl_val), move |b| {
            b.iter(|| {
                black_box(cache_tuning::compute_new_ttl(
                    black_box(ttl_val),
                    black_box(&corr),
                    black_box(&cfg),
                ))
            })
        });
    }
    group.finish();
}

/// 基准测试: make_tuning_decision
fn bench_make_tuning_decision(c: &mut Criterion) {
    let mut group = c.benchmark_group("make_tuning_decision");

    let sizes: Vec<usize> = vec![10, 100, 1_000];
    let config = cache_tuning::CacheTuningConfig::default();

    for size in &sizes {
        group.throughput(Throughput::Elements(*size as u64));
        let corr = build_correlation(*size, *size, 0.35);
        let stats = CacheStats::new();
        let size_val = *size;
        let cfg = config.clone();

        group.bench_function(format!("decision_{}", size_val), move |b| {
            b.iter(|| {
                black_box(cache_tuning::make_tuning_decision(
                    black_box(&corr),
                    black_box(&stats),
                    black_box(1800),
                    black_box(&cfg),
                ))
            })
        });
    }
    group.finish();
}

/// 生成决策列表
fn generate_decisions(count: usize) -> Vec<cache_tuning::CacheTuningDecision> {
    let config = cache_tuning::CacheTuningConfig::default();
    (0..count)
        .map(|_| {
            let corr = build_correlation(10, 10, 0.3);
            let stats = CacheStats::new();
            cache_tuning::make_tuning_decision(&corr, &stats, 1800, &config)
        })
        .collect()
}

/// 基准测试: extract_ttl_trajectory
fn bench_extract_ttl_trajectory(c: &mut Criterion) {
    let mut group = c.benchmark_group("extract_ttl_trajectory");

    let sizes: Vec<usize> = vec![10, 100, 1_000, 10_000];

    for size in &sizes {
        group.throughput(Throughput::Elements(*size as u64));
        let decisions = generate_decisions(*size);

        group.bench_with_input(
            BenchmarkId::new("trajectory", size),
            &decisions,
            |b, decisions| {
                b.iter(|| black_box(cache_tuning::extract_ttl_trajectory(black_box(decisions))))
            },
        );
    }
    group.finish();
}

/// 基准测试: extract_correlation_diffs
fn bench_extract_correlation_diffs(c: &mut Criterion) {
    let mut group = c.benchmark_group("extract_correlation_diffs");

    let sizes: Vec<usize> = vec![10, 100, 1_000, 10_000];

    for size in &sizes {
        group.throughput(Throughput::Elements(*size as u64));
        let decisions = generate_decisions(*size);

        group.bench_with_input(
            BenchmarkId::new("diffs", size),
            &decisions,
            |b, decisions| {
                b.iter(|| {
                    black_box(cache_tuning::extract_correlation_diffs(black_box(
                        decisions,
                    )))
                })
            },
        );
    }
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
        .output_directory(std::path::Path::new("target/criterion/cache_tuning"))
}

criterion_group! {
    name = cache_tuning_benches;
    config = configure_criterion();
    targets =
        bench_should_disable_cache,
        bench_compute_new_ttl,
        bench_make_tuning_decision,
        bench_extract_ttl_trajectory,
        bench_extract_correlation_diffs,
}

criterion_main!(cache_tuning_benches);
