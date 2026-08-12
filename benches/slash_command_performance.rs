#![allow(clippy::useless_vec)]

//! Slash Command 模块性能基准测试
//!
//! 测试目标:
//! 1. parse_from_response — 指令解析性能 (多行/代码块/大小写)
//! 2. strip_commands — 指令移除性能
//! 3. deduplicate_commands — 指令去重性能
//! 4. format_summary_report — 统计报告格式化性能
//! 5. edge_cases — 边界条件 (辅助函数/空文本/超长文本)

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use forge::slash_command::{
    self, compute_execution_rate, deduplicate_commands, format_summary_report, is_boundary_char,
    is_code_block_boundary, is_known_keyword, parse_from_response, strip_commands, SlashCommand,
};

/// 构建包含 N 个指令的 AI 回复
fn build_response_with_commands(count: usize) -> String {
    let mut text = String::from("好的，我来处理。\n");
    let commands = ["skip", "compact", "refocus", "retry", "escalate"];
    for i in 0..count {
        let cmd = commands[i % commands.len()];
        text.push_str(&format!("/{cmd}\n"));
        text.push_str(&format!("处理步骤 {i}\n"));
    }
    text.push_str("```file:src/main.rs\nfn main() {}\n```");
    text
}

/// 构建包含代码块和指令的回复
fn build_response_with_code_blocks(block_count: usize) -> String {
    let mut text = String::new();
    for i in 0..block_count {
        text.push_str(&format!("代码块 {i}:\n"));
        text.push_str("```rust\n");
        text.push_str("// /skip 不应被检测到\n");
        text.push_str(&format!("let x = {};\n", i));
        text.push_str("```\n\n");
    }
    text.push_str("/skip\n");
    text
}

/// 基准测试: parse_from_response
fn bench_parse_from_response(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse_from_response");

    let sizes: Vec<usize> = vec![0, 1, 5, 20, 100];

    for &size in &sizes {
        group.throughput(Throughput::Elements(size as u64));
        let response = build_response_with_commands(size);

        group.bench_with_input(
            BenchmarkId::new("commands", size),
            &response,
            |b, response| b.iter(|| black_box(parse_from_response(black_box(response)))),
        );
    }

    // 代码块内的指令不应被检测
    let block_sizes: Vec<usize> = vec![1, 5, 20];
    for &size in &block_sizes {
        group.throughput(Throughput::Elements(size as u64));
        let response = build_response_with_code_blocks(size);

        group.bench_with_input(
            BenchmarkId::new("code_blocks", size),
            &response,
            |b, response| b.iter(|| black_box(parse_from_response(black_box(response)))),
        );
    }

    // 搜索指令带查询参数
    let search_response = "好的\n/search Rust async runtime comparison\n完成";
    group.bench_function("search_with_query", |b| {
        b.iter(|| black_box(parse_from_response(black_box(search_response))))
    });

    // 混合大小写
    let mixed_case = "done\n/SKIP\n/Compact\n/REFOCUS";
    group.bench_function("mixed_case", |b| {
        b.iter(|| black_box(parse_from_response(black_box(mixed_case))))
    });

    // 无指令的纯代码回复
    let pure_code = "```file:src/main.rs\nfn main() {\n    println!(\"hello\");\n}\n```";
    group.bench_function("no_commands", |b| {
        b.iter(|| black_box(parse_from_response(black_box(pure_code))))
    });

    group.finish();
}

/// 基准测试: strip_commands
fn bench_strip_commands(c: &mut Criterion) {
    let mut group = c.benchmark_group("strip_commands");

    let sizes: Vec<usize> = vec![0, 1, 5, 20, 100];

    for &size in &sizes {
        group.throughput(Throughput::Elements(size as u64));
        let response = build_response_with_commands(size);

        group.bench_with_input(BenchmarkId::new("strip", size), &response, |b, response| {
            b.iter(|| black_box(strip_commands(black_box(response))))
        });
    }

    // 代码块内不应被移除
    let code_response = "```rust\n// /skip\nlet x = 1;\n```\n/skip";
    group.bench_function("code_block_protected", |b| {
        b.iter(|| black_box(strip_commands(black_box(code_response))))
    });

    group.finish();
}

/// 基准测试: deduplicate_commands
fn bench_deduplicate_commands(c: &mut Criterion) {
    let mut group = c.benchmark_group("deduplicate_commands");

    let sizes: Vec<usize> = vec![1, 5, 20, 100];

    for &size in &sizes {
        group.throughput(Throughput::Elements(size as u64));
        // 构建大量重复指令
        let commands: Vec<SlashCommand> = (0..size)
            .map(|i| match i % 5 {
                0 => SlashCommand::Skip,
                1 => SlashCommand::Compact,
                2 => SlashCommand::Refocus,
                3 => SlashCommand::Retry,
                _ => SlashCommand::Escalate,
            })
            .collect();

        group.bench_with_input(
            BenchmarkId::new("with_dups", size),
            &commands,
            |b, commands| b.iter(|| black_box(deduplicate_commands(black_box(commands.clone())))),
        );

        // 无重复指令
        let unique: Vec<SlashCommand> = vec![
            SlashCommand::Skip,
            SlashCommand::Compact,
            SlashCommand::Refocus,
            SlashCommand::Retry,
            SlashCommand::Escalate,
        ];
        group.bench_with_input(BenchmarkId::new("no_dups", size), &unique, |b, commands| {
            b.iter(|| black_box(deduplicate_commands(black_box(commands.clone()))))
        });
    }

    // 空列表
    let empty: Vec<SlashCommand> = vec![];
    group.bench_function("empty", |b| {
        b.iter(|| black_box(deduplicate_commands(black_box(empty.clone()))))
    });

    group.finish();
}

