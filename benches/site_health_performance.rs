#![allow(clippy::useless_vec)]

//! Site Health 模块性能基准测试
//!
//! 测试目标:
//! 1. interpret_health_json - 健康状态解释性能
//! 2. classify_health_severity - 严重程度分类性能
//! 3. compute_health_check_interval - 检查间隔计算性能
//! 4. select_best_healthy_tab - 最佳标签页选择性能
//! 5. edge_cases - 边界条件性能

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use forge::site_health::{self, HealthCheckJson, HealthCheckResult, SiteHealthStatus};

/// 所有健康状态变体
fn all_health_statuses() -> Vec<(&'static str, SiteHealthStatus)> {
    vec![
        ("healthy", SiteHealthStatus::Healthy),
        ("not_logged_in", SiteHealthStatus::NotLoggedIn),
        ("rate_limited", SiteHealthStatus::RateLimited),
        ("under_maintenance", SiteHealthStatus::UnderMaintenance),
        ("network_error", SiteHealthStatus::NetworkError),
        ("unknown", SiteHealthStatus::Unknown),
    ]
}

/// 构建各种 HealthCheckJson 组合
fn build_health_check_jsons() -> Vec<(&'static str, HealthCheckJson)> {
    vec![
        (
            "healthy",
            HealthCheckJson::default()
                .with_input(true)
                .with_url("https://chat.z.ai"),
        ),
        (
            "maintenance",
            HealthCheckJson::default()
                .with_maintenance(true)
                .with_input(true)
                .with_url("https://chat.z.ai/maintenance"),
        ),
        (
            "rate_limited",
            HealthCheckJson::default()
                .with_rate_limit(true)
                .with_message("请求过于频繁")
                .with_url("https://chat.z.ai"),
        ),
        (
            "not_logged_in",
            HealthCheckJson::default()
                .with_login_button(true)
                .with_url("https://chat.z.ai/login"),
        ),
        (
            "unknown",
            HealthCheckJson::default().with_url("https://unknown.page"),
        ),
        (
            "all_flags",
            HealthCheckJson::default()
                .with_input(true)
                .with_login_button(true)
                .with_rate_limit(true)
                .with_maintenance(true)
                .with_message("multiple issues")
                .with_url("https://broken.site"),
        ),
    ]
}

/// 基准测试: interpret_health_json
fn bench_interpret_health_json(c: &mut Criterion) {
    let mut group = c.benchmark_group("interpret_health_json");

    let jsons = build_health_check_jsons();

    for (name, json) in &jsons {
        group.bench_function(*name, |b| {
            b.iter(|| black_box(site_health::interpret_health_json(black_box(json))))
        });
    }
    group.finish();
}

/// 基准测试: classify_health_severity
fn bench_classify_health_severity(c: &mut Criterion) {
    let mut group = c.benchmark_group("classify_health_severity");

    let statuses = all_health_statuses();

    for (name, status) in &statuses {
        let status = status.clone();
        group.bench_function(*name, move |b| {
            b.iter(|| black_box(site_health::classify_health_severity(black_box(&status))))
        });
    }
    group.finish();
}

/// 基准测试: compute_health_check_interval
fn bench_compute_health_check_interval(c: &mut Criterion) {
    let mut group = c.benchmark_group("compute_health_check_interval");

    let statuses = all_health_statuses();
    let intervals: Vec<u64> = vec![30, 60, 120, 300];

    for (name, status) in &statuses {
        for &interval in &intervals {
            let status = status.clone();
            group.throughput(Throughput::Elements(interval));
            group.bench_function(format!("{name}/base_{interval}"), move |b| {
                b.iter(|| {
                    black_box(site_health::compute_health_check_interval(
                        black_box(&status),
                        black_box(interval),
                    ))
                })
            });
        }
    }
    group.finish();
}

