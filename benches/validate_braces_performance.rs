#![allow(clippy::useless_vec)]

//! validate_rust_braces 大规模性能基准测试 (Session 135)
//!
//! 测试目标:
//! 1. basic_validation — 基本括号配对检测 (10/100/500/1000 函数 × 平衡/不平衡)
//! 2. string_handling — 字符串中括号跳过 (普通/raw/字节/转义字符串 × 100-1000行)
//! 3. comment_handling — 注释中括号跳过 (行注释/块注释/嵌套块注释 × 100-1000行)
//! 4. macro_rules_handling — macro_rules! 宏定义 (重复/TT muncher/属性/嵌套 × 100-500行)
//! 5. edge_cases — 边界情况 (空/单行/超大/Unicode/混合)

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use forge::extract::validate_rust_braces;

/// 构建平衡的 Rust 代码 (n 个函数)
fn build_balanced_code(n: usize) -> String {
    let mut code = String::new();
    for i in 0..n {
        code.push_str(&format!(
            "fn func_{}(x: i32) -> i32 {{\n    let y = (x + 1) * 2;\n    let z = [y, y + 1, y + 2];\n    z[0]\n}}\n\n",
            i
        ));
    }
    code
}

/// 构建不平衡的 Rust 代码 (n 个函数, 最后一个缺少闭合大括号)
fn build_unbalanced_code(n: usize) -> String {
    let mut code = String::new();
    for i in 0..n - 1 {
        code.push_str(&format!(
            "fn func_{}(x: i32) -> i32 {{\n    x + 1\n}}\n\n",
            i
        ));
    }
    // 最后一个函数缺少闭合大括号
    code.push_str(&format!("fn func_{}(x: i32) -> i32 {{\n    x + 1\n", n - 1));
    code
}

/// 构建包含普通字符串的代码 (字符串中有括号)
fn build_string_code(n: usize) -> String {
    let mut code = String::from("fn main() {\n");
    for i in 0..n {
        code.push_str(&format!("    let s{} = \"{{{}}} () [] {{}}\";\n", i, i));
    }
    code.push_str("}\n");
    code
}

/// 构建包含 raw 字符串的代码
fn build_raw_string_code(n: usize) -> String {
    let mut code = String::from("fn main() {\n");
    for i in 0..n {
        code.push_str(&format!("    let s{} = r#\"{{ }} ( ) [ ]\"#;\n", i));
    }
    code.push_str("}\n");
    code
}

/// 构建包含字节字符串的代码
fn build_byte_string_code(n: usize) -> String {
    let mut code = String::from("fn main() {\n");
    for i in 0..n {
        code.push_str(&format!("    let s{} = b\"{{ }} ( ) [ ]\";\n", i));
    }
    code.push_str("}\n");
    code
}

/// 构建包含转义字符串的代码
fn build_escaped_string_code(n: usize) -> String {
    let mut code = String::from("fn main() {\n");
    for i in 0..n {
        code.push_str(&format!("    let s{} = \"{{ \\\"}}\\\" () [] {{}}\";\n", i));
    }
    code.push_str("}\n");
    code
}

/// 构建包含行注释的代码 (注释中有括号)
fn build_line_comment_code(n: usize) -> String {
    let mut code = String::from("fn main() {\n");
    for i in 0..n {
        code.push_str(&format!(
            "    // comment {{ }} ( ) [ ] {}\n    let x{} = {};\n",
            i, i, i
        ));
    }
    code.push_str("}\n");
    code
}

/// 构建包含块注释的代码
fn build_block_comment_code(n: usize) -> String {
    let mut code = String::from("fn main() {\n");
    for i in 0..n {
        code.push_str(&format!(
            "    /* block {{ }} ( ) [ ] {} */ let x{} = {};\n",
            i, i, i
        ));
    }
    code.push_str("}\n");
    code
}

/// 构建包含嵌套块注释的代码
fn build_nested_block_comment_code(n: usize) -> String {
    let mut code = String::from("fn main() {\n");
    for i in 0..n {
        code.push_str(&format!(
            "    /* outer {{ /* inner {{ }} */ }} */ let x{} = {};\n",
            i, i
        ));
    }
    code.push_str("}\n");
    code
}

