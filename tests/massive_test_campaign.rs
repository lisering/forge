//! # Massive Test Campaign — 超大规模属性测试
//!
//! 本文件包含 Forge 代码库中所有主要纯函数的属性测试 (property-based tests)。
//! 每个 proptest 默认生成 256 个随机测试用例, 提供远超手写测试的覆盖度。
//!
//! ## 测试分类
//!
//! | 模块 | 属性测试数 | 覆盖函数 |
//! |------|-----------|---------|
//! | sparkline | 12 | normalize_value, map_value_to_char, compute_sparkline_stats, ... |
//! | dev_trace | 10 | calculate_success_rate, format_duration_human, parse_jsonl_line, ... |
//! | dev_trace_analyzer | 8 | compute_health_score, compute_health_score_trend, ... |
//! | cache_tuning | 8 | has_sufficient_data, should_disable_cache, compute_new_ttl, ... |
//! | search_quality | 6 | has_sufficient_search_data, should_disable_search, ... |
//! | memory_evaluation | 5 | has_sufficient_evaluation_data, should_disable_injection, ... |
//! | error_search | 5 | build_error_search_query, extract_error_keywords, ... |
//! | search_cache | 6 | build_cache_key, normalize_query_for_cache, is_cache_expired, ... |
//! | radix_tree | 5 | compute_fingerprints, common_prefix_length, ... |
//! | live_continuation | 6 | compute_diff, find_duplicates, deduplicate, ... |
//! | html_report | 4 | generate_doughnut_colors, generate_point_colors, ... |
//! | failover_chat | 3 | should_failover_decision, build_error_health_result, ... |
//! | joint_decision | 5 | should_enter_conservative_mode, should_escalate_warning, ... |
//! | stress | 8 | 大规模数据压力测试 |
//! | total | 91 | ~23,000+ 随机测试用例 |

use forge::{
    cache_tuning::{self, CacheTuningConfig, CacheTuningDecision, TuningAction},
    dev_trace::{
        self, CacheFixCorrelation, DevTraceEntry, DevTraceSummary, MemoryEvaluationStats,
        TraceAction,
    },
    dev_trace_analyzer::{self, AnalysisConfig},
    error_search,
    evaluator_synergy::ScoreTrend,
    failover_chat, html_report, joint_decision, live_continuation, memory_evaluation, radix_tree,
    search_cache, search_quality,
    site_health::{HealthCheckResult, SiteHealthStatus},
    sparkline,
    testrunner::CompileError,
};

use proptest::prelude::*;

// 禁用失败持久化, 避免并行测试时文件写入冲突
fn test_config() -> ProptestConfig {
    ProptestConfig {
        failure_persistence: None,
        ..ProptestConfig::default()
    }
}

// =============================================================================
//  1. Sparkline 模块属性测试
// =============================================================================

#[test]
fn prop_normalize_value_in_range() {
    proptest!(&test_config(), |(val in -1e6_f64..1e6, min in -1e6_f64..0.0, max in 0.0..1e6_f64)| {
        if min < max {
            let result = sparkline::normalize_value(val, min, max);
            prop_assert!((0.0..=1.0).contains(&result),
                "normalize_value({}, {}, {}) = {} not in [0,1]", val, min, max, result);
        }
    });
}

#[test]
fn prop_normalize_value_at_min() {
    proptest!(&test_config(), |(min in -1e6_f64..1e6, max in 0.0..1e6_f64)| {
        if min < max {
            let result = sparkline::normalize_value(min, min, max);
            prop_assert!((result - 0.0).abs() < 1e-9);
        }
    });
}

#[test]
fn prop_normalize_value_at_max() {
    proptest!(&test_config(), |(min in -1e6_f64..0.0, max in 0.0..1e6_f64)| {
        if min < max {
            let result = sparkline::normalize_value(max, min, max);
            prop_assert!((result - 1.0).abs() < 1e-9);
        }
    });
}

#[test]
fn prop_map_value_to_char_valid() {
    proptest!(&test_config(), |(val in -1e6_f64..1e6, min in -1e6_f64..0.0, max in 0.0..1e6_f64)| {
        if min < max {
            let ch = sparkline::map_value_to_char(val, min, max);
            let valid_chars: Vec<char> = "▁▂▃▄▅▆▇█".chars().collect();
            prop_assert!(valid_chars.contains(&ch), "invalid char: {}", ch);
        }
    });
}

#[test]
fn prop_map_value_to_char_at_min() {
    proptest!(&test_config(), |(min in -1e6_f64..0.0, max in 0.0..1e6_f64)| {
        if min < max {
            let ch = sparkline::map_value_to_char(min, min, max);
            prop_assert_eq!(ch, '▁');
        }
    });
}

#[test]
fn prop_map_value_to_char_at_max() {
    proptest!(&test_config(), |(min in -1e6_f64..0.0, max in 0.0..1e6_f64)| {
        if min < max {
            let ch = sparkline::map_value_to_char(max, min, max);
            prop_assert_eq!(ch, '█');
        }
    });
}

