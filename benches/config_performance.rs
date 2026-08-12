#![allow(clippy::useless_vec)]

//! Config 模块性能基准测试
//!
//! 测试目标:
//! 1. parse_bool - 布尔值解析性能
//! 2. expand_tilde - 路径展开性能
//! 3. default_config_path - 默认配置路径性能
//! 4. load_from_file - 配置文件加载性能 (不存在/有效/部分)
//! 5. edge_cases - 边界条件性能 (空字符串/Unicode/无效TOML)

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use forge::config::{default_config_path, expand_tilde, load_from_file, parse_bool};
use std::path::PathBuf;

/// 基准测试: parse_bool
fn bench_parse_bool(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse_bool");

    let inputs: Vec<(&str, &str)> = vec![
        ("true", "true"),
        ("false", "false"),
        ("1", "1"),
        ("0", "0"),
        ("yes", "yes"),
        ("no", "no"),
        ("on", "on"),
        ("off", "off"),
        ("TRUE", "TRUE"),
        ("Yes", "Yes"),
        ("invalid", "invalid"),
        ("empty", ""),
    ];

    let count = inputs.len() as u64;
    group.throughput(Throughput::Elements(count));

    for (name, input) in &inputs {
        group.bench_function(*name, |b| {
            b.iter(|| black_box(parse_bool(black_box(input))))
        });
    }

    // 批量解析
    group.bench_function("batch_all", |b| {
        b.iter(|| {
            for (_, input) in &inputs {
                black_box(parse_bool(black_box(input)));
            }
        })
    });

    group.finish();
}

/// 基准测试: expand_tilde
fn bench_expand_tilde(c: &mut Criterion) {
    let mut group = c.benchmark_group("expand_tilde");

    let inputs: Vec<&str> = vec![
        "~/test/path",
        "~/documents/forge/config.toml",
        "/absolute/path/to/file",
        "relative/path",
        "~/.forge/config.toml",
        "/Users/john/freesoft/forge",
        "no_tilde.rs",
        "~/a/b/c/d/e/f/g/h/i/j",
    ];

    let count = inputs.len() as u64;
    group.throughput(Throughput::Elements(count));

    for input in &inputs {
        group.bench_function(*input, |b| {
            b.iter(|| black_box(expand_tilde(black_box(input))))
        });
    }

    // 批量展开
    group.bench_function("batch_all", |b| {
        b.iter(|| {
            for input in &inputs {
                black_box(expand_tilde(black_box(input)));
            }
        })
    });

    group.finish();
}

/// 基准测试: default_config_path
fn bench_default_config_path(c: &mut Criterion) {
    let mut group = c.benchmark_group("default_config_path");

    group.throughput(Throughput::Elements(1));
    group.bench_function("single_call", |b| {
        b.iter(|| black_box(default_config_path()))
    });

    // 重复调用 (模拟多次配置加载)
    group.throughput(Throughput::Elements(10));
    group.bench_function("x10_calls", |b| {
        b.iter(|| {
            for _ in 0..10 {
                black_box(default_config_path());
            }
        })
    });

    group.finish();
}

/// 基准测试: load_from_file
fn bench_load_from_file(c: &mut Criterion) {
    let mut group = c.benchmark_group("load_from_file");

    // 不存在的文件 (返回默认配置)
    let nonexistent = PathBuf::from("/nonexistent/path/config.toml");
    group.bench_function("nonexistent_file", |b| {
        b.iter(|| black_box(load_from_file(black_box(&nonexistent)).unwrap()))
    });

    // 有效 TOML 配置
    let temp_dir = tempfile::tempdir().unwrap();
    let valid_path = temp_dir.path().join("valid_config.toml");
    std::fs::write(
        &valid_path,
        r#"
[browser]
port = 9223
auto_launch = true

[chat]
phase1_timeout = 60
default_site = "zai"

[storage]
trace_backend = "sqlite"

[recovery]
auto_recovery = true
max_retries = 5
auto_failover = true
"#,
    )
    .unwrap();

    group.bench_function("valid_toml", |b| {
        b.iter(|| black_box(load_from_file(black_box(&valid_path)).unwrap()))
    });

    // 部分 TOML (只设置部分字段)
    let partial_path = temp_dir.path().join("partial.toml");
    std::fs::write(&partial_path, "[browser]\nport = 8080\n").unwrap();

    group.bench_function("partial_toml", |b| {
        b.iter(|| black_box(load_from_file(black_box(&partial_path)).unwrap()))
    });

    group.finish();
}

/// 边界条件基准测试
fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_cases");

    // parse_bool 边界
    group.bench_function("parse_bool_empty", |b| {
        b.iter(|| black_box(parse_bool(black_box(""))))
    });

    group.bench_function("parse_bool_unicode", |b| {
        b.iter(|| black_box(parse_bool(black_box("是的"))))
    });

    group.bench_function("parse_bool_mixed_case", |b| {
        b.iter(|| black_box(parse_bool(black_box("TrUe"))))
    });

    group.bench_function("parse_bool_whitespace", |b| {
        b.iter(|| black_box(parse_bool(black_box(" true "))))
    });

    // expand_tilde 边界
    group.bench_function("expand_tilde_root", |b| {
        b.iter(|| black_box(expand_tilde(black_box("~"))))
    });

    group.bench_function("expand_tilde_only", |b| {
        b.iter(|| black_box(expand_tilde(black_box("~/"))))
    });

    group.bench_function("expand_tilde_deep", |b| {
        b.iter(|| black_box(expand_tilde(black_box("~/a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p"))))
    });

    group.bench_function("expand_tilde_no_home_prefix", |b| {
        b.iter(|| black_box(expand_tilde(black_box("/simple/path"))))
    });

    // default_config_path 结果验证
    group.bench_function("default_config_path_valid", |b| {
        b.iter(|| {
            let path = default_config_path();
            black_box(
                path.to_string_lossy().contains("forge")
                    && path.to_string_lossy().contains("config.toml"),
            )
        })
    });

    // load_from_file 不存在文件返回默认配置
    let nonexistent = PathBuf::from("/nonexistent/benchmark/config.toml");
    group.bench_function("load_nonexistent_returns_default", |b| {
        b.iter(|| {
            let config = load_from_file(black_box(&nonexistent)).unwrap();
            black_box(config.browser.port == 9222)
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
        .output_directory(std::path::Path::new("target/criterion/config"))
}

criterion_group! {
    name = config_benches;
    config = configure_criterion();
    targets =
        bench_parse_bool,
        bench_expand_tilde,
        bench_default_config_path,
        bench_load_from_file,
        bench_edge_cases,
}

criterion_main!(config_benches);
