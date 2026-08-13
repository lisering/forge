#![allow(clippy::useless_vec)]

//! Diff 算法对比性能基准测试 (Session 132)
//!
//! 测试目标:
//! 1. compare_diff_algorithms — 四种算法 (Basic/LCS/Myers/Hirschberg) 对比性能
//! 2. format_diff_comparison — 表格格式化输出性能
//! 3. 一致性检查 — 不同规模输入的算法一致性验证
//! 4. 算法对比 — 各算法在不同差异模式下的表现
//! 5. 边界情况 — 空输入/单行/大规模/Unicode

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use forge::extract::{compare_diff_algorithms, format_diff_comparison};

/// 构建无差异代码 (n 行)
fn build_identical_code(n: usize) -> String {
    (0..n).map(|i| format!("line {i}\n")).collect()
}

/// 构建有添加行的代码 (original n 行, fixed n+added 行)
fn build_added_lines(n: usize, added: usize) -> (String, String) {
    let original: String = (0..n).map(|i| format!("line {i}\n")).collect();
    let mut fixed = original.clone();
    for i in 0..added {
        fixed.push_str(&format!("added line {i}\n"));
    }
    (original, fixed)
}

/// 构建有删除行的代码 (original n+removed 行, fixed n 行)
fn build_removed_lines(n: usize, removed: usize) -> (String, String) {
    let mut original: String = (0..n).map(|i| format!("line {i}\n")).collect();
    for i in 0..removed {
        original.push_str(&format!("removed line {i}\n"));
    }
    let fixed: String = (0..n).map(|i| format!("line {i}\n")).collect();
    (original, fixed)
}

/// 构建有修改行的代码 (每隔 step 行修改一行)
fn build_modified_lines(n: usize, step: usize) -> (String, String) {
    let original: String = (0..n).map(|i| format!("line {i}\n")).collect();
    let fixed: String = (0..n)
        .map(|i| {
            if i % step == 0 {
                format!("MODIFIED {i}\n")
            } else {
                format!("line {i}\n")
            }
        })
        .collect();
    (original, fixed)
}

/// 构建 Unicode 代码
fn build_unicode_code(n: usize) -> (String, String) {
    let original: String = (0..n).map(|i| format!("// 第{i}行 中文注释\n")).collect();
    let fixed: String = (0..n)
        .map(|i| {
            if i % 3 == 0 {
                format!("// 已修改 第{i}行 🚀\n")
            } else {
                format!("// 第{i}行 中文注释\n")
            }
        })
        .collect();
    (original, fixed)
}

/// 基准测试 1: compare_diff_algorithms — 不同规模
fn bench_compare_algorithms(c: &mut Criterion) {
    let mut group = c.benchmark_group("compare_diff_algorithms");

    for size in [10, 50, 100, 500, 1000] {
        let (original, fixed) = build_added_lines(size, size / 10);
        group.bench_with_input(BenchmarkId::new("added", size), &size, |b, _| {
            b.iter(|| {
                let result = compare_diff_algorithms(black_box(&original), black_box(&fixed));
                black_box(result);
            });
        });
    }

    for size in [10, 50, 100, 500, 1000] {
        let (original, fixed) = build_modified_lines(size, 5);
        group.bench_with_input(BenchmarkId::new("modified", size), &size, |b, _| {
            b.iter(|| {
                let result = compare_diff_algorithms(black_box(&original), black_box(&fixed));
                black_box(result);
            });
        });
    }

    for size in [10, 50, 100, 500, 1000] {
        let (original, fixed) = build_removed_lines(size, size / 10);
        group.bench_with_input(BenchmarkId::new("removed", size), &size, |b, _| {
            b.iter(|| {
                let result = compare_diff_algorithms(black_box(&original), black_box(&fixed));
                black_box(result);
            });
        });
    }

    group.finish();
}

/// 基准测试 2: format_diff_comparison — 表格格式化
fn bench_format_comparison(c: &mut Criterion) {
    let mut group = c.benchmark_group("format_diff_comparison");

    for size in [10, 50, 100, 500, 1000] {
        let (original, fixed) = build_added_lines(size, size / 10);
        let result = compare_diff_algorithms(&original, &fixed);
        group.bench_with_input(BenchmarkId::new("added", size), &size, |b, _| {
            b.iter(|| {
                let table = format_diff_comparison(black_box(&result));
                black_box(table);
            });
        });
    }

    for size in [10, 50, 100, 500, 1000] {
        let (original, fixed) = build_modified_lines(size, 5);
        let result = compare_diff_algorithms(&original, &fixed);
        group.bench_with_input(BenchmarkId::new("modified", size), &size, |b, _| {
            b.iter(|| {
                let table = format_diff_comparison(black_box(&result));
                black_box(table);
            });
        });
    }

    // 无差异
    for size in [10, 100, 1000] {
        let code = build_identical_code(size);
        let result = compare_diff_algorithms(&code, &code);
        group.bench_with_input(BenchmarkId::new("identical", size), &size, |b, _| {
            b.iter(|| {
                let table = format_diff_comparison(black_box(&result));
                black_box(table);
            });
        });
    }

    group.finish();
}

