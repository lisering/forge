//! FailoverChatClient — 多网站自动切换的 ChatClient 包装器
//!
//! 包装多个 `Failoverable` 客户端引用, 实现 ChatClient trait。
//! 在每次发送消息前, 自动检查当前网站的健康状态,
//! 如果不健康 (未登录/限流/维护中/网络错误), 自动切换到备用标签页。
//!
//! ## DIP 架构 (泛型化)
//!
//! `FailoverChatClient<'a, C: Failoverable>` 泛型化于 `Failoverable` trait,
//! 而非直接依赖 `ChatTab`。这使得:
//! - 生产环境: `C = ChatTab` (真实 Chrome 标签页)
//! - 测试环境: `C = MockFailoverClient` (预编程响应, 无需 Chrome)
//!
//! ## 工作流程
//!
//! ```text
//! send_message()
//!   → maybe_check_health() (每 N 轮检查一次)
//!     → C::health_check()
//!     → 不健康? → try_switch() → SiteFailover::should_switch()
//!       → 有备用标签页? → 切换 + 重试
//!       → 无备用标签页? → 记录失败 + 继续使用当前标签页
//!   → 当前标签页.send_message()
//!   → 失败? → try_switch(NetworkError) + 重试
//! ```
//!
//! ## DIP 架构
//!
//! FailoverChatClient 实现 ChatClient trait,
//! Orchestrator 只看到一个 ChatClient, 不需要知道 failover 逻辑。
//! 这保持了 SOLID 的依赖倒置原则。

use crate::browser::SiteType;
use crate::dev_trace::{DevTraceWriter, TraceAction};
use crate::site_health::{HealthCheckResult, SiteFailover, SiteHealthChecker, SiteHealthStatus};
use crate::traits::{ChatClient, ChatResult, Failoverable};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex as StdMutex;
use std::time::Instant;
use tokio::sync::Mutex;
use tracing::{info, warn};

// ============================================================================
//  SitePerformanceStats — 网站性能统计
// ============================================================================

/// 单个网站的性能统计
#[derive(Debug, Clone, Default)]
pub struct SitePerformanceStats {
    /// 总发送次数
    pub total_sends: u64,
    /// 成功次数
    pub success_count: u64,
    /// 失败次数
    pub failure_count: u64,
    /// 健康检查次数
    pub health_checks: u64,
    /// 健康检查通过次数
    pub healthy_count: u64,
    /// 总响应时间 (毫秒)
    pub total_response_ms: u64,
    /// 最快响应时间 (毫秒)
    pub min_response_ms: u64,
    /// 最慢响应时间 (毫秒)
    pub max_response_ms: u64,
    /// 被切换走次数 (作为主网站不健康被切换)
    pub failover_from_count: u64,
    /// 被切换到次数 (作为备用网站被切换到)
    pub failover_to_count: u64,
}

impl SitePerformanceStats {
    /// 记录一次成功的发送
    pub fn record_success(&mut self, duration_ms: u64) {
        self.total_sends += 1;
        self.success_count += 1;
        self.total_response_ms += duration_ms;
        self.min_response_ms = update_min_response_time(self.min_response_ms, duration_ms);
        if duration_ms > self.max_response_ms {
            self.max_response_ms = duration_ms;
        }
    }

    /// 记录一次失败的发送
    pub fn record_failure(&mut self) {
        self.total_sends += 1;
        self.failure_count += 1;
    }

    /// 记录一次健康检查
    pub fn record_health_check(&mut self, healthy: bool) {
        self.health_checks += 1;
        if healthy {
            self.healthy_count += 1;
        }
    }

    /// 记录被切换走 (作为主网站不健康)
    pub fn record_failover_from(&mut self) {
        self.failover_from_count += 1;
    }

    /// 记录被切换到 (作为备用网站)
    pub fn record_failover_to(&mut self) {
        self.failover_to_count += 1;
    }

    /// 成功率 (0.0 ~ 1.0)
    pub fn success_rate(&self) -> f64 {
        if self.total_sends == 0 {
            return 0.0;
        }
        self.success_count as f64 / self.total_sends as f64
    }

    /// 平均响应时间 (毫秒)
    pub fn avg_response_ms(&self) -> u64 {
        if self.success_count == 0 {
            return 0;
        }
        self.total_response_ms / self.success_count
    }

    /// 健康率 (0.0 ~ 1.0)
    pub fn health_rate(&self) -> f64 {
        if self.health_checks == 0 {
            return 0.0;
        }
        self.healthy_count as f64 / self.health_checks as f64
    }

    /// 生成摘要字符串
    pub fn summary(&self) -> String {
        format!(
            "发送:{} 成功:{} 失败:{} 成功率:{:.1}% 平均:{}ms 最快:{}ms 最慢:{}ms 健康:{}/{} 切出:{} 切入:{}",
            self.total_sends,
            self.success_count,
            self.failure_count,
            self.success_rate() * 100.0,
            self.avg_response_ms(),
            self.min_response_ms,
            self.max_response_ms,
            self.healthy_count,
            self.health_checks,
            self.failover_from_count,
            self.failover_to_count,
        )
    }
}

// ============================================================================
//  Pure Logic Functions — 纯逻辑函数 (可独立测试, 无异步/外部依赖)
// ============================================================================

/// 判断健康检查结果是否应该触发故障切换
///
/// 当且仅当: 不健康 **且** 应该切换 时返回 `true`。
///
/// # 示例
///
/// ```
/// # use forge::site_health::{HealthCheckResult, SiteHealthStatus};
/// # use forge::failover_chat::should_failover_decision;
/// let healthy = HealthCheckResult::new(SiteHealthStatus::Healthy);
/// assert!(!should_failover_decision(&healthy));
///
/// let rate_limited = HealthCheckResult::new(SiteHealthStatus::RateLimited);
/// assert!(should_failover_decision(&rate_limited));
/// ```
pub fn should_failover_decision(health: &HealthCheckResult) -> bool {
    !health.is_healthy() && health.should_failover()
}

/// 从错误字符串构建 `NetworkError` 健康检查结果
///
/// 用于健康检查本身失败 (如 CDP 连接断开) 时, 创建一个表示网络错误的
/// `HealthCheckResult`。时间戳使用当前系统时间。
///
/// # 示例
///
/// ```
/// # use forge::failover_chat::build_error_health_result;
/// # use forge::site_health::SiteHealthStatus;
/// let result = build_error_health_result("connection refused".to_string());
/// assert_eq!(result.status, SiteHealthStatus::NetworkError);
/// assert_eq!(result.message.as_deref(), Some("connection refused"));
/// ```
pub fn build_error_health_result(error_msg: String) -> HealthCheckResult {
    HealthCheckResult {
        status: SiteHealthStatus::NetworkError,
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        message: Some(error_msg),
        current_url: None,
        check_duration_ms: 0,
    }
}

/// 分类故障切换失败的原因
///
/// 根据切换策略状态返回人类可读的原因描述, 用于日志和 DevTrace。
///
/// - `all_tried == true` → "所有标签页都已尝试"
/// - `consecutive_failures >= max_failures` → "超过最大连续失败次数"
/// - 否则 → "无可用标签页"
///
/// # 示例
///
/// ```
/// # use forge::failover_chat::classify_failover_failure_reason;
/// assert_eq!(classify_failover_failure_reason(true, 0, 3), "所有标签页都已尝试");
/// assert_eq!(classify_failover_failure_reason(false, 3, 3), "超过最大连续失败次数");
/// assert_eq!(classify_failover_failure_reason(false, 0, 3), "无可用标签页");
/// ```
pub fn classify_failover_failure_reason(
    all_tried: bool,
    consecutive_failures: usize,
    max_failures: usize,
) -> &'static str {
    if all_tried {
        "所有标签页都已尝试"
    } else if consecutive_failures >= max_failures {
        "超过最大连续失败次数"
    } else {
        "无可用标签页"
    }
}

/// 计算健康检查间隔是否已到
///
/// - `interval == 0` → 每次都检查 (返回 `true`)
/// - `current_turn - last_check_turn >= interval` → 应检查 (返回 `true`)
///
/// 使用 `saturating_sub` 防止下溢。
///
/// # 示例
///
/// ```
/// # use forge::failover_chat::calculate_health_check_interval_elapsed;
/// // interval=0 → 每次都检查
/// assert!(calculate_health_check_interval_elapsed(0, 0, 0));
/// assert!(calculate_health_check_interval_elapsed(5, 3, 0));
///
/// // interval=3, 当前=5, 上次=0 → 5-0=5 >= 3 → 检查
/// assert!(calculate_health_check_interval_elapsed(5, 0, 3));
///
/// // interval=3, 当前=2, 上次=0 → 2-0=2 < 3 → 跳过
/// assert!(!calculate_health_check_interval_elapsed(2, 0, 3));
/// ```
pub fn calculate_health_check_interval_elapsed(
    current_turn: usize,
    last_check_turn: usize,
    interval: usize,
) -> bool {
    if interval == 0 {
        return true;
    }
    current_turn.saturating_sub(last_check_turn) >= interval
}

