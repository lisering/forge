#![allow(clippy::useless_vec)]

//! traits 性能基准测试
//!
//! 测试目标:
//! 1. chat_result - ChatResult 构造和字段访问
//! 2. clarification - ClarificationResult (no/yes) + ClarificationContext (can_ask_more)
//! 3. language - Language enum (display_name/Display/all variants)
//! 4. task_action - TaskAction enum (构造/比较/Debug)
//! 5. edge_cases - 边界条件 (空/极值/批量)

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use forge::traits::{
    ChatResult, ClarificationContext, ClarificationResult, FixContext, Language, PhaseInfo,
    PlanInfo, TaskAction, TaskInfo,
};

// ============================================================================
//  基准测试 1: chat_result
// ============================================================================

fn bench_chat_result(c: &mut Criterion) {
    let mut group = c.benchmark_group("chat_result");

    // 构造
    group.bench_function("construct", |b| {
        b.iter(|| {
            let result = ChatResult {
                text: "AI response text".to_string(),
                timed_out: false,
            };
            black_box(result);
        })
    });

    // 大文本
    let large_text = "x".repeat(10_000);
    group.bench_function("construct_large_10k", |b| {
        b.iter(|| {
            let result = ChatResult {
                text: black_box(&large_text).clone(),
                timed_out: true,
            };
            black_box(result);
        })
    });

    // clone
    let result = ChatResult {
        text: "Some response".to_string(),
        timed_out: false,
    };
    group.bench_function("clone", |b| {
        b.iter(|| {
            let cloned = black_box(&result).clone();
            black_box(cloned);
        })
    });

    // 批量构造 100
    group.bench_function("batch_100", |b| {
        b.iter(|| {
            let results: Vec<ChatResult> = (0..100)
                .map(|i| ChatResult {
                    text: format!("Response {}", i),
                    timed_out: i % 10 == 0,
                })
                .collect();
            black_box(results);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 2: clarification
// ============================================================================

fn bench_clarification(c: &mut Criterion) {
    let mut group = c.benchmark_group("clarification");

    // ClarificationResult::no
    group.bench_function("result_no", |b| {
        b.iter(|| {
            let result = ClarificationResult::no();
            black_box(result);
        })
    });

    // ClarificationResult::yes
    group.bench_function("result_yes", |b| {
        b.iter(|| {
            let result = ClarificationResult::yes("What do you mean?", "Ambiguous response");
            black_box(result);
        })
    });

    // ClarificationResult::yes 长文本
    let long_question = "x".repeat(1000);
    let long_reason = "y".repeat(500);
    group.bench_function("result_yes_long", |b| {
        b.iter(|| {
            let result =
                ClarificationResult::yes(black_box(&long_question), black_box(&long_reason));
            black_box(result);
        })
    });

    // ClarificationContext::can_ask_more
    let ctx = ClarificationContext {
        task_prompt: "Build a calculator".to_string(),
        timed_out: false,
        questions_asked: 2,
        max_questions: 5,
        previous_questions: vec!["What language?".to_string()],
    };
    group.bench_function("can_ask_more_true", |b| {
        b.iter(|| {
            let result = black_box(&ctx).can_ask_more();
            black_box(result);
        })
    });

    let ctx_maxed = ClarificationContext {
        task_prompt: "Build a calculator".to_string(),
        timed_out: false,
        questions_asked: 5,
        max_questions: 5,
        previous_questions: vec![],
    };
    group.bench_function("can_ask_more_false", |b| {
        b.iter(|| {
            let result = black_box(&ctx_maxed).can_ask_more();
            black_box(result);
        })
    });

    // ClarificationContext clone
    group.bench_function("context_clone", |b| {
        b.iter(|| {
            let cloned = black_box(&ctx).clone();
            black_box(cloned);
        })
    });

    // 批量 no/yes 交替
    group.bench_function("batch_100", |b| {
        b.iter(|| {
            let results: Vec<ClarificationResult> = (0..100)
                .map(|i| {
                    if i % 2 == 0 {
                        ClarificationResult::no()
                    } else {
                        ClarificationResult::yes("Why?", "Unclear")
                    }
                })
                .collect();
            black_box(results);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 3: language
// ============================================================================

fn bench_language(c: &mut Criterion) {
    let mut group = c.benchmark_group("language");

    let languages = [
        Language::Rust,
        Language::Python,
        Language::Go,
        Language::Node,
        Language::Unknown,
    ];

    // display_name
    group.bench_function("display_name_all", |b| {
        b.iter(|| {
            for lang in black_box(&languages) {
                let _ = lang.display_name();
            }
        })
    });

    // Display trait
    group.bench_function("display_all", |b| {
        b.iter(|| {
            for lang in black_box(&languages) {
                let _ = format!("{}", lang);
            }
        })
    });

    // Copy/Clone
    group.bench_function("copy_all", |b| {
        b.iter(|| {
            let copies: Vec<Language> = black_box(&languages).to_vec();
            black_box(copies);
        })
    });

    // PartialEq
    group.bench_function("eq_compare", |b| {
        b.iter(|| {
            let result = black_box(Language::Rust) == black_box(Language::Rust);
            black_box(result);
        })
    });

    // Debug format
    group.bench_function("debug_format_all", |b| {
        b.iter(|| {
            for lang in black_box(&languages) {
                let _ = format!("{:?}", lang);
            }
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 4: task_action
// ============================================================================

fn bench_task_action(c: &mut Criterion) {
    let mut group = c.benchmark_group("task_action");

    let actions = [TaskAction::Execute, TaskAction::Skip, TaskAction::Abort];

    // 构造
    group.bench_function("construct_all", |b| {
        b.iter(|| {
            let execute = TaskAction::Execute;
            let skip = TaskAction::Skip;
            let abort = TaskAction::Abort;
            black_box((execute, skip, abort));
        })
    });

    // PartialEq
    group.bench_function("eq_compare", |b| {
        b.iter(|| {
            for a in black_box(&actions) {
                for b2 in black_box(&actions) {
                    let _ = a == b2;
                }
            }
        })
    });

    // Clone
    group.bench_function("clone_all", |b| {
        b.iter(|| {
            let cloned: Vec<TaskAction> = black_box(&actions).to_vec();
            black_box(cloned);
        })
    });

    // Debug
    group.bench_function("debug_format", |b| {
        b.iter(|| {
            for a in black_box(&actions) {
                let _ = format!("{:?}", a);
            }
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 5: edge_cases
// ============================================================================

fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_cases");

    // PlanInfo 构造
    group.bench_function("plan_info_construct", |b| {
        b.iter(|| {
            let plan = PlanInfo {
                goal: "Build a Rust CLI calculator".to_string(),
                phases: vec![PhaseInfo {
                    name: "Phase 1".to_string(),
                    description: "Basic operations".to_string(),
                    tasks: vec![TaskInfo {
                        id: "0-0".to_string(),
                        name: "Create main.rs".to_string(),
                        prompt: "Create a main.rs file with arg parsing".to_string(),
                    }],
                }],
            };
            black_box(plan);
        })
    });

    // PlanInfo clone
    let plan = PlanInfo {
        goal: "Test goal".to_string(),
        phases: vec![PhaseInfo {
            name: "P1".to_string(),
            description: "Desc".to_string(),
            tasks: vec![TaskInfo {
                id: "0-0".to_string(),
                name: "Task".to_string(),
                prompt: "Do something".to_string(),
            }],
        }],
    };
    group.bench_function("plan_info_clone", |b| {
        b.iter(|| {
            let cloned = black_box(&plan).clone();
            black_box(cloned);
        })
    });

    // FixContext 构造
    group.bench_function("fix_context_construct", |b| {
        b.iter(|| {
            let ctx = FixContext {
                phase_idx: 0,
                task_idx: 1,
                attempt: 2,
                max_attempts: 5,
                feedback: "error: expected type, found string literal".to_string(),
            };
            black_box(ctx);
        })
    });

    // FixContext clone
    let fix_ctx = FixContext {
        phase_idx: 0,
        task_idx: 0,
        attempt: 1,
        max_attempts: 3,
        feedback: "Compilation failed".to_string(),
    };
    group.bench_function("fix_context_clone", |b| {
        b.iter(|| {
            let cloned = black_box(&fix_ctx).clone();
            black_box(cloned);
        })
    });

    // 大 PlanInfo (50 phases × 10 tasks)
    group.bench_function("plan_info_large_50x10", |b| {
        b.iter(|| {
            let phases: Vec<PhaseInfo> = (0..50)
                .map(|p| PhaseInfo {
                    name: format!("Phase {}", p),
                    description: format!("Description {}", p),
                    tasks: (0..10)
                        .map(|t| TaskInfo {
                            id: format!("{}-{}", p, t),
                            name: format!("Task {}", t),
                            prompt: format!("Prompt for task {}-{}", p, t),
                        })
                        .collect(),
                })
                .collect();
            let plan = PlanInfo {
                goal: "Large project".to_string(),
                phases,
            };
            black_box(plan);
        })
    });

    // ClarificationContext 边界
    group.bench_function("ctx_empty_questions", |b| {
        b.iter(|| {
            let ctx = ClarificationContext {
                task_prompt: String::new(),
                timed_out: false,
                questions_asked: 0,
                max_questions: 0,
                previous_questions: vec![],
            };
            let _ = ctx.can_ask_more();
            black_box(ctx);
        })
    });

    group.bench_function("ctx_max_questions", |b| {
        b.iter(|| {
            let ctx = ClarificationContext {
                task_prompt: "Task".to_string(),
                timed_out: true,
                questions_asked: u32::MAX,
                max_questions: u32::MAX,
                previous_questions: vec!["q1".to_string(); 100],
            };
            let _ = ctx.can_ask_more();
            black_box(ctx);
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
        .output_directory(std::path::Path::new("target/criterion/traits"))
}

criterion_group! {
    name = traits_benches;
    config = configure_criterion();
    targets = bench_chat_result,
        bench_clarification,
        bench_language,
        bench_task_action,
        bench_edge_cases,
}

criterion_main!(traits_benches);
