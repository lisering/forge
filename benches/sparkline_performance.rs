#![allow(clippy::useless_vec)]

//! Sparkline 性能基准测试
//!
//! 测试目标:
//! 1. normalize_value - 基础数值处理性能
//! 2. compute_sparkline_stats - 统计分析性能  
//! 3. render_sparkline - ASCII 渲染性能
//! 4. format_trend_sparkline - 趋势图格式化性能

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use forge::sparkline;
use rand::Rng;

/// 生成指定大小的随机数据点
fn generate_data_points(size: usize) -> Vec<f64> {
    let mut rng = rand::thread_rng();
    (0..size).map(|_| rng.gen_range(-100.0..100.0)).collect()
}

/// 生成正态分布的数据点（模拟真实场景）
fn generate_normal_data(size: usize) -> Vec<f64> {
    let mut rng = rand::thread_rng();
    (0..size)
        .map(|_| {
            let u1: f64 = rng.gen();
            let u2: f64 = rng.gen();
            (u1.ln() * -2.0).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
        })
        .collect()
}

/// 基准测试: normalize_value 函数
fn bench_normalize_value(c: &mut Criterion) {
    let mut group = c.benchmark_group("normalize_value");
    group.throughput(Throughput::Elements(1));

    let test_cases = vec![
        (0.5, 0.0, 1.0),       // 标准情况
        (150.0, 100.0, 200.0), // 大数值
        (-0.5, -1.0, 0.0),     // 负数值
        (0.999, 0.0, 1.0),     // 边界值
    ];

    for (value, min, max) in test_cases {
        let case_name = format!("val_{}_range_{}_{}", value, min, max);
        group.bench_with_input(
            BenchmarkId::new("normalize", case_name),
            &(value, min, max),
            |b, &(value, min, max)| {
                b.iter(|| {
                    black_box(sparkline::normalize_value(
                        black_box(value),
                        black_box(min),
                        black_box(max),
                    ))
                })
            },
        );
    }
    group.finish();
}

/// 基准测试: compute_sparkline_stats 函数
fn bench_compute_sparkline_stats(c: &mut Criterion) {
    let mut group = c.benchmark_group("compute_sparkline_stats");

    // 测试不同数据规模
    let sizes = vec![10, 100, 1_000, 10_000, 100_000];

    for size in sizes {
        group.throughput(Throughput::Elements(size as u64));
        let data = generate_normal_data(size);

        group.bench_with_input(BenchmarkId::new("stats", size), &data, |b, data| {
            b.iter(|| black_box(sparkline::compute_sparkline_stats(black_box(data))))
        });
    }
    group.finish();
}

/// 基准测试: render_sparkline 函数
fn bench_render_sparkline(c: &mut Criterion) {
    let mut group = c.benchmark_group("render_sparkline");

    // 测试不同数据规模和宽度组合
    let test_cases = vec![
        (10, 20),     // 小数据，小宽度
        (100, 50),    // 中等数据，中等宽度
        (1_000, 60),  // 大数据，标准宽度
        (10_000, 80), // 压力测试场景
    ];

    for (data_size, width) in test_cases {
        group.throughput(Throughput::Elements(data_size as u64));
        let data = generate_data_points(data_size);
        let config = forge::sparkline::SparklineConfig::new(width);

        group.bench_with_input(
            BenchmarkId::new("render", format!("{}x{}", data_size, width)),
            &(&data, &config),
            |b, &(data, config)| {
                b.iter(|| {
                    black_box(sparkline::render_sparkline(
                        black_box(data),
                        black_box(config),
                    ))
                })
            },
        );
    }
    group.finish();
}

/// 基准测试: format_trend_sparkline 函数
fn bench_format_trend_sparkline(c: &mut Criterion) {
    let mut group = c.benchmark_group("format_trend_sparkline");

    // 测试不同数据规模下的完整格式化性能
    let sizes = vec![10, 100, 1_000, 10_000];

    for size in sizes {
        group.throughput(Throughput::Elements(size as u64));
        let data = generate_data_points(size);
        let config = forge::sparkline::SparklineConfig::default();

        group.bench_with_input(
            BenchmarkId::new("format_trend", size),
            &(&data, &config),
            |b, &(data, config)| {
                b.iter(|| {
                    black_box(sparkline::format_trend_sparkline(
                        black_box("Test"),
                        black_box(data),
                        black_box(config),
                    ))
                })
            },
        );
    }
    group.finish();
}

/// 边界条件基准测试
fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_cases");

    // 空数据
    group.bench_function("empty_data", |b| {
        let config = forge::sparkline::SparklineConfig::default();
        b.iter(|| {
            black_box(sparkline::render_sparkline(
                black_box(&[]),
                black_box(&config),
            ))
        })
    });

    // 单元素
    group.bench_function("single_element", |b| {
        let config = forge::sparkline::SparklineConfig::default();
        b.iter(|| {
            black_box(sparkline::render_sparkline(
                black_box(&[42.0]),
                black_box(&config),
            ))
        })
    });

    group.finish();
}

/// 配置基准测试参数
fn configure_criterion() -> Criterion {
    Criterion::default()
        .sample_size(50) // 适度样本量
        .measurement_time(std::time::Duration::from_secs(5))
        .warm_up_time(std::time::Duration::from_secs(2))
        .nresamples(50_000)
        .noise_threshold(0.05)
        .output_directory(std::path::Path::new("target/criterion/sparkline"))
}

// 注册基准测试组
criterion_group! {
    name = sparkline_benches;
    config = configure_criterion();
    targets =
        bench_normalize_value,
        bench_compute_sparkline_stats,
        bench_render_sparkline,
        bench_format_trend_sparkline,
        bench_edge_cases,
}

criterion_main!(sparkline_benches);
