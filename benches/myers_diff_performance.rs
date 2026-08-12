//! Myers diff 算法性能基准测试 (Session 126)
//!
//! 覆盖 `compute_line_diff_myers` 的各种场景:
//! - 基本操作 (空/单行/相同/不同)
//! - 规模扩展 (10/100/500/1000 行)
//! - 纯追加/纯删除/混合修改
//! - 与 LCS 算法对比
//! - 边界情况

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use forge::extract::{
    compute_line_diff_lcs, compute_line_diff_myers, compute_line_diff_with_algorithm,
    format_diff_summary, DiffAlgorithm, LineDiffType,
};

/// 基本操作基准测试
fn myers_basic(c: &mut Criterion) {
    let mut group = c.benchmark_group("myers_basic");

    group.bench_function("empty_both", |b| {
        b.iter(|| {
            let diffs = compute_line_diff_myers("", "");
            black_box(diffs);
        })
    });

    group.bench_function("empty_original", |b| {
        b.iter(|| {
            let diffs = compute_line_diff_myers("", "a\nb\nc\nd\ne");
            black_box(diffs);
        })
    });

    group.bench_function("empty_fixed", |b| {
        b.iter(|| {
            let diffs = compute_line_diff_myers("a\nb\nc\nd\ne", "");
            black_box(diffs);
        })
    });

    group.bench_function("identical", |b| {
        let code = "fn foo() {\n    let x = 42;\n    println!(\"{}\", x);\n}\n";
        b.iter(|| {
            let diffs = compute_line_diff_myers(code, code);
            black_box(diffs);
        })
    });

    group.bench_function("single_line_change", |b| {
        b.iter(|| {
            let diffs = compute_line_diff_myers("let x = 1;", "let x = 2;");
            black_box(diffs);
        })
    });

    group.bench_function("single_insertion", |b| {
        b.iter(|| {
            let diffs =
                compute_line_diff_myers("fn foo() {}\n", "fn foo() {\n    let x = 42;\n}\n");
            black_box(diffs);
        })
    });

    group.bench_function("single_deletion", |b| {
        b.iter(|| {
            let diffs =
                compute_line_diff_myers("fn foo() {\n    let x = 42;\n}\n", "fn foo() {}\n");
            black_box(diffs);
        })
    });

    group.finish();
}

/// 规模扩展基准测试
fn myers_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("myers_scaling");
    let sizes = [10, 50, 100, 500, 1000];

    for size in sizes {
        let original: String = (0..size).map(|i| format!("line {i}\n")).collect();

        // 纯追加: 在末尾添加 10% 的行
        let mut fixed_add = original.clone();
        for i in 0..(size / 10).max(1) {
            fixed_add.push_str(&format!("added line {i}\n"));
        }
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(
            BenchmarkId::new("pure_addition", size),
            &fixed_add,
            |b, fixed| {
                b.iter(|| {
                    let diffs = compute_line_diff_myers(&original, fixed);
                    black_box(diffs);
                })
            },
        );

        // 纯删除: 删除末尾 10% 的行
        let lines: Vec<&str> = original.lines().collect();
        let fixed_del: String = lines[..size - (size / 10).max(1)]
            .iter()
            .map(|l| format!("{l}\n"))
            .collect();
        group.bench_with_input(
            BenchmarkId::new("pure_deletion", size),
            &fixed_del,
            |b, fixed| {
                b.iter(|| {
                    let diffs = compute_line_diff_myers(&original, fixed);
                    black_box(diffs);
                })
            },
        );

        // 混合修改: 修改 10% 的行 (每隔 10 行修改一行)
        let mut fixed_mixed = String::new();
        for (i, line) in lines.iter().enumerate() {
            if i % 10 == 0 && i > 0 {
                fixed_mixed.push_str(&format!("MODIFIED {line}\n"));
            } else {
                fixed_mixed.push_str(&format!("{line}\n"));
            }
        }
        group.bench_with_input(
            BenchmarkId::new("mixed_modification", size),
            &fixed_mixed,
            |b, fixed| {
                b.iter(|| {
                    let diffs = compute_line_diff_myers(&original, fixed);
                    black_box(diffs);
                })
            },
        );
    }

    group.finish();
}

