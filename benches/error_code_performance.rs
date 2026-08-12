#![allow(clippy::useless_vec)]

//! Error Code 模块性能基准测试
//!
//! 测试目标:
//! 1. error_code - 错误代码查询 (29 种变体)
//! 2. is_recoverable - 可恢复性判断
//! 3. severity - 严重级别查询
//! 4. category - 错误类别查询
//! 5. edge_cases - 边界条件 (classify_anyhow/全组合)

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use forge::error_code::{classify_anyhow, ErrorCategory, ForgeError};

/// 所有 ForgeError 变体 (29 种)
fn all_errors() -> Vec<ForgeError> {
    vec![
        ForgeError::CdpTimeout("Runtime.evaluate".to_string(), 30000),
        ForgeError::CdpCommandFailed("Page.navigate".to_string(), "error".to_string()),
        ForgeError::CdpConnectionFailed("ws://localhost:9222".to_string()),
        ForgeError::CdpWebSocketClosed,
        ForgeError::CdpChannelClosed,
        ForgeError::BrowserUnreachable("port 9222".to_string()),
        ForgeError::BrowserProcessExited("exit code 1".to_string()),
        ForgeError::TabClosed("tab-1".to_string()),
        ForgeError::NoChatTab,
        ForgeError::ChatTimeout(120),
        ForgeError::ChatEmptyResponse,
        ForgeError::ChatSiteUnavailable("chat.z.ai".to_string()),
        ForgeError::SendMessageFailed("network error".to_string()),
        ForgeError::CompileFailed("syntax error".to_string()),
        ForgeError::TestFailed("assertion failed".to_string()),
        ForgeError::RuntimeError("panic".to_string()),
        ForgeError::FileNotFound("/tmp/test.rs".to_string()),
        ForgeError::FileWriteFailed("/tmp/out.rs".to_string()),
        ForgeError::ExtractFailed("no code blocks".to_string()),
        ForgeError::InvalidProxyUrl("ftp://bad".to_string()),
        ForgeError::ProxyConnectionFailed("timeout".to_string()),
        ForgeError::ConfigError("invalid toml".to_string()),
        ForgeError::InvalidEnvVar("FORGE_PORT".to_string(), "abc".to_string()),
        ForgeError::HttpError("500".to_string()),
        ForgeError::UrlParseError("invalid url".to_string()),
        ForgeError::RecoveryFailed("retry exhausted".to_string()),
        ForgeError::RecoveryExhausted(5),
        ForgeError::Internal("unexpected".to_string()),
        ForgeError::Unknown("mystery".to_string()),
    ]
}

/// 基准测试: error_code
fn bench_error_code(c: &mut Criterion) {
    let mut group = c.benchmark_group("error_code");

    let errors = all_errors();
    let count = errors.len() as u64;

    // 单个变体
    group.bench_function("cdp_timeout", |b| {
        b.iter(|| black_box(ForgeError::CdpTimeout("test".to_string(), 0).error_code()))
    });
    group.bench_function("web_socket_closed", |b| {
        b.iter(|| black_box(ForgeError::CdpWebSocketClosed.error_code()))
    });
    group.bench_function("unknown", |b| {
        b.iter(|| black_box(ForgeError::Unknown("test".to_string()).error_code()))
    });

    // 全部变体遍历
    group.throughput(Throughput::Elements(count));
    group.bench_function("all_variants", |b| {
        b.iter(|| {
            for err in &errors {
                black_box(err.error_code());
            }
        })
    });

    group.finish();
}

/// 基准测试: is_recoverable
fn bench_is_recoverable(c: &mut Criterion) {
    let mut group = c.benchmark_group("is_recoverable");

    let errors = all_errors();
    let count = errors.len() as u64;

    // 可恢复 vs 不可恢复
    group.bench_function("recoverable", |b| {
        b.iter(|| black_box(ForgeError::CdpTimeout("test".to_string(), 0).is_recoverable()))
    });
    group.bench_function("not_recoverable", |b| {
        b.iter(|| black_box(ForgeError::ConfigError("test".to_string()).is_recoverable()))
    });

    // 全部变体
    group.throughput(Throughput::Elements(count));
    group.bench_function("all_variants", |b| {
        b.iter(|| {
            for err in &errors {
                black_box(err.is_recoverable());
            }
        })
    });

    group.finish();
}

