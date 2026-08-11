#![allow(clippy::useless_vec)]

//! 错误搜索模块性能基准测试
//!
//! 测试目标:
//! 1. build_error_search_query - 搜索查询构建性能
//! 2. extract_error_keywords - 关键词提取性能
//! 3. should_search_errors - 搜索决策性能
//! 4. format_search_results_section - 结果格式化性能
//! 5. truncate_search_results - 结果截断性能

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use forge::error_search;
use forge::testrunner::CompileError;

/// 生成编译错误列表
fn generate_errors(count: usize) -> Vec<CompileError> {
    (0..count)
        .map(|i| CompileError {
            file: format!("src/module_{i}.rs"),
            line: Some((i as u32 % 200) + 1),
            column: Some((i as u32 % 80) + 1),
            message: format!(
                "mismatched types: expected `u32`, found `&str` in function foo at offset {i}"
            ),
            error_code: Some(format!("E{}", 300 + i % 100)),
        })
        .collect()
}

/// 生成长错误消息
fn generate_long_message(words: usize) -> String {
    (0..words)
        .map(|i| match i % 5 {
            0 => "expected".to_string(),
            1 => "type".to_string(),
            2 => format!("`Foo{i}`"),
            3 => "but".to_string(),
            _ => "found".to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// 基准测试: build_error_search_query
fn bench_build_error_search_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("build_error_search_query");

    let sizes = vec![1, 5, 20, 100];

    for size in sizes {
        group.throughput(Throughput::Elements(size as u64));
        let errors = generate_errors(size);

        group.bench_with_input(BenchmarkId::new("query", size), &errors, |b, errors| {
            b.iter(|| black_box(error_search::build_error_search_query(black_box(errors))))
        });
    }
    group.finish();
}

/// 基准测试: extract_error_keywords
fn bench_extract_error_keywords(c: &mut Criterion) {
    let mut group = c.benchmark_group("extract_error_keywords");

    let word_counts = vec![10, 100, 1_000, 10_000];

    for words in word_counts {
        group.throughput(Throughput::Elements(words as u64));
        let message = generate_long_message(words);

        group.bench_with_input(
            BenchmarkId::new("keywords", words),
            &message,
            |b, message| {
                b.iter(|| black_box(error_search::extract_error_keywords(black_box(message))))
            },
        );
    }
    group.finish();
}

/// 基准测试: should_search_errors
fn bench_should_search_errors(c: &mut Criterion) {
    let mut group = c.benchmark_group("should_search_errors");

    let attempts = vec![1, 3, 5, 10];
    let errors = generate_errors(5);

    for attempt in attempts {
        group.bench_with_input(
            BenchmarkId::new("attempt", attempt),
            &(attempt, &errors),
            |b, &(attempt, errors)| {
                b.iter(|| {
                    black_box(error_search::should_search_errors(
                        black_box(errors),
                        black_box(attempt),
                        black_box(false),
                    ))
                })
            },
        );
    }
    group.finish();
}

/// 基准测试: format_search_results_section
fn bench_format_search_results_section(c: &mut Criterion) {
    let mut group = c.benchmark_group("format_search_results_section");

    let result_sizes = vec![100, 1_000, 10_000, 100_000];

    for size in result_sizes {
        group.throughput(Throughput::Elements(size as u64));
        let results = "x".repeat(size);

        group.bench_with_input(BenchmarkId::new("format", size), &results, |b, results| {
            b.iter(|| {
                black_box(error_search::format_search_results_section(
                    black_box("rust error type mismatch"),
                    black_box(results),
                    black_box(150),
                ))
            })
        });
    }
    group.finish();
}

/// 基准测试: truncate_search_results
fn bench_truncate_search_results(c: &mut Criterion) {
    let mut group = c.benchmark_group("truncate_search_results");

    let sizes = vec![100, 1_000, 10_000, 100_000];

    for size in sizes {
        group.throughput(Throughput::Elements(size as u64));
        let results = "line of search result text\n".repeat(size / 25 + 1);

        group.bench_with_input(
            BenchmarkId::new("truncate", size),
            &results,
            |b, results| {
                b.iter(|| {
                    black_box(error_search::truncate_search_results(
                        black_box(results),
                        black_box(2000),
                    ))
                })
            },
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
        .output_directory(std::path::Path::new("target/criterion/error_search"))
}

criterion_group! {
    name = error_search_benches;
    config = configure_criterion();
    targets =
        bench_build_error_search_query,
        bench_extract_error_keywords,
        bench_should_search_errors,
        bench_format_search_results_section,
        bench_truncate_search_results,
}

criterion_main!(error_search_benches);
