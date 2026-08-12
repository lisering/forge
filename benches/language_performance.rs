#![allow(clippy::useless_vec)]

//! Language 模块性能基准测试
//!
//! 测试目标:
//! 1. detect_language - 语言检测 (Rust/Python/Go/Node/Unknown)
//! 2. get_adapter - 获取语言适配器
//! 3. find_python_files - Python 文件查找
//! 4. find_entry_point - 入口文件查找 (Python/Node)
//! 5. edge_cases - 边界条件 (is_typescript/优先级/空目录)

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use forge::language::{
    detect_language, get_adapter, MultiLanguageTestRunner, NodeAdapter, PythonAdapter,
};
use forge::traits::Language;
use std::path::Path;

/// 基准测试: detect_language
fn bench_detect_language(c: &mut Criterion) {
    let mut group = c.benchmark_group("detect_language");

    // 创建各语言项目目录
    let rust_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        rust_dir.path().join("Cargo.toml"),
        "[package]\nname = \"test\"",
    )
    .unwrap();

    let python_dir = tempfile::tempdir().unwrap();
    std::fs::write(python_dir.path().join("pyproject.toml"), "[project]").unwrap();

    let go_dir = tempfile::tempdir().unwrap();
    std::fs::write(go_dir.path().join("go.mod"), "module test").unwrap();

    let node_dir = tempfile::tempdir().unwrap();
    std::fs::write(node_dir.path().join("package.json"), r#"{"name":"test"}"#).unwrap();

    let unknown_dir = tempfile::tempdir().unwrap();

    // 各语言检测
    group.bench_function("rust", |b| {
        b.iter(|| black_box(detect_language(black_box(rust_dir.path()))))
    });
    group.bench_function("python", |b| {
        b.iter(|| black_box(detect_language(black_box(python_dir.path()))))
    });
    group.bench_function("go", |b| {
        b.iter(|| black_box(detect_language(black_box(go_dir.path()))))
    });
    group.bench_function("node", |b| {
        b.iter(|| black_box(detect_language(black_box(node_dir.path()))))
    });
    group.bench_function("unknown", |b| {
        b.iter(|| black_box(detect_language(black_box(unknown_dir.path()))))
    });

    // Rust 优先级 (同时有 Cargo.toml 和 package.json)
    let mixed_dir = tempfile::tempdir().unwrap();
    std::fs::write(mixed_dir.path().join("Cargo.toml"), "[package]").unwrap();
    std::fs::write(mixed_dir.path().join("package.json"), "{}").unwrap();
    group.bench_function("rust_priority", |b| {
        b.iter(|| black_box(detect_language(black_box(mixed_dir.path()))))
    });

    group.finish();
}

/// 基准测试: get_adapter
fn bench_get_adapter(c: &mut Criterion) {
    let mut group = c.benchmark_group("get_adapter");

    let rust_dir = tempfile::tempdir().unwrap();
    std::fs::write(rust_dir.path().join("Cargo.toml"), "[package]").unwrap();

    let python_dir = tempfile::tempdir().unwrap();
    std::fs::write(python_dir.path().join("pyproject.toml"), "[project]").unwrap();

    let go_dir = tempfile::tempdir().unwrap();
    std::fs::write(go_dir.path().join("go.mod"), "module test").unwrap();

    let node_dir = tempfile::tempdir().unwrap();
    std::fs::write(node_dir.path().join("package.json"), "{}").unwrap();

    let unknown_dir = tempfile::tempdir().unwrap();

    group.bench_function("rust", |b| {
        b.iter(|| black_box(get_adapter(black_box(rust_dir.path())).language()))
    });
    group.bench_function("python", |b| {
        b.iter(|| black_box(get_adapter(black_box(python_dir.path())).language()))
    });
    group.bench_function("go", |b| {
        b.iter(|| black_box(get_adapter(black_box(go_dir.path())).language()))
    });
    group.bench_function("node", |b| {
        b.iter(|| black_box(get_adapter(black_box(node_dir.path())).language()))
    });
    group.bench_function("unknown_defaults_rust", |b| {
        b.iter(|| black_box(get_adapter(black_box(unknown_dir.path())).language()))
    });

    group.finish();
}

