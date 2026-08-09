//! 24h 可靠性链路集成测试 (Session 65)
//!
//! 跨模块集成测试，验证完整管道:
//! Chrome 崩溃 → 连接监控 (connection_monitor)
//!   → 自动恢复 (auto_recovery)
//!   → 网站健康检查 (site_health)
//!   → 故障转移 (failover_chat)
//!
//! ## 测试场景
//!
//! 1. **单层故障**: 仅 CDP 连接异常或仅网站异常
//! 2. **多层级联故障**: CDP 连接异常 + 网站异常同时发生
//! 3. **恢复后重新故障**: 恢复成功 → 正常运行 → 再次故障
//! 4. **全链路压力测试**: 模拟 24h 多次故障/恢复循环
//! 5. **故障转移耗尽**: 所有标签页都不健康
//! 6. **报告一致性**: 跨模块统计报告协同验证

// ============================================================================
//  导入 — 四模块纯函数
// ============================================================================

use forge::auto_recovery::{
    assess_recovery_urgency, compute_backoff_schedule, compute_recovery_success_rate,
    decide_recovery_action, estimate_max_recovery_secs, format_recovery_rate, make_failed_result,
    make_success_result, recovery_efficiency, result_error, select_recovery_strategy,
    BackoffStrategy, RecoveryAction, RecoveryConfig, RecoveryStrategy, RecoveryUrgency,
};
use forge::connection_monitor::{
    calculate_monitor_success_rate, classify_connection_severity, compute_next_check_delay,
    determine_health_level, format_monitor_success_rate, format_recovery_event_line, format_uptime,
    is_chrome_crashed_status, should_trigger_recovery, ConnectionSeverity, ConnectionStatus,
    HealthLevel, MonitorConfig, RecoveryEvent,
};
use forge::failover_chat::{
    build_error_health_result, calculate_health_check_interval_elapsed,
    classify_failover_failure_reason, format_failover_failure_trace, format_switch_trace,
    should_failover_decision, update_min_response_time,
};
use forge::site_health::{
    calculate_health_rate, classify_health_severity, compute_health_check_interval,
    determine_failover_priority, format_health_rate, format_health_result_line,
    interpret_health_json, select_best_healthy_tab, should_skip_tab, HealthCheckJson,
    HealthCheckResult, HealthSeverity, SiteFailover, SiteHealthStatus,
};

// ============================================================================
//  辅助常量与函数
// ============================================================================

/// 默认最大连续失败次数 (崩溃阈值)
const MAX_CONSECUTIVE_FAILURES: u32 = 3;

/// 默认最大重试次数
const MAX_RETRIES: u32 = 10;

/// 默认心跳间隔 (秒)
const HEARTBEAT_INTERVAL: u64 = 30;

/// 默认健康检查基础间隔 (秒)
const HEALTH_CHECK_BASE_INTERVAL: u64 = 60;

/// 默认退避策略
fn default_backoff() -> BackoffStrategy {
    BackoffStrategy::new(2, 60)
}

/// 默认恢复配置
fn default_recovery_config() -> RecoveryConfig {
    RecoveryConfig::new(9222, MAX_RETRIES).with_backoff(default_backoff())
}

/// 模拟一次完整的连接检查 + 恢复决策流程
///
/// 返回 (recovery_action, health_level, recovery_strategy)
#[allow(dead_code)]
struct CheckRecoveryStep {
    /// 连接状态
    conn_status: ConnectionStatus,
    /// 连续失败次数
    consecutive_failures: u32,
    /// 恢复动作
    action: RecoveryAction,
    /// 健康等级
    health_level: HealthLevel,
    /// 恢复策略
    strategy: RecoveryStrategy,
    /// 恢复紧急度
    urgency: RecoveryUrgency,
    /// 是否触发了恢复
    triggered: bool,
    /// 下次检查延迟 (秒)
    next_delay: u64,
}

/// 执行一次完整的连接检查 → 恢复决策
fn check_and_decide_recovery(
    conn_status: &ConnectionStatus,
    consecutive_failures: u32,
    attempt: u32,
) -> CheckRecoveryStep {
    let is_connected = conn_status.is_connected();
    let health_level =
        determine_health_level(conn_status, consecutive_failures, MAX_CONSECUTIVE_FAILURES);
    let strategy = select_recovery_strategy(conn_status);
    let urgency = assess_recovery_urgency(&health_level);
    let triggered =
        should_trigger_recovery(conn_status, consecutive_failures, MAX_CONSECUTIVE_FAILURES);
    let action = decide_recovery_action(is_connected, attempt, MAX_RETRIES, &default_backoff());
    let next_delay =
        compute_next_check_delay(conn_status, consecutive_failures, HEARTBEAT_INTERVAL);

    CheckRecoveryStep {
        conn_status: conn_status.clone(),
        consecutive_failures,
        action,
        health_level,
        strategy,
        urgency,
        triggered,
        next_delay,
    }
}

/// 模拟网站健康检查 → 故障转移决策流程
#[allow(dead_code)]
struct HealthFailoverStep {
    /// 网站健康状态
    site_status: SiteHealthStatus,
    /// 健康严重程度
    severity: HealthSeverity,
    /// 是否需要故障转移
    should_failover: bool,
    /// 故障转移优先级
    priority: u8,
    /// 下次健康检查间隔 (秒)
    next_check_interval: u64,
    /// 是否需要立即故障转移
    requires_immediate: bool,
}

/// 执行一次网站健康检查 → 故障转移决策
fn check_and_decide_failover(site_status: &SiteHealthStatus) -> HealthFailoverStep {
    let severity = classify_health_severity(site_status);
    let should_failover = should_failover_decision(&HealthCheckResult::new(site_status.clone()));
    let priority = determine_failover_priority(site_status);
    let next_check_interval =
        compute_health_check_interval(site_status, HEALTH_CHECK_BASE_INTERVAL);
    let requires_immediate = severity.requires_immediate_failover();

    HealthFailoverStep {
        site_status: site_status.clone(),
        severity,
        should_failover,
        priority,
        next_check_interval,
        requires_immediate,
    }
}

// ============================================================================
//  场景 1: 单层 CDP 连接故障 → 自动恢复
// ============================================================================

#[test]
fn test_single_layer_chrome_crash_recovery() {
    // 场景: Chrome 崩溃 (ChromeUnreachable) → 检测 → 恢复

    // 第 1 次检查: Chrome 不可达
    let step1 = check_and_decide_recovery(&ConnectionStatus::ChromeUnreachable, 1, 0);
    assert_eq!(step1.health_level, HealthLevel::Critical);
    assert_eq!(step1.strategy, RecoveryStrategy::ChromeRestart);
    assert_eq!(step1.urgency, RecoveryUrgency::Critical);
    assert!(step1.triggered);
    assert!(step1.urgency.requires_immediate_recovery());
    // 第 1 次失败 → Retry
    assert!(matches!(
        step1.action,
        RecoveryAction::Retry {
            next_attempt: 1,
            ..
        }
    ));

    // 第 2 次检查: 仍然不可达
    let step2 = check_and_decide_recovery(&ConnectionStatus::ChromeUnreachable, 2, 1);
    assert_eq!(step2.health_level, HealthLevel::Critical);
    assert!(matches!(
        step2.action,
        RecoveryAction::Retry {
            next_attempt: 2,
            ..
        }
    ));

    // 第 3 次检查: 达到崩溃阈值
    let step3 = check_and_decide_recovery(&ConnectionStatus::ChromeUnreachable, 3, 2);
    assert!(is_chrome_crashed_status(3, MAX_CONSECUTIVE_FAILURES));
    assert_eq!(step3.health_level, HealthLevel::Critical);

    // 第 4 次检查: Chrome 恢复
    let step4 = check_and_decide_recovery(&ConnectionStatus::Connected, 0, 3);
    assert_eq!(step4.health_level, HealthLevel::Healthy);
    assert_eq!(step4.strategy, RecoveryStrategy::None);
    assert_eq!(step4.urgency, RecoveryUrgency::None);
    assert!(!step4.triggered);
    assert!(matches!(
        step4.action,
        RecoveryAction::Succeed { attempts: 3 }
    ));
}