/// 基准测试: severity
fn bench_severity(c: &mut Criterion) {
    let mut group = c.benchmark_group("severity");

    let errors = all_errors();
    let count = errors.len() as u64;

    // 各级别
    group.bench_function("critical", |b| {
        b.iter(|| black_box(ForgeError::CdpWebSocketClosed.severity()))
    });
    group.bench_function("warning", |b| {
        b.iter(|| black_box(ForgeError::CdpTimeout("test".to_string(), 0).severity()))
    });
    group.bench_function("info", |b| {
        b.iter(|| black_box(ForgeError::ChatEmptyResponse.severity()))
    });

    // 全部变体
    group.throughput(Throughput::Elements(count));
    group.bench_function("all_variants", |b| {
        b.iter(|| {
            for err in &errors {
                black_box(err.severity());
            }
        })
    });

    group.finish();
}

/// 基准测试: category
fn bench_category(c: &mut Criterion) {
    let mut group = c.benchmark_group("category");

    let errors = all_errors();
    let count = errors.len() as u64;

    // 各类别
    group.bench_function("cdp", |b| {
        b.iter(|| black_box(ForgeError::CdpTimeout("test".to_string(), 0).category()))
    });
    group.bench_function("browser", |b| {
        b.iter(|| black_box(ForgeError::BrowserUnreachable("test".to_string()).category()))
    });
    group.bench_function("chat", |b| {
        b.iter(|| black_box(ForgeError::ChatTimeout(0).category()))
    });
    group.bench_function("build", |b| {
        b.iter(|| black_box(ForgeError::CompileFailed("test".to_string()).category()))
    });

    // category.name()
    group.bench_function("category_name", |b| {
        b.iter(|| black_box(ErrorCategory::Cdp.name()))
    });

    // 全部变体
    group.throughput(Throughput::Elements(count));
    group.bench_function("all_variants", |b| {
        b.iter(|| {
            for err in &errors {
                black_box(err.category());
            }
        })
    });

    group.finish();
}

/// 边界条件基准测试
fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_cases");

    let errors = all_errors();
    let count = errors.len() as u64;

    // 全组合: error_code + is_recoverable + severity + category
    group.throughput(Throughput::Elements(count));
    group.bench_function("all_methods_all_variants", |b| {
        b.iter(|| {
            for err in &errors {
                let code = err.error_code();
                let recoverable = err.is_recoverable();
                let sev = err.severity();
                let cat = err.category();
                black_box((code, recoverable, sev, cat));
            }
        })
    });

    // classify_anyhow: 各种错误消息
    let anyhow_errors: Vec<anyhow::Error> = vec![
        anyhow::anyhow!("CDP 命令超时 (30s): Runtime.evaluate"),
        anyhow::anyhow!("CDP WebSocket 已关闭"),
        anyhow::anyhow!("浏览器不可达: port 9222"),
        anyhow::anyhow!("编译失败: syntax error"),
        anyhow::anyhow!("代理 URL 无效: ftp://bad"),
        anyhow::anyhow!("文件不存在: /tmp/test.rs"),
        anyhow::anyhow!("some weird error"),
    ];
    group.throughput(Throughput::Elements(anyhow_errors.len() as u64));
    group.bench_function("classify_anyhow_various", |b| {
        b.iter(|| {
            for err in &anyhow_errors {
                black_box(classify_anyhow(black_box(err)));
            }
        })
    });

    // classify_anyhow: 长错误消息
    let long_msg = "x".repeat(1000);
    let long_err = anyhow::anyhow!("编译失败: {}", long_msg);
    group.bench_function("classify_long_message", |b| {
        b.iter(|| black_box(classify_anyhow(black_box(&long_err))))
    });

    // Display 格式化
    group.bench_function("display_format", |b| {
        b.iter(|| {
            black_box(format!(
                "{}",
                ForgeError::CdpTimeout("test".to_string(), 30000)
            ))
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
        .output_directory(std::path::Path::new("target/criterion/error_code"))
}

criterion_group! {
    name = error_code_benches;
    config = configure_criterion();
    targets =
        bench_error_code,
        bench_is_recoverable,
        bench_severity,
        bench_category,
        bench_edge_cases,
}

criterion_main!(error_code_benches);
