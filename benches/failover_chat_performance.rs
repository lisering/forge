#![allow(clippy::useless_vec)]

//! Failover Chat 模块性能基准测试
//!
//! 测试目标:
//! 1. should_failover_decision — 故障切换决策性能
//! 2. classify_failover_failure_reason — 失败原因分类性能
//! 3. calculate_health_check_interval_elapsed — 健康检查间隔判断性能
//! 4. update_min_response_time — 最小响应时间更新性能
//! 5. edge_cases — 边界条件 (format_switch_trace/format_failover_failure_trace/build_error_health_result)

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use forge::browser::SiteType;
use forge::failover_chat::{
    build_error_health_result, calculate_health_check_interval_elapsed,
    classify_failover_failure_reason, format_failover_failure_trace, format_switch_trace,
    should_failover_decision, update_min_response_time,
};
use forge::site_health::{HealthCheckResult, SiteHealthStatus};

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

/// 基准测试: should_failover_decision
fn bench_should_failover_decision(c: &mut Criterion) {
    let mut group = c.benchmark_group("should_failover_decision");

    let statuses = all_health_statuses();

    for (name, status) in &statuses {
        let health = HealthCheckResult::new(status.clone());
        group.bench_function(*name, |b| {
            b.iter(|| black_box(should_failover_decision(black_box(&health))))
        });
    }
    group.finish();
}

/// 基准测试: classify_failover_failure_reason
fn bench_classify_failover_failure_reason(c: &mut Criterion) {
    let mut group = c.benchmark_group("classify_failover_failure_reason");

    let cases: Vec<(&str, bool, usize, usize)> = vec![
        ("all_tried", true, 0, 3),
        ("below_max", false, 0, 3),
        ("at_max", false, 3, 3),
        ("above_max", false, 5, 3),
        ("all_tried_high_failures", true, 10, 3),
        ("no_failures", false, 0, 5),
    ];

    for (name, all_tried, consecutive, max) in &cases {
        group.bench_function(*name, |b| {
            b.iter(|| {
                black_box(classify_failover_failure_reason(
                    black_box(*all_tried),
                    black_box(*consecutive),
                    black_box(*max),
                ))
            })
        });
    }
    group.finish();
}

/// 基准测试: calculate_health_check_interval_elapsed
fn bench_calculate_health_check_interval(c: &mut Criterion) {
    let mut group = c.benchmark_group("calculate_health_check_interval");

    // interval=0 (每次都检查)
    group.bench_function("interval_zero", |b| {
        b.iter(|| {
            black_box(calculate_health_check_interval_elapsed(
                black_box(100),
                black_box(50),
                black_box(0),
            ))
        })
    });

    // 各种 interval/turn 组合
    let intervals: Vec<usize> = vec![1, 3, 5, 10, 30];
    let turns: Vec<usize> = vec![0, 1, 2, 3, 5, 10, 20, 50];

    for &interval in &intervals {
        for &turn in &turns {
            group.bench_function(format!("i{interval}/t{turn}"), |b| {
                b.iter(|| {
                    black_box(calculate_health_check_interval_elapsed(
                        black_box(turn),
                        black_box(0),
                        black_box(interval),
                    ))
                })
            });
        }
    }

    // 饱和减法 (last_check > current)
    group.bench_function("saturating_sub", |b| {
        b.iter(|| {
            black_box(calculate_health_check_interval_elapsed(
                black_box(5),
                black_box(100),
                black_box(10),
            ))
        })
    });

    group.finish();
}