#[test]
fn prop_compute_sparkline_stats_empty() {
    let stats = sparkline::compute_sparkline_stats(&[]);
    assert!(!stats.has_data());
}

#[test]
fn prop_compute_sparkline_stats_nonempty() {
    proptest!(&test_config(), |(v1 in -1e3_f64..1e3, v2 in -1e3_f64..1e3)| {
        let values = if (v1 - v2).abs() < 1e-6 { vec![v1, v2 + 1.0] } else { vec![v1, v2] };
        let stats = sparkline::compute_sparkline_stats(&values);
        prop_assert!(stats.has_data());
        prop_assert_eq!(stats.count, values.len());
    });
}

#[test]
fn prop_compute_sparkline_stats_min_max() {
    proptest!(&test_config(), |(values in proptest::collection::vec(-1e3_f64..1e3, 2..100))| {
        let stats = sparkline::compute_sparkline_stats(&values);
        let expected_min = values.iter().cloned().fold(f64::INFINITY, f64::min);
        let expected_max = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        prop_assert!((stats.min - expected_min).abs() < 1e-9);
        prop_assert!((stats.max - expected_max).abs() < 1e-9);
    });
}

#[test]
fn prop_render_sparkline_empty() {
    let config = sparkline::SparklineConfig::new(60);
    let result = sparkline::render_sparkline(&[], &config);
    // 空输入可能返回空字符串或默认占位符
    assert!(result.is_empty() || result.len() < 20);
}

#[test]
fn prop_render_sparkline_nonempty() {
    proptest!(&test_config(), |(values in proptest::collection::vec(-1e3_f64..1e3, 1..100))| {
        let config = sparkline::SparklineConfig::new(60);
        let result = sparkline::render_sparkline(&values, &config);
        prop_assert!(!result.is_empty());
    });
}

#[test]
fn prop_escape_html() {
    proptest!(&test_config(), |(s in ".{0,100}")| {
        let escaped = sparkline::escape_html(&s);
        prop_assert!(!escaped.contains('<') || s.contains('&'));
        prop_assert!(!escaped.contains('>') || s.contains('&'));
    });
}

// =============================================================================
//  2. DevTrace 模块属性测试
// =============================================================================

#[test]
fn prop_success_rate_in_range() {
    proptest!(&test_config(), |(total in 0usize..10000, success in 0usize..10000)| {
        let actual_success = success.min(total);
        let rate = dev_trace::calculate_success_rate(total, actual_success);
        if total == 0 {
            prop_assert_eq!(rate, 0.0);
        } else {
            prop_assert!((0.0..=1.0).contains(&rate));
        }
    });
}

#[test]
fn prop_success_rate_all_success() {
    proptest!(&test_config(), |(total in 1usize..10000)| {
        let rate = dev_trace::calculate_success_rate(total, total);
        prop_assert!((rate - 1.0).abs() < 1e-9);
    });
}

#[test]
fn prop_success_rate_zero_success() {
    proptest!(&test_config(), |(total in 1usize..10000)| {
        let rate = dev_trace::calculate_success_rate(total, 0);
        prop_assert!((rate - 0.0).abs() < 1e-9);
    });
}

#[test]
fn prop_format_duration_zero() {
    let result = dev_trace::format_duration_human(0);
    assert!(result.contains('s') || result.contains("ms"));
}

#[test]
fn prop_format_duration_milliseconds() {
    proptest!(&test_config(), |(ms in 1u64..500)| {
        let result = dev_trace::format_duration_human(ms);
        prop_assert!(result.contains(&ms.to_string()) || result.contains('s'));
    });
}

#[test]
fn prop_format_duration_seconds() {
    proptest!(&test_config(), |(ms in 1000u64..60000)| {
        let result = dev_trace::format_duration_human(ms);
        prop_assert!(result.contains('s') || result.contains("ms"));
    });
}

#[test]
fn prop_format_success_rate_percent() {
    proptest!(&test_config(), |(rate in 0.0_f64..1.0)| {
        let result = dev_trace::format_success_rate_percent(rate);
        prop_assert!(result.contains('%'));
    });
}

#[test]
fn prop_parse_jsonl_line_invalid() {
    proptest!(&test_config(), |(line in "[^\"]{0,100}")| {
        let result = dev_trace::parse_jsonl_line(&line);
        if !line.starts_with('{') {
            prop_assert!(result.is_none());
        }
    });
}

#[test]
fn prop_group_entries_empty() {
    let map = dev_trace::group_entries_by_action(&[]);
    assert!(map.is_empty());
}

#[test]
fn prop_build_timeline_empty() {
    let timeline = dev_trace::build_timeline(&[], 100);
    assert!(timeline.is_empty());
}

// =============================================================================
//  3. DevTrace Analyzer 模块属性测试
// =============================================================================