#[test]
fn test_single_layer_tab_closed_recovery() {
    // 场景: 标签页关闭 → 简单重试 → 恢复
    let step1 = check_and_decide_recovery(&ConnectionStatus::TabClosed, 1, 0);
    assert_eq!(step1.health_level, HealthLevel::Degraded);
    assert_eq!(step1.strategy, RecoveryStrategy::SimpleRetry);
    assert_eq!(step1.urgency, RecoveryUrgency::Low);
    assert!(step1.triggered);

    // 恢复
    let step2 = check_and_decide_recovery(&ConnectionStatus::Connected, 0, 1);
    assert_eq!(step2.health_level, HealthLevel::Healthy);
    assert!(matches!(
        step2.action,
        RecoveryAction::Succeed { attempts: 1 }
    ));
}

#[test]
fn test_single_layer_websocket_error_recovery() {
    // 场景: WebSocket 异常 → WebSocket 重连 → 恢复
    let step1 = check_and_decide_recovery(
        &ConnectionStatus::WebSocketError("connection reset".to_string()),
        1,
        0,
    );
    assert_eq!(step1.health_level, HealthLevel::Degraded);
    assert_eq!(step1.strategy, RecoveryStrategy::WebSocketReconnect);
    assert_eq!(step1.urgency, RecoveryUrgency::Low);

    // 恢复
    let step2 = check_and_decide_recovery(&ConnectionStatus::Connected, 0, 1);
    assert!(matches!(
        step2.action,
        RecoveryAction::Succeed { attempts: 1 }
    ));
}

#[test]
fn test_single_layer_check_timeout_recovery() {
    // 场景: 检查超时 → 简单重试 → 恢复
    let step1 = check_and_decide_recovery(&ConnectionStatus::CheckTimeout, 1, 0);
    assert_eq!(step1.health_level, HealthLevel::Degraded);
    assert_eq!(step1.strategy, RecoveryStrategy::SimpleRetry);

    // 多次超时但未达到崩溃阈值
    let step2 = check_and_decide_recovery(&ConnectionStatus::CheckTimeout, 2, 1);
    assert_eq!(step2.health_level, HealthLevel::Degraded);
    assert!(!is_chrome_crashed_status(2, MAX_CONSECUTIVE_FAILURES));

    // 恢复
    let step3 = check_and_decide_recovery(&ConnectionStatus::Connected, 0, 2);
    assert!(matches!(
        step3.action,
        RecoveryAction::Succeed { attempts: 2 }
    ));
}

// ============================================================================
//  场景 2: 单层网站故障 → 故障转移
// ============================================================================

#[test]
fn test_single_layer_rate_limited_failover() {
    // 场景: 网站限流 → 故障转移到备用标签页
    let step = check_and_decide_failover(&SiteHealthStatus::RateLimited);
    assert_eq!(step.severity, HealthSeverity::Warning);
    assert!(step.should_failover);
    assert!(!step.requires_immediate); // 限流不是 Critical
    assert_eq!(step.priority, 3);
    // 限流时缩短检查间隔
    assert_eq!(step.next_check_interval, 15); // 60 * 0.25

    // 故障转移决策
    let health = HealthCheckResult::new(SiteHealthStatus::RateLimited);
    assert!(should_failover_decision(&health));

    // 选择最佳标签页
    let results = vec![
        (0, HealthCheckResult::new(SiteHealthStatus::RateLimited)),
        (1, HealthCheckResult::new(SiteHealthStatus::Healthy)),
    ];
    assert_eq!(select_best_healthy_tab(&results), Some(1));
}

#[test]
fn test_single_layer_not_logged_in_failover() {
    let step = check_and_decide_failover(&SiteHealthStatus::NotLoggedIn);
    assert_eq!(step.severity, HealthSeverity::Warning);
    assert!(step.should_failover);
    assert_eq!(step.priority, 2);
    assert_eq!(step.next_check_interval, 30); // 60 * 0.5
}

#[test]
fn test_single_layer_under_maintenance_failover() {
    let step = check_and_decide_failover(&SiteHealthStatus::UnderMaintenance);
    assert_eq!(step.severity, HealthSeverity::Critical);
    assert!(step.should_failover);
    assert!(step.requires_immediate); // 维护是 Critical
    assert_eq!(step.priority, 5);
    assert_eq!(step.next_check_interval, 120); // 60 * 2.0
}

#[test]
fn test_single_layer_network_error_failover() {
    let step = check_and_decide_failover(&SiteHealthStatus::NetworkError);
    assert_eq!(step.severity, HealthSeverity::Critical);
    assert!(step.should_failover);
    assert!(step.requires_immediate);
    assert_eq!(step.priority, 4);
    assert_eq!(step.next_check_interval, 15); // 60 * 0.25

    // 用 build_error_health_result 构建网络错误结果
    let error_result = build_error_health_result("CDP connection lost".to_string());
    assert_eq!(error_result.status, SiteHealthStatus::NetworkError);
    assert!(should_failover_decision(&error_result));
}

#[test]
fn test_single_layer_unknown_no_failover() {
    // Unknown 状态不触发故障转移
    let step = check_and_decide_failover(&SiteHealthStatus::Unknown);
    assert_eq!(step.severity, HealthSeverity::Unknown);
    assert!(!step.should_failover);
    assert_eq!(step.priority, 1);
    assert_eq!(step.next_check_interval, 30); // 60 * 0.5
}

// ============================================================================
//  场景 3: 多层级联故障 (CDP + 网站同时异常)
// ============================================================================

#[test]
fn test_cascading_chrome_crash_and_site_maintenance() {
    // 场景: Chrome 崩溃 + 网站维护中 → 双重 Critical

    // CDP 层
    let conn_step = check_and_decide_recovery(&ConnectionStatus::ChromeUnreachable, 3, 2);
    assert_eq!(conn_step.health_level, HealthLevel::Critical);
    assert_eq!(conn_step.strategy, RecoveryStrategy::ChromeRestart);
    assert_eq!(conn_step.urgency, RecoveryUrgency::Critical);

    // 网站层 (假设 Chrome 恢复后检查网站)
    let site_step = check_and_decide_failover(&SiteHealthStatus::UnderMaintenance);
    assert_eq!(site_step.severity, HealthSeverity::Critical);
    assert!(site_step.requires_immediate);

    // 两层都是 Critical → 需要同时恢复 + 故障转移
    assert!(conn_step.urgency.requires_immediate_recovery());
    assert!(site_step.requires_immediate);

    // 严重程度一致: CDP Critical = 网站 Critical
    let conn_severity = classify_connection_severity(&ConnectionStatus::ChromeUnreachable);
    let site_severity = classify_health_severity(&SiteHealthStatus::UnderMaintenance);
    assert_eq!(conn_severity, ConnectionSeverity::Critical);
    assert_eq!(site_severity, HealthSeverity::Critical);
}

