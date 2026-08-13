#![allow(clippy::useless_vec)]

//! strip_use_line_comments / strip_use_block_comments 大规模性能基准测试 (Session 136)
//!
//! 测试目标:
//! 1. line_comments_stripping — 行注释移除 (无注释/单行注释/多行注释/URL × 10-1000行)
//! 2. block_comments_stripping — 块注释移除 (无注释/单行/嵌套/多块 × 10-1000行)
//! 3. mixed_comments_stripping — 混合注释移除 (行+块/嵌套/URL混合 × 10-1000行)
//! 4. glob_import_with_comments — 带注释的 glob 导入提取 (单行/多行/嵌套 × 10-500)
//! 5. edge_cases — 边界情况 (空/单行/超大/Unicode/纯注释)

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use forge::extract::{extract_glob_imports, strip_use_block_comments, strip_use_line_comments};

// ── 1. line_comments_stripping ──────────────────────────────

/// 构建无注释的 use 语句 (n 行)
fn build_no_comment_uses(n: usize) -> String {
    let mut code = String::new();
    for i in 0..n {
        code.push_str(&format!("use std::module_{}::*;\n", i));
    }
    code
}

/// 构建带单行注释的 use 语句 (n 行)
fn build_single_line_comment_uses(n: usize) -> String {
    let mut code = String::new();
    for i in 0..n {
        code.push_str(&format!("use std::module_{}::*; // comment {}\n", i, i));
    }
    code
}

/// 构建带多行注释的 use 语句 (n 行)
fn build_multi_line_comment_uses(n: usize) -> String {
    let mut code = String::new();
    for i in 0..n {
        code.push_str(&format!(
            "use std::{{\n    // comment a {}\n    io::*,\n    // comment b {}\n    sync::*\n}};\n",
            i, i
        ));
    }
    code
}

/// 构建带 URL 的 use 语句 (n 行)
fn build_url_uses(n: usize) -> String {
    let mut code = String::new();
    for i in 0..n {
        code.push_str(&format!(
            "use example::http://example.com/module_{}::*; // real comment\n",
            i
        ));
    }
    code
}

fn bench_line_comments_stripping(c: &mut Criterion) {
    let mut group = c.benchmark_group("line_comments_stripping");
    let sizes = [10, 100, 500, 1000];

    for &size in &sizes {
        let no_comment = build_no_comment_uses(size);
        group.bench_with_input(
            BenchmarkId::new("no_comment", size),
            &no_comment,
            |b, code| {
                b.iter(|| {
                    for line in code.lines() {
                        black_box(strip_use_line_comments(line));
                    }
                });
            },
        );

        let single_comment = build_single_line_comment_uses(size);
        group.bench_with_input(
            BenchmarkId::new("single_comment", size),
            &single_comment,
            |b, code| {
                b.iter(|| {
                    for line in code.lines() {
                        black_box(strip_use_line_comments(line));
                    }
                });
            },
        );

        let multi_comment = build_multi_line_comment_uses(size);
        group.bench_with_input(
            BenchmarkId::new("multi_comment", size),
            &multi_comment,
            |b, code| {
                b.iter(|| {
                    for line in code.lines() {
                        black_box(strip_use_line_comments(line));
                    }
                });
            },
        );

        let url_uses = build_url_uses(size);
        group.bench_with_input(BenchmarkId::new("url", size), &url_uses, |b, code| {
            b.iter(|| {
                for line in code.lines() {
                    black_box(strip_use_line_comments(line));
                }
            });
        });
    }

    group.finish();
}

// ── 2. block_comments_stripping ──────────────────────────────

/// 构建无块注释的 use 语句 (n 行)
fn build_no_block_comment_uses(n: usize) -> String {
    build_no_comment_uses(n)
}

/// 构建带单行块注释的 use 语句 (n 行)
fn build_single_block_comment_uses(n: usize) -> String {
    let mut code = String::new();
    for i in 0..n {
        code.push_str(&format!(
            "use std::{{/* block comment {} */ module_{}::*}};\n",
            i, i
        ));
    }
    code
}

/// 构建带嵌套块注释的 use 语句 (n 行)
fn build_nested_block_comment_uses(n: usize) -> String {
    let mut code = String::new();
    for i in 0..n {
        code.push_str(&format!(
            "use std::{{/* outer {} /* inner {} */ */ module_{}::*}};\n",
            i, i, i
        ));
    }
    code
}

