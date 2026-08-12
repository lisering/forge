#![allow(clippy::useless_vec)]

//! Joint Decision 模块性能基准测试
//!
//! 测试目标:
//! 1. count_disabled_evaluators - 统计已禁用评估器数量
//! 2. should_enter_conservative_mode - 保守模式判断
//! 3. should_escalate_warning - 升级警告判断
//! 4. compute_joint_decision - 联合决策核心计算
//! 5. edge_cases - 边界条件 (select_re_enable_candidate/各种组合)

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use forge::evaluator_synergy::{EvaluatorSnapshot, EvaluatorType};
use forge::joint_decision::{
    compute_joint_decision, count_disabled_evaluators, should_enter_conservative_mode,
    should_escalate_warning, JointDecisionConfig,
};

/// 创建评估器快照
fn make_snapshot(
    evaluator_type: EvaluatorType,
    enabled: bool,
    diff: f64,
    total_checks: usize,
) -> EvaluatorSnapshot {
    EvaluatorSnapshot {
        evaluator_type,
        enabled,
        with_fix_rate: if enabled { 0.8 } else { 0.3 },
        without_fix_rate: 0.6,
        diff,
        is_beneficial: diff > 0.0,
        total_checks,
        evaluation_count: 3,
        disable_count: if enabled { 0 } else { 1 },
        contribution_score: diff,
    }
}

/// 全部启用的快照 (3 个评估器)
fn all_enabled_snapshots() -> Vec<EvaluatorSnapshot> {
    vec![
        make_snapshot(EvaluatorType::CacheTuner, true, 0.2, 10),
        make_snapshot(EvaluatorType::SearchQuality, true, 0.15, 10),
        make_snapshot(EvaluatorType::MemoryContext, true, 0.1, 10),
    ]
}

/// 部分禁用的快照 (1 个禁用)
fn one_disabled_snapshots() -> Vec<EvaluatorSnapshot> {
    vec![
        make_snapshot(EvaluatorType::CacheTuner, false, -0.4, 10),
        make_snapshot(EvaluatorType::SearchQuality, true, 0.15, 10),
        make_snapshot(EvaluatorType::MemoryContext, true, 0.1, 10),
    ]
}

/// 部分禁用的快照 (2 个禁用)
fn two_disabled_snapshots() -> Vec<EvaluatorSnapshot> {
    vec![
        make_snapshot(EvaluatorType::CacheTuner, false, -0.4, 10),
        make_snapshot(EvaluatorType::SearchQuality, false, -0.3, 10),
        make_snapshot(EvaluatorType::MemoryContext, true, 0.1, 10),
    ]
}

/// 全部禁用的快照 (3 个禁用)
fn all_disabled_snapshots() -> Vec<EvaluatorSnapshot> {
    vec![
        make_snapshot(EvaluatorType::CacheTuner, false, -0.4, 10),
        make_snapshot(EvaluatorType::SearchQuality, false, -0.3, 10),
        make_snapshot(EvaluatorType::MemoryContext, false, -0.2, 10),
    ]
}

/// 基准测试: count_disabled_evaluators
fn bench_count_disabled_evaluators(c: &mut Criterion) {
    let mut group = c.benchmark_group("count_disabled_evaluators");

    let all_enabled = all_enabled_snapshots();
    let one_disabled = one_disabled_snapshots();
    let two_disabled = two_disabled_snapshots();
    let all_disabled = all_disabled_snapshots();

    group.throughput(Throughput::Elements(3));
    group.bench_function("all_enabled", |b| {
        b.iter(|| black_box(count_disabled_evaluators(black_box(&all_enabled))))
    });
    group.bench_function("one_disabled", |b| {
        b.iter(|| black_box(count_disabled_evaluators(black_box(&one_disabled))))
    });
    group.bench_function("two_disabled", |b| {
        b.iter(|| black_box(count_disabled_evaluators(black_box(&two_disabled))))
    });
    group.bench_function("all_disabled", |b| {
        b.iter(|| black_box(count_disabled_evaluators(black_box(&all_disabled))))
    });

    // 空列表
    let empty: Vec<EvaluatorSnapshot> = vec![];
    group.bench_function("empty", |b| {
        b.iter(|| black_box(count_disabled_evaluators(black_box(&empty))))
    });

    group.finish();
}

/// 基准测试: should_enter_conservative_mode
fn bench_should_enter_conservative_mode(c: &mut Criterion) {
    let mut group = c.benchmark_group("should_enter_conservative_mode");

    // 各种 (disabled, total, threshold) 组合
    group.bench_function("no_disabled", |b| {
        b.iter(|| {
            black_box(should_enter_conservative_mode(
                black_box(0),
                black_box(3),
                black_box(3),
            ))
        })
    });
    group.bench_function("below_threshold", |b| {
        b.iter(|| {
            black_box(should_enter_conservative_mode(
                black_box(2),
                black_box(3),
                black_box(3),
            ))
        })
    });
    group.bench_function("at_threshold", |b| {
        b.iter(|| {
            black_box(should_enter_conservative_mode(
                black_box(3),
                black_box(3),
                black_box(3),
            ))
        })
    });
    group.bench_function("above_threshold", |b| {
        b.iter(|| {
            black_box(should_enter_conservative_mode(
                black_box(5),
                black_box(3),
                black_box(3),
            ))
        })
    });
    group.bench_function("total_zero", |b| {
        b.iter(|| {
            black_box(should_enter_conservative_mode(
                black_box(0),
                black_box(0),
                black_box(3),
            ))
        })
    });

    group.finish();
}

