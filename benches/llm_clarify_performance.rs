#![allow(clippy::useless_vec)]

//! LLM Clarify 模块性能基准测试
//!
//! 测试目标:
//! 1. classify_llm_failure — LLM 失败类型分类性能
//! 2. should_retry_llm — 重试决策性能
//! 3. truncate_response — 回复截断性能
//! 4. parse_llm_judge_result — LLM 判断结果解析性能
//! 5. edge_cases — 边界条件 (build_judge_prompt/is_duplicate/follow_up)

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use forge::llm_clarify::{
    self, build_default_follow_up_message, build_judge_prompt_text, classify_llm_failure,
    is_duplicate_question, parse_llm_judge_result, should_retry_llm, truncate_response,
    LlmFailureType,
};
use forge::traits::ClarificationContext;

/// 创建默认上下文
fn ctx() -> ClarificationContext {
    ClarificationContext {
        task_prompt: "创建一个 Rust CLI 工具".to_string(),
        timed_out: false,
        questions_asked: 0,
        max_questions: 2,
        previous_questions: vec![],
    }
}

/// 基准测试: classify_llm_failure
fn bench_classify_llm_failure(c: &mut Criterion) {
    let mut group = c.benchmark_group("classify_llm_failure");

    let cases: Vec<(&str, &str)> = vec![
        (
            "connection_refused",
            "Ollama 请求失败: error sending request",
        ),
        ("connection_reset", "connection reset by peer"),
        ("timeout", "operation timed out"),
        ("request_timeout", "request timeout"),
        ("deadline", "deadline exceeded"),
        ("http_500", "Ollama 返回错误状态: 500"),
        ("http_404", "error status 404"),
        ("parse_error", "Ollama 响应解析失败"),
        ("json_parse", "json parse error"),
        ("unknown", "some unknown error"),
        ("empty", ""),
        ("uppercase", "TIMED OUT"),
        ("mixed_case", "Connection Refused"),
    ];

    for (name, msg) in &cases {
        group.bench_function(*name, |b| {
            b.iter(|| black_box(classify_llm_failure(black_box(msg))))
        });
    }
    group.finish();
}

/// 基准测试: should_retry_llm
fn bench_should_retry_llm(c: &mut Criterion) {
    let mut group = c.benchmark_group("should_retry_llm");

    let failure_types: Vec<(&str, LlmFailureType)> = vec![
        ("timeout", LlmFailureType::Timeout),
        ("connection_refused", LlmFailureType::ConnectionRefused),
        ("http_error", LlmFailureType::HttpError),
        ("parse_error", LlmFailureType::ParseError),
        ("other", LlmFailureType::Other),
    ];

    let max_retries: u32 = 3;

    for (name, ft) in &failure_types {
        for attempt in 0..=max_retries {
            group.bench_function(format!("{name}/attempt_{attempt}"), |b| {
                b.iter(|| {
                    black_box(should_retry_llm(
                        black_box(ft),
                        black_box(attempt),
                        black_box(max_retries),
                    ))
                })
            });
        }
    }
    group.finish();
}

/// 基准测试: truncate_response
fn bench_truncate_response(c: &mut Criterion) {
    let mut group = c.benchmark_group("truncate_response");

    let sizes: Vec<usize> = vec![10, 100, 1000, 10000];

    for &size in &sizes {
        group.throughput(Throughput::Elements(size as u64));
        let text = "x".repeat(size);
        let half = size / 2;

        // 不截断 (max > len)
        group.bench_with_input(BenchmarkId::new("no_truncation", size), &text, |b, text| {
            b.iter(|| black_box(truncate_response(black_box(text), black_box(size + 100))))
        });

        // 截断到一半
        group.bench_with_input(BenchmarkId::new("truncate_half", size), &text, |b, text| {
            b.iter(|| black_box(truncate_response(black_box(text), black_box(half))))
        });

        // 截断到 0
        group.bench_with_input(BenchmarkId::new("truncate_zero", size), &text, |b, text| {
            b.iter(|| black_box(truncate_response(black_box(text), black_box(0))))
        });
    }

    // Unicode 文本
    let unicode_text = "你好世界测试".repeat(100);
    group.bench_function("unicode_truncate", |b| {
        b.iter(|| black_box(truncate_response(black_box(&unicode_text), black_box(50))))
    });

    // 空文本
    group.bench_function("empty_text", |b| {
        b.iter(|| black_box(truncate_response(black_box(""), black_box(100))))
    });

    group.finish();
}