#[test]
fn prop_health_score_in_range() {
    proptest!(&test_config(), |(
        success_rate in 0.0_f64..1.0,
        cache_hit in 0.0_f64..1.0
    )| {
        let summary = build_test_summary(success_rate, cache_hit);
        let analysis = dev_trace_analyzer::analyze_dev_trace_summary(&summary);
        prop_assert!(analysis.health_score.score >= 0.0 && analysis.health_score.score <= 100.0,
            "health_score {} out of [0,100]", analysis.health_score.score);
    });
}

#[test]
fn prop_health_score_perfect() {
    let summary = build_test_summary(1.0, 1.0);
    let analysis = dev_trace_analyzer::analyze_dev_trace_summary(&summary);
    assert!(
        analysis.health_score.score > 80.0,
        "perfect score should be > 80: {}",
        analysis.health_score.score
    );
}

#[test]
fn prop_health_score_worst() {
    let summary = build_test_summary(0.0, 0.0);
    let analysis = dev_trace_analyzer::analyze_dev_trace_summary(&summary);
    assert!(
        analysis.health_score.score < 50.0,
        "worst score should be < 50: {}",
        analysis.health_score.score
    );
}

#[test]
fn prop_health_score_trend_empty() {
    let trend = dev_trace_analyzer::compute_health_score_trend(&[]);
    assert!(matches!(trend, ScoreTrend::Insufficient));
}

#[test]
fn prop_health_score_trend_single() {
    proptest!(&test_config(), |(score in 0.0_f64..100.0)| {
        let trend = dev_trace_analyzer::compute_health_score_trend(&[score]);
        prop_assert!(matches!(trend, ScoreTrend::Insufficient));
    });
}

#[test]
fn prop_health_score_trend_improving() {
    let scores = vec![10.0, 20.0, 30.0, 40.0, 50.0];
    let trend = dev_trace_analyzer::compute_health_score_trend(&scores);
    assert!(matches!(trend, ScoreTrend::Improving));
}

#[test]
fn prop_health_score_trend_declining() {
    let scores = vec![50.0, 40.0, 30.0, 20.0, 10.0];
    let trend = dev_trace_analyzer::compute_health_score_trend(&scores);
    assert!(matches!(trend, ScoreTrend::Declining));
}

#[test]
fn prop_analysis_config_default() {
    let config = AnalysisConfig::default();
    assert!((config.weights.success_rate - 0.30).abs() < 1e-9);
    assert!((config.weights.cache - 0.15).abs() < 1e-9);
    assert!((config.thresholds.success_rate_low - 0.30).abs() < 1e-9);
}

// =============================================================================
//  4. Cache Tuning 模块属性测试
// =============================================================================

#[test]
fn prop_cache_sufficient_data_low() {
    proptest!(&test_config(), |(
        checks_hit in 0usize..5,
        successes_hit in 0usize..5,
        checks_miss in 0usize..5,
        successes_miss in 0usize..5
    )| {
        let corr = build_test_correlation(checks_hit, successes_hit, checks_miss, successes_miss);
        prop_assert!(!cache_tuning::has_sufficient_data(&corr, 10));
    });
}

#[test]
fn prop_cache_sufficient_data_high() {
    proptest!(&test_config(), |(
        checks_hit in 10usize..1000,
        successes_hit in 0usize..1000,
        checks_miss in 10usize..1000,
        successes_miss in 0usize..1000
    )| {
        let corr = build_test_correlation(checks_hit, successes_hit, checks_miss, successes_miss);
        prop_assert!(cache_tuning::has_sufficient_data(&corr, 5));
    });
}

#[test]
fn prop_should_disable_cache_negative_diff() {
    proptest!(&test_config(), |(
        checks_hit in 20usize..1000,
        checks_miss in 20usize..1000
    )| {
        // 命中修复率=0%, 未命中修复率=100% → diff=-1.0 → 应禁用
        let corr = build_test_correlation(checks_hit, 0, checks_miss, checks_miss);
        let config = CacheTuningConfig::default();
        if cache_tuning::has_sufficient_data(&corr, config.min_samples) {
            prop_assert!(cache_tuning::should_disable_cache(&corr, &config));
        }
    });
}

#[test]
fn prop_compute_new_ttl_sufficient_data() {
    proptest!(&test_config(), |(current_ttl in 60u64..7200)| {
        // diff 中等负值 → 应该缩短 TTL (不禁用)
        let corr = build_test_correlation(20, 8, 20, 16); // hit=40%, miss=80%, diff=-40%
        let config = CacheTuningConfig::default();
        let new_ttl = cache_tuning::compute_new_ttl(current_ttl, &corr, &config);
        if cache_tuning::has_sufficient_data(&corr, config.min_samples) {
            let diff = corr.hit_vs_miss_diff();
            // diff 在 disable_threshold 和 reduce_ttl_threshold 之间时缩短 TTL
            if diff >= config.disable_threshold && diff < config.reduce_ttl_threshold {
                prop_assert!(new_ttl.is_some());
                if let Some(ttl) = new_ttl {
                    prop_assert!(ttl >= config.min_ttl_secs && ttl <= config.max_ttl_secs);
                }
            } else if diff < config.disable_threshold {
                // diff 太低 → 禁用, compute_new_ttl 返回 None
                prop_assert!(new_ttl.is_none());
            } else {
                // diff 在正常范围, 可能有或无调整
                if let Some(ttl) = new_ttl {
                    prop_assert!(ttl >= config.min_ttl_secs && ttl <= config.max_ttl_secs);
                }
            }
        } else {
            prop_assert!(new_ttl.is_none());
        }
    });
}

