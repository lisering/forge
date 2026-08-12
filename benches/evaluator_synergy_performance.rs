#![allow(clippy::useless_vec)]

//! evaluator_synergy 性能基准测试
//!
//! 测试目标:
//! 1. parse_evaluator_timeline_action - 解析 DevTrace 条目为时间线动作
//! 2. build_evaluator_timeline - 从 DevTrace 条目列表构建时间线
//! 3. compute_synergy_score - 计算协同评分
//! 4. build_evaluator_synergy_summary - 构建三评估器协同分析摘要
//! 5. edge_cases - 边界场景 (空数据/大规模/历史摘要/趋势计算)

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use forge::dev_trace::{DevTraceEntry, TraceAction};
use forge::evaluator_synergy::*;

// ============================================================================
//  辅助函数
// ============================================================================

fn make_cache_tuning_entry(output: &str) -> DevTraceEntry {
    DevTraceEntry::new(
        TraceAction::CacheTuning,
        Some(0),
        Some(0),
        Some("task1"),
        "hit=2/3",
        output,
        50,
        true,
        None,
    )
}

fn make_search_quality_entry(output: &str) -> DevTraceEntry {
    DevTraceEntry::new(
        TraceAction::SearchQuality,
        Some(0),
        Some(0),
        Some("task1"),
        "with=2/3",
        output,
        50,
        true,
        None,
    )
}

fn make_memory_evaluation_entry(output: &str) -> DevTraceEntry {
    DevTraceEntry::new(
        TraceAction::MemoryEvaluation,
        Some(0),
        Some(0),
        Some("task1"),
        "with=2/3",
        output,
        50,
        true,
        None,
    )
}

fn make_states(count: usize) -> Vec<EvaluatorState> {
    let types = [
        EvaluatorType::CacheTuner,
        EvaluatorType::SearchQuality,
        EvaluatorType::MemoryContext,
    ];
    (0..count)
        .map(|i| {
            let t = types[i % 3];
            EvaluatorState::new(t, true, 0.75, 0.55, 0.2, true, 20, 5, 0)
        })
        .collect()
}

fn make_entries(n: usize) -> Vec<DevTraceEntry> {
    (0..n)
        .map(|i| match i % 3 {
            0 => make_cache_tuning_entry("缓存调优: 保持当前配置 (差值 +10.0%, 原因: 有效)"),
            1 => make_search_quality_entry("搜索质量: 保持搜索 (差值 +5.0%, 原因: 有效)"),
            _ => make_memory_evaluation_entry("Memory: KeepInjecting (差值 +15.0%, 原因: 有效)"),
        })
        .collect()
}

// ============================================================================
//  基准测试 1: parse_evaluator_timeline_action
// ============================================================================

fn bench_parse_timeline_action(c: &mut Criterion) {
    let entries = vec![
        (
            "cache_keep",
            make_cache_tuning_entry("缓存调优: 保持当前配置 (差值 +10.0%, 原因: 有效)"),
        ),
        (
            "cache_disable",
            make_cache_tuning_entry("缓存调优: 禁用缓存 (差值 -20.0%, 原因: 有害)"),
        ),
        (
            "cache_adjust",
            make_cache_tuning_entry("缓存调优: 调整 TTL (差值 +30.0%, 原因: 有效)"),
        ),
        (
            "search_keep",
            make_search_quality_entry("搜索质量: 保持搜索 (差值 +5.0%, 原因: 有效)"),
        ),
        (
            "search_disable",
            make_search_quality_entry("搜索质量: 禁用搜索 (差值 -10.0%, 原因: 有害)"),
        ),
        (
            "memory_keep",
            make_memory_evaluation_entry("Memory: KeepInjecting (差值 +15.0%, 原因: 有效)"),
        ),
        (
            "memory_disable",
            make_memory_evaluation_entry("Memory: DisableInjection (差值 -5.0%, 原因: 有害)"),
        ),
    ];

    c.bench_function("parse_evaluator_timeline_action", |b| {
        b.iter(|| {
            for (_, entry) in black_box(&entries) {
                let _ = parse_evaluator_timeline_action(entry);
            }
        })
    });
}

// ============================================================================
//  基准测试 2: build_evaluator_timeline
// ============================================================================

fn bench_build_timeline(c: &mut Criterion) {
    let mut group = c.benchmark_group("build_evaluator_timeline");
    let sizes = [10, 100, 500];

    for size in sizes {
        let entries = make_entries(size);
        group.bench_with_input(BenchmarkId::from_parameter(size), &entries, |b, entries| {
            b.iter(|| {
                let timeline = build_evaluator_timeline(black_box(entries));
                black_box(timeline);
            })
        });
    }

    group.finish();
}

// ============================================================================
//  基准测试 3: compute_synergy_score
// ============================================================================

