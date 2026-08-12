#![allow(clippy::useless_vec)]

//! browser 性能基准测试
//!
//! 测试目标:
//! 1. site_type_detection - SiteType::detect 不同 URL
//! 2. site_type_methods - new_conversation_url/page_ready_condition/is_known/display_name
//! 3. estimate_tokens - token 估算 (英文/中文/混合)
//! 4. looks_like_chat - 聊天页面识别 (正例/反例)
//! 5. edge_cases - 边界条件 (空/极值/批量)

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use forge::browser::{estimate_tokens, BrowserManager, ChatElements, SiteType};

// ============================================================================
//  基准测试 1: site_type_detection
// ============================================================================

fn bench_site_type_detection(c: &mut Criterion) {
    let mut group = c.benchmark_group("site_type_detection");

    let urls = [
        ("zai", "https://chat.z.ai/"),
        ("deepseek", "https://chat.deepseek.com/"),
        ("kimi", "https://kimi.moonshot.cn/"),
        ("tongyi", "https://tongyi.aliyun.com/"),
        ("claude", "https://claude.ai/new"),
        ("unknown", "https://example.com/"),
        ("empty", ""),
    ];

    for (name, url) in &urls {
        group.bench_function(format!("detect_{}", name), |b| {
            b.iter(|| {
                let site = SiteType::detect(black_box(url));
                black_box(site);
            })
        });
    }

    // 大小写无关
    group.bench_function("detect_uppercase", |b| {
        b.iter(|| {
            let site = SiteType::detect(black_box("HTTPS://CHAT.Z.AI/"));
            black_box(site);
        })
    });

    // 批量检测 50 个 URL
    let batch_urls: Vec<String> = (0..50)
        .map(|i| format!("https://chat.z.ai/chat/session{}", i))
        .collect();
    group.bench_function("batch_detect_50", |b| {
        b.iter(|| {
            let sites: Vec<SiteType> = black_box(&batch_urls)
                .iter()
                .map(|u| SiteType::detect(u))
                .collect();
            black_box(sites);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 2: site_type_methods
// ============================================================================

fn bench_site_type_methods(c: &mut Criterion) {
    let mut group = c.benchmark_group("site_type_methods");

    let sites = [
        SiteType::Zai,
        SiteType::DeepSeek,
        SiteType::Kimi,
        SiteType::Tongyi,
        SiteType::Claude,
        SiteType::Unknown,
    ];

    // new_conversation_url
    group.bench_function("new_conversation_url_all", |b| {
        b.iter(|| {
            for site in black_box(&sites) {
                let _ = site.new_conversation_url();
            }
        })
    });

    // page_ready_condition
    group.bench_function("page_ready_condition_all", |b| {
        b.iter(|| {
            for site in black_box(&sites) {
                let _ = site.page_ready_condition();
            }
        })
    });

    // is_known
    group.bench_function("is_known_all", |b| {
        b.iter(|| {
            for site in black_box(&sites) {
                let _ = site.is_known();
            }
        })
    });

    // display_name
    group.bench_function("display_name_all", |b| {
        b.iter(|| {
            for site in black_box(&sites) {
                let _ = site.display_name();
            }
        })
    });

    // Display trait
    group.bench_function("display_all", |b| {
        b.iter(|| {
            for site in black_box(&sites) {
                let _ = format!("{}", site);
            }
        })
    });

    // serde
    group.bench_function("serde_roundtrip", |b| {
        b.iter(|| {
            for site in black_box(&sites) {
                let json = serde_json::to_string(site).unwrap();
                let parsed: SiteType = serde_json::from_str(&json).unwrap();
                black_box(parsed);
            }
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 3: estimate_tokens
// ============================================================================

fn bench_estimate_tokens(c: &mut Criterion) {
    let mut group = c.benchmark_group("estimate_tokens");

    // 英文文本
    let english = "The quick brown fox jumps over the lazy dog. This is a test sentence.";
    group.bench_function("english_short", |b| {
        b.iter(|| {
            let tokens = estimate_tokens(black_box(english));
            black_box(tokens);
        })
    });

    // 中文文本
    let chinese = "快速的棕色狐狸跳过了懒惰的狗。这是一个测试句子。";
    group.bench_function("chinese_short", |b| {
        b.iter(|| {
            let tokens = estimate_tokens(black_box(chinese));
            black_box(tokens);
        })
    });

    // 混合文本
    let mixed = "The quick 狐狸 jumps 懒惰的狗. Mixed 文本 with English and 中文.";
    group.bench_function("mixed_short", |b| {
        b.iter(|| {
            let tokens = estimate_tokens(black_box(mixed));
            black_box(tokens);
        })
    });

    // 空文本
    group.bench_function("empty", |b| {
        b.iter(|| {
            let tokens = estimate_tokens(black_box(""));
            black_box(tokens);
        })
    });

    // 大文本 (10k 英文字符)
    let large_english = "word ".repeat(2000);
    group.bench_function("english_10k", |b| {
        b.iter(|| {
            let tokens = estimate_tokens(black_box(&large_english));
            black_box(tokens);
        })
    });

    // 大文本 (10k 中文字符)
    let large_chinese = "测试".repeat(5000);
    group.bench_function("chinese_10k", |b| {
        b.iter(|| {
            let tokens = estimate_tokens(black_box(&large_chinese));
            black_box(tokens);
        })
    });

    // 批量 100 条短文本
    let batch: Vec<String> = (0..100)
        .map(|i| format!("Task {} description with some text content {}", i, i))
        .collect();
    group.bench_function("batch_100", |b| {
        b.iter(|| {
            let tokens: Vec<usize> = black_box(&batch)
                .iter()
                .map(|t| estimate_tokens(t))
                .collect();
            black_box(tokens);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 4: looks_like_chat
// ============================================================================

fn bench_looks_like_chat(c: &mut Criterion) {
    let mut group = c.benchmark_group("looks_like_chat");

    // 正例
    let positives = [
        ("zai", "https://chat.z.ai/", "Z.ai"),
        ("deepseek", "https://chat.deepseek.com/", "DeepSeek"),
        ("claude", "https://claude.ai/", "Claude"),
        ("kimi", "https://kimi.moonshot.cn/", "Kimi"),
        ("chatgpt", "https://chatgpt.com/", "ChatGPT"),
    ];

    for (name, url, title) in &positives {
        group.bench_function(format!("positive_{}", name), |b| {
            b.iter(|| {
                let result = BrowserManager::looks_like_chat(black_box(url), black_box(title));
                black_box(result);
            })
        });
    }

    // 反例
    let negatives = [
        ("google", "https://google.com/", "Google"),
        ("github", "https://github.com/", "GitHub"),
        (
            "youtube",
            "https://youtube.com/watch?v=123",
            "AIProg Tutorial",
        ),
    ];

    for (name, url, title) in &negatives {
        group.bench_function(format!("negative_{}", name), |b| {
            b.iter(|| {
                let result = BrowserManager::looks_like_chat(black_box(url), black_box(title));
                black_box(result);
            })
        });
    }

    // 中文关键词
    group.bench_function("chinese_keyword_doubao", |b| {
        b.iter(|| {
            let result = BrowserManager::looks_like_chat("https://doubao.com/", "豆包");
            black_box(result);
        })
    });

    group.bench_function("chinese_keyword_yiyan", |b| {
        b.iter(|| {
            let result = BrowserManager::looks_like_chat("https://yiyan.baidu.com/", "文心一言");
            black_box(result);
        })
    });

    // 批量 50 对
    let batch: Vec<(String, String)> = (0..50)
        .map(|i| {
            (
                format!("https://chat.z.ai/session{}", i),
                format!("Chat {}", i),
            )
        })
        .collect();
    group.bench_function("batch_50", |b| {
        b.iter(|| {
            let results: Vec<bool> = black_box(&batch)
                .iter()
                .map(|(u, t)| BrowserManager::looks_like_chat(u, t))
                .collect();
            black_box(results);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 5: edge_cases
// ============================================================================

fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_cases");

    // ChatElements default
    group.bench_function("chat_elements_default", |b| {
        b.iter(|| {
            let elements = ChatElements::default();
            black_box(elements);
        })
    });

    // ChatElements serde roundtrip
    let elements = ChatElements::default();
    group.bench_function("chat_elements_serde", |b| {
        b.iter(|| {
            let json = serde_json::to_string(black_box(&elements)).unwrap();
            let parsed: ChatElements = serde_json::from_str(&json).unwrap();
            black_box(parsed);
        })
    });

    // SiteType all variants detect
    let all_urls = [
        "https://chat.z.ai/",
        "https://z.ai/path",
        "https://chat.deepseek.com/",
        "https://deepseek.com/",
        "https://kimi.moonshot.cn/",
        "https://moonshot.cn/",
        "https://tongyi.aliyun.com/",
        "https://aliyun.com/bot",
        "https://claude.ai/",
        "https://claude.ai/new",
        "https://example.com/",
        "",
    ];
    group.bench_function("detect_all_variants", |b| {
        b.iter(|| {
            let sites: Vec<SiteType> = black_box(&all_urls)
                .iter()
                .map(|u| SiteType::detect(u))
                .collect();
            black_box(sites);
        })
    });

    // estimate_tokens 极端情况
    group.bench_function("tokens_single_char", |b| {
        b.iter(|| {
            let tokens = estimate_tokens(black_box("a"));
            black_box(tokens);
        })
    });

    group.bench_function("tokens_single_chinese", |b| {
        b.iter(|| {
            let tokens = estimate_tokens(black_box("你"));
            black_box(tokens);
        })
    });

    // 50k 纯英文
    let huge = "a".repeat(50_000);
    group.bench_function("tokens_50k_english", |b| {
        b.iter(|| {
            let tokens = estimate_tokens(black_box(&huge));
            black_box(tokens);
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
        .output_directory(std::path::Path::new("target/criterion/browser"))
}

criterion_group! {
    name = browser_benches;
    config = configure_criterion();
    targets = bench_site_type_detection,
        bench_site_type_methods,
        bench_estimate_tokens,
        bench_looks_like_chat,
        bench_edge_cases,
}

criterion_main!(browser_benches);