/// 基准测试: select_best_healthy_tab
fn bench_select_best_healthy_tab(c: &mut Criterion) {
    let mut group = c.benchmark_group("select_best_healthy_tab");

    let sizes: Vec<usize> = vec![1, 3, 10, 50, 200];

    for &size in &sizes {
        group.throughput(Throughput::Elements(size as u64));

        // 全部健康
        let all_healthy: Vec<(usize, HealthCheckResult)> = (0..size)
            .map(|i| (i, HealthCheckResult::new(SiteHealthStatus::Healthy)))
            .collect();

        group.bench_with_input(
            BenchmarkId::new("all_healthy", size),
            &all_healthy,
            |b, results| {
                b.iter(|| black_box(site_health::select_best_healthy_tab(black_box(results))))
            },
        );

        // 全部不健康
        let all_unhealthy: Vec<(usize, HealthCheckResult)> = (0..size)
            .map(|i| (i, HealthCheckResult::new(SiteHealthStatus::RateLimited)))
            .collect();

        group.bench_with_input(
            BenchmarkId::new("all_unhealthy", size),
            &all_unhealthy,
            |b, results| {
                b.iter(|| black_box(site_health::select_best_healthy_tab(black_box(results))))
            },
        );

        // 混合 (最后一个健康)
        let mut mixed: Vec<(usize, HealthCheckResult)> = (0..size - 1)
            .map(|i| {
                (
                    i,
                    HealthCheckResult::new(SiteHealthStatus::UnderMaintenance),
                )
            })
            .collect();
        mixed.push((size - 1, HealthCheckResult::new(SiteHealthStatus::Healthy)));

        group.bench_with_input(BenchmarkId::new("mixed", size), &mixed, |b, results| {
            b.iter(|| black_box(site_health::select_best_healthy_tab(black_box(results))))
        });
    }
    group.finish();
}

/// 边界条件基准测试
fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_cases");

    // 空标签页列表
    let empty_results: Vec<(usize, HealthCheckResult)> = vec![];
    group.bench_function("empty_tab_list", |b| {
        b.iter(|| {
            black_box(site_health::select_best_healthy_tab(black_box(
                &empty_results,
            )))
        })
    });

    // should_skip_tab 边界
    let skip_cases: Vec<(&str, u32, u32)> = vec![
        ("zero_unhealthy", 0, 3),
        ("below_threshold", 2, 3),
        ("at_threshold", 3, 3),
        ("above_threshold", 5, 3),
    ];

    for (name, unhealthy, threshold) in &skip_cases {
        group.bench_function(format!("skip_tab_{name}"), |b| {
            b.iter(|| {
                black_box(site_health::should_skip_tab(
                    black_box(*unhealthy),
                    black_box(*threshold),
                ))
            })
        });
    }

    // calculate_health_rate 边界
    let rate_cases: Vec<(&str, u64, u64)> = vec![
        ("zero_checks", 0, 0),
        ("all_healthy", 1000, 1000),
        ("all_unhealthy", 1000, 0),
        ("partial", 1000, 750),
    ];

    for (name, total, healthy) in &rate_cases {
        group.bench_function(format!("health_rate_{name}"), |b| {
            b.iter(|| {
                black_box(site_health::calculate_health_rate(
                    black_box(*total),
                    black_box(*healthy),
                ))
            })
        });
    }

    // format_health_rate 边界
    let format_cases: Vec<(&str, f64)> = vec![
        ("full", 1.0),
        ("zero", 0.0),
        ("partial", 0.75),
        ("high", 0.999),
    ];

    for (name, rate) in &format_cases {
        group.bench_function(format!("format_rate_{name}"), |b| {
            b.iter(|| black_box(site_health::format_health_rate(black_box(*rate))))
        });
    }

    // SiteHealthStatus 方法
    let healthy = SiteHealthStatus::Healthy;
    group.bench_function("status_is_healthy", |b| {
        b.iter(|| black_box(healthy.is_healthy()))
    });

    group.bench_function("status_should_failover", |b| {
        b.iter(|| black_box(healthy.should_failover()))
    });

    // determine_failover_priority 边界
    let statuses = all_health_statuses();
    for (name, status) in &statuses {
        let status = status.clone();
        group.bench_function(format!("priority_{name}"), move |b| {
            b.iter(|| black_box(site_health::determine_failover_priority(black_box(&status))))
        });
    }

    // HealthCheckJson builder 链式调用
    group.bench_function("json_builder_chain", |b| {
        b.iter(|| {
            black_box(
                HealthCheckJson::default()
                    .with_url("https://test.com")
                    .with_input(true)
                    .with_login_button(false)
                    .with_rate_limit(false)
                    .with_maintenance(false)
                    .with_message("test"),
            )
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
        .output_directory(std::path::Path::new("target/criterion/site_health"))
}

criterion_group! {
    name = site_health_benches;
    config = configure_criterion();
    targets =
        bench_interpret_health_json,
        bench_classify_health_severity,
        bench_compute_health_check_interval,
        bench_select_best_healthy_tab,
        bench_edge_cases,
}

criterion_main!(site_health_benches);
