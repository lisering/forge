#![allow(clippy::useless_vec)]

//! Package 模块性能基准测试
//!
//! 测试目标:
//! 1. package_single - 单文件打包
//! 2. package_multiple - 多文件打包
//! 3. package_large - 大规模文件打包
//! 4. package_unicode - Unicode 内容打包
//! 5. edge_cases - 边界条件 (空列表/前导斜杠/metadata)

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use forge::extract::ExtractedFile;
use forge::package::package;

/// 创建测试文件
fn make_file(path: &str, content: &str, language: &str) -> ExtractedFile {
    ExtractedFile {
        path: path.to_string(),
        content: content.to_string(),
        language: language.to_string(),
    }
}

/// 生成指定大小的代码内容
fn generate_code(size: usize) -> String {
    let line = "    let x = 42;\n";
    let count = size / line.len();
    let mut result = String::from("fn main() {\n");
    for _ in 0..count {
        result.push_str(line);
    }
    result.push_str("}\n");
    result
}

/// 基准测试: 单文件打包
fn bench_package_single(c: &mut Criterion) {
    let mut group = c.benchmark_group("package_single");

    // 小文件
    let small = vec![make_file("src/main.rs", "fn main() {}", "rust")];
    group.throughput(Throughput::Bytes(small[0].content.len() as u64));
    group.bench_function("small", |b| {
        b.iter(|| {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("output.zip");
            black_box(package(black_box(&small), black_box(&path))).unwrap();
            black_box(dir)
        })
    });

    // 中等文件
    let medium_content = generate_code(1000);
    let medium = vec![make_file("src/main.rs", &medium_content, "rust")];
    group.throughput(Throughput::Bytes(medium_content.len() as u64));
    group.bench_function("medium_1kb", |b| {
        b.iter(|| {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("output.zip");
            black_box(package(black_box(&medium), black_box(&path))).unwrap();
            black_box(dir)
        })
    });

    // 大文件
    let large_content = generate_code(10000);
    let large = vec![make_file("src/main.rs", &large_content, "rust")];
    group.throughput(Throughput::Bytes(large_content.len() as u64));
    group.bench_function("large_10kb", |b| {
        b.iter(|| {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("output.zip");
            black_box(package(black_box(&large), black_box(&path))).unwrap();
            black_box(dir)
        })
    });

    group.finish();
}

/// 基准测试: 多文件打包
fn bench_package_multiple(c: &mut Criterion) {
    let mut group = c.benchmark_group("package_multiple");

    // 3 个文件
    let files_3 = vec![
        make_file("src/main.rs", "fn main() {}", "rust"),
        make_file("src/lib.rs", "pub fn hello() {}", "rust"),
        make_file("Cargo.toml", "[package]\nname = \"test\"", "toml"),
    ];
    group.throughput(Throughput::Elements(3));
    group.bench_function("three_files", |b| {
        b.iter(|| {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("output.zip");
            black_box(package(black_box(&files_3), black_box(&path))).unwrap();
            black_box(dir)
        })
    });

    // 10 个文件
    let files_10: Vec<ExtractedFile> = (0..10)
        .map(|i| {
            make_file(
                &format!("src/mod_{i}.rs"),
                &format!("pub fn func_{i}() {{}}"),
                "rust",
            )
        })
        .collect();
    group.throughput(Throughput::Elements(10));
    group.bench_function("ten_files", |b| {
        b.iter(|| {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("output.zip");
            black_box(package(black_box(&files_10), black_box(&path))).unwrap();
            black_box(dir)
        })
    });

    // 50 个文件
    let files_50: Vec<ExtractedFile> = (0..50)
        .map(|i| {
            make_file(
                &format!("src/file_{i}.rs"),
                &format!("// file {}\nfn f_{i}() {{}}\n", i),
                "rust",
            )
        })
        .collect();
    group.throughput(Throughput::Elements(50));
    group.bench_function("fifty_files", |b| {
        b.iter(|| {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("output.zip");
            black_box(package(black_box(&files_50), black_box(&path))).unwrap();
            black_box(dir)
        })
    });

    group.finish();
}