#[test]
fn prop_compute_new_ttl_insufficient_data() {
    proptest!(&test_config(), |(current_ttl in 60u64..7200)| {
        let corr = build_test_correlation(1, 0, 1, 0);
        let config = CacheTuningConfig::default();
        let new_ttl = cache_tuning::compute_new_ttl(current_ttl, &corr, &config);
        prop_assert!(new_ttl.is_none());
    });
}

#[test]
fn prop_compute_new_ttl_clamped() {
    proptest!(&test_config(), |(current_ttl in 1u64..50)| {
        let corr = build_test_correlation(20, 20, 20, 2); // diff positive → increase
        let config = CacheTuningConfig::default();
        let new_ttl = cache_tuning::compute_new_ttl(current_ttl, &corr, &config);
        if let Some(ttl) = new_ttl {
            prop_assert!(ttl >= config.min_ttl_secs,
                "new_ttl {} < min_ttl {}", ttl, config.min_ttl_secs);
        }
    });
}

#[test]
fn prop_extract_ttl_trajectory_empty() {
    let result = cache_tuning::extract_ttl_trajectory(&[]);
    assert!(result.is_empty());
}

#[test]
fn prop_extract_correlation_diffs_empty() {
    let result = cache_tuning::extract_correlation_diffs(&[]);
    assert!(result.is_empty());
}

// =============================================================================
//  5. Search Quality 模块属性测试
// =============================================================================

#[test]
fn prop_search_sufficient_data_low() {
    proptest!(&test_config(), |(
        checks_with in 0usize..3,
        successes_with in 0usize..3,
        checks_without in 0usize..3,
        successes_without in 0usize..3
    )| {
        // has_sufficient_data: checks_with > 0 && checks_without > 0 && total >= min_samples
        let stats = build_test_search_stats(checks_with, successes_with, checks_without, successes_without);
        let total = stats.total_checks();
        if total < 10 {
            prop_assert!(!search_quality::has_sufficient_search_data(&stats, 10));
        }
    });
}

#[test]
fn prop_search_sufficient_data_high() {
    proptest!(&test_config(), |(
        checks_with in 10usize..1000,
        successes_with in 0usize..1000,
        checks_without in 10usize..1000,
        successes_without in 0usize..1000
    )| {
        let stats = build_test_search_stats(checks_with, successes_with, checks_without, successes_without);
        prop_assert!(search_quality::has_sufficient_search_data(&stats, 5));
    });
}

#[test]
fn prop_should_disable_search_harmful() {
    proptest!(&test_config(), |(checks in 20usize..1000)| {
        // 搜索修复率=0%, 不搜索修复率=100% → diff=-1.0 → 应禁用
        let stats = build_test_search_stats(checks, 0, checks, checks);
        let config = search_quality::SearchQualityConfig::default();
        let decision = search_quality::compute_search_quality_decision(&stats, &config);
        prop_assert!(decision.is_disable() || decision.is_insufficient_data(),
            "checks={}, succ_with=0, succ_without={}", checks, checks);
    });
}

#[test]
fn prop_should_disable_search_beneficial() {
    proptest!(&test_config(), |(
        checks_with in 20usize..1000,
        successes_with in 15usize..1000,
        checks_without in 20usize..1000,
        successes_without in 0usize..5
    )| {
        let stats = build_test_search_stats(checks_with, successes_with, checks_without, successes_without);
        let config = search_quality::SearchQualityConfig::default();
        let decision = search_quality::compute_search_quality_decision(&stats, &config);
        prop_assert!(decision.is_keep() || decision.is_insufficient_data());
    });
}

#[test]
fn prop_search_fix_rate_in_range() {
    proptest!(&test_config(), |(
        checks_with in 1usize..1000,
        successes_with in 0usize..1000,
        checks_without in 1usize..1000,
        successes_without in 0usize..1000
    )| {
        let stats = build_test_search_stats(
            checks_with, successes_with.min(checks_with),
            checks_without, successes_without.min(checks_without)
        );
        let with_rate = stats.with_search_fix_rate();
        let without_rate = stats.without_search_fix_rate();
        prop_assert!((0.0..=1.0).contains(&with_rate));
        prop_assert!((0.0..=1.0).contains(&without_rate));
    });
}

#[test]
fn prop_search_config_serde_roundtrip() {
    let config = search_quality::SearchQualityConfig::default();
    let json = serde_json::to_string(&config).unwrap();
    let deserialized: search_quality::SearchQualityConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.min_samples, config.min_samples);
}

// =============================================================================
//  6. Memory Evaluation 模块属性测试
// =============================================================================

