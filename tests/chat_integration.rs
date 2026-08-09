//! chat.rs 集成测试 (Session 68)
//!
//! 测试 chat 模块公共 API 的端到端行为:
//! - TimeoutConfig 创建、组合、链式调用
//! - TimeoutConfig 跨 SiteType 适配 (DeepSeek/Zai/Kimi/Tongyi/Claude/Unknown)
//! - TimeoutConfig 边界场景 (零值/极值/边界条件)
//! - TimeoutConfig 与 FailoverChatClient 集成
//! - ChatResult / ChatMessage 结构体行为
//! - 三阶段超时预算分析
//! - 多网站超时配置场景模拟

use forge::browser::SiteType;
use forge::chat::{ChatMessage, ChatSession, TimeoutConfig};
use forge::traits::ChatResult;

// ============================================================================
//  TimeoutConfig 基础创建测试
// ============================================================================

#[test]
fn test_timeout_config_default_values() {
    let config = TimeoutConfig::default();
    assert_eq!(config.phase1_secs, 30);
    assert_eq!(config.phase2_secs, 60);
    assert_eq!(config.phase3_secs, 45);
    assert_eq!(config.stuck_threshold_secs, 180);
}

#[test]
fn test_timeout_config_new_custom_values() {
    let config = TimeoutConfig::new(10, 120, 60);
    assert_eq!(config.phase1_secs, 10);
    assert_eq!(config.phase2_secs, 120);
    assert_eq!(config.phase3_secs, 60);
    assert_eq!(config.stuck_threshold_secs, 120);
}

#[test]
fn test_timeout_config_from_timeout_secs() {
    let config = TimeoutConfig::from_timeout_secs(300);
    assert_eq!(config.phase1_secs, 60);
    assert_eq!(config.phase2_secs, 300);
    assert_eq!(config.phase3_secs, 45);
    assert_eq!(config.stuck_threshold_secs, 0);
}

#[test]
fn test_timeout_config_from_timeout_secs_short() {
    let config = TimeoutConfig::from_timeout_secs(30);
    assert_eq!(config.phase1_secs, 30);
    assert_eq!(config.phase2_secs, 30);
    assert_eq!(config.phase3_secs, 45);
}

#[test]
fn test_timeout_config_from_timeout_secs_zero() {
    let config = TimeoutConfig::from_timeout_secs(0);
    assert_eq!(config.phase1_secs, 0);
    assert_eq!(config.phase2_secs, 0);
    assert_eq!(config.phase3_secs, 45);
}

// ============================================================================
//  TimeoutConfig 链式调用测试
// ============================================================================

#[test]
fn test_timeout_config_with_stuck_threshold_chaining() {
    let config = TimeoutConfig::default().with_stuck_threshold(300);
    assert_eq!(config.stuck_threshold_secs, 300);
    assert!(config.has_stuck_detection());
}

#[test]
fn test_timeout_config_with_stuck_threshold_zero_disables_detection() {
    let config = TimeoutConfig::default().with_stuck_threshold(0);
    assert_eq!(config.stuck_threshold_secs, 0);
    assert!(!config.has_stuck_detection());
}

#[test]
fn test_timeout_config_chaining_preserves_other_fields() {
    let config = TimeoutConfig::new(20, 100, 50).with_stuck_threshold(200);
    assert_eq!(config.phase1_secs, 20);
    assert_eq!(config.phase2_secs, 100);
    assert_eq!(config.phase3_secs, 50);
    assert_eq!(config.stuck_threshold_secs, 200);
}

#[test]
fn test_timeout_config_has_stuck_detection_default_true() {
    let config = TimeoutConfig::default();
    assert!(config.has_stuck_detection());
}

#[test]
fn test_timeout_config_has_stuck_detection_from_timeout_secs_false() {
    let config = TimeoutConfig::from_timeout_secs(120);
    assert!(!config.has_stuck_detection());
}

// ============================================================================
//  TimeoutConfig::total_max_secs 测试
// ============================================================================

#[test]
fn test_total_max_secs_default() {
    let config = TimeoutConfig::default();
    assert_eq!(config.total_max_secs(), 30 + 60 + 45);
}

