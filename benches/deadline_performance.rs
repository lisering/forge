#![allow(clippy::useless_vec)]

//! Deadline 模块性能基准测试
//!
//! 测试目标:
//! 1. from_millis/from_duration - 截止时间构建
//! 2. remaining/expired - 状态查询
//! 3. clamp_timeout - 超时钳制
//! 4. sub_deadline - 子截止时间生成
//! 5. edge_cases - 边界条件 (no_deadline/排序/overrun)

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use forge::deadline::{no_deadline, Deadline};
use std::time::Duration;

/// 基准测试: Deadline 构建
fn bench_construction(c: &mut Criterion) {
    let mut group = c.benchmark_group("construction");

    // from_millis
    group.bench_function("from_millis_1000", |b| {
        b.iter(|| black_box(Deadline::from_millis(black_box(1000))))
    });
    group.bench_function("from_millis_0", |b| {
        b.iter(|| black_box(Deadline::from_millis(black_box(0))))
    });
    group.bench_function("from_millis_max", |b| {
        b.iter(|| black_box(Deadline::from_millis(black_box(u64::MAX / 2))))
    });

    // from_duration
    group.bench_function("from_duration_5s", |b| {
        b.iter(|| black_box(Deadline::from_duration(black_box(Duration::from_secs(5)))))
    });
    group.bench_function("from_duration_zero", |b| {
        b.iter(|| black_box(Deadline::from_duration(black_box(Duration::ZERO))))
    });

    // no_deadline
    group.bench_function("no_deadline", |b| b.iter(|| black_box(no_deadline())));

    group.finish();
}

/// 基准测试: remaining / expired
fn bench_remaining_expired(c: &mut Criterion) {
    let mut group = c.benchmark_group("remaining_expired");

    // remaining — 未过期
    let future = Deadline::from_millis(10000);
    group.bench_function("remaining_future", |b| {
        b.iter(|| black_box(future.remaining()))
    });

    // remaining — 已过期
    let expired = Deadline::from_millis(0);
    group.bench_function("remaining_expired", |b| {
        b.iter(|| black_box(expired.remaining()))
    });

    // expired — 未过期
    group.bench_function("expired_false", |b| b.iter(|| black_box(future.expired())));

    // expired — 已过期
    group.bench_function("expired_true", |b| b.iter(|| black_box(expired.expired())));

    // overrun — 未过期
    group.bench_function("overrun_not_expired", |b| {
        b.iter(|| black_box(future.overrun()))
    });

    // overrun — 已过期
    group.bench_function("overrun_expired", |b| {
        b.iter(|| black_box(expired.overrun()))
    });

    // absolute
    group.bench_function("absolute", |b| b.iter(|| black_box(future.absolute())));

    // 组合: remaining + expired
    group.throughput(Throughput::Elements(2));
    group.bench_function("remaining_and_expired", |b| {
        b.iter(|| {
            let r = future.remaining();
            let e = future.expired();
            black_box((r, e))
        })
    });

    group.finish();
}

/// 基准测试: clamp_timeout
fn bench_clamp_timeout(c: &mut Criterion) {
    let mut group = c.benchmark_group("clamp_timeout");

    let deadline = Deadline::from_millis(10000);

    // timeout < remaining
    group.bench_function("smaller_than_remaining", |b| {
        b.iter(|| black_box(deadline.clamp_timeout(black_box(Duration::from_millis(100)))))
    });

    // timeout > remaining
    group.bench_function("larger_than_remaining", |b| {
        b.iter(|| black_box(deadline.clamp_timeout(black_box(Duration::from_secs(60)))))
    });

    // timeout == remaining (近似)
    group.bench_function("equal_to_remaining", |b| {
        b.iter(|| black_box(deadline.clamp_timeout(black_box(Duration::from_secs(10)))))
    });

    // 已过期的 clamp
    let expired = Deadline::from_millis(0);
    group.bench_function("expired_clamp", |b| {
        b.iter(|| black_box(expired.clamp_timeout(black_box(Duration::from_secs(60)))))
    });

    group.finish();
}

/// 基准测试: sub_deadline
fn bench_sub_deadline(c: &mut Criterion) {
    let mut group = c.benchmark_group("sub_deadline");

    let parent = Deadline::from_millis(10000);

    // 子 deadline 在父 deadline 内
    group.bench_function("within_parent", |b| {
        b.iter(|| black_box(parent.sub_deadline(black_box(Duration::from_millis(500)))))
    });

    // 子 deadline 超过父 deadline
    group.bench_function("exceeds_parent", |b| {
        b.iter(|| black_box(parent.sub_deadline(black_box(Duration::from_secs(60)))))
    });

    // 子 deadline == 0
    group.bench_function("zero_additional", |b| {
        b.iter(|| black_box(parent.sub_deadline(black_box(Duration::ZERO))))
    });

    // 嵌套 sub_deadline
    group.bench_function("nested_sub_deadline", |b| {
        b.iter(|| {
            let child = parent.sub_deadline(Duration::from_millis(5000));
            let grandchild = child.sub_deadline(Duration::from_millis(2000));
            black_box(grandchild.remaining())
        })
    });

    group.finish();
}

/// 边界条件基准测试
fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_cases");

    // no_deadline 不过期
    let infinite = no_deadline();
    group.bench_function("no_deadline_expired", |b| {
        b.iter(|| black_box(infinite.expired()))
    });
    group.bench_function("no_deadline_remaining", |b| {
        b.iter(|| black_box(infinite.remaining()))
    });
    group.bench_function("no_deadline_clamp", |b| {
        b.iter(|| black_box(infinite.clamp_timeout(black_box(Duration::from_secs(60)))))
    });

    // Deadline 排序
    let deadlines: Vec<Deadline> = (0..20)
        .map(|i| Deadline::from_millis((i + 1) * 1000))
        .collect();
    group.throughput(Throughput::Elements(deadlines.len() as u64));
    group.bench_function("sort_20_deadlines", |b| {
        b.iter(|| {
            let mut sorted = deadlines.clone();
            sorted.sort();
            black_box(sorted)
        })
    });

    // Deadline 比较
    let d1 = Deadline::from_millis(100);
    let d2 = Deadline::from_millis(200);
    group.bench_function("compare_lt", |b| b.iter(|| black_box(d1 < d2)));
    group.bench_function("compare_eq", |b| {
        b.iter(|| {
            let now = std::time::Instant::now();
            let a = Deadline::from_instant(now);
            let b_ = Deadline::from_instant(now);
            black_box(a == b_)
        })
    });

    // 全方法组合
    let d = Deadline::from_millis(5000);
    group.throughput(Throughput::Elements(5));
    group.bench_function("all_methods", |b| {
        b.iter(|| {
            let r = d.remaining();
            let e = d.expired();
            let o = d.overrun();
            let a = d.absolute();
            let c = d.clamp_timeout(Duration::from_secs(1));
            black_box((r, e, o, a, c))
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
        .output_directory(std::path::Path::new("target/criterion/deadline"))
}

criterion_group! {
    name = deadline_benches;
    config = configure_criterion();
    targets =
        bench_construction,
        bench_remaining_expired,
        bench_clamp_timeout,
        bench_sub_deadline,
        bench_edge_cases,
}

criterion_main!(deadline_benches);