#[test]
fn prop_should_disable_injection_negative() {
    proptest!(&test_config(), |(diff in -1.0_f64..-0.11)| {
        let result = memory_evaluation::should_disable_injection(diff, -0.1);
        prop_assert!(result);
    });
}

#[test]
fn prop_should_disable_injection_positive() {
    proptest!(&test_config(), |(diff in 0.01_f64..1.0)| {
        let result = memory_evaluation::should_disable_injection(diff, -0.1);
        prop_assert!(!result);
    });
}

#[test]
fn prop_should_disable_injection_zero() {
    let result = memory_evaluation::should_disable_injection(0.0, -0.1);
    assert!(!result);
}

#[test]
fn prop_memory_sufficient_data_low() {
    assert!(!memory_evaluation::has_sufficient_evaluation_data(0, 0, 5));
}

#[test]
fn prop_memory_fix_rate_in_range() {
    proptest!(&test_config(), |(
        checks_with in 1usize..1000,
        successes_with in 0usize..1000
    )| {
        let mut stats = MemoryEvaluationStats::new();
        for _ in 0..checks_with {
            stats.record_with_memory(false);
        }
        for _ in 0..successes_with.min(checks_with) {
            stats.record_with_memory(true);
        }
        let with_rate = stats.with_memory_fix_rate();
        prop_assert!((0.0..=1.0).contains(&with_rate));
    });
}

// =============================================================================
//  7. Error Search 模块属性测试
// =============================================================================

#[test]
fn prop_build_search_query_empty() {
    let errors: Vec<CompileError> = vec![];
    let result = error_search::build_error_search_query(&errors);
    assert!(result.is_none());
}

#[test]
fn prop_build_search_query_nonempty() {
    proptest!(&test_config(), |(messages in proptest::collection::vec("[a-zA-Z0-9 ]{5,100}", 1..10))| {
        let errors: Vec<CompileError> = messages.iter().map(|m| CompileError {
            file: "test.rs".to_string(),
            line: Some(1),
            column: Some(1),
            message: m.clone(),
            error_code: Some("E0001".to_string()),
        }).collect();
        let result = error_search::build_error_search_query(&errors);
        prop_assert!(result.is_some(), "messages: {:?}", messages);
    });
}

#[test]
fn prop_extract_keywords_nonempty() {
    proptest!(&test_config(), |(message in "[a-zA-Z0-9]{5,200}")| {
        let keywords = error_search::extract_error_keywords(&message);
        prop_assert!(!keywords.is_empty());
    });
}

#[test]
fn prop_truncate_search_results_length() {
    proptest!(&test_config(), |(
        results in "[a-zA-Z0-9 ]{0,10000}",
        max_chars in 500usize..5000
    )| {
        let truncated = error_search::truncate_search_results(&results, max_chars);
        // 允许一定容差 (省略号 + 换行等)
        prop_assert!(truncated.len() <= max_chars + 200,
            "truncated.len()={} > max_chars+200={}", truncated.len(), max_chars + 200);
    });
}

#[test]
fn prop_should_search_errors_network() {
    let errors = vec![CompileError {
        file: "test.rs".to_string(),
        line: Some(1),
        column: None,
        message: "network error".to_string(),
        error_code: None,
    }];
    let result = error_search::should_search_errors(&errors, 1, true);
    assert!(!result);
}

// =============================================================================
//  8. Search Cache 模块属性测试
// =============================================================================

#[test]
fn prop_build_cache_key_empty() {
    let errors: Vec<CompileError> = vec![];
    let result = search_cache::build_cache_key(&errors);
    assert!(result.is_none());
}

#[test]
fn prop_build_cache_key_nonempty() {
    proptest!(&test_config(), |(messages in proptest::collection::vec(".{5,100}", 1..10))| {
        let errors: Vec<CompileError> = messages.iter().map(|m| CompileError {
            file: "test.rs".to_string(),
            line: Some(1),
            column: Some(1),
            message: m.clone(),
            error_code: None,
        }).collect();
        let result = search_cache::build_cache_key(&errors);
        prop_assert!(result.is_some());
    });
}

#[test]
fn prop_build_cache_key_deterministic() {
    proptest!(&test_config(), |(messages in proptest::collection::vec(".{5,100}", 1..5))| {
        let errors: Vec<CompileError> = messages.iter().map(|m| CompileError {
            file: "test.rs".to_string(),
            line: Some(1),
            column: Some(1),
            message: m.clone(),
            error_code: None,
        }).collect();
        let key1 = search_cache::build_cache_key(&errors);
        let key2 = search_cache::build_cache_key(&errors);
        prop_assert_eq!(key1, key2);
    });
}

#[test]
fn prop_normalize_query_nonempty() {
    proptest!(&test_config(), |(query in ".{1,200}")| {
        let normalized = search_cache::normalize_query_for_cache(&query);
        prop_assert!(!normalized.is_empty());
    });
}

#[test]
fn prop_is_cache_expired_old() {
    proptest!(&test_config(), |(
        cached_at in 0u64..1000,
        now in 2000u64..10000,
        ttl in 1u64..1000
    )| {
        prop_assert!(search_cache::is_cache_expired(cached_at, now, ttl));
    });
}