/// 构建包含 macro_rules! 重复语法的代码
fn build_macro_repetition_code(n: usize) -> String {
    let mut code = String::new();
    for i in 0..n {
        code.push_str(&format!(
            r#"macro_rules! macro_{} {{
    ( $( $x:expr ),* ) => {{
        {{
            let mut v = Vec::new();
            $( v.push($x); )*
            v
        }}
    }};
}}
"#,
            i
        ));
    }
    code.push_str("fn main() {}\n");
    code
}

/// 构建包含 TT muncher 的代码
fn build_tt_muncher_code(n: usize) -> String {
    let mut code = String::new();
    for i in 0..n {
        code.push_str(&format!(
            r#"macro_rules! parse_{} {{
    (@step $head:tt) => {{ $head }};
    (@step $head:tt $($tail:tt)*) => {{
        parse_{}!(@step $($tail)*)
    }};
    ($($tokens:tt)*) => {{
        parse_{}!(@step $($tokens)*)
    }};
}}
"#,
            i, i, i
        ));
    }
    code.push_str("fn main() {}\n");
    code
}

/// 构建包含属性和嵌套的 macro_rules! 代码
fn build_macro_attrs_code(n: usize) -> String {
    let mut code = String::new();
    for i in 0..n {
        code.push_str(&format!(
            r#"macro_rules! make_struct_{} {{
    ($name:ident) => {{
        #[derive(Debug, Clone)]
        struct $name {{
            value: i32,
            data: Vec<String>,
        }}
    }};
}}
"#,
            i
        ));
    }
    code.push_str("fn main() {}\n");
    code
}

/// 构建 Unicode 代码 (含中文/日文/emoji)
fn build_unicode_code(n: usize) -> String {
    let mut code = String::from("fn main() {\n");
    for i in 0..n {
        code.push_str(&format!(
            "    let 中文变量{} = \"你好世界 🌍\";\n    let 日文{} = \"こんにちは\";\n",
            i, i
        ));
    }
    code.push_str("}\n");
    code
}

/// 构建混合内容代码 (字符串+注释+宏+普通代码)
fn build_mixed_code(n: usize) -> String {
    let mut code = String::new();
    for i in 0..n {
        code.push_str(&format!(
            r##"fn func_{}(x: i32) -> i32 {{
    // comment {{ }} ( )
    let s = "{{ }} ( ) [ ]";
    let r = r#"{{ }}"#;
    /* block {{ }} */
    let v = vec![1, 2, 3];
    v.iter().map(|&x| x * 2).collect::<Vec<_>>()
}}
"##,
            i
        ));
    }
    code
}

/// 基本括号配对检测
fn bench_basic_validation(c: &mut Criterion) {
    let mut group = c.benchmark_group("basic_validation");

    for n in [10, 100, 500, 1000] {
        let balanced = build_balanced_code(n);
        group.bench_with_input(BenchmarkId::new("balanced", n), &n, |b, _| {
            b.iter(|| {
                let result = validate_rust_braces(black_box(&balanced));
                black_box(result);
            });
        });

        let unbalanced = build_unbalanced_code(n);
        group.bench_with_input(BenchmarkId::new("unbalanced", n), &n, |b, _| {
            b.iter(|| {
                let result = validate_rust_braces(black_box(&unbalanced));
                black_box(result);
            });
        });
    }

    group.finish();
}

