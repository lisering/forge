#![allow(clippy::useless_vec)]

//! proxy_pool 性能基准测试
//!
//! 测试目标:
//! 1. is_valid_proxy_url - 代理 URL 格式验证
//! 2. proxy_entry_parse - ProxyEntry 解析各种格式
//! 3. validate_proxy_list - 批量验证代理列表
//! 4. proxy_pool_operations - ProxyPool 创建/刷新/过期检查
//! 5. edge_cases - 边界场景 (空/Chrome参数/负载环境变量)

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use forge::proxy_pool::*;
use std::sync::Arc;

// ============================================================================
//  辅助数据
// ============================================================================

const PROXY_URLS: &[&str] = &[
    "http://127.0.0.1:8080",
    "https://proxy.example.com:443",
    "socks5://10.0.0.1:1080",
    "socks4://192.168.1.1:1080",
    "http://user:pass@proxy:3128",
    "https://auth:pwd@secure.proxy.io:8443",
    "http://10.0.0.1:8080/some/path",
    "socks5://[::1]:1080",
];

const INVALID_URLS: &[&str] = &[
    "",
    "invalid",
    "ftp://proxy:8080",
    "127.0.0.1:8080",
    "  ",
    "://missing",
];

// ============================================================================
//  基准测试 1: is_valid_proxy_url
// ============================================================================

fn bench_is_valid_proxy_url(c: &mut Criterion) {
    c.bench_function("is_valid_proxy_url_valid", |b| {
        b.iter(|| {
            for url in black_box(PROXY_URLS) {
                let _ = is_valid_proxy_url(url);
            }
        })
    });

    c.bench_function("is_valid_proxy_url_invalid", |b| {
        b.iter(|| {
            for url in black_box(INVALID_URLS) {
                let _ = is_valid_proxy_url(url);
            }
        })
    });

    // 大小写不敏感
    let upper_urls = &[
        "HTTP://127.0.0.1:8080",
        "HTTPS://proxy.com:443",
        "SOCKS5://10.0.0.1:1080",
    ];
    c.bench_function("is_valid_proxy_url_case_insensitive", |b| {
        b.iter(|| {
            for url in black_box(upper_urls) {
                let _ = is_valid_proxy_url(url);
            }
        })
    });
}

// ============================================================================
//  基准测试 2: proxy_entry_parse
// ============================================================================

