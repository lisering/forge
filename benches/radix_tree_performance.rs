#![allow(clippy::useless_vec)]

//! Radix Tree 模块性能基准测试
//!
//! 测试目标:
//! 1. MessageFingerprint::from_text - 指纹计算性能
//! 2. compute_fingerprints - 批量指纹计算性能
//! 3. RadixTree::insert/lookup - 树操作性能
//! 4. longest_prefix_len - 最长前缀查找性能
//! 5. ConversationTracker::compute_delta - 增量计算性能

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use forge::radix_tree;

/// 生成消息指纹序列
fn generate_fingerprints(count: usize, prefix: &str) -> Vec<radix_tree::MessageFingerprint> {
    (0..count)
        .map(|i| radix_tree::MessageFingerprint::from_text(&format!("{prefix}_msg_{i}")))
        .collect()
}

/// 基准测试: MessageFingerprint::from_text
fn bench_fingerprint_from_text(c: &mut Criterion) {
    let mut group = c.benchmark_group("fingerprint_from_text");

    let long_text = "x".repeat(1000);
    let test_cases = vec![
        ("short", "hello"),
        ("medium", "This is a medium length message for benchmarking"),
        ("long", long_text.as_str()),
    ];

    for (name, text) in &test_cases {
        group.bench_function(*name, |b| {
            b.iter(|| black_box(radix_tree::MessageFingerprint::from_text(black_box(text))))
        });
    }
    group.finish();
}

/// 基准测试: compute_fingerprints
fn bench_compute_fingerprints(c: &mut Criterion) {
    let mut group = c.benchmark_group("compute_fingerprints");

    let sizes: Vec<usize> = vec![10, 100, 1_000, 10_000];

    for size in &sizes {
        group.throughput(Throughput::Elements(*size as u64));
        let messages: Vec<String> = (0..*size).map(|i| format!("msg_{i}")).collect();

        group.bench_with_input(
            BenchmarkId::new("fingerprints", size),
            &messages,
            |b, msgs| b.iter(|| black_box(radix_tree::compute_fingerprints_owned(black_box(msgs)))),
        );
    }
    group.finish();
}

/// 基准测试: RadixTree insert + lookup
fn bench_tree_insert_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("tree_insert_lookup");

    let sizes: Vec<usize> = vec![10, 100, 1_000];

    for size in &sizes {
        group.throughput(Throughput::Elements(*size as u64));
        let keys: Vec<Vec<radix_tree::MessageFingerprint>> = (0..*size)
            .map(|i| generate_fingerprints(5, &format!("seq{i}")))
            .collect();

        // insert 基准
        group.bench_with_input(BenchmarkId::new("insert", size), &keys, |b, keys| {
            b.iter(|| {
                let mut tree = radix_tree::RadixTree::new();
                for (i, key) in keys.iter().enumerate() {
                    tree.insert(black_box(key), i as u64);
                }
                black_box(tree.len())
            })
        });

        // lookup 基准 (预先构建树)
        let mut tree = radix_tree::RadixTree::new();
        for (i, key) in keys.iter().enumerate() {
            tree.insert(key, i as u64);
        }
        group.bench_with_input(BenchmarkId::new("lookup", size), &keys, |b, keys| {
            b.iter(|| {
                let mut found = 0;
                for key in keys {
                    if tree.lookup(black_box(key)).is_some() {
                        found += 1;
                    }
                }
                black_box(found)
            })
        });
    }
    group.finish();
}

/// 基准测试: longest_prefix_len
fn bench_longest_prefix(c: &mut Criterion) {
    let mut group = c.benchmark_group("longest_prefix_len");

    let sizes: Vec<usize> = vec![10, 100, 1_000];

    for size in &sizes {
        group.throughput(Throughput::Elements(*size as u64));
        // 构建有公共前缀的树
        let mut tree = radix_tree::RadixTree::new();
        for i in 0..*size {
            let key = generate_fingerprints(10, &format!("seq{i}"));
            tree.insert(&key, i as u64);
        }

        // 查询: 与第一个序列有 8 个公共前缀
        let query = generate_fingerprints(10, "seq0");
        group.bench_function(format!("prefix_{}", size), |b| {
            b.iter(|| black_box(tree.longest_prefix_len(black_box(&query))))
        });
    }
    group.finish();
}

/// 基准测试: ConversationTracker compute_delta
fn bench_compute_delta(c: &mut Criterion) {
    let mut group = c.benchmark_group("conversation_compute_delta");

    let sizes: Vec<usize> = vec![10, 100, 1_000, 10_000];

    for size in &sizes {
        group.throughput(Throughput::Elements(*size as u64));
        let initial: Vec<String> = (0..*size).map(|i| format!("msg_{i}")).collect();
        let mut updated = initial.clone();
        updated.push(format!("msg_{}", *size));
        updated.push(format!("msg_{}", *size + 1));

        group.bench_with_input(
            BenchmarkId::new("delta", size),
            &(&initial, &updated),
            |b, (initial, updated)| {
                b.iter_batched(
                    || {
                        let mut tracker = radix_tree::ConversationTracker::new();
                        tracker.mark_sent(initial);
                        tracker
                    },
                    |tracker| black_box(tracker.compute_delta(black_box(updated))),
                    criterion::BatchSize::SmallInput,
                )
            },
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
        .output_directory(std::path::Path::new("target/criterion/radix_tree"))
}

criterion_group! {
    name = radix_tree_benches;
    config = configure_criterion();
    targets =
        bench_fingerprint_from_text,
        bench_compute_fingerprints,
        bench_tree_insert_lookup,
        bench_longest_prefix,
        bench_compute_delta,
}

criterion_main!(radix_tree_benches);
