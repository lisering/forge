#![allow(clippy::useless_vec)]

//! Live Continuation 模块性能基准测试
//!
//! 测试目标:
//! 1. MessageId::from_text - 消息 ID 哈希计算性能
//! 2. compute_message_ids - 批量 ID 计算性能
//! 3. compute_diff - 消息差异计算性能
//! 4. MessageTracker::compute_incremental - 增量计算性能
//! 5. find_duplicates / deduplicate - 去重性能

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use forge::live_continuation;

/// 生成消息列表
fn generate_messages(count: usize, prefix: &str) -> Vec<String> {
    (0..count).map(|i| format!("{prefix}_msg_{i}")).collect()
}

/// 生成消息引用列表
fn generate_message_refs(count: usize, prefix: &str) -> Vec<&'static str> {
    // 使用 leak 来创建 'static 引用 (基准测试专用, 不会清理)
    (0..count)
        .map(|i| Box::leak(format!("{prefix}_msg_{i}").into_boxed_str()) as &'static str)
        .collect()
}

/// 基准测试: MessageId::from_text
fn bench_message_id_from_text(c: &mut Criterion) {
    let mut group = c.benchmark_group("message_id_from_text");

    let long_text = "x".repeat(1000);
    let test_cases = vec![
        ("short", "hello"),
        ("medium", "This is a medium length message for benchmarking"),
        ("long", long_text.as_str()),
    ];

    for (name, text) in &test_cases {
        group.bench_function(*name, |b| {
            b.iter(|| black_box(live_continuation::MessageId::from_text(black_box(text))))
        });
    }
    group.finish();
}

/// 基准测试: compute_message_ids
fn bench_compute_message_ids(c: &mut Criterion) {
    let mut group = c.benchmark_group("compute_message_ids");

    let sizes: Vec<usize> = vec![10, 100, 1_000, 10_000];

    for size in &sizes {
        group.throughput(Throughput::Elements(*size as u64));
        let messages = generate_message_refs(*size, "ctx");

        group.bench_with_input(BenchmarkId::new("ids", size), &messages, |b, msgs| {
            b.iter(|| black_box(live_continuation::compute_message_ids(black_box(msgs))))
        });
    }
    group.finish();
}

/// 基准测试: compute_diff
fn bench_compute_diff(c: &mut Criterion) {
    let mut group = c.benchmark_group("compute_diff");

    let sizes: Vec<usize> = vec![10, 100, 1_000, 10_000];

    for size in &sizes {
        group.throughput(Throughput::Elements(*size as u64));
        let sent = generate_message_refs(*size, "sent");
        let mut messages = sent.clone();
        // 添加 20% 新消息
        let new_msgs = generate_message_refs(*size / 5, "new");
        messages.extend(new_msgs);

        group.bench_with_input(
            BenchmarkId::new("diff", size),
            &(&sent, &messages),
            |b, (sent, messages)| {
                b.iter(|| {
                    black_box(live_continuation::compute_diff(
                        black_box(sent),
                        black_box(messages),
                    ))
                })
            },
        );
    }
    group.finish();
}

/// 基准测试: MessageTracker::compute_incremental
fn bench_compute_incremental(c: &mut Criterion) {
    let mut group = c.benchmark_group("compute_incremental");

    let sizes: Vec<usize> = vec![10, 100, 1_000, 10_000];

    for size in &sizes {
        group.throughput(Throughput::Elements(*size as u64));
        let initial_msgs = generate_messages(*size, "initial");
        let mut updated_msgs = initial_msgs.clone();
        // 添加 10% 新消息
        for i in 0..(*size / 10) {
            updated_msgs.push(format!("new_msg_{i}"));
        }

        group.bench_with_input(
            BenchmarkId::new("incremental", size),
            &(&initial_msgs, &updated_msgs),
            |b, (initial, updated)| {
                b.iter_batched(
                    || {
                        let mut tracker = live_continuation::MessageTracker::new();
                        tracker.register_many_owned(initial);
                        tracker
                    },
                    |mut tracker| black_box(tracker.compute_incremental(black_box(updated))),
                    criterion::BatchSize::SmallInput,
                )
            },
        );
    }
    group.finish();
}

/// 基准测试: find_duplicates / deduplicate
fn bench_dedup_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("dedup_operations");

    let sizes: Vec<usize> = vec![10, 100, 1_000, 10_000];

    for &size in &sizes {
        group.throughput(Throughput::Elements(size as u64));
        // 创建有 30% 重复的消息列表
        let messages: Vec<&str> = (0..size)
            .map(|i| {
                let idx = i % (size * 7 / 10);
                Box::leak(format!("msg_{idx}").into_boxed_str()) as &'static str
            })
            .collect();

        group.bench_with_input(
            BenchmarkId::new("find_duplicates", size),
            &messages,
            |b, msgs| b.iter(|| black_box(live_continuation::find_duplicates(black_box(msgs)))),
        );

        group.bench_with_input(
            BenchmarkId::new("deduplicate", size),
            &messages,
            |b, msgs| b.iter(|| black_box(live_continuation::deduplicate(black_box(msgs)))),
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
        .output_directory(std::path::Path::new("target/criterion/live_continuation"))
}

criterion_group! {
    name = live_continuation_benches;
    config = configure_criterion();
    targets =
        bench_message_id_from_text,
        bench_compute_message_ids,
        bench_compute_diff,
        bench_compute_incremental,
        bench_dedup_operations,
}

criterion_main!(live_continuation_benches);