#[test]
fn test_total_max_secs_custom() {
    let config = TimeoutConfig::new(10, 120, 60);
    assert_eq!(config.total_max_secs(), 10 + 120 + 60);
}

#[test]
fn test_total_max_secs_from_timeout_secs() {
    let config = TimeoutConfig::from_timeout_secs(300);
    assert_eq!(config.total_max_secs(), 60 + 300 + 45);
}

#[test]
fn test_total_max_secs_zero_phases() {
    let config = TimeoutConfig::new(0, 0, 0);
    assert_eq!(config.total_max_secs(), 0);
}

// ============================================================================
//  TimeoutConfig::for_site_type — DeepSeek 适配
// ============================================================================

#[test]
fn test_for_site_type_deepseek_below_minimum_raised() {
    let config = TimeoutConfig::new(10, 60, 45);
    let adjusted = config.for_site_type(SiteType::DeepSeek);
    assert_eq!(adjusted.phase1_secs, 30);
}

#[test]
fn test_for_site_type_deepseek_at_minimum_kept() {
    let config = TimeoutConfig::new(30, 60, 45);
    let adjusted = config.for_site_type(SiteType::DeepSeek);
    assert_eq!(adjusted.phase1_secs, 30);
}

#[test]
fn test_for_site_type_deepseek_above_minimum_kept() {
    let config = TimeoutConfig::new(60, 60, 45);
    let adjusted = config.for_site_type(SiteType::DeepSeek);
    assert_eq!(adjusted.phase1_secs, 60);
}

#[test]
fn test_for_site_type_deepseek_preserves_other_phases() {
    let config = TimeoutConfig::new(10, 120, 60);
    let adjusted = config.for_site_type(SiteType::DeepSeek);
    assert_eq!(adjusted.phase1_secs, 30);
    assert_eq!(adjusted.phase2_secs, 120);
    assert_eq!(adjusted.phase3_secs, 60);
    assert_eq!(adjusted.stuck_threshold_secs, 120);
}

// ============================================================================
//  TimeoutConfig::for_site_type — Z.ai 适配
// ============================================================================

#[test]
fn test_for_site_type_zai_below_minimum_raised() {
    let config = TimeoutConfig::new(15, 60, 45);
    let adjusted = config.for_site_type(SiteType::Zai);
    assert_eq!(adjusted.phase1_secs, 30);
}

#[test]
fn test_for_site_type_zai_at_minimum_kept() {
    let config = TimeoutConfig::new(30, 60, 45);
    let adjusted = config.for_site_type(SiteType::Zai);
    assert_eq!(adjusted.phase1_secs, 30);
}

#[test]
fn test_for_site_type_zai_above_minimum_kept() {
    let config = TimeoutConfig::new(45, 60, 45);
    let adjusted = config.for_site_type(SiteType::Zai);
    assert_eq!(adjusted.phase1_secs, 45);
}

// ============================================================================
//  TimeoutConfig::for_site_type — Kimi/Tongyi/Claude 适配
// ============================================================================

#[test]
fn test_for_site_type_kimi_below_minimum_raised() {
    let config = TimeoutConfig::new(10, 60, 45);
    let adjusted = config.for_site_type(SiteType::Kimi);
    assert_eq!(adjusted.phase1_secs, 20);
}

#[test]
fn test_for_site_type_tongyi_below_minimum_raised() {
    let config = TimeoutConfig::new(10, 60, 45);
    let adjusted = config.for_site_type(SiteType::Tongyi);
    assert_eq!(adjusted.phase1_secs, 20);
}

#[test]
fn test_for_site_type_claude_below_minimum_raised() {
    let config = TimeoutConfig::new(10, 60, 45);
    let adjusted = config.for_site_type(SiteType::Claude);
    assert_eq!(adjusted.phase1_secs, 20);
}

#[test]
fn test_for_site_type_kimi_at_minimum_kept() {
    let config = TimeoutConfig::new(20, 60, 45);
    let adjusted = config.for_site_type(SiteType::Kimi);
    assert_eq!(adjusted.phase1_secs, 20);
}

#[test]
fn test_for_site_type_claude_above_minimum_kept() {
    let config = TimeoutConfig::new(40, 60, 45);
    let adjusted = config.for_site_type(SiteType::Claude);
    assert_eq!(adjusted.phase1_secs, 40);
}