/// 基准测试: update_min_response_time
fn bench_update_min_response_time(c: &mut Criterion) {
    let mut group = c.benchmark_group("update_min_response_time");

    let cases: Vec<(&str, u64, u64)> = vec![
        ("initial_zero", 0, 150),
        ("initial_zero_large", 0, 10000),
        ("smaller_update", 150, 100),
        ("larger_no_update", 100, 200),
        ("equal_no_update", 100, 100),
        ("both_zero", 0, 0),
        ("large_values", 999999, 888888),
    ];

    for (name, current, new) in &cases {
        group.bench_function(*name, |b| {
            b.iter(|| {
                black_box(update_min_response_time(
                    black_box(*current),
                    black_box(*new),
                ))
            })
        });
    }

    // 连续更新序列 (模拟运行时场景)
    group.bench_function("sequential_updates", |b| {
        b.iter(|| {
            let mut min: u64 = 0;
            for &duration in &[200, 180, 150, 170, 120, 130, 100, 110, 90, 80] {
                min = update_min_response_time(black_box(min), black_box(duration));
            }
            black_box(min)
        })
    });

    group.finish();
}

/// 边界条件基准测试
fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_cases");

    // format_switch_trace — 所有 SiteType 组合
    let site_types: Vec<(&str, SiteType)> = vec![
        ("zai", SiteType::Zai),
        ("deepseek", SiteType::DeepSeek),
        ("kimi", SiteType::Kimi),
        ("tongyi", SiteType::Tongyi),
        ("claude", SiteType::Claude),
        ("unknown", SiteType::Unknown),
    ];

    for (name, site) in &site_types {
        group.bench_function(format!("switch_trace/from_{name}"), |b| {
            b.iter(|| {
                black_box(format_switch_trace(
                    black_box(0),
                    black_box(*site),
                    black_box(1),
                    black_box(SiteType::DeepSeek),
                ))
            })
        });
    }

    // format_switch_trace — 大索引
    group.bench_function("switch_trace_large_idx", |b| {
        b.iter(|| {
            black_box(format_switch_trace(
                black_box(999),
                black_box(SiteType::Zai),
                black_box(1000),
                black_box(SiteType::DeepSeek),
            ))
        })
    });

    // format_failover_failure_trace — 所有 SiteType
    for (name, site) in &site_types {
        group.bench_function(format!("failure_trace/{name}"), |b| {
            b.iter(|| {
                black_box(format_failover_failure_trace(
                    black_box(0),
                    black_box(*site),
                ))
            })
        });
    }

    // format_failover_failure_trace — 大索引
    group.bench_function("failure_trace_large_idx", |b| {
        b.iter(|| {
            black_box(format_failover_failure_trace(
                black_box(999),
                black_box(SiteType::Kimi),
            ))
        })
    });

    // build_error_health_result
    let error_msgs: Vec<(&str, String)> = vec![
        ("short", "connection refused".to_string()),
        ("empty", String::new()),
        ("long", "x".repeat(1000)),
        ("unicode", "连接被拒绝: 超时".to_string()),
    ];

    for (name, msg) in &error_msgs {
        group.bench_function(format!("error_health/{name}"), |b| {
            b.iter(|| black_box(build_error_health_result(black_box(msg.clone()))))
        });
    }

    // 组合: should_failover_decision + classify_failover_failure_reason
    let rate_limited = HealthCheckResult::new(SiteHealthStatus::RateLimited);
    group.bench_function("decision_then_classify", |b| {
        b.iter(|| {
            let should = should_failover_decision(black_box(&rate_limited));
            let reason = if !should {
                classify_failover_failure_reason(black_box(false), black_box(0), black_box(3))
            } else {
                "切换中"
            };
            black_box(reason)
        })
    });

    // SitePerformanceStats summary (通过纯函数组合)
    group.bench_function("stats_summary_chain", |b| {
        b.iter(|| {
            let min = update_min_response_time(black_box(0), black_box(150));
            let min = update_min_response_time(black_box(min), black_box(100));
            let min = update_min_response_time(black_box(min), black_box(200));
            black_box(min)
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
        .output_directory(std::path::Path::new("target/criterion/failover_chat"))
}

criterion_group! {
    name = failover_chat_benches;
    config = configure_criterion();
    targets =
        bench_should_failover_decision,
        bench_classify_failover_failure_reason,
        bench_calculate_health_check_interval,
        bench_update_min_response_time,
        bench_edge_cases,
}

criterion_main!(failover_chat_benches);
