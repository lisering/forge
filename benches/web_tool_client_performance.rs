#![allow(clippy::useless_vec)]

//! web_tool_client 性能基准测试
//!
//! 测试目标:
//! 1. url_encoding - URL 编码函数 (ASCII/特殊字符/Unicode/批量)
//! 2. mock_tool_construction - MockWebTool 构建器链
//! 3. mock_tool_operations - MockWebTool 搜索/导航操作 (tokio runtime async)
//! 4. web_search_result - WebSearchResult 构造/clone/批量
//! 5. edge_cases - 边界场景 (空/Unicode/大响应/默认值)

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use forge::traits::{WebSearchResult, WebTool};
use forge::web_tool_client::{url_encode, MockWebTool};

// ============================================================================
//  基准测试 1: url_encoding
// ============================================================================

fn bench_url_encoding(c: &mut Criterion) {
    let mut group = c.benchmark_group("url_encoding");

    // ASCII 纯字母数字 (无需编码)
    group.bench_function("ascii_alphanumeric", |b| {
        b.iter(|| {
            let result = url_encode(black_box("rustlang2024"));
            black_box(result);
        })
    });

    // 带空格的查询
    group.bench_function("with_spaces", |b| {
        b.iter(|| {
            let result = url_encode(black_box("how to use rust async"));
            black_box(result);
        })
    });

    // 特殊字符
    group.bench_function("special_chars", |b| {
        b.iter(|| {
            let result = url_encode(black_box("c++ & python != rust"));
            black_box(result);
        })
    });

    // Unicode 中文
    group.bench_function("unicode_chinese", |b| {
        b.iter(|| {
            let result = url_encode(black_box("Rust 编程语言入门教程"));
            black_box(result);
        })
    });

    // Unicode 混合 (中日韩 + 英文 + 特殊)
    group.bench_function("unicode_mixed", |b| {
        b.iter(|| {
            let result = url_encode(black_box("Rust プログラミング & 한글 == 测试"));
            black_box(result);
        })
    });

    // URL 路径
    group.bench_function("url_path", |b| {
        b.iter(|| {
            let result = url_encode(black_box("https://doc.rust-lang.org/std/"));
            black_box(result);
        })
    });

    // 空字符串
    group.bench_function("empty_string", |b| {
        b.iter(|| {
            let result = url_encode(black_box(""));
            black_box(result);
        })
    });

    // 长查询 (200 字符)
    let long_query = "rust programming language tutorial ".repeat(6);
    group.bench_function("long_query_200", |b| {
        b.iter(|| {
            let result = url_encode(black_box(&long_query));
            black_box(result);
        })
    });

    // 批量编码 100 个查询
    let queries: Vec<String> = (0..100)
        .map(|i| format!("query {} with spaces & symbols!", i))
        .collect();
    group.bench_function("batch_100", |b| {
        b.iter(|| {
            let results: Vec<String> = black_box(&queries).iter().map(|q| url_encode(q)).collect();
            black_box(results);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 2: mock_tool_construction
// ============================================================================

fn bench_mock_tool_construction(c: &mut Criterion) {
    let mut group = c.benchmark_group("mock_tool_construction");

    // 默认构造
    group.bench_function("new_default", |b| {
        b.iter(|| {
            let tool = MockWebTool::new();
            black_box(tool);
        })
    });

    // 默认构造 (Default trait)
    group.bench_function("default_trait", |b| {
        b.iter(|| {
            let tool = MockWebTool::default();
            black_box(tool);
        })
    });

    // 带 1 个预编程响应
    group.bench_function("with_1_response", |b| {
        b.iter(|| {
            let tool =
                MockWebTool::new().with_response("rust", "Rust is a systems programming language.");
            black_box(tool);
        })
    });

    // 带 5 个预编程响应
    group.bench_function("with_5_responses", |b| {
        b.iter(|| {
            let tool = MockWebTool::new()
                .with_response("rust", "Rust documentation")
                .with_response("python", "Python documentation")
                .with_response("go", "Go documentation")
                .with_response("javascript", "JavaScript documentation")
                .with_response("java", "Java documentation");
            black_box(tool);
        })
    });

    // 带 20 个预编程响应
    group.bench_function("with_20_responses", |b| {
        b.iter(|| {
            let mut tool = MockWebTool::new();
            for i in 0..20 {
                tool = tool.with_response(
                    &format!("query_{i}"),
                    &format!("Response content for query {i}"),
                );
            }
            black_box(tool);
        })
    });

    // 带 100 个预编程响应
    group.bench_function("with_100_responses", |b| {
        b.iter(|| {
            let mut tool = MockWebTool::new();
            for i in 0..100 {
                tool = tool.with_response(
                    &format!("query_{i}"),
                    &format!("Response content for query {i}"),
                );
            }
            black_box(tool);
        })
    });

    // Clone 操作
    group.bench_function("clone_tool", |b| {
        let tool = MockWebTool::new()
            .with_response("rust", "Rust documentation")
            .with_response("python", "Python documentation");
        b.iter(|| {
            let cloned = black_box(&tool).clone();
            black_box(cloned);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 3: mock_tool_operations
// ============================================================================

fn bench_mock_tool_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("mock_tool_operations");

    let rt = tokio::runtime::Runtime::new().unwrap();

    // 搜索 - 命中预编程响应
    let tool_with_response = MockWebTool::new().with_response(
        "rust",
        "# Rust documentation\n\nRust is a systems programming language.",
    );

    group.bench_function("search_hit", |b| {
        b.iter(|| {
            let result = rt
                .block_on(tool_with_response.search_web(
                    black_box("rust"),
                    black_box(None),
                    black_box(None),
                ))
                .unwrap();
            black_box(result);
        })
    });

    // 搜索 - 未命中 (使用默认响应)
    let tool_default = MockWebTool::new();

    group.bench_function("search_miss", |b| {
        b.iter(|| {
            let result = rt
                .block_on(tool_default.search_web(
                    black_box("unknown_query"),
                    black_box(None),
                    black_box(None),
                ))
                .unwrap();
            black_box(result);
        })
    });

    // 搜索 - 带指定 URL
    group.bench_function("search_with_url", |b| {
        b.iter(|| {
            let result = rt
                .block_on(tool_default.search_web(
                    black_box("test"),
                    black_box(Some("https://docs.example.com")),
                    black_box(None),
                ))
                .unwrap();
            black_box(result);
        })
    });

    // 导航并提取
    group.bench_function("navigate_and_extract", |b| {
        b.iter(|| {
            let result = rt
                .block_on(
                    tool_default.navigate_and_extract(
                        black_box("https://example.com/page"),
                        black_box(None),
                    ),
                )
                .unwrap();
            black_box(result);
        })
    });

    // 组合操作: 搜索 3 次 (不同查询)
    group.bench_function("search_3x_different", |b| {
        b.iter(|| {
            let r1 = rt
                .block_on(tool_default.search_web("rust", None, None))
                .unwrap();
            let r2 = rt
                .block_on(tool_default.search_web("python", None, None))
                .unwrap();
            let r3 = rt
                .block_on(tool_default.search_web("go", None, None))
                .unwrap();
            black_box((r1.content.len(), r2.content.len(), r3.content.len()));
        })
    });

    // 批量搜索 10 次
    group.bench_function("search_batch_10", |b| {
        b.iter(|| {
            let mut total_len = 0usize;
            for _ in 0..10 {
                let result = rt
                    .block_on(tool_with_response.search_web(
                        black_box("rust"),
                        black_box(None),
                        black_box(None),
                    ))
                    .unwrap();
                total_len += result.content.len();
            }
            black_box(total_len);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 4: web_search_result
// ============================================================================

fn bench_web_search_result(c: &mut Criterion) {
    let mut group = c.benchmark_group("web_search_result");

    // 构造
    group.bench_function("construct_small", |b| {
        b.iter(|| {
            let result = WebSearchResult {
                content: "Small content".to_string(),
                query: "test".to_string(),
                duration_ms: 100,
            };
            black_box(result);
        })
    });

    // 构造大内容
    let large_content =
        "# Documentation\n\n".to_string() + &"This is a line of content.\n".repeat(500);
    group.bench_function("construct_large", |b| {
        b.iter(|| {
            let result = WebSearchResult {
                content: large_content.clone(),
                query: "large query".to_string(),
                duration_ms: 5000,
            };
            black_box(result);
        })
    });

    // Clone 小
    let small_result = WebSearchResult {
        content: "Small content".to_string(),
        query: "test".to_string(),
        duration_ms: 100,
    };
    group.bench_function("clone_small", |b| {
        b.iter(|| {
            let cloned = black_box(&small_result).clone();
            black_box(cloned);
        })
    });

    // Clone 大
    let large_result = WebSearchResult {
        content: large_content.clone(),
        query: "large query".to_string(),
        duration_ms: 5000,
    };
    group.bench_function("clone_large", |b| {
        b.iter(|| {
            let cloned = black_box(&large_result).clone();
            black_box(cloned);
        })
    });

    // 字段访问
    group.bench_function("field_access", |b| {
        b.iter(|| {
            let result = black_box(&large_result);
            let content_len = result.content.len();
            let query_len = result.query.len();
            let duration = result.duration_ms;
            black_box((content_len, query_len, duration));
        })
    });

    // 批量构造 100 个结果
    group.bench_function("batch_100", |b| {
        b.iter(|| {
            let results: Vec<WebSearchResult> = (0..100)
                .map(|i| WebSearchResult {
                    content: format!("Content {i}"),
                    query: format!("query_{i}"),
                    duration_ms: i as u64 * 10,
                })
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
    let mut group = c.benchmark_group("web_tool_client_edge_cases");

    let rt = tokio::runtime::Runtime::new().unwrap();

    // url_encode 空字符串
    group.bench_function("url_encode_empty", |b| {
        b.iter(|| {
            let result = url_encode(black_box(""));
            black_box(result);
        })
    });

    // url_encode 单字符
    group.bench_function("url_encode_single_char", |b| {
        b.iter(|| {
            let result = url_encode(black_box("a"));
            black_box(result);
        })
    });

    // url_encode 纯特殊字符 (全部需要编码)
    group.bench_function("url_encode_all_special", |b| {
        b.iter(|| {
            let result = url_encode(black_box("!@#$%^&*(){}[]<>?/\\|~`"));
            black_box(result);
        })
    });

    // url_encode 纯 Unicode
    group.bench_function("url_encode_pure_unicode", |b| {
        b.iter(|| {
            let result = url_encode(black_box("你好世界日本語한국어"));
            black_box(result);
        })
    });

    // url_encode 超长字符串 (10KB)
    let very_long = "test query with spaces ".repeat(400);
    group.bench_function("url_encode_very_long", |b| {
        b.iter(|| {
            let result = url_encode(black_box(&very_long));
            black_box(result.len());
        })
    });

    // MockWebTool 默认搜索 (无预编程响应)
    let tool_default = MockWebTool::default();
    group.bench_function("default_tool_search", |b| {
        b.iter(|| {
            let result = rt
                .block_on(tool_default.search_web(
                    black_box("anything"),
                    black_box(None),
                    black_box(None),
                ))
                .unwrap();
            black_box(result.content.len());
        })
    });

    // MockWebTool 带 100 响应后搜索
    let mut tool_100 = MockWebTool::new();
    for i in 0..100 {
        tool_100 = tool_100.with_response(
            &format!("query_{i}"),
            &format!("Response {i} with some content"),
        );
    }
    group.bench_function("tool_100_responses_search", |b| {
        b.iter(|| {
            let result = rt
                .block_on(tool_100.search_web(
                    black_box("query_50"),
                    black_box(None),
                    black_box(None),
                ))
                .unwrap();
            black_box(result.content);
        })
    });

    // 大内容响应搜索
    let large_response = "# Large Response\n\n".to_string() + &"content line\n".repeat(1000);
    let tool_large = MockWebTool::new().with_response("large", &large_response);
    group.bench_function("search_large_response", |b| {
        b.iter(|| {
            let result = rt
                .block_on(tool_large.search_web(
                    black_box("large"),
                    black_box(None),
                    black_box(None),
                ))
                .unwrap();
            black_box(result.content.len());
        })
    });

    // 混合操作: url_encode + 搜索
    group.bench_function("encode_then_search", |b| {
        b.iter(|| {
            let encoded = url_encode("rust async programming");
            let result = rt
                .block_on(tool_default.search_web(&encoded, None, None))
                .unwrap();
            black_box((encoded, result.content.len()));
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
        .output_directory(std::path::Path::new("target/criterion/web_tool_client"))
}

criterion_group! {
    name = web_tool_client_benches;
    config = configure_criterion();
    targets = bench_url_encoding,
        bench_mock_tool_construction,
        bench_mock_tool_operations,
        bench_web_search_result,
        bench_edge_cases,
}

criterion_main!(web_tool_client_benches);