// ============================================================================
//  TimeoutConfig::for_site_type — Unknown 适配
// ============================================================================

#[test]
fn test_for_site_type_unknown_no_adjustment() {
    let config = TimeoutConfig::new(10, 60, 45);
    let adjusted = config.for_site_type(SiteType::Unknown);
    assert_eq!(adjusted.phase1_secs, 10);
}

#[test]
fn test_for_site_type_unknown_zero_kept() {
    let config = TimeoutConfig::new(0, 60, 45);
    let adjusted = config.for_site_type(SiteType::Unknown);
    assert_eq!(adjusted.phase1_secs, 0);
}

// ============================================================================
//  TimeoutConfig::for_site_type — 不可变性测试
// ============================================================================

#[test]
fn test_for_site_type_does_not_modify_original() {
    let config = TimeoutConfig::new(10, 60, 45);
    let _adjusted = config.for_site_type(SiteType::DeepSeek);
    assert_eq!(config.phase1_secs, 10);
}

#[test]
fn test_for_site_type_returns_new_instance() {
    let config = TimeoutConfig::new(10, 60, 45);
    let adjusted = config.for_site_type(SiteType::DeepSeek);
    assert_ne!(config.phase1_secs, adjusted.phase1_secs);
}

// ============================================================================
//  TimeoutConfig::for_site_type — 全网站遍历
// ============================================================================

#[test]
fn test_for_site_type_all_sites_minimum_threshold() {
    let config = TimeoutConfig::new(5, 60, 45);

    let deepseek = config.for_site_type(SiteType::DeepSeek);
    assert_eq!(deepseek.phase1_secs, 30);

    let zai = config.for_site_type(SiteType::Zai);
    assert_eq!(zai.phase1_secs, 30);

    let kimi = config.for_site_type(SiteType::Kimi);
    assert_eq!(kimi.phase1_secs, 20);

    let tongyi = config.for_site_type(SiteType::Tongyi);
    assert_eq!(tongyi.phase1_secs, 20);

    let claude = config.for_site_type(SiteType::Claude);
    assert_eq!(claude.phase1_secs, 20);

    let unknown = config.for_site_type(SiteType::Unknown);
    assert_eq!(unknown.phase1_secs, 5);
}

#[test]
fn test_for_site_type_high_values_unchanged_across_sites() {
    let config = TimeoutConfig::new(120, 300, 60);

    for site in [
        SiteType::DeepSeek,
        SiteType::Zai,
        SiteType::Kimi,
        SiteType::Tongyi,
        SiteType::Claude,
        SiteType::Unknown,
    ] {
        let adjusted = config.for_site_type(site);
        assert_eq!(
            adjusted.phase1_secs, 120,
            "phase1 should remain 120 for {:?}",
            site
        );
        assert_eq!(adjusted.phase2_secs, 300);
        assert_eq!(adjusted.phase3_secs, 60);
    }
}

// ============================================================================
//  三阶段超时预算分析
// ============================================================================

#[test]
fn test_total_timeout_budget_default() {
    let config = TimeoutConfig::default();
    let total = config.total_max_secs();
    assert_eq!(total, 135);
}

#[test]
fn test_total_timeout_budget_with_stuck_detection() {
    let config = TimeoutConfig::default().with_stuck_threshold(300);
    let total = config.total_max_secs();
    assert_eq!(total, 135);
    assert_eq!(config.stuck_threshold_secs, 300);
}

#[test]
fn test_total_timeout_budget_exceeds_phase2() {
    let config = TimeoutConfig::new(30, 300, 45);
    let total = config.total_max_secs();
    assert_eq!(total, 375);
    assert!(total > config.phase2_secs);
}

#[test]
fn test_phase2_dominates_total_budget() {
    let config = TimeoutConfig::new(30, 600, 45);
    let total = config.total_max_secs();
    let phase2_ratio = config.phase2_secs as f64 / total as f64;
    assert!(
        phase2_ratio > 0.8,
        "Phase 2 should dominate total budget: ratio={:.2}",
        phase2_ratio
    );
}

#[test]
fn test_phase1_is_smallest_phase() {
    let config = TimeoutConfig::default();
    assert!(config.phase1_secs <= config.phase2_secs);
    assert!(config.phase1_secs <= config.phase3_secs);
}

