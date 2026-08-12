#![allow(clippy::useless_vec)]

//! cdp 性能基准测试
//!
//! 测试目标:
//! 1. value_conversion - Value 类型转换 (value_as_bool, value_to_string)
//! 2. js_builders - JS 表达式构建函数
//! 3. response_extractors - CDP 响应提取函数
//! 4. command_building - CDP 命令构建和解析
//! 5. edge_cases - 边界场景 (空值/极端值/批量)

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use forge::cdp::*;
use serde_json::{json, Value};

// ============================================================================
//  基准测试 1: value_conversion
// ============================================================================

fn bench_value_conversion(c: &mut Criterion) {
    let mut group = c.benchmark_group("value_conversion");

    // value_as_bool 各种类型
    let test_values = vec![
        ("bool_true", json!(true)),
        ("bool_false", json!(false)),
        ("string_nonempty", json!("hello")),
        ("string_empty", json!("")),
        ("number_nonzero", json!(42)),
        ("number_zero", json!(0)),
        ("null", Value::Null),
        ("array", json!([1, 2, 3])),
        ("object", json!({"key": "value"})),
    ];

    for (name, value) in &test_values {
        group.bench_function(format!("value_as_bool_{}", name), |b| {
            b.iter(|| {
                let result = value_as_bool(black_box(value));
                black_box(result);
            })
        });
    }

    // 批量 value_as_bool
    group.bench_function("value_as_bool_batch_9", |b| {
        b.iter(|| {
            for (_, value) in black_box(&test_values) {
                let _ = value_as_bool(value);
            }
        })
    });

    // value_to_string 各种类型
    for (name, value) in &test_values {
        group.bench_function(format!("value_to_string_{}", name), |b| {
            b.iter(|| {
                let result = value_to_string(black_box(value.clone()));
                black_box(result);
            })
        });
    }

    // is_printable_key
    let keys = vec![
        ("single_char", "a"),
        ("enter", "Enter"),
        ("tab", "Tab"),
        ("escape", "Escape"),
        ("space", " "),
        ("digit", "1"),
    ];
    for (name, key) in &keys {
        group.bench_function(format!("is_printable_key_{}", name), |b| {
            b.iter(|| {
                let result = is_printable_key(black_box(key));
                black_box(result);
            })
        });
    }

    group.finish();
}

// ============================================================================
//  基准测试 2: js_builders
// ============================================================================