/// 更新最小响应时间
///
/// 当 `current_min` 为 `0` (初始值) 或 `new_duration` 更小时, 返回 `new_duration`。
/// 否则返回 `current_min` 不变。
///
/// # 示例
///
/// ```
/// # use forge::failover_chat::update_min_response_time;
/// assert_eq!(update_min_response_time(0, 150), 150);   // 初始 → 更新
/// assert_eq!(update_min_response_time(150, 100), 100);  // 更小 → 更新
/// assert_eq!(update_min_response_time(100, 200), 100);  // 更大 → 不变
/// ```
pub fn update_min_response_time(current_min: u64, new_duration: u64) -> u64 {
    if current_min == 0 || new_duration < current_min {
        new_duration
    } else {
        current_min
    }
}

/// 格式化切换成功的 trace 消息
///
/// 生成如 `"切换 [0] Z.ai → [1] DeepSeek"` 的格式, 用于日志和 DevTrace。
///
/// # 示例
///
/// ```
/// # use forge::failover_chat::format_switch_trace;
/// # use forge::browser::SiteType;
/// let msg = format_switch_trace(0, SiteType::Zai, 1, SiteType::DeepSeek);
/// assert!(msg.contains("Z.ai"));
/// assert!(msg.contains("DeepSeek"));
/// ```
pub fn format_switch_trace(
    old_idx: usize,
    old_site: SiteType,
    new_idx: usize,
    new_site: SiteType,
) -> String {
    format!(
        "切换 [{}] {} → [{}] {}",
        old_idx, old_site, new_idx, new_site
    )
}

/// 格式化切换失败的 trace 消息
///
/// 生成如 `"尝试从 [0] Z.ai 切换"` 的格式, 用于日志和 DevTrace。
///
/// # 示例
///
/// ```
/// # use forge::failover_chat::format_failover_failure_trace;
/// # use forge::browser::SiteType;
/// let msg = format_failover_failure_trace(0, SiteType::Zai);
/// assert!(msg.contains("Z.ai"));
/// assert!(msg.contains("尝试从"));
/// ```
pub fn format_failover_failure_trace(old_idx: usize, old_site: SiteType) -> String {
    format!("尝试从 [{}] {} 切换", old_idx, old_site)
}

// ============================================================================
//  FailoverChatClient — 多网站自动切换 ChatClient (泛型)
// ============================================================================

/// 多网站自动切换的 ChatClient 包装器 (泛型于 `Failoverable`)
///
/// 包装多个 `Failoverable` 客户端引用, 在发送消息前自动检查网站健康状态,
/// 不健康时切换到备用标签页。对 Orchestrator 透明 (实现 ChatClient trait)。
///
/// ## 泛型参数
///
/// - `C: Failoverable` — 可故障切换的聊天客户端
///   - 生产环境: `C = ChatTab` (真实 Chrome 标签页)
///   - 测试环境: `C = MockFailoverClient` (预编程响应)
///
/// ## 内部状态
///
/// - `current_index`: 当前活跃标签页索引 (原子操作)
/// - `failover`: 切换策略 (SiteFailover, 在 Mutex 中)
/// - `health_check_interval`: 健康检查间隔 (每 N 轮检查一次)
/// - `stats`: 各网站的性能统计
pub struct FailoverChatClient<'a, C: Failoverable> {
    /// 所有可用的聊天客户端 (泛型, 支持 ChatTab / MockFailoverClient)
    tabs: Vec<&'a C>,
    /// 当前活跃标签页索引 (原子操作, 线程安全)
    current_index: AtomicUsize,
    /// 多网站自动切换策略 (需要 Mutex, 因为 SiteFailover 有可变状态)
    failover: Mutex<SiteFailover>,
    /// 健康检查间隔 (每 N 轮对话检查一次, 0 = 每次都检查)
    health_check_interval: usize,
    /// 上次健康检查时的对话轮数
    last_check_turn: AtomicUsize,
    /// 各网站的性能统计 (按 tab 索引)
    stats: Mutex<Vec<SitePerformanceStats>>,
    /// DevTrace 写入器 (可选, 由 Orchestrator 通过 set_dev_trace 设置)
    ///
    /// 使用 std::sync::Mutex 而非 tokio::sync::Mutex, 因为:
    /// 1. set_dev_trace 是非异步方法 (trait 定义)
    /// 2. trace 写入是同步文件 I/O, 不跨 await 点
    /// 3. 锁持有时间极短 (仅检查 Option + 写入 trace)
    dev_trace: StdMutex<Option<DevTraceWriter>>,
}

impl<'a, C: Failoverable> FailoverChatClient<'a, C> {
    /// 创建多网站自动切换客户端
    ///
    /// - `tabs`: 所有可用的聊天客户端 (至少 1 个)
    /// - `current_tab`: 初始活跃标签页索引
    /// - `max_failures`: 最大连续失败次数 (超过后放弃切换)
    /// - `cooldown_secs`: 切换冷却时间 (秒, 避免频繁切换)
    /// - `check_interval`: 健康检查间隔 (每 N 轮对话检查一次)
    pub fn new(
        tabs: Vec<&'a C>,
        current_tab: usize,
        max_failures: usize,
        cooldown_secs: u64,
        check_interval: usize,
    ) -> Self {
        assert!(!tabs.is_empty(), "FailoverChatClient 需要至少一个标签页");
        assert!(current_tab < tabs.len(), "初始标签页索引超出范围");

        let available_tabs: Vec<usize> = (0..tabs.len()).collect();
        let failover = SiteFailover::new(available_tabs, current_tab)
            .with_max_failures(max_failures)
            .with_cooldown(cooldown_secs);

        let stats_count = tabs.len();
        Self {
            tabs,
            current_index: AtomicUsize::new(current_tab),
            failover: Mutex::new(failover),
            health_check_interval: check_interval,
            last_check_turn: AtomicUsize::new(0),
            stats: Mutex::new(vec![SitePerformanceStats::default(); stats_count]),
            dev_trace: StdMutex::new(None),
        }
    }

    /// 获取当前活跃标签页
    fn current_tab(&self) -> &'a C {
        let idx = self.current_index.load(Ordering::Relaxed);
        self.tabs[idx]
    }

    /// 获取当前活跃标签页索引
    pub fn current_tab_index(&self) -> usize {
        self.current_index.load(Ordering::Relaxed)
    }

    /// 获取标签页数量
    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    /// 是否到了健康检查的时候
    fn should_check_health(&self) -> bool {
        let current_turn = self.current_tab().conversation_turn_count();
        let last_check = self.last_check_turn.load(Ordering::Relaxed);
        calculate_health_check_interval_elapsed(
            current_turn,
            last_check,
            self.health_check_interval,
        )
    }

    /// 执行健康检查 (如果到了检查的时候)
    ///
    /// 返回 Some(result) 表示执行了检查, None 表示跳过 (不到检查间隔)。
    async fn maybe_check_health(&self) -> Option<HealthCheckResult> {
        if !self.should_check_health() {
            return None;
        }

        let current_turn = self.current_tab().conversation_turn_count();
        self.last_check_turn.store(current_turn, Ordering::Relaxed);

        let tab = self.current_tab();
        let site_type = tab.site_type();
        let result = match tab.health_check().await {
            Ok(r) => r,
            Err(e) => {
                warn!("健康检查失败 [{}]: {}", site_type, e);
                build_error_health_result(e.to_string())
            }
        };

        // 记录健康检查统计
        let is_healthy = result.is_healthy();
        let idx = self.current_tab_index();
        if let Ok(mut stats) = self.stats.try_lock() {
            stats[idx].record_health_check(is_healthy);
        }

        if is_healthy {
            info!("✅ 网站健康检查通过 [{}]", site_type);
        } else {
            warn!(
                "⚠ 网站健康检查未通过 [{}]: {} — {}",
                site_type,
                result.status,
                result.message.as_deref().unwrap_or("无详细信息")
            );
        }

        // 写入 DevTrace (健康检查事件)
        self.write_trace(
            TraceAction::HealthCheck,
            None,
            None,
            None,
            &format!("检查 [{}] {}", idx, site_type),
            &format!("{}", result.status),
            result.check_duration_ms,
            is_healthy,
            result.message.as_deref(),
        );

        Some(result)
    }

    /// 尝试切换到备用标签页
    ///
    /// 返回 true 表示成功切换, false 表示无法切换 (无可用标签页或超过最大失败次数)。
    async fn try_switch(&self, health: &HealthCheckResult) -> bool {
        let mut failover = self.failover.lock().await;

        // 记录被切换走
        let old_idx = self.current_tab_index();
        let old_site_type = self.tabs[old_idx].site_type();
        if let Ok(mut stats) = self.stats.try_lock() {
            if old_idx < stats.len() {
                stats[old_idx].record_failover_from();
            }
        }

        match failover.should_switch(health) {
            Some(new_idx) => {
                let new_site_type = self.tabs[new_idx].site_type();
                info!(
                    "🔄 网站自动切换: 标签页 [{}] ({}) → 标签页 [{}] ({})",
                    old_idx, old_site_type, new_idx, new_site_type
                );
                println!(
                    "\n  🔄 网站自动切换: [{}] {} → [{}] {}",
                    old_idx, old_site_type, new_idx, new_site_type
                );

                failover.record_switch(new_idx);
                self.current_index.store(new_idx, Ordering::Relaxed);

                // 记录被切换到
                if let Ok(mut stats) = self.stats.try_lock() {
                    if new_idx < stats.len() {
                        stats[new_idx].record_failover_to();
                    }
                }

                // 写入 DevTrace (切换成功)
                self.write_trace(
                    TraceAction::SiteFailover,
                    None,
                    None,
                    None,
                    &format_switch_trace(old_idx, old_site_type, new_idx, new_site_type),
                    "成功",
                    0,
                    true,
                    None,
                );

                true
            }
            None => {
                failover.record_failure();
                if failover.all_tried() {
                    warn!("所有标签页都已尝试, 无法切换");
                } else if failover.consecutive_failures >= failover.max_consecutive_failures {
                    warn!(
                        "连续失败 {} 次, 超过最大 {}, 放弃切换",
                        failover.consecutive_failures, failover.max_consecutive_failures
                    );
                }

                // 写入 DevTrace (切换失败)
                let failover_reason = classify_failover_failure_reason(
                    failover.all_tried(),
                    failover.consecutive_failures,
                    failover.max_consecutive_failures,
                );
                self.write_trace(
                    TraceAction::SiteFailover,
                    None,
                    None,
                    None,
                    &format_failover_failure_trace(old_idx, old_site_type),
                    "无法切换",
                    0,
                    false,
                    Some(failover_reason),
                );

                false
            }
        }
    }

    /// 获取所有网站的性能统计
    pub async fn get_stats(&self) -> Vec<(usize, SiteType, SitePerformanceStats)> {
        let stats = self.stats.lock().await;
        self.tabs
            .iter()
            .enumerate()
            .map(|(i, tab)| (i, tab.site_type(), stats[i].clone()))
            .collect()
    }

    /// 打印性能统计摘要
    pub async fn print_stats(&self) {
        let stats = self.get_stats().await;
        println!("\n📊 网站性能统计:");
        println!("──────────────────────────────────────────────");
        for (idx, site_type, s) in &stats {
            println!("  [{}] {}: {}", idx, site_type, s.summary());
        }
        println!("──────────────────────────────────────────────");
    }

    /// 写入 DevTrace 条目 (如果已设置 dev_trace_writer)
    ///
    /// 使用 std::sync::Mutex (非 async), 因为:
    /// - trace 写入是同步文件 I/O
    /// - 锁持有时间极短
    /// - 不跨 await 点
    #[allow(clippy::too_many_arguments)]
    fn write_trace(
        &self,
        action: TraceAction,
        phase_idx: Option<usize>,
        task_idx: Option<usize>,
        task_name: Option<&str>,
        input: &str,
        output: &str,
        duration_ms: u64,
        success: bool,
        error: Option<&str>,
    ) {
        if let Ok(dt) = self.dev_trace.lock() {
            if let Some(ref writer) = *dt {
                if let Err(e) = writer.trace(
                    action,
                    phase_idx,
                    task_idx,
                    task_name,
                    input,
                    output,
                    duration_ms,
                    success,
                    error,
                ) {
                    warn!("DevTrace 写入失败: {}", e);
                }
            }
        }
    }
}