/// 字符串中括号跳过检测
fn bench_string_handling(c: &mut Criterion) {
    let mut group = c.benchmark_group("string_handling");

    for n in [100, 500, 1000] {
        let code = build_string_code(n);
        group.bench_with_input(BenchmarkId::new("normal_string", n), &n, |b, _| {
            b.iter(|| {
                let result = validate_rust_braces(black_box(&code));
                black_box(result);
            });
        });

        let raw_code = build_raw_string_code(n);
        group.bench_with_input(BenchmarkId::new("raw_string", n), &n, |b, _| {
            b.iter(|| {
                let result = validate_rust_braces(black_box(&raw_code));
                black_box(result);
            });
        });

        let byte_code = build_byte_string_code(n);
        group.bench_with_input(BenchmarkId::new("byte_string", n), &n, |b, _| {
            b.iter(|| {
                let result = validate_rust_braces(black_box(&byte_code));
                black_box(result);
            });
        });

        let escaped_code = build_escaped_string_code(n);
        group.bench_with_input(BenchmarkId::new("escaped_string", n), &n, |b, _| {
            b.iter(|| {
                let result = validate_rust_braces(black_box(&escaped_code));
                black_box(result);
            });
        });
    }

    group.finish();
}

/// 注释中括号跳过检测
fn bench_comment_handling(c: &mut Criterion) {
    let mut group = c.benchmark_group("comment_handling");

    for n in [100, 500, 1000] {
        let line_code = build_line_comment_code(n);
        group.bench_with_input(BenchmarkId::new("line_comment", n), &n, |b, _| {
            b.iter(|| {
                let result = validate_rust_braces(black_box(&line_code));
                black_box(result);
            });
        });

        let block_code = build_block_comment_code(n);
        group.bench_with_input(BenchmarkId::new("block_comment", n), &n, |b, _| {
            b.iter(|| {
                let result = validate_rust_braces(black_box(&block_code));
                black_box(result);
            });
        });

        let nested_code = build_nested_block_comment_code(n);
        group.bench_with_input(BenchmarkId::new("nested_block_comment", n), &n, |b, _| {
            b.iter(|| {
                let result = validate_rust_braces(black_box(&nested_code));
                black_box(result);
            });
        });
    }

    group.finish();
}

/// macro_rules! 宏定义检测
fn bench_macro_rules_handling(c: &mut Criterion) {
    let mut group = c.benchmark_group("macro_rules_handling");

    for n in [10, 50, 100, 500] {
        let rep_code = build_macro_repetition_code(n);
        group.bench_with_input(BenchmarkId::new("repetition", n), &n, |b, _| {
            b.iter(|| {
                let result = validate_rust_braces(black_box(&rep_code));
                black_box(result);
            });
        });

        let tt_code = build_tt_muncher_code(n);
        group.bench_with_input(BenchmarkId::new("tt_muncher", n), &n, |b, _| {
            b.iter(|| {
                let result = validate_rust_braces(black_box(&tt_code));
                black_box(result);
            });
        });

        let attrs_code = build_macro_attrs_code(n);
        group.bench_with_input(BenchmarkId::new("with_attrs", n), &n, |b, _| {
            b.iter(|| {
                let result = validate_rust_braces(black_box(&attrs_code));
                black_box(result);
            });
        });
    }

    group.finish();
}

/// 边界情况
fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_cases");

    // 空内容
    group.bench_function("empty", |b| {
        b.iter(|| {
            let result = validate_rust_braces(black_box(""));
            black_box(result);
        });
    });

    // 单行代码
    group.bench_function("single_line", |b| {
        b.iter(|| {
            let result = validate_rust_braces(black_box("fn main() { let x = 42; }"));
            black_box(result);
        });
    });

    // 超大文件 (5000行)
    let large_code = build_balanced_code(5000);
    group.bench_function("large_5000", |b| {
        b.iter(|| {
            let result = validate_rust_braces(black_box(&large_code));
            black_box(result);
        });
    });

    // Unicode
    let unicode_code = build_unicode_code(100);
    group.bench_function("unicode_100", |b| {
        b.iter(|| {
            let result = validate_rust_braces(black_box(&unicode_code));
            black_box(result);
        });
    });

    // 混合内容 (字符串+注释+宏+普通代码)
    let mixed_code = build_mixed_code(100);
    group.bench_function("mixed_100", |b| {
        b.iter(|| {
            let result = validate_rust_braces(black_box(&mixed_code));
            black_box(result);
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_basic_validation,
    bench_string_handling,
    bench_comment_handling,
    bench_macro_rules_handling,
    bench_edge_cases,
);
criterion_main!(benches);