fn bench_compute_synergy_score(c: &mut Criterion) {
    let mut group = c.benchmark_group("compute_synergy_score");

    // 3 个评估器全启用, 全部有效
    let snapshots_all_beneficial: Vec<EvaluatorSnapshot> = make_states(3)
        .iter()
        .map(EvaluatorSnapshot::from_state)
        .collect();

    group.bench_function("all_beneficial_3_evaluators", |b| {
        b.iter(|| {
            let score = compute_synergy_score(black_box(&snapshots_all_beneficial), false, true);
            black_box(score);
        })
    });

    // 3 个评估器, 有禁用, 部分有害
    let mut states_mixed = make_states(3);
    states_mixed[1].enabled = false;
    states_mixed[1].disable_count = 1;
    states_mixed[1].is_beneficial = false;
    states_mixed[1].diff = -0.2;
    let snapshots_mixed: Vec<EvaluatorSnapshot> = states_mixed
        .iter()
        .map(EvaluatorSnapshot::from_state)
        .collect();

    group.bench_function("mixed_disabled_3_evaluators", |b| {
        b.iter(|| {
            let score = compute_synergy_score(black_box(&snapshots_mixed), true, false);
            black_box(score);
        })
    });

    // 10 个评估器 (大规模)
    let snapshots_large: Vec<EvaluatorSnapshot> = make_states(10)
        .iter()
        .map(EvaluatorSnapshot::from_state)
        .collect();

    group.bench_function("large_10_evaluators", |b| {
        b.iter(|| {
            let score = compute_synergy_score(black_box(&snapshots_large), false, true);
            black_box(score);
        })
    });

    // 空列表
    let empty: Vec<EvaluatorSnapshot> = vec![];
    group.bench_function("empty", |b| {
        b.iter(|| {
            let score = compute_synergy_score(black_box(&empty), false, true);
            black_box(score);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 4: build_evaluator_synergy_summary
// ============================================================================

fn bench_build_synergy_summary(c: &mut Criterion) {
    let mut group = c.benchmark_group("build_evaluator_synergy_summary");

    let states = make_states(3);
    let entries_small = make_entries(10);
    let entries_medium = make_entries(100);

    group.bench_function("3_evaluators_10_entries", |b| {
        b.iter(|| {
            let summary = build_evaluator_synergy_summary(
                black_box(&states),
                20,
                12,
                black_box(&entries_small),
            );
            black_box(summary);
        })
    });

    group.bench_function("3_evaluators_100_entries", |b| {
        b.iter(|| {
            let summary = build_evaluator_synergy_summary(
                black_box(&states),
                50,
                30,
                black_box(&entries_medium),
            );
            black_box(summary);
        })
    });

    // 有禁用的评估器
    let mut states_disabled = make_states(3);
    states_disabled[1].enabled = false;
    states_disabled[1].disable_count = 2;

    group.bench_function("with_disabled", |b| {
        b.iter(|| {
            let summary = build_evaluator_synergy_summary(
                black_box(&states_disabled),
                30,
                15,
                black_box(&entries_small),
            );
            black_box(summary);
        })
    });

    // 空状态 + 空条目
    let empty_states: Vec<EvaluatorState> = vec![];
    let empty_entries: Vec<DevTraceEntry> = vec![];

    group.bench_function("empty", |b| {
        b.iter(|| {
            let summary = build_evaluator_synergy_summary(
                black_box(&empty_states),
                0,
                0,
                black_box(&empty_entries),
            );
            black_box(summary);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 5: edge_cases
// ============================================================================

fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("evaluator_synergy_edge_cases");

    // EvaluatorSnapshot::from_state
    let state = EvaluatorState::new(
        EvaluatorType::CacheTuner,
        true,
        0.8,
        0.6,
        0.2,
        true,
        100,
        10,
        0,
    );
    group.bench_function("snapshot_from_state", |b| {
        b.iter(|| {
            let snap = EvaluatorSnapshot::from_state(black_box(&state));
            black_box(snap);
        })
    });

    // parse_diff_value 各种格式
    let diffs = vec![
        "差值 +10.0%",
        "差值 -5.0%",
        "差值 0.0%",
        "无差值",
        "差值 +100.0%",
    ];
    group.bench_function("parse_diff_value", |b| {
        b.iter(|| {
            for d in black_box(&diffs) {
                let _ = parse_diff_value(d);
            }
        })
    });

    // to_summary 格式化
    let summary = build_evaluator_synergy_summary(&make_states(3), 20, 12, &make_entries(10));
    group.bench_function("summary_to_summary", |b| {
        b.iter(|| {
            let s = black_box(&summary).to_summary();
            black_box(s);
        })
    });

    // synergy_summary_to_json
    group.bench_function("summary_to_json", |b| {
        b.iter(|| {
            let json = synergy_summary_to_json(black_box(&summary)).unwrap();
            black_box(json);
        })
    });

    // compute_synergy_trend
    let scores_improving = vec![0.3, 0.4, 0.5, 0.6, 0.7];
    let scores_declining = vec![0.7, 0.6, 0.5, 0.4, 0.3];
    let scores_stable = vec![0.5, 0.5, 0.5, 0.5];
    let scores_insufficient = vec![0.5];

    group.bench_function("compute_synergy_trend", |b| {
        b.iter(|| {
            let _ = compute_synergy_trend(black_box(&scores_improving));
            let _ = compute_synergy_trend(black_box(&scores_declining));
            let _ = compute_synergy_trend(black_box(&scores_stable));
            let _ = compute_synergy_trend(black_box(&scores_insufficient));
        })
    });

    // build_synergy_history_summary
    let mut history = EvaluatorSynergyHistory::new();
    for i in 0..10 {
        let entry = build_synergy_history_entry(&summary, i + 1, chrono::Utc::now());
        history.add_entry(entry);
    }
    group.bench_function("build_history_summary", |b| {
        b.iter(|| {
            let hs = build_synergy_history_summary(black_box(&history));
            black_box(hs);
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
        .output_directory(std::path::Path::new("target/criterion/evaluator_synergy"))
}

criterion_group! {
    name = evaluator_synergy_benches;
    config = configure_criterion();
    targets = bench_parse_timeline_action,
        bench_build_timeline,
        bench_compute_synergy_score,
        bench_build_synergy_summary,
        bench_edge_cases,
}

criterion_main!(evaluator_synergy_benches);
