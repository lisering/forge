#![allow(clippy::useless_vec)]

//! Memory 评估模块性能基准测试
//!
//! 测试目标:
//! 1. has_sufficient_evaluation_data - 数据充分性检查性能
//! 2. should_disable_injection - 禁用决策性能
//! 3. compute_memory_evaluation_decision - 完整评估决策性能
//! 4. evaluate_and_apply - 评估器一步评估性能
//! 5. edge_cases - 边界条件性能

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use forge::memory_evaluation;

/// 基准测试: has_sufficient_evaluation_data
fn bench_has_sufficient_data(c: &mut Criterion) {
    let mut group = c.benchmark_group("has_sufficient_evaluation_data");

    let test_cases = vec![
        ("empty", 0, 0, 5),
        ("partial", 2, 1, 5),
        ("sufficient", 3, 3, 5),
        ("large", 100, 100, 5),
    ];

    for (name, with, without, min) in &test_cases {
        let with_val = *with;
        let without_val = *without;
        let min_val = *min;
        group.bench_function(*name, |b| {
            b.iter(|| {
                black_box(memory_evaluation::has_sufficient_evaluation_data(
                    black_box(with_val),
                    black_box(without_val),
                    black_box(min_val),
                ))
            })
        });
    }
    group.finish();
}

/// 基准测试: should_disable_injection
fn bench_should_disable_injection(c: &mut Criterion) {
    let mut group = c.benchmark_group("should_disable_injection");

    let test_cases = vec![
        ("harmful", -0.15, -0.10),
        ("very_harmful", -0.50, -0.10),
        ("beneficial", 0.05, -0.10),
        ("neutral", 0.0, -0.10),
        ("borderline", -0.09, -0.10),
    ];

    for (name, diff, threshold) in &test_cases {
        let diff_val = *diff;
        let threshold_val = *threshold;
        group.bench_function(*name, |b| {
            b.iter(|| {
                black_box(memory_evaluation::should_disable_injection(
                    black_box(diff_val),
                    black_box(threshold_val),
                ))
            })
        });
    }
    group.finish();
}

/// 基准测试: compute_memory_evaluation_decision
fn bench_compute_decision(c: &mut Criterion) {
    let mut group = c.benchmark_group("compute_memory_evaluation_decision");

    let sizes: Vec<usize> = vec![5, 50, 500, 5_000];
    let config = memory_evaluation::MemoryEvaluationConfig::default();

    for size in &sizes {
        group.throughput(Throughput::Elements(*size as u64));
        let cfg = config.clone();
        let s = *size;
        // 注入有害: with 30% success, without 70% success
        group.bench_function(format!("harmful_{}", s), move |b| {
            b.iter(|| {
                black_box(memory_evaluation::compute_memory_evaluation_decision(
                    black_box(s),
                    black_box(s),
                    black_box((s as f64 * 0.3) as usize),
                    black_box((s as f64 * 0.7) as usize),
                    black_box(&cfg),
                ))
            })
        });
    }

    for size in &sizes {
        group.throughput(Throughput::Elements(*size as u64));
        let cfg = config.clone();
        let s = *size;
        // 注入有效: with 70% success, without 30% success
        group.bench_function(format!("beneficial_{}", s), move |b| {
            b.iter(|| {
                black_box(memory_evaluation::compute_memory_evaluation_decision(
                    black_box(s),
                    black_box(s),
                    black_box((s as f64 * 0.7) as usize),
                    black_box((s as f64 * 0.3) as usize),
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
    let config = memory_evaluation::MemoryEvaluationConfig::default();

    for size in &sizes {
        group.throughput(Throughput::Elements(*size as u64));
        let cfg = config.clone();
        let s = *size;
        let successes_with = (s as f64 * 0.4) as usize;
        let successes_without = (s as f64 * 0.6) as usize;

        group.bench_function(format!("eval_{}", s), move |b| {
            b.iter_batched(
                || memory_evaluation::MemoryContextEvaluator::new(cfg.clone()),
                |mut evaluator| {
                    black_box(evaluator.evaluate_and_apply(
                        black_box(s),
                        black_box(successes_with),
                        black_box(s),
                        black_box(successes_without),
                    ))
                },
                criterion::BatchSize::SmallInput,
            )
        });
    }
    group.finish();
}

/// 边界条件基准测试
fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_cases");

    let config = memory_evaluation::MemoryEvaluationConfig::default();

    // 空数据
    group.bench_function("empty_data", |b| {
        b.iter(|| {
            black_box(memory_evaluation::compute_memory_evaluation_decision(
                black_box(0),
                black_box(0),
                black_box(0),
                black_box(0),
                black_box(&config),
            ))
        })
    });

    // 极小样本
    group.bench_function("tiny_sample", |b| {
        b.iter(|| {
            black_box(memory_evaluation::compute_memory_evaluation_decision(
                black_box(1),
                black_box(1),
                black_box(1),
                black_box(0),
                black_box(&config),
            ))
        })
    });

    // 完全相同修复率
    group.bench_function("identical_rates", |b| {
        b.iter(|| {
            black_box(memory_evaluation::compute_memory_evaluation_decision(
                black_box(100),
                black_box(100),
                black_box(50),
                black_box(50),
                black_box(&config),
            ))
        })
    });

    // 严格配置
    let strict_config = memory_evaluation::MemoryEvaluationConfig::strict();
    group.bench_function("strict_config", |b| {
        b.iter(|| {
            black_box(memory_evaluation::compute_memory_evaluation_decision(
                black_box(100),
                black_box(100),
                black_box(30),
                black_box(70),
                black_box(&strict_config),
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
        .output_directory(std::path::Path::new("target/criterion/memory_evaluation"))
}

criterion_group! {
    name = memory_evaluation_benches;
    config = configure_criterion();
    targets =
        bench_has_sufficient_data,
        bench_should_disable_injection,
        bench_compute_decision,
        bench_evaluate_and_apply,
        bench_edge_cases,
}

criterion_main!(memory_evaluation_benches);