/// 基准测试: format_summary_report
fn bench_format_summary_report(c: &mut Criterion) {
    let mut group = c.benchmark_group("format_summary_report");

    #[allow(clippy::type_complexity)]
    let cases: Vec<(&str, usize, usize, usize, usize, usize, usize, usize)> = vec![
        ("zero", 0, 0, 0, 0, 0, 0, 0),
        ("minimal", 1, 1, 1, 0, 0, 0, 0),
        ("balanced", 10, 8, 2, 3, 2, 2, 1),
        ("large", 1000, 950, 200, 300, 150, 200, 100),
    ];

    for (name, total, executed, skipped, compacts, refocuses, retries, escalations) in &cases {
        group.bench_function(*name, |b| {
            b.iter(|| {
                black_box(format_summary_report(
                    black_box(*total),
                    black_box(*executed),
                    black_box(*skipped),
                    black_box(*compacts),
                    black_box(*refocuses),
                    black_box(*retries),
                    black_box(*escalations),
                ))
            })
        });
    }
    group.finish();
}

/// 边界条件基准测试
fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_cases");

    // is_code_block_boundary
    let boundary_cases: Vec<(&str, &str)> = vec![
        ("rust", "```rust"),
        ("indented", "  ```"),
        ("plain", "```"),
        ("not_boundary", "code"),
        ("inline", "text with `inline`"),
    ];
    for (name, line) in &boundary_cases {
        group.bench_function(format!("code_block_boundary/{name}"), |b| {
            b.iter(|| black_box(is_code_block_boundary(black_box(line))))
        });
    }

    // is_known_keyword
    let keyword_cases: Vec<(&str, &str)> = vec![
        ("compact", "compact"),
        ("skip_upper", "SKIP"),
        ("search", "search"),
        ("unknown", "foobar"),
        ("empty", ""),
    ];
    for (name, kw) in &keyword_cases {
        group.bench_function(format!("known_keyword/{name}"), |b| {
            b.iter(|| black_box(is_known_keyword(black_box(kw))))
        });
    }

    // is_boundary_char
    for c in [' ', '.', '\n', 'a', '1', '/'] {
        group.bench_function(format!("boundary_char/{c:?}"), |b| {
            b.iter(|| black_box(is_boundary_char(black_box(c))))
        });
    }

    // compute_execution_rate
    let rate_cases: Vec<(&str, usize, usize)> = vec![
        ("zero", 0, 0),
        ("half", 10, 5),
        ("third", 3, 1),
        ("full", 100, 100),
    ];
    for (name, total, executed) in &rate_cases {
        group.bench_function(format!("execution_rate/{name}"), |b| {
            b.iter(|| {
                black_box(compute_execution_rate(
                    black_box(*total),
                    black_box(*executed),
                ))
            })
        });
    }

    // SlashCommand 方法
    group.bench_function("keyword_skip", |b| {
        b.iter(|| black_box(SlashCommand::Skip.keyword()))
    });
    group.bench_function("full_command_compact", |b| {
        b.iter(|| black_box(SlashCommand::Compact.full_command()))
    });
    group.bench_function("from_keyword_search", |b| {
        b.iter(|| black_box(SlashCommand::from_keyword(black_box("search"))))
    });

    // has_command
    let text_with_skip = "done\n/skip\nmore text";
    group.bench_function("has_command_found", |b| {
        b.iter(|| {
            black_box(slash_command::has_command(
                black_box(text_with_skip),
                SlashCommand::Skip,
            ))
        })
    });
    group.bench_function("has_command_not_found", |b| {
        b.iter(|| {
            black_box(slash_command::has_command(
                black_box("just text"),
                SlashCommand::Skip,
            ))
        })
    });

    // 超长文本解析
    let huge_text = format!("{}\n/skip\n", "正常文本行\n".repeat(1000));
    group.throughput(Throughput::Bytes(huge_text.len() as u64));
    group.bench_function("parse_huge_text", |b| {
        b.iter(|| black_box(parse_from_response(black_box(&huge_text))))
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
        .output_directory(std::path::Path::new("target/criterion/slash_command"))
}

criterion_group! {
    name = slash_command_benches;
    config = configure_criterion();
    targets =
        bench_parse_from_response,
        bench_strip_commands,
        bench_deduplicate_commands,
        bench_format_summary_report,
        bench_edge_cases,
}

criterion_main!(slash_command_benches);