#[async_trait]
impl<'a, C: Failoverable> ChatClient for FailoverChatClient<'a, C> {
    async fn send_message(&self, msg: &str, timeout: u64) -> Result<ChatResult> {
        let send_start = Instant::now();

        // 1. 健康检查 (如果到了检查的时候)
        if let Some(health) = self.maybe_check_health().await {
            if should_failover_decision(&health) {
                // 尝试切换
                self.try_switch(&health).await;
            }
        }

        // 2. 通过当前标签页发送消息
        let tab = self.current_tab();
        let site_type = tab.site_type();
        match tab.send_message(msg, timeout).await {
            Ok(result) => {
                let duration_ms = send_start.elapsed().as_millis() as u64;

                // 第 39 项改进: 主动检测 AI 回复中的限流标志
                // Z.ai 限流时, 回复内容可能是 "回复内容为空，请稍后重试" 等限流提示
                // 此时虽然 send_message 返回 Ok, 但实际内容不可用 → 提前切换到备用标签页
                if let Some(rate_limit_health) =
                    SiteHealthChecker::check_response_for_rate_limit(&result.text)
                {
                    warn!(
                        "⚠️ AI 回复包含限流标志 [{}], 主动切换到备用标签页: {}",
                        site_type,
                        rate_limit_health.message.as_deref().unwrap_or("限流")
                    );
                    println!("\n  ⚠️ 检测到 [{}] 限流, 主动切换到备用标签页", site_type);

                    // 记录失败统计 (限流 = 实际失败)
                    let idx = self.current_tab_index();
                    if let Ok(mut stats) = self.stats.try_lock() {
                        stats[idx].record_failure();
                    }

                    // 尝试切换到备用标签页
                    let switched = self.try_switch(&rate_limit_health).await;
                    if switched {
                        // 用新标签页重试
                        let new_tab = self.current_tab();
                        let new_site_type = new_tab.site_type();
                        info!(
                            "使用备用标签页 [{}] ({}) 重试 (限流切换)",
                            self.current_tab_index(),
                            new_site_type
                        );
                        return new_tab.send_message(msg, timeout).await;
                    }
                    // 无法切换, 返回原始结果 (可能内容为限流提示)
                }

                // 记录成功统计
                let idx = self.current_tab_index();
                if let Ok(mut stats) = self.stats.try_lock() {
                    stats[idx].record_success(duration_ms);
                }

                // 重置 failover 状态 (成功后清除失败计数)
                let mut failover = self.failover.lock().await;
                failover.record_success();

                Ok(result)
            }
            Err(e) => {
                warn!("send_message 失败 [{}]: {}, 尝试切换...", site_type, e);

                // 记录失败统计
                let idx = self.current_tab_index();
                if let Ok(mut stats) = self.stats.try_lock() {
                    stats[idx].record_failure();
                }

                // 尝试切换到备用标签页
                let network_error = HealthCheckResult::new(SiteHealthStatus::NetworkError);
                let switched = self.try_switch(&network_error).await;

                if switched {
                    // 用新标签页重试
                    let new_tab = self.current_tab();
                    let new_site_type = new_tab.site_type();
                    info!(
                        "使用备用标签页 [{}] ({}) 重试",
                        self.current_tab_index(),
                        new_site_type
                    );
                    new_tab.send_message(msg, timeout).await
                } else {
                    Err(e)
                }
            }
        }
    }

    async fn start_new_conversation(&self) -> Result<()> {
        self.current_tab().start_new_conversation().await
    }

    /// 上传文件 — 委托给当前标签页
    ///
    /// 文件上传不需要健康检查和故障切换 (上传是本地操作),
    /// 直接委托给当前活跃标签页。
    async fn upload_files(&self, file_paths: &[&str]) -> Result<()> {
        self.current_tab().upload_files(file_paths).await
    }

    fn conversation_turn_count(&self) -> usize {
        self.current_tab().conversation_turn_count()
    }

    /// 接收 DevTraceWriter — 由 Orchestrator 在运行开始前调用
    ///
    /// 接收后, 健康检查和网站切换事件会自动写入 trace 文件。
    fn set_dev_trace(&self, writer: DevTraceWriter) {
        if let Ok(mut dt) = self.dev_trace.lock() {
            *dt = Some(writer);
            info!("FailoverChatClient: DevTrace 已集成");
        } else {
            warn!("FailoverChatClient: DevTrace Mutex 锁定失败, 无法集成");
        }
    }