/// 基准测试: parse_llm_judge_result
fn bench_parse_llm_judge_result(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse_llm_judge_result");

    let cases: Vec<(&str, &str)> = vec![
        (
            "needs_with_followup",
            "NEEDS_CLARIFICATION: AI 在提问\nFOLLOW_UP: 请直接选择最适合的框架",
        ),
        ("needs_no_followup", "NEEDS_CLARIFICATION: AI 回复不确定"),
        ("ok_simple", "OK"),
        ("ok_with_text", "OK, 一切正常"),
        (
            "multiline_reason",
            "NEEDS_CLARIFICATION: 第一行原因\n第二行不该出现\nFOLLOW_UP: test",
        ),
        ("unparseable", "无法解析的格式"),
        ("empty", ""),
    ];

    for (name, text) in &cases {
        group.bench_function(*name, |b| {
            b.iter(|| black_box(parse_llm_judge_result(black_box(text))))
        });
    }
    group.finish();
}

/// 边界条件基准测试
fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_cases");

    let context = ctx();

    // build_judge_prompt_text — 短回复
    let short_response = "好的，我来创建项目。";
    group.bench_function("build_prompt_short", |b| {
        b.iter(|| {
            black_box(build_judge_prompt_text(
                black_box(short_response),
                black_box(&context),
                black_box(2000),
            ))
        })
    });

    // build_judge_prompt_text — 长回复 (需要截断)
    let long_response = "x".repeat(5000);
    group.throughput(Throughput::Bytes(long_response.len() as u64));
    group.bench_function("build_prompt_long_truncated", |b| {
        b.iter(|| {
            black_box(build_judge_prompt_text(
                black_box(&long_response),
                black_box(&context),
                black_box(2000),
            ))
        })
    });

    // build_default_follow_up_message
    let long_reason = "这是一个非常长的原因".repeat(50);
    let reasons: Vec<(&str, &str)> = vec![
        ("short_reason", "AI 在提问"),
        ("empty_reason", ""),
        ("long_reason", &long_reason),
    ];

    for (name, reason) in &reasons {
        group.bench_function(format!("follow_up_{name}"), |b| {
            b.iter(|| black_box(build_default_follow_up_message(black_box(reason))))
        });
    }

    // is_duplicate_question
    let previous: Vec<String> = (0..50)
        .map(|i| format!("请直接选择最适合的框架并输出代码。版本{i}"))
        .collect();
    group.throughput(Throughput::Elements(previous.len() as u64));
    group.bench_function("duplicate_check_many_previous", |b| {
        b.iter(|| {
            black_box(is_duplicate_question(
                black_box("请直接选择最适合的框架并输出代码。版本0"),
                black_box(&previous),
                black_box(30),
            ))
        })
    });

    // is_duplicate_question — 空列表
    let empty_previous: Vec<String> = vec![];
    group.bench_function("duplicate_check_empty", |b| {
        b.iter(|| {
            black_box(is_duplicate_question(
                black_box("test"),
                black_box(&empty_previous),
                black_box(30),
            ))
        })
    });

    // classify_llm_failure — 超长错误消息
    let long_error = &"x".repeat(10000);
    group.bench_function("classify_long_error", |b| {
        b.iter(|| black_box(classify_llm_failure(black_box(long_error))))
    });

    // 完整纯函数调用链: classify → should_retry
    let error_msg = "operation timed out";
    group.bench_function("classify_then_retry_chain", |b| {
        b.iter(|| {
            let ft = classify_llm_failure(black_box(error_msg));
            black_box(should_retry_llm(black_box(&ft), black_box(0), black_box(2)))
        })
    });

    // llm_clarify 模块常量检查
    let _ = llm_clarify::LlmFailureType::Other;
    group.bench_function("const_access", |b| b.iter(|| black_box(1 + 1)));

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
        .output_directory(std::path::Path::new("target/criterion/llm_clarify"))
}

criterion_group! {
    name = llm_clarify_benches;
    config = configure_criterion();
    targets =
        bench_classify_llm_failure,
        bench_should_retry_llm,
        bench_truncate_response,
        bench_parse_llm_judge_result,
        bench_edge_cases,
}

criterion_main!(llm_clarify_benches);