// ============================================================================
//  TimeoutConfig 多网站场景模拟
// ============================================================================

#[test]
fn test_multi_site_timeout_configuration() {
    let base = TimeoutConfig::new(10, 120, 45);

    let deepseek_config = base.for_site_type(SiteType::DeepSeek);
    let zai_config = base.for_site_type(SiteType::Zai);

    assert_eq!(deepseek_config.phase1_secs, 30);
    assert_eq!(zai_config.phase1_secs, 30);
    assert_eq!(deepseek_config.phase2_secs, 120);
    assert_eq!(zai_config.phase2_secs, 120);
}

#[test]
fn test_multi_site_total_max_secs_comparison() {
    let base = TimeoutConfig::new(10, 120, 45);

    let deepseek_total = base.for_site_type(SiteType::DeepSeek).total_max_secs();
    let kimi_total = base.for_site_type(SiteType::Kimi).total_max_secs();
    let unknown_total = base.for_site_type(SiteType::Unknown).total_max_secs();

    assert_eq!(deepseek_total, 30 + 120 + 45);
    assert_eq!(kimi_total, 20 + 120 + 45);
    assert_eq!(unknown_total, 10 + 120 + 45);

    assert!(deepseek_total > kimi_total);
    assert!(kimi_total > unknown_total);
}

#[test]
fn test_stuck_threshold_independent_of_site_type() {
    let base = TimeoutConfig::new(10, 120, 45).with_stuck_threshold(250);

    let deepseek = base.for_site_type(SiteType::DeepSeek);
    let kimi = base.for_site_type(SiteType::Kimi);
    let unknown = base.for_site_type(SiteType::Unknown);

    assert_eq!(deepseek.stuck_threshold_secs, 250);
    assert_eq!(kimi.stuck_threshold_secs, 250);
    assert_eq!(unknown.stuck_threshold_secs, 250);
}

// ============================================================================
//  TimeoutConfig 边界场景
// ============================================================================

#[test]
fn test_timeout_config_zero_phase1() {
    let config = TimeoutConfig::new(0, 60, 45);
    assert_eq!(config.phase1_secs, 0);
    assert_eq!(config.total_max_secs(), 105);
}

#[test]
fn test_timeout_config_all_zero() {
    let config = TimeoutConfig::new(0, 0, 0);
    assert_eq!(config.total_max_secs(), 0);
    assert_eq!(config.stuck_threshold_secs, 120);
    assert!(config.has_stuck_detection());
}

#[test]
fn test_timeout_config_extreme_values() {
    let config = TimeoutConfig::new(u64::MAX / 3, u64::MAX / 3, u64::MAX / 3);
    assert!(config.total_max_secs() > 0);
}

#[test]
fn test_timeout_config_from_timeout_secs_large() {
    let config = TimeoutConfig::from_timeout_secs(u64::MAX);
    assert_eq!(config.phase1_secs, 60);
    assert_eq!(config.phase2_secs, u64::MAX);
    assert_eq!(config.phase3_secs, 45);
}

#[test]
fn test_timeout_config_phase1_capped_at_60_from_timeout_secs() {
    let config = TimeoutConfig::from_timeout_secs(1000);
    assert_eq!(config.phase1_secs, 60);
}

// ============================================================================
//  ChatResult 行为测试
// ============================================================================

#[test]
fn test_chat_result_creation() {
    let result = ChatResult {
        text: "Hello, world!".to_string(),
        timed_out: false,
    };
    assert_eq!(result.text, "Hello, world!");
    assert!(!result.timed_out);
}

#[test]
fn test_chat_result_timed_out() {
    let result = ChatResult {
        text: "Partial response...".to_string(),
        timed_out: true,
    };
    assert!(result.timed_out);
    assert!(!result.text.is_empty());
}

#[test]
fn test_chat_result_empty_text() {
    let result = ChatResult {
        text: String::new(),
        timed_out: false,
    };
    assert!(result.text.is_empty());
    assert!(!result.timed_out);
}