#[test]
fn test_cascading_websocket_error_and_rate_limited() {
    // 场景: WebSocket 异常 + 网站限流 → Degraded + Warning

    // CDP 层: WebSocket 异常
    let conn_step = check_and_decide_recovery(
        &ConnectionStatus::WebSocketError("ws closed".to_string()),
        1,
        0,
    );
    assert_eq!(conn_step.health_level, HealthLevel::Degraded);
    assert_eq!(conn_step.urgency, RecoveryUrgency::Low);

    // 网站层: 限流
    let site_step = check_and_decide_failover(&SiteHealthStatus::RateLimited);
    assert_eq!(site_step.severity, HealthSeverity::Warning);
    assert!(!site_step.requires_immediate);

    // CDP 层需要恢复但不是最紧急, 网站层需要故障转移
    assert!(conn_step.triggered);
    assert!(site_step.should_failover);
}

#[test]
fn test_cascading_tab_closed_and_not_logged_in() {
    // 场景: 标签页关闭 + 网站未登录 → Degraded + Warning

    let conn_step = check_and_decide_recovery(&ConnectionStatus::TabClosed, 1, 0);
    assert_eq!(conn_step.health_level, HealthLevel::Degraded);

    let site_step = check_and_decide_failover(&SiteHealthStatus::NotLoggedIn);
    assert_eq!(site_step.severity, HealthSeverity::Warning);

    // 两者都是可恢复的
    assert_eq!(conn_step.strategy, RecoveryStrategy::SimpleRetry);
    assert!(site_step.should_failover);
}

#[test]
fn test_cascading_all_normal() {
    // 场景: CDP 连接正常 + 网站健康 → 全部正常
    let conn_step = check_and_decide_recovery(&ConnectionStatus::Connected, 0, 0);
    assert_eq!(conn_step.health_level, HealthLevel::Healthy);
    assert_eq!(conn_step.strategy, RecoveryStrategy::None);
    assert_eq!(conn_step.urgency, RecoveryUrgency::None);
    assert!(!conn_step.triggered);

    let site_step = check_and_decide_failover(&SiteHealthStatus::Healthy);
    assert_eq!(site_step.severity, HealthSeverity::Info);
    assert!(!site_step.should_failover);
    assert_eq!(site_step.next_check_interval, 60); // 正常间隔
}

// ============================================================================
//  场景 4: 恢复后重新故障
// ============================================================================

#[test]
fn test_recovery_then_refailure_cycle() {
    // 模拟: 正常 → 崩溃 → 恢复 → 正常 → 再崩溃 → 再恢复

    // 阶段 1: 正常运行
    let phase1 = check_and_decide_recovery(&ConnectionStatus::Connected, 0, 0);
    assert_eq!(phase1.health_level, HealthLevel::Healthy);

    // 阶段 2: 第一次崩溃
    let phase2 = check_and_decide_recovery(&ConnectionStatus::ChromeUnreachable, 1, 0);
    assert_eq!(phase2.health_level, HealthLevel::Critical);
    assert!(matches!(phase2.action, RecoveryAction::Retry { .. }));

    // 阶段 3: 恢复
    let phase3 = check_and_decide_recovery(&ConnectionStatus::Connected, 0, 1);
    assert_eq!(phase3.health_level, HealthLevel::Healthy);
    assert!(matches!(
        phase3.action,
        RecoveryAction::Succeed { attempts: 1 }
    ));

    // 阶段 4: 再次正常运行
    let phase4 = check_and_decide_recovery(&ConnectionStatus::Connected, 0, 0);
    assert_eq!(phase4.health_level, HealthLevel::Healthy);

    // 阶段 5: 再次崩溃
    let phase5 = check_and_decide_recovery(&ConnectionStatus::ChromeUnreachable, 1, 0);
    assert_eq!(phase5.health_level, HealthLevel::Critical);
    assert!(phase5.triggered);

    // 阶段 6: 再次恢复
    let phase6 = check_and_decide_recovery(&ConnectionStatus::Connected, 0, 1);
    assert!(matches!(
        phase6.action,
        RecoveryAction::Succeed { attempts: 1 }
    ));

    // 验证: 恢复效率
    // 第一次恢复用了 1 次重试, 第二次也用了 1 次
    let efficiency1 = recovery_efficiency(1, MAX_RETRIES);
    let efficiency2 = recovery_efficiency(1, MAX_RETRIES);
    assert!((efficiency1 - efficiency2).abs() < 0.001);
    assert!((efficiency1 - 0.9).abs() < 0.01); // 1/10 → 0.9 efficiency
}

#[test]
fn test_site_failover_then_recovery_then_refailover() {
    // 模拟: 网站正常 → 限流 → 切换到备用 → 备用恢复 → 主站恢复 → 主站再限流

    // 初始: 两个标签页都健康
    let results1 = vec![
        (0, HealthCheckResult::new(SiteHealthStatus::Healthy)),
        (1, HealthCheckResult::new(SiteHealthStatus::Healthy)),
    ];
    assert_eq!(select_best_healthy_tab(&results1), Some(0)); // 选第一个

    // 主站限流
    let results2 = vec![
        (0, HealthCheckResult::new(SiteHealthStatus::RateLimited)),
        (1, HealthCheckResult::new(SiteHealthStatus::Healthy)),
    ];
    assert_eq!(select_best_healthy_tab(&results2), Some(1)); // 切换到标签页 1

    // 主站恢复, 备站限流
    let results3 = vec![
        (0, HealthCheckResult::new(SiteHealthStatus::Healthy)),
        (1, HealthCheckResult::new(SiteHealthStatus::RateLimited)),
    ];
    assert_eq!(select_best_healthy_tab(&results3), Some(0)); // 切回标签页 0

    // 主站再次限流
    let results4 = vec![
        (0, HealthCheckResult::new(SiteHealthStatus::RateLimited)),
        (1, HealthCheckResult::new(SiteHealthStatus::Healthy)),
    ];
    assert_eq!(select_best_healthy_tab(&results4), Some(1)); // 再切换到标签页 1

    // 验证: 故障转移决策一致
    assert!(should_failover_decision(&results2[0].1));
    assert!(!should_failover_decision(&results3[0].1));
    assert!(should_failover_decision(&results4[0].1));
}

// ============================================================================
//  场景 5: 全链路压力测试 — 多次故障/恢复循环
// ============================================================================

