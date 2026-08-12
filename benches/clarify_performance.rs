#![allow(clippy::useless_vec)]

//! Clarify 模块性能基准测试
//!
//! 测试目标:
//! 1. check_normal — 正常代码回复检测性能 (快速路径, 无需追问)
//! 2. check_question — 中英文提问检测性能
//! 3. check_uncertainty — 不确定标记检测性能
//! 4. check_short — 过短回复检测性能
//! 5. edge_cases — 边界条件 (超时/重复/上限/长文本)

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use forge::clarify::HeuristicClarificationChecker;
use forge::traits::{ClarificationChecker, ClarificationContext};

/// 创建默认上下文 (0 次追问, 最大 2 次)
fn ctx() -> ClarificationContext {
    ClarificationContext {
        task_prompt: "创建一个 Rust CLI 工具".to_string(),
        timed_out: false,
        questions_asked: 0,
        max_questions: 2,
        previous_questions: vec![],
    }
}

/// 创建已追问 2 次的上下文 (达到上限)
fn ctx_maxed() -> ClarificationContext {
    ClarificationContext {
        task_prompt: "test".to_string(),
        timed_out: false,
        questions_asked: 2,
        max_questions: 2,
        previous_questions: vec!["q1".to_string(), "q2".to_string()],
    }
}

/// 正常代码回复 (包含 file: 格式代码块)
fn normal_response() -> String {
    "好的，我来创建项目。\n\
     ```file:src/main.rs\n\
     fn main() {\n\
         println!(\"hello\");\n\
     }\n\
     ```\n\
     ```file:Cargo.toml\n\
     [package]\n\
     name = \"test\"\n\
     version = \"0.1.0\"\n\
     ```"
    .to_string()
}

/// 包含中文提问的回复
fn chinese_question_response() -> String {
    "你希望使用哪种框架？是 Actix 还是 Axum？".to_string()
}

/// 包含英文提问的回复
fn english_question_response() -> String {
    "Would you like me to use Tokio or async-std for this project?".to_string()
}

/// 包含不确定标记的回复
fn uncertainty_response() -> String {
    "There are multiple approaches to solve this. Let me think.".to_string()
}

/// 过短回复
fn short_response() -> String {
    "ok".to_string()
}

/// 超长正常回复
fn long_normal_response() -> String {
    let mut response =
        String::from("我来帮你创建这个项目。首先创建 Cargo.toml 文件，然后创建 src/main.rs 文件。");
    response.push_str("代码结构清晰，功能完整。\n");
    response.push_str("```file:Cargo.toml\n[package]\nname = \"myapp\"\nversion = \"0.1.0\"\nedition = \"2021\"\n```\n");
    response.push_str("```file:src/main.rs\nfn main() {\n    println!(\"Hello, World!\");\n}\n```");
    response
}

/// 基准测试: check — 正常代码回复 (快速路径)
fn bench_check_normal(c: &mut Criterion) {
    let mut group = c.benchmark_group("check_normal");

    let checker = HeuristicClarificationChecker::new();
    let context = ctx();
    let response = normal_response();

    group.bench_function("normal_code_response", |b| {
        b.iter(|| {
            black_box(
                tokio::runtime::Runtime::new()
                    .unwrap()
                    .block_on(checker.check(black_box(&response), black_box(&context))),
            )
        })
    });

    let long_response = long_normal_response();
    group.bench_function("long_normal_response", |b| {
        b.iter(|| {
            black_box(
                tokio::runtime::Runtime::new()
                    .unwrap()
                    .block_on(checker.check(black_box(&long_response), black_box(&context))),
            )
        })
    });

    group.finish();
}

/// 基准测试: check — 中英文提问检测
fn bench_check_question(c: &mut Criterion) {
    let mut group = c.benchmark_group("check_question");

    let checker = HeuristicClarificationChecker::new();
    let context = ctx();

    let cases: Vec<(&str, String)> = vec![
        ("chinese_question", chinese_question_response()),
        ("english_question", english_question_response()),
        (
            "chinese_please_tell",
            "请告诉我你想要实现什么功能？".to_string(),
        ),
        (
            "english_please_clarify",
            "Please clarify what kind of CLI commands you need.".to_string(),
        ),
        (
            "question_mark_only",
            "这是代码\n```rust\nlet x = 1;\n```\n明白了？".to_string(),
        ),
    ];

    for (name, response) in &cases {
        group.bench_function(*name, |b| {
            b.iter(|| {
                black_box(
                    tokio::runtime::Runtime::new()
                        .unwrap()
                        .block_on(checker.check(black_box(response), black_box(&context))),
                )
            })
        });
    }
    group.finish();
}

