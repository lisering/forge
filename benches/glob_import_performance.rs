#![allow(clippy::useless_vec)]

//! Glob 导入检测大规模性能基准测试 (Session 134)
//!
//! 测试目标:
//! 1. glob_exclusion — std glob 导入覆盖检测 (简单/混合/嵌套 × 10-1000行)
//! 2. glob_detection — 多行 use 语句 glob 检测 (单行/多行/嵌套多行 × 规模)
//! 3. nested_glob — 嵌套 glob 路径检测 (2层/3层/4层嵌套 × 规模)
//! 4. multiline_glob — 多行 use 语句合并性能 (3行/10行/50行 use 块)
//! 5. edge_cases — 边界情况 (空/单行/纯glob/混合/Unicode/超大)

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use forge::extract::{ensure_external_imports, ensure_std_imports};

/// 构建带简单 glob 导入的代码 (use module::*;)
fn build_simple_glob_code(n: usize) -> String {
    let mut code = String::from("use std::collections::*;\n");
    for i in 0..n {
        code.push_str(&format!(
            "fn func_{}() -> HashMap<String, HashSet<i32>> {{ HashMap::new() }}\n",
            i
        ));
    }
    code
}

/// 构建带混合 glob 导入的代码 (use module::{Type, sub::*};)
fn build_mixed_glob_code(n: usize) -> String {
    let mut code = String::from("use std::collections::{HashMap, hash_map::*};\n");
    for i in 0..n {
        code.push_str(&format!(
            "fn func_{}() -> (HashMap<String, i32>, Entry<i32>) {{ HashMap::new() }}\n",
            i
        ));
    }
    code
}

/// 构建带嵌套 glob 导入的代码 (use std::{sync::{*, atomic::*}, io::*};)
fn build_nested_glob_code(n: usize) -> String {
    let mut code = String::from("use std::{sync::{*, atomic::*}, io::*};\n");
    for i in 0..n {
        code.push_str(&format!(
            "fn func_{}() -> (Arc<Mutex<i32>>, AtomicBool, BufReader) {{ unimplemented!() }}\n",
            i
        ));
    }
    code
}

/// 构建多行 use 语句的代码
fn build_multiline_glob_code(lines: usize) -> String {
    let mut code = String::from("use std::{\n");
    for i in 0..lines {
        code.push_str(&format!("    module_{}::*,\n", i));
    }
    code.push_str("};\nfn foo() {}\n");
    code
}

/// 构建多行嵌套 use 语句的代码
fn build_multiline_nested_glob_code(depth: usize) -> String {
    let mut code = String::from("use std::{\n");
    for d in 0..depth {
        code.push_str(&format!("    level_{}::{{\n", d));
        code.push_str("        *,\n");
        code.push_str("        sub::*\n");
        code.push_str("    },\n");
    }
    code.push_str("};\nfn foo() {}\n");
    code
}

/// 构建带外部 crate glob 导入的代码
fn build_external_glob_code(n: usize) -> String {
    let mut code = String::from("use serde::*;\n");
    for i in 0..n {
        code.push_str(&format!(
            "#[derive(Serialize, Deserialize)]\nstruct S{} {{ x: i32 }}\n",
            i
        ));
    }
    code
}

/// 构建超大 Unicode 代码
fn build_unicode_code(n: usize) -> String {
    let mut code = String::from("use std::collections::*;\n");
    for i in 0..n {
        code.push_str(&format!(
            "// 函数_{}: 处理中文数据 🔥\nfn func_{}() -> HashMap<String, Vec<中文类型>> {{ HashMap::new() }}\n",
            i, i
        ));
    }
    code
}

/// 1. glob_exclusion — std glob 导入覆盖检测性能
fn glob_exclusion(c: &mut Criterion) {
    let mut group = c.benchmark_group("glob_exclusion");

    for (name, code) in [
        ("simple", build_simple_glob_code(100)),
        ("mixed", build_mixed_glob_code(100)),
        ("nested", build_nested_glob_code(100)),
    ] {
        group.bench_with_input(BenchmarkId::new("ensure_std", name), &code, |b, c| {
            b.iter(|| black_box(ensure_std_imports(c)));
        });
    }

    let sizes: Vec<usize> = vec![10, 100, 1000];
    for &size in &sizes {
        let code = build_simple_glob_code(size);
        group.bench_with_input(BenchmarkId::new("simple_size", size), &code, |b, c| {
            b.iter(|| black_box(ensure_std_imports(c)));
        });

        let code = build_nested_glob_code(size);
        group.bench_with_input(BenchmarkId::new("nested_size", size), &code, |b, c| {
            b.iter(|| black_box(ensure_std_imports(c)));
        });
    }

    group.finish();
}

