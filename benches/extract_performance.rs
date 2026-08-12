#![allow(clippy::useless_vec)]

//! Extract 模块性能基准测试
//!
//! 测试目标:
//! 1. extract_files_tagged - ```file:path``` 格式提取性能
//! 2. extract_files_plain - ```lang``` 格式提取性能
//! 3. extract_files_ui_text - DeepSeek UI 污染文本提取性能
//! 4. extract_files_large - 大量文件提取性能
//! 5. edge_cases - 边界条件性能 (空文本/无文件/混合格式)

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use forge::extract::extract_files;

/// 构建 ```file:path``` 格式的文本
fn build_tagged_text(count: usize) -> String {
    let mut text = String::from("这是 AI 回复:\n\n");
    for i in 0..count {
        text.push_str(&format!(
            "```file:src/file_{i}.rs\nfn function_{i}() -> i32 {{\n    {i}\n}}\n```\n\n"
        ));
    }
    text
}

/// 构建 ```lang``` 格式的文本 (无 file: 前缀)
fn build_plain_text(count: usize) -> String {
    let mut text = String::from("这是 AI 回复:\n\n");
    for i in 0..count {
        let content = format!(
            "pub fn function_{i}() -> i32 {{\n    let x = {i};\n    let y = x * 2;\n    y + {i}\n}}\n"
        );
        text.push_str(&format!("```rust\n{content}```\n\n"));
    }
    text
}

/// 构建 DeepSeek UI 污染文本
fn build_ui_polluted_text(count: usize) -> String {
    let mut text = String::from("这是 AI 回复:\n\n");
    for i in 0..count {
        text.push_str(&format!(
            "file:src/file_{i}.rs复制下载fn function_{i}() -> i32 {{\n    {i}\n}}\n\n"
        ));
    }
    text
}

/// 构建混合格式文本 (tagged + plain + file_marker)
fn build_mixed_text(count: usize) -> String {
    let mut text = String::from("这是 AI 回复:\n\n");
    for i in 0..count {
        match i % 3 {
            0 => {
                text.push_str(&format!(
                    "```file:src/file_{i}.rs\nfn function_{i}() {{}}\n```\n\n"
                ));
            }
            1 => {
                text.push_str(&format!(
                    "```rust:src/file_{i}.rs\npub fn function_{i}() {{}}\n```\n\n"
                ));
            }
            _ => {
                text.push_str(&format!(
                    "```toml:Cargo.toml\n[package]\nname = \"test\"\nversion = \"0.{i}\"\n```\n\n"
                ));
            }
        }
    }
    text
}

/// 基准测试: extract_files_tagged (```file:path``` 格式)
fn bench_extract_files_tagged(c: &mut Criterion) {
    let mut group = c.benchmark_group("extract_files_tagged");

    let sizes: Vec<usize> = vec![1, 10, 50, 100];

    for &size in &sizes {
        group.throughput(Throughput::Elements(size as u64));
        let text = build_tagged_text(size);

        group.bench_with_input(BenchmarkId::new("tagged", size), &text, |b, text| {
            b.iter(|| black_box(extract_files(black_box(text))))
        });
    }
    group.finish();
}

/// 基准测试: extract_files_plain (```lang``` 格式)
fn bench_extract_files_plain(c: &mut Criterion) {
    let mut group = c.benchmark_group("extract_files_plain");

    let sizes: Vec<usize> = vec![1, 10, 50, 100];

    for &size in &sizes {
        group.throughput(Throughput::Elements(size as u64));
        let text = build_plain_text(size);

        group.bench_with_input(BenchmarkId::new("plain", size), &text, |b, text| {
            b.iter(|| black_box(extract_files(black_box(text))))
        });
    }
    group.finish();
}

/// 基准测试: extract_files_ui_text (DeepSeek UI 污染)
fn bench_extract_files_ui_text(c: &mut Criterion) {
    let mut group = c.benchmark_group("extract_files_ui_text");

    let sizes: Vec<usize> = vec![1, 10, 50, 100];

    for &size in &sizes {
        group.throughput(Throughput::Elements(size as u64));
        let text = build_ui_polluted_text(size);

        group.bench_with_input(BenchmarkId::new("ui_polluted", size), &text, |b, text| {
            b.iter(|| black_box(extract_files(black_box(text))))
        });
    }
    group.finish();
}

/// 基准测试: extract_files_large (大量文件)
fn bench_extract_files_large(c: &mut Criterion) {
    let mut group = c.benchmark_group("extract_files_large");

    let sizes: Vec<usize> = vec![50, 100, 200, 500];

    for &size in &sizes {
        group.throughput(Throughput::Elements(size as u64));
        let mixed = build_mixed_text(size);

        group.bench_with_input(BenchmarkId::new("mixed", size), &mixed, |b, text| {
            b.iter(|| black_box(extract_files(black_box(text))))
        });
    }
    group.finish();
}

/// 边界条件基准测试
fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_cases");

    // 空文本
    group.bench_function("empty_text", |b| {
        b.iter(|| black_box(extract_files(black_box(""))))
    });

    // 纯文本无代码块
    group.bench_function("no_code_blocks", |b| {
        b.iter(|| black_box(extract_files(black_box("这是纯文本回复，没有任何代码。"))))
    });

    // 单个 tagged 文件
    let single = build_tagged_text(1);
    group.bench_function("single_file", |b| {
        b.iter(|| black_box(extract_files(black_box(&single))))
    });

    // 短代码块 (低于 50 字符阈值, 不被提取)
    let short_code = "```rust\nfn main() {}\n```";
    group.bench_function("short_code_block", |b| {
        b.iter(|| black_box(extract_files(black_box(short_code))))
    });

    // 大内容单文件
    let large_file = format!("```file:src/main.rs\n{}\n```", "x".repeat(10000));
    group.bench_function("large_single_file", |b| {
        b.iter(|| black_box(extract_files(black_box(&large_file))))
    });

    // 多语言混合
    let multi_lang = "\
```file:src/main.rs\nfn main() {}\n```\n\
```file:src/lib.rs\npub fn lib() {}\n```\n\
```file:Cargo.toml\n[package]\nname = \"test\"\n```\n\
```file:README.md\n# Test\n```\n\
```file:build.sh\necho hello\n```";
    group.bench_function("multi_language", |b| {
        b.iter(|| black_box(extract_files(black_box(multi_lang))))
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
        .output_directory(std::path::Path::new("target/criterion/extract"))
}

criterion_group! {
    name = extract_benches;
    config = configure_criterion();
    targets =
        bench_extract_files_tagged,
        bench_extract_files_plain,
        bench_extract_files_ui_text,
        bench_extract_files_large,
        bench_edge_cases,
}

criterion_main!(extract_benches);