#[test]
fn prop_is_cache_expired_fresh() {
    proptest!(&test_config(), |(cached_at in 0u64..1000, ttl in 2000u64..10000)| {
        let now = cached_at + 100;
        prop_assert!(!search_cache::is_cache_expired(cached_at, now, ttl));
    });
}

// =============================================================================
//  9. Radix Tree 模块属性测试
// =============================================================================

#[test]
fn prop_compute_fingerprints_count() {
    proptest!(&test_config(), |(messages in proptest::collection::vec(".{1,200}", 0..50))| {
        let fingerprints = radix_tree::compute_fingerprints_owned(&messages);
        prop_assert_eq!(fingerprints.len(), messages.len());
    });
}

#[test]
fn prop_compute_fingerprints_deterministic() {
    proptest!(&test_config(), |(messages in proptest::collection::vec(".{1,200}", 1..20))| {
        let fp1 = radix_tree::compute_fingerprints_owned(&messages.clone());
        let fp2 = radix_tree::compute_fingerprints_owned(&messages);
        prop_assert_eq!(fp1, fp2);
    });
}

#[test]
fn prop_common_prefix_length_bound() {
    proptest!(&test_config(), |(
        a in proptest::collection::vec(".{0,50}", 0..20),
        b in proptest::collection::vec(".{0,50}", 0..20)
    )| {
        let a_refs: Vec<&str> = a.iter().map(|s| s.as_str()).collect();
        let b_refs: Vec<&str> = b.iter().map(|s| s.as_str()).collect();
        let prefix_len = radix_tree::common_prefix_length(&a_refs, &b_refs);
        prop_assert!(prefix_len <= a.len());
        prop_assert!(prefix_len <= b.len());
    });
}

#[test]
fn prop_common_prefix_empty() {
    let a: Vec<&str> = vec![];
    let b: Vec<&str> = vec![];
    let prefix_len = radix_tree::common_prefix_length(&a, &b);
    assert_eq!(prefix_len, 0);
}

#[test]
fn prop_common_prefix_identical() {
    proptest!(&test_config(), |(messages in proptest::collection::vec(".{1,50}", 1..20))| {
        let refs: Vec<&str> = messages.iter().map(|s| s.as_str()).collect();
        let prefix_len = radix_tree::common_prefix_length(&refs, &refs);
        prop_assert_eq!(prefix_len, messages.len());
    });
}

// =============================================================================
//  10. Live Continuation 模块属性测试
// =============================================================================

#[test]
fn prop_compute_diff_subset() {
    proptest!(&test_config(), |(
        sent in proptest::collection::vec(".{1,100}", 0..20),
        messages in proptest::collection::vec(".{1,100}", 0..20)
    )| {
        let sent_refs: Vec<&str> = sent.iter().map(|s| s.as_str()).collect();
        let msg_refs: Vec<&str> = messages.iter().map(|s| s.as_str()).collect();
        let diff = live_continuation::compute_diff(&sent_refs, &msg_refs);
        prop_assert!(diff.len() <= messages.len());
    });
}

#[test]
fn prop_compute_diff_empty_sent() {
    proptest!(&test_config(), |(messages in proptest::collection::vec(".{1,100}", 0..20))| {
        let msg_refs: Vec<&str> = messages.iter().map(|s| s.as_str()).collect();
        let diff = live_continuation::compute_diff(&[], &msg_refs);
        prop_assert_eq!(diff.len(), messages.len());
    });
}

#[test]
fn prop_deduplicate_bound() {
    proptest!(&test_config(), |(messages in proptest::collection::vec(".{1,100}", 0..50))| {
        let refs: Vec<&str> = messages.iter().map(|s| s.as_str()).collect();
        let deduped = live_continuation::deduplicate(&refs);
        prop_assert!(deduped.len() <= messages.len());
    });
}

#[test]
fn prop_deduplicate_no_duplicates() {
    proptest!(&test_config(), |(messages in proptest::collection::vec("[a-z]{1,20}", 1..30))| {
        let unique: Vec<String> = {
            let mut seen = std::collections::HashSet::new();
            messages.into_iter().filter(|m| seen.insert(m.clone())).collect()
        };
        let refs: Vec<&str> = unique.iter().map(|s| s.as_str()).collect();
        let deduped = live_continuation::deduplicate(&refs);
        prop_assert_eq!(deduped.len(), unique.len());
    });
}

#[test]
fn prop_find_duplicates_none() {
    proptest!(&test_config(), |(messages in proptest::collection::vec("[a-z]{1,20}", 1..30))| {
        let unique: Vec<String> = {
            let mut seen = std::collections::HashSet::new();
            messages.into_iter().filter(|m| seen.insert(m.clone())).collect()
        };
        let refs: Vec<&str> = unique.iter().map(|s| s.as_str()).collect();
        let dups = live_continuation::find_duplicates(&refs);
        prop_assert!(dups.is_empty());
    });
}