/// 基准测试: find_python_files
fn bench_find_python_files(c: &mut Criterion) {
    let mut group = c.benchmark_group("find_python_files");

    // 小项目 (3 个文件)
    let small_dir = tempfile::tempdir().unwrap();
    std::fs::write(small_dir.path().join("main.py"), "print('hello')").unwrap();
    std::fs::write(small_dir.path().join("utils.py"), "def foo(): pass").unwrap();
    std::fs::write(small_dir.path().join("config.py"), "X = 1").unwrap();
    group.throughput(Throughput::Elements(3));
    group.bench_function("small_3_files", |b| {
        b.iter(|| {
            black_box(PythonAdapter::find_python_files(black_box(
                small_dir.path(),
            )))
        })
    });

    // 中等项目 (20 个文件)
    let medium_dir = tempfile::tempdir().unwrap();
    for i in 0..20 {
        std::fs::write(
            medium_dir.path().join(format!("mod_{i}.py")),
            format!("x_{i} = {i}"),
        )
        .unwrap();
    }
    group.throughput(Throughput::Elements(20));
    group.bench_function("medium_20_files", |b| {
        b.iter(|| {
            black_box(PythonAdapter::find_python_files(black_box(
                medium_dir.path(),
            )))
        })
    });

    // 带子目录的项目 (跳过 __pycache__)
    let nested_dir = tempfile::tempdir().unwrap();
    std::fs::write(nested_dir.path().join("main.py"), "print('hello')").unwrap();
    std::fs::create_dir(nested_dir.path().join("sub")).unwrap();
    std::fs::write(nested_dir.path().join("sub").join("mod.py"), "y = 2").unwrap();
    std::fs::create_dir(nested_dir.path().join("__pycache__")).unwrap();
    std::fs::write(
        nested_dir.path().join("__pycache__").join("cached.py"),
        "z = 3",
    )
    .unwrap();
    group.bench_function("nested_with_pycache", |b| {
        b.iter(|| {
            black_box(PythonAdapter::find_python_files(black_box(
                nested_dir.path(),
            )))
        })
    });

    // 空目录
    let empty_dir = tempfile::tempdir().unwrap();
    group.bench_function("empty_dir", |b| {
        b.iter(|| {
            black_box(PythonAdapter::find_python_files(black_box(
                empty_dir.path(),
            )))
        })
    });

    group.finish();
}