/// 基准测试: 大规模文件打包
fn bench_package_large(c: &mut Criterion) {
    let mut group = c.benchmark_group("package_large");

    // 100 个文件
    let files_100: Vec<ExtractedFile> = (0..100)
        .map(|i| {
            let content = generate_code(500);
            make_file(&format!("src/module_{i}.rs"), &content, "rust")
        })
        .collect();
    group.throughput(Throughput::Elements(100));
    group.bench_function("hundred_files_500b_each", |b| {
        b.iter(|| {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("output.zip");
            black_box(package(black_box(&files_100), black_box(&path))).unwrap();
            black_box(dir)
        })
    });

    // 混合语言 (Rust + Python + TOML)
    let mixed_files: Vec<ExtractedFile> = vec![
        make_file("src/main.rs", "fn main() { println!(\"hello\"); }", "rust"),
        make_file(
            "src/lib.rs",
            "pub fn add(a: i32, b: i32) -> i32 { a + b }",
            "rust",
        ),
        make_file(
            "Cargo.toml",
            "[package]\nname = \"test\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            "toml",
        ),
        make_file("scripts/run.py", "print('running')", "python"),
        make_file("scripts/test.py", "import unittest\n", "python"),
        make_file(
            "README.md",
            "# Test Project\n\nA test project.\n",
            "markdown",
        ),
    ];
    group.throughput(Throughput::Elements(mixed_files.len() as u64));
    group.bench_function("mixed_languages_6_files", |b| {
        b.iter(|| {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("output.zip");
            black_box(package(black_box(&mixed_files), black_box(&path))).unwrap();
            black_box(dir)
        })
    });

    group.finish();
}

/// 基准测试: Unicode 内容打包
fn bench_package_unicode(c: &mut Criterion) {
    let mut group = c.benchmark_group("package_unicode");

    // 中文注释
    let chinese_content = "// 这是中文注释\nfn main() {\n    println!(\"你好世界\");\n}\n";
    let chinese = vec![make_file("src/main.rs", chinese_content, "rust")];
    group.throughput(Throughput::Bytes(chinese_content.len() as u64));
    group.bench_function("chinese_content", |b| {
        b.iter(|| {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("output.zip");
            black_box(package(black_box(&chinese), black_box(&path))).unwrap();
            black_box(dir)
        })
    });

    // 多语言 Unicode
    let unicode_files = vec![
        make_file(
            "src/jp.rs",
            "// 日本語コメント\nfn hello() { println!(\"こんにちは\"); }",
            "rust",
        ),
        make_file(
            "src/kr.rs",
            "// 한국어 주석\nfn hi() { println!(\"안녕하세요\"); }",
            "rust",
        ),
        make_file(
            "src/cn.rs",
            "// 中文注释\nfn nihao() { println!(\"你好\"); }",
            "rust",
        ),
    ];
    group.throughput(Throughput::Elements(3));
    group.bench_function("multi_unicode", |b| {
        b.iter(|| {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("output.zip");
            black_box(package(black_box(&unicode_files), black_box(&path))).unwrap();
            black_box(dir)
        })
    });

    group.finish();
}

/// 边界条件基准测试
fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_cases");

    // 空文件列表
    let empty: Vec<ExtractedFile> = vec![];
    group.bench_function("empty_file_list", |b| {
        b.iter(|| {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("output.zip");
            black_box(package(black_box(&empty), black_box(&path))).unwrap();
            black_box(dir)
        })
    });

    // 前导斜杠路径
    let slash_files = vec![
        make_file("/src/main.rs", "fn main() {}", "rust"),
        make_file("/src/lib.rs", "pub fn lib() {}", "rust"),
    ];
    group.throughput(Throughput::Elements(2));
    group.bench_function("leading_slash", |b| {
        b.iter(|| {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("output.zip");
            black_box(package(black_box(&slash_files), black_box(&path))).unwrap();
            black_box(dir)
        })
    });

    // 大文件名
    let long_name = format!("src/{}.rs", "x".repeat(100));
    let long_name_files = vec![make_file(&long_name, "fn main() {}", "rust")];
    group.bench_function("long_file_name", |b| {
        b.iter(|| {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("output.zip");
            black_box(package(black_box(&long_name_files), black_box(&path))).unwrap();
            black_box(dir)
        })
    });

    // metadata 验证 (序列化 + 写入)
    let meta_files: Vec<ExtractedFile> = (0..20)
        .map(|i| make_file(&format!("src/f{i}.rs"), &format!("fn f{i}() {{}}"), "rust"))
        .collect();
    group.throughput(Throughput::Elements(20));
    group.bench_function("metadata_20_files", |b| {
        b.iter(|| {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("output.zip");
            black_box(package(black_box(&meta_files), black_box(&path))).unwrap();
            black_box(dir)
        })
    });

    group.finish();
}

/// 配置基准测试参数
fn configure_criterion() -> Criterion {
    Criterion::default()
        .sample_size(30)
        .measurement_time(std::time::Duration::from_secs(5))
        .warm_up_time(std::time::Duration::from_secs(2))
        .nresamples(50_000)
        .noise_threshold(0.05)
        .output_directory(std::path::Path::new("target/criterion/package"))
}

criterion_group! {
    name = package_benches;
    config = configure_criterion();
    targets =
        bench_package_single,
        bench_package_multiple,
        bench_package_large,
        bench_package_unicode,
        bench_edge_cases,
}

criterion_main!(package_benches);