/// 构建带多个块注释的 use 语句 (n 行)
fn build_multi_block_comment_uses(n: usize) -> String {
    let mut code = String::new();
    for i in 0..n {
        code.push_str(&format!(
            "use std::{{/* a {} */ io::*, /* b {} */ sync::*}};\n",
            i, i
        ));
    }
    code
}

fn bench_block_comments_stripping(c: &mut Criterion) {
    let mut group = c.benchmark_group("block_comments_stripping");
    let sizes = [10, 100, 500, 1000];

    for &size in &sizes {
        let no_comment = build_no_block_comment_uses(size);
        group.bench_with_input(
            BenchmarkId::new("no_comment", size),
            &no_comment,
            |b, code| {
                b.iter(|| {
                    for line in code.lines() {
                        black_box(strip_use_block_comments(line));
                    }
                });
            },
        );

        let single_comment = build_single_block_comment_uses(size);
        group.bench_with_input(
            BenchmarkId::new("single_comment", size),
            &single_comment,
            |b, code| {
                b.iter(|| {
                    for line in code.lines() {
                        black_box(strip_use_block_comments(line));
                    }
                });
            },
        );

        let nested_comment = build_nested_block_comment_uses(size);
        group.bench_with_input(
            BenchmarkId::new("nested_comment", size),
            &nested_comment,
            |b, code| {
                b.iter(|| {
                    for line in code.lines() {
                        black_box(strip_use_block_comments(line));
                    }
                });
            },
        );

        let multi_comment = build_multi_block_comment_uses(size);
        group.bench_with_input(
            BenchmarkId::new("multi_comment", size),
            &multi_comment,
            |b, code| {
                b.iter(|| {
                    for line in code.lines() {
                        black_box(strip_use_block_comments(line));
                    }
                });
            },
        );
    }

    group.finish();
}

// ── 3. mixed_comments_stripping ──────────────────────────────

/// 构建带混合注释 (行+块) 的 use 语句 (n 行)
fn build_mixed_comment_uses(n: usize) -> String {
    let mut code = String::new();
    for i in 0..n {
        code.push_str(&format!(
            "use std::{{\n    // line comment {}\n    /* block comment {} */ io::*,\n    sync::* // trailing {}\n}};\n",
            i, i, i
        ));
    }
    code
}

/// 构建带嵌套混合注释的 use 语句 (n 行)
fn build_nested_mixed_comment_uses(n: usize) -> String {
    let mut code = String::new();
    for i in 0..n {
        code.push_str(&format!(
            "use std::{{\n    // line {}\n    /* outer {} /* inner {} */ */ io::*,\n    /* block {} */ sync::*\n}};\n",
            i, i, i, i
        ));
    }
    code
}

/// 构建带 URL 和混合注释的 use 语句 (n 行)
fn build_url_mixed_comment_uses(n: usize) -> String {
    let mut code = String::new();
    for i in 0..n {
        code.push_str(&format!(
            "use example::http://example.com/{{\n    // comment {}\n    /* block {} */ module_{}::*\n}};\n",
            i, i, i
        ));
    }
    code
}

fn bench_mixed_comments_stripping(c: &mut Criterion) {
    let mut group = c.benchmark_group("mixed_comments_stripping");
    let sizes = [10, 100, 500, 1000];

    for &size in &sizes {
        let mixed = build_mixed_comment_uses(size);
        group.bench_with_input(
            BenchmarkId::new("line_and_block", size),
            &mixed,
            |b, code| {
                b.iter(|| {
                    for line in code.lines() {
                        let stripped = strip_use_line_comments(line);
                        black_box(strip_use_block_comments(&stripped));
                    }
                });
            },
        );

        let nested = build_nested_mixed_comment_uses(size);
        group.bench_with_input(BenchmarkId::new("nested", size), &nested, |b, code| {
            b.iter(|| {
                for line in code.lines() {
                    let stripped = strip_use_line_comments(line);
                    black_box(strip_use_block_comments(&stripped));
                }
            });
        });

        let url_mixed = build_url_mixed_comment_uses(size);
        group.bench_with_input(
            BenchmarkId::new("url_mixed", size),
            &url_mixed,
            |b, code| {
                b.iter(|| {
                    for line in code.lines() {
                        let stripped = strip_use_line_comments(line);
                        black_box(strip_use_block_comments(&stripped));
                    }
                });
            },
        );
    }

    group.finish();
}

// ── 4. glob_import_with_comments ──────────────────────────────

