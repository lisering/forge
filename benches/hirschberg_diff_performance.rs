//! # Hirschberg Diff 算法性能基准测试 (Session 125)
//!
//! 覆盖 `compute_line_diff_hirschberg` 和相关 diff 算法的性能特征。
//!
//! ## 函数组
//!
//! 1. `hirschberg_basic` — 基础操作 (空/单行/相同/不同)
//! 2. `hirschberg_vs_lcs` — Hirschberg vs LCS 算法对比
//! 3. `hirschberg_vs_myers` — Hirschberg vs Myers 算法对比
//! 4. `hirschberg_scaling` — 不同规模输入的扩展性
//! 5. `format_diff_unified_perf` — format_diff_unified_with_options 性能
//! 6. `edge_cases` — 边界情况

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use forge::extract::{
    compute_line_diff_hirschberg, compute_line_diff_lcs, compute_line_diff_myers,
    compute_line_diff_with_algorithm, format_diff_unified_with_options, DiffAlgorithm,
};

/// 生成 n 行的文本
fn generate_text(n: usize) -> String {
    (0..n).map(|i| format!("line {i}\n")).collect()
}

/// 生成有少量修改的文本 (每 k 行修改 1 行)
fn generate_modified(text: &str, k: usize) -> String {
    text.lines()
        .enumerate()
        .map(|(i, line)| {
            if i % k == 0 {
                format!("MODIFIED {line}")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 生成插入行后的文本
fn generate_with_insertions(text: &str, k: usize) -> String {
    let mut result = String::new();
    for (i, line) in text.lines().enumerate() {
        result.push_str(line);
        result.push('\n');
        if i % k == 0 {
            result.push_str(&format!("inserted line {i}\n"));
        }
    }
    result
}

fn hirschberg_basic(c: &mut Criterion) {
    let mut group = c.benchmark_group("hirschberg_basic");

    // 空输入
    group.bench_function("empty", |b| {
        b.iter(|| {
            let diffs = compute_line_diff_hirschberg(black_box(""), black_box(""));
            black_box(diffs);
        })
    });

    // 单行相同
    group.bench_function("single_same", |b| {
        b.iter(|| {
            let diffs = compute_line_diff_hirschberg(black_box("hello"), black_box("hello"));
            black_box(diffs);
        })
    });

    // 单行不同
    group.bench_function("single_diff", |b| {
        b.iter(|| {
            let diffs = compute_line_diff_hirschberg(black_box("hello"), black_box("world"));
            black_box(diffs);
        })
    });

    // 完全不同
    group.bench_function("completely_different", |b| {
        let orig = generate_text(50);
        let fixed: String = (0..50).map(|i| format!("other {i}\n")).collect();
        b.iter(|| {
            let diffs = compute_line_diff_hirschberg(black_box(&orig), black_box(&fixed));
            black_box(diffs);
        })
    });

    group.finish();
}

fn hirschberg_vs_lcs(c: &mut Criterion) {
    let mut group = c.benchmark_group("hirschberg_vs_lcs");

    for size in [50, 100, 200, 500] {
        let orig = generate_text(size);
        let fixed = generate_modified(&orig, 5);

        group.bench_with_input(BenchmarkId::new("hirschberg", size), &size, |b, _| {
            b.iter(|| {
                let diffs = compute_line_diff_hirschberg(black_box(&orig), black_box(&fixed));
                black_box(diffs);
            })
        });

        group.bench_with_input(BenchmarkId::new("lcs", size), &size, |b, _| {
            b.iter(|| {
                let diffs = compute_line_diff_lcs(black_box(&orig), black_box(&fixed));
                black_box(diffs);
            })
        });
    }

    group.finish();
}

fn hirschberg_vs_myers(c: &mut Criterion) {
    let mut group = c.benchmark_group("hirschberg_vs_myers");

    for size in [50, 100, 200, 500] {
        let orig = generate_text(size);
        let fixed = generate_with_insertions(&orig, 10);

        group.bench_with_input(BenchmarkId::new("hirschberg", size), &size, |b, _| {
            b.iter(|| {
                let diffs = compute_line_diff_hirschberg(black_box(&orig), black_box(&fixed));
                black_box(diffs);
            })
        });

        group.bench_with_input(BenchmarkId::new("myers", size), &size, |b, _| {
            b.iter(|| {
                let diffs = compute_line_diff_myers(black_box(&orig), black_box(&fixed));
                black_box(diffs);
            })
        });
    }

    group.finish();
}

fn hirschberg_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("hirschberg_scaling");

    for size in [100, 500, 1000, 2000, 5000] {
        let orig = generate_text(size);
        let fixed = generate_modified(&orig, 10);

        group.bench_with_input(BenchmarkId::new("sparse_changes", size), &size, |b, _| {
            b.iter(|| {
                let diffs = compute_line_diff_hirschberg(black_box(&orig), black_box(&fixed));
                black_box(diffs);
            })
        });

        // 大量插入
        let fixed2 = generate_with_insertions(&orig, 5);
        group.bench_with_input(BenchmarkId::new("many_insertions", size), &size, |b, _| {
            b.iter(|| {
                let diffs = compute_line_diff_hirschberg(black_box(&orig), black_box(&fixed2));
                black_box(diffs);
            })
        });
    }

    group.finish();
}

fn format_diff_unified_perf(c: &mut Criterion) {
    let mut group = c.benchmark_group("format_diff_unified_perf");

    for size in [50, 200, 500, 1000] {
        let orig = generate_text(size);
        let fixed = generate_modified(&orig, 5);

        group.bench_with_input(BenchmarkId::new("default_context", size), &size, |b, _| {
            b.iter(|| {
                let diff = format_diff_unified_with_options(
                    black_box(&orig),
                    black_box(&fixed),
                    "original.rs",
                    "fixed.rs",
                    3,
                );
                black_box(diff);
            })
        });

        group.bench_with_input(BenchmarkId::new("zero_context", size), &size, |b, _| {
            b.iter(|| {
                let diff = format_diff_unified_with_options(
                    black_box(&orig),
                    black_box(&fixed),
                    "original.rs",
                    "fixed.rs",
                    0,
                );
                black_box(diff);
            })
        });

        group.bench_with_input(BenchmarkId::new("large_context", size), &size, |b, _| {
            b.iter(|| {
                let diff = format_diff_unified_with_options(
                    black_box(&orig),
                    black_box(&fixed),
                    "original.rs",
                    "fixed.rs",
                    20,
                );
                black_box(diff);
            })
        });
    }

    group.finish();
}

fn edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("hirschberg_edge_cases");

    // 空对非空
    group.bench_function("empty_to_nonempty", |b| {
        let text = generate_text(100);
        b.iter(|| {
            let diffs = compute_line_diff_hirschberg(black_box(""), black_box(&text));
            black_box(diffs);
        })
    });

    // 非空对空
    group.bench_function("nonempty_to_empty", |b| {
        let text = generate_text(100);
        b.iter(|| {
            let diffs = compute_line_diff_hirschberg(black_box(&text), black_box(""));
            black_box(diffs);
        })
    });

    // 完全相同 (无差异)
    group.bench_function("identical", |b| {
        let text = generate_text(500);
        b.iter(|| {
            let diffs = compute_line_diff_hirschberg(black_box(&text), black_box(&text));
            black_box(diffs);
        })
    });

    // Auto 算法选择 — 大输入应选 Hirschberg
    group.bench_function("auto_large_selects_hirschberg", |b| {
        let orig = generate_text(200);
        let fixed = generate_modified(&orig, 5);
        b.iter(|| {
            let diffs = compute_line_diff_with_algorithm(
                black_box(&orig),
                black_box(&fixed),
                DiffAlgorithm::Auto,
            );
            black_box(diffs);
        })
    });

    // 纯追加 (末尾添加行)
    group.bench_function("pure_append", |b| {
        let orig = generate_text(500);
        let mut fixed = orig.clone();
        for i in 500..600 {
            fixed.push_str(&format!("line {i}\n"));
        }
        b.iter(|| {
            let diffs = compute_line_diff_hirschberg(black_box(&orig), black_box(&fixed));
            black_box(diffs);
        })
    });

    // 纯删除 (删除后半部分)
    group.bench_function("pure_delete", |b| {
        let orig = generate_text(500);
        let fixed: String = orig.lines().take(250).map(|l| format!("{l}\n")).collect();
        b.iter(|| {
            let diffs = compute_line_diff_hirschberg(black_box(&orig), black_box(&fixed));
            black_box(diffs);
        })
    });

    group.finish();
}

criterion_group!(
    benches,
    hirschberg_basic,
    hirschberg_vs_lcs,
    hirschberg_vs_myers,
    hirschberg_scaling,
    format_diff_unified_perf,
    edge_cases,
);
criterion_main!(benches);