/// 基准测试 3: 一致性检查 — 验证各算法一致性
fn bench_consistency(c: &mut Criterion) {
    let mut group = c.benchmark_group("consistency_check");

    for size in [10, 50, 100, 500, 1000] {
        let (original, fixed) = build_added_lines(size, 1);
        group.bench_with_input(BenchmarkId::new("single_add", size), &size, |b, _| {
            b.iter(|| {
                let result = compare_diff_algorithms(black_box(&original), black_box(&fixed));
                assert!(result.all_consistent, "单行添加应一致");
                black_box(result);
            });
        });
    }

    for size in [10, 50, 100, 500, 1000] {
        let code = build_identical_code(size);
        group.bench_with_input(BenchmarkId::new("identical", size), &size, |b, _| {
            b.iter(|| {
                let result = compare_diff_algorithms(black_box(&code), black_box(&code));
                assert!(result.all_consistent, "相同代码应一致");
                assert_eq!(result.entries.len(), 4, "应有 4 种算法");
                black_box(result);
            });
        });
    }

    group.finish();
}

/// 基准测试 4: 算法对比 — 各算法在不同模式下的表现
fn bench_algorithm_comparison(c: &mut Criterion) {
    let mut group = c.benchmark_group("algorithm_comparison");

    // 大规模稀疏差异 (适合 Myers)
    for size in [100, 500, 1000, 5000] {
        let (original, fixed) = build_added_lines(size, 1);
        group.bench_with_input(BenchmarkId::new("sparse_diff", size), &size, |b, _| {
            b.iter(|| {
                let result = compare_diff_algorithms(black_box(&original), black_box(&fixed));
                // Myers 和 Hirschberg 在稀疏差异时应高效
                let myers = result
                    .entries
                    .iter()
                    .find(|e| e.algorithm == "Myers")
                    .unwrap();
                assert_eq!(myers.added_count, 1, "Myers 应检测到 1 个 Added");
                black_box(result);
            });
        });
    }

    // 密集差异 (适合 LCS)
    for size in [10, 50, 100, 500] {
        let (original, fixed) = build_modified_lines(size, 2);
        group.bench_with_input(BenchmarkId::new("dense_diff", size), &size, |b, _| {
            b.iter(|| {
                let result = compare_diff_algorithms(black_box(&original), black_box(&fixed));
                // Basic 应检测到 Modified
                let basic = result
                    .entries
                    .iter()
                    .find(|e| e.algorithm == "Basic")
                    .unwrap();
                assert!(basic.modified_count > 0, "Basic 应有 Modified");
                black_box(result);
            });
        });
    }

    group.finish();
}

/// 基准测试 5: 边界情况
fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_cases");

    // 空输入
    group.bench_function("empty", |b| {
        b.iter(|| {
            let result = compare_diff_algorithms(black_box(""), black_box(""));
            assert!(result.all_consistent, "空输入应一致");
            assert_eq!(result.original_lines, 0);
            assert_eq!(result.fixed_lines, 0);
            black_box(result);
        });
    });

    // 单行
    group.bench_function("single_line", |b| {
        b.iter(|| {
            let result = compare_diff_algorithms(black_box("a"), black_box("b"));
            assert_eq!(result.entries.len(), 4, "应有 4 种算法");
            black_box(result);
        });
    });

    // 一侧空
    group.bench_function("one_empty", |b| {
        let code: String = (0..100).map(|i| format!("line {i}\n")).collect();
        b.iter(|| {
            let result = compare_diff_algorithms(black_box(""), black_box(&code));
            assert_eq!(result.original_lines, 0);
            assert_eq!(result.fixed_lines, 100);
            black_box(result);
        });
    });

    // 超大输入
    for size in [5000, 10000] {
        let (original, fixed) = build_added_lines(size, 10);
        group.bench_with_input(BenchmarkId::new("large", size), &size, |b, _| {
            b.iter(|| {
                let result = compare_diff_algorithms(black_box(&original), black_box(&fixed));
                black_box(result);
            });
        });
    }

    // Unicode
    for size in [10, 50, 100] {
        let (original, fixed) = build_unicode_code(size);
        group.bench_with_input(BenchmarkId::new("unicode", size), &size, |b, _| {
            b.iter(|| {
                let result = compare_diff_algorithms(black_box(&original), black_box(&fixed));
                black_box(result);
            });
        });
    }

    // format_diff_comparison 空结果
    group.bench_function("format_empty", |b| {
        let result = compare_diff_algorithms("", "");
        b.iter(|| {
            let table = format_diff_comparison(black_box(&result));
            assert!(table.contains("Consistent: Yes"), "空输入应一致");
            black_box(table);
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_compare_algorithms,
    bench_format_comparison,
    bench_consistency,
    bench_algorithm_comparison,
    bench_edge_cases,
);
criterion_main!(benches);
