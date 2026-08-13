#![allow(clippy::useless_vec)]

//! 导入检测与修复模块性能基准测试 (Session 128)
//!
//! 测试目标:
//! 1. ensure_std_imports — std 类型导入检测与添加性能
//! 2. ensure_external_imports — 外部 crate 导入检测与添加性能
//! 3. verify_imports — 导入完整性检查性能
//! 4. verify_imports_to_json — JSON 格式报告生成性能
//! 5. verify_imports_report — 结构化报告生成性能

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use forge::extract::{
    ensure_external_imports, ensure_std_imports, verify_imports, verify_imports_report,
    verify_imports_to_json,
};

/// 构建需要 std 导入的代码片段
fn build_std_import_code(n: usize) -> String {
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

/// 构建需要外部 crate 导入的代码片段
fn build_external_import_code(n: usize) -> String {
    let mut code = String::from("fn foo() {\n");
    for i in 0..n {
        code.push_str(&format!("    let s{}: Serialize = serialize_{}();\n", i, i));
    }
    code.push_str("}\n");
    code
}

/// 构建混合类型代码 (std + external + tracing)
fn build_mixed_code(n: usize) -> String {
    let mut code = String::from("#[derive(Serialize, Deserialize)]\nstruct Foo {\n");
    for i in 0..n {
        code.push_str(&format!("    field_{}: HashMap<String, Value>,\n", i));
    }
    code.push_str("}\n\nfn bar() {\n");
    for i in 0..n {
        code.push_str(&format!("    info!(\"msg_{}\");\n", i));
    }
    code.push_str("}\n");
    code
}

/// 构建已有导入的代码 (无需修复)
fn build_clean_code(n: usize) -> String {
    let mut code = String::from("use std::collections::HashMap;\nuse std::sync::Arc;\n");
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

/// 基准测试: ensure_std_imports
fn bench_ensure_std_imports(c: &mut Criterion) {
    let mut group = c.benchmark_group("ensure_std_imports");

    let sizes: &[usize] = &[1, 10, 50, 100];

    for &size in sizes {
        let code = build_std_import_code(size);

        group.bench_with_input(BenchmarkId::new("detect", size), &code, |b, code| {
            b.iter(|| black_box(ensure_std_imports(black_box(code))))
        });
    }

    // 已有导入 (幂等检查)
    for &size in sizes {
        let code = build_clean_code(size);

        group.bench_with_input(BenchmarkId::new("idempotent", size), &code, |b, code| {
            b.iter(|| black_box(ensure_std_imports(black_box(code))))
        });
    }

    group.finish();
}

/// 基准测试: ensure_external_imports
fn bench_ensure_external_imports(c: &mut Criterion) {
    let mut group = c.benchmark_group("ensure_external_imports");

    let sizes: &[usize] = &[1, 10, 50, 100];

    for &size in sizes {
        let code = build_external_import_code(size);

        group.bench_with_input(BenchmarkId::new("detect", size), &code, |b, code| {
            b.iter(|| black_box(ensure_external_imports(black_box(code))))
        });
    }

    // 混合类型 (std + serde + tracing)
    for &size in sizes {
        let code = build_mixed_code(size);

        group.bench_with_input(BenchmarkId::new("mixed", size), &code, |b, code| {
            b.iter(|| black_box(ensure_external_imports(black_box(code))))
        });
    }

    // 幂等检查
    let code = build_external_import_code(50);
    let fixed = ensure_external_imports(&code);
    group.bench_function("idempotent_50", |b| {
        b.iter(|| black_box(ensure_external_imports(black_box(&fixed))))
    });

    group.finish();
}

/// 基准测试: verify_imports
fn bench_verify_imports(c: &mut Criterion) {
    let mut group = c.benchmark_group("verify_imports");

    let sizes: &[usize] = &[1, 10, 50, 100];

    // 有问题的代码
    for &size in sizes {
        let code = build_std_import_code(size);

        group.bench_with_input(BenchmarkId::new("with_issues", size), &code, |b, code| {
            b.iter(|| black_box(verify_imports(black_box(code))))
        });
    }

    // 无问题的代码
    for &size in sizes {
        let code = build_clean_code(size);

        group.bench_with_input(BenchmarkId::new("no_issues", size), &code, |b, code| {
            b.iter(|| black_box(verify_imports(black_box(code))))
        });
    }

    // 外部 crate 问题
    for &size in sizes {
        let code = build_external_import_code(size);

        group.bench_with_input(
            BenchmarkId::new("external_issues", size),
            &code,
            |b, code| b.iter(|| black_box(verify_imports(black_box(code)))),
        );
    }

    group.finish();
}

/// 基准测试: verify_imports_to_json
fn bench_verify_imports_to_json(c: &mut Criterion) {
    let mut group = c.benchmark_group("verify_imports_to_json");

    let sizes: &[usize] = &[1, 10, 50, 100];

    for &size in sizes {
        let code = build_std_import_code(size);

        group.bench_with_input(BenchmarkId::new("with_issues", size), &code, |b, code| {
            b.iter(|| black_box(verify_imports_to_json(black_box(code))))
        });
    }

    // 无问题
    let clean = build_clean_code(100);
    group.bench_function("no_issues_100", |b| {
        b.iter(|| black_box(verify_imports_to_json(black_box(&clean))))
    });

    group.finish();
}

/// 基准测试: verify_imports_report
fn bench_verify_imports_report(c: &mut Criterion) {
    let mut group = c.benchmark_group("verify_imports_report");

    let sizes: &[usize] = &[1, 10, 50, 100];

    for &size in sizes {
        let code = build_std_import_code(size);

        group.bench_with_input(BenchmarkId::new("report", size), &code, |b, code| {
            b.iter(|| black_box(verify_imports_report(black_box(code))))
        });
    }

    // 混合问题
    for &size in sizes {
        let code = build_mixed_code(size);

        group.bench_with_input(BenchmarkId::new("mixed", size), &code, |b, code| {
            b.iter(|| black_box(verify_imports_report(black_box(code))))
        });
    }

    group.finish();
}

/// 边界情况基准测试
fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("import_edge_cases");

    // 空代码
    group.bench_function("empty", |b| {
        b.iter(|| black_box(ensure_std_imports(black_box(""))))
    });

    // 单行代码
    group.bench_function("single_line", |b| {
        b.iter(|| black_box(ensure_std_imports(black_box("fn foo() -> i32 { 42 }"))))
    });

    // 只有注释
    group.bench_function("comments_only", |b| {
        b.iter(|| {
            black_box(ensure_std_imports(black_box(
                "// This is a comment\n// Another comment",
            )))
        })
    });

    // 全限定路径 (无需添加导入)
    group.bench_function("full_path", |b| {
        b.iter(|| {
            black_box(ensure_std_imports(black_box(
                "fn foo() -> std::collections::HashMap<i32, i32> { std::collections::HashMap::new() }",
            )))
        })
    });

    // 超长代码 (1000 行)
    let large_code = build_std_import_code(1000);
    group.bench_function("large_1000", |b| {
        b.iter(|| black_box(ensure_std_imports(black_box(&large_code))))
    });

    // verify_imports 空代码
    group.bench_function("verify_empty", |b| {
        b.iter(|| black_box(verify_imports(black_box(""))))
    });

    // verify_imports_to_json 空代码
    group.bench_function("json_empty", |b| {
        b.iter(|| black_box(verify_imports_to_json(black_box(""))))
    });

    // verify_imports_report 空代码
    group.bench_function("report_empty", |b| {
        b.iter(|| black_box(verify_imports_report(black_box(""))))
    });

    // Unicode 代码
    group.bench_function("unicode", |b| {
        b.iter(|| {
            black_box(ensure_std_imports(black_box(
                "fn foo() -> HashMap<String, i32> { /* 中文注释 */ HashMap::new() }",
            )))
        })
    });

    group.finish();
}

/// 配置 Criterion
fn configure_criterion() -> Criterion {
    Criterion::default().sample_size(100)
}

criterion_group! {
    name = ensure_imports_benches;
    config = configure_criterion();
    targets =
        bench_ensure_std_imports,
        bench_ensure_external_imports,
        bench_verify_imports,
        bench_verify_imports_to_json,
        bench_verify_imports_report,
        bench_edge_cases,
}

criterion_main!(ensure_imports_benches);
