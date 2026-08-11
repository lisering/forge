#![allow(clippy::useless_vec)]

//! Auto Recovery 模块性能基准测试
//!
//! 测试目标:
//! 1. BackoffStrategy::delay_secs - 退避延迟计算性能
//! 2. decide_recovery_action - 恢复决策性能
//! 3. compute_backoff_schedule - 退避计划计算性能
//! 4. select_recovery_strategy - 策略选择性能
//! 5. edge_cases - 边界条件性能

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use forge::auto_recovery::{self, BackoffStrategy, RecoveryConfig};
use forge::connection_monitor::{ConnectionStatus, HealthLevel};

/// 基准测试: BackoffStrategy::delay_secs
fn bench_backoff_delay(c: &mut Criterion) {
    let mut group = c.benchmark_group("backoff_delay_secs");

    let strategies = vec![
        ("default", BackoffStrategy::default()),
        ("fast", BackoffStrategy::new(1, 10)),
        ("slow", BackoffStrategy::new(5, 300)),
    ];

    let attempts: Vec<u32> = vec![1, 3, 5, 10, 20];

    for (name, strategy) in &strategies {
        for &attempt in &attempts {
            let strategy = strategy.clone();
            group.bench_function(format!("{name}/attempt_{attempt}"), move |b| {
                b.iter(|| black_box(strategy.delay_secs(black_box(attempt))))
            });
        }
    }
    group.finish();
}

/// 基准测试: decide_recovery_action
fn bench_decide_recovery_action(c: &mut Criterion) {
    let mut group = c.benchmark_group("decide_recovery_action");

    let backoff = BackoffStrategy::default();
    let max_retries: Vec<u32> = vec![3, 10, 50];

    for &max_retry in &max_retries {
        group.throughput(Throughput::Elements(max_retry as u64));

        // 连接正常 → Succeed
        let bo = backoff.clone();
        group.bench_function(format!("succeed/max_{max_retry}"), move |b| {
            b.iter(|| {
                black_box(auto_recovery::decide_recovery_action(
                    black_box(true),
                    black_box(0),
                    black_box(max_retry),
                    black_box(&bo),
                ))
            })
        });

        // 连接异常 → Retry
        let bo = backoff.clone();
        group.bench_function(format!("retry/max_{max_retry}"), move |b| {
            b.iter(|| {
                black_box(auto_recovery::decide_recovery_action(
                    black_box(false),
                    black_box(max_retry / 2),
                    black_box(max_retry),
                    black_box(&bo),
                ))
            })
        });

        // 超过最大重试 → GiveUp
        let bo = backoff.clone();
        group.bench_function(format!("giveup/max_{max_retry}"), move |b| {
            b.iter(|| {
                black_box(auto_recovery::decide_recovery_action(
                    black_box(false),
                    black_box(max_retry),
                    black_box(max_retry),
                    black_box(&bo),
                ))
            })
        });
    }
    group.finish();
}

/// 基准测试: compute_backoff_schedule
fn bench_compute_backoff_schedule(c: &mut Criterion) {
    let mut group = c.benchmark_group("compute_backoff_schedule");

    let strategies = vec![
        ("default", BackoffStrategy::default()),
        ("fast", BackoffStrategy::new(1, 10)),
        ("slow", BackoffStrategy::new(5, 300)),
    ];

    let max_attempts: Vec<u32> = vec![3, 10, 50];

    for (name, strategy) in &strategies {
        for &max_attempt in &max_attempts {
            let strategy = strategy.clone();
            group.throughput(Throughput::Elements(max_attempt as u64));
            group.bench_with_input(
                BenchmarkId::new(name.to_string(), max_attempt),
                &strategy,
                |b, bo| {
                    b.iter(|| {
                        black_box(auto_recovery::compute_backoff_schedule(
                            black_box(bo),
                            black_box(max_attempt),
                        ))
                    })
                },
            );
        }
    }
    group.finish();
}

