#![allow(clippy::useless_vec)]

//! interaction 性能基准测试
//!
//! 测试目标:
//! 1. auto_approve - AutoApprove 实现 (tokio runtime async)
//! 2. mock_interaction_build - MockInteraction 构建器链
//! 3. mock_interaction_responses - MockInteraction 各种响应 (tokio runtime async)
//! 4. mock_task_responses_queue - 任务响应队列 (tokio runtime async)
//! 5. edge_cases - 边界场景 (CliInteraction创建/默认值/调用计数)

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use forge::interaction::{AutoApprove, CliInteraction, MockInteraction};
use forge::traits::{FixContext, HumanInteraction, PlanInfo, TaskAction, TaskInfo};

// ============================================================================
//  辅助函数
// ============================================================================

fn make_plan() -> PlanInfo {
    PlanInfo {
        goal: "构建 CLI 工具".to_string(),
        phases: vec![],
    }
}

fn make_task() -> TaskInfo {
    TaskInfo {
        id: "0-0".to_string(),
        name: "测试任务".to_string(),
        prompt: "执行测试".to_string(),
    }
}

fn make_fix_context() -> FixContext {
    FixContext {
        phase_idx: 0,
        task_idx: 0,
        attempt: 2,
        max_attempts: 3,
        feedback: "编译错误".to_string(),
    }
}

// ============================================================================
//  基准测试 1: auto_approve
// ============================================================================