#[test]
fn test_full_chain_stress_24h_simulation() {
    // 模拟 24h 运行中的多次故障/恢复循环
    // 每 3 小时发生一次故障, 共 8 次故障

    let mut total_checks: u64 = 0;
    let mut total_failures: u64 = 0;
    let mut total_recoveries: u64 = 0;
    let mut total_recovery_successes: u64 = 0;
    let mut total_health_checks: u64 = 0;
    let mut total_healthy: u64 = 0;

    let failure_scenarios = [
        ConnectionStatus::ChromeUnreachable,
        ConnectionStatus::TabClosed,
        ConnectionStatus::WebSocketError("stress test error".to_string()),
        ConnectionStatus::CheckTimeout,
    ];

    let site_failure_scenarios = [
        SiteHealthStatus::RateLimited,
        SiteHealthStatus::NotLoggedIn,
        SiteHealthStatus::NetworkError,
        SiteHealthStatus::UnderMaintenance,
    ];

    for cycle in 0..8u32 {
        // 正常运行阶段 (每 3 小时约 360 次检查)
        for _ in 0..360 {
            total_checks += 1;
            total_health_checks += 1;
            total_healthy += 1;

            // CDP 正常
            let step = check_and_decide_recovery(&ConnectionStatus::Connected, 0, 0);
            assert_eq!(step.health_level, HealthLevel::Healthy);

            // 网站正常
            let site_step = check_and_decide_failover(&SiteHealthStatus::Healthy);
            assert!(!site_step.should_failover);
        }

        // 故障阶段
        let conn_failure = &failure_scenarios[cycle as usize % failure_scenarios.len()];
        let site_failure = &site_failure_scenarios[cycle as usize % site_failure_scenarios.len()];

        // 连续失败直到恢复
        let mut attempt = 0u32;
        let mut consecutive_failures = 0u32;

        loop {
            total_checks += 1;
            total_health_checks += 1;

            let step = check_and_decide_recovery(conn_failure, consecutive_failures, attempt);

            if !step.conn_status.is_connected() {
                total_failures += 1;
                consecutive_failures += 1;
            }

            match step.action {
                RecoveryAction::Retry {
                    next_attempt,
                    delay_secs: _,
                } => {
                    attempt = next_attempt;
                    // 继续重试
                }
                RecoveryAction::Succeed { .. } => {
                    // 恢复成功
                    total_recoveries += 1;
                    total_recovery_successes += 1;
                    break;
                }
                RecoveryAction::GiveUp { .. } => {
                    // 恢复失败 (不应该在压力测试中发生, max_retries=10)
                    total_recoveries += 1;
                    break;
                }
            }

            // 模拟恢复成功 (在第 attempt 次重试后恢复)
            if attempt >= 3 {
                // 检查恢复后状态
                let recovered = check_and_decide_recovery(&ConnectionStatus::Connected, 0, attempt);
                assert_eq!(recovered.health_level, HealthLevel::Healthy);
                assert!(matches!(recovered.action, RecoveryAction::Succeed { .. }));
                total_recoveries += 1;
                total_recovery_successes += 1;
                break;
            }
        }

        // 网站故障检查
        let site_step = check_and_decide_failover(site_failure);
        if site_step.should_failover {
            // 选择最佳标签页
            let results = vec![
                (0, HealthCheckResult::new(site_failure.clone())),
                (1, HealthCheckResult::new(SiteHealthStatus::Healthy)),
            ];
            let best = select_best_healthy_tab(&results);
            assert_eq!(best, Some(1));
        }
    }

    // 验证统计数据
    let monitor_rate = calculate_monitor_success_rate(total_checks, total_failures);
    let recovery_rate = compute_recovery_success_rate(total_recoveries, total_recovery_successes);
    let health_rate = calculate_health_rate(total_health_checks, total_healthy);

    // 8 次故障, 每次 ~3 次失败 → 总失败 ~24, 总检查 ~2880+24
    assert!(
        monitor_rate > 0.98,
        "监控成功率应 > 98%, 实际: {}",
        monitor_rate
    );
    assert!(
        recovery_rate > 0.99,
        "恢复成功率应 > 99%, 实际: {}",
        recovery_rate
    );
    assert!(health_rate > 0.98, "健康率应 > 98%, 实际: {}", health_rate);

    // 报告格式化不 panic
    let _monitor_report = format_monitor_success_rate(monitor_rate);
    let _recovery_report = format_recovery_rate(recovery_rate);
    let _health_report = format_health_rate(health_rate);
    let _uptime = format_uptime(86400); // 24h in seconds
}

#[test]
fn test_stress_many_recovery_cycles() {
    // 模拟 100 次恢复循环, 验证退避策略一致性
    let backoff = default_backoff();
    let schedule = compute_backoff_schedule(&backoff, MAX_RETRIES);

    // 验证退避计划正确
    assert_eq!(schedule.len(), MAX_RETRIES as usize);
    assert!(schedule.iter().all(|&d| d > 0));

    for cycle in 0..100u32 {
        let attempt = cycle % MAX_RETRIES;
        let is_connected = cycle % 5 == 0; // 每 5 次恢复一次

        let action = decide_recovery_action(is_connected, attempt, MAX_RETRIES, &backoff);

        if is_connected {
            assert!(matches!(action, RecoveryAction::Succeed { .. }));
        } else if attempt < MAX_RETRIES {
            assert!(matches!(action, RecoveryAction::Retry { .. }));
        } else {
            assert!(matches!(action, RecoveryAction::GiveUp { .. }));
        }
    }
}

// ============================================================================
//  场景 6: 故障转移耗尽 — 所有标签页不健康
// ============================================================================

#[test]
fn test_failover_exhaustion_all_tabs_unhealthy() {
    // 场景: 所有标签页都不健康

    let results = vec![
        (0, HealthCheckResult::new(SiteHealthStatus::RateLimited)),
        (
            1,
            HealthCheckResult::new(SiteHealthStatus::UnderMaintenance),
        ),
        (2, HealthCheckResult::new(SiteHealthStatus::NetworkError)),
    ];

    // 没有健康标签页 → 返回优先级最低的
    let best = select_best_healthy_tab(&results);
    assert_eq!(best, Some(0)); // RateLimited (priority 3) 最低

    // 所有标签页都应触发故障转移决策
    for (_, result) in &results {
        assert!(should_failover_decision(result));
    }

    // 故障转移策略: 尝试所有标签页后放弃
    let mut failover = SiteFailover::new(vec![0, 1, 2], 0).with_max_failures(3);

    // 尝试切换
    let health0 = HealthCheckResult::new(SiteHealthStatus::RateLimited);
    let switch1 = failover.should_switch(&health0);
    assert!(switch1.is_some());

    // 模拟所有标签页都尝试过
    failover.consecutive_failures = 3;
    failover.tried_tabs = vec![0, 1, 2];

    // 分类失败原因
    let reason = classify_failover_failure_reason(true, 3, 3);
    assert_eq!(reason, "所有标签页都已尝试");

    let reason2 = classify_failover_failure_reason(false, 3, 3);
    assert_eq!(reason2, "超过最大连续失败次数");

    let reason3 = classify_failover_failure_reason(false, 0, 3);
    assert_eq!(reason3, "无可用标签页");
}

#[test]
fn test_failover_exhaustion_should_skip_tab() {
    // 验证: 连续失败超过阈值的标签页应被跳过
    assert!(!should_skip_tab(0, 3));
    assert!(!should_skip_tab(1, 3));
    assert!(!should_skip_tab(2, 3));
    assert!(should_skip_tab(3, 3));
    assert!(should_skip_tab(5, 3));
}