/// 2. glob_detection — 多行 use 语句 glob 检测性能
fn glob_detection(c: &mut Criterion) {
    let mut group = c.benchmark_group("glob_detection");

    // 单行 vs 多行
    let single_line = "use std::{io::*, sync::*};\nfn foo() {}\n";
    let multi_line = "use std::{\n    io::*,\n    sync::*\n};\nfn foo() {}\n";
    let nested_multi =
        "use std::{\n    sync::{\n        *,\n        atomic::*\n    },\n    io::*\n};\nfn foo() {}\n";

    for (name, code) in [
        ("single_line", single_line.to_string()),
        ("multi_line", multi_line.to_string()),
        ("nested_multi", nested_multi.to_string()),
    ] {
        group.bench_with_input(BenchmarkId::new("format", name), &code, |b, c| {
            b.iter(|| black_box(ensure_std_imports(c)));
        });
    }

    // 规模测试
    for &n in &[3, 10, 50] {
        let code = build_multiline_glob_code(n);
        group.bench_with_input(BenchmarkId::new("multiline", n), &code, |b, c| {
            b.iter(|| black_box(ensure_std_imports(c)));
        });
    }

    group.finish();
}

/// 3. nested_glob — 嵌套 glob 路径检测性能
fn nested_glob(c: &mut Criterion) {
    let mut group = c.benchmark_group("nested_glob");

    for &depth in &[1, 2, 3, 4] {
        let code = build_multiline_nested_glob_code(depth);
        group.bench_with_input(BenchmarkId::new("depth", depth), &code, |b, c| {
            b.iter(|| black_box(ensure_std_imports(c)));
        });
    }

    // 外部 crate glob
    let sizes: Vec<usize> = vec![10, 100, 500];
    for &size in &sizes {
        let code = build_external_glob_code(size);
        group.bench_with_input(BenchmarkId::new("external_size", size), &code, |b, c| {
            b.iter(|| black_box(ensure_external_imports(c)));
        });
    }

    group.finish();
}

/// 4. multiline_glob — 多行 use 语句合并性能
fn multiline_glob(c: &mut Criterion) {
    let mut group = c.benchmark_group("multiline_glob");

    // 多种多行 use 块大小
    for &lines in &[5, 10, 20, 50] {
        let code = build_multiline_glob_code(lines);
        group.bench_with_input(BenchmarkId::new("lines", lines), &code, |b, c| {
            b.iter(|| black_box(ensure_std_imports(c)));
        });
    }

    // 多行 + 类型混合
    let mut mixed_code = String::from("use std::{\n");
    mixed_code.push_str("    io::*,\n");
    mixed_code.push_str("    sync::*\n");
    mixed_code.push_str("};\n");
    for i in 0..100 {
        mixed_code.push_str(&format!(
            "fn func_{}() -> (Arc<Mutex<i32>>, BufReader) {{ unimplemented!() }}\n",
            i
        ));
    }
    group.bench_function("mixed_100_fns", |b| {
        b.iter(|| black_box(ensure_std_imports(&mixed_code)));
    });

    group.finish();
}

/// 5. edge_cases — 边界情况
fn edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_cases");

    let empty = "";
    let single_line = "use std::collections::*;\nfn foo() {}";
    let all_glob = "use std::*;\nuse serde::*;\nuse tokio::*;\nuse regex::*;\nfn foo() {}";
    let no_glob =
        "use std::collections::HashMap;\nfn foo() -> HashMap<i32, i32> { HashMap::new() }";
    let unicode_100 = build_unicode_code(100);

    for (name, code) in [
        ("empty", empty.to_string()),
        ("single_line", single_line.to_string()),
        ("all_glob", all_glob.to_string()),
        ("no_glob", no_glob.to_string()),
        ("unicode_100", unicode_100),
    ] {
        group.bench_with_input(BenchmarkId::new("ensure_std", name), &code, |b, c| {
            b.iter(|| black_box(ensure_std_imports(c)));
        });
    }

    // 超大规模
    let huge_5000 = build_simple_glob_code(5000);
    group.bench_function("ensure_std_5000_lines", |b| {
        b.iter(|| black_box(ensure_std_imports(&huge_5000)));
    });

    let huge_external_1000 = build_external_glob_code(1000);
    group.bench_function("ensure_external_1000", |b| {
        b.iter(|| black_box(ensure_external_imports(&huge_external_1000)));
    });

    group.finish();
}

criterion_group!(
    benches,
    glob_exclusion,
    glob_detection,
    nested_glob,
    multiline_glob,
    edge_cases
);
criterion_main!(benches);
