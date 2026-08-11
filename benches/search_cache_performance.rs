#![allow(clippy::useless_vec)]

//! 搜索缓存模块性能基准测试
//!
//! 测试目标:
//! 1. build_cache_key - 缓存键构建性能
//! 2. normalize_query_for_cache - 查询规范化性能
//! 3. is_cache_expired - 过期检查性能
//! 4. format_cache_stats - 统计格式化性能
//! 5. find_oldest_key - 最旧键查找性能

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use forge::search_cache;
use forge::testrunner::CompileError;
use std::collections::HashMap;

/// 生成编译错误列表
fn generate_errors(count: usize) -> Vec<CompileError> {
    (0..count)
        .map(|i| CompileError {
            file: format!("src/module_{i}.rs"),
            line: Some((i as u32 % 200) + 1),
            column: Some((i as u32 % 80) + 1),
            message: format!("error in module {i}: mismatched types expected u32 found str"),
            error_code: Some(format!("E{}", 300 + i % 100)),
        })
        .collect()
}

/// 基准测试: build_cache_key
fn bench_build_cache_key(c: &mut Criterion) {
    let mut group = c.benchmark_group("build_cache_key");

    let sizes = vec![1, 3, 10, 50];

    for size in sizes {
        group.throughput(Throughput::Elements(size as u64));
        let errors = generate_errors(size);

        group.bench_with_input(BenchmarkId::new("key", size), &errors, |b, errors| {
            b.iter(|| black_box(search_cache::build_cache_key(black_box(errors))))
        });
    }
    group.finish();
}

/// 基准测试: normalize_query_for_cache
fn bench_normalize_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("normalize_query_for_cache");

    let word_counts = vec![5, 20, 100, 500];

    for words in word_counts {
        group.throughput(Throughput::Elements(words as u64));
        // 包含大小写、多余空格的查询
        let query: String = (0..words)
            .map(|i| match i % 3 {
                0 => "Rust",
                1 => "Error",
                _ => "Type",
            })
            .collect::<Vec<_>>()
            .join("  ")
            + " Extra Spaces ";

        group.bench_with_input(BenchmarkId::new("normalize", words), &query, |b, query| {
            b.iter(|| black_box(search_cache::normalize_query_for_cache(black_box(query))))
        });
    }
    group.finish();
}

/// 基准测试: is_cache_expired
fn bench_is_cache_expired(c: &mut Criterion) {
    let mut group = c.benchmark_group("is_cache_expired");

    let test_cases = vec![
        ("not_expired", 1000, 1500, 1000),
        ("just_expired", 1000, 2001, 1000),
        ("far_expired", 1000, 100_000, 1000),
        ("clock_back", 2000, 1000, 1000),
    ];

    for (name, cached_at, now, ttl) in &test_cases {
        group.bench_function(*name, |b| {
            b.iter(|| {
                black_box(search_cache::is_cache_expired(
                    black_box(*cached_at),
                    black_box(*now),
                    black_box(*ttl),
                ))
            })
        });
    }
    group.finish();
}

/// 基准测试: format_cache_stats
fn bench_format_cache_stats(c: &mut Criterion) {
    let mut group = c.benchmark_group("format_cache_stats");

    let test_cases = vec![
        ("empty", search_cache::CacheStats::new()),
        ("small", {
            let mut s = search_cache::CacheStats::new();
            s.hits = 50;
            s.misses = 30;
            s.evictions = 5;
            s
        }),
        ("large", {
            let mut s = search_cache::CacheStats::new();
            s.hits = 10_000;
            s.misses = 5_000;
            s.evictions = 500;
            s
        }),
    ];

    for (name, stats) in &test_cases {
        group.bench_function(*name, |b| {
            b.iter(|| black_box(search_cache::format_cache_stats(black_box(stats))))
        });
    }
    group.finish();
}

/// 基准测试: find_oldest_key
fn bench_find_oldest_key(c: &mut Criterion) {
    let mut group = c.benchmark_group("find_oldest_key");

    let sizes = vec![10, 100, 1_000, 10_000];

    for size in sizes {
        group.throughput(Throughput::Elements(size as u64));

        let mut entries: HashMap<String, search_cache::CachedSearchEntry> = HashMap::new();
        for i in 0..size {
            let entry = search_cache::CachedSearchEntry::with_timestamp(
                format!("query_{i}"),
                format!("result_{i}"),
                100,
                i as u64,
            );
            entries.insert(format!("key_{i}"), entry);
        }

        group.bench_with_input(
            BenchmarkId::new("find_oldest", size),
            &entries,
            |b, entries| b.iter(|| black_box(search_cache::find_oldest_key(black_box(entries)))),
        );
    }
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
        .output_directory(std::path::Path::new("target/criterion/search_cache"))
}

criterion_group! {
    name = search_cache_benches;
    config = configure_criterion();
    targets =
        bench_build_cache_key,
        bench_normalize_query,
        bench_is_cache_expired,
        bench_format_cache_stats,
        bench_find_oldest_key,
}

criterion_main!(search_cache_benches);
