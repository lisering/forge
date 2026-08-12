#![allow(clippy::useless_vec)]

//! chat 性能基准测试
//!
//! 测试目标:
//! 1. timeout_config - TimeoutConfig 创建和方法 (default/new/from_timeout_secs)
//! 2. timeout_config_methods - total_max_secs/has_stuck_detection/for_site_type
//! 3. chat_message_serde - ChatMessage 序列化/反序列化
//! 4. response_result - ResponseResult 构造和字段访问
//! 5. edge_cases - 边界场景 (极值/大量配置/序列化往返)

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use forge::browser::SiteType;
use forge::chat::{ChatMessage, ResponseResult, TimeoutConfig};
use std::time::Duration;

// ============================================================================
//  基准测试 1: timeout_config
// ============================================================================

fn bench_timeout_config(c: &mut Criterion) {
    let mut group = c.benchmark_group("timeout_config");

    // default
    group.bench_function("default", |b| {
        b.iter(|| {
            let config = TimeoutConfig::default();
            black_box(config);
        })
    });

    // new
    group.bench_function("new", |b| {
        b.iter(|| {
            let config = TimeoutConfig::new(black_box(15), black_box(90), black_box(45));
            black_box(config);
        })
    });

    // from_timeout_secs 不同值
    let timeouts = vec![30u64, 60, 120, 300, 600];
    for &secs in &timeouts {
        group.bench_function(format!("from_timeout_secs_{}", secs), |b| {
            b.iter(|| {
                let config = TimeoutConfig::from_timeout_secs(black_box(secs));
                black_box(config);
            })
        });
    }

    // with_stuck_threshold builder
    group.bench_function("with_stuck_threshold", |b| {
        b.iter(|| {
            let config = TimeoutConfig::new(30, 60, 45).with_stuck_threshold(black_box(180));
            black_box(config);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 2: timeout_config_methods
// ============================================================================

fn bench_timeout_config_methods(c: &mut Criterion) {
    let mut group = c.benchmark_group("timeout_config_methods");

    let config = TimeoutConfig::default();

    // total_max_secs
    group.bench_function("total_max_secs", |b| {
        b.iter(|| {
            let total = config.total_max_secs();
            black_box(total);
        })
    });

    // has_stuck_detection (true)
    group.bench_function("has_stuck_detection_true", |b| {
        b.iter(|| {
            let has = config.has_stuck_detection();
            black_box(has);
        })
    });

    // has_stuck_detection (false)
    let config_no_stuck = TimeoutConfig::from_timeout_secs(120);
    group.bench_function("has_stuck_detection_false", |b| {
        b.iter(|| {
            let has = config_no_stuck.has_stuck_detection();
            black_box(has);
        })
    });

    // for_site_type 不同网站
    let sites = vec![
        ("zai", SiteType::Zai),
        ("deepseek", SiteType::DeepSeek),
        ("kimi", SiteType::Kimi),
        ("tongyi", SiteType::Tongyi),
        ("claude", SiteType::Claude),
        ("unknown", SiteType::Unknown),
    ];
    for (name, site) in &sites {
        group.bench_function(format!("for_site_type_{}", name), |b| {
            b.iter(|| {
                let adjusted = config.for_site_type(black_box(*site));
                black_box(adjusted);
            })
        });
    }

    // 批量 for_site_type (所有网站一次)
    group.bench_function("for_all_sites", |b| {
        b.iter(|| {
            for (_, site) in black_box(&sites) {
                let _ = config.for_site_type(*site);
            }
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 3: chat_message_serde
// ============================================================================

fn bench_chat_message_serde(c: &mut Criterion) {
    let mut group = c.benchmark_group("chat_message_serde");

    let msg = ChatMessage {
        role: "assistant".to_string(),
        content: "这是一个测试回复内容，包含一些中文和English混合文本。".to_string(),
        timestamp: "2025-01-15T10:30:00Z".to_string(),
    };

    // serialize
    group.bench_function("serialize", |b| {
        b.iter(|| {
            let json = serde_json::to_string(black_box(&msg)).unwrap();
            black_box(json);
        })
    });

    // deserialize
    let json_str = serde_json::to_string(&msg).unwrap();
    group.bench_function("deserialize", |b| {
        b.iter(|| {
            let msg: ChatMessage = serde_json::from_str(black_box(&json_str)).unwrap();
            black_box(msg);
        })
    });

    // serialize_roundtrip
    group.bench_function("roundtrip", |b| {
        b.iter(|| {
            let json = serde_json::to_string(&msg).unwrap();
            let back: ChatMessage = serde_json::from_str(&json).unwrap();
            black_box(back);
        })
    });

    // 大内容消息
    let large_msg = ChatMessage {
        role: "user".to_string(),
        content: "x".repeat(10000),
        timestamp: "2025-01-15T10:30:00Z".to_string(),
    };
    group.bench_function("serialize_large_10k", |b| {
        b.iter(|| {
            let json = serde_json::to_string(&large_msg).unwrap();
            black_box(json);
        })
    });

    // 批量序列化 100 条
    let messages: Vec<ChatMessage> = (0..100)
        .map(|i| ChatMessage {
            role: if i % 2 == 0 { "user" } else { "assistant" }.to_string(),
            content: format!("Message number {}", i),
            timestamp: format!("2025-01-15T10:{:02}:00Z", i % 60),
        })
        .collect();
    group.bench_function("serialize_batch_100", |b| {
        b.iter(|| {
            let json = serde_json::to_string(black_box(&messages)).unwrap();
            black_box(json);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 4: response_result
// ============================================================================

fn bench_response_result(c: &mut Criterion) {
    let mut group = c.benchmark_group("response_result");

    // 构造
    group.bench_function("construct", |b| {
        b.iter(|| {
            let result = ResponseResult {
                text: black_box("Hello world").to_string(),
                timed_out: false,
                elapsed: Duration::from_secs(5),
            };
            black_box(result);
        })
    });

    // 字段访问
    let result = ResponseResult {
        text: "AI回复内容".to_string(),
        timed_out: false,
        elapsed: Duration::from_millis(3500),
    };
    group.bench_function("field_access", |b| {
        b.iter(|| {
            let text = &black_box(&result).text;
            let timed_out = black_box(result.timed_out);
            let elapsed = black_box(result.elapsed);
            black_box((text, timed_out, elapsed));
        })
    });

    // 超时结果
    group.bench_function("construct_timeout", |b| {
        b.iter(|| {
            let result = ResponseResult {
                text: String::new(),
                timed_out: true,
                elapsed: Duration::from_secs(300),
            };
            black_box(result);
        })
    });

    // 大文本结果
    group.bench_function("construct_large_text", |b| {
        b.iter(|| {
            let result = ResponseResult {
                text: "x".repeat(50000),
                timed_out: false,
                elapsed: Duration::from_secs(60),
            };
            black_box(result);
        })
    });

    // clone
    group.bench_function("clone", |b| {
        b.iter(|| {
            // ResponseResult 没有 Clone, 使用文本字段复制
            let text = black_box(&result).text.clone();
            black_box(text);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 5: edge_cases
// ============================================================================

fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("chat_edge_cases");

    // 极值超时
    group.bench_function("from_timeout_secs_zero", |b| {
        b.iter(|| {
            let config = TimeoutConfig::from_timeout_secs(black_box(0));
            black_box(config);
        })
    });

    group.bench_function("from_timeout_secs_max", |b| {
        b.iter(|| {
            let config = TimeoutConfig::from_timeout_secs(black_box(u64::MAX));
            black_box(config);
        })
    });

    // 极大 stuck_threshold
    group.bench_function("with_stuck_threshold_max", |b| {
        b.iter(|| {
            let config = TimeoutConfig::default().with_stuck_threshold(black_box(u64::MAX));
            black_box(config);
        })
    });

    // 空消息序列化
    let empty_msg = ChatMessage {
        role: String::new(),
        content: String::new(),
        timestamp: String::new(),
    };
    group.bench_function("serialize_empty_message", |b| {
        b.iter(|| {
            let json = serde_json::to_string(&empty_msg).unwrap();
            black_box(json);
        })
    });

    // Unicode 消息序列化
    let unicode_msg = ChatMessage {
        role: "assistant".to_string(),
        content: "你好世界 🌍 Привет мир こんにちは世界".to_string(),
        timestamp: "2025-01-15T10:30:00Z".to_string(),
    };
    group.bench_function("serialize_unicode_message", |b| {
        b.iter(|| {
            let json = serde_json::to_string(&unicode_msg).unwrap();
            black_box(json);
        })
    });

    // 大量 TimeoutConfig 创建
    group.bench_function("batch_create_100_configs", |b| {
        b.iter(|| {
            let configs: Vec<TimeoutConfig> = (0..100)
                .map(|i| TimeoutConfig::from_timeout_secs(black_box(30 + i)))
                .collect();
            black_box(configs);
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
        .output_directory(std::path::Path::new("target/criterion/chat"))
}

criterion_group! {
    name = chat_benches;
    config = configure_criterion();
    targets = bench_timeout_config,
        bench_timeout_config_methods,
        bench_chat_message_serde,
        bench_response_result,
        bench_edge_cases,
}

criterion_main!(chat_benches);