#[test]
fn test_failover_exhaustion_all_network_error() {
    // 场景: 所有标签页都是网络错误 → 全部 Critical
    let results = vec![
        (0, HealthCheckResult::new(SiteHealthStatus::NetworkError)),
        (1, HealthCheckResult::new(SiteHealthStatus::NetworkError)),
        (2, HealthCheckResult::new(SiteHealthStatus::NetworkError)),
    ];

    for (_, result) in &results {
        let severity = classify_health_severity(&result.status);
        assert_eq!(severity, HealthSeverity::Critical);
        assert!(severity.requires_immediate_failover());
    }

    // 全部优先级相同 → 返回第一个
    let best = select_best_healthy_tab(&results);
    assert_eq!(best, Some(0));
}

// ============================================================================
//  场景 7: 完整管道模拟 — monitor → recovery → health → failover
// ============================================================================

#[test]
fn test_complete_pipeline_chrome_crash_then_site_failover() {
    // 完整管道:
    // 1. Chrome 崩溃 → connection_monitor 检测
    // 2. auto_recovery 重试 → 恢复成功
    // 3. 恢复后检查网站 → 发现网站限流
    // 4. failover_chat 决策 → 切换到备用标签页

    // Step 1: Chrome 崩溃, 检测到
    let crash_step = check_and_decide_recovery(&ConnectionStatus::ChromeUnreachable, 1, 0);
    assert_eq!(crash_step.health_level, HealthLevel::Critical);
    assert_eq!(crash_step.strategy, RecoveryStrategy::ChromeRestart);

    // Step 2: 恢复重试
    let mut recovery_attempt = 0u32;
    let backoff = default_backoff();
    loop {
        let action = decide_recovery_action(false, recovery_attempt, MAX_RETRIES, &backoff);
        match action {
            RecoveryAction::Retry {
                next_attempt,
                delay_secs,
            } => {
                assert!(delay_secs > 0);
                recovery_attempt = next_attempt;
                // 模拟第 3 次重试后 Chrome 恢复
                if recovery_attempt >= 3 {
                    let success =
                        decide_recovery_action(true, recovery_attempt, MAX_RETRIES, &backoff);
                    assert!(matches!(success, RecoveryAction::Succeed { .. }));
                    break;
                }
            }
            RecoveryAction::Succeed { .. } => break,
            RecoveryAction::GiveUp { .. } => panic!("不应放弃"),
        }
    }

    // Step 3: Chrome 恢复后, 检查网站健康
    let conn_recovered =
        check_and_decide_recovery(&ConnectionStatus::Connected, 0, recovery_attempt);
    assert_eq!(conn_recovered.health_level, HealthLevel::Healthy);

    // 网站限流
    let site_step = check_and_decide_failover(&SiteHealthStatus::RateLimited);
    assert!(site_step.should_failover);

    // Step 4: 故障转移决策
    let health = HealthCheckResult::new(SiteHealthStatus::RateLimited);
    assert!(should_failover_decision(&health));

    // 切换到健康标签页
    let results = vec![
        (0, HealthCheckResult::new(SiteHealthStatus::RateLimited)),
        (1, HealthCheckResult::new(SiteHealthStatus::Healthy)),
    ];
    let best = select_best_healthy_tab(&results);
    assert_eq!(best, Some(1));

    // 生成 trace 消息
    let trace_msg = format_switch_trace(
        0,
        forge::browser::SiteType::Zai,
        1,
        forge::browser::SiteType::DeepSeek,
    );
    assert!(trace_msg.contains("Z.ai"));
    assert!(trace_msg.contains("DeepSeek"));

    // 恢复结果
    let recovery_result = make_success_result(recovery_attempt, 5000);
    assert!(recovery_result.is_success());
    assert_eq!(recovery_result.attempts(), recovery_attempt);
}

#[test]
fn test_complete_pipeline_giveup_then_all_critical() {
    // 修正版: 放弃恢复 + 所有标签页 Critical

    // Step 1: 放弃恢复
    let backoff = default_backoff();
    let giveup_action = decide_recovery_action(false, MAX_RETRIES, MAX_RETRIES, &backoff);
    assert!(matches!(giveup_action, RecoveryAction::GiveUp { .. }));

    let failed_result = make_failed_result(
        MAX_RETRIES,
        60000,
        ConnectionStatus::ChromeUnreachable,
        "Chrome 未恢复",
    );
    assert!(failed_result.is_failed());

    // Step 2: 所有标签页不健康
    let results = vec![
        (
            0,
            HealthCheckResult::new(SiteHealthStatus::UnderMaintenance),
        ),
        (1, HealthCheckResult::new(SiteHealthStatus::NetworkError)),
    ];

    // 两者都是 Critical
    for (_, result) in &results {
        assert_eq!(
            classify_health_severity(&result.status),
            HealthSeverity::Critical
        );
        assert!(should_failover_decision(result));
    }

    // 选择最佳 → 优先级最低的 (NetworkError=4 < UnderMaintenance=5)
    let best = select_best_healthy_tab(&results);
    assert_eq!(best, Some(1)); // NetworkError 优先级更低 = 更好

    // 故障转移失败原因
    let reason = classify_failover_failure_reason(true, 0, 3);
    assert_eq!(reason, "所有标签页都已尝试");

    // 失败 trace
    let fail_trace = format_failover_failure_trace(0, forge::browser::SiteType::Zai);
    assert!(fail_trace.contains("Z.ai"));
}

// ============================================================================
//  场景 8: 跨模块报告一致性
// ============================================================================

#[test]
fn test_cross_module_report_consistency() {
    // 验证: 四个模块的统计报告在相同数据下产生一致结果

    // 模拟: 1000 次检查, 50 次失败, 40 次恢复 (35 成功), 200 次健康检查 (180 健康)
    let total_checks = 1000u64;
    let total_failures = 50u64;
    let total_recoveries = 40u64;
    let total_recovery_successes = 35u64;
    let total_health_checks = 200u64;
    let total_healthy = 180u64;

    // connection_monitor 成功率
    let monitor_rate = calculate_monitor_success_rate(total_checks, total_failures);
    let monitor_report = format_monitor_success_rate(monitor_rate);
    assert!(monitor_report.contains('%'));
    assert!((monitor_rate - 0.95).abs() < 0.001); // 950/1000 = 0.95

    // auto_recovery 成功率
    let recovery_rate = compute_recovery_success_rate(total_recoveries, total_recovery_successes);
    let recovery_report = format_recovery_rate(recovery_rate);
    assert!(recovery_report.contains('%'));
    assert!((recovery_rate - 0.875).abs() < 0.001); // 35/40 = 0.875

    // site_health 健康率
    let health_rate = calculate_health_rate(total_health_checks, total_healthy);
    let health_report = format_health_rate(health_rate);
    assert!(health_report.contains('%'));
    assert!((health_rate - 0.9).abs() < 0.001); // 180/200 = 0.9

    // 验证: 格式化函数都产生百分比格式
    assert!(monitor_report.ends_with('%'));
    assert!(recovery_report.ends_with('%'));
    assert!(health_report.ends_with('%'));

    // 验证: 运行时间格式化
    let uptime_1h = format_uptime(3600);
    let uptime_24h = format_uptime(86400);
    assert!(uptime_1h.contains("1.0h"));
    assert!(uptime_24h.contains("24.0h"));
}