fn bench_auto_approve(c: &mut Criterion) {
    let mut group = c.benchmark_group("auto_approve");
    let rt = tokio::runtime::Runtime::new().unwrap();

    let auto = AutoApprove;
    let plan = make_plan();
    let task = make_task();
    let fix = make_fix_context();

    group.bench_function("confirm_planning", |b| {
        b.iter(|| {
            let result = rt
                .block_on(auto.confirm_planning(black_box(&plan)))
                .unwrap();
            black_box(result);
        })
    });

    group.bench_function("confirm_task", |b| {
        b.iter(|| {
            let result = rt.block_on(auto.confirm_task(black_box(&task))).unwrap();
            black_box(result);
        })
    });

    group.bench_function("confirm_fix", |b| {
        b.iter(|| {
            let result = rt.block_on(auto.confirm_fix(black_box(&fix))).unwrap();
            black_box(result);
        })
    });

    group.bench_function("confirm_requirement_change", |b| {
        b.iter(|| {
            let result = rt
                .block_on(auto.confirm_requirement_change(black_box("变更1")))
                .unwrap();
            black_box(result);
        })
    });

    // 全部 4 个方法一次调用
    group.bench_function("all_methods", |b| {
        b.iter(|| {
            let _ = rt.block_on(auto.confirm_planning(&plan));
            let _ = rt.block_on(auto.confirm_task(&task));
            let _ = rt.block_on(auto.confirm_fix(&fix));
            let _ = rt.block_on(auto.confirm_requirement_change("变更"));
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 2: mock_interaction_build
// ============================================================================

fn bench_mock_interaction_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("mock_interaction_build");

    // new
    group.bench_function("new", |b| {
        b.iter(|| {
            let mock = MockInteraction::new();
            black_box(mock);
        })
    });

    // default
    group.bench_function("default", |b| {
        b.iter(|| {
            let mock = MockInteraction::default();
            black_box(mock);
        })
    });

    // with_plan_response
    group.bench_function("with_plan_response", |b| {
        b.iter(|| {
            let mock = MockInteraction::new().with_plan_response(black_box(false));
            black_box(mock);
        })
    });

    // with_task_response
    group.bench_function("with_task_response", |b| {
        b.iter(|| {
            let mock = MockInteraction::new().with_task_response(black_box(TaskAction::Skip));
            black_box(mock);
        })
    });

    // 完整 builder 链
    group.bench_function("full_builder_chain", |b| {
        b.iter(|| {
            let mock = MockInteraction::new()
                .with_plan_response(false)
                .with_task_response(TaskAction::Skip)
                .with_fix_response(false)
                .with_change_response(false);
            black_box(mock);
        })
    });

    // with_task_responses (序列)
    let responses = vec![TaskAction::Execute, TaskAction::Skip, TaskAction::Abort];
    group.bench_function("with_task_responses_3", |b| {
        b.iter(|| {
            let mock = MockInteraction::new().with_task_responses(black_box(responses.clone()));
            black_box(mock);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 3: mock_interaction_responses
// ============================================================================

fn bench_mock_interaction_responses(c: &mut Criterion) {
    let mut group = c.benchmark_group("mock_interaction_responses");
    let rt = tokio::runtime::Runtime::new().unwrap();

    // confirm_planning true
    let mock_true = MockInteraction::new();
    let plan = make_plan();
    group.bench_function("confirm_planning_true", |b| {
        b.iter(|| {
            let result = rt
                .block_on(mock_true.confirm_planning(black_box(&plan)))
                .unwrap();
            black_box(result);
        })
    });

    // confirm_planning false
    let mock_false = MockInteraction::new().with_plan_response(false);
    group.bench_function("confirm_planning_false", |b| {
        b.iter(|| {
            let result = rt
                .block_on(mock_false.confirm_planning(black_box(&plan)))
                .unwrap();
            black_box(result);
        })
    });

    // confirm_task Execute
    let mock_exec = MockInteraction::new();
    let task = make_task();
    group.bench_function("confirm_task_execute", |b| {
        b.iter(|| {
            let result = rt
                .block_on(mock_exec.confirm_task(black_box(&task)))
                .unwrap();
            black_box(result);
        })
    });

    // confirm_task Skip
    let mock_skip = MockInteraction::new().with_task_response(TaskAction::Skip);
    group.bench_function("confirm_task_skip", |b| {
        b.iter(|| {
            let result = rt
                .block_on(mock_skip.confirm_task(black_box(&task)))
                .unwrap();
            black_box(result);
        })
    });

    // confirm_fix
    let mock_fix = MockInteraction::new();
    let fix = make_fix_context();
    group.bench_function("confirm_fix", |b| {
        b.iter(|| {
            let result = rt.block_on(mock_fix.confirm_fix(black_box(&fix))).unwrap();
            black_box(result);
        })
    });

    // confirm_requirement_change
    let mock_change = MockInteraction::new();
    group.bench_function("confirm_change", |b| {
        b.iter(|| {
            let result = rt
                .block_on(mock_change.confirm_requirement_change(black_box("变更内容")))
                .unwrap();
            black_box(result);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 4: mock_task_responses_queue
// ============================================================================

fn bench_mock_task_responses_queue(c: &mut Criterion) {
    let mut group = c.benchmark_group("mock_task_responses_queue");
    let rt = tokio::runtime::Runtime::new().unwrap();

    let task = make_task();

    // 3 项序列
    group.bench_function("sequence_3", |b| {
        b.iter(|| {
            let mock = MockInteraction::new().with_task_responses(vec![
                TaskAction::Execute,
                TaskAction::Skip,
                TaskAction::Abort,
            ]);
            let _ = rt.block_on(mock.confirm_task(&task));
            let _ = rt.block_on(mock.confirm_task(&task));
            let _ = rt.block_on(mock.confirm_task(&task));
        })
    });

    // 10 项序列
    group.bench_function("sequence_10", |b| {
        b.iter(|| {
            let mock = MockInteraction::new().with_task_responses(
                (0..10)
                    .map(|i| {
                        if i % 2 == 0 {
                            TaskAction::Execute
                        } else {
                            TaskAction::Skip
                        }
                    })
                    .collect(),
            );
            for _ in 0..10 {
                let _ = rt.block_on(mock.confirm_task(&task));
            }
        })
    });

    // 队列空后回退默认值
    group.bench_function("queue_empty_fallback", |b| {
        b.iter(|| {
            let mock = MockInteraction::new()
                .with_task_responses(vec![TaskAction::Skip])
                .with_task_response(TaskAction::Execute);
            let _ = rt.block_on(mock.confirm_task(&task)); // Skip (队列)
            let _ = rt.block_on(mock.confirm_task(&task)); // Execute (默认)
            let _ = rt.block_on(mock.confirm_task(&task)); // Execute (默认)
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 5: edge_cases
// ============================================================================

fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("interaction_edge_cases");
    let rt = tokio::runtime::Runtime::new().unwrap();

    // CliInteraction::new
    group.bench_function("cli_new", |b| {
        b.iter(|| {
            let cli = CliInteraction::new();
            black_box(cli);
        })
    });

    // CliInteraction::default
    group.bench_function("cli_default", |b| {
        b.iter(|| {
            let cli = CliInteraction;
            black_box(cli);
        })
    });

    // 调用计数验证
    let task = make_task();
    group.bench_function("call_count_tracking", |b| {
        b.iter(|| {
            // 每次迭代创建新 mock 避免计数累积
            let mock = MockInteraction::new();
            let _ = rt.block_on(mock.confirm_task(&task));
            let count = mock
                .call_counts
                .confirm_task
                .load(std::sync::atomic::Ordering::Relaxed);
            black_box(count);
        })
    });

    // trait object (Box<dyn HumanInteraction>)
    group.bench_function("trait_object_auto", |b| {
        b.iter(|| {
            let auto: Box<dyn HumanInteraction> = Box::new(AutoApprove);
            black_box(auto);
        })
    });

    // trait_object mock
    group.bench_function("trait_object_mock", |b| {
        b.iter(|| {
            let mock: Box<dyn HumanInteraction> = Box::new(MockInteraction::new());
            black_box(mock);
        })
    });

    // 多次 confirm_fix
    let mock_fix = MockInteraction::new().with_fix_response(true);
    let fix = make_fix_context();
    group.bench_function("confirm_fix_5_times", |b| {
        b.iter(|| {
            for _ in 0..5 {
                let _ = rt.block_on(mock_fix.confirm_fix(&fix));
            }
        })
    });

    group.finish();
}

// ============================================================================
//  配置 & 入口
// ============================================================================

fn configure_criterion() -> Criterion {
    Criterion::default()
        .sample_size(30)
        .measurement_time(std::time::Duration::from_secs(5))
        .warm_up_time(std::time::Duration::from_secs(2))
        .nresamples(50_000)
        .noise_threshold(0.05)
        .output_directory(std::path::Path::new("target/criterion/interaction"))
}

criterion_group! {
    name = interaction_benches;
    config = configure_criterion();
    targets = bench_auto_approve,
        bench_mock_interaction_build,
        bench_mock_interaction_responses,
        bench_mock_task_responses_queue,
        bench_edge_cases,
}

criterion_main!(interaction_benches);