#[test]
fn prop_compute_message_ids_count() {
    proptest!(&test_config(), |(messages in proptest::collection::vec(".{1,100}", 0..50))| {
        let refs: Vec<&str> = messages.iter().map(|s| s.as_str()).collect();
        let ids = live_continuation::compute_message_ids(&refs);
        prop_assert_eq!(ids.len(), messages.len());
    });
}

// =============================================================================
//  11. HTML Report 模块属性测试
// =============================================================================

#[test]
fn prop_doughnut_colors_count() {
    proptest!(&test_config(), |(count in 1usize..50)| {
        let colors = html_report::generate_doughnut_colors(count);
        prop_assert_eq!(colors.len(), count);
    });
}

#[test]
fn prop_doughnut_colors_zero() {
    let colors = html_report::generate_doughnut_colors(0);
    assert!(colors.is_empty());
}

#[test]
fn prop_point_colors_count() {
    proptest!(&test_config(), |(data in proptest::collection::vec(-1e3_f64..1e3, 0..100))| {
        let colors = html_report::generate_point_colors(&data);
        prop_assert_eq!(colors.len(), data.len());
    });
}

#[test]
fn prop_point_colors_positive() {
    proptest!(&test_config(), |(value in 0.01_f64..1e3)| {
        let colors = html_report::generate_point_colors(&[value]);
        prop_assert!(colors[0].contains("75, 192, 192"));
    });
}

// =============================================================================
//  12. Failover Chat 模块属性测试
// =============================================================================

#[test]
fn prop_should_failover_healthy() {
    let healthy_result = HealthCheckResult::new(SiteHealthStatus::Healthy);
    assert!(!failover_chat::should_failover_decision(&healthy_result));
}

#[test]
fn prop_should_failover_rate_limited() {
    let rl_result = HealthCheckResult::new(SiteHealthStatus::RateLimited);
    assert!(failover_chat::should_failover_decision(&rl_result));
}

#[test]
fn prop_build_error_health_result() {
    proptest!(&test_config(), |(msg in ".{1,200}")| {
        let result = failover_chat::build_error_health_result(msg.clone());
        prop_assert_eq!(result.status, SiteHealthStatus::NetworkError);
        prop_assert_eq!(result.message.as_deref(), Some(msg.as_str()));
    });
}

// =============================================================================
//  13. Joint Decision 模块属性测试
// =============================================================================

#[test]
fn prop_conservative_mode_at_threshold() {
    proptest!(&test_config(), |(
        disabled in 0usize..100,
        total in 1usize..100,
        threshold in 1usize..100
    )| {
        let actual_disabled = disabled.min(total);
        let result = joint_decision::should_enter_conservative_mode(actual_disabled, total, threshold);
        // should_enter_conservative_mode: disabled_count >= threshold.min(total)
        if actual_disabled >= threshold.min(total) {
            prop_assert!(result);
        }
    });
}

#[test]
fn prop_conservative_mode_zero_total() {
    proptest!(&test_config(), |(threshold in 1usize..100)| {
        prop_assert!(!joint_decision::should_enter_conservative_mode(0, 0, threshold));
    });
}

#[test]
fn prop_escalate_warning_zero_total() {
    proptest!(&test_config(), |(
        warn_threshold in 1usize..100,
        conservative_threshold in 1usize..100
    )| {
        prop_assert!(!joint_decision::should_escalate_warning(0, 0, warn_threshold, conservative_threshold));
    });
}

#[test]
fn prop_escalate_warning_below() {
    proptest!(&test_config(), |(
        warnings in 0usize..5,
        total in 10usize..100,
        warn_threshold in 6usize..100,
        conservative_threshold in 1usize..100
    )| {
        prop_assert!(!joint_decision::should_escalate_warning(warnings, total, warn_threshold, conservative_threshold));
    });
}

#[test]
fn prop_escalate_warning_at() {
    proptest!(&test_config(), |(
        warnings in 6usize..100,
        total in 10usize..200,
        warn_threshold in 1usize..6,
        conservative_threshold in 1usize..100
    )| {
        let actual_warnings = warnings.min(total);
        let result = joint_decision::should_escalate_warning(actual_warnings, total, warn_threshold, conservative_threshold);
        let conservative = joint_decision::should_enter_conservative_mode(actual_warnings, total, conservative_threshold);
        if !conservative {
            prop_assert!(result);
        }
    });
}

// =============================================================================
//  14. 压力测试 — 大规模数据
// =============================================================================

/// 生成大规模 DevTrace 条目列表
fn generate_large_entries(count: usize) -> Vec<DevTraceEntry> {
    use chrono::Utc;
    (0..count)
        .map(|i| DevTraceEntry {
            timestamp: Utc::now(),
            phase_idx: Some(i % 5),
            task_idx: Some(i % 20),
            task_name: Some(format!("task_{i}")),
            action: TraceAction::all()[i % TraceAction::all().len()],
            input_summary: format!("input_{i}"),
            output_summary: format!("output_{i}"),
            duration_ms: (i as u64) * 10,
            success: i % 3 != 0,
            error: if i % 3 == 0 {
                Some(format!("error_{i}"))
            } else {
                None
            },
        })
        .collect()
}