#[test]
fn test_cross_module_severity_mapping_consistency() {
    // 验证: 严重程度映射在四个模块间一致

    // ConnectionSeverity → HealthLevel → RecoveryUrgency 映射一致性

    // 正常状态
    let conn_status = ConnectionStatus::Connected;
    let conn_severity = classify_connection_severity(&conn_status);
    let health_level = determine_health_level(&conn_status, 0, MAX_CONSECUTIVE_FAILURES);
    let urgency = assess_recovery_urgency(&health_level);
    assert_eq!(conn_severity, ConnectionSeverity::Info);
    assert_eq!(health_level, HealthLevel::Healthy);
    assert_eq!(urgency, RecoveryUrgency::None);

    // 轻微异常 (TabClosed)
    let conn_status = ConnectionStatus::TabClosed;
    let conn_severity = classify_connection_severity(&conn_status);
    let health_level = determine_health_level(&conn_status, 1, MAX_CONSECUTIVE_FAILURES);
    let urgency = assess_recovery_urgency(&health_level);
    assert_eq!(conn_severity, ConnectionSeverity::Warning);
    assert_eq!(health_level, HealthLevel::Degraded);
    assert_eq!(urgency, RecoveryUrgency::Low);

    // 严重异常 (ChromeUnreachable)
    let conn_status = ConnectionStatus::ChromeUnreachable;
    let conn_severity = classify_connection_severity(&conn_status);
    let health_level = determine_health_level(&conn_status, 3, MAX_CONSECUTIVE_FAILURES);
    let urgency = assess_recovery_urgency(&health_level);
    assert_eq!(conn_severity, ConnectionSeverity::Critical);
    assert_eq!(health_level, HealthLevel::Critical);
    assert_eq!(urgency, RecoveryUrgency::Critical);

    // 网站层: HealthSeverity 映射
    for status in [
        SiteHealthStatus::Healthy,
        SiteHealthStatus::NotLoggedIn,
        SiteHealthStatus::RateLimited,
        SiteHealthStatus::UnderMaintenance,
        SiteHealthStatus::NetworkError,
        SiteHealthStatus::Unknown,
    ] {
        let severity = classify_health_severity(&status);
        let priority = determine_failover_priority(&status);
        let interval = compute_health_check_interval(&status, HEALTH_CHECK_BASE_INTERVAL);

        // 验证: 严重程度与优先级一致 (越严重优先级越高)
        match severity {
            HealthSeverity::Info => {
                assert_eq!(priority, 0);
                assert!(!should_failover_decision(&HealthCheckResult::new(
                    status.clone()
                )));
                assert_eq!(interval, 60);
            }
            HealthSeverity::Warning => {
                assert!((2..=3).contains(&priority));
                assert!(should_failover_decision(&HealthCheckResult::new(
                    status.clone()
                )));
            }
            HealthSeverity::Critical => {
                assert!(priority >= 4);
                assert!(should_failover_decision(&HealthCheckResult::new(
                    status.clone()
                )));
                assert!(severity.requires_immediate_failover());
            }
            HealthSeverity::Unknown => {
                assert_eq!(priority, 1);
                assert!(!should_failover_decision(&HealthCheckResult::new(
                    status.clone()
                )));
            }
        }
    }
}

// ============================================================================
//  场景 9: 健康检查间隔与退避策略协同
// ============================================================================

#[test]
fn test_timing_consistency_across_modules() {
    // 验证: 各模块的时间策略在 24h 运行中协同工作

    // 1. connection_monitor: 心跳间隔随失败次数缩短
    let normal_delay =
        compute_next_check_delay(&ConnectionStatus::Connected, 0, HEARTBEAT_INTERVAL);
    let failure_delay_1 =
        compute_next_check_delay(&ConnectionStatus::TabClosed, 1, HEARTBEAT_INTERVAL);
    let failure_delay_2 =
        compute_next_check_delay(&ConnectionStatus::TabClosed, 2, HEARTBEAT_INTERVAL);
    let failure_delay_3 =
        compute_next_check_delay(&ConnectionStatus::TabClosed, 3, HEARTBEAT_INTERVAL);

    assert_eq!(normal_delay, 30); // 正常 → 完整间隔
    assert_eq!(failure_delay_1, 15); // 30 / 2
    assert_eq!(failure_delay_2, 10); // 30 / 3
    assert_eq!(failure_delay_3, 7); // 30 / 4 = 7 (整数除法)
    assert!(normal_delay >= failure_delay_1);
    assert!(failure_delay_1 >= failure_delay_2);
    assert!(failure_delay_2 >= failure_delay_3);

    // 2. site_health: 健康检查间隔随状态变化
    let healthy_interval =
        compute_health_check_interval(&SiteHealthStatus::Healthy, HEALTH_CHECK_BASE_INTERVAL);
    let rate_limited_interval =
        compute_health_check_interval(&SiteHealthStatus::RateLimited, HEALTH_CHECK_BASE_INTERVAL);
    let maintenance_interval = compute_health_check_interval(
        &SiteHealthStatus::UnderMaintenance,
        HEALTH_CHECK_BASE_INTERVAL,
    );

    assert_eq!(healthy_interval, 60);
    assert_eq!(rate_limited_interval, 15); // 缩短
    assert_eq!(maintenance_interval, 120); // 延长

    // 3. auto_recovery: 退避时间递增
    let backoff = default_backoff();
    let schedule = compute_backoff_schedule(&backoff, 5);
    assert_eq!(schedule, vec![4, 8, 16, 32, 60]); // 递增, 上限 60

    // 4. failover_chat: 健康检查间隔判断
    assert!(calculate_health_check_interval_elapsed(5, 0, 3)); // 5-0=5 >= 3 → 检查
    assert!(!calculate_health_check_interval_elapsed(2, 0, 3)); // 2-0=2 < 3 → 跳过
    assert!(calculate_health_check_interval_elapsed(10, 0, 0)); // interval=0 → 每次都检查

    // 5. 估算最大恢复时间
    let config = default_recovery_config();
    let max_recovery = estimate_max_recovery_secs(&config);
    // 10 次重试: 4+8+16+32+60+60+60+60+60+60 = 420
    assert_eq!(max_recovery, 420);
}

#[test]
fn test_recovery_event_trace_consistency() {
    // 验证: RecoveryEvent 记录与格式化函数一致

    // 成功恢复事件
    let success_event = RecoveryEvent::new(
        3600000, // 1h in ms
        ConnectionStatus::ChromeUnreachable,
        ConnectionStatus::Connected,
        "Chrome 重启恢复",
        5000,
        true,
        None,
    );
    let success_line = format_recovery_event_line(&success_event);
    assert!(success_line.contains("✅"));
    assert!(success_line.contains("Chrome 重启恢复"));
    assert!(success_line.contains("[3600s]"));

    // 失败恢复事件
    let failure_event = RecoveryEvent::new(
        7200000, // 2h in ms
        ConnectionStatus::ChromeUnreachable,
        ConnectionStatus::ChromeUnreachable,
        "Chrome 重启恢复",
        60000,
        false,
        Some("Chrome 未恢复"),
    );
    let failure_line = format_recovery_event_line(&failure_event);
    assert!(failure_line.contains("❌"));
    assert!(failure_line.contains("Chrome 重启恢复"));
    assert!(failure_line.contains("[7200s]"));
    assert!(failure_line.contains("Chrome 不可达"));
}

