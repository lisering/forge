#![allow(clippy::useless_vec)]

//! Error Diagnosis 模块性能基准测试
//!
//! 测试目标:
//! 1. ErrorCategory::from_error_code / from_message - 错误分类性能
//! 2. extract_error_code_from_text - 错误码提取性能
//! 3. format_errors_for_prompt - 错误格式化性能
//! 4. parse_llm_diagnosis - LLM 诊断解析性能
//! 5. edge_cases - 边界条件性能

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use forge::error_diagnosis;
use forge::testrunner::CompileError;

/// 构建测试用 CompileError
fn make_error(code: Option<&str>, msg: &str, file: &str) -> CompileError {
    CompileError {
        file: file.to_string(),
        line: Some(10),
        column: Some(5),
        message: msg.to_string(),
        error_code: code.map(String::from),
    }
}

/// 基准测试: ErrorCategory 分类
fn bench_error_category(c: &mut Criterion) {
    let mut group = c.benchmark_group("error_category");

    let codes = vec![
        ("E0308", "type mismatch"),
        ("E0382", "use of moved value"),
        ("E0277", "trait bound not satisfied"),
        ("E0004", "non-exhaustive patterns"),
        ("E0106", "missing lifetime"),
        ("UNKNOWN", "unknown error"),
    ];

    for (code, msg) in &codes {
        group.bench_function(format!("from_code/{code}"), |b| {
            b.iter(|| {
                black_box(error_diagnosis::ErrorCategory::from_error_code(black_box(
                    code,
                )))
            })
        });

        group.bench_function(format!("from_msg/{code}"), |b| {
            b.iter(|| black_box(error_diagnosis::ErrorCategory::from_message(black_box(msg))))
        });
    }
    group.finish();
}

/// 基准测试: extract_error_code_from_text
fn bench_extract_error_code(c: &mut Criterion) {
    let mut group = c.benchmark_group("extract_error_code");

    let short_msg = "error[E0308]: mismatched types";
    let multi_line =
        "error[E0308]: mismatched types\nexpected `i32`, found `&str`\n --> src/main.rs:10:5";
    let no_code = "this is a regular message without error code";
    let long_msg = format!("error[E0308]: {}", "x".repeat(1000));

    let test_cases = vec![
        ("short", short_msg.to_string()),
        ("multi_line", multi_line.to_string()),
        ("no_code", no_code.to_string()),
        ("long", long_msg),
    ];

    for (name, msg) in &test_cases {
        group.bench_function(*name, |b| {
            b.iter(|| {
                black_box(error_diagnosis::extract_error_code_from_text(black_box(
                    msg,
                )))
            })
        });
    }
    group.finish();
}

/// 基准测试: format_errors_for_prompt
fn bench_format_errors_for_prompt(c: &mut Criterion) {
    let mut group = c.benchmark_group("format_errors_for_prompt");

    let sizes: Vec<usize> = vec![1, 5, 20, 100];

    for &size in &sizes {
        group.throughput(Throughput::Elements(size as u64));
        let errors: Vec<CompileError> = (0..size)
            .map(|i| {
                make_error(
                    Some("E0308"),
                    &format!("mismatched types: expected i32, found str in function {i}"),
                    &format!("src/module_{i}/handler.rs"),
                )
            })
            .collect();

        group.bench_with_input(BenchmarkId::new("format", size), &errors, |b, errs| {
            b.iter(|| black_box(error_diagnosis::format_errors_for_prompt(black_box(errs))))
        });
    }
    group.finish();
}

/// 基准测试: parse_llm_diagnosis
fn bench_parse_llm_diagnosis(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse_llm_diagnosis");

    let valid_response = "\
CATEGORY: TypeError
ANALYSIS: The variable `x` is declared as `i32` but assigned a `&str` value. \
This is a type mismatch that occurs because Rust is statically typed and \
requires explicit type conversions.
FIX_GUIDANCE: Use `x.parse::<i32>()` or `i32::from_str_radix()` to convert \
the string to an integer, or change the type of `x` to `&str`.";

    let missing_category = "\
ANALYSIS: Some analysis without a category field.
FIX_GUIDANCE: Some fix guidance.";

    let long_analysis = format!(
        "CATEGORY: BorrowError\nANALYSIS: {}\nFIX_GUIDANCE: Use clone() or Rc",
        "a".repeat(5000)
    );

    let test_cases = vec![
        ("valid", valid_response.to_string()),
        ("missing_category", missing_category.to_string()),
        ("long", long_analysis),
    ];

    for (name, response) in &test_cases {
        group.bench_function(*name, |b| {
            b.iter(|| black_box(error_diagnosis::parse_llm_diagnosis(black_box(response))))
        });
    }
    group.finish();
}

/// 边界条件基准测试
fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_cases");

    // 空错误列表
    let empty_errors: Vec<CompileError> = vec![];
    group.bench_function("empty_errors_format", |b| {
        b.iter(|| {
            black_box(error_diagnosis::format_errors_for_prompt(black_box(
                &empty_errors,
            )))
        })
    });

    // extract_field_value 边界
    group.bench_function("extract_field_empty", |b| {
        b.iter(|| {
            black_box(error_diagnosis::extract_field_value(
                black_box(""),
                black_box("CATEGORY"),
            ))
        })
    });

    // compute_diagnosis_confidence 边界
    let categories = vec![
        error_diagnosis::ErrorCategory::TypeError,
        error_diagnosis::ErrorCategory::Unknown,
        error_diagnosis::ErrorCategory::BorrowError,
    ];
    let sources = vec!["hybrid", "llm", "heuristic", "none"];

    for cat in &categories {
        for src in &sources {
            let cat_val = *cat;
            group.bench_function(format!("confidence_{cat_val:?}_{src}"), |b| {
                b.iter(|| {
                    black_box(error_diagnosis::compute_diagnosis_confidence(
                        black_box(cat_val),
                        black_box(*src),
                    ))
                })
            });
        }
    }

    // format_error_location 边界
    let no_line_error = CompileError {
        file: "src/main.rs".to_string(),
        line: None,
        column: None,
        message: "error".to_string(),
        error_code: None,
    };
    group.bench_function("format_location_no_line", |b| {
        b.iter(|| {
            black_box(error_diagnosis::format_error_location(black_box(
                &no_line_error,
            )))
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
        .output_directory(std::path::Path::new("target/criterion/error_diagnosis"))
}

criterion_group! {
    name = error_diagnosis_benches;
    config = configure_criterion();
    targets =
        bench_error_category,
        bench_extract_error_code,
        bench_format_errors_for_prompt,
        bench_parse_llm_diagnosis,
        bench_edge_cases,
}

criterion_main!(error_diagnosis_benches);
