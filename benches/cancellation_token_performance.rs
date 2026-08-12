#![allow(clippy::useless_vec)]

//! Cancellation Token 模块性能基准测试
//!
//! 测试目标:
//! 1. is_cancelled - 取消状态检查 (未取消/手动取消/超时取消)
//! 2. token_creation - 令牌创建 (new/with_timeout/with_deadline)
//! 3. token_clone - 令牌克隆与传播
//! 4. remaining_timeout - 剩余超时查询
//! 5. edge_cases - 边界条件 (CancelError Display/多克隆/cancel传播)

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use forge::cancellation_token::{CancelError, CancellationToken, CancellationTokenSource};
use forge::deadline::Deadline;
use std::time::Duration;

/// 基准测试: is_cancelled
fn bench_is_cancelled(c: &mut Criterion) {
    let mut group = c.benchmark_group("is_cancelled");

    // 未取消
    let source = CancellationTokenSource::new();
    let token = source.token();
    group.bench_function("not_cancelled", |b| {
        b.iter(|| black_box(token.is_cancelled()))
    });

    // 手动取消
    let mut cancelled_source = CancellationTokenSource::new();
    cancelled_source.cancel();
    let cancelled_token = cancelled_source.token();
    group.bench_function("manual_cancelled", |b| {
        b.iter(|| black_box(cancelled_token.is_cancelled()))
    });

    // 超时取消 (已过期)
    let timeout_source = CancellationTokenSource::with_timeout(Duration::from_millis(0));
    let timeout_token = timeout_source.token();
    // 等待过期
    std::thread::sleep(Duration::from_millis(5));
    group.bench_function("timeout_cancelled", |b| {
        b.iter(|| black_box(timeout_token.is_cancelled()))
    });

    // 无 deadline 的 token
    let no_deadline_source = CancellationTokenSource::with_deadline(None);
    let no_deadline_token = no_deadline_source.token();
    group.bench_function("no_deadline", |b| {
        b.iter(|| black_box(no_deadline_token.is_cancelled()))
    });

    group.finish();
}

/// 基准测试: 令牌创建
fn bench_token_creation(c: &mut Criterion) {
    let mut group = c.benchmark_group("token_creation");

    // new (无超时)
    group.bench_function("new", |b| {
        b.iter(|| black_box(CancellationTokenSource::new()))
    });

    // with_timeout
    group.bench_function("with_timeout_5s", |b| {
        b.iter(|| {
            black_box(CancellationTokenSource::with_timeout(black_box(
                Duration::from_secs(5),
            )))
        })
    });

    // with_timeout 0ms
    group.bench_function("with_timeout_0", |b| {
        b.iter(|| {
            black_box(CancellationTokenSource::with_timeout(black_box(
                Duration::from_millis(0),
            )))
        })
    });

    // with_deadline
    let deadline = Deadline::from_millis(10000);
    group.bench_function("with_deadline_some", |b| {
        b.iter(|| {
            black_box(CancellationTokenSource::with_deadline(black_box(Some(
                deadline,
            ))))
        })
    });
    group.bench_function("with_deadline_none", |b| {
        b.iter(|| black_box(CancellationTokenSource::with_deadline(black_box(None))))
    });

    // Default
    group.bench_function("default", |b| {
        b.iter(|| black_box(CancellationTokenSource::default()))
    });

    group.finish();
}

/// 基准测试: token 克隆
fn bench_token_clone(c: &mut Criterion) {
    let mut group = c.benchmark_group("token_clone");

    let source = CancellationTokenSource::new();

    // 单次克隆
    group.bench_function("single_clone", |b| b.iter(|| black_box(source.token())));

    // 多次克隆
    group.throughput(Throughput::Elements(10));
    group.bench_function("clone_10_times", |b| {
        b.iter(|| {
            let tokens: Vec<CancellationToken> = (0..10).map(|_| source.token()).collect();
            black_box(tokens)
        })
    });

    // 克隆后检查
    let token = source.token();
    group.bench_function("clone_then_check", |b| {
        b.iter(|| {
            let cloned = token.clone();
            black_box(cloned.is_cancelled())
        })
    });

    // cancel 传播到克隆
    let mut cancel_source = CancellationTokenSource::new();
    let cloned_token = cancel_source.token();
    cancel_source.cancel();
    group.bench_function("cancel_propagation", |b| {
        b.iter(|| black_box(cloned_token.is_cancelled()))
    });

    group.finish();
}