    /// 写入最终性能统计到 DevTrace — 运行结束后调用
    ///
    /// 将各网站的 SitePerformanceStats 摘要写入 trace 文件,
    /// 提供运行结束后的性能可观测性。
    async fn write_final_trace(&self) {
        let stats = self.get_stats().await;
        for (idx, site_type, s) in &stats {
            self.write_trace(
                TraceAction::PerformanceStats,
                None,
                None,
                None,
                &format!("性能统计 [{}] {}", idx, site_type),
                &s.summary(),
                0,
                true,
                None,
            );
        }
    }
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::SiteType;
    use crate::site_health::SiteHealthStatus;
    use crate::traits::{ChatResult, Failoverable};
    use anyhow::anyhow;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex as StdMutex};
    use tempfile::tempdir;

    // ===== SitePerformanceStats 测试 =====

    #[test]
    fn test_performance_stats_default() {
        let stats = SitePerformanceStats::default();
        assert_eq!(stats.total_sends, 0);
        assert_eq!(stats.success_count, 0);
        assert_eq!(stats.failure_count, 0);
    }

    #[test]
    fn test_performance_stats_record_success() {
        let mut stats = SitePerformanceStats::default();
        stats.record_success(100);
        stats.record_success(200);
        assert_eq!(stats.total_sends, 2);
        assert_eq!(stats.success_count, 2);
        assert_eq!(stats.total_response_ms, 300);
        assert_eq!(stats.min_response_ms, 100);
        assert_eq!(stats.max_response_ms, 200);
    }

    #[test]
    fn test_performance_stats_record_failure() {
        let mut stats = SitePerformanceStats::default();
        stats.record_failure();
        stats.record_failure();
        assert_eq!(stats.total_sends, 2);
        assert_eq!(stats.failure_count, 2);
        assert_eq!(stats.success_count, 0);
    }

    #[test]
    fn test_performance_stats_success_rate() {
        let mut stats = SitePerformanceStats::default();
        stats.record_success(100);
        stats.record_success(200);
        stats.record_failure();
        assert_eq!(stats.success_rate(), 2.0 / 3.0);
    }

    #[test]
    fn test_performance_stats_success_rate_empty() {
        let stats = SitePerformanceStats::default();
        assert_eq!(stats.success_rate(), 0.0);
    }

    #[test]
    fn test_performance_stats_avg_response_ms() {
        let mut stats = SitePerformanceStats::default();
        stats.record_success(100);
        stats.record_success(300);
        assert_eq!(stats.avg_response_ms(), 200);
    }

    #[test]
    fn test_performance_stats_avg_response_ms_empty() {
        let stats = SitePerformanceStats::default();
        assert_eq!(stats.avg_response_ms(), 0);
    }

    #[test]
    fn test_performance_stats_health_check() {
        let mut stats = SitePerformanceStats::default();
        stats.record_health_check(true);
        stats.record_health_check(true);
        stats.record_health_check(false);
        assert_eq!(stats.health_checks, 3);
        assert_eq!(stats.healthy_count, 2);
        assert_eq!(stats.health_rate(), 2.0 / 3.0);
    }

    #[test]
    fn test_performance_stats_failover_counts() {
        let mut stats = SitePerformanceStats::default();
        stats.record_failover_from();
        stats.record_failover_from();
        stats.record_failover_to();
        assert_eq!(stats.failover_from_count, 2);
        assert_eq!(stats.failover_to_count, 1);
    }

    #[test]
    fn test_performance_stats_summary() {
        let mut stats = SitePerformanceStats::default();
        stats.record_success(150);
        let summary = stats.summary();
        assert!(summary.contains("发送:1"));
        assert!(summary.contains("成功:1"));
        assert!(summary.contains("150ms"));
    }

    #[test]
    fn test_performance_stats_min_response_first_success() {
        let mut stats = SitePerformanceStats::default();
        assert_eq!(stats.min_response_ms, 0);
        stats.record_success(150);
        assert_eq!(stats.min_response_ms, 150);
        stats.record_success(100);
        assert_eq!(stats.min_response_ms, 100);
        stats.record_success(200);
        assert_eq!(stats.min_response_ms, 100);
    }

    #[test]
    fn test_performance_stats_health_rate_empty() {
        let stats = SitePerformanceStats::default();
        assert_eq!(stats.health_rate(), 0.0);
    }

    #[test]
    fn test_performance_stats_all_failures() {
        let mut stats = SitePerformanceStats::default();
        stats.record_failure();
        stats.record_failure();
        assert_eq!(stats.success_rate(), 0.0);
        assert_eq!(stats.avg_response_ms(), 0);
    }

    // ===== MockFailoverClient — 无 Chrome 测试用的 Failoverable 实现 =====

    /// 测试用的可故障切换客户端
    ///
    /// 实现 `Failoverable` trait, 提供:
    /// - 预编程回复队列 (按调用顺序弹出)
    /// - 可配置的健康检查结果
    /// - 对话轮数跟踪
    /// - 发送消息记录 (用于断言)
    /// - 健康检查调用记录 (用于断言)
    struct MockFailoverClient {
        /// 预编程回复队列
        responses: Arc<StdMutex<Vec<String>>>,
        /// 记录所有收到的消息
        sent_messages: Arc<StdMutex<Vec<String>>>,
        /// 健康检查结果 (每次调用弹出一个)
        health_results: Arc<StdMutex<Vec<HealthCheckResult>>>,
        /// 默认健康检查结果 (队列为空时使用)
        default_health: Arc<StdMutex<HealthCheckResult>>,
        /// 网站类型
        site: SiteType,
        /// 对话轮数
        turn_count: AtomicUsize,
        /// 健康检查调用次数
        health_check_calls: AtomicUsize,
        /// 是否强制 send_message 失败
        force_error: Arc<StdMutex<bool>>,
        /// 记录所有 upload_files 调用
        uploaded_files: Arc<StdMutex<Vec<Vec<String>>>>,
    }

    impl MockFailoverClient {
        fn new(site: SiteType, responses: Vec<String>) -> Self {
            Self {
                responses: Arc::new(StdMutex::new(responses)),
                sent_messages: Arc::new(StdMutex::new(vec![])),
                health_results: Arc::new(StdMutex::new(vec![])),
                default_health: Arc::new(StdMutex::new(HealthCheckResult::new(
                    SiteHealthStatus::Healthy,
                ))),
                site,
                turn_count: AtomicUsize::new(0),
                health_check_calls: AtomicUsize::new(0),
                force_error: Arc::new(StdMutex::new(false)),
                uploaded_files: Arc::new(StdMutex::new(vec![])),
            }
        }

        #[allow(dead_code)]
        fn with_health_results(mut self, results: Vec<HealthCheckResult>) -> Self {
            self.health_results = Arc::new(StdMutex::new(results));
            self
        }

        /// 设置默认健康检查结果
        fn with_default_health(mut self, health: HealthCheckResult) -> Self {
            self.default_health = Arc::new(StdMutex::new(health));
            self
        }

        /// 强制 send_message 返回错误
        fn with_force_error(mut self) -> Self {
            self.force_error = Arc::new(StdMutex::new(true));
            self
        }

        /// 获取收到的消息列表
        fn sent_messages(&self) -> Vec<String> {
            self.sent_messages.lock().unwrap().clone()
        }

        /// 获取上传文件调用记录
        fn uploaded_files(&self) -> Vec<Vec<String>> {
            self.uploaded_files.lock().unwrap().clone()
        }

        /// 获取健康检查调用次数
        fn health_check_call_count(&self) -> usize {
            self.health_check_calls.load(Ordering::Relaxed)
        }

        /// 获取当前对话轮数
        fn turn(&self) -> usize {
            self.turn_count.load(Ordering::Relaxed)
        }
    }

    #[async_trait]
    impl Failoverable for MockFailoverClient {
        fn site_type(&self) -> SiteType {
            self.site
        }

        async fn health_check(&self) -> Result<HealthCheckResult> {
            self.health_check_calls.fetch_add(1, Ordering::Relaxed);
            let results = self.health_results.lock().unwrap();
            if !results.is_empty() {
                // 弹出第一个结果 (但不移除, 保留队列)
                // 使用 clone 避免移除
                Ok(results[0].clone())
            } else {
                Ok(self.default_health.lock().unwrap().clone())
            }
        }
    }

    #[async_trait]
    impl ChatClient for MockFailoverClient {
        async fn send_message(&self, msg: &str, _timeout: u64) -> Result<ChatResult> {
            self.sent_messages.lock().unwrap().push(msg.to_string());
            self.turn_count.fetch_add(1, Ordering::Relaxed);

            if *self.force_error.lock().unwrap() {
                return Err(anyhow!("mock send_message error"));
            }

            let mut queue = self.responses.lock().unwrap();
            let text = if queue.is_empty() {
                "(empty)".to_string()
            } else {
                queue.remove(0)
            };
            Ok(ChatResult {
                text,
                timed_out: false,
            })
        }

        /// 覆盖默认实现 — 返回实际的对话轮数
        fn conversation_turn_count(&self) -> usize {
            self.turn_count.load(Ordering::Relaxed)
        }

        /// 覆盖默认实现 — 记录上传的文件路径
        async fn upload_files(&self, file_paths: &[&str]) -> Result<()> {
            let files: Vec<String> = file_paths.iter().map(|s| s.to_string()).collect();
            self.uploaded_files.lock().unwrap().push(files);
            Ok(())
        }
    }

    /// 辅助函数: 创建两个 MockFailoverClient
    fn make_two_mocks() -> (MockFailoverClient, MockFailoverClient) {
        let tab0 = MockFailoverClient::new(SiteType::Zai, vec!["response from Zai".to_string()]);
        let tab1 = MockFailoverClient::new(
            SiteType::DeepSeek,
            vec!["response from DeepSeek".to_string()],
        );
        (tab0, tab1)
    }

    // ===== FailoverChatClient 构造测试 =====

    #[test]
    #[should_panic(expected = "至少一个标签页")]
    fn test_failover_chat_empty_tabs_panics() {
        let _ = FailoverChatClient::<MockFailoverClient>::new(vec![], 0, 3, 30, 5);
    }

    #[test]
    #[should_panic(expected = "初始标签页索引超出范围")]
    fn test_failover_chat_index_out_of_range_panics() {
        let (tab0, _tab1) = make_two_mocks();
        let _ = FailoverChatClient::new(vec![&tab0], 5, 3, 30, 5);
    }

    #[test]
    fn test_failover_chat_construction() {
        let (tab0, tab1) = make_two_mocks();
        let client = FailoverChatClient::new(vec![&tab0, &tab1], 0, 3, 30, 5);
        assert_eq!(client.tab_count(), 2);
        assert_eq!(client.current_tab_index(), 0);
    }

    #[test]
    fn test_failover_chat_construction_with_index_1() {
        let (tab0, tab1) = make_two_mocks();
        let client = FailoverChatClient::new(vec![&tab0, &tab1], 1, 3, 30, 5);
        assert_eq!(client.current_tab_index(), 1);
    }

    #[test]
    fn test_failover_chat_single_tab() {
        let (tab0, _tab1) = make_two_mocks();
        let client = FailoverChatClient::new(vec![&tab0], 0, 3, 30, 5);
        assert_eq!(client.tab_count(), 1);
        assert_eq!(client.current_tab_index(), 0);
    }

    // ===== send_message 基本测试 (无健康检查触发) =====

    #[tokio::test]
    async fn test_send_message_success() {
        let (tab0, _tab1) = make_two_mocks();
        let client = FailoverChatClient::new(vec![&tab0], 0, 3, 30, 0);
        let result = client.send_message("hello", 60).await.unwrap();
        assert_eq!(result.text, "response from Zai");
        assert!(!result.timed_out);
        assert_eq!(tab0.sent_messages(), vec!["hello".to_string()]);
    }

    #[tokio::test]
    async fn test_send_message_increments_turn_count() {
        let (tab0, _tab1) = make_two_mocks();
        let client = FailoverChatClient::new(vec![&tab0], 0, 3, 30, 0);
        assert_eq!(tab0.turn(), 0);
        client.send_message("msg1", 60).await.unwrap();
        assert_eq!(tab0.turn(), 1);
        client.send_message("msg2", 60).await.unwrap();
        assert_eq!(tab0.turn(), 2);
    }

    #[tokio::test]
    async fn test_conversation_turn_count_via_trait() {
        let (tab0, _tab1) = make_two_mocks();
        let client = FailoverChatClient::new(vec![&tab0], 0, 3, 30, 0);
        assert_eq!(client.conversation_turn_count(), 0);
        client.send_message("msg1", 60).await.unwrap();
        assert_eq!(client.conversation_turn_count(), 1);
    }

    // ===== 健康检查测试 =====

    #[tokio::test]
    async fn test_health_check_interval_skips() {
        // health_check_interval = 10, tab.turn() = 0 → 0 < 10 → 跳过
        let (tab0, _tab1) = make_two_mocks();
        let client = FailoverChatClient::new(vec![&tab0], 0, 3, 30, 10);
        client.send_message("hello", 60).await.unwrap();
        assert_eq!(tab0.health_check_call_count(), 0);
    }

    #[tokio::test]
    async fn test_health_check_interval_zero_always_checks() {
        // health_check_interval = 0 → 每次都检查
        let (tab0, _tab1) = make_two_mocks();
        let client = FailoverChatClient::new(vec![&tab0], 0, 3, 30, 0);
        client.send_message("hello", 60).await.unwrap();
        assert_eq!(tab0.health_check_call_count(), 1);
        client.send_message("hello2", 60).await.unwrap();
        assert_eq!(tab0.health_check_call_count(), 2);
    }

    #[tokio::test]
    async fn test_health_check_interval_triggers_after_n_turns() {
        // health_check_interval = 2
        let (tab0, _tab1) = make_two_mocks();
        let client = FailoverChatClient::new(vec![&tab0], 0, 3, 30, 2);

        // turn=0, last_check=0 → 0-0=0 < 2 → 跳过
        client.send_message("msg1", 60).await.unwrap();
        assert_eq!(tab0.health_check_call_count(), 0);

        // turn=1, last_check=0 → 1-0=1 < 2 → 跳过
        client.send_message("msg2", 60).await.unwrap();
        assert_eq!(tab0.health_check_call_count(), 0);

        // turn=2, last_check=0 → 2-0=2 >= 2 → 检查
        client.send_message("msg3", 60).await.unwrap();
        assert_eq!(tab0.health_check_call_count(), 1);
    }

    // ===== 多网站自动切换测试 =====

    #[tokio::test]
    async fn test_failover_switch_on_unhealthy() {
        // tab0 健康 → tab0 不健康 → 切换到 tab1
        let tab0 = MockFailoverClient::new(SiteType::Zai, vec!["zai response".to_string()])
            .with_default_health(HealthCheckResult::new(SiteHealthStatus::NotLoggedIn));

        let tab1 =
            MockFailoverClient::new(SiteType::DeepSeek, vec!["deepseek response".to_string()])
                .with_default_health(HealthCheckResult::new(SiteHealthStatus::Healthy));

        let client = FailoverChatClient::new(vec![&tab0, &tab1], 0, 3, 0, 0);

        // 发送消息 → 健康检查发现 tab0 不健康 → 切换到 tab1 → tab1 发送
        let result = client.send_message("hello", 60).await.unwrap();
        assert_eq!(result.text, "deepseek response");
        assert_eq!(client.current_tab_index(), 1);

        // tab0 应该收到了原始消息 (但切换后重试)
        // 注意: tab0 的 send_message 不会被调用, 因为健康检查在 send 之前
        // tab1 应该收到了消息
        assert_eq!(tab1.sent_messages(), vec!["hello".to_string()]);
    }

    #[tokio::test]
    async fn test_failover_switch_on_send_error() {
        // tab0 send_message 失败 → 切换到 tab1
        let tab0 = MockFailoverClient::new(SiteType::Zai, vec![])
            .with_default_health(HealthCheckResult::new(SiteHealthStatus::Healthy))
            .with_force_error();

        let tab1 =
            MockFailoverClient::new(SiteType::DeepSeek, vec!["deepseek response".to_string()])
                .with_default_health(HealthCheckResult::new(SiteHealthStatus::Healthy));

        let client = FailoverChatClient::new(vec![&tab0, &tab1], 0, 3, 0, 0);

        // send_message 失败 → 尝试切换 → tab1 重试成功
        let result = client.send_message("hello", 60).await.unwrap();
        assert_eq!(result.text, "deepseek response");
        assert_eq!(client.current_tab_index(), 1);
        assert_eq!(tab0.sent_messages(), vec!["hello".to_string()]);
        assert_eq!(tab1.sent_messages(), vec!["hello".to_string()]);
    }

    #[tokio::test]
    async fn test_failover_no_switch_when_healthy() {
        let tab0 = MockFailoverClient::new(SiteType::Zai, vec!["zai response".to_string()])
            .with_default_health(HealthCheckResult::new(SiteHealthStatus::Healthy));

        let tab1 = MockFailoverClient::new(SiteType::DeepSeek, vec![]);

        let client = FailoverChatClient::new(vec![&tab0, &tab1], 0, 3, 30, 0);

        let result = client.send_message("hello", 60).await.unwrap();
        assert_eq!(result.text, "zai response");
        assert_eq!(client.current_tab_index(), 0); // 没有切换
        assert_eq!(tab0.sent_messages(), vec!["hello".to_string()]);
        assert!(tab1.sent_messages().is_empty());
    }

    #[tokio::test]
    async fn test_failover_no_available_tabs_to_switch() {
        // 只有一个标签页, 无法切换
        let tab0 = MockFailoverClient::new(SiteType::Zai, vec![]).with_force_error();

        let client = FailoverChatClient::new(vec![&tab0], 0, 3, 0, 0);

        // send_message 失败 → 尝试切换 → 无可用标签页 → 返回错误
        let result = client.send_message("hello", 60).await;
        assert!(result.is_err());
        assert_eq!(client.current_tab_index(), 0); // 没有切换
    }

    // ===== 性能统计测试 =====

    #[tokio::test]
    async fn test_stats_after_successful_send() {
        let (tab0, _tab1) = make_two_mocks();
        let client = FailoverChatClient::new(vec![&tab0], 0, 3, 30, 0);

        client.send_message("hello", 60).await.unwrap();

        let stats = client.get_stats().await;
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].0, 0); // idx
        assert_eq!(stats[0].1, SiteType::Zai); // site_type
        assert_eq!(stats[0].2.total_sends, 1);
        assert_eq!(stats[0].2.success_count, 1);
        assert_eq!(stats[0].2.failure_count, 0);
    }

    #[tokio::test]
    async fn test_stats_after_failed_send() {
        let tab0 = MockFailoverClient::new(SiteType::Zai, vec![]).with_force_error();
        let client = FailoverChatClient::new(vec![&tab0], 0, 3, 0, 0);

        let _ = client.send_message("hello", 60).await;

        let stats = client.get_stats().await;
        assert_eq!(stats[0].2.total_sends, 1);
        assert_eq!(stats[0].2.failure_count, 1);
        assert_eq!(stats[0].2.success_count, 0);
    }

    #[tokio::test]
    async fn test_stats_after_failover() {
        let tab0 = MockFailoverClient::new(SiteType::Zai, vec![])
            .with_default_health(HealthCheckResult::new(SiteHealthStatus::NotLoggedIn));
        let tab1 =
            MockFailoverClient::new(SiteType::DeepSeek, vec!["deepseek response".to_string()])
                .with_default_health(HealthCheckResult::new(SiteHealthStatus::Healthy));

        let client = FailoverChatClient::new(vec![&tab0, &tab1], 0, 3, 0, 0);

        client.send_message("hello", 60).await.unwrap();

        let stats = client.get_stats().await;
        // tab0: 健康检查不健康, failover_from +1
        assert_eq!(stats[0].2.failover_from_count, 1);
        assert_eq!(stats[0].2.health_checks, 1);
        assert_eq!(stats[0].2.healthy_count, 0);
        // tab1: 被切换到, 成功发送
        assert_eq!(stats[1].2.failover_to_count, 1);
        assert_eq!(stats[1].2.success_count, 1);
    }

    // ===== DevTrace 集成测试 =====

    /// 辅助函数: 创建临时 DevTraceWriter
    fn make_trace_writer() -> (tempfile::TempDir, DevTraceWriter) {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
        let writer = DevTraceWriter::new(dir.path());
        (dir, writer)
    }

    #[tokio::test]
    async fn test_devtrace_health_check_written() {
        let (_dir, writer) = make_trace_writer();
        let (tab0, _tab1) = make_two_mocks();
        let client = FailoverChatClient::new(vec![&tab0], 0, 3, 30, 0);
        client.set_dev_trace(writer.clone());

        client.send_message("hello", 60).await.unwrap();

        let entries = writer.read_all().unwrap();
        // 应包含至少一条 HealthCheck 条目
        let health_entries: Vec<_> = entries
            .iter()
            .filter(|e| e.action == TraceAction::HealthCheck)
            .collect();
        assert!(
            !health_entries.is_empty(),
            "DevTrace 应包含 HealthCheck 条目"
        );
        assert!(health_entries[0].input_summary.contains("Z.ai"));
    }

    #[tokio::test]
    async fn test_devtrace_failover_written() {
        let (_dir, writer) = make_trace_writer();
        let tab0 = MockFailoverClient::new(SiteType::Zai, vec![])
            .with_default_health(HealthCheckResult::new(SiteHealthStatus::NotLoggedIn));
        let tab1 = MockFailoverClient::new(SiteType::DeepSeek, vec!["response".to_string()])
            .with_default_health(HealthCheckResult::new(SiteHealthStatus::Healthy));

        let client = FailoverChatClient::new(vec![&tab0, &tab1], 0, 3, 0, 0);
        client.set_dev_trace(writer.clone());

        client.send_message("hello", 60).await.unwrap();

        let entries = writer.read_all().unwrap();
        // 应包含 HealthCheck + SiteFailover 条目
        let has_health = entries.iter().any(|e| e.action == TraceAction::HealthCheck);
        let has_failover = entries
            .iter()
            .any(|e| e.action == TraceAction::SiteFailover);
        assert!(has_health, "DevTrace 应包含 HealthCheck 条目");
        assert!(has_failover, "DevTrace 应包含 SiteFailover 条目");

        // 验证 SiteFailover 条目包含切换信息
        let failover_entries: Vec<_> = entries
            .iter()
            .filter(|e| e.action == TraceAction::SiteFailover)
            .collect();
        assert!(!failover_entries.is_empty());
        assert!(failover_entries[0].input_summary.contains("Z.ai"));
        assert!(failover_entries[0].input_summary.contains("DeepSeek"));
        assert!(failover_entries[0].success);
    }

    #[tokio::test]
    async fn test_devtrace_failover_failure_written() {
        let (_dir, writer) = make_trace_writer();
        // 只有一个标签页, 切换会失败
        let tab0 = MockFailoverClient::new(SiteType::Zai, vec![]).with_force_error();

        let client = FailoverChatClient::new(vec![&tab0], 0, 3, 0, 0);
        client.set_dev_trace(writer.clone());

        let _ = client.send_message("hello", 60).await;

        let entries = writer.read_all().unwrap();
        let failover_entries: Vec<_> = entries
            .iter()
            .filter(|e| e.action == TraceAction::SiteFailover)
            .collect();
        assert!(!failover_entries.is_empty());
        assert!(!failover_entries[0].success);
        assert_eq!(failover_entries[0].output_summary, "无法切换");
    }

    #[tokio::test]
    async fn test_devtrace_performance_stats_written() {
        let (_dir, writer) = make_trace_writer();
        let (tab0, _tab1) = make_two_mocks();
        let client = FailoverChatClient::new(vec![&tab0], 0, 3, 30, 0);
        client.set_dev_trace(writer.clone());

        client.send_message("hello", 60).await.unwrap();
        client.write_final_trace().await;

        let entries = writer.read_all().unwrap();
        let perf_entries: Vec<_> = entries
            .iter()
            .filter(|e| e.action == TraceAction::PerformanceStats)
            .collect();
        assert!(
            !perf_entries.is_empty(),
            "DevTrace 应包含 PerformanceStats 条目"
        );
        assert!(perf_entries[0].input_summary.contains("Z.ai"));
        assert!(perf_entries[0].output_summary.contains("发送:1"));
        assert!(perf_entries[0].output_summary.contains("成功:1"));
    }

    #[tokio::test]
    async fn test_devtrace_no_writer_no_crash() {
        // 不设置 DevTraceWriter, 确保不崩溃
        let (tab0, _tab1) = make_two_mocks();
        let client = FailoverChatClient::new(vec![&tab0], 0, 3, 30, 0);

        // 不调用 set_dev_trace
        client.send_message("hello", 60).await.unwrap();
        client.write_final_trace().await;
        // 不应 panic
    }

    #[tokio::test]
    async fn test_devtrace_multiple_sends_accumulate_entries() {
        let (_dir, writer) = make_trace_writer();
        let (tab0, _tab1) = make_two_mocks();
        let client = FailoverChatClient::new(vec![&tab0], 0, 3, 30, 0);
        client.set_dev_trace(writer.clone());

        // 发送 3 条消息
        for i in 0..3 {
            client.send_message(&format!("msg{}", i), 60).await.unwrap();
        }

        let entries = writer.read_all().unwrap();
        // 每次发送都会触发一次健康检查 (interval=0)
        let health_count = entries
            .iter()
            .filter(|e| e.action == TraceAction::HealthCheck)
            .count();
        assert_eq!(health_count, 3);
    }

    #[tokio::test]
    async fn test_devtrace_shared_writer_orchestrator_simulation() {
        // 模拟 Orchestrator + FailoverChatClient 共享同一 DevTraceWriter
        let (_dir, writer) = make_trace_writer();

        // Orchestrator 端: 写入 Planning
        writer
            .trace(
                TraceAction::Planning,
                None,
                None,
                None,
                "拆解目标",
                "3阶段5任务",
                5000,
                true,
                None,
            )
            .unwrap();

        // FailoverChatClient 端: 写入 HealthCheck + SiteFailover
        let tab0 = MockFailoverClient::new(SiteType::Zai, vec!["response".to_string()])
            .with_default_health(HealthCheckResult::new(SiteHealthStatus::RateLimited));
        let tab1 =
            MockFailoverClient::new(SiteType::DeepSeek, vec!["deepseek response".to_string()])
                .with_default_health(HealthCheckResult::new(SiteHealthStatus::Healthy));

        let client = FailoverChatClient::new(vec![&tab0, &tab1], 0, 3, 0, 0);
        client.set_dev_trace(writer.clone());

        client.send_message("hello", 60).await.unwrap();
        client.write_final_trace().await;

        // Orchestrator 端: 写入 TaskExecution
        writer
            .trace(
                TraceAction::TaskExecution,
                Some(0),
                Some(0),
                Some("初始化"),
                "创建项目",
                "main.rs",
                3000,
                true,
                None,
            )
            .unwrap();

        // 验证所有条目在同一文件中
        let entries = writer.read_all().unwrap();
        let actions: Vec<_> = entries.iter().map(|e| e.action).collect();
        assert!(actions.contains(&TraceAction::Planning));
        assert!(actions.contains(&TraceAction::HealthCheck));
        assert!(actions.contains(&TraceAction::SiteFailover));
        assert!(actions.contains(&TraceAction::PerformanceStats));
        assert!(actions.contains(&TraceAction::TaskExecution));
    }

    #[tokio::test]
    async fn test_start_new_conversation_delegates() {
        let (tab0, _tab1) = make_two_mocks();
        let client = FailoverChatClient::new(vec![&tab0], 0, 3, 30, 0);

        // start_new_conversation 默认实现返回 Ok(())
        client.start_new_conversation().await.unwrap();
    }

    #[tokio::test]
    async fn test_failover_success_resets_failure_count() {
        // 成功发送后, failover 状态应该重置
        let tab0 = MockFailoverClient::new(
            SiteType::Zai,
            vec!["response1".to_string(), "response2".to_string()],
        )
        .with_default_health(HealthCheckResult::new(SiteHealthStatus::Healthy));

        let client = FailoverChatClient::new(vec![&tab0], 0, 3, 30, 0);

        client.send_message("msg1", 60).await.unwrap();
        client.send_message("msg2", 60).await.unwrap();

        let stats = client.get_stats().await;
        assert_eq!(stats[0].2.success_count, 2);
    }

    #[tokio::test]
    async fn test_failover_multiple_switches() {
        // 三个标签页, 前两个不健康, 第三个健康
        let tab0 = MockFailoverClient::new(SiteType::Zai, vec![])
            .with_default_health(HealthCheckResult::new(SiteHealthStatus::NotLoggedIn));
        let tab1 = MockFailoverClient::new(SiteType::DeepSeek, vec![])
            .with_default_health(HealthCheckResult::new(SiteHealthStatus::RateLimited));
        let tab2 = MockFailoverClient::new(SiteType::Kimi, vec!["kimi response".to_string()])
            .with_default_health(HealthCheckResult::new(SiteHealthStatus::Healthy));

        let client = FailoverChatClient::new(vec![&tab0, &tab1, &tab2], 0, 3, 0, 0);

        // 第一次发送: tab0 不健康 → 切换到 tab1 → tab1 也不健康 → tab1.send 成功? 不对
        // 健康检查只检查当前 tab, 切换后直接发送, 不再检查新 tab 的健康
        // 所以: tab0 不健康 → 切换到 tab1 → tab1.send_message 成功
        // 但 tab1 的 responses 为空, 会返回 "(empty)"
        let result = client.send_message("hello", 60).await.unwrap();
        assert_eq!(result.text, "(empty)");
        assert_eq!(client.current_tab_index(), 1);
    }

    // ===== upload_files 测试 =====

    #[tokio::test]
    async fn test_mock_upload_files_records() {
        let tab = MockFailoverClient::new(SiteType::Zai, vec![]);
        tab.upload_files(&["/tmp/screenshot.png", "/tmp/doc.pdf"])
            .await
            .unwrap();

        let uploaded = tab.uploaded_files();
        assert_eq!(uploaded.len(), 1, "应记录一次 upload_files 调用");
        assert_eq!(uploaded[0].len(), 2, "应上传 2 个文件");
        assert_eq!(uploaded[0][0], "/tmp/screenshot.png");
        assert_eq!(uploaded[0][1], "/tmp/doc.pdf");
    }

    #[tokio::test]
    async fn test_mock_upload_files_empty() {
        let tab = MockFailoverClient::new(SiteType::Zai, vec![]);
        tab.upload_files(&[]).await.unwrap();

        let uploaded = tab.uploaded_files();
        assert_eq!(uploaded.len(), 1, "应记录一次调用 (即使空列表)");
        assert_eq!(uploaded[0].len(), 0, "文件列表应为空");
    }

    #[tokio::test]
    async fn test_mock_upload_files_multiple_calls() {
        let tab = MockFailoverClient::new(SiteType::Zai, vec![]);
        tab.upload_files(&["/tmp/a.png"]).await.unwrap();
        tab.upload_files(&["/tmp/b.png", "/tmp/c.png"])
            .await
            .unwrap();

        let uploaded = tab.uploaded_files();
        assert_eq!(uploaded.len(), 2, "应记录两次调用");
        assert_eq!(uploaded[0].len(), 1);
        assert_eq!(uploaded[1].len(), 2);
    }

    #[tokio::test]
    async fn test_failover_upload_files_delegates() {
        // upload_files 应委托给当前标签页
        let (tab0, _tab1) = make_two_mocks();
        let client = FailoverChatClient::new(vec![&tab0], 0, 3, 30, 0);

        client.upload_files(&["/tmp/test.png"]).await.unwrap();

        let uploaded = tab0.uploaded_files();
        assert_eq!(
            uploaded.len(),
            1,
            "FailoverChatClient 应委托 upload_files 给 tab0"
        );
        assert_eq!(uploaded[0][0], "/tmp/test.png");
    }

    #[tokio::test]
    async fn test_failover_upload_files_after_switch() {
        // 故障切换后, upload_files 应委托给新的当前标签页
        let tab0 = MockFailoverClient::new(SiteType::Zai, vec!["resp".to_string()])
            .with_default_health(HealthCheckResult::new(SiteHealthStatus::NotLoggedIn));
        let tab1 = MockFailoverClient::new(SiteType::DeepSeek, vec!["resp2".to_string()])
            .with_default_health(HealthCheckResult::new(SiteHealthStatus::Healthy));

        let client = FailoverChatClient::new(vec![&tab0, &tab1], 0, 3, 0, 0);

        // 发送消息触发故障切换: tab0 不健康 → 切换到 tab1
        client.send_message("hello", 60).await.unwrap();
        assert_eq!(client.current_tab_index(), 1);

        // upload_files 应委托给 tab1 (当前标签页)
        client.upload_files(&["/tmp/screenshot.png"]).await.unwrap();

        // tab0 不应有上传记录
        assert_eq!(tab0.uploaded_files().len(), 0, "tab0 不应有上传记录");
        // tab1 应有上传记录
        assert_eq!(tab1.uploaded_files().len(), 1, "tab1 应有上传记录");
        assert_eq!(tab1.uploaded_files()[0][0], "/tmp/screenshot.png");
    }

    // ===== 主动限流检测测试 (第 39 项改进) =====

    #[tokio::test]
    async fn test_proactive_rate_limit_detection_switches() {
        // Z.ai 回复包含限流文本 → 主动切换到 DeepSeek
        let tab0 =
            MockFailoverClient::new(SiteType::Zai, vec!["回复内容为空，请稍后重试".to_string()])
                .with_default_health(HealthCheckResult::new(SiteHealthStatus::Healthy));
        let tab1 = MockFailoverClient::new(
            SiteType::DeepSeek,
            vec!["正常的 DeepSeek 代码回复".to_string()],
        )
        .with_default_health(HealthCheckResult::new(SiteHealthStatus::Healthy));

        let client = FailoverChatClient::new(vec![&tab0, &tab1], 0, 3, 0, 0);

        let result = client.send_message("hello", 60).await.unwrap();
        // 应切换到 tab1 并返回 DeepSeek 的回复
        assert_eq!(client.current_tab_index(), 1, "应切换到 DeepSeek (tab 1)");
        assert_eq!(result.text, "正常的 DeepSeek 代码回复");
    }

    #[tokio::test]
    async fn test_proactive_rate_limit_no_switch_single_tab() {
        // 只有一个标签页, 限流回复无法切换 → 返回原始限流文本
        let tab0 = MockFailoverClient::new(SiteType::Zai, vec!["请求过于频繁".to_string()])
            .with_default_health(HealthCheckResult::new(SiteHealthStatus::Healthy));

        let client = FailoverChatClient::new(vec![&tab0], 0, 3, 0, 0);

        let result = client.send_message("hello", 60).await.unwrap();
        // 无法切换, 返回原始限流文本
        assert_eq!(result.text, "请求过于频繁");
        assert_eq!(client.current_tab_index(), 0, "只有一个标签页, 不应切换");
    }

    #[tokio::test]
    async fn test_proactive_rate_limit_normal_response_no_switch() {
        // 正常代码回复不应触发切换
        let tab0 = MockFailoverClient::new(
            SiteType::Zai,
            vec!["file:src/main.rs\nfn main() {}".to_string()],
        )
        .with_default_health(HealthCheckResult::new(SiteHealthStatus::Healthy));
        let tab1 =
            MockFailoverClient::new(SiteType::DeepSeek, vec!["deepseek response".to_string()])
                .with_default_health(HealthCheckResult::new(SiteHealthStatus::Healthy));

        let client = FailoverChatClient::new(vec![&tab0, &tab1], 0, 3, 30, 0);

        let result = client.send_message("hello", 60).await.unwrap();
        // 正常回复, 不应切换
        assert_eq!(client.current_tab_index(), 0, "正常回复不应切换");
        assert!(result.text.contains("fn main"));
    }

    #[tokio::test]
    async fn test_proactive_rate_limit_records_failure_stats() {
        // 限流切换后, 原标签页应记录失败
        let tab0 =
            MockFailoverClient::new(SiteType::Zai, vec!["回复内容为空，请稍后重试".to_string()])
                .with_default_health(HealthCheckResult::new(SiteHealthStatus::Healthy));
        let tab1 = MockFailoverClient::new(SiteType::DeepSeek, vec!["正常回复".to_string()])
            .with_default_health(HealthCheckResult::new(SiteHealthStatus::Healthy));

        let client = FailoverChatClient::new(vec![&tab0, &tab1], 0, 3, 0, 0);

        client.send_message("hello", 60).await.unwrap();

        let stats = client.get_stats().await;
        // Z.ai (tab 0) 应有 1 次失败 (限流)
        assert_eq!(stats[0].2.failure_count, 1, "Z.ai 应记录 1 次限流失败");
        assert_eq!(stats[0].2.success_count, 0, "Z.ai 不应有成功记录");
        // DeepSeek (tab 1) 的重试直接调用 MockFailoverClient, 不经过 FailoverChatClient 统计
        // 所以 success_count 仍为 0 (与 error retry 路径行为一致)
        assert_eq!(
            stats[1].2.failover_to_count, 1,
            "DeepSeek 应有 1 次切入记录"
        );
    }

    // ===== 纯逻辑函数测试 (第 54 项) =====

    // --- should_failover_decision 测试 ---

    #[test]
    fn test_should_failover_decision_healthy() {
        let health = HealthCheckResult::new(SiteHealthStatus::Healthy);
        assert!(!should_failover_decision(&health));
    }

    #[test]
    fn test_should_failover_decision_not_logged_in() {
        let health = HealthCheckResult::new(SiteHealthStatus::NotLoggedIn);
        assert!(should_failover_decision(&health));
    }

    #[test]
    fn test_should_failover_decision_rate_limited() {
        let health = HealthCheckResult::new(SiteHealthStatus::RateLimited);
        assert!(should_failover_decision(&health));
    }

    #[test]
    fn test_should_failover_decision_under_maintenance() {
        let health = HealthCheckResult::new(SiteHealthStatus::UnderMaintenance);
        assert!(should_failover_decision(&health));
    }

    #[test]
    fn test_should_failover_decision_network_error() {
        let health = HealthCheckResult::new(SiteHealthStatus::NetworkError);
        assert!(should_failover_decision(&health));
    }

    #[test]
    fn test_should_failover_decision_unknown() {
        // Unknown: 不健康但不触发 failover → false
        let health = HealthCheckResult::new(SiteHealthStatus::Unknown);
        assert!(!should_failover_decision(&health));
    }

    #[test]
    fn test_should_failover_decision_with_message() {
        // 带有消息的结果不影响决策
        let health = HealthCheckResult {
            status: SiteHealthStatus::RateLimited,
            timestamp: 12345,
            message: Some("请求过于频繁".to_string()),
            current_url: Some("https://chat.z.ai".to_string()),
            check_duration_ms: 50,
        };
        assert!(should_failover_decision(&health));
    }

    // --- build_error_health_result 测试 ---

    #[test]
    fn test_build_error_health_result_basic() {
        let result = build_error_health_result("connection refused".to_string());
        assert_eq!(result.status, SiteHealthStatus::NetworkError);
        assert_eq!(result.message.as_deref(), Some("connection refused"));
        assert!(result.current_url.is_none());
        assert_eq!(result.check_duration_ms, 0);
    }

    #[test]
    fn test_build_error_health_result_empty_message() {
        let result = build_error_health_result(String::new());
        assert_eq!(result.status, SiteHealthStatus::NetworkError);
        assert_eq!(result.message.as_deref(), Some(""));
    }

    #[test]
    fn test_build_error_health_result_long_message() {
        let long_msg = "x".repeat(1000);
        let result = build_error_health_result(long_msg.clone());
        assert_eq!(result.message.as_deref(), Some(long_msg.as_str()));
    }

    #[test]
    fn test_build_error_health_result_unicode() {
        let result = build_error_health_result("连接被拒绝".to_string());
        assert_eq!(result.message.as_deref(), Some("连接被拒绝"));
    }

    #[test]
    fn test_build_error_health_result_timestamp_positive() {
        // 时间戳应该是合理的 (非零, 在当前时间附近)
        let result = build_error_health_result("error".to_string());
        assert!(result.timestamp > 0);
    }

    // --- classify_failover_failure_reason 测试 ---

    #[test]
    fn test_classify_failover_reason_all_tried() {
        assert_eq!(
            classify_failover_failure_reason(true, 0, 3),
            "所有标签页都已尝试"
        );
    }

    #[test]
    fn test_classify_failover_reason_max_failures() {
        assert_eq!(
            classify_failover_failure_reason(false, 3, 3),
            "超过最大连续失败次数"
        );
    }

    #[test]
    fn test_classify_failover_reason_exceeds_max_failures() {
        assert_eq!(
            classify_failover_failure_reason(false, 5, 3),
            "超过最大连续失败次数"
        );
    }

    #[test]
    fn test_classify_failover_reason_no_available() {
        assert_eq!(
            classify_failover_failure_reason(false, 0, 3),
            "无可用标签页"
        );
    }

    #[test]
    fn test_classify_failover_reason_below_max() {
        assert_eq!(
            classify_failover_failure_reason(false, 1, 3),
            "无可用标签页"
        );
    }

    #[test]
    fn test_classify_failover_reason_all_tried_takes_priority() {
        // all_tried 优先于 max_failures
        assert_eq!(
            classify_failover_failure_reason(true, 5, 3),
            "所有标签页都已尝试"
        );
    }

    // --- calculate_health_check_interval_elapsed 测试 ---

    #[test]
    fn test_health_check_interval_elapsed_zero_always_checks() {
        assert!(calculate_health_check_interval_elapsed(0, 0, 0));
        assert!(calculate_health_check_interval_elapsed(1, 0, 0));
        assert!(calculate_health_check_interval_elapsed(100, 50, 0));
    }

    #[test]
    fn test_health_check_interval_exact_match() {
        // current - last == interval → 检查
        assert!(calculate_health_check_interval_elapsed(5, 0, 5));
        assert!(calculate_health_check_interval_elapsed(10, 7, 3));
    }

    #[test]
    fn test_health_check_interval_above_threshold() {
        // current - last > interval → 检查
        assert!(calculate_health_check_interval_elapsed(10, 0, 5));
        assert!(calculate_health_check_interval_elapsed(100, 0, 10));
    }

    #[test]
    fn test_health_check_interval_below_threshold() {
        // current - last < interval → 跳过
        assert!(!calculate_health_check_interval_elapsed(1, 0, 5));
        assert!(!calculate_health_check_interval_elapsed(4, 0, 5));
    }

    #[test]
    fn test_health_check_interval_same_turn() {
        // current == last → 0 < interval (interval > 0) → 跳过
        assert!(!calculate_health_check_interval_elapsed(5, 5, 3));
    }

    #[test]
    fn test_health_check_interval_last_greater_than_current() {
        // last > current (不会在正常流程中出现, 但不应 panic)
        // saturating_sub 防止下溢 → 0 < interval → 跳过
        assert!(!calculate_health_check_interval_elapsed(3, 5, 2));
    }

    #[test]
    fn test_health_check_interval_large_interval() {
        assert!(!calculate_health_check_interval_elapsed(1, 0, 1000));
        assert!(calculate_health_check_interval_elapsed(1000, 0, 1000));
    }

    // --- update_min_response_time 测试 ---

    #[test]
    fn test_update_min_response_time_initial() {
        // current_min=0 (初始值) → 返回 new_duration
        assert_eq!(update_min_response_time(0, 150), 150);
    }

    #[test]
    fn test_update_min_response_time_smaller() {
        // new < current → 返回 new
        assert_eq!(update_min_response_time(150, 100), 100);
    }

    #[test]
    fn test_update_min_response_time_larger() {
        // new > current → 返回 current
        assert_eq!(update_min_response_time(100, 200), 100);
    }

    #[test]
    fn test_update_min_response_time_equal() {
        // new == current → 返回 current (不变)
        assert_eq!(update_min_response_time(100, 100), 100);
    }

    #[test]
    fn test_update_min_response_time_both_zero() {
        assert_eq!(update_min_response_time(0, 0), 0);
    }

    #[test]
    fn test_update_min_response_time_zero_current_zero_new() {
        // 0 初始值 + 0 duration → 0
        assert_eq!(update_min_response_time(0, 0), 0);
    }

    #[test]
    fn test_update_min_response_time_sequence() {
        // 模拟连续调用: 初始 → 200 → 150 → 100 → 120 (不变)
        let mut min = 0u64;
        min = update_min_response_time(min, 200);
        assert_eq!(min, 200);
        min = update_min_response_time(min, 150);
        assert_eq!(min, 150);
        min = update_min_response_time(min, 100);
        assert_eq!(min, 100);
        min = update_min_response_time(min, 120);
        assert_eq!(min, 100);
    }

    // --- format_switch_trace 测试 ---

    #[test]
    fn test_format_switch_trace_basic() {
        let msg = format_switch_trace(0, SiteType::Zai, 1, SiteType::DeepSeek);
        assert!(msg.contains("[0]"));
        assert!(msg.contains("Z.ai"));
        assert!(msg.contains("[1]"));
        assert!(msg.contains("DeepSeek"));
        assert!(msg.contains("→"));
    }

    #[test]
    fn test_format_switch_trace_same_site_type() {
        let msg = format_switch_trace(0, SiteType::Zai, 2, SiteType::Zai);
        assert!(msg.contains("[0]"));
        assert!(msg.contains("[2]"));
        // 同类型 → 两次 Z.ai
        assert_eq!(msg.matches("Z.ai").count(), 2);
    }

    #[test]
    fn test_format_switch_trace_kimi_to_tongyi() {
        let msg = format_switch_trace(1, SiteType::Kimi, 3, SiteType::Tongyi);
        assert!(msg.contains("Kimi"));
        assert!(msg.contains("通义千问"));
    }

    #[test]
    fn test_format_switch_trace_large_indices() {
        let msg = format_switch_trace(999, SiteType::Zai, 1000, SiteType::DeepSeek);
        assert!(msg.contains("[999]"));
        assert!(msg.contains("[1000]"));
    }

    #[test]
    fn test_format_switch_trace_unknown_site() {
        let msg = format_switch_trace(0, SiteType::Unknown, 1, SiteType::Unknown);
        assert!(msg.contains("未知网站"));
    }

    // --- format_failover_failure_trace 测试 ---

    #[test]
    fn test_format_failover_failure_trace_basic() {
        let msg = format_failover_failure_trace(0, SiteType::Zai);
        assert!(msg.contains("[0]"));
        assert!(msg.contains("Z.ai"));
        assert!(msg.contains("尝试从"));
    }

    #[test]
    fn test_format_failover_failure_trace_deepseek() {
        let msg = format_failover_failure_trace(1, SiteType::DeepSeek);
        assert!(msg.contains("[1]"));
        assert!(msg.contains("DeepSeek"));
    }

    #[test]
    fn test_format_failover_failure_trace_large_index() {
        let msg = format_failover_failure_trace(999, SiteType::Kimi);
        assert!(msg.contains("[999]"));
        assert!(msg.contains("Kimi"));
    }

    #[test]
    fn test_format_failover_failure_trace_unknown_site() {
        let msg = format_failover_failure_trace(0, SiteType::Unknown);
        assert!(msg.contains("未知网站"));
    }

    // --- 纯逻辑函数与 SitePerformanceStats 集成测试 ---

    #[test]
    fn test_update_min_response_time_with_stats() {
        // 验证纯函数与 SitePerformanceStats::record_success 的一致性
        let mut stats = SitePerformanceStats::default();

        // 第一次成功: 150ms
        stats.record_success(150);
        assert_eq!(stats.min_response_ms, update_min_response_time(0, 150));

        // 第二次成功: 100ms (更小)
        stats.record_success(100);
        assert_eq!(stats.min_response_ms, update_min_response_time(150, 100));

        // 第三次成功: 200ms (更大, 不变)
        stats.record_success(200);
        assert_eq!(stats.min_response_ms, update_min_response_time(100, 200));
    }

    #[test]
    fn test_classify_failover_reason_matches_try_switch_logic() {
        // 验证纯函数与 try_switch 中实际的失败分类逻辑一致
        let all_tried = true;
        let consecutive_failures = 0;
        let max_failures = 3;

        let reason =
            classify_failover_failure_reason(all_tried, consecutive_failures, max_failures);

        // all_tried=true → "所有标签页都已尝试"
        assert_eq!(reason, "所有标签页都已尝试");

        // all_tried=false, consecutive >= max → "超过最大连续失败次数"
        let reason2 = classify_failover_failure_reason(false, 3, 3);
        assert_eq!(reason2, "超过最大连续失败次数");
    }
}
