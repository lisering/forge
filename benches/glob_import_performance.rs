#![allow(clippy::useless_vec)]

//! Glob 导入排除性能基准测试 (Session 130)
//!
//! 测试目标:
//! 1. glob 导入排除 — ensure_std_imports 在有 glob 导入时的性能
//! 2. glob 导入排除 — ensure_external_imports 在有 glob 导入时的性能
//! 3. glob 导入检测 — verify_imports 在有 glob 导入时的性能
//! 4. 无 glob 导入对比 — 基线性能对比
//! 5. 边界情况 — 空代码/纯 glob/混合 glob

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use forge::extract::{ensure_external_imports, ensure_std_imports, verify_imports};

/// 构建带有 glob 导入的代码 (std 类型)
fn build_std_glob_code(n: usize) -> String {
    let mut code = String::from("use std::collections::*;\nuse std::sync::*;\n");
    code.push_str("fn foo() {\n");
    for i in 0..n {
        code.push_str(&format!(
            "    let v{}: HashMap<i32, Arc<{}>> = HashMap::new();\n",
            i, i
        ));
    }
    code.push_str("}\n");
    code
}

/// 构建不带 glob 导入的代码 (std 类型) — 基线
fn build_std_no_glob_code(n: usize) -> String {
    let mut code = String::from("fn foo() {\n");
    for i in 0..n {
        code.push_str(&format!(
            "    let v{}: HashMap<i32, Arc<{}>> = HashMap::new();\n",
            i, i
        ));
    }
    code.push_str("}\n");
    code
}

/// 构建带有 glob 导入的代码 (外部 crate)
fn build_external_glob_code(n: usize) -> String {
    let mut code = String::from("use serde::*;\nuse regex::*;\n");
    code.push_str("fn foo() {\n");
    for i in 0..n {
        code.push_str(&format!("    let s{}: Serialize = serialize_{}();\n", i, i));
    }
    code.push_str("}\n");
    code
}

/// 构建不带 glob 导入的代码 (外部 crate) — 基线
fn build_external_no_glob_code(n: usize) -> String {
    let mut code = String::from("fn foo() {\n");
    for i in 0..n {
        code.push_str(&format!("    let s{}: Serialize = serialize_{}();\n", i, i));
    }
    code.push_str("}\n");
    code
}

/// 构建多层 glob 导入代码
fn build_nested_glob_code(n: usize) -> String {
    let mut code = String::from("use std::*;\nuse std::sync::*;\nuse std::collections::*;\n");
    code.push_str("fn foo() {\n");
    for i in 0..n {
        code.push_str(&format!(
            "    let v{}: HashMap<String, Arc<Mutex<{}>>> = HashMap::new();\n",
            i, i
        ));
    }
    code.push_str("}\n");
    code
}

/// glob 导入排除基准测试
fn glob_exclusion_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("glob_exclusion");

    let sizes: Vec<usize> = vec![1, 10, 50, 100];

    // ensure_std_imports with glob
    for &size in &sizes {
        let code = build_std_glob_code(size);
        group.bench_with_input(
            BenchmarkId::new("ensure_std_imports_with_glob", size),
            &code,
            |b, code| b.iter(|| black_box(ensure_std_imports(code))),
        );
    }

    // ensure_std_imports without glob (baseline)
    for &size in &sizes {
        let code = build_std_no_glob_code(size);
        group.bench_with_input(
            BenchmarkId::new("ensure_std_imports_no_glob", size),
            &code,
            |b, code| b.iter(|| black_box(ensure_std_imports(code))),
        );
    }

    // ensure_external_imports with glob
    for &size in &sizes {
        let code = build_external_glob_code(size);
        group.bench_with_input(
            BenchmarkId::new("ensure_external_imports_with_glob", size),
            &code,
            |b, code| b.iter(|| black_box(ensure_external_imports(code))),
        );
    }

    // ensure_external_imports without glob (baseline)
    for &size in &sizes {
        let code = build_external_no_glob_code(size);
        group.bench_with_input(
            BenchmarkId::new("ensure_external_imports_no_glob", size),
            &code,
            |b, code| b.iter(|| black_box(ensure_external_imports(code))),
        );
    }

    group.finish();
}

/// glob 导入检测基准测试 (verify_imports)
fn glob_detection_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("glob_detection");

    let sizes: Vec<usize> = vec![1, 10, 50, 100];

    for &size in &sizes {
        let code = build_std_glob_code(size);
        group.bench_with_input(
            BenchmarkId::new("verify_imports_with_glob", size),
            &code,
            |b, code| b.iter(|| black_box(verify_imports(code))),
        );
    }

    for &size in &sizes {
        let code = build_std_no_glob_code(size);
        group.bench_with_input(
            BenchmarkId::new("verify_imports_no_glob", size),
            &code,
            |b, code| b.iter(|| black_box(verify_imports(code))),
        );
    }

    group.finish();
}

/// 多层 glob 导入基准测试
fn nested_glob_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("nested_glob");

    let sizes: Vec<usize> = vec![1, 10, 50, 100];

    for &size in &sizes {
        let code = build_nested_glob_code(size);
        group.bench_with_input(
            BenchmarkId::new("ensure_std_imports_nested_glob", size),
            &code,
            |b, code| b.iter(|| black_box(ensure_std_imports(code))),
        );
    }

    for &size in &sizes {
        let code = build_nested_glob_code(size);
        group.bench_with_input(
            BenchmarkId::new("verify_imports_nested_glob", size),
            &code,
            |b, code| b.iter(|| black_box(verify_imports(code))),
        );
    }

    group.finish();
}

/// 边界情况基准测试
fn edge_cases_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("glob_edge_cases");

    // 空代码
    group.bench_function("empty_code", |b| {
        b.iter(|| black_box(ensure_std_imports("")))
    });

    // 纯 glob 导入无类型使用
    group.bench_function("pure_glob_no_types", |b| {
        b.iter(|| {
            black_box(ensure_std_imports(
                "use std::collections::*;\nuse std::sync::*;\nfn foo() {}\n",
            ))
        })
    });

    // 多个 glob 导入 + 少量类型
    group.bench_function("many_globs_few_types", |b| {
        let code = "use std::collections::*;\nuse std::sync::*;\nuse std::io::*;\nuse std::path::*;\nuse std::process::*;\nfn foo() -> HashMap<i32, i32> { HashMap::new() }\n";
        b.iter(|| black_box(ensure_std_imports(code)))
    });

    // glob 导入与显式导入混合
    group.bench_function("mixed_glob_explicit", |b| {
        let code = "use std::collections::*;\nuse std::sync::Arc;\nfn foo() -> (HashMap<i32, i32>, Arc<u8>) { (HashMap::new(), Arc::new(0)) }\n";
        b.iter(|| black_box(ensure_std_imports(code)))
    });

    // 大量 glob 导入
    group.bench_function("many_glob_imports", |b| {
        let mut code = String::new();
        for _ in 0..20 {
            code.push_str("use std::collections::*;\n");
        }
        code.push_str("fn foo() -> HashMap<i32, i32> { HashMap::new() }\n");
        b.iter(|| black_box(ensure_std_imports(&code)))
    });

    group.finish();
}

criterion_group!(
    glob_import_benchmarks,
    glob_exclusion_benchmark,
    glob_detection_benchmark,
    nested_glob_benchmark,
    edge_cases_benchmark,
);
criterion_main!(glob_import_benchmarks);