/// 基准测试: remaining_timeout
fn bench_remaining_timeout(c: &mut Criterion) {
    let mut group = c.benchmark_group("remaining_timeout");

    // 有 deadline
    let source = CancellationTokenSource::with_timeout(Duration::from_secs(10));
    let token = source.token();
    group.bench_function("with_deadline_10s", |b| {
        b.iter(|| black_box(token.remaining_timeout()))
    });

    // 无 deadline
    let no_dl_source = CancellationTokenSource::new();
    let no_dl_token = no_dl_source.token();
    group.bench_function("no_deadline", |b| {
        b.iter(|| black_box(no_dl_token.remaining_timeout()))
    });

    // 已过期 deadline
    let expired_source = CancellationTokenSource::with_timeout(Duration::from_millis(0));
    let expired_token = expired_source.token();
    std::thread::sleep(Duration::from_millis(5));
    group.bench_function("expired_deadline", |b| {
        b.iter(|| black_box(expired_token.remaining_timeout()))
    });

    // source.is_cancelled() (委托到 token)
    group.bench_function("source_is_cancelled", |b| {
        b.iter(|| black_box(source.is_cancelled()))
    });

    group.finish();
}

/// 边界条件基准测试
fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_cases");

    // CancelError Display
    let cancelled_err = CancelError::Cancelled;
    let timeout_err = CancelError::Timeout(Duration::from_millis(5000));
    group.bench_function("display_cancelled", |b| {
        b.iter(|| black_box(format!("{}", cancelled_err)))
    });
    group.bench_function("display_timeout", |b| {
        b.iter(|| black_box(format!("{}", timeout_err)))
    });

    // CancelError 比较
    let err_a = CancelError::Cancelled;
    let err_b = CancelError::Cancelled;
    group.bench_function("eq_same", |b| b.iter(|| black_box(err_a == err_b)));
    group.bench_function("eq_different", |b| {
        b.iter(|| black_box(err_a == CancelError::Timeout(Duration::from_millis(100))))
    });

    // 多次克隆后检查 (Arc 引用计数)
    let source = CancellationTokenSource::new();
    let tokens: Vec<CancellationToken> = (0..50).map(|_| source.token()).collect();
    group.throughput(Throughput::Elements(50));
    group.bench_function("check_50_clones", |b| {
        b.iter(|| {
            for token in &tokens {
                black_box(token.is_cancelled());
            }
        })
    });

    // cancel 后所有克隆都感知
    let mut multi_source = CancellationTokenSource::new();
    let multi_tokens: Vec<CancellationToken> = (0..20).map(|_| multi_source.token()).collect();
    multi_source.cancel();
    group.throughput(Throughput::Elements(20));
    group.bench_function("all_clones_cancelled", |b| {
        b.iter(|| {
            let mut all_cancelled = true;
            for token in &multi_tokens {
                if !token.is_cancelled() {
                    all_cancelled = false;
                }
            }
            black_box(all_cancelled)
        })
    });

    // 组合: 创建 + 克隆 + 检查
    group.bench_function("create_clone_check", |b| {
        b.iter(|| {
            let source = CancellationTokenSource::with_timeout(Duration::from_secs(30));
            let token = source.token();
            let cloned = token.clone();
            let is_cancelled = cloned.is_cancelled();
            let remaining = cloned.remaining_timeout();
            black_box((is_cancelled, remaining))
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
        .output_directory(std::path::Path::new("target/criterion/cancellation_token"))
}

criterion_group! {
    name = cancellation_token_benches;
    config = configure_criterion();
    targets =
        bench_is_cancelled,
        bench_token_creation,
        bench_token_clone,
        bench_remaining_timeout,
        bench_edge_cases,
}

criterion_main!(cancellation_token_benches);