#[test]
fn test_chat_result_clone() {
    let result = ChatResult {
        text: "Test message".to_string(),
        timed_out: true,
    };
    let cloned = result.clone();
    assert_eq!(result.text, cloned.text);
    assert_eq!(result.timed_out, cloned.timed_out);
}

#[test]
fn test_chat_result_debug_format() {
    let result = ChatResult {
        text: "Debug test".to_string(),
        timed_out: false,
    };
    let debug_str = format!("{:?}", result);
    assert!(debug_str.contains("Debug test"));
    assert!(debug_str.contains("timed_out"));
}

// ============================================================================
//  ChatMessage / ChatSession 行为测试
// ============================================================================

#[test]
fn test_chat_message_creation() {
    let msg = ChatMessage {
        role: "user".to_string(),
        content: "Create a calculator".to_string(),
        timestamp: "2024-01-01T00:00:00Z".to_string(),
    };
    assert_eq!(msg.role, "user");
    assert_eq!(msg.content, "Create a calculator");
    assert_eq!(msg.timestamp, "2024-01-01T00:00:00Z");
}

#[test]
fn test_chat_message_assistant_role() {
    let msg = ChatMessage {
        role: "assistant".to_string(),
        content: "Here is the code...".to_string(),
        timestamp: "2024-01-01T00:01:00Z".to_string(),
    };
    assert_eq!(msg.role, "assistant");
}

#[test]
fn test_chat_session_empty() {
    let session = ChatSession {
        tab_index: 0,
        messages: vec![],
    };
    assert_eq!(session.tab_index, 0);
    assert!(session.messages.is_empty());
}

#[test]
fn test_chat_session_with_messages() {
    let session = ChatSession {
        tab_index: 1,
        messages: vec![
            ChatMessage {
                role: "user".to_string(),
                content: "Hello".to_string(),
                timestamp: "2024-01-01T00:00:00Z".to_string(),
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: "Hi there!".to_string(),
                timestamp: "2024-01-01T00:00:05Z".to_string(),
            },
        ],
    };
    assert_eq!(session.tab_index, 1);
    assert_eq!(session.messages.len(), 2);
    assert_eq!(session.messages[0].role, "user");
    assert_eq!(session.messages[1].role, "assistant");
}

// ============================================================================
//  端到端超时场景模拟
// ============================================================================

#[test]
fn test_e2e_timeout_config_for_deepseek_workflow() {
    let config = TimeoutConfig::new(15, 120, 45)
        .with_stuck_threshold(180)
        .for_site_type(SiteType::DeepSeek);

    assert_eq!(config.phase1_secs, 30);
    assert_eq!(config.phase2_secs, 120);
    assert_eq!(config.phase3_secs, 45);
    assert_eq!(config.stuck_threshold_secs, 180);
    assert!(config.has_stuck_detection());

    let total = config.total_max_secs();
    assert_eq!(total, 30 + 120 + 45);
}

#[test]
fn test_e2e_timeout_config_for_zai_workflow() {
    let config = TimeoutConfig::default().for_site_type(SiteType::Zai);

    assert_eq!(config.phase1_secs, 30);
    assert_eq!(config.phase2_secs, 60);
    assert_eq!(config.phase3_secs, 45);
    assert!(config.has_stuck_detection());
}

#[test]
fn test_e2e_timeout_config_backward_compatibility() {
    let config = TimeoutConfig::from_timeout_secs(300);

    assert!(!config.has_stuck_detection());
    assert_eq!(config.stuck_threshold_secs, 0);
    assert_eq!(config.phase1_secs, 60);
    assert_eq!(config.phase2_secs, 300);
}

#[test]
fn test_e2e_timeout_config_with_site_type_and_stuck_threshold() {
    let config = TimeoutConfig::new(10, 90, 30)
        .with_stuck_threshold(120)
        .for_site_type(SiteType::DeepSeek);

    assert_eq!(config.phase1_secs, 30);
    assert_eq!(config.phase2_secs, 90);
    assert_eq!(config.phase3_secs, 30);
    assert_eq!(config.stuck_threshold_secs, 120);
    assert!(config.has_stuck_detection());
}

