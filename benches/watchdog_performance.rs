#![allow(clippy::useless_vec)]

//! Watchdog 模块性能基准测试
//!
//! 测试目标:
//! 1. event_severity - 事件严重级别查询性能
//! 2. should_handle_event - 事件匹配逻辑性能
//! 3. should_trigger_auto_recovery - 自动恢复触发判断性能
//! 4. event_priority - 事件优先级计算性能
//! 5. edge_cases - 边界条件性能 (全部变体/Custom事件/空列表)

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use forge::watchdog::{
    event_priority, should_handle_event, should_trigger_auto_recovery, WatchEvent,
};

/// 所有 WatchEvent 变体 (用于遍历测试)
fn all_events() -> Vec<WatchEvent> {
    vec![
        WatchEvent::ChromeCrashed,
        WatchEvent::ChromeUnreachable,
        WatchEvent::WebSocketDisconnected("ws://localhost:9222".to_string()),
        WatchEvent::TabClosed,
        WatchEvent::SiteUnhealthy("chat.z.ai".to_string()),
        WatchEvent::CaptchaDetected,
        WatchEvent::PopupDetected("alert dialog".to_string()),
        WatchEvent::DomChanged,
        WatchEvent::ResponseTimeout,
        WatchEvent::LoopDetected,
        WatchEvent::RecoveryStarted,
        WatchEvent::RecoveryCompleted(true),
        WatchEvent::RecoveryCompleted(false),
        WatchEvent::Custom("custom_event".to_string()),
    ]
}

/// 基准测试: WatchEvent::severity
fn bench_event_severity(c: &mut Criterion) {
    let mut group = c.benchmark_group("event_severity");

    let events = all_events();
    let count = events.len() as u64;

    // 单个事件
    group.bench_function("single_critical", |b| {
        b.iter(|| black_box(WatchEvent::ChromeCrashed.severity()))
    });
    group.bench_function("single_info", |b| {
        b.iter(|| black_box(WatchEvent::DomChanged.severity()))
    });

    // 全部变体遍历
    group.throughput(Throughput::Elements(count));
    group.bench_function("all_variants", |b| {
        b.iter(|| {
            for event in &events {
                black_box(event.severity());
            }
        })
    });

    // needs_immediate_recovery
    group.bench_function("needs_recovery_critical", |b| {
        b.iter(|| black_box(WatchEvent::ChromeCrashed.needs_immediate_recovery()))
    });
    group.bench_function("needs_recovery_info", |b| {
        b.iter(|| black_box(WatchEvent::DomChanged.needs_immediate_recovery()))
    });

    // name() 方法
    group.throughput(Throughput::Elements(count));
    group.bench_function("name_all_variants", |b| {
        b.iter(|| {
            for event in &events {
                black_box(event.name());
            }
        })
    });

    group.finish();
}

/// 基准测试: should_handle_event
fn bench_should_handle_event(c: &mut Criterion) {
    let mut group = c.benchmark_group("should_handle_event");

    let listens_to = vec![
        WatchEvent::ChromeCrashed,
        WatchEvent::TabClosed,
        WatchEvent::ResponseTimeout,
    ];

    // 精确匹配
    group.bench_function("exact_match", |b| {
        b.iter(|| {
            black_box(should_handle_event(
                black_box(&listens_to),
                black_box(&WatchEvent::ChromeCrashed),
            ))
        })
    });

    // 不匹配
    group.bench_function("no_match", |b| {
        b.iter(|| {
            black_box(should_handle_event(
                black_box(&listens_to),
                black_box(&WatchEvent::DomChanged),
            ))
        })
    });

    // 空列表
    let empty: Vec<WatchEvent> = vec![];
    group.bench_function("empty_listens_to", |b| {
        b.iter(|| {
            black_box(should_handle_event(
                black_box(&empty),
                black_box(&WatchEvent::ChromeCrashed),
            ))
        })
    });

    // 大列表 (模拟多 watchdog 场景)
    let large_listens_to: Vec<WatchEvent> = all_events();
    group.throughput(Throughput::Elements(large_listens_to.len() as u64));
    group.bench_function("large_listens_match", |b| {
        b.iter(|| {
            black_box(should_handle_event(
                black_box(&large_listens_to),
                black_box(&WatchEvent::RecoveryStarted),
            ))
        })
    });

    // Custom 事件匹配
    let custom_listens = vec![WatchEvent::Custom("custom_event".to_string())];
    group.bench_function("custom_match", |b| {
        b.iter(|| {
            black_box(should_handle_event(
                black_box(&custom_listens),
                black_box(&WatchEvent::Custom("custom_event".to_string())),
            ))
        })
    });

    group.finish();
}