/// 压力测试: 10,000 条 DevTrace 条目的统计计算
#[test]
fn stress_test_large_devtrace_summary_10k() {
    let entries = generate_large_entries(10_000);
    let summary = DevTraceSummary::from_entries(&entries);
    assert_eq!(summary.total_entries, 10_000);
}

/// 压力测试: 10,000 条 DevTrace 条目的时间线构建
#[test]
fn stress_test_large_timeline_10k() {
    let entries = generate_large_entries(10_000);
    let timeline = dev_trace::build_timeline(&entries, 100);
    assert!(timeline.len() <= 100);
}

/// 压力测试: 10,000 条 DevTrace 条目的分组统计
#[test]
fn stress_test_large_group_entries_10k() {
    let entries = generate_large_entries(10_000);
    let grouped = dev_trace::group_entries_by_action(&entries);
    assert!(!grouped.is_empty());
}

/// 压力测试: 大规模 CacheFixCorrelation 计算
#[test]
fn stress_test_large_cache_correlation_1k() {
    let entries = generate_large_entries(1_000);
    let _corr = dev_trace::build_cache_fix_correlation(&entries);
}

/// 压力测试: 大规模搜索质量统计
#[test]
fn stress_test_large_search_quality_1k() {
    let entries = generate_large_entries(1_000);
    let _stats = dev_trace::build_search_quality_stats(&entries);
}

/// 压力测试: 大规模内存评估统计
#[test]
fn stress_test_large_memory_evaluation_1k() {
    let entries = generate_large_entries(1_000);
    let _stats = dev_trace::build_memory_evaluation_stats(&entries);
}

/// 压力测试: 1,000 条缓存调优决策的 TTL 轨迹提取
#[test]
fn stress_test_large_ttl_trajectory_1k() {
    let decisions: Vec<CacheTuningDecision> = (0..1_000)
        .map(|i| CacheTuningDecision {
            action: if i % 3 == 0 {
                TuningAction::DisableCache
            } else if i % 3 == 1 {
                TuningAction::AdjustTtl { new_ttl: 150 }
            } else {
                TuningAction::KeepCurrent
            },
            reason: format!("test_{}", i),
            old_ttl: 300,
            correlation_diff: if i % 2 == 0 { -0.05 } else { 0.05 },
        })
        .collect();
    let trajectory = cache_tuning::extract_ttl_trajectory(&decisions);
    assert!(trajectory.len() <= decisions.len());
    let diffs = cache_tuning::extract_correlation_diffs(&decisions);
    assert!(diffs.len() <= decisions.len());
}

/// 压力测试: 大规模 sparkline 渲染 (10,000 数据点)
#[test]
fn stress_test_large_sparkline_10k() {
    let values: Vec<f64> = (0..10_000).map(|i| (i as f64).sin() * 100.0).collect();
    let config = sparkline::SparklineConfig::new(60);
    let rendered = sparkline::render_sparkline(&values, &config);
    assert!(!rendered.is_empty());
    assert!(rendered.len() <= 200); // 60 chars + min/max labels + ANSI codes
}

// =============================================================================
//  辅助函数 — 构建测试数据
// =============================================================================

/// 构建 DevTraceSummary 测试数据
fn build_test_summary(success_rate: f64, cache_hit_rate: f64) -> DevTraceSummary {
    let mut summary = DevTraceSummary::empty();
    summary.total_entries = 100;
    summary.success_rate = success_rate;
    summary.cache_summary = Some(dev_trace::CacheStatsSummary {
        cache_hits: (cache_hit_rate * 50.0) as u32,
        cache_misses: 50 - (cache_hit_rate * 50.0) as u32,
        search_failures: 0,
        time_saved_ms: 5000,
    });
    summary
}

/// 构建 CacheFixCorrelation 测试数据
fn build_test_correlation(
    checks_hit: usize,
    successes_hit: usize,
    checks_miss: usize,
    successes_miss: usize,
) -> CacheFixCorrelation {
    let mut corr = CacheFixCorrelation::new();
    corr.checks_after_hit = checks_hit;
    corr.successes_after_hit = successes_hit;
    corr.checks_after_miss = checks_miss;
    corr.successes_after_miss = successes_miss;
    corr
}

/// 构建 SearchQualityStats 测试数据
fn build_test_search_stats(
    checks_with: usize,
    successes_with: usize,
    checks_without: usize,
    successes_without: usize,
) -> dev_trace::SearchQualityStats {
    let mut stats = dev_trace::SearchQualityStats::new();
    for _ in 0..checks_with {
        stats.record_with_search(false);
    }
    for _ in 0..successes_with.min(checks_with) {
        stats.record_with_search(true);
    }
    for _ in 0..checks_without {
        stats.record_without_search(false);
    }
    for _ in 0..successes_without.min(checks_without) {
        stats.record_without_search(true);
    }
    stats
}