/// Myers vs LCS 算法对比
fn myers_vs_lcs(c: &mut Criterion) {
    let mut group = c.benchmark_group("myers_vs_lcs");
    let sizes = [50, 200, 500];

    for size in sizes {
        let original: String = (0..size).map(|i| format!("line {i}\n")).collect();

        // 稀疏差异: 只修改 2 行
        let mut fixed_sparse = original.clone();
        fixed_sparse = fixed_sparse.replace("line 1\n", "line CHANGED1\n");
        fixed_sparse = fixed_sparse.replace("line 10\n", "line CHANGED2\n");

        group.bench_with_input(
            BenchmarkId::new("myers_sparse", size),
            &fixed_sparse,
            |b, fixed| {
                b.iter(|| {
                    let diffs = compute_line_diff_myers(&original, fixed);
                    black_box(diffs);
                })
            },
        );

        group.bench_with_input(
            BenchmarkId::new("lcs_sparse", size),
            &fixed_sparse,
            |b, fixed| {
                b.iter(|| {
                    let diffs = compute_line_diff_lcs(&original, fixed);
                    black_box(diffs);
                })
            },
        );

        // 密集差异: 修改 50% 的行
        let lines: Vec<&str> = original.lines().collect();
        let mut fixed_dense = String::new();
        for (i, line) in lines.iter().enumerate() {
            if i % 2 == 0 {
                fixed_dense.push_str(&format!("CHANGED {line}\n"));
            } else {
                fixed_dense.push_str(&format!("{line}\n"));
            }
        }

        group.bench_with_input(
            BenchmarkId::new("myers_dense", size),
            &fixed_dense,
            |b, fixed| {
                b.iter(|| {
                    let diffs = compute_line_diff_myers(&original, fixed);
                    black_box(diffs);
                })
            },
        );

        group.bench_with_input(
            BenchmarkId::new("lcs_dense", size),
            &fixed_dense,
            |b, fixed| {
                b.iter(|| {
                    let diffs = compute_line_diff_lcs(&original, fixed);
                    black_box(diffs);
                })
            },
        );
    }

    group.finish();
}

/// format_diff_summary 性能
fn myers_diff_summary(c: &mut Criterion) {
    let mut group = c.benchmark_group("myers_diff_summary");

    group.bench_function("small_diff", |b| {
        let original = "fn foo() {\n}\n";
        let fixed = "fn foo() {\n    let x = 42;\n}\n";
        b.iter(|| {
            let diffs = compute_line_diff_myers(original, fixed);
            let summary = format_diff_summary(&diffs);
            black_box(summary);
        })
    });

    group.bench_function("medium_diff", |b| {
        let original: String = (0..50).map(|i| format!("line {i}\n")).collect();
        let mut fixed = original.clone();
        for i in 0..5 {
            fixed = fixed.replace(&format!("line {i}\n"), &format!("changed {i}\n"));
        }
        b.iter(|| {
            let diffs = compute_line_diff_myers(&original, &fixed);
            let summary = format_diff_summary(&diffs);
            black_box(summary);
        })
    });

    group.bench_function("large_diff", |b| {
        let original: String = (0..500).map(|i| format!("line {i}\n")).collect();
        let mut fixed = original.clone();
        for i in 0..50 {
            fixed = fixed.replace(&format!("line {i}\n"), &format!("changed {i}\n"));
        }
        b.iter(|| {
            let diffs = compute_line_diff_myers(&original, &fixed);
            let summary = format_diff_summary(&diffs);
            black_box(summary);
        })
    });

    group.finish();
}

/// 边界情况
fn myers_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("myers_edge_cases");

    // 单字符差异
    group.bench_function("single_char_diff", |b| {
        b.iter(|| {
            let diffs = compute_line_diff_myers("a", "b");
            black_box(diffs);
        })
    });

    // 完全不同的内容
    group.bench_function("completely_different", |b| {
        let original: String = (0..100).map(|i| format!("orig {i}\n")).collect();
        let fixed: String = (0..100).map(|i| format!("fixed {i}\n")).collect();
        b.iter(|| {
            let diffs = compute_line_diff_myers(&original, &fixed);
            black_box(diffs);
        })
    });

    // 重复行
    group.bench_function("repeated_lines", |b| {
        let original = "same\nsame\nsame\nsame\nsame\n";
        let fixed = "same\nsame\nchanged\nsame\nsame\n";
        b.iter(|| {
            let diffs = compute_line_diff_myers(original, fixed);
            black_box(diffs);
        })
    });

    // 长行内容
    group.bench_function("long_lines", |b| {
        let long_line = "x".repeat(1000);
        let original = format!("{long_line}\n{long_line}\n{long_line}\n");
        let fixed = format!("{long_line}\nchanged\n{long_line}\n");
        b.iter(|| {
            let diffs = compute_line_diff_myers(&original, &fixed);
            black_box(diffs);
        })
    });

    // Auto 算法选择验证
    group.bench_function("auto_vs_myers_small", |b| {
        let original = "fn foo() {\n}\n";
        let fixed = "fn foo() {\n    let x = 42;\n}\n";
        b.iter(|| {
            let myers_diffs = compute_line_diff_myers(original, fixed);
            let auto_diffs = compute_line_diff_with_algorithm(original, fixed, DiffAlgorithm::Auto);
            assert_eq!(
                myers_diffs
                    .iter()
                    .filter(|d| d.diff_type == LineDiffType::Added)
                    .count(),
                auto_diffs
                    .iter()
                    .filter(|d| d.diff_type == LineDiffType::Added)
                    .count(),
            );
            black_box(myers_diffs);
        })
    });

    // Unicode 内容
    group.bench_function("unicode_content", |b| {
        let original = "fn 你好() {\n    let 世界 = 42;\n}\n";
        let fixed = "fn 你好() {\n    let 世界 = 84;\n    println!(\"{}\", 世界);\n}\n";
        b.iter(|| {
            let diffs = compute_line_diff_myers(original, fixed);
            black_box(diffs);
        })
    });

    group.finish();
}

criterion_group!(
    myers_bench,
    myers_basic,
    myers_scaling,
    myers_vs_lcs,
    myers_diff_summary,
    myers_edge_cases,
);
criterion_main!(myers_bench);