/// 基准测试: should_trigger_auto_recovery
fn bench_should_trigger_auto_recovery(c: &mut Criterion) {
    let mut group = c.benchmark_group("should_trigger_auto_recovery");

    let events = all_events();
    let count = events.len() as u64;

    // 单个事件
    group.bench_function("critical", |b| {
        b.iter(|| {
            black_box(should_trigger_auto_recovery(black_box(
                &WatchEvent::ChromeCrashed,
            )))
        })
    });
    group.bench_function("warning", |b| {
        b.iter(|| {
            black_box(should_trigger_auto_recovery(black_box(
                &WatchEvent::TabClosed,
            )))
        })
    });
    group.bench_function("info", |b| {
        b.iter(|| {
            black_box(should_trigger_auto_recovery(black_box(
                &WatchEvent::DomChanged,
            )))
        })
    });

    // 全部变体
    group.throughput(Throughput::Elements(count));
    group.bench_function("all_variants", |b| {
        b.iter(|| {
            for event in &events {
                black_box(should_trigger_auto_recovery(black_box(event)));
            }
        })
    });

    group.finish();
}

/// 基准测试: event_priority
fn bench_event_priority(c: &mut Criterion) {
    let mut group = c.benchmark_group("event_priority");

    let events = all_events();
    let count = events.len() as u64;

    // 单个事件
    group.bench_function("chrome_crashed", |b| {
        b.iter(|| black_box(event_priority(black_box(&WatchEvent::ChromeCrashed))))
    });
    group.bench_function("recovery_completed_true", |b| {
        b.iter(|| {
            black_box(event_priority(black_box(&WatchEvent::RecoveryCompleted(
                true,
            ))))
        })
    });
    group.bench_function("recovery_completed_false", |b| {
        b.iter(|| {
            black_box(event_priority(black_box(&WatchEvent::RecoveryCompleted(
                false,
            ))))
        })
    });
    group.bench_function("custom", |b| {
        b.iter(|| {
            black_box(event_priority(black_box(&WatchEvent::Custom(
                "test".to_string(),
            ))))
        })
    });

    // 全部变体
    group.throughput(Throughput::Elements(count));
    group.bench_function("all_variants", |b| {
        b.iter(|| {
            for event in &events {
                black_box(event_priority(black_box(event)));
            }
        })
    });

    // 排序场景: 收集优先级并排序
    group.throughput(Throughput::Elements(count));
    group.bench_function("collect_and_sort", |b| {
        b.iter(|| {
            let mut priorities: Vec<u32> = events.iter().map(event_priority).collect();
            priorities.sort();
            black_box(priorities)
        })
    });

    group.finish();
}

/// 边界条件基准测试
fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_cases");

    let events = all_events();

    // 全事件 severity/name/priority 组合
    group.bench_function("severity_name_priority_all", |b| {
        b.iter(|| {
            for event in &events {
                let s = event.severity();
                let n = event.name();
                let p = event_priority(event);
                black_box((s, n, p));
            }
        })
    });

    // Custom 事件不同名称
    let custom_events: Vec<WatchEvent> = (0..20)
        .map(|i| WatchEvent::Custom(format!("custom_{i}")))
        .collect();
    group.bench_function("custom_events_20", |b| {
        b.iter(|| {
            for event in &custom_events {
                black_box(event.name());
                black_box(event.severity());
            }
        })
    });

    // WebSocketDisconnected 不同 URL
    let ws_events: Vec<WatchEvent> = (0..10)
        .map(|i| WatchEvent::WebSocketDisconnected(format!("ws://localhost:922{i}")))
        .collect();
    group.bench_function("ws_events_10", |b| {
        b.iter(|| {
            for event in &ws_events {
                black_box(event.severity());
                black_box(event.needs_immediate_recovery());
            }
        })
    });

    // should_handle_event: 全匹配 vs 全不匹配
    let all_events_list = all_events();
    group.bench_function("match_all_events", |b| {
        b.iter(|| {
            for event in &all_events_list {
                black_box(should_handle_event(&all_events_list, event));
            }
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
        .output_directory(std::path::Path::new("target/criterion/watchdog"))
}

criterion_group! {
    name = watchdog_benches;
    config = configure_criterion();
    targets =
        bench_event_severity,
        bench_should_handle_event,
        bench_should_trigger_auto_recovery,
        bench_event_priority,
        bench_edge_cases,
}

criterion_main!(watchdog_benches);