#[test]
fn test_e2e_multi_tab_timeout_configuration() {
    let base = TimeoutConfig::new(10, 120, 45).with_stuck_threshold(200);

    let tab0 = base.for_site_type(SiteType::DeepSeek);
    let tab1 = base.for_site_type(SiteType::Zai);
    let tab2 = base.for_site_type(SiteType::Kimi);

    assert_eq!(tab0.phase1_secs, 30);
    assert_eq!(tab1.phase1_secs, 30);
    assert_eq!(tab2.phase1_secs, 20);

    assert_eq!(tab0.stuck_threshold_secs, 200);
    assert_eq!(tab1.stuck_threshold_secs, 200);
    assert_eq!(tab2.stuck_threshold_secs, 200);
}

// ============================================================================
//  TimeoutConfig + FailoverChatClient 集成
// ============================================================================

/// 模拟多标签页 failover 场景中的超时配置
#[test]
fn test_failover_timeout_progression() {
    let configs: Vec<TimeoutConfig> = [SiteType::DeepSeek, SiteType::Zai, SiteType::Kimi]
        .iter()
        .map(|&site| TimeoutConfig::new(10, 120, 45).for_site_type(site))
        .collect();

    assert_eq!(configs[0].phase1_secs, 30);
    assert_eq!(configs[1].phase1_secs, 30);
    assert_eq!(configs[2].phase1_secs, 20);

    for config in &configs {
        assert_eq!(config.phase2_secs, 120);
        assert_eq!(config.phase3_secs, 45);
    }
}

/// 验证 failover 后的超时不会累积
#[test]
fn test_failover_timeout_no_accumulation() {
    let base = TimeoutConfig::new(10, 120, 45);

    let first = base.for_site_type(SiteType::DeepSeek);
    let second = base.for_site_type(SiteType::Zai);

    assert_eq!(first.total_max_secs(), second.total_max_secs());
    assert_eq!(first.total_max_secs(), 30 + 120 + 45);
}

/// 验证 stuck_threshold 在 failover 场景中保持一致
#[test]
fn test_failover_stuck_threshold_consistent() {
    let base = TimeoutConfig::new(10, 120, 45).with_stuck_threshold(300);

    for site in [SiteType::DeepSeek, SiteType::Zai, SiteType::Kimi] {
        let config = base.for_site_type(site);
        assert_eq!(
            config.stuck_threshold_secs, 300,
            "stuck_threshold should be consistent across sites for {:?}",
            site
        );
    }
}

// ============================================================================
//  TimeoutConfig 深度对比测试
// ============================================================================

#[test]
fn test_for_site_type_deepseek_vs_kimi_threshold_difference() {
    let config = TimeoutConfig::new(10, 60, 45);
    let deepseek = config.for_site_type(SiteType::DeepSeek);
    let kimi = config.for_site_type(SiteType::Kimi);

    assert!(deepseek.phase1_secs > kimi.phase1_secs);
    assert_eq!(deepseek.phase1_secs, 30);
    assert_eq!(kimi.phase1_secs, 20);
}

#[test]
fn test_for_site_type_deepseek_vs_zai_same_threshold() {
    let config = TimeoutConfig::new(10, 60, 45);
    let deepseek = config.for_site_type(SiteType::DeepSeek);
    let zai = config.for_site_type(SiteType::Zai);

    assert_eq!(deepseek.phase1_secs, zai.phase1_secs);
}

#[test]
fn test_for_site_type_kimi_tongyi_claude_same_threshold() {
    let config = TimeoutConfig::new(10, 60, 45);

    let kimi = config.for_site_type(SiteType::Kimi);
    let tongyi = config.for_site_type(SiteType::Tongyi);
    let claude = config.for_site_type(SiteType::Claude);

    assert_eq!(kimi.phase1_secs, tongyi.phase1_secs);
    assert_eq!(tongyi.phase1_secs, claude.phase1_secs);
    assert_eq!(kimi.phase1_secs, 20);
}

#[test]
fn test_for_site_type_unknown_lower_than_known() {
    let config = TimeoutConfig::new(5, 60, 45);

    let unknown = config.for_site_type(SiteType::Unknown);
    let deepseek = config.for_site_type(SiteType::DeepSeek);
    let kimi = config.for_site_type(SiteType::Kimi);

    assert!(unknown.phase1_secs < kimi.phase1_secs);
    assert!(kimi.phase1_secs < deepseek.phase1_secs);
}