fn bench_proxy_entry_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("proxy_entry_parse");

    // 各格式解析
    let test_cases = vec![
        ("http_simple", "http://127.0.0.1:8080"),
        ("https_domain", "https://proxy.example.com:443"),
        ("socks5_ip", "socks5://10.0.0.1:1080"),
        ("socks4_ip", "socks4://192.168.1.1:1080"),
        ("http_auth", "http://user:pass@proxy:3128"),
        ("http_path", "http://proxy:8080/some/path"),
    ];

    for (name, url) in &test_cases {
        group.bench_function(*name, |b| {
            b.iter(|| {
                let entry = ProxyEntry::parse(black_box(url)).unwrap();
                black_box(entry);
            })
        });
    }

    // 错误处理
    let invalid_entries = &["", "invalid", "ftp://proxy:8080", "http://", "   "];
    group.bench_function("invalid_urls", |b| {
        b.iter(|| {
            for url in black_box(invalid_entries) {
                let _ = ProxyEntry::parse(url);
            }
        })
    });

    // chrome_proxy_arg 生成
    let entry = ProxyEntry::parse("http://127.0.0.1:8080").unwrap();
    group.bench_function("chrome_proxy_arg", |b| {
        b.iter(|| {
            let arg = black_box(&entry).chrome_proxy_arg();
            black_box(arg);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 3: validate_proxy_list
// ============================================================================

fn bench_validate_proxy_list(c: &mut Criterion) {
    let mut group = c.benchmark_group("validate_proxy_list");

    // 混合有效/无效
    let mixed: Vec<&str> = (0..20)
        .map(|i| {
            if i % 3 == 0 {
                "invalid"
            } else if i % 3 == 1 {
                "http://valid.proxy.com:8080"
            } else {
                "socks5://10.0.0.1:1080"
            }
        })
        .collect();

    for size in [10, 50, 100] {
        let urls: Vec<&str> = mixed.iter().take(size / 2).copied().collect();
        let urls_full: Vec<&str> = std::iter::repeat_with(|| {
            if urls.len().is_multiple_of(2) {
                "http://proxy:8080"
            } else {
                "socks5://10.0.0.1:1080"
            }
        })
        .take(size)
        .collect();
        group.bench_with_input(BenchmarkId::new("mixed", size), &urls_full, |b, urls| {
            b.iter(|| {
                let (valid, invalid) = validate_proxy_list(black_box(urls));
                black_box((valid.len(), invalid.len()));
            })
        });
    }

    group.finish();
}

// ============================================================================
//  基准测试 4: proxy_pool_operations
// ============================================================================

fn bench_proxy_pool_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("proxy_pool_operations");

    // 创建空池
    group.bench_function("new_empty", |b| {
        b.iter(|| {
            let pool = ProxyPool::new(ProxyConfig::default());
            black_box(pool);
        })
    });

    // 创建带 3 代理的池
    let config = ProxyConfig {
        proxies: vec![
            "http://proxy1:8080".to_string(),
            "http://proxy2:8080".to_string(),
            "http://proxy3:8080".to_string(),
        ],
        ttl_secs: 300,
        max_retries: 3,
    };
    group.bench_function("new_with_3_proxies", |b| {
        b.iter(|| {
            let pool = ProxyPool::new(black_box(config.clone()));
            black_box(pool);
        })
    });

    // current()
    let pool = ProxyPool::new(config.clone());
    group.bench_function("current", |b| {
        b.iter(|| {
            let p = black_box(&pool).current();
            black_box(p);
        })
    });

    // is_expired()
    group.bench_function("is_expired", |b| {
        b.iter(|| {
            let expired = black_box(&pool).is_expired();
            black_box(expired);
        })
    });

    // refresh()
    group.bench_function("refresh", |b| {
        b.iter(|| {
            let p = ProxyPool::new(config.clone());
            let _ = p.refresh();
        })
    });

    // mark_failed()
    group.bench_function("mark_failed", |b| {
        b.iter(|| {
            let p = ProxyPool::new(config.clone());
            p.mark_failed();
        })
    });

    // len() / is_empty()
    group.bench_function("len_is_empty", |b| {
        b.iter(|| {
            let len = black_box(&pool).len();
            let empty = black_box(&pool).is_empty();
            black_box((len, empty));
        })
    });

    // Arc<ProxyPool> ProxyRefresh trait
    let arc_pool = Arc::new(ProxyPool::new(config.clone()));
    group.bench_function("arc_proxy_refresh", |b| {
        b.iter(|| {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let _ = black_box(&arc_pool).refresh_proxy_if_expired().await;
                let _ = black_box(&arc_pool).current_proxy().await;
            })
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 5: edge_cases
// ============================================================================

fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("proxy_pool_edge_cases");

    // ProxyEntry Display/Debug
    let entry = ProxyEntry::parse("http://user:pass@proxy.example.com:8080").unwrap();
    group.bench_function("proxy_entry_debug", |b| {
        b.iter(|| {
            let s = format!("{:?}", black_box(&entry));
            black_box(s);
        })
    });

    // ProxyEntry PartialEq
    let entry2 = entry.clone();
    group.bench_function("proxy_entry_eq", |b| {
        b.iter(|| {
            let eq = black_box(&entry) == black_box(&entry2);
            black_box(eq);
        })
    });

    // load_proxies_from_env (无环境变量设置)
    group.bench_function("load_proxies_from_env_empty", |b| {
        b.iter(|| {
            // 注意: 不修改实际环境变量, 仅测试无设置时的性能
            let proxies = load_proxies_from_env();
            black_box(proxies);
        })
    });

    // 空代理池刷新不 panic
    let empty_pool = ProxyPool::new(ProxyConfig::default());
    group.bench_function("empty_pool_refresh_no_panic", |b| {
        b.iter(|| {
            let _ = black_box(&empty_pool).refresh();
        })
    });

    // build_reqwest_proxy
    group.bench_function("build_reqwest_proxy_http", |b| {
        b.iter(|| {
            let _ = build_reqwest_proxy(black_box("http://127.0.0.1:8080"));
        })
    });

    group.bench_function("build_reqwest_proxy_socks5", |b| {
        b.iter(|| {
            let _ = build_reqwest_proxy(black_box("socks5://127.0.0.1:1080"));
        })
    });

    group.bench_function("build_reqwest_proxy_invalid", |b| {
        b.iter(|| {
            let _ = build_reqwest_proxy(black_box("invalid"));
        })
    });

    group.finish();
}

// ============================================================================
//  配置 & 入口
// ============================================================================

fn configure_criterion() -> Criterion {
    Criterion::default()
        .sample_size(50)
        .measurement_time(std::time::Duration::from_secs(5))
        .warm_up_time(std::time::Duration::from_secs(2))
        .nresamples(50_000)
        .noise_threshold(0.05)
        .output_directory(std::path::Path::new("target/criterion/proxy_pool"))
}

criterion_group! {
    name = proxy_pool_benches;
    config = configure_criterion();
    targets = bench_is_valid_proxy_url,
        bench_proxy_entry_parse,
        bench_validate_proxy_list,
        bench_proxy_pool_operations,
        bench_edge_cases,
}

criterion_main!(proxy_pool_benches);