/// 基准测试: find_entry_point
fn bench_find_entry_point(c: &mut Criterion) {
    let mut group = c.benchmark_group("find_entry_point");

    // Python: main.py
    let py_main_dir = tempfile::tempdir().unwrap();
    std::fs::write(py_main_dir.path().join("main.py"), "print('hello')").unwrap();
    group.bench_function("python_main_py", |b| {
        b.iter(|| {
            black_box(PythonAdapter::find_entry_point(black_box(
                py_main_dir.path(),
            )))
        })
    });

    // Python: app.py
    let py_app_dir = tempfile::tempdir().unwrap();
    std::fs::write(py_app_dir.path().join("app.py"), "print('hello')").unwrap();
    group.bench_function("python_app_py", |b| {
        b.iter(|| {
            black_box(PythonAdapter::find_entry_point(black_box(
                py_app_dir.path(),
            )))
        })
    });

    // Python: fallback (任意 .py)
    let py_fallback_dir = tempfile::tempdir().unwrap();
    std::fs::write(py_fallback_dir.path().join("script.py"), "x = 1").unwrap();
    group.bench_function("python_fallback", |b| {
        b.iter(|| {
            black_box(PythonAdapter::find_entry_point(black_box(
                py_fallback_dir.path(),
            )))
        })
    });

    // Python: 无文件
    let py_none_dir = tempfile::tempdir().unwrap();
    group.bench_function("python_none", |b| {
        b.iter(|| {
            black_box(PythonAdapter::find_entry_point(black_box(
                py_none_dir.path(),
            )))
        })
    });

    // Node: index.js
    let node_index_dir = tempfile::tempdir().unwrap();
    std::fs::write(node_index_dir.path().join("index.js"), "console.log('hi')").unwrap();
    group.bench_function("node_index_js", |b| {
        b.iter(|| {
            black_box(NodeAdapter::find_entry_point(black_box(
                node_index_dir.path(),
            )))
        })
    });

    // Node: package.json main
    let node_pkg_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(node_pkg_dir.path().join("src")).unwrap();
    std::fs::write(
        node_pkg_dir.path().join("package.json"),
        r#"{"main":"src/app.js"}"#,
    )
    .unwrap();
    std::fs::write(
        node_pkg_dir.path().join("src").join("app.js"),
        "console.log('hi')",
    )
    .unwrap();
    group.bench_function("node_package_json_main", |b| {
        b.iter(|| {
            black_box(NodeAdapter::find_entry_point(black_box(
                node_pkg_dir.path(),
            )))
        })
    });

    group.finish();
}

/// 边界条件基准测试
fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_cases");

    // is_typescript
    let ts_dir = tempfile::tempdir().unwrap();
    std::fs::write(ts_dir.path().join("tsconfig.json"), "{}").unwrap();
    let no_ts_dir = tempfile::tempdir().unwrap();
    group.bench_function("is_typescript_true", |b| {
        b.iter(|| black_box(NodeAdapter::is_typescript(black_box(ts_dir.path()))))
    });
    group.bench_function("is_typescript_false", |b| {
        b.iter(|| black_box(NodeAdapter::is_typescript(black_box(no_ts_dir.path()))))
    });

    // Language Display
    group.bench_function("language_display_all", |b| {
        b.iter(|| {
            black_box(Language::Rust.to_string());
            black_box(Language::Python.to_string());
            black_box(Language::Go.to_string());
            black_box(Language::Node.to_string());
            black_box(Language::Unknown.to_string());
        })
    });

    // MultiLanguageTestRunner 创建
    group.bench_function("runner_new", |b| {
        b.iter(|| black_box(MultiLanguageTestRunner::new()))
    });

    // detect_language: 全语言遍历
    let rust_dir = tempfile::tempdir().unwrap();
    std::fs::write(rust_dir.path().join("Cargo.toml"), "[package]").unwrap();
    let py_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        py_dir.path().join("setup.py"),
        "from setuptools import setup",
    )
    .unwrap();
    let go_dir = tempfile::tempdir().unwrap();
    std::fs::write(go_dir.path().join("go.mod"), "module test").unwrap();
    let node_dir = tempfile::tempdir().unwrap();
    std::fs::write(node_dir.path().join("package.json"), "{}").unwrap();
    let req_dir = tempfile::tempdir().unwrap();
    std::fs::write(req_dir.path().join("requirements.txt"), "flask").unwrap();
    let dirs: Vec<&Path> = vec![
        rust_dir.path(),
        py_dir.path(),
        go_dir.path(),
        node_dir.path(),
        req_dir.path(),
    ];
    group.throughput(Throughput::Elements(dirs.len() as u64));
    group.bench_function("detect_all_languages", |b| {
        b.iter(|| {
            for dir in &dirs {
                black_box(detect_language(dir));
            }
        })
    });

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
        .output_directory(std::path::Path::new("target/criterion/language"))
}

criterion_group! {
    name = language_benches;
    config = configure_criterion();
    targets =
        bench_detect_language,
        bench_get_adapter,
        bench_find_python_files,
        bench_find_entry_point,
        bench_edge_cases,
}

criterion_main!(language_benches);