// ============================================================================
//  场景 10: HealthCheckJson → interpret → classify → failover 完整链路
// ============================================================================

#[test]
fn test_health_json_to_failover_complete_chain() {
    // 验证: 从 JSON 检测结果到故障转移决策的完整链路

    // 健康页面
    let healthy_json = HealthCheckJson::new().with_input(true);
    let healthy_status = interpret_health_json(&healthy_json);
    assert_eq!(healthy_status, SiteHealthStatus::Healthy);
    let healthy_severity = classify_health_severity(&healthy_status);
    assert_eq!(healthy_severity, HealthSeverity::Info);
    assert!(!should_failover_decision(&HealthCheckResult::new(
        healthy_status
    )));

    // 限流页面
    let rate_limited_json = HealthCheckJson::new()
        .with_rate_limit(true)
        .with_input(true);
    let rate_limited_status = interpret_health_json(&rate_limited_json);
    assert_eq!(rate_limited_status, SiteHealthStatus::RateLimited);
    let rate_limited_severity = classify_health_severity(&rate_limited_status);
    assert_eq!(rate_limited_severity, HealthSeverity::Warning);
    assert!(should_failover_decision(&HealthCheckResult::new(
        rate_limited_status
    )));

    // 维护中页面
    let maintenance_json = HealthCheckJson::new()
        .with_maintenance(true)
        .with_input(true);
    let maintenance_status = interpret_health_json(&maintenance_json);
    assert_eq!(maintenance_status, SiteHealthStatus::UnderMaintenance);
    let maintenance_severity = classify_health_severity(&maintenance_status);
    assert_eq!(maintenance_severity, HealthSeverity::Critical);
    assert!(maintenance_severity.requires_immediate_failover());
    assert!(should_failover_decision(&HealthCheckResult::new(
        maintenance_status
    )));

    // 未登录页面
    let not_logged_json = HealthCheckJson::new().with_login_button(true);
    let not_logged_status = interpret_health_json(&not_logged_json);
    assert_eq!(not_logged_status, SiteHealthStatus::NotLoggedIn);
    assert!(should_failover_decision(&HealthCheckResult::new(
        not_logged_status
    )));

    // 未知页面
    let unknown_json = HealthCheckJson::new();
    let unknown_status = interpret_health_json(&unknown_json);
    assert_eq!(unknown_status, SiteHealthStatus::Unknown);
    assert!(!should_failover_decision(&HealthCheckResult::new(
        unknown_status
    )));

    // 网络错误 (由 failover_chat 构建)
    let network_error_result = build_error_health_result("timeout".to_string());
    assert_eq!(network_error_result.status, SiteHealthStatus::NetworkError);
    assert!(should_failover_decision(&network_error_result));
}

#[test]
fn test_health_result_line_formatting() {
    // 验证: 健康检查结果格式化行正确
    let healthy_result = HealthCheckResult::new(SiteHealthStatus::Healthy);
    let line = format_health_result_line(0, forge::browser::SiteType::Zai, &healthy_result);
    assert!(line.contains("0"));
    assert!(line.contains("健康"));

    let rate_limited_result = HealthCheckResult::new(SiteHealthStatus::RateLimited);
    let line =
        format_health_result_line(1, forge::browser::SiteType::DeepSeek, &rate_limited_result);
    assert!(line.contains("1"));
    assert!(line.contains("限流"));
}

// ============================================================================
//  场景 11: RecoveryResult 构建与格式化
// ============================================================================

#[test]
fn test_recovery_result_success_and_failure() {
    // 成功结果
    let success = make_success_result(3, 12000);
    assert!(success.is_success());
    assert!(!success.is_failed());
    assert_eq!(success.attempts(), 3);
    assert_eq!(success.total_duration_ms(), 12000);
    let success_error = result_error(&success);
    assert!(success_error.is_empty() || success_error.contains("成功"));

    // 失败结果
    let failed = make_failed_result(
        MAX_RETRIES,
        420000,
        ConnectionStatus::ChromeUnreachable,
        "超过最大重试次数",
    );
    assert!(!failed.is_success());
    assert!(failed.is_failed());
    assert_eq!(failed.attempts(), MAX_RETRIES);
    assert_eq!(failed.total_duration_ms(), 420000);
    let failed_error = result_error(&failed);
    assert!(failed_error.contains("超过最大重试次数"));
}

#[test]
fn test_recovery_efficiency_and_max_time() {
    // 恢复效率: 1 次重试 vs 10 次重试
    let efficiency_fast = recovery_efficiency(1, MAX_RETRIES);
    let efficiency_slow = recovery_efficiency(10, MAX_RETRIES);
    assert!(efficiency_fast > efficiency_slow);
    assert!((efficiency_fast - 0.9).abs() < 0.01); // 1 - 1/10 = 0.9
    assert!((efficiency_slow - 0.0).abs() < 0.01); // 1 - 10/10 = 0.0

    // 最大恢复时间
    let config = default_recovery_config();
    let max_time = estimate_max_recovery_secs(&config);
    assert_eq!(max_time, 420); // 4+8+16+32+60*6 = 420

    // 小配置
    let small_config = RecoveryConfig::new(9222, 3).with_backoff(BackoffStrategy::new(1, 30));
    let small_max = estimate_max_recovery_secs(&small_config);
    // 2 + 4 + 8 = 14
    assert_eq!(small_max, 14);
}

// ============================================================================
//  场景 12: 性能统计与响应时间
// ============================================================================

#[test]
fn test_performance_stats_min_response_time() {
    // 验证: update_min_response_time 纯函数
    assert_eq!(update_min_response_time(0, 150), 150); // 初始
    assert_eq!(update_min_response_time(150, 100), 100); // 更小
    assert_eq!(update_min_response_time(100, 200), 100); // 更大, 不变
    assert_eq!(update_min_response_time(50, 50), 50); // 相等, 不变

    // 模拟多次响应时间更新
    let mut current_min = 0u64;
    for &duration in &[200, 150, 300, 100, 250, 80, 180] {
        current_min = update_min_response_time(current_min, duration);
    }
    assert_eq!(current_min, 80); // 最小值
}

#[test]
fn test_failover_trace_messages() {
    // 切换成功 trace
    let switch_msg = format_switch_trace(
        0,
        forge::browser::SiteType::Zai,
        1,
        forge::browser::SiteType::DeepSeek,
    );
    assert!(switch_msg.contains("[0]"));
    assert!(switch_msg.contains("Z.ai"));
    assert!(switch_msg.contains("[1]"));
    assert!(switch_msg.contains("DeepSeek"));

    // 切换失败 trace
    let fail_msg = format_failover_failure_trace(0, forge::browser::SiteType::Zai);
    assert!(fail_msg.contains("[0]"));
    assert!(fail_msg.contains("Z.ai"));
    assert!(fail_msg.contains("尝试从"));

    // 多网站切换
    let switch_msg2 = format_switch_trace(
        1,
        forge::browser::SiteType::DeepSeek,
        2,
        forge::browser::SiteType::Kimi,
    );
    assert!(switch_msg2.contains("DeepSeek"));
    assert!(switch_msg2.contains("Kimi"));
}