/// 构建带注释的单行 glob 导入 (n 个)
fn build_single_line_glob_with_comments(n: usize) -> String {
    let mut code = String::new();
    for i in 0..n {
        code.push_str(&format!("use std::module_{}::*; // comment {}\n", i, i));
    }
    code
}

/// 构建带注释的多行 glob 导入 (n 个)
fn build_multi_line_glob_with_comments(n: usize) -> String {
    let mut code = String::new();
    for i in 0..n {
        code.push_str(&format!(
            "use std::{{\n    // comment a {}\n    /* block {} */ io::*,\n    sync::*\n}};\n",
            i, i
        ));
    }
    code
}

/// 构建带嵌套块注释的多行 glob 导入 (n 个)
fn build_nested_glob_with_comments(n: usize) -> String {
    let mut code = String::new();
    for i in 0..n {
        code.push_str(&format!(
            "use std::{{\n    /* outer {} /* inner {} */ */ io::*,\n    // line {}\n    sync::{{*, atomic::*}}\n}};\n",
            i, i, i
        ));
    }
    code
}

fn bench_glob_import_with_comments(c: &mut Criterion) {
    let mut group = c.benchmark_group("glob_import_with_comments");
    let sizes = [10, 100, 500];

    for &size in &sizes {
        let single_line = build_single_line_glob_with_comments(size);
        group.bench_with_input(
            BenchmarkId::new("single_line", size),
            &single_line,
            |b, code| {
                b.iter(|| black_box(extract_glob_imports(code)));
            },
        );

        let multi_line = build_multi_line_glob_with_comments(size);
        group.bench_with_input(
            BenchmarkId::new("multi_line", size),
            &multi_line,
            |b, code| {
                b.iter(|| black_box(extract_glob_imports(code)));
            },
        );

        let nested = build_nested_glob_with_comments(size);
        group.bench_with_input(BenchmarkId::new("nested", size), &nested, |b, code| {
            b.iter(|| black_box(extract_glob_imports(code)));
        });
    }

    group.finish();
}

// ── 5. edge_cases ──────────────────────────────────────────────

fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_cases");

    // 空字符串
    group.bench_function("empty", |b| {
        b.iter(|| {
            black_box(strip_use_line_comments(""));
            black_box(strip_use_block_comments(""));
        });
    });

    // 单行无注释
    group.bench_function("single_line_no_comment", |b| {
        b.iter(|| {
            black_box(strip_use_line_comments("use std::io::*;"));
            black_box(strip_use_block_comments("use std::io::*;"));
        });
    });

    // 单行带注释
    group.bench_function("single_line_with_comment", |b| {
        b.iter(|| {
            black_box(strip_use_line_comments("use std::io::*; // comment"));
            black_box(strip_use_block_comments("use std::{/* comment */ io::*};"));
        });
    });

    // 超大输入 (5000 行带注释的 use 语句)
    let large_input = build_mixed_comment_uses(5000);
    group.bench_function("large_5000_lines", |b| {
        b.iter(|| {
            for line in large_input.lines() {
                let stripped = strip_use_line_comments(line);
                black_box(strip_use_block_comments(&stripped));
            }
        });
    });

    // Unicode 注释
    let unicode_input =
        "use std::{\n    // 中文注释\n    /* 日本語コメント */ io::*,\n    sync::*\n};\n";
    group.bench_function("unicode_comments", |b| {
        b.iter(|| {
            for line in unicode_input.lines() {
                let stripped = strip_use_line_comments(line);
                black_box(strip_use_block_comments(&stripped));
            }
        });
    });

    // 纯注释 (无 use 语句)
    let pure_comments = format!(
        "{}\n{}\n",
        "// just a line comment", "/* just a block comment */"
    );
    group.bench_function("pure_comments", |b| {
        b.iter(|| {
            for line in pure_comments.lines() {
                black_box(strip_use_line_comments(line));
                black_box(strip_use_block_comments(line));
            }
        });
    });

    // 块注释中包含大括号
    let braces_in_comment = "use std::{/* { } ( ) [ ] */ io::*};\n".repeat(100);
    group.bench_function("braces_in_block_comment", |b| {
        b.iter(|| {
            for line in braces_in_comment.lines() {
                black_box(strip_use_block_comments(line));
            }
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_line_comments_stripping,
    bench_block_comments_stripping,
    bench_mixed_comments_stripping,
    bench_glob_import_with_comments,
    bench_edge_cases,
);
criterion_main!(benches);