/// 基准测试: check — 不确定标记检测
fn bench_check_uncertainty(c: &mut Criterion) {
    let mut group = c.benchmark_group("check_uncertainty");

    let checker = HeuristicClarificationChecker::new();
    let context = ctx();

    let cases: Vec<(&str, String)> = vec![
        ("multiple_approaches", uncertainty_response()),
        (
            "either_option",
            "There are either option to consider.".to_string(),
        ),
        ("or_you_can", "或者你可以选择另一种方案。".to_string()),
        ("two_solutions", "两种方案都可以实现。".to_string()),
    ];

    for (name, response) in &cases {
        group.bench_function(*name, |b| {
            b.iter(|| {
                black_box(
                    tokio::runtime::Runtime::new()
                        .unwrap()
                        .block_on(checker.check(black_box(response), black_box(&context))),
                )
            })
        });
    }
    group.finish();
}

/// 基准测试: check — 过短回复检测
fn bench_check_short(c: &mut Criterion) {
    let mut group = c.benchmark_group("check_short");

    let checker = HeuristicClarificationChecker::new();
    let context = ctx();

    let cases: Vec<(&str, String)> = vec![
        ("very_short", short_response()),
        ("empty", String::new()),
        ("whitespace_only", "   \n   \t  ".to_string()),
        (
            "just_above_threshold",
            "这是一段足够长的回复，超过了阈值。".to_string(),
        ),
    ];

    for (name, response) in &cases {
        group.bench_function(*name, |b| {
            b.iter(|| {
                black_box(
                    tokio::runtime::Runtime::new()
                        .unwrap()
                        .block_on(checker.check(black_box(response), black_box(&context))),
                )
            })
        });
    }

    // 自定义阈值
    let checker_high = HeuristicClarificationChecker::new().with_min_response_len(1000);
    let response = "这是一段普通长度的回复。";
    group.bench_function("custom_high_threshold", |b| {
        b.iter(|| {
            black_box(
                tokio::runtime::Runtime::new()
                    .unwrap()
                    .block_on(checker_high.check(black_box(response), black_box(&context))),
            )
        })
    });

    group.finish();
}

/// 边界条件基准测试
fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_cases");

    let checker = HeuristicClarificationChecker::new();

    // 超时检测 (最高优先级)
    let mut timeout_ctx = ctx();
    timeout_ctx.timed_out = true;
    let response = "你希望用什么框架？";
    group.bench_function("timeout_detection", |b| {
        b.iter(|| {
            black_box(
                tokio::runtime::Runtime::new()
                    .unwrap()
                    .block_on(checker.check(black_box(response), black_box(&timeout_ctx))),
            )
        })
    });

    // 超时 + 空回复
    group.bench_function("timeout_empty_response", |b| {
        b.iter(|| {
            black_box(
                tokio::runtime::Runtime::new()
                    .unwrap()
                    .block_on(checker.check(black_box(""), black_box(&timeout_ctx))),
            )
        })
    });

    // 达到最大追问次数
    let maxed_ctx = ctx_maxed();
    group.bench_function("max_questions_reached", |b| {
        b.iter(|| {
            black_box(
                tokio::runtime::Runtime::new()
                    .unwrap()
                    .block_on(checker.check(black_box("你希望用什么？"), black_box(&maxed_ctx))),
            )
        })
    });

    // 重复问题检测
    let response = "你希望用哪种框架？";
    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(checker.check(response, &ctx()));
    let dup_ctx = ClarificationContext {
        task_prompt: "test".to_string(),
        timed_out: false,
        questions_asked: 1,
        max_questions: 2,
        previous_questions: vec![result.question.clone()],
    };
    group.bench_function("duplicate_question", |b| {
        b.iter(|| {
            black_box(
                tokio::runtime::Runtime::new()
                    .unwrap()
                    .block_on(checker.check(black_box(response), black_box(&dup_ctx))),
            )
        })
    });

    // 大量代码块中的问号 (代码块内不检测)
    let big_code_response = format!(
        "好的，代码如下:\n```rust\n{}?\n```\n完成。",
        "let x = 1".repeat(100)
    );
    group.throughput(Throughput::Bytes(big_code_response.len() as u64));
    group.bench_function("large_code_block", |b| {
        b.iter(|| {
            black_box(
                tokio::runtime::Runtime::new()
                    .unwrap()
                    .block_on(checker.check(black_box(&big_code_response), black_box(&ctx()))),
            )
        })
    });

    // 超长文本 (5000 字符)
    let long_text = format!("这是一段很长的文本。{}", "正常内容。".repeat(500));
    group.throughput(Throughput::Bytes(long_text.len() as u64));
    group.bench_function("very_long_text", |b| {
        b.iter(|| {
            black_box(
                tokio::runtime::Runtime::new()
                    .unwrap()
                    .block_on(checker.check(black_box(&long_text), black_box(&ctx()))),
            )
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
        .output_directory(std::path::Path::new("target/criterion/clarify"))
}

criterion_group! {
    name = clarify_benches;
    config = configure_criterion();
    targets =
        bench_check_normal,
        bench_check_question,
        bench_check_uncertainty,
        bench_check_short,
        bench_edge_cases,
}

criterion_main!(clarify_benches);