// ============================================================================
//  场景 13: MonitorConfig 与配置验证
// ============================================================================

#[test]
fn test_monitor_config_defaults() {
    // 验证: MonitorConfig 默认值合理
    let config = MonitorConfig {
        port: 9222,
        check_timeout_secs: 10,
        heartbeat_interval_secs: 30,
        max_consecutive_failures: 3,
    };

    // 崩溃阈值
    assert!(is_chrome_crashed_status(3, config.max_consecutive_failures));
    assert!(!is_chrome_crashed_status(
        2,
        config.max_consecutive_failures
    ));

    // 心跳间隔
    let delay = compute_next_check_delay(
        &ConnectionStatus::Connected,
        0,
        config.heartbeat_interval_secs,
    );
    assert_eq!(delay, 30);

    // 触发恢复
    assert!(should_trigger_recovery(
        &ConnectionStatus::ChromeUnreachable,
        1,
        config.max_consecutive_failures,
    ));
    assert!(!should_trigger_recovery(
        &ConnectionStatus::Connected,
        0,
        config.max_consecutive_failures,
    ));
}

// ============================================================================
//  场景 14: 综合 24h 模拟 — 所有模块联动
// ============================================================================

#[test]
fn test_comprehensive_24h_simulation() {
    // 综合模拟: 24h 运行, 包含多种故障类型和恢复场景

    let backoff = default_backoff();
    let mut stats = SimulationStats::default();

    // 24h = 86400 秒, 每 30 秒检查一次 → 2880 次检查
    // 但我们用更少的迭代来保持测试速度
    let check_interval_secs = 3600; // 1 小时模拟间隔
    let total_intervals = 24; // 24 个间隔 = 24h

    // 故障时间表 (每 6 小时一次故障, 共 4 次)
    let failure_times = [6, 12, 18, 23]; // 小时 (0..24, 24 不含)
    let failure_types = [
        (
            ConnectionStatus::ChromeUnreachable,
            SiteHealthStatus::UnderMaintenance,
        ),
        (
            ConnectionStatus::WebSocketError("ws reset".to_string()),
            SiteHealthStatus::RateLimited,
        ),
        (ConnectionStatus::TabClosed, SiteHealthStatus::NotLoggedIn),
        (
            ConnectionStatus::CheckTimeout,
            SiteHealthStatus::NetworkError,
        ),
    ];

    for interval in 0..total_intervals {
        let current_hour = (interval * check_interval_secs) / 3600;

        // 检查是否在故障时间
        let failure_idx = failure_times.iter().position(|&t| t == current_hour);

        if let Some(fidx) = failure_idx {
            let (conn_failure, site_failure) = &failure_types[fidx];

            // === CDP 层故障 ===
            stats.conn_checks += 1;
            stats.conn_failures += 1;

            let conn_step = check_and_decide_recovery(conn_failure, 1, 0);
            assert_ne!(conn_step.health_level, HealthLevel::Healthy);

            // 恢复重试
            let mut attempt = 0u32;
            loop {
                let action = decide_recovery_action(false, attempt, MAX_RETRIES, &backoff);
                match action {
                    RecoveryAction::Retry { next_attempt, .. } => {
                        attempt = next_attempt;
                        stats.conn_checks += 1;
                        stats.conn_failures += 1;
                        if attempt >= 2 {
                            // 恢复
                            break;
                        }
                    }
                    RecoveryAction::Succeed { .. } => break,
                    RecoveryAction::GiveUp { .. } => {
                        stats.recovery_failures += 1;
                        break;
                    }
                }
            }

            // 恢复成功
            let recovered = decide_recovery_action(true, attempt, MAX_RETRIES, &backoff);
            assert!(matches!(recovered, RecoveryAction::Succeed { .. }));
            stats.recovery_attempts += 1;
            stats.recovery_successes += 1;

            // === 网站层故障 ===
            stats.health_checks += 1;
            let site_step = check_and_decide_failover(site_failure);
            if site_step.should_failover {
                stats.failover_decisions += 1;

                // 故障转移到健康标签页
                let results = vec![
                    (0, HealthCheckResult::new(site_failure.clone())),
                    (1, HealthCheckResult::new(SiteHealthStatus::Healthy)),
                ];
                let best = select_best_healthy_tab(&results);
                assert_eq!(best, Some(1));
                stats.successful_failovers += 1;
            }
        } else {
            // === 正常运行 ===
            stats.conn_checks += 1;
            stats.health_checks += 1;
            stats.healthy_checks += 1;

            let conn_step = check_and_decide_recovery(&ConnectionStatus::Connected, 0, 0);
            assert_eq!(conn_step.health_level, HealthLevel::Healthy);

            let site_step = check_and_decide_failover(&SiteHealthStatus::Healthy);
            assert!(!site_step.should_failover);
        }
    }

    // 验证统计
    let monitor_rate = calculate_monitor_success_rate(stats.conn_checks, stats.conn_failures);
    let recovery_rate =
        compute_recovery_success_rate(stats.recovery_attempts, stats.recovery_successes);
    let health_rate = calculate_health_rate(stats.health_checks, stats.healthy_checks);

    // 24 个间隔, 4 次故障 → 20 次正常 + 4 次故障
    // 每次故障 ~3 次检查 → 20 + 12 = 32
    assert!(
        stats.conn_checks >= 24,
        "总检查数应 >= 24, 实际: {}",
        stats.conn_checks
    );
    assert!(
        stats.recovery_attempts == 4,
        "应有 4 次恢复, 实际: {}",
        stats.recovery_attempts
    );
    assert!(stats.recovery_successes == 4, "应有 4 次恢复成功");
    assert!(stats.failover_decisions == 4, "应有 4 次故障转移决策");
    assert!(stats.successful_failovers == 4, "应有 4 次成功故障转移");

    // 成功率验证 (24 间隔中 4 次故障, 每次故障 3 次失败检查 → 12/32 = 37.5% 失败)
    assert!(
        monitor_rate > 0.6,
        "监控成功率应 > 60%, 实际: {}",
        monitor_rate
    );
    assert!((recovery_rate - 1.0).abs() < 0.001, "恢复成功率应为 100%");
    assert!(health_rate > 0.6, "健康率应 > 60%, 实际: {}", health_rate);

    // 报告生成不 panic
    let _report = format!(
        "24h 模拟报告:\n  监控成功率: {}\n  恢复成功率: {}\n  健康率: {}\n  运行时长: {}",
        format_monitor_success_rate(monitor_rate),
        format_recovery_rate(recovery_rate),
        format_health_rate(health_rate),
        format_uptime(86400),
    );
}

/// 24h 模拟统计
#[derive(Default)]
struct SimulationStats {
    conn_checks: u64,
    conn_failures: u64,
    recovery_attempts: u64,
    recovery_successes: u64,
    recovery_failures: u64,
    health_checks: u64,
    healthy_checks: u64,
    failover_decisions: u64,
    successful_failovers: u64,
}