/// 基准测试: select_recovery_strategy
fn bench_select_recovery_strategy(c: &mut Criterion) {
    let mut group = c.benchmark_group("select_recovery_strategy");

    let statuses = vec![
        ("connected", ConnectionStatus::Connected),
        ("tab_closed", ConnectionStatus::TabClosed),
        (
            "ws_error",
            ConnectionStatus::WebSocketError("reset".to_string()),
        ),
        ("chrome_unreachable", ConnectionStatus::ChromeUnreachable),
        ("check_timeout", ConnectionStatus::CheckTimeout),
    ];

    for (name, status) in &statuses {
        let status = status.clone();
        group.bench_function(*name, move |b| {
            b.iter(|| black_box(auto_recovery::select_recovery_strategy(black_box(&status))))
        });
    }
    group.finish();
}

/// 边界条件基准测试
fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_cases");

    // should_continue_retrying 边界
    let retry_cases: Vec<(&str, u32, u32)> = vec![
        ("zero_attempt", 0, 10),
        ("near_max", 9, 10),
        ("at_max", 10, 10),
        ("over_max", 11, 10),
    ];

    for (name, attempt, max) in &retry_cases {
        group.bench_function(format!("continue_retry_{name}"), |b| {
            b.iter(|| {
                black_box(auto_recovery::should_continue_retrying(
                    black_box(*attempt),
                    black_box(*max),
                ))
            })
        });
    }

    // recovery_efficiency 边界
    let efficiency_cases: Vec<(&str, u32, u32)> = vec![
        ("instant_success", 0, 10),
        ("halfway", 5, 10),
        ("at_max", 10, 10),
        ("zero_max", 0, 0),
    ];

    for (name, attempts, max) in &efficiency_cases {
        group.bench_function(format!("efficiency_{name}"), |b| {
            b.iter(|| {
                black_box(auto_recovery::recovery_efficiency(
                    black_box(*attempts),
                    black_box(*max),
                ))
            })
        });
    }

    // format_recovery_rate 边界
    let rate_cases: Vec<(&str, f64)> = vec![
        ("full", 1.0),
        ("zero", 0.0),
        ("partial", 0.85),
        ("clamped_high", 1.5),
        ("clamped_low", -0.5),
    ];

    for (name, rate) in &rate_cases {
        group.bench_function(format!("format_rate_{name}"), |b| {
            b.iter(|| black_box(auto_recovery::format_recovery_rate(black_box(*rate))))
        });
    }

    // assess_recovery_urgency 边界
    let health_levels = vec![
        ("healthy", HealthLevel::Healthy),
        ("degraded", HealthLevel::Degraded),
        ("critical", HealthLevel::Critical),
    ];

    for (name, level) in &health_levels {
        let level = level.clone();
        group.bench_function(format!("urgency_{name}"), move |b| {
            b.iter(|| black_box(auto_recovery::assess_recovery_urgency(black_box(&level))))
        });
    }

    // compute_recovery_success_rate 边界
    let success_cases: Vec<(&str, u64, u64)> = vec![
        ("zero_recoveries", 0, 0),
        ("all_success", 100, 100),
        ("all_failure", 100, 0),
        ("partial", 100, 75),
    ];

    for (name, total, success) in &success_cases {
        group.bench_function(format!("success_rate_{name}"), |b| {
            b.iter(|| {
                black_box(auto_recovery::compute_recovery_success_rate(
                    black_box(*total),
                    black_box(*success),
                ))
            })
        });
    }

    // estimate_max_recovery_secs 边界
    let configs = vec![
        ("default", RecoveryConfig::default()),
        (
            "fast",
            RecoveryConfig::new(9222, 3).with_backoff(BackoffStrategy::new(1, 10)),
        ),
        (
            "slow",
            RecoveryConfig::new(9222, 20).with_backoff(BackoffStrategy::new(5, 300)),
        ),
    ];

    for (name, config) in &configs {
        let config = config.clone();
        group.bench_function(format!("estimate_max_{name}"), move |b| {
            b.iter(|| {
                black_box(auto_recovery::estimate_max_recovery_secs(black_box(
                    &config,
                )))
            })
        });
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
        .output_directory(std::path::Path::new("target/criterion/auto_recovery"))
}

criterion_group! {
    name = auto_recovery_benches;
    config = configure_criterion();
    targets =
        bench_backoff_delay,
        bench_decide_recovery_action,
        bench_compute_backoff_schedule,
        bench_select_recovery_strategy,
        bench_edge_cases,
}

criterion_main!(auto_recovery_benches);