fn bench_js_builders(c: &mut Criterion) {
    let mut group = c.benchmark_group("js_builders");

    // build_length_js
    let expressions = vec![
        ("simple", "document.title"),
        ("medium", "document.querySelector('.content').innerText"),
        ("complex", "(() => { let r = document.querySelectorAll('p'); return Array.from(r).map(p => p.textContent).join('\\n'); })()"),
    ];
    for (name, expr) in &expressions {
        group.bench_function(format!("build_length_js_{}", name), |b| {
            b.iter(|| {
                let js = build_length_js(black_box(expr));
                black_box(js);
            })
        });
    }

    // build_chunk_js
    let offsets_sizes = vec![(0, 50000), (50000, 100000), (100000, 150000)];
    for &(offset, end) in &offsets_sizes {
        group.bench_function(format!("build_chunk_js_{}_{}", offset, end), |b| {
            b.iter(|| {
                let js = build_chunk_js(black_box("document.body.innerText"), offset, end);
                black_box(js);
            })
        });
    }

    // build_condition_js
    let conditions = vec![
        ("simple", "document.querySelector('#ready') !== null"),
        ("medium", "document.querySelectorAll('.item').length > 5"),
        ("complex", "(() => { let el = document.querySelector('.status'); return el && el.textContent.includes('done'); })()"),
    ];
    for (name, cond) in &conditions {
        group.bench_function(format!("build_condition_js_{}", name), |b| {
            b.iter(|| {
                let js = build_condition_js(black_box(cond));
                black_box(js);
            })
        });
    }

    // build_focus_js
    let selectors = vec![
        ("id", "#input"),
        ("class", ".textarea"),
        ("complex", "div.container > form > textarea[name='message']"),
    ];
    for (name, selector) in &selectors {
        group.bench_function(format!("build_focus_js_{}", name), |b| {
            b.iter(|| {
                let js = build_focus_js(black_box(selector));
                black_box(js);
            })
        });
    }

    // build_file_change_js
    group.bench_function("build_file_change_js", |b| {
        b.iter(|| {
            let js = build_file_change_js(black_box("input[type='file']"));
            black_box(js);
        })
    });

    // 批量构建所有 JS
    group.bench_function("build_all_js", |b| {
        b.iter(|| {
            let _ = build_length_js("document.title");
            let _ = build_chunk_js("document.body.innerText", 0, 50000);
            let _ = build_condition_js("document.readyState === 'complete'");
            let _ = build_focus_js("#input");
            let _ = build_file_change_js("input[type='file']");
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 3: response_extractors
// ============================================================================

fn bench_response_extractors(c: &mut Criterion) {
    let mut group = c.benchmark_group("response_extractors");

    // extract_result (成功)
    let success_response = json!({
        "id": 1,
        "result": {"value": 42}
    });
    group.bench_function("extract_result_success", |b| {
        b.iter(|| {
            let result = extract_result(black_box(&success_response), "Runtime.evaluate").unwrap();
            black_box(result);
        })
    });

    // extract_result (错误)
    let error_response = json!({
        "id": 1,
        "error": {"message": "Cannot find context"}
    });
    group.bench_function("extract_result_error", |b| {
        b.iter(|| {
            let result =
                extract_result(black_box(&error_response), "Runtime.evaluate").unwrap_err();
            black_box(result);
        })
    });

    // extract_evaluate_value (有值)
    let eval_result = json!({
        "result": {"value": "Hello World"}
    });
    group.bench_function("extract_evaluate_value", |b| {
        b.iter(|| {
            let value = extract_evaluate_value(black_box(&eval_result)).unwrap();
            black_box(value);
        })
    });

    // extract_evaluate_value (异常)
    let eval_exception = json!({
        "exceptionDetails": {"text": "SyntaxError: unexpected token"}
    });
    group.bench_function("extract_evaluate_exception", |b| {
        b.iter(|| {
            let value = extract_evaluate_value(black_box(&eval_exception)).unwrap_err();
            black_box(value);
        })
    });

    // extract_node_id
    let dom_response = json!({"nodeId": 42});
    group.bench_function("extract_node_id", |b| {
        b.iter(|| {
            let id = extract_node_id(black_box(&dom_response), "node not found").unwrap();
            black_box(id);
        })
    });

    // extract_root_node_id
    let doc_response = json!({"root": {"nodeId": 1}});
    group.bench_function("extract_root_node_id", |b| {
        b.iter(|| {
            let id = extract_root_node_id(black_box(&doc_response)).unwrap();
            black_box(id);
        })
    });

    // extract_response_id
    let text_responses = vec![
        ("response", r#"{"id": 42, "result": {}}"#),
        ("event", r#"{"method": "Page.loadEventFired"}"#),
        ("invalid", "not json"),
    ];
    for (name, text) in &text_responses {
        group.bench_function(format!("extract_response_id_{}", name), |b| {
            b.iter(|| {
                let id = extract_response_id(black_box(text));
                black_box(id);
            })
        });
    }

    // extract_browser_ws_url
    let version_response = json!({
        "webSocketDebuggerUrl": "ws://localhost:9222/devtools/browser/abc-123"
    });
    group.bench_function("extract_browser_ws_url", |b| {
        b.iter(|| {
            let url = extract_browser_ws_url(black_box(&version_response)).unwrap();
            black_box(url);
        })
    });

    // extract_target_id
    let target_response = json!({
        "result": {"targetId": "target-123"}
    });
    group.bench_function("extract_target_id", |b| {
        b.iter(|| {
            let id = extract_target_id(black_box(&target_response)).unwrap();
            black_box(id);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 4: command_building
// ============================================================================

fn bench_command_building(c: &mut Criterion) {
    let mut group = c.benchmark_group("command_building");

    // build_command 不同方法
    let commands = vec![
        ("page_enable", "Page.enable", json!({})),
        (
            "runtime_eval",
            "Runtime.evaluate",
            json!({
                "expression": "document.title",
                "returnByValue": true
            }),
        ),
        (
            "input_key",
            "Input.dispatchKeyEvent",
            json!({
                "type": "char",
                "text": "a"
            }),
        ),
        (
            "dom_query",
            "DOM.querySelector",
            json!({
                "nodeId": 1,
                "selector": ".chat-assistant"
            }),
        ),
        (
            "navigate",
            "Page.navigate",
            json!({
                "url": "https://chat.z.ai"
            }),
        ),
    ];

    for (name, method, params) in &commands {
        group.bench_function(format!("build_command_{}", name), |b| {
            b.iter(|| {
                let cmd = build_command(black_box(1), black_box(method), black_box(params.clone()));
                black_box(cmd);
            })
        });
    }

    // 不同 ID
    for &id in &[1u32, 100, 1000, 65535] {
        group.bench_function(format!("build_command_id_{}", id), |b| {
            b.iter(|| {
                let cmd = build_command(black_box(id), "Page.enable", json!({}));
                black_box(cmd);
            })
        });
    }

    // 批量构建
    group.bench_function("build_command_batch_100", |b| {
        b.iter(|| {
            let cmds: Vec<Value> = (0..100)
                .map(|i| {
                    build_command(
                        i as u32 + 1,
                        "Runtime.evaluate",
                        json!({"expression": format!("fn{}()", i)}),
                    )
                })
                .collect();
            black_box(cmds);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 5: edge_cases
// ============================================================================

fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("cdp_edge_cases");

    // needs_chunking 边界
    group.bench_function("needs_chunking_at_boundary", |b| {
        b.iter(|| {
            let at = needs_chunking(black_box(50000), 50000);
            let below = needs_chunking(black_box(49999), 50000);
            let above = needs_chunking(black_box(50001), 50000);
            black_box((at, below, above));
        })
    });

    // is_result_complete 边界
    group.bench_function("is_result_complete_edge", |b| {
        b.iter(|| {
            let zero = is_result_complete(black_box(0), 100);
            let equal = is_result_complete(black_box(100), 100);
            let less = is_result_complete(black_box(50), 100);
            let more = is_result_complete(black_box(200), 100);
            black_box((zero, equal, less, more));
        })
    });

    // extract_result 空响应
    let empty_response = json!({});
    group.bench_function("extract_result_empty", |b| {
        b.iter(|| {
            let result = extract_result(black_box(&empty_response), "test").unwrap();
            black_box(result);
        })
    });

    // extract_node_id 缺失
    let no_node = json!({});
    group.bench_function("extract_node_id_missing", |b| {
        b.iter(|| {
            let result = extract_node_id(black_box(&no_node), "missing node").unwrap_err();
            black_box(result);
        })
    });

    // extract_response_id 大量消息
    let messages: Vec<String> = (0..100)
        .map(|i| {
            if i % 2 == 0 {
                format!(r#"{{"id": {}, "result": {{}}}}"#, i)
            } else {
                r#"{"method": "Page.frameNavigated", "params": {}}}"#.to_string()
            }
        })
        .collect();
    group.bench_function("extract_response_id_batch_100", |b| {
        b.iter(|| {
            let ids: Vec<Option<u32>> = black_box(&messages)
                .iter()
                .map(|m| extract_response_id(m))
                .collect();
            black_box(ids);
        })
    });

    // value_as_bool 全类型批量
    let all_types = vec![
        json!(true),
        json!(false),
        json!("text"),
        json!(""),
        json!(1),
        json!(0),
        json!(-1),
        Value::Null,
        json!([1]),
        json!({"k": "v"}),
    ];
    group.bench_function("value_as_bool_all_types", |b| {
        b.iter(|| {
            let results: Vec<bool> = black_box(&all_types).iter().map(value_as_bool).collect();
            black_box(results);
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
        .output_directory(std::path::Path::new("target/criterion/cdp"))
}

criterion_group! {
    name = cdp_benches;
    config = configure_criterion();
    targets = bench_value_conversion,
        bench_js_builders,
        bench_response_extractors,
        bench_command_building,
        bench_edge_cases,
}

criterion_main!(cdp_benches);
