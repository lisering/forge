#![allow(clippy::useless_vec)]

//! Prompt Builder 模块性能基准测试
//!
//! 测试目标:
//! 1. build - 完整系统约束构建性能
//! 2. build_for_planning - 规划阶段约束构建性能
//! 3. build_for_task - 任务执行约束构建性能
//! 4. build_brief - 简短约束构建性能
//! 5. edge_cases - 边界条件性能 (确定性/长度比较/重复调用)

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use forge::prompt_builder::SystemPrompt;

/// 基准测试: SystemPrompt::build
fn bench_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("build");

    group.throughput(Throughput::Elements(1));
    group.bench_function("full_prompt", |b| {
        b.iter(|| black_box(SystemPrompt::build()))
    });

    // 多次调用 (测试 String 分配性能)
    group.throughput(Throughput::Elements(10));
    group.bench_function("x10_calls", |b| {
        b.iter(|| {
            for _ in 0..10 {
                black_box(SystemPrompt::build());
            }
        })
    });

    group.finish();
}

/// 基准测试: SystemPrompt::build_for_planning
fn bench_build_for_planning(c: &mut Criterion) {
    let mut group = c.benchmark_group("build_for_planning");

    group.throughput(Throughput::Elements(1));
    group.bench_function("planning_prompt", |b| {
        b.iter(|| black_box(SystemPrompt::build_for_planning()))
    });

    // 重复调用 (模拟多阶段场景)
    group.throughput(Throughput::Elements(5));
    group.bench_function("x5_phases", |b| {
        b.iter(|| {
            for _ in 0..5 {
                black_box(SystemPrompt::build_for_planning());
            }
        })
    });

    group.finish();
}

/// 基准测试: SystemPrompt::build_for_task
fn bench_build_for_task(c: &mut Criterion) {
    let mut group = c.benchmark_group("build_for_task");

    group.throughput(Throughput::Elements(1));
    group.bench_function("task_prompt", |b| {
        b.iter(|| black_box(SystemPrompt::build_for_task()))
    });

    // 重复调用 (模拟多任务场景)
    group.throughput(Throughput::Elements(20));
    group.bench_function("x20_tasks", |b| {
        b.iter(|| {
            for _ in 0..20 {
                black_box(SystemPrompt::build_for_task());
            }
        })
    });

    group.finish();
}

/// 基准测试: SystemPrompt::build_brief
fn bench_build_brief(c: &mut Criterion) {
    let mut group = c.benchmark_group("build_brief");

    group.throughput(Throughput::Elements(1));
    group.bench_function("brief_prompt", |b| {
        b.iter(|| black_box(SystemPrompt::build_brief()))
    });

    // 重复调用 (模拟上下文衔接场景)
    group.throughput(Throughput::Elements(10));
    group.bench_function("x10_handoffs", |b| {
        b.iter(|| {
            for _ in 0..10 {
                black_box(SystemPrompt::build_brief());
            }
        })
    });

    group.finish();
}

/// 边界条件基准测试
fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_cases");

    // 确定性验证: 两次调用应产生相同结果
    group.bench_function("determinism_check", |b| {
        b.iter(|| {
            let p1 = SystemPrompt::build();
            let p2 = SystemPrompt::build();
            black_box(p1 == p2)
        })
    });

    // brief 比 build 更短
    group.bench_function("brief_shorter_than_build", |b| {
        b.iter(|| {
            let full = SystemPrompt::build();
            let brief = SystemPrompt::build_brief();
            black_box(brief.len() < full.len())
        })
    });

    // 所有方法返回非空
    group.bench_function("non_empty_all_methods", |b| {
        b.iter(|| {
            let build = SystemPrompt::build();
            let planning = SystemPrompt::build_for_planning();
            let task = SystemPrompt::build_for_task();
            let brief = SystemPrompt::build_brief();
            black_box(
                !build.is_empty() && !planning.is_empty() && !task.is_empty() && !brief.is_empty(),
            )
        })
    });

    // 验证 build == build_for_planning == build_for_task (当前实现相同)
    group.bench_function("build_equals_planning", |b| {
        b.iter(|| {
            let build = SystemPrompt::build();
            let planning = SystemPrompt::build_for_planning();
            black_box(build == planning)
        })
    });

    // 验证 build_contains_key_terms
    group.bench_function("contains_key_terms", |b| {
        b.iter(|| {
            let prompt = SystemPrompt::build();
            black_box(prompt.contains("FORGE") && prompt.contains("铁律") && prompt.contains("TDD"))
        })
    });

    // build_attachment_reference 直接调用
    group.bench_function("attachment_reference", |b| {
        b.iter(|| black_box(SystemPrompt::build_attachment_reference()))
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
        .output_directory(std::path::Path::new("target/criterion/prompt_builder"))
}

criterion_group! {
    name = prompt_builder_benches;
    config = configure_criterion();
    targets =
        bench_build,
        bench_build_for_planning,
        bench_build_for_task,
        bench_build_brief,
        bench_edge_cases,
}

criterion_main!(prompt_builder_benches);