/// 基准测试: should_escalate_warning
fn bench_should_escalate_warning(c: &mut Criterion) {
    let mut group = c.benchmark_group("should_escalate_warning");

    // 各种 (disabled, total, escalate, conservative) 组合
    group.bench_function("no_escalation", |b| {
        b.iter(|| {
            black_box(should_escalate_warning(
                black_box(1),
                black_box(3),
                black_box(2),
                black_box(3),
            ))
        })
    });
    group.bench_function("escalate", |b| {
        b.iter(|| {
            black_box(should_escalate_warning(
                black_box(2),
                black_box(3),
                black_box(2),
                black_box(3),
            ))
        })
    });
    group.bench_function("conservative_not_escalate", |b| {
        b.iter(|| {
            black_box(should_escalate_warning(
                black_box(3),
                black_box(3),
                black_box(2),
                black_box(3),
            ))
        })
    });
    group.bench_function("total_zero", |b| {
        b.iter(|| {
            black_box(should_escalate_warning(
                black_box(0),
                black_box(0),
                black_box(2),
                black_box(3),
            ))
        })
    });

    group.finish();
}

/// 基准测试: compute_joint_decision
fn bench_compute_joint_decision(c: &mut Criterion) {
    let mut group = c.benchmark_group("compute_joint_decision");

    let config = JointDecisionConfig::default();
    let all_enabled = all_enabled_snapshots();
    let one_disabled = one_disabled_snapshots();
    let two_disabled = two_disabled_snapshots();
    let all_disabled = all_disabled_snapshots();

    group.throughput(Throughput::Elements(3));
    group.bench_function("all_enabled_noaction", |b| {
        b.iter(|| {
            black_box(compute_joint_decision(
                black_box(&all_enabled),
                black_box(&config),
            ))
        })
    });
    group.bench_function("one_disabled_noaction", |b| {
        b.iter(|| {
            black_box(compute_joint_decision(
                black_box(&one_disabled),
                black_box(&config),
            ))
        })
    });
    group.bench_function("two_disabled_escalate", |b| {
        b.iter(|| {
            black_box(compute_joint_decision(
                black_box(&two_disabled),
                black_box(&config),
            ))
        })
    });
    group.bench_function("all_disabled_conservative", |b| {
        b.iter(|| {
            black_box(compute_joint_decision(
                black_box(&all_disabled),
                black_box(&config),
            ))
        })
    });

    // 空列表
    let empty: Vec<EvaluatorSnapshot> = vec![];
    group.bench_function("empty_snapshots", |b| {
        b.iter(|| {
            black_box(compute_joint_decision(
                black_box(&empty),
                black_box(&config),
            ))
        })
    });

    group.finish();
}

/// 边界条件基准测试
fn bench_edge_cases(c: &mut Criterion) {
    use forge::joint_decision::select_re_enable_candidate;

    let mut group = c.benchmark_group("edge_cases");

    let all_disabled = all_disabled_snapshots();
    let two_disabled = two_disabled_snapshots();
    let all_enabled = all_enabled_snapshots();

    // select_re_enable_candidate
    group.bench_function("re_enable_all_disabled", |b| {
        b.iter(|| black_box(select_re_enable_candidate(black_box(&all_disabled))))
    });
    group.bench_function("re_enable_two_disabled", |b| {
        b.iter(|| black_box(select_re_enable_candidate(black_box(&two_disabled))))
    });
    group.bench_function("re_enable_none_disabled", |b| {
        b.iter(|| black_box(select_re_enable_candidate(black_box(&all_enabled))))
    });

    // 联合决策全组合 (4 种场景 × count + compute)
    group.throughput(Throughput::Elements(4));
    group.bench_function("count_all_scenarios", |b| {
        b.iter(|| {
            let c1 = count_disabled_evaluators(&all_enabled);
            let c2 = count_disabled_evaluators(&one_disabled_snapshots());
            let c3 = count_disabled_evaluators(&two_disabled);
            let c4 = count_disabled_evaluators(&all_disabled);
            black_box((c1, c2, c3, c4))
        })
    });

    // 大规模快照列表 (10 个评估器)
    let large_snapshots: Vec<EvaluatorSnapshot> = (0..10)
        .map(|i| {
            let etype = match i % 3 {
                0 => EvaluatorType::CacheTuner,
                1 => EvaluatorType::SearchQuality,
                _ => EvaluatorType::MemoryContext,
            };
            make_snapshot(etype, i % 2 == 0, if i % 2 == 0 { 0.1 } else { -0.2 }, 10)
        })
        .collect();
    group.throughput(Throughput::Elements(large_snapshots.len() as u64));
    group.bench_function("large_10_evaluators", |b| {
        b.iter(|| {
            black_box(compute_joint_decision(
                black_box(&large_snapshots),
                black_box(&JointDecisionConfig::default()),
            ))
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
        .output_directory(std::path::Path::new("target/criterion/joint_decision"))
}

criterion_group! {
    name = joint_decision_benches;
    config = configure_criterion();
    targets =
        bench_count_disabled_evaluators,
        bench_should_enter_conservative_mode,
        bench_should_escalate_warning,
        bench_compute_joint_decision,
        bench_edge_cases,
}

criterion_main!(joint_decision_benches);
