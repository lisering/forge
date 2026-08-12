#![allow(clippy::useless_vec)]

//! web_tool 性能基准测试
//!
//! 测试目标:
//! 1. build_js_functions - JavaScript 代码生成函数
//! 2. parse_scroll_result - 滚动结果解析
//! 3. is_page_ready_from_probe - 页面就绪检测
//! 4. js_content_validation - JS 内容完整性验证
//! 5. edge_cases - 边界场景 (空/长文本/异常格式)

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use forge::web_tool::*;

// ============================================================================
//  基准测试 1: build_js_functions
// ============================================================================

fn bench_build_js_functions(c: &mut Criterion) {
    let mut group = c.benchmark_group("build_js_functions");

    group.bench_function("build_scroll_dynamic_page_js", |b| {
        b.iter(|| {
            let js = build_scroll_dynamic_page_js();
            black_box(js);
        })
    });

    group.bench_function("build_extract_page_content_js", |b| {
        b.iter(|| {
            let js = build_extract_page_content_js();
            black_box(js);
        })
    });

    group.bench_function("build_extract_search_results_js", |b| {
        b.iter(|| {
            let js = build_extract_search_results_js();
            black_box(js);
        })
    });

    group.bench_function("build_page_probe_js", |b| {
        b.iter(|| {
            let js = build_page_probe_js();
            black_box(js);
        })
    });

    group.bench_function("build_page_ready_condition_js", |b| {
        b.iter(|| {
            let js = build_page_ready_condition_js();
            black_box(js);
        })
    });

    // 全部 JS 函数一次调用
    group.bench_function("all_js_functions", |b| {
        b.iter(|| {
            let _ = build_scroll_dynamic_page_js();
            let _ = build_extract_page_content_js();
            let _ = build_extract_search_results_js();
            let _ = build_page_probe_js();
            let _ = build_page_ready_condition_js();
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 2: parse_scroll_result
// ============================================================================

fn bench_parse_scroll_result(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse_scroll_result");

    let test_cases = vec![
        ("scrolled", "scrolled 15 text=12345"),
        ("skipped", "scroll skipped hooks=0 text=0"),
        ("unchanged", "scroll probe unchanged text=500"),
        ("large", "scrolled 28 text=999999"),
        ("max_steps", "scrolled 28 text=1000000"),
    ];

    for (name, input) in &test_cases {
        group.bench_function(*name, |b| {
            b.iter(|| {
                let (steps, text) = parse_scroll_result(black_box(input));
                black_box((steps, text));
            })
        });
    }

    // 空字符串和异常格式
    let edge_cases = &["", "no numbers here", "scrolled text=", "text=100"];
    group.bench_function("edge_inputs", |b| {
        b.iter(|| {
            for input in black_box(edge_cases) {
                let _ = parse_scroll_result(input);
            }
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 3: is_page_ready_from_probe
// ============================================================================

fn bench_is_page_ready_from_probe(c: &mut Criterion) {
    let mut group = c.benchmark_group("is_page_ready_from_probe");

    let ready_cases = vec![
        "https://example.com/page\ncomplete\n12345",
        "https://example.com/page\ninteractive\n500",
        "https://example.com/page\ncomplete\n999999",
    ];

    group.bench_function("ready_cases", |b| {
        b.iter(|| {
            for probe in black_box(&ready_cases) {
                let _ = is_page_ready_from_probe(probe);
            }
        })
    });

    let not_ready_cases = vec![
        "https://example.com/page\nloading\n0",
        "https://example.com/page\ncomplete\n0",
        "about:blank\ncomplete\n0",
        "only one line",
        "",
    ];

    group.bench_function("not_ready_cases", |b| {
        b.iter(|| {
            for probe in black_box(&not_ready_cases) {
                let _ = is_page_ready_from_probe(probe);
            }
        })
    });

    // 批量处理 100 条探测
    let batch: Vec<String> = (0..100)
        .map(|i| {
            if i % 2 == 0 {
                format!("https://example.com/{i}\ncomplete\n{}", i * 100)
            } else {
                format!("https://example.com/{i}\nloading\n0")
            }
        })
        .collect();

    group.bench_function("batch_100", |b| {
        b.iter(|| {
            let results: Vec<bool> = black_box(&batch)
                .iter()
                .map(|p| is_page_ready_from_probe(p))
                .collect();
            black_box(results);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 4: js_content_validation
// ============================================================================

fn bench_js_content_validation(c: &mut Criterion) {
    let mut group = c.benchmark_group("js_content_validation");

    // 验证 scroll JS 包含关键元素
    group.bench_function("validate_scroll_js", |b| {
        b.iter(|| {
            let js = build_scroll_dynamic_page_js();
            assert!(js.contains("scrollTo"));
            assert!(js.contains("Promise"));
            assert!(js.contains("hookCount"));
            assert!(js.contains("atBottom"));
            assert!(js.contains("lazy"));
            black_box(js);
        })
    });

    // 验证 extract page JS 包含关键元素
    group.bench_function("validate_extract_page_js", |b| {
        b.iter(|| {
            let js = build_extract_page_content_js();
            assert!(js.contains("querySelectorAll"));
            assert!(js.contains("Markdown"));
            assert!(js.contains("visible"));
            black_box(js);
        })
    });

    // 验证 search results JS 包含关键元素
    group.bench_function("validate_search_results_js", |b| {
        b.iter(|| {
            let js = build_extract_search_results_js();
            assert!(js.contains("Google"));
            assert!(js.contains("links"));
            black_box(js);
        })
    });

    // JS 字符串长度
    group.bench_function("js_lengths", |b| {
        b.iter(|| {
            let scroll_len = build_scroll_dynamic_page_js().len();
            let extract_len = build_extract_page_content_js().len();
            let search_len = build_extract_search_results_js().len();
            let probe_len = build_page_probe_js().len();
            let ready_len = build_page_ready_condition_js().len();
            black_box((scroll_len, extract_len, search_len, probe_len, ready_len));
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 5: edge_cases
// ============================================================================

fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("web_tool_edge_cases");

    // parse_scroll_result 极大值
    group.bench_function("parse_extreme_values", |b| {
        b.iter(|| {
            let (s, t) = parse_scroll_result("scrolled 4294967295 text=18446744073709551615");
            black_box((s, t));
        })
    });

    // parse_scroll_result 零值
    group.bench_function("parse_zero_values", |b| {
        b.iter(|| {
            let (s, t) = parse_scroll_result("scrolled 0 text=0");
            black_box((s, t));
        })
    });

    // is_page_ready_from_probe 多行变体
    let multi_line = "https://example.com/page\ncomplete\n12345\nextra\nlines";
    group.bench_function("probe_multi_line", |b| {
        b.iter(|| {
            let r = is_page_ready_from_probe(black_box(multi_line));
            black_box(r);
        })
    });

    // JS 字符串包含检查 (模拟实际使用)
    group.bench_function("js_contains_check", |b| {
        b.iter(|| {
            let js = build_scroll_dynamic_page_js();
            let has_scroll = js.contains("scrollTo");
            let has_promise = js.contains("Promise");
            let has_hooks = js.contains("hookCount");
            black_box((has_scroll, has_promise, has_hooks));
        })
    });

    // 反复构建所有 JS (模拟初始化)
    group.bench_function("init_all_js", |b| {
        b.iter(|| {
            let js1 = build_scroll_dynamic_page_js();
            let js2 = build_extract_page_content_js();
            let js3 = build_extract_search_results_js();
            let js4 = build_page_probe_js();
            let js5 = build_page_ready_condition_js();
            // 模拟存储
            let total = js1.len() + js2.len() + js3.len() + js4.len() + js5.len();
            black_box(total);
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
        .output_directory(std::path::Path::new("target/criterion/web_tool"))
}

criterion_group! {
    name = web_tool_benches;
    config = configure_criterion();
    targets = bench_build_js_functions,
        bench_parse_scroll_result,
        bench_is_page_ready_from_probe,
        bench_js_content_validation,
        bench_edge_cases,
}

criterion_main!(web_tool_benches);
