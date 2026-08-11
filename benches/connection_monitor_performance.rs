#![allow(clippy::useless_vec)]

//! Connection Monitor 模块性能基准测试
//!
//! 测试目标:
//! 1. determine_health_level - 健康等级判定性能
//! 2. classify_connection_severity - 严重程度分类性能
//! 3. should_trigger_recovery - 恢复触发决策性能
//! 4. compute_next_check_delay - 延迟计算性能
//! 5. edge_cases - 边界条件性能

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use forge::connection_monitor::{self, ConnectionStatus};

/// 所有连接状态变体
fn all_statuses() -> Vec<(&'static str, ConnectionStatus)> {
    vec![
        ("connected", ConnectionStatus::Connected),
        ("chrome_unreachable", ConnectionStatus::ChromeUnreachable),
        ("tab_closed", ConnectionStatus::TabClosed),
        (
            "ws_error",
            ConnectionStatus::WebSocketError("connection reset".to_string()),
        ),
        ("check_timeout", ConnectionStatus::CheckTimeout),
    ]
}

/// 基准测试: determine_health_level
fn bench_determine_health_level(c: &mut Criterion) {
    let mut group = c.benchmark_group("determine_health_level");

    let statuses = all_statuses();
    let failure_counts: Vec<u32> = vec![0, 1, 2, 3, 5, 10];

    for (name, status) in &statuses {
        for &failures in &failure_counts {
            let status = status.clone();
            group.bench_function(format!("{name}/fail_{failures}"), move |b| {
                b.iter(|| {
                    black_box(connection_monitor::determine_health_level(
                        black_box(&status),
                        black_box(failures),
                        black_box(3),
                    ))
                })
            });
        }
    }
    group.finish();
}

/// 基准测试: classify_connection_severity
fn bench_classify_severity(c: &mut Criterion) {
    let mut group = c.benchmark_group("classify_connection_severity");

    let statuses = all_statuses();

    for (name, status) in &statuses {
        let status = status.clone();
        group.bench_function(*name, move |b| {
            b.iter(|| {
                black_box(connection_monitor::classify_connection_severity(black_box(
                    &status,
                )))
            })
        });
    }
    group.finish();
}

/// 基准测试: should_trigger_recovery
fn bench_should_trigger_recovery(c: &mut Criterion) {
    let mut group = c.benchmark_group("should_trigger_recovery");

    let statuses = all_statuses();
    let failure_counts: Vec<u32> = vec![0, 1, 2, 3, 5];

    for (name, status) in &statuses {
        for &failures in &failure_counts {
            let status = status.clone();
            group.bench_function(format!("{name}/fail_{failures}"), move |b| {
                b.iter(|| {
                    black_box(connection_monitor::should_trigger_recovery(
                        black_box(&status),
                        black_box(failures),
                        black_box(3),
                    ))
                })
            });
        }
    }
    group.finish();
}

/// 基准测试: compute_next_check_delay
fn bench_compute_next_check_delay(c: &mut Criterion) {
    let mut group = c.benchmark_group("compute_next_check_delay");

    let statuses = all_statuses();
    let intervals: Vec<u64> = vec![10, 30, 60, 120];

    for (name, status) in &statuses {
        for &interval in &intervals {
            let status = status.clone();
            group.throughput(Throughput::Elements(interval));
            group.bench_function(format!("{name}/interval_{interval}"), move |b| {
                b.iter(|| {
                    black_box(connection_monitor::compute_next_check_delay(
                        black_box(&status),
                        black_box(2),
                        black_box(interval),
                    ))
                })
            });
        }
    }
    group.finish();
}

/// 边界条件基准测试
fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_cases");

    // calculate_monitor_success_rate 边界
    let test_cases: Vec<(&str, u64, u64)> = vec![
        ("zero_checks", 0, 0),
        ("all_success", 1000, 0),
        ("all_failure", 1000, 1000),
        ("half", 1000, 500),
    ];

    for (name, total, failures) in &test_cases {
        group.bench_function(*name, |b| {
            b.iter(|| {
                black_box(connection_monitor::calculate_monitor_success_rate(
                    black_box(*total),
                    black_box(*failures),
                ))
            })
        });
    }

    // is_chrome_crashed_status 边界
    let crash_cases: Vec<(&str, u32, u32)> = vec![
        ("below_threshold", 2, 3),
        ("at_threshold", 3, 3),
        ("above_threshold", 5, 3),
        ("zero_failures", 0, 3),
    ];

    for (name, failures, max) in &crash_cases {
        group.bench_function(format!("crash_{name}"), |b| {
            b.iter(|| {
                black_box(connection_monitor::is_chrome_crashed_status(
                    black_box(*failures),
                    black_box(*max),
                ))
            })
        });
    }

    // ConnectionStatus 方法
    let connected = ConnectionStatus::Connected;
    group.bench_function("status_is_connected", |b| {
        b.iter(|| black_box(connected.is_connected()))
    });

    group.bench_function("status_needs_recovery", |b| {
        b.iter(|| black_box(connected.needs_recovery()))
    });

    group.bench_function("status_description", |b| {
        b.iter(|| black_box(connected.description()))
    });

    group.bench_function("status_recovery_difficulty", |b| {
        b.iter(|| black_box(connected.recovery_difficulty()))
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
        .output_directory(std::path::Path::new("target/criterion/connection_monitor"))
}

criterion_group! {
    name = connection_monitor_benches;
    config = configure_criterion();
    targets =
        bench_determine_health_level,
        bench_classify_severity,
        bench_should_trigger_recovery,
        bench_compute_next_check_delay,
        bench_edge_cases,
}

criterion_main!(connection_monitor_benches);
