//! 结构化开发追踪 — 借鉴方向 4
//!
//! 为 24 小时不间断运行提供可观测性, 记录每一轮 AI 交互的详细信息
//! (时间戳、阶段、任务、操作类型、输入摘要、输出摘要、结果),
//! 持久化到 `.forge/devtrace.jsonl` (JSON Lines 格式, 便于流式写入和后续分析)。
//!
//! ## 核心思路
//!
//! 24 小时运行后, 人类需要知道这 24 小时 Forge 做了什么。
//! 没有结构化追踪, 24 小时的运行结果就是一个黑箱。
//! DevTrace 提供时间线视图, 让人类可以快速了解:
//! 哪些任务成功了、哪些失败了、每轮交互花了多长时间、AI 的回复质量如何。
//!
//! ## 与现有机制的关系
//!
//! - **Memory (memory.json)**: 记录宏观状态 (阶段/任务/决策/对话历史)
//! - **DevTrace (devtrace.jsonl)**: 记录微观时间线 (每一轮交互的详细 trace)
//! - **ErrorHistory (error_history.json)**: 记录错误模式 (用于智能错误诊断)
//! - 三者互补: Memory 是"做了什么", DevTrace 是"什么时候怎么做的",
//!   ErrorHistory 是"犯了什么错"
//!
//! ## JSONL 格式
//!
//! 每行一个 JSON 对象, 便于流式写入和后续分析:
//! ```jsonl
//! {"timestamp":"2024-01-01T00:00:00Z","action":"Planning","input_summary":"...","output_summary":"...","duration_ms":5000,"success":true}
//! {"timestamp":"2024-01-01T00:05:00Z","action":"TaskExecution","phase_idx":0,"task_idx":0,"task_name":"初始化项目","input_summary":"...","output_summary":"...","duration_ms":3000,"success":true}
//! ```

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use tracing::warn;

use crate::trace_store::{StorageBackend, StorageConfig as TraceStorageConfig};

// ============================================================================
//  TraceAction — 操作类型
// ============================================================================

/// Trace 操作类型 — 标识每一轮交互的操作性质
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TraceAction {
    /// 阶段规划 — AI 拆解终极目标为开发阶段
    Planning,
    /// 任务执行 — attempt 1, 首次执行任务
    TaskExecution,
    /// 修复尝试 — attempt > 1, 修复编译/测试错误
    FixAttempt,
    /// 自主追问 — 检查 AI 回复后发起追问
    Clarification,
    /// 上下文衔接 — 对话过长时新开对话并交接上下文
    ContextHandoff,
    /// 转向提醒 — 每隔 N 轮注入提醒, 防止 AI 跑偏
    SteerReminder,
    /// 循环终止检测 — 检测到修复死循环, 改变策略
    LoopDetection,
    /// 编译检查 — cargo check / 语言适配器 check
    CompileCheck,
    /// 测试运行 — cargo test / 语言适配器 test
    TestRun,
    /// E2E 测试 — 运行二进制进行端到端测试
    E2ETest,
    /// 需求变更 — 检测到需求变更并重新规划
    RequirementChange,
    /// AI 自主指令 — AI 回复中包含 slash command 并被执行
    SlashCommand,
    /// 自动恢复 — Chrome 断连后自动重连 (24h 可靠性)
    Recovery,
    /// 网站健康检查 — 检测网站是否健康 (登录/限流/维护)
    HealthCheck,
    /// 网站自动切换 — 主网站不健康时切换到备用标签页
    SiteFailover,
    /// 性能统计 — 运行结束后写入各网站性能统计摘要
    PerformanceStats,
    /// 网页搜索 — AI 通过 /search 指令主动搜索网页/查阅文档
    WebSearch,
    /// 增量发送 — LiveContinuation / RadixTree 增量发送统计
    IncrementalSend,
    /// 缓存调优 — CacheTuner 自动评估并调整缓存 TTL 或禁用缓存 (Session 82)
    CacheTuning,
    /// 搜索质量评估 — SearchQualityEvaluator 评估搜索效果并自动禁用 (Session 85)
    SearchQuality,
    /// Memory 上下文注入 — 修复轮次中注入 Memory 对话历史 (Session 90)
    MemoryInjection,
    /// Memory 评估 — MemoryContextEvaluator 评估注入效果并自动禁用 (Session 90)
    MemoryEvaluation,
}

impl std::fmt::Display for TraceAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TraceAction::Planning => write!(f, "Planning"),
            TraceAction::TaskExecution => write!(f, "TaskExecution"),
            TraceAction::FixAttempt => write!(f, "FixAttempt"),
            TraceAction::Clarification => write!(f, "Clarification"),
            TraceAction::ContextHandoff => write!(f, "ContextHandoff"),
            TraceAction::SteerReminder => write!(f, "SteerReminder"),
            TraceAction::LoopDetection => write!(f, "LoopDetection"),
            TraceAction::CompileCheck => write!(f, "CompileCheck"),
            TraceAction::TestRun => write!(f, "TestRun"),
            TraceAction::E2ETest => write!(f, "E2ETest"),
            TraceAction::RequirementChange => write!(f, "RequirementChange"),
            TraceAction::SlashCommand => write!(f, "SlashCommand"),
            TraceAction::Recovery => write!(f, "Recovery"),
            TraceAction::HealthCheck => write!(f, "HealthCheck"),
            TraceAction::SiteFailover => write!(f, "SiteFailover"),
            TraceAction::PerformanceStats => write!(f, "PerformanceStats"),
            TraceAction::WebSearch => write!(f, "WebSearch"),
            TraceAction::IncrementalSend => write!(f, "IncrementalSend"),
            TraceAction::CacheTuning => write!(f, "CacheTuning"),
            TraceAction::SearchQuality => write!(f, "SearchQuality"),
            TraceAction::MemoryInjection => write!(f, "MemoryInjection"),
            TraceAction::MemoryEvaluation => write!(f, "MemoryEvaluation"),
        }
    }
}

impl TraceAction {
    /// 获取操作的中文描述
    pub fn description(&self) -> &'static str {
        match self {
            TraceAction::Planning => "阶段规划",
            TraceAction::TaskExecution => "任务执行",
            TraceAction::FixAttempt => "修复尝试",
            TraceAction::Clarification => "自主追问",
            TraceAction::ContextHandoff => "上下文衔接",
            TraceAction::SteerReminder => "转向提醒",
            TraceAction::LoopDetection => "循环终止检测",
            TraceAction::CompileCheck => "编译检查",
            TraceAction::TestRun => "测试运行",
            TraceAction::E2ETest => "E2E 测试",
            TraceAction::RequirementChange => "需求变更",
            TraceAction::SlashCommand => "AI 自主指令",
            TraceAction::Recovery => "自动恢复",
            TraceAction::HealthCheck => "健康检查",
            TraceAction::SiteFailover => "网站切换",
            TraceAction::PerformanceStats => "性能统计",
            TraceAction::WebSearch => "网页搜索",
            TraceAction::IncrementalSend => "增量发送",
            TraceAction::CacheTuning => "缓存调优",
            TraceAction::SearchQuality => "搜索质量评估",
            TraceAction::MemoryInjection => "Memory 注入",
            TraceAction::MemoryEvaluation => "Memory 评估",
        }
    }

    /// 所有操作类型
    pub fn all() -> Vec<TraceAction> {
        vec![
            TraceAction::Planning,
            TraceAction::TaskExecution,
            TraceAction::FixAttempt,
            TraceAction::Clarification,
            TraceAction::ContextHandoff,
            TraceAction::SteerReminder,
            TraceAction::LoopDetection,
            TraceAction::CompileCheck,
            TraceAction::TestRun,
            TraceAction::E2ETest,
            TraceAction::RequirementChange,
            TraceAction::SlashCommand,
            TraceAction::Recovery,
            TraceAction::HealthCheck,
            TraceAction::SiteFailover,
            TraceAction::PerformanceStats,
            TraceAction::WebSearch,
            TraceAction::IncrementalSend,
            TraceAction::CacheTuning,
            TraceAction::SearchQuality,
            TraceAction::MemoryInjection,
            TraceAction::MemoryEvaluation,
        ]
    }
}

// ============================================================================
//  DevTraceEntry — 单条 Trace 记录
// ============================================================================

/// 单条开发追踪记录 — 记录一轮 AI 交互的详细信息
///
/// 每条记录包含时间戳、操作类型、阶段/任务索引、输入/输出摘要、
/// 耗时和结果, 序列化为 JSONL 格式写入 `.forge/devtrace.jsonl`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevTraceEntry {
    /// 时间戳 (UTC)
    pub timestamp: DateTime<Utc>,
    /// 阶段索引 (None = planning 阶段或全局操作)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase_idx: Option<usize>,
    /// 任务索引 (None = 阶段级操作)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_idx: Option<usize>,
    /// 任务名称 (便于人类阅读)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_name: Option<String>,
    /// 操作类型
    pub action: TraceAction,
    /// 输入摘要 (前 200 字符)
    pub input_summary: String,
    /// 输出摘要 (前 200 字符)
    pub output_summary: String,
    /// 耗时 (毫秒)
    pub duration_ms: u64,
    /// 是否成功
    pub success: bool,
    /// 错误信息 (如有)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl DevTraceEntry {
    /// 创建一条新的 Trace 记录
    ///
    /// 自动截断输入/输出摘要到 200 字符, 设置当前时间戳。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        action: TraceAction,
        phase_idx: Option<usize>,
        task_idx: Option<usize>,
        task_name: Option<&str>,
        input: &str,
        output: &str,
        duration_ms: u64,
        success: bool,
        error: Option<&str>,
    ) -> Self {
        Self {
            timestamp: Utc::now(),
            phase_idx,
            task_idx,
            task_name: task_name.map(String::from),
            action,
            input_summary: truncate_str(input, 200),
            output_summary: truncate_str(output, 200),
            duration_ms,
            success,
            error: error.map(String::from),
        }
    }

    /// 序列化为 JSON 字符串 (单行, 用于 JSONL)
    pub fn to_jsonl(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    /// 从 JSON 字符串反序列化
    pub fn from_jsonl(line: &str) -> Result<Self> {
        Ok(serde_json::from_str(line)?)
    }
}

/// 截断字符串到指定字符数
fn truncate_str(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

// ============================================================================
//  IncrementalStats — 增量发送统计 (Session 75)
// ============================================================================

/// 增量发送统计 — 追踪 LiveContinuation / RadixTree 的节省效果
///
/// 记录每次增量发送的总消息数、实际发送数和跳过数,
/// 提供累计统计和节省比例计算。
///
/// # 设计
///
/// 在 Orchestrator 的 `send_with_continuation` 方法中,
/// 每次发送后更新统计, 并通过 `trace_dev` 写入 DevTrace。
/// 24 小时运行后, 可以从 DevTrace 中查看增量发送的整体效果。
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::IncrementalStats;
/// let mut stats = IncrementalStats::new();
///
/// // 第一次发送: 全量 (5条消息)
/// stats.record(5, 5); // total=5, sent=5, skipped=0
/// assert_eq!(stats.total_messages, 5);
/// assert_eq!(stats.skipped_messages, 0);
/// assert!((stats.saved_ratio() - 0.0).abs() < 0.001);
///
/// // 第二次发送: 增量 (5条消息中3条已发送)
/// stats.record(5, 2); // total=5, sent=2, skipped=3
/// assert_eq!(stats.total_messages, 10);
/// assert_eq!(stats.skipped_messages, 3);
/// assert!((stats.saved_ratio() - 0.3).abs() < 0.001);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IncrementalStats {
    /// 累计总消息数 (含重复)
    pub total_messages: usize,
    /// 累计实际发送消息数 (增量)
    pub sent_messages: usize,
    /// 累计跳过消息数 (已发送过, 复用)
    pub skipped_messages: usize,
    /// 增量发送次数
    pub send_count: usize,
}

impl IncrementalStats {
    /// 创建新的统计计数器
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一次增量发送
    ///
    /// - `total`: 本次发送的总消息数
    /// - `sent`: 本次实际发送的消息数 (增量)
    ///
    /// skipped = total - sent
    pub fn record(&mut self, total: usize, sent: usize) {
        let skipped = total.saturating_sub(sent);
        self.total_messages += total;
        self.sent_messages += sent;
        self.skipped_messages += skipped;
        self.send_count += 1;
    }

    /// 节省比例 (0.0 ~ 1.0)
    ///
    /// 跳过的消息数占总消息数的比例。
    /// 0.0 表示没有节省 (全部为增量发送),
    /// 1.0 表示全部跳过 (无需发送任何消息)。
    pub fn saved_ratio(&self) -> f64 {
        if self.total_messages == 0 {
            return 0.0;
        }
        self.skipped_messages as f64 / self.total_messages as f64
    }

    /// 平均每次发送的消息数
    pub fn avg_messages_per_send(&self) -> f64 {
        if self.send_count == 0 {
            return 0.0;
        }
        self.total_messages as f64 / self.send_count as f64
    }

    /// 平均每次实际发送的消息数 (增量)
    pub fn avg_sent_per_send(&self) -> f64 {
        if self.send_count == 0 {
            return 0.0;
        }
        self.sent_messages as f64 / self.send_count as f64
    }

    /// 格式化为可读字符串
    pub fn to_summary(&self) -> String {
        format!(
            "增量发送统计: {} 次发送, 总消息 {} 条, 实际发送 {} 条, 跳过 {} 条, 节省比例 {:.1}%",
            self.send_count,
            self.total_messages,
            self.sent_messages,
            self.skipped_messages,
            self.saved_ratio() * 100.0
        )
    }
}

// ============================================================================
//  CacheStatsSummary — 搜索缓存统计摘要 (Session 79)
// ============================================================================

/// 搜索缓存统计摘要 — 从 DevTrace WebSearch 条目中解析的缓存效果统计
///
/// 追踪缓存命中/未命中/搜索失败次数, 以及缓存节省的总时间,
/// 用于在 DevTraceSummary 报告中展示缓存效果面板。
///
/// # 设计
///
/// 在 Orchestrator 的 `auto_search_error_solutions` 方法中,
/// 每次搜索都会通过 `trace_dev` 写入 WebSearch 类型的 DevTrace 条目。
/// 缓存命中时 error 字段包含 "缓存命中 (key=..., 原始耗时=Xms, ...)";
/// 缓存未命中时 error 字段包含 "编译错误自动搜索" 或 "编译错误自动搜索 (已缓存)";
/// 搜索失败时 error 字段包含 "搜索失败: ..."。
///
/// 24 小时运行后, 可以从 DevTrace 中查看搜索缓存的整体效果。
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::CacheStatsSummary;
/// let mut stats = CacheStatsSummary::new();
///
/// // 第一次搜索: 缓存未命中, 搜索耗时 500ms
/// stats.record_miss();
/// assert_eq!(stats.cache_misses, 1);
/// assert_eq!(stats.time_saved_ms, 0);
///
/// // 第二次搜索: 相同错误, 缓存命中, 原始耗时 500ms
/// stats.record_hit(500);
/// assert_eq!(stats.cache_hits, 1);
/// assert_eq!(stats.time_saved_ms, 500);
/// assert!((stats.hit_rate() - 0.5).abs() < 0.001); // 1/(1+1) = 0.5
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CacheStatsSummary {
    /// 缓存命中次数
    pub cache_hits: u32,
    /// 缓存未命中次数 (成功搜索但未命中缓存)
    pub cache_misses: u32,
    /// 搜索失败次数
    pub search_failures: u32,
    /// 缓存命中节省的总时间 (毫秒) — 即所有缓存命中条目的原始搜索耗时之和
    pub time_saved_ms: u64,
}

impl CacheStatsSummary {
    /// 创建空的缓存统计
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一次缓存命中
    ///
    /// # 参数
    ///
    /// - `original_duration_ms`: 原始搜索耗时 (毫秒), 即缓存条目中记录的首次搜索耗时
    pub fn record_hit(&mut self, original_duration_ms: u64) {
        self.cache_hits += 1;
        self.time_saved_ms += original_duration_ms;
    }

    /// 记录一次缓存未命中 (成功搜索)
    pub fn record_miss(&mut self) {
        self.cache_misses += 1;
    }

    /// 记录一次搜索失败
    pub fn record_failure(&mut self) {
        self.search_failures += 1;
    }

    /// 总搜索次数 = 命中 + 未命中 + 失败
    pub fn total_searches(&self) -> u32 {
        self.cache_hits + self.cache_misses + self.search_failures
    }

    /// 缓存命中率 (0.0 ~ 1.0)
    ///
    /// 命中次数 / (命中 + 未命中), 搜索失败不计入分母。
    /// 因为搜索失败不代表缓存未命中, 只是网络搜索本身失败了。
    pub fn hit_rate(&self) -> f64 {
        let total = self.cache_hits + self.cache_misses;
        if total == 0 {
            return 0.0;
        }
        self.cache_hits as f64 / total as f64
    }

    /// 平均每次命中节省的时间 (毫秒)
    pub fn avg_time_saved_per_hit(&self) -> f64 {
        if self.cache_hits == 0 {
            return 0.0;
        }
        self.time_saved_ms as f64 / self.cache_hits as f64
    }

    /// 是否为空 (没有任何搜索记录)
    pub fn is_empty(&self) -> bool {
        self.total_searches() == 0
    }

    /// 格式化为可读字符串
    pub fn to_summary(&self) -> String {
        format!(
            "搜索缓存统计: 命中 {}, 未命中 {}, 失败 {}, 命中率 {:.1}%, 节省 {}ms",
            self.cache_hits,
            self.cache_misses,
            self.search_failures,
            self.hit_rate() * 100.0,
            self.time_saved_ms,
        )
    }
}

// ============================================================================
//  CacheFixCorrelation — 缓存命中与修复成功率关联分析 (Session 80)
// ============================================================================

/// 缓存命中与修复成功率关联分析 — 评估搜索缓存对修复效果的实际影响
///
/// 分析搜索缓存命中/未命中/失败后, 后续编译检查 (`CompileCheck`) 的通过率,
/// 回答核心问题: **缓存命中的搜索结果是否和新鲜搜索一样有效?**
///
/// # 设计
///
/// 在 Orchestrator 的修复流程中:
/// 1. 编译失败 → `auto_search_error_solutions` 搜索 → 记录 `WebSearch` trace
/// 2. 搜索结果注入修复 prompt → AI 修复 → 记录 `FixAttempt` trace
/// 3. 再次编译检查 → 记录 `CompileCheck` trace (success=true/false)
///
/// 关联分析逻辑:
/// - 遍历所有 `WebSearch` 条目, 解析缓存状态 (Hit/Miss/Failure)
/// - 查找同一任务 (phase_idx + task_idx) 的下一个 `CompileCheck` 条目
/// - 记录该 CompileCheck 的成功/失败, 汇总为关联统计
///
/// # 核心指标
///
/// - `hit_fix_rate()`: 缓存命中后的编译通过率
/// - `miss_fix_rate()`: 缓存未命中后的编译通过率
/// - `hit_vs_miss_diff()`: 两者差值 (正=缓存有效, 负=缓存有害)
/// - `is_cache_effective()`: 缓存命中是否比未命中有更好的修复效果
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::CacheFixCorrelation;
/// let mut corr = CacheFixCorrelation::new();
///
/// // 缓存命中后编译通过
/// corr.record_hit_check(true);
/// // 缓存命中后编译失败
/// corr.record_hit_check(false);
/// // 缓存未命中后编译通过
/// corr.record_miss_check(true);
///
/// // 命中后修复率: 1/2 = 50%
/// assert!((corr.hit_fix_rate() - 0.5).abs() < 0.001);
/// // 未命中后修复率: 1/1 = 100%
/// assert!((corr.miss_fix_rate() - 1.0).abs() < 0.001);
/// // 差值: 50% - 100% = -50% (缓存命中修复率低于未命中)
/// assert!((corr.hit_vs_miss_diff() - (-0.5)).abs() < 0.001);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CacheFixCorrelation {
    /// 缓存命中后的编译检查次数
    pub checks_after_hit: usize,
    /// 缓存命中后编译通过次数
    pub successes_after_hit: usize,
    /// 缓存未命中后的编译检查次数
    pub checks_after_miss: usize,
    /// 缓存未命中后编译通过次数
    pub successes_after_miss: usize,
    /// 搜索失败后的编译检查次数
    pub checks_after_failure: usize,
    /// 搜索失败后编译通过次数
    pub successes_after_failure: usize,
    /// 无后续编译检查的搜索次数 (搜索后没有 CompileCheck 条目)
    pub searches_without_check: usize,
}

impl CacheFixCorrelation {
    /// 创建空的关联分析
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一次缓存命中后的编译检查结果
    ///
    /// # 参数
    ///
    /// - `success`: 编译检查是否通过
    pub fn record_hit_check(&mut self, success: bool) {
        self.checks_after_hit += 1;
        if success {
            self.successes_after_hit += 1;
        }
    }

    /// 记录一次缓存未命中后的编译检查结果
    pub fn record_miss_check(&mut self, success: bool) {
        self.checks_after_miss += 1;
        if success {
            self.successes_after_miss += 1;
        }
    }

    /// 记录一次搜索失败后的编译检查结果
    pub fn record_failure_check(&mut self, success: bool) {
        self.checks_after_failure += 1;
        if success {
            self.successes_after_failure += 1;
        }
    }

    /// 记录一次无后续编译检查的搜索 (搜索后没有 CompileCheck 条目)
    pub fn record_no_check(&mut self) {
        self.searches_without_check += 1;
    }

    /// 缓存命中后的修复成功率 (0.0 ~ 1.0)
    ///
    /// 无数据时返回 0.0。
    pub fn hit_fix_rate(&self) -> f64 {
        if self.checks_after_hit == 0 {
            return 0.0;
        }
        self.successes_after_hit as f64 / self.checks_after_hit as f64
    }

    /// 缓存未命中后的修复成功率 (0.0 ~ 1.0)
    ///
    /// 无数据时返回 0.0。
    pub fn miss_fix_rate(&self) -> f64 {
        if self.checks_after_miss == 0 {
            return 0.0;
        }
        self.successes_after_miss as f64 / self.checks_after_miss as f64
    }

    /// 搜索失败后的修复成功率 (0.0 ~ 1.0)
    ///
    /// 无数据时返回 0.0。
    pub fn failure_fix_rate(&self) -> f64 {
        if self.checks_after_failure == 0 {
            return 0.0;
        }
        self.successes_after_failure as f64 / self.checks_after_failure as f64
    }

    /// 缓存命中与未命中的修复成功率差值
    ///
    /// 正值表示缓存命中比未命中有更好的修复效果;
    /// 负值表示缓存命中效果不如未命中;
    /// 零表示两者效果相同。
    ///
    /// 当 hit 或 miss 任一无数据时返回 0.0。
    pub fn hit_vs_miss_diff(&self) -> f64 {
        if self.checks_after_hit == 0 || self.checks_after_miss == 0 {
            return 0.0;
        }
        self.hit_fix_rate() - self.miss_fix_rate()
    }

    /// 缓存是否有效 — 缓存命中的修复成功率 >= 未命中的修复成功率
    ///
    /// 当 hit 或 miss 任一无数据时返回 false。
    pub fn is_cache_effective(&self) -> bool {
        if self.checks_after_hit == 0 || self.checks_after_miss == 0 {
            return false;
        }
        self.hit_fix_rate() >= self.miss_fix_rate()
    }

    /// 有后续编译检查的搜索总次数
    pub fn total_correlated(&self) -> usize {
        self.checks_after_hit + self.checks_after_miss + self.checks_after_failure
    }

    /// 是否为空 (没有任何搜索记录)
    pub fn is_empty(&self) -> bool {
        self.total_correlated() == 0 && self.searches_without_check == 0
    }

    /// 格式化为可读字符串
    pub fn to_summary(&self) -> String {
        format!(
            "缓存修复关联: 命中后 {} 次检查 (通过 {}), 未命中后 {} 次检查 (通过 {}), \
             失败后 {} 次检查 (通过 {}), 命中修复率 {:.1}%, 未命中修复率 {:.1}%, 差值 {:+.1}%",
            self.checks_after_hit,
            self.successes_after_hit,
            self.checks_after_miss,
            self.successes_after_miss,
            self.checks_after_failure,
            self.successes_after_failure,
            self.hit_fix_rate() * 100.0,
            self.miss_fix_rate() * 100.0,
            self.hit_vs_miss_diff() * 100.0,
        )
    }
}

// ============================================================================
//  SearchQualityStats — 搜索质量统计 (Session 85)
// ============================================================================

/// 搜索质量统计 — 评估自动搜索对修复成功率的影响
///
/// 比较使用了搜索结果的修复 (with search) 和未使用搜索结果的修复
/// (without search) 的成功率, 回答核心问题:
/// **自动搜索是否真正提高了修复成功率?**
///
/// # 设计
///
/// 在 Orchestrator 的修复流程中:
/// 1. 编译失败 → `auto_search_error_solutions` 搜索 → 记录 `WebSearch` trace
/// 2. 搜索结果注入修复 prompt → AI 修复 → 记录 `FixAttempt` trace
/// 3. 再次编译检查 → 记录 `CompileCheck` trace (success=true/false)
///
/// 质量统计逻辑:
/// - 遍历所有 `CompileCheck` 条目
/// - 检查同一任务 (phase_idx + task_idx) 在此 `CompileCheck` 之前是否有 `WebSearch`
/// - 有 → 记录为 "with search"; 无 → 记录为 "without search"
/// - 汇总两组的成功/失败次数
///
/// # 核心指标
///
/// - `with_search_fix_rate()`: 使用搜索结果后的编译通过率
/// - `without_search_fix_rate()`: 未使用搜索结果时的编译通过率
/// - `search_vs_no_search_diff()`: 两者差值 (正=搜索有效, 负=搜索有害)
/// - `is_search_beneficial()`: 搜索是否有助于提高修复成功率
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::SearchQualityStats;
/// let mut stats = SearchQualityStats::new();
///
/// // 有搜索结果: 2次成功, 1次失败
/// stats.record_with_search(true);
/// stats.record_with_search(true);
/// stats.record_with_search(false);
///
/// // 无搜索结果: 1次成功, 2次失败
/// stats.record_without_search(true);
/// stats.record_without_search(false);
/// stats.record_without_search(false);
///
/// // 有搜索修复率: 2/3 = 67%
/// assert!((stats.with_search_fix_rate() - 2.0 / 3.0).abs() < 0.001);
/// // 无搜索修复率: 1/3 = 33%
/// assert!((stats.without_search_fix_rate() - 1.0 / 3.0).abs() < 0.001);
/// // 差值: 67% - 33% = +33% (搜索有效)
/// assert!((stats.search_vs_no_search_diff() - (2.0 / 3.0 - 1.0 / 3.0)).abs() < 0.001);
/// assert!(stats.is_search_beneficial());
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SearchQualityStats {
    /// 使用搜索结果后的编译检查次数
    pub checks_with_search: usize,
    /// 使用搜索结果后编译通过次数
    pub successes_with_search: usize,
    /// 未使用搜索结果时的编译检查次数
    pub checks_without_search: usize,
    /// 未使用搜索结果时编译通过次数
    pub successes_without_search: usize,
    /// 总搜索次数 (WebSearch 条目数)
    pub total_searches: usize,
    /// 搜索成功次数
    pub successful_searches: usize,
    /// 搜索失败次数
    pub failed_searches: usize,
}

impl SearchQualityStats {
    /// 创建空的搜索质量统计
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一次使用搜索结果后的编译检查结果
    ///
    /// # 参数
    ///
    /// - `success`: 编译检查是否通过
    pub fn record_with_search(&mut self, success: bool) {
        self.checks_with_search += 1;
        if success {
            self.successes_with_search += 1;
        }
    }

    /// 记录一次未使用搜索结果时的编译检查结果
    ///
    /// # 参数
    ///
    /// - `success`: 编译检查是否通过
    pub fn record_without_search(&mut self, success: bool) {
        self.checks_without_search += 1;
        if success {
            self.successes_without_search += 1;
        }
    }

    /// 记录一次搜索尝试
    ///
    /// # 参数
    ///
    /// - `success`: 搜索是否成功
    pub fn record_search(&mut self, success: bool) {
        self.total_searches += 1;
        if success {
            self.successful_searches += 1;
        } else {
            self.failed_searches += 1;
        }
    }

    /// 使用搜索结果后的修复成功率 (0.0 ~ 1.0)
    ///
    /// 无数据时返回 0.0。
    pub fn with_search_fix_rate(&self) -> f64 {
        if self.checks_with_search == 0 {
            return 0.0;
        }
        self.successes_with_search as f64 / self.checks_with_search as f64
    }

    /// 未使用搜索结果时的修复成功率 (0.0 ~ 1.0)
    ///
    /// 无数据时返回 0.0。
    pub fn without_search_fix_rate(&self) -> f64 {
        if self.checks_without_search == 0 {
            return 0.0;
        }
        self.successes_without_search as f64 / self.checks_without_search as f64
    }

    /// 搜索与不搜索的修复成功率差值
    ///
    /// 正值表示搜索有助于提高修复成功率;
    /// 负值表示搜索反而降低了修复成功率;
    /// 零表示两者效果相同。
    ///
    /// 当 with_search 或 without_search 任一无数据时返回 0.0。
    pub fn search_vs_no_search_diff(&self) -> f64 {
        if self.checks_with_search == 0 || self.checks_without_search == 0 {
            return 0.0;
        }
        self.with_search_fix_rate() - self.without_search_fix_rate()
    }

    /// 搜索是否有助于提高修复成功率
    ///
    /// 当 with_search 和 without_search 都有数据, 且搜索修复率 >= 不搜索修复率时返回 true。
    pub fn is_search_beneficial(&self) -> bool {
        if self.checks_with_search == 0 || self.checks_without_search == 0 {
            return false;
        }
        self.with_search_fix_rate() >= self.without_search_fix_rate()
    }

    /// 总编译检查次数 (with + without)
    pub fn total_checks(&self) -> usize {
        self.checks_with_search + self.checks_without_search
    }

    /// 是否为空 (没有任何编译检查记录)
    pub fn is_empty(&self) -> bool {
        self.total_checks() == 0 && self.total_searches == 0
    }

    /// 是否有足够的样本进行评估
    ///
    /// 需要同时有 with_search 和 without_search 的数据, 且总数 >= min_samples。
    pub fn has_sufficient_data(&self, min_samples: usize) -> bool {
        self.checks_with_search > 0
            && self.checks_without_search > 0
            && self.total_checks() >= min_samples
    }

    /// 搜索成功率 (0.0 ~ 1.0)
    ///
    /// 无搜索数据时返回 0.0。
    pub fn search_success_rate(&self) -> f64 {
        if self.total_searches == 0 {
            return 0.0;
        }
        self.successful_searches as f64 / self.total_searches as f64
    }

    /// 格式化为可读字符串
    pub fn to_summary(&self) -> String {
        format!(
            "搜索质量: 有搜索 {} 次检查 (通过 {}), 无搜索 {} 次检查 (通过 {}), \
             搜索修复率 {:.1}%, 无搜索修复率 {:.1}%, 差值 {:+.1}%, \
             搜索 {} 次 (成功 {}, 失败 {})",
            self.checks_with_search,
            self.successes_with_search,
            self.checks_without_search,
            self.successes_without_search,
            self.with_search_fix_rate() * 100.0,
            self.without_search_fix_rate() * 100.0,
            self.search_vs_no_search_diff() * 100.0,
            self.total_searches,
            self.successful_searches,
            self.failed_searches,
        )
    }
}

/// 检查 CompileCheck 条目之前是否有同一任务的 WebSearch
///
/// 从 `idx` 位置向前搜索, 直到找到同一任务的 WebSearch (返回 true)
/// 或遇到同一任务的另一个 CompileCheck (返回 false, 表示这是新一轮修复)
/// 或到达列表起始 (返回 false)。
///
/// # 参数
///
/// - `entries`: DevTrace 条目列表
/// - `idx`: CompileCheck 条目的索引
fn has_preceding_websearch(entries: &[DevTraceEntry], idx: usize) -> bool {
    if idx == 0 || idx >= entries.len() {
        return false;
    }

    let ref_entry = &entries[idx];
    let ref_phase = ref_entry.phase_idx;
    let ref_task = ref_entry.task_idx;

    // 向前搜索
    for j in (0..idx).rev() {
        let e = &entries[j];
        // 只匹配同一任务的条目
        if e.phase_idx != ref_phase || e.task_idx != ref_task {
            continue;
        }
        if e.action == TraceAction::WebSearch {
            return true;
        }
        if e.action == TraceAction::CompileCheck {
            // 遇到前一个 CompileCheck, 说明当前是新一轮修复, 不算
            return false;
        }
    }

    false
}

/// 从 DevTrace 条目列表构建搜索质量统计
///
/// 遍历所有条目:
/// 1. `WebSearch` 条目 → 记录搜索尝试 (成功/失败)
/// 2. `CompileCheck` 条目 → 检查是否有前置 `WebSearch` (同一任务)
///    - 有 → 记录为 "with search"
///    - 无 → 记录为 "without search"
///
/// # 参数
///
/// - `entries`: DevTrace 条目列表
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::{build_search_quality_stats, DevTraceEntry, TraceAction};
/// let entries = vec![
///     // Task 1: 首次编译失败 (无搜索)
///     DevTraceEntry::new(
///         TraceAction::CompileCheck, Some(0), Some(0), Some("task1"),
///         "check", "failed", 50, false, None,
///     ),
///     // Task 2: 搜索后编译通过 (有搜索)
///     DevTraceEntry::new(
///         TraceAction::WebSearch, Some(0), Some(1), Some("task2"),
///         "query", "result", 500, true, None,
///     ),
///     DevTraceEntry::new(
///         TraceAction::CompileCheck, Some(0), Some(1), Some("task2"),
///         "check", "passed", 50, true, None,
///     ),
/// ];
/// let stats = build_search_quality_stats(&entries);
/// assert_eq!(stats.checks_without_search, 1); // Task 1
/// assert_eq!(stats.checks_with_search, 1);    // Task 2
/// assert_eq!(stats.total_searches, 1);
/// ```
pub fn build_search_quality_stats(entries: &[DevTraceEntry]) -> SearchQualityStats {
    entries
        .iter()
        .enumerate()
        .fold(SearchQualityStats::new(), |mut acc, (idx, entry)| {
            match entry.action {
                TraceAction::WebSearch => {
                    acc.record_search(entry.success);
                }
                TraceAction::CompileCheck => {
                    if has_preceding_websearch(entries, idx) {
                        acc.record_with_search(entry.success);
                    } else {
                        acc.record_without_search(entry.success);
                    }
                }
                _ => {}
            }
            acc
        })
}

// ============================================================================
//  MemoryEvaluationStats — Memory 上下文注入效果统计 (Session 90)
// ============================================================================

/// Memory 上下文注入效果统计 — 评估 Memory 注入对修复成功率的影响
///
/// 比较使用了 Memory 上下文注入的修复 (with memory) 和未使用注入的修复
/// (without memory) 的成功率, 回答核心问题:
/// **Memory 上下文注入是否真正提高了修复成功率?**
///
/// # 设计
///
/// 在 Orchestrator 的修复流程中:
/// 1. 修复轮次 → `send_attempt_prompt` 注入 Memory → 记录 `MemoryInjection` trace
/// 2. AI 修复 → 编译检查 → 记录 `CompileCheck` trace (success=true/false)
///
/// 统计逻辑:
/// - 遍历所有 `CompileCheck` 条目
/// - 检查同一任务在此之前是否有 `MemoryInjection`
/// - 有 → 记录为 "with memory"; 无 → 记录为 "without memory"
///
/// # 核心指标
///
/// - `with_memory_fix_rate()`: 有注入后的编译通过率
/// - `without_memory_fix_rate()`: 无注入时的编译通过率
/// - `memory_vs_no_memory_diff()`: 两者差值 (正=有效, 负=有害)
/// - `is_memory_beneficial()`: 注入是否有助于提高修复成功率
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::MemoryEvaluationStats;
/// let mut stats = MemoryEvaluationStats::new();
///
/// stats.record_with_memory(true);
/// stats.record_with_memory(true);
/// stats.record_without_memory(false);
///
/// assert!((stats.with_memory_fix_rate() - 1.0).abs() < 0.001);
/// assert!((stats.without_memory_fix_rate() - 0.0).abs() < 0.001);
/// assert!(stats.is_memory_beneficial());
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MemoryEvaluationStats {
    /// 有 Memory 注入后的编译检查次数
    pub checks_with_memory: usize,
    /// 有 Memory 注入后编译通过次数
    pub successes_with_memory: usize,
    /// 无 Memory 注入时的编译检查次数
    pub checks_without_memory: usize,
    /// 无 Memory 注入时编译通过次数
    pub successes_without_memory: usize,
    /// 总注入次数 (MemoryInjection 条目数)
    pub total_injections: usize,
}

impl MemoryEvaluationStats {
    /// 创建空的统计
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一次有 Memory 注入后的编译检查结果
    pub fn record_with_memory(&mut self, success: bool) {
        self.checks_with_memory += 1;
        if success {
            self.successes_with_memory += 1;
        }
    }

    /// 记录一次无 Memory 注入时的编译检查结果
    pub fn record_without_memory(&mut self, success: bool) {
        self.checks_without_memory += 1;
        if success {
            self.successes_without_memory += 1;
        }
    }

    /// 记录一次 Memory 注入
    pub fn record_injection(&mut self) {
        self.total_injections += 1;
    }

    /// 有注入后的修复成功率 (0.0 ~ 1.0)
    pub fn with_memory_fix_rate(&self) -> f64 {
        if self.checks_with_memory == 0 {
            return 0.0;
        }
        self.successes_with_memory as f64 / self.checks_with_memory as f64
    }

    /// 无注入时的修复成功率 (0.0 ~ 1.0)
    pub fn without_memory_fix_rate(&self) -> f64 {
        if self.checks_without_memory == 0 {
            return 0.0;
        }
        self.successes_without_memory as f64 / self.checks_without_memory as f64
    }

    /// 注入与不注入的修复成功率差值
    ///
    /// 正值=注入有效, 负值=注入有害, 零=效果相同。
    pub fn memory_vs_no_memory_diff(&self) -> f64 {
        if self.checks_with_memory == 0 || self.checks_without_memory == 0 {
            return 0.0;
        }
        self.with_memory_fix_rate() - self.without_memory_fix_rate()
    }

    /// 注入是否有助于提高修复成功率
    pub fn is_memory_beneficial(&self) -> bool {
        if self.checks_with_memory == 0 || self.checks_without_memory == 0 {
            return false;
        }
        self.with_memory_fix_rate() >= self.without_memory_fix_rate()
    }

    /// 总编译检查次数
    pub fn total_checks(&self) -> usize {
        self.checks_with_memory + self.checks_without_memory
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.total_checks() == 0 && self.total_injections == 0
    }

    /// 格式化为可读字符串
    pub fn to_summary(&self) -> String {
        format!(
            "Memory 评估: 有注入 {} 次检查 (通过 {}), 无注入 {} 次检查 (通过 {}), \
             注入修复率 {:.1}%, 无注入修复率 {:.1}%, 差值 {:+.1}%, \
             总注入 {} 次",
            self.checks_with_memory,
            self.successes_with_memory,
            self.checks_without_memory,
            self.successes_without_memory,
            self.with_memory_fix_rate() * 100.0,
            self.without_memory_fix_rate() * 100.0,
            self.memory_vs_no_memory_diff() * 100.0,
            self.total_injections,
        )
    }
}

/// 检查 CompileCheck 条目之前是否有同一任务的 MemoryInjection
///
/// 从 `idx` 位置向前搜索, 直到找到同一任务的 MemoryInjection (返回 true)
/// 或遇到同一任务的另一个 CompileCheck (返回 false)
/// 或到达列表起始 (返回 false)。
fn has_preceding_memory_injection(entries: &[DevTraceEntry], idx: usize) -> bool {
    if idx == 0 || idx >= entries.len() {
        return false;
    }

    let ref_entry = &entries[idx];
    let ref_phase = ref_entry.phase_idx;
    let ref_task = ref_entry.task_idx;

    for j in (0..idx).rev() {
        let e = &entries[j];
        if e.phase_idx != ref_phase || e.task_idx != ref_task {
            continue;
        }
        if e.action == TraceAction::MemoryInjection {
            return true;
        }
        if e.action == TraceAction::CompileCheck {
            return false;
        }
    }

    false
}

/// 从 DevTrace 条目列表构建 Memory 评估统计
///
/// 遍历所有条目:
/// 1. `MemoryInjection` 条目 → 记录注入次数
/// 2. `CompileCheck` 条目 → 检查是否有前置 `MemoryInjection` (同一任务)
///    - 有 → 记录为 "with memory"
///    - 无 → 记录为 "without memory"
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::{build_memory_evaluation_stats, DevTraceEntry, TraceAction};
/// let entries = vec![
///     DevTraceEntry::new(
///         TraceAction::CompileCheck, Some(0), Some(0), Some("t1"),
///         "check", "failed", 50, false, None,
///     ),
///     DevTraceEntry::new(
///         TraceAction::MemoryInjection, Some(0), Some(1), Some("t2"),
///         "inject", "msgs=3", 0, true, None,
///     ),
///     DevTraceEntry::new(
///         TraceAction::CompileCheck, Some(0), Some(1), Some("t2"),
///         "check", "passed", 50, true, None,
///     ),
/// ];
/// let stats = build_memory_evaluation_stats(&entries);
/// assert_eq!(stats.checks_without_memory, 1);
/// assert_eq!(stats.checks_with_memory, 1);
/// assert_eq!(stats.total_injections, 1);
/// ```
pub fn build_memory_evaluation_stats(entries: &[DevTraceEntry]) -> MemoryEvaluationStats {
    entries
        .iter()
        .enumerate()
        .fold(MemoryEvaluationStats::new(), |mut acc, (idx, entry)| {
            match entry.action {
                TraceAction::MemoryInjection => {
                    acc.record_injection();
                }
                TraceAction::CompileCheck => {
                    if has_preceding_memory_injection(entries, idx) {
                        acc.record_with_memory(entry.success);
                    } else {
                        acc.record_without_memory(entry.success);
                    }
                }
                _ => {}
            }
            acc
        })
}

// ============================================================================
//  CacheEntryInfo — 缓存条目类型 (Session 79)
// ============================================================================

/// 缓存条目类型 — 从 DevTrace WebSearch 条目解析
///
/// 表示一条 WebSearch trace 条目与缓存的关系。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheEntryInfo {
    /// 缓存命中, 包含原始搜索耗时 (毫秒)
    Hit(u64),
    /// 缓存未命中 (成功搜索)
    Miss,
    /// 搜索失败
    Failure,
    /// 非缓存相关条目 (非 WebSearch 或无 error 字段)
    None,
}

// ============================================================================
//  纯函数 — 缓存条目解析 (Session 79)
// ============================================================================

/// 从缓存命中的 error 字段中解析原始搜索耗时 (毫秒)
///
/// 缓存命中时 error 格式为: `"缓存命中 (key=..., 原始耗时=Xms, 命中次数=Y)"`
/// 本函数提取其中的 `X` 作为原始搜索耗时。
///
/// # 参数
///
/// - `error`: DevTrace 条目的 error 字段
///
/// # 返回
///
/// - `Some(ms)`: 原始搜索耗时 (毫秒)
/// - `None`: 不是缓存命中条目, 或无法解析耗时
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::parse_cache_hit_duration;
/// let err = "缓存命中 (key=E0308, 原始耗时=500ms, 命中次数=2)";
/// assert_eq!(parse_cache_hit_duration(err), Some(500));
///
/// assert_eq!(parse_cache_hit_duration("编译错误自动搜索"), None);
/// assert_eq!(parse_cache_hit_duration(""), None);
/// ```
pub fn parse_cache_hit_duration(error: &str) -> Option<u64> {
    // 必须包含 "缓存命中" 才是缓存命中条目
    if !error.contains("缓存命中") {
        return None;
    }

    // 查找 "原始耗时=" 后面的数字
    let key = "原始耗时=";
    let pos = error.find(key)?;
    let after_key = &error[pos + key.len()..];

    // 读取连续的数字字符
    let digits: String = after_key
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();

    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

/// 判断 error 字段是否表示缓存未命中 (成功搜索)
///
/// 缓存未命中时 error 格式为:
/// - `"编译错误自动搜索 (已缓存)"` — 搜索成功且结果已缓存
/// - `"编译错误自动搜索"` — 搜索成功但无法缓存 (无 error_code)
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::is_cache_miss;
/// assert!(is_cache_miss("编译错误自动搜索 (已缓存)"));
/// assert!(is_cache_miss("编译错误自动搜索"));
/// assert!(!is_cache_miss("缓存命中 (key=E0308)"));
/// assert!(!is_cache_miss("搜索失败: timeout"));
/// ```
pub fn is_cache_miss(error: &str) -> bool {
    error.contains("编译错误自动搜索") && !error.contains("缓存命中")
}

/// 判断 error 字段是否表示搜索失败
///
/// 搜索失败时 error 格式为: `"搜索失败: ..."`
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::is_search_failure;
/// assert!(is_search_failure("搜索失败: connection refused"));
/// assert!(!is_search_failure("缓存命中 (key=E0308)"));
/// assert!(!is_search_failure("编译错误自动搜索"));
/// ```
pub fn is_search_failure(error: &str) -> bool {
    error.contains("搜索失败")
}

/// 从 DevTrace 条目解析缓存信息
///
/// 根据 trace 条目的 action 和 error 字段判断缓存状态:
/// - `WebSearch` action + error 含 "缓存命中" → `Hit(duration)`
/// - `WebSearch` action + error 含 "搜索失败" → `Failure`
/// - `WebSearch` action + error 含 "编译错误自动搜索" → `Miss`
/// - 其他 → `None`
///
/// # 参数
///
/// - `entry`: DevTrace 条目
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::{parse_cache_entry, CacheEntryInfo, DevTraceEntry, TraceAction};
/// let entry = DevTraceEntry::new(
///     TraceAction::WebSearch, None, None, None,
///     "query", "result", 0, true,
///     Some("缓存命中 (key=E0308, 原始耗时=300ms, 命中次数=1)"),
/// );
/// assert_eq!(parse_cache_entry(&entry), CacheEntryInfo::Hit(300));
/// ```
pub fn parse_cache_entry(entry: &DevTraceEntry) -> CacheEntryInfo {
    if entry.action != TraceAction::WebSearch {
        return CacheEntryInfo::None;
    }

    match &entry.error {
        None => CacheEntryInfo::None,
        Some(err) => {
            if let Some(dur) = parse_cache_hit_duration(err) {
                CacheEntryInfo::Hit(dur)
            } else if is_search_failure(err) {
                CacheEntryInfo::Failure
            } else if is_cache_miss(err) {
                CacheEntryInfo::Miss
            } else {
                CacheEntryInfo::None
            }
        }
    }
}

/// 从 DevTrace 条目列表构建缓存统计摘要
///
/// 遍历所有 WebSearch 条目, 解析缓存命中/未命中/失败信息,
/// 汇总为 `CacheStatsSummary`。
///
/// # 参数
///
/// - `entries`: DevTrace 条目列表
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::{build_cache_summary, DevTraceEntry, TraceAction};
/// let entries = vec![
///     DevTraceEntry::new(
///         TraceAction::WebSearch, None, None, None,
///         "q1", "r1", 500, true,
///         Some("编译错误自动搜索 (已缓存)"),
///     ),
///     DevTraceEntry::new(
///         TraceAction::WebSearch, None, None, None,
///         "q2", "r2", 0, true,
///         Some("缓存命中 (key=E0308, 原始耗时=500ms, 命中次数=1)"),
///     ),
/// ];
/// let summary = build_cache_summary(&entries);
/// assert_eq!(summary.cache_hits, 1);
/// assert_eq!(summary.cache_misses, 1);
/// assert_eq!(summary.time_saved_ms, 500);
/// ```
pub fn build_cache_summary(entries: &[DevTraceEntry]) -> CacheStatsSummary {
    entries
        .iter()
        .map(parse_cache_entry)
        .fold(CacheStatsSummary::new(), |mut acc, info| {
            match info {
                CacheEntryInfo::Hit(dur) => acc.record_hit(dur),
                CacheEntryInfo::Miss => acc.record_miss(),
                CacheEntryInfo::Failure => acc.record_failure(),
                CacheEntryInfo::None => {}
            }
            acc
        })
}

// ============================================================================
//  缓存与修复关联分析纯函数 (Session 80)
// ============================================================================

/// 在 trace 条目列表中, 从指定索引之后查找同一任务的下一个编译检查条目
///
/// 从 `from_idx + 1` 开始遍历, 查找 `CompileCheck` 类型且
/// `phase_idx` 和 `task_idx` 与参考条目匹配的条目。
///
/// # 匹配规则
///
/// - 参考条目和目标条目的 `phase_idx` 必须相同 (都为 None 或都为 Some 且值相等)
/// - 参考条目和目标条目的 `task_idx` 必须相同
/// - 目标条目的 `action` 必须为 `CompileCheck`
///
/// # 参数
///
/// - `entries`: DevTrace 条目列表
/// - `from_idx`: 起始搜索索引 (不含), 从 `from_idx + 1` 开始查找
///
/// # 返回
///
/// 匹配的 `CompileCheck` 条目的 `success` 字段值; 未找到则返回 `None`。
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::{find_next_compile_check, DevTraceEntry, TraceAction};
/// let entries = vec![
///     DevTraceEntry::new(
///         TraceAction::WebSearch, Some(0), Some(0), Some("task"),
///         "query", "result", 100, true, None,
///     ),
///     DevTraceEntry::new(
///         TraceAction::FixAttempt, Some(0), Some(0), Some("task"),
///         "fix", "response", 200, true, None,
///     ),
///     DevTraceEntry::new(
///         TraceAction::CompileCheck, Some(0), Some(0), Some("task"),
///         "check", "passed", 50, true, None,
///     ),
/// ];
/// // 从索引 0 (WebSearch) 之后查找 CompileCheck
/// assert_eq!(find_next_compile_check(&entries, 0), Some(true));
/// // 从索引 1 (FixAttempt) 之后查找 CompileCheck
/// assert_eq!(find_next_compile_check(&entries, 1), Some(true));
/// // 从索引 2 (CompileCheck) 之后查找 — 没有更多 CompileCheck
/// assert_eq!(find_next_compile_check(&entries, 2), None);
/// ```
pub fn find_next_compile_check(entries: &[DevTraceEntry], from_idx: usize) -> Option<bool> {
    if from_idx >= entries.len() {
        return None;
    }

    let ref_entry = &entries[from_idx];
    let ref_phase = ref_entry.phase_idx;
    let ref_task = ref_entry.task_idx;

    entries
        .iter()
        .skip(from_idx + 1)
        .find(|e| {
            e.action == TraceAction::CompileCheck
                && e.phase_idx == ref_phase
                && e.task_idx == ref_task
        })
        .map(|e| e.success)
}

/// 从 DevTrace 条目列表构建缓存与修复关联分析
///
/// 遍历所有 `WebSearch` 条目, 解析缓存状态 (Hit/Miss/Failure),
/// 然后查找同一任务的下一个 `CompileCheck` 条目,
/// 记录编译通过/失败, 汇总为 `CacheFixCorrelation`。
///
/// # 分析逻辑
///
/// 1. 遍历条目, 找到 `WebSearch` 条目
/// 2. 用 `parse_cache_entry` 解析缓存状态
/// 3. 用 `find_next_compile_check` 查找后续编译检查
/// 4. 根据缓存状态 + 编译结果记录到 `CacheFixCorrelation`
///
/// # 参数
///
/// - `entries`: DevTrace 条目列表
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::{build_cache_fix_correlation, DevTraceEntry, TraceAction};
/// let entries = vec![
///     // 缓存命中 → 后续编译通过
///     DevTraceEntry::new(
///         TraceAction::WebSearch, Some(0), Some(0), Some("task"),
///         "query", "result", 0, true,
///         Some("缓存命中 (key=E0308, 原始耗时=500ms, 命中次数=1)"),
///     ),
///     DevTraceEntry::new(
///         TraceAction::CompileCheck, Some(0), Some(0), Some("task"),
///         "check", "passed", 50, true, None,
///     ),
///     // 缓存未命中 → 后续编译失败
///     DevTraceEntry::new(
///         TraceAction::WebSearch, Some(0), Some(1), Some("task2"),
///         "query", "result", 500, true,
///         Some("编译错误自动搜索"),
///     ),
///     DevTraceEntry::new(
///         TraceAction::CompileCheck, Some(0), Some(1), Some("task2"),
///         "check", "failed", 50, false, None,
///     ),
/// ];
/// let corr = build_cache_fix_correlation(&entries);
/// assert_eq!(corr.checks_after_hit, 1);
/// assert_eq!(corr.successes_after_hit, 1);
/// assert_eq!(corr.checks_after_miss, 1);
/// assert_eq!(corr.successes_after_miss, 0);
/// assert!((corr.hit_fix_rate() - 1.0).abs() < 0.001);  // 1/1 = 100%
/// assert_eq!(corr.miss_fix_rate(), 0.0);  // 0/1 = 0%
/// ```
pub fn build_cache_fix_correlation(entries: &[DevTraceEntry]) -> CacheFixCorrelation {
    entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.action == TraceAction::WebSearch)
        .fold(CacheFixCorrelation::new(), |mut acc, (idx, entry)| {
            let cache_info = parse_cache_entry(entry);
            match cache_info {
                CacheEntryInfo::Hit(_) => match find_next_compile_check(entries, idx) {
                    Some(success) => acc.record_hit_check(success),
                    None => acc.record_no_check(),
                },
                CacheEntryInfo::Miss => match find_next_compile_check(entries, idx) {
                    Some(success) => acc.record_miss_check(success),
                    None => acc.record_no_check(),
                },
                CacheEntryInfo::Failure => match find_next_compile_check(entries, idx) {
                    Some(success) => acc.record_failure_check(success),
                    None => acc.record_no_check(),
                },
                CacheEntryInfo::None => {}
            }
            acc
        })
}

// ============================================================================
//  缓存调优效果可视化纯函数 (Session 83)
// ============================================================================

/// 从 `CacheTuning` trace 条目中解析的调优动作信息
///
/// 由 [`parse_cache_tuning_entry`][] 生成, 表示单次缓存调优决策的动作类型。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TuningActionInfo {
    /// 保持当前配置
    KeepCurrent,
    /// 调整 TTL
    AdjustTtl {
        /// 调整前的 TTL (秒)
        old_ttl: u64,
        /// 调整后的 TTL (秒)
        new_ttl: u64,
    },
    /// 禁用缓存
    DisableCache,
}

/// 从单个 `CacheTuning` trace 条目解析的调优信息
///
/// 包含调优动作、关联差值和缓存命中/未命中的修复检查数据。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheTuningEntryInfo {
    /// 调优动作
    pub action: TuningActionInfo,
    /// 缓存命中与未命中的修复成功率差值 (-1.0 ~ 1.0)
    pub correlation_diff: f64,
    /// 缓存命中后编译检查成功数
    pub hit_successes: usize,
    /// 缓存命中后编译检查总数
    pub hit_checks: usize,
    /// 缓存未命中后编译检查成功数
    pub miss_successes: usize,
    /// 缓存未命中后编译检查总数
    pub miss_checks: usize,
}

/// 缓存调优效果摘要 — 从 `CacheTuning` trace 条目中解析的调优历史统计
///
/// 追踪 CacheTuner 的所有调优决策, 用于在 DevTraceSummary 报告中
/// 展示调优历史和效果 (Session 83)。
///
/// # 字段
///
/// | 字段 | 含义 |
/// |------|------|
/// | `total_evaluations` | 总评估次数 |
/// | `keep_current_count` | "保持当前" 次数 |
/// | `adjust_ttl_count` | "调整 TTL" 次数 |
/// | `disable_count` | "禁用缓存" 次数 |
/// | `ttl_history` | TTL 调整历史 `(old_ttl, new_ttl)` 对 |
/// | `final_ttl` | 最终 TTL (最后一次 `AdjustTtl` 的 `new_ttl`) |
/// | `cache_disabled` | 缓存是否已被禁用 |
/// | `correlation_diffs` | 所有评估的差值列表 (趋势分析) |
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::{build_cache_tuning_summary, DevTraceEntry, TraceAction};
/// let entries = vec![
///     DevTraceEntry::new(
///         TraceAction::CacheTuning, Some(0), Some(0), Some("task"),
///         "hit=2/3 miss=3/3",
///         "缓存调优: 调整 TTL: 1800s → 2700s (差值 +67.0%, 原因: 缓存有效)",
///         0, true, Some("缓存有效"),
///     ),
///     DevTraceEntry::new(
///         TraceAction::CacheTuning, Some(0), Some(1), Some("task2"),
///         "hit=1/3 miss=3/3",
///         "缓存调优: 禁用缓存 (差值 -100.0%, 原因: 缓存有害)",
///         0, true, Some("缓存有害"),
///     ),
/// ];
/// let summary = build_cache_tuning_summary(&entries);
/// assert_eq!(summary.total_evaluations, 2);
/// assert_eq!(summary.adjust_ttl_count, 1);
/// assert_eq!(summary.disable_count, 1);
/// assert!(summary.cache_disabled);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheTuningSummary {
    /// 总评估次数
    pub total_evaluations: usize,
    /// "保持当前" 次数
    pub keep_current_count: usize,
    /// "调整 TTL" 次数
    pub adjust_ttl_count: usize,
    /// "禁用缓存" 次数
    pub disable_count: usize,
    /// TTL 调整历史 `(old_ttl, new_ttl)` 对
    pub ttl_history: Vec<(u64, u64)>,
    /// 最终 TTL (最后一次 `AdjustTtl` 的 `new_ttl`)
    ///
    /// `None` 表示从未调整过 TTL。
    pub final_ttl: Option<u64>,
    /// 缓存是否已被禁用 (任一决策为 `DisableCache` 时为 `true`)
    pub cache_disabled: bool,
    /// 所有评估的差值列表 (用于趋势分析)
    pub correlation_diffs: Vec<f64>,
}

impl CacheTuningSummary {
    /// 创建空调优摘要
    pub fn new() -> Self {
        Self {
            total_evaluations: 0,
            keep_current_count: 0,
            adjust_ttl_count: 0,
            disable_count: 0,
            ttl_history: vec![],
            final_ttl: None,
            cache_disabled: false,
            correlation_diffs: vec![],
        }
    }

    /// 是否为空 (无评估记录)
    pub fn is_empty(&self) -> bool {
        self.total_evaluations == 0
    }

    /// 平均关联差值
    ///
    /// 返回所有评估差值的算术平均。无评估时返回 `0.0`。
    ///
    /// # 示例
    ///
    /// ```
    /// # use forge::dev_trace::CacheTuningSummary;
    /// let mut s = CacheTuningSummary::new();
    /// s.correlation_diffs = vec![0.1, -0.2, 0.3];
    /// assert!((s.avg_correlation_diff() - 0.0667).abs() < 0.001);
    /// ```
    pub fn avg_correlation_diff(&self) -> f64 {
        if self.correlation_diffs.is_empty() {
            return 0.0;
        }
        self.correlation_diffs.iter().sum::<f64>() / self.correlation_diffs.len() as f64
    }

    /// 初始 TTL (第一次 `AdjustTtl` 的 `old_ttl`)
    ///
    /// `None` 表示从未调整过 TTL。
    pub fn initial_ttl(&self) -> Option<u64> {
        self.ttl_history.first().map(|(old, _)| *old)
    }

    /// 从单次调优信息记录
    pub fn record(&mut self, info: CacheTuningEntryInfo) {
        self.total_evaluations += 1;
        self.correlation_diffs.push(info.correlation_diff);

        match info.action {
            TuningActionInfo::KeepCurrent => {
                self.keep_current_count += 1;
            }
            TuningActionInfo::AdjustTtl { old_ttl, new_ttl } => {
                self.adjust_ttl_count += 1;
                self.ttl_history.push((old_ttl, new_ttl));
                self.final_ttl = Some(new_ttl);
            }
            TuningActionInfo::DisableCache => {
                self.disable_count += 1;
                self.cache_disabled = true;
            }
        }
    }
}

impl Default for CacheTuningSummary {
    fn default() -> Self {
        Self::new()
    }
}

/// 从 `output_summary` 中解析调优动作
///
/// `output_summary` 格式由 `CacheTuningDecision::to_summary()` 生成:
/// - `"缓存调优: 保持当前配置 (差值 {diff}%, 原因: {reason})"`
/// - `"缓存调优: 调整 TTL: {old}s → {new}s (差值 {diff}%, 原因: {reason})"`
/// - `"缓存调优: 禁用缓存 (差值 {diff}%, 原因: {reason})"`
///
/// 解析失败时返回 `None`。
fn parse_tuning_action(output_summary: &str) -> Option<TuningActionInfo> {
    if output_summary.contains("禁用缓存") {
        Some(TuningActionInfo::DisableCache)
    } else if output_summary.contains("保持当前配置") {
        Some(TuningActionInfo::KeepCurrent)
    } else if output_summary.contains("调整 TTL") {
        // 格式: "调整 TTL: 1800s → 2700s"
        let ttl_part = output_summary.split("调整 TTL:").nth(1)?;
        let after_ttl = ttl_part.split("(差值").next()?;
        let parts: Vec<&str> = after_ttl.split('→').collect();
        if parts.len() != 2 {
            return None;
        }
        let old_ttl = parse_ttl_value(parts[0])?;
        let new_ttl = parse_ttl_value(parts[1])?;
        Some(TuningActionInfo::AdjustTtl { old_ttl, new_ttl })
    } else {
        None
    }
}

/// 从字符串中解析 TTL 值 (如 `"1800s"` → `1800`)
fn parse_ttl_value(s: &str) -> Option<u64> {
    let trimmed = s.trim();
    let without_s = trimmed.strip_suffix('s')?;
    without_s.trim().parse().ok()
}

/// 从 `output_summary` 中解析关联差值 (百分比 → 小数)
///
/// 格式: `"差值 +67.0%"` 或 `"差值 -100.0%"`
fn parse_correlation_diff(output_summary: &str) -> Option<f64> {
    let diff_part = output_summary.split("差值").nth(1)?;
    let after_diff = diff_part.trim_start();
    let num_str: String = after_diff
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '+' || *c == '-' || *c == '.')
        .collect();
    if num_str.is_empty() {
        return None;
    }
    let percent: f64 = num_str.parse().ok()?;
    Some(percent / 100.0)
}

/// 从 `input_summary` 中解析 hit/miss 修复检查数据
///
/// 格式: `"hit=2/3 miss=3/3"`
///
/// 返回 `(hit_successes, hit_checks, miss_successes, miss_checks)`。
fn parse_hit_miss(input_summary: &str) -> Option<(usize, usize, usize, usize)> {
    let hit_part = input_summary
        .split("hit=")
        .nth(1)?
        .split_whitespace()
        .next()?;
    let hit_parts: Vec<&str> = hit_part.split('/').collect();
    if hit_parts.len() < 2 {
        return None;
    }
    let hit_successes: usize = hit_parts[0].parse().ok()?;
    let hit_checks: usize = hit_parts[1].parse().ok()?;

    let miss_part = input_summary
        .split("miss=")
        .nth(1)?
        .split_whitespace()
        .next()?;
    let miss_parts: Vec<&str> = miss_part.split('/').collect();
    if miss_parts.len() < 2 {
        return None;
    }
    let miss_successes: usize = miss_parts[0].parse().ok()?;
    let miss_checks: usize = miss_parts[1].parse().ok()?;

    Some((hit_successes, hit_checks, miss_successes, miss_checks))
}

/// 从单个 `CacheTuning` trace 条目解析调优信息
///
/// 根据 trace 条目的 `action`、`input_summary` 和 `output_summary` 字段
/// 解析调优决策的详细信息。
///
/// # 解析规则
///
/// - `action` 必须为 `CacheTuning`
/// - `output_summary` 包含调优动作和关联差值
/// - `input_summary` 包含 hit/miss 修复检查数据 (可选, 解析失败时默认为 `0`)
///
/// # 参数
///
/// - `entry`: DevTrace 条目
///
/// # 返回
///
/// 解析成功返回 `Some(CacheTuningEntryInfo)`, 否则返回 `None`。
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::{parse_cache_tuning_entry, DevTraceEntry, TraceAction, TuningActionInfo};
/// let entry = DevTraceEntry::new(
///     TraceAction::CacheTuning, Some(0), Some(0), Some("task"),
///     "hit=2/3 miss=3/3",
///     "缓存调优: 调整 TTL: 1800s → 2700s (差值 +67.0%, 原因: 缓存有效)",
///     0, true, Some("缓存有效"),
/// );
/// let info = parse_cache_tuning_entry(&entry).unwrap();
/// assert_eq!(info.action, TuningActionInfo::AdjustTtl { old_ttl: 1800, new_ttl: 2700 });
/// assert!((info.correlation_diff - 0.67).abs() < 0.001);
/// assert_eq!(info.hit_successes, 2);
/// assert_eq!(info.hit_checks, 3);
/// ```
pub fn parse_cache_tuning_entry(entry: &DevTraceEntry) -> Option<CacheTuningEntryInfo> {
    if entry.action != TraceAction::CacheTuning {
        return None;
    }

    let action = parse_tuning_action(&entry.output_summary)?;
    let correlation_diff = parse_correlation_diff(&entry.output_summary).unwrap_or(0.0);

    let (hit_successes, hit_checks, miss_successes, miss_checks) =
        parse_hit_miss(&entry.input_summary).unwrap_or((0, 0, 0, 0));

    Some(CacheTuningEntryInfo {
        action,
        correlation_diff,
        hit_successes,
        hit_checks,
        miss_successes,
        miss_checks,
    })
}

/// 从 DevTrace 条目列表构建缓存调优效果摘要
///
/// 遍历所有 `CacheTuning` 条目, 解析调优决策信息,
/// 汇总为 `CacheTuningSummary`。
///
/// # 参数
///
/// - `entries`: DevTrace 条目列表
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::{build_cache_tuning_summary, DevTraceEntry, TraceAction};
/// let entries = vec![
///     DevTraceEntry::new(
///         TraceAction::CacheTuning, None, None, None,
///         "hit=2/3 miss=3/3",
///         "缓存调优: 保持当前配置 (差值 +5.0%, 原因: 数据不足)",
///         0, true, Some("数据不足"),
///     ),
/// ];
/// let summary = build_cache_tuning_summary(&entries);
/// assert_eq!(summary.total_evaluations, 1);
/// assert_eq!(summary.keep_current_count, 1);
/// ```
pub fn build_cache_tuning_summary(entries: &[DevTraceEntry]) -> CacheTuningSummary {
    entries.iter().filter_map(parse_cache_tuning_entry).fold(
        CacheTuningSummary::new(),
        |mut acc, info| {
            acc.record(info);
            acc
        },
    )
}

// ============================================================================
//  跨 Session 历史摘要 (Session 87)
// ============================================================================

/// 搜索质量历史摘要 — 跨 session 的搜索质量评估概览
///
/// 从 `SearchQualityHistory` (`.forge/search_quality_history.json`) 提取,
/// 展示搜索功能在多个 session 中的启用/禁用状态变化和累计评估统计。
///
/// 与 `SearchQualityStats` (单 session) 不同, 本摘要反映的是跨 session 的
/// 持久化历史数据, 用于判断搜索质量评估的长期趋势。
///
/// # 字段
///
/// | 字段 | 说明 |
/// |------|------|
/// | `initial_enabled` | session 开始时的搜索启用状态 |
/// | `current_enabled` | 最终搜索启用状态 |
/// | `evaluation_count` | 累计评估次数 |
/// | `disable_count` | 累计禁用次数 |
/// | `enabled_changed` | 启用状态是否发生变化 |
/// | `saved_at` | 历史保存时间 (ISO 8601) |
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::SearchQualityHistorySummary;
/// let s = SearchQualityHistorySummary::new(true, false, 5, 1, Some("2024-01-01T00:00:00Z".to_string()));
/// assert!(s.enabled_changed); // true → false
/// assert!(!s.is_empty()); // 5 evaluations
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchQualityHistorySummary {
    /// session 开始时的搜索启用状态
    pub initial_enabled: bool,
    /// 最终搜索启用状态
    pub current_enabled: bool,
    /// 累计评估次数
    pub evaluation_count: u32,
    /// 累计禁用次数
    pub disable_count: u32,
    /// 启用状态是否发生变化 (initial != current)
    pub enabled_changed: bool,
    /// 历史保存时间 (ISO 8601 格式, 可选)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved_at: Option<String>,
}

impl SearchQualityHistorySummary {
    /// 创建搜索质量历史摘要
    ///
    /// # 参数
    ///
    /// - `initial_enabled`: session 开始时的搜索启用状态
    /// - `current_enabled`: 最终搜索启用状态
    /// - `evaluation_count`: 累计评估次数
    /// - `disable_count`: 累计禁用次数
    /// - `saved_at`: 历史保存时间 (ISO 8601, 可选)
    pub fn new(
        initial_enabled: bool,
        current_enabled: bool,
        evaluation_count: u32,
        disable_count: u32,
        saved_at: Option<String>,
    ) -> Self {
        Self {
            initial_enabled,
            current_enabled,
            evaluation_count,
            disable_count,
            enabled_changed: initial_enabled != current_enabled,
            saved_at,
        }
    }

    /// 是否为空 (无评估记录)
    pub fn is_empty(&self) -> bool {
        self.evaluation_count == 0
    }

    /// 禁用率 (disable_count / evaluation_count)
    ///
    /// 无评估时返回 0.0。
    pub fn disable_rate(&self) -> f64 {
        if self.evaluation_count == 0 {
            return 0.0;
        }
        self.disable_count as f64 / self.evaluation_count as f64
    }
}

impl Default for SearchQualityHistorySummary {
    fn default() -> Self {
        Self::new(true, true, 0, 0, None)
    }
}

/// 缓存调优历史摘要 — 跨 session 的缓存调优概览
///
/// 从 `CacheTuningHistory` (`.forge/cache_tuning_history.json`) 提取,
/// 展示缓存调优在多个 session 中的 TTL 变化趋势和累计统计。
///
/// 与 `CacheTuningSummary` (单 session, 从 trace 条目解析) 不同,
/// 本摘要反映的是跨 session 的持久化历史数据, 用于判断缓存策略的长期效果。
///
/// # 字段
///
/// | 字段 | 说明 |
/// |------|------|
/// | `initial_ttl` | session 开始时的 TTL (秒) |
/// | `current_ttl` | 最终 TTL (秒) |
/// | `enabled` | 缓存是否启用 |
/// | `adjustment_count` | 累计 TTL 调整次数 |
/// | `disable_count` | 累计禁用次数 |
/// | `decision_count` | 决策记录总数 |
/// | `ttl_delta` | TTL 变化量 (current - initial) |
/// | `saved_at` | 历史保存时间 (ISO 8601) |
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::CacheTuningHistorySummary;
/// let s = CacheTuningHistorySummary::new(1800, 2700, true, 1, 0, 1, None);
/// assert_eq!(s.ttl_delta, 900);
/// assert!(!s.is_empty()); // 1 decision
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheTuningHistorySummary {
    /// session 开始时的 TTL (秒)
    pub initial_ttl: u64,
    /// 最终 TTL (秒)
    pub current_ttl: u64,
    /// 缓存是否启用
    pub enabled: bool,
    /// 累计 TTL 调整次数
    pub adjustment_count: u32,
    /// 累计禁用次数
    pub disable_count: u32,
    /// 决策记录总数
    pub decision_count: usize,
    /// TTL 变化量 (current - initial, 正=延长 负=缩短)
    pub ttl_delta: i64,
    /// 历史保存时间 (ISO 8601 格式, 可选)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved_at: Option<String>,
}

impl CacheTuningHistorySummary {
    /// 创建缓存调优历史摘要
    ///
    /// # 参数
    ///
    /// - `initial_ttl`: session 开始时的 TTL (秒)
    /// - `current_ttl`: 最终 TTL (秒)
    /// - `enabled`: 缓存是否启用
    /// - `adjustment_count`: 累计 TTL 调整次数
    /// - `disable_count`: 累计禁用次数
    /// - `decision_count`: 决策记录总数
    /// - `saved_at`: 历史保存时间 (ISO 8601, 可选)
    pub fn new(
        initial_ttl: u64,
        current_ttl: u64,
        enabled: bool,
        adjustment_count: u32,
        disable_count: u32,
        decision_count: usize,
        saved_at: Option<String>,
    ) -> Self {
        Self {
            initial_ttl,
            current_ttl,
            enabled,
            adjustment_count,
            disable_count,
            decision_count,
            ttl_delta: current_ttl as i64 - initial_ttl as i64,
            saved_at,
        }
    }

    /// 是否为空 (无决策记录)
    pub fn is_empty(&self) -> bool {
        self.decision_count == 0
    }

    /// TTL 变化百分比 (relative to initial)
    ///
    /// 无初始 TTL 时返回 0.0。
    pub fn ttl_delta_percent(&self) -> f64 {
        if self.initial_ttl == 0 {
            return 0.0;
        }
        self.ttl_delta as f64 / self.initial_ttl as f64 * 100.0
    }
}

impl Default for CacheTuningHistorySummary {
    fn default() -> Self {
        Self::new(0, 0, true, 0, 0, 0, None)
    }
}

/// 从原始数据构建搜索质量历史摘要 (纯函数)
///
/// # 参数
///
/// - `initial_enabled`: session 开始时的搜索启用状态
/// - `current_enabled`: 最终搜索启用状态
/// - `evaluation_count`: 累计评估次数
/// - `disable_count`: 累计禁用次数
/// - `saved_at`: 历史保存时间 (ISO 8601, 可选)
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::build_search_quality_history_summary;
/// let s = build_search_quality_history_summary(true, false, 3, 1, None);
/// assert!(s.enabled_changed);
/// assert!((s.disable_rate() - (1.0 / 3.0)).abs() < 0.001);
/// ```
pub fn build_search_quality_history_summary(
    initial_enabled: bool,
    current_enabled: bool,
    evaluation_count: u32,
    disable_count: u32,
    saved_at: Option<String>,
) -> SearchQualityHistorySummary {
    SearchQualityHistorySummary::new(
        initial_enabled,
        current_enabled,
        evaluation_count,
        disable_count,
        saved_at,
    )
}

/// 从原始数据构建缓存调优历史摘要 (纯函数)
///
/// # 参数
///
/// - `initial_ttl`: session 开始时的 TTL (秒)
/// - `current_ttl`: 最终 TTL (秒)
/// - `enabled`: 缓存是否启用
/// - `adjustment_count`: 累计 TTL 调整次数
/// - `disable_count`: 累计禁用次数
/// - `decision_count`: 决策记录总数
/// - `saved_at`: 历史保存时间 (ISO 8601, 可选)
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::build_cache_tuning_history_summary;
/// let s = build_cache_tuning_history_summary(1800, 2700, true, 1, 0, 1, None);
/// assert_eq!(s.ttl_delta, 900);
/// assert!((s.ttl_delta_percent() - 50.0).abs() < 0.001);
/// ```
pub fn build_cache_tuning_history_summary(
    initial_ttl: u64,
    current_ttl: u64,
    enabled: bool,
    adjustment_count: u32,
    disable_count: u32,
    decision_count: usize,
    saved_at: Option<String>,
) -> CacheTuningHistorySummary {
    CacheTuningHistorySummary::new(
        initial_ttl,
        current_ttl,
        enabled,
        adjustment_count,
        disable_count,
        decision_count,
        saved_at,
    )
}

// ============================================================================
//  MemoryEvaluationHistorySummary — 跨 Session 历史摘要 (Session 90)
// ============================================================================

/// Memory 评估历史摘要 — 跨 session 的 Memory 注入评估概览
///
/// 从 `MemoryEvaluationHistory` (`.forge/memory_evaluation_history.json`) 提取,
/// 展示 Memory 注入在多个 session 中的启用/禁用状态变化和累计评估统计。
///
/// # 字段
///
/// | 字段 | 说明 |
/// |------|------|
/// | `initial_enabled` | session 开始时的注入启用状态 |
/// | `current_enabled` | 最终注入启用状态 |
/// | `evaluation_count` | 累计评估次数 |
/// | `disable_count` | 累计禁用次数 |
/// | `enabled_changed` | 启用状态是否发生变化 |
/// | `saved_at` | 历史保存时间 (ISO 8601) |
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::MemoryEvaluationHistorySummary;
/// let s = MemoryEvaluationHistorySummary::new(true, false, 5, 1, None);
/// assert!(s.enabled_changed);
/// assert!(!s.is_empty());
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEvaluationHistorySummary {
    /// session 开始时的注入启用状态
    pub initial_enabled: bool,
    /// 最终注入启用状态
    pub current_enabled: bool,
    /// 累计评估次数
    pub evaluation_count: u32,
    /// 累计禁用次数
    pub disable_count: u32,
    /// 启用状态是否发生变化
    pub enabled_changed: bool,
    /// 历史保存时间 (ISO 8601, 可选)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved_at: Option<String>,
}

impl MemoryEvaluationHistorySummary {
    /// 创建 Memory 评估历史摘要
    pub fn new(
        initial_enabled: bool,
        current_enabled: bool,
        evaluation_count: u32,
        disable_count: u32,
        saved_at: Option<String>,
    ) -> Self {
        Self {
            initial_enabled,
            current_enabled,
            evaluation_count,
            disable_count,
            enabled_changed: initial_enabled != current_enabled,
            saved_at,
        }
    }

    /// 是否为空 (无评估记录)
    pub fn is_empty(&self) -> bool {
        self.evaluation_count == 0
    }

    /// 禁用率
    pub fn disable_rate(&self) -> f64 {
        if self.evaluation_count == 0 {
            return 0.0;
        }
        self.disable_count as f64 / self.evaluation_count as f64
    }
}

impl Default for MemoryEvaluationHistorySummary {
    fn default() -> Self {
        Self::new(true, true, 0, 0, None)
    }
}

/// 从原始数据构建 Memory 评估历史摘要 (纯函数)
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::build_memory_evaluation_history_summary;
/// let s = build_memory_evaluation_history_summary(true, false, 5, 1, None);
/// assert!(s.enabled_changed);
/// ```
pub fn build_memory_evaluation_history_summary(
    initial_enabled: bool,
    current_enabled: bool,
    evaluation_count: u32,
    disable_count: u32,
    saved_at: Option<String>,
) -> MemoryEvaluationHistorySummary {
    MemoryEvaluationHistorySummary::new(
        initial_enabled,
        current_enabled,
        evaluation_count,
        disable_count,
        saved_at,
    )
}

// ============================================================================
//  纯逻辑函数 — 统计计算
// ============================================================================

/// 计算成功率 (0.0 ~ 1.0)。
///
/// 当 `total` 为 0 时返回 0.0。
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::calculate_success_rate;
/// assert_eq!(calculate_success_rate(0, 0), 0.0);
/// assert_eq!(calculate_success_rate(10, 10), 1.0);
/// assert!((calculate_success_rate(3, 2) - 2.0 / 3.0).abs() < 0.001);
/// ```
pub fn calculate_success_rate(total: usize, success_count: usize) -> f64 {
    if total == 0 {
        return 0.0;
    }
    success_count as f64 / total as f64
}

// ============================================================================
//  ActionStats — 操作统计
// ============================================================================

/// 单个操作类型的统计信息
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ActionStats {
    /// 总次数
    pub count: usize,
    /// 成功次数
    pub success_count: usize,
    /// 总耗时 (毫秒)
    pub total_duration_ms: u64,
}

impl ActionStats {
    /// 创建空统计
    pub fn new() -> Self {
        Self::default()
    }

    /// 成功率 (0.0 ~ 1.0)
    pub fn success_rate(&self) -> f64 {
        calculate_success_rate(self.count, self.success_count)
    }

    /// 平均耗时 (毫秒)
    pub fn avg_duration_ms(&self) -> u64 {
        if self.count == 0 {
            return 0;
        }
        self.total_duration_ms / self.count as u64
    }

    /// 记录一次操作
    pub fn record(&mut self, duration_ms: u64, success: bool) {
        self.count += 1;
        if success {
            self.success_count += 1;
        }
        self.total_duration_ms += duration_ms;
    }
}

// ============================================================================
//  纯逻辑函数 — 增量发送解析 (Session 76)
// ============================================================================

/// 从 `IncrementalSend` trace 条目的 `input_summary` 中解析 `(total, sent, skipped)`。
///
/// `input_summary` 格式由 Orchestrator 的 `send_with_continuation` 写入:
/// - `"total=5, sent=2, skipped=3"` (正常增量发送)
/// - `"[全部跳过] total=5, sent=0, skipped=5"` (全部跳过)
///
/// 解析失败时返回 `None`。
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::parse_incremental_entry;
/// // 正常格式
/// assert_eq!(parse_incremental_entry("total=5, sent=2, skipped=3"), Some((5, 2, 3)));
///
/// // 带前缀的格式
/// assert_eq!(parse_incremental_entry("[全部跳过] total=5, sent=0, skipped=5"), Some((5, 0, 5)));
///
/// // 格式错误
/// assert_eq!(parse_incremental_entry("not a valid format"), None);
/// assert_eq!(parse_incremental_entry(""), None);
/// ```
pub fn parse_incremental_entry(input_summary: &str) -> Option<(usize, usize, usize)> {
    let total = parse_number_after(input_summary, "total")?;
    let sent = parse_number_after(input_summary, "sent")?;
    let skipped = parse_number_after(input_summary, "skipped")?;
    Some((total, sent, skipped))
}

/// 从字符串中查找 `key` 后面的数字并解析。
///
/// 在 `text` 中查找 `key` 的位置, 然后跳过可选空格和 `=` 号,
/// 再跳过可选空格, 读取连续的数字字符并解析为 `usize`。
///
/// 支持 `"total=5"` 和 `"total = 5"` 两种格式。
fn parse_number_after(text: &str, key: &str) -> Option<usize> {
    let pos = text.find(key)?;
    let after_key = &text[pos + key.len()..];
    // 跳过可选空格、= 号、再跳过可选空格
    let rest: &str = after_key.trim_start();
    let rest = rest.strip_prefix('=')?;
    let rest = rest.trim_start();
    // 读取连续的数字字符
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

// ============================================================================
//  纯逻辑函数 — 格式化
// ============================================================================

/// 将毫秒格式化为人类可读的时长字符串。
///
/// 格式: `"5.0s (0.1m)"`。
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::format_duration_human;
/// assert_eq!(format_duration_human(0), "0.0s (0.0m)");
/// assert_eq!(format_duration_human(5000), "5.0s (0.1m)");
/// assert_eq!(format_duration_human(60000), "60.0s (1.0m)");
/// ```
pub fn format_duration_human(ms: u64) -> String {
    format!("{:.1}s ({:.1}m)", ms as f64 / 1000.0, ms as f64 / 60000.0)
}

/// 将成功率 (0.0 ~ 1.0) 格式化为百分比字符串。
///
/// 格式: `"85.7%"`。
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::format_success_rate_percent;
/// assert_eq!(format_success_rate_percent(0.0), "0.0%");
/// assert_eq!(format_success_rate_percent(1.0), "100.0%");
/// assert_eq!(format_success_rate_percent(0.5), "50.0%");
/// ```
pub fn format_success_rate_percent(rate: f64) -> String {
    format!("{:.1}%", rate * 100.0)
}

// ============================================================================
//  TimelineEntry — 简化的时间线条目
// ============================================================================

/// 简化的时间线条目 — 用于 DevTraceSummary 的概览
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineEntry {
    /// 时间戳 (UTC)
    pub timestamp: DateTime<Utc>,
    /// 操作类型
    pub action: TraceAction,
    /// 任务名称 (便于人类阅读)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_name: Option<String>,
    /// 是否成功
    pub success: bool,
    /// 耗时 (毫秒)
    pub duration_ms: u64,
}

impl TimelineEntry {
    /// 从 DevTraceEntry 创建
    pub fn from_entry(entry: &DevTraceEntry) -> Self {
        Self {
            timestamp: entry.timestamp,
            action: entry.action,
            task_name: entry.task_name.clone(),
            success: entry.success,
            duration_ms: entry.duration_ms,
        }
    }
}

/// 格式化单条时间线条目为可读字符串。
///
/// 格式: `"  HH:MM:SS ✅ 任务执行  task (1000ms)\n"`。
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::{TimelineEntry, TraceAction, format_timeline_line};
/// # use chrono::Utc;
/// let entry = TimelineEntry {
///     timestamp: Utc::now(),
///     action: TraceAction::TaskExecution,
///     task_name: Some("初始化".to_string()),
///     success: true,
///     duration_ms: 3000,
/// };
/// let line = format_timeline_line(&entry);
/// assert!(line.contains("✅"));
/// assert!(line.contains("任务执行"));
/// assert!(line.contains("初始化"));
/// assert!(line.contains("3000ms"));
/// ```
pub fn format_timeline_line(entry: &TimelineEntry) -> String {
    let status = if entry.success { "✅" } else { "❌" };
    let task = entry.task_name.as_deref().unwrap_or("-");
    format!(
        "  {} {} {:20} {} ({}ms)\n",
        entry.timestamp.format("%H:%M:%S"),
        status,
        entry.action.description(),
        task,
        entry.duration_ms
    )
}

// ============================================================================
//  DevTraceSummary — 追踪摘要
// ============================================================================

/// 开发追踪摘要 — 24 小时运行后的快速概览
///
/// 包含总条目数、总耗时、按操作类型的统计、成功率和简化时间线。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevTraceSummary {
    /// 总条目数
    pub total_entries: usize,
    /// 总耗时 (毫秒)
    pub total_duration_ms: u64,
    /// 按操作类型的统计
    pub by_action: HashMap<TraceAction, ActionStats>,
    /// 总体成功率 (0.0 ~ 1.0)
    pub success_rate: f64,
    /// 简化时间线 (最近 100 条)
    pub timeline: Vec<TimelineEntry>,
    /// 增量发送统计摘要 (Session 76)
    ///
    /// 从 `IncrementalSend` trace 条目中解析的累计统计。
    /// `None` 表示没有增量发送记录 (未启用增量发送或无 trace 数据)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incremental_summary: Option<IncrementalStats>,

    /// 搜索缓存统计摘要 (Session 79)
    ///
    /// 从 `WebSearch` trace 条目中解析的缓存命中/未命中/失败统计。
    /// `None` 表示没有搜索缓存记录 (未启用自动搜索或无 trace 数据)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_summary: Option<CacheStatsSummary>,

    /// 缓存与修复关联分析 (Session 80)
    ///
    /// 从 `WebSearch` + `CompileCheck` trace 条目中解析的关联统计,
    /// 分析缓存命中/未命中/失败后的修复成功率。
    /// `None` 表示没有关联数据 (无 WebSearch 或无后续 CompileCheck)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_fix_correlation: Option<CacheFixCorrelation>,

    /// 缓存调优效果摘要 (Session 83)
    ///
    /// 从 `CacheTuning` trace 条目中解析的调优历史统计,
    /// 展示 CacheTuner 的调整历史和效果。
    /// `None` 表示没有缓存调优记录 (未启用 CacheTuner 或无 trace 数据)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_tuning_summary: Option<CacheTuningSummary>,

    /// 搜索质量统计摘要 (Session 85)
    ///
    /// 从 `WebSearch` + `CompileCheck` trace 条目中解析的搜索质量统计,
    /// 比较使用搜索结果 vs 不使用搜索结果的修复成功率。
    /// `None` 表示没有搜索质量数据 (无 WebSearch 或无 CompileCheck)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_quality_summary: Option<SearchQualityStats>,

    /// 搜索质量历史摘要 (Session 87)
    ///
    /// 从 `SearchQualityHistory` (`.forge/search_quality_history.json`) 提取的
    /// 跨 session 持久化数据, 展示搜索功能的长期启用/禁用趋势。
    /// `None` 表示没有历史数据 (首次运行或未启用 SearchQualityEvaluator)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_quality_history_summary: Option<SearchQualityHistorySummary>,

    /// 缓存调优历史摘要 (Session 87)
    ///
    /// 从 `CacheTuningHistory` (`.forge/cache_tuning_history.json`) 提取的
    /// 跨 session 持久化数据, 展示缓存策略的长期 TTL 变化趋势。
    /// `None` 表示没有历史数据 (首次运行或未启用 CacheTuner)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_tuning_history_summary: Option<CacheTuningHistorySummary>,

    /// Memory 评估统计摘要 (Session 90)
    ///
    /// 从 `MemoryInjection` + `CompileCheck` trace 条目中解析的注入效果统计,
    /// 比较有注入 vs 无注入的修复成功率。
    /// `None` 表示没有 Memory 评估数据。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_evaluation_summary: Option<MemoryEvaluationStats>,

    /// Memory 评估历史摘要 (Session 90)
    ///
    /// 从 `MemoryEvaluationHistory` 提取的跨 session 数据。
    /// `None` 表示没有历史数据。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_evaluation_history_summary: Option<MemoryEvaluationHistorySummary>,
}

impl DevTraceSummary {
    /// 创建空摘要
    pub fn empty() -> Self {
        Self {
            total_entries: 0,
            total_duration_ms: 0,
            by_action: HashMap::new(),
            success_rate: 0.0,
            timeline: vec![],
            incremental_summary: None,
            cache_summary: None,
            cache_fix_correlation: None,
            cache_tuning_summary: None,
            search_quality_summary: None,
            search_quality_history_summary: None,
            cache_tuning_history_summary: None,
            memory_evaluation_summary: None,
            memory_evaluation_history_summary: None,
        }
    }

    /// 从 trace 条目列表构建摘要
    ///
    /// 除了常规统计外, 还会从 `IncrementalSend` 条目中解析
    /// 增量发送的累计统计 (total/sent/skipped), 用于报告中的
    /// 增量发送效果可视化 (Session 76)。
    pub fn from_entries(entries: &[DevTraceEntry]) -> Self {
        let total_entries = entries.len();
        let total_duration_ms: u64 = entries.iter().map(|e| e.duration_ms).sum();
        let success_count = entries.iter().filter(|e| e.success).count();
        let success_rate = calculate_success_rate(total_entries, success_count);
        let by_action = group_entries_by_action(entries);
        let timeline = build_timeline(entries, 100);

        // 从 IncrementalSend 条目中解析累计增量发送统计
        let incremental_stats = entries
            .iter()
            .filter(|e| e.action == TraceAction::IncrementalSend)
            .filter_map(|e| parse_incremental_entry(&e.input_summary))
            .fold(
                IncrementalStats::new(),
                |mut acc, (total, sent, _skipped)| {
                    acc.record(total, sent);
                    acc
                },
            );

        // 只在有增量发送记录时才包含
        let incremental_summary = if incremental_stats.send_count > 0 {
            Some(incremental_stats)
        } else {
            None
        };

        // 从 WebSearch 条目中解析缓存统计 (Session 79)
        let cache_stats = build_cache_summary(entries);
        let cache_summary = if cache_stats.is_empty() {
            None
        } else {
            Some(cache_stats)
        };

        // 从 WebSearch + CompileCheck 条目中解析缓存与修复关联 (Session 80)
        let correlation = build_cache_fix_correlation(entries);
        let cache_fix_correlation = if correlation.is_empty() {
            None
        } else {
            Some(correlation)
        };

        // 从 CacheTuning 条目中解析缓存调优效果 (Session 83)
        let tuning_stats = build_cache_tuning_summary(entries);
        let cache_tuning_summary = if tuning_stats.is_empty() {
            None
        } else {
            Some(tuning_stats)
        };

        // 从 WebSearch + CompileCheck 条目中解析搜索质量统计 (Session 85)
        let sq_stats = build_search_quality_stats(entries);
        let search_quality_summary = if sq_stats.is_empty() {
            None
        } else {
            Some(sq_stats)
        };

        // 从 MemoryInjection + CompileCheck 条目中解析 Memory 评估统计 (Session 90)
        let me_stats = build_memory_evaluation_stats(entries);
        let memory_evaluation_summary = if me_stats.is_empty() {
            None
        } else {
            Some(me_stats)
        };

        Self {
            total_entries,
            total_duration_ms,
            by_action,
            success_rate,
            timeline,
            incremental_summary,
            cache_summary,
            cache_fix_correlation,
            cache_tuning_summary,
            search_quality_summary,
            // 跨 session 历史摘要来自外部持久化文件, 不从 trace 条目解析
            search_quality_history_summary: None,
            cache_tuning_history_summary: None,
            memory_evaluation_summary,
            memory_evaluation_history_summary: None,
        }
    }

    /// 获取某个操作类型的统计 (如有)
    pub fn get_action_stats(&self, action: TraceAction) -> Option<&ActionStats> {
        self.by_action.get(&action)
    }

    /// 附加搜索质量历史摘要 (builder 模式)
    ///
    /// 从 `SearchQualityHistory` 提取的跨 session 数据,
    /// 由 Orchestrator 在 `final_report` 时调用。
    ///
    /// # 参数
    ///
    /// - `summary`: 搜索质量历史摘要
    pub fn with_search_quality_history(mut self, summary: SearchQualityHistorySummary) -> Self {
        self.search_quality_history_summary = Some(summary);
        self
    }

    /// 附加缓存调优历史摘要 (builder 模式)
    ///
    /// 从 `CacheTuningHistory` 提取的跨 session 数据,
    /// 由 Orchestrator 在 `final_report` 时调用。
    ///
    /// # 参数
    ///
    /// - `summary`: 缓存调优历史摘要
    pub fn with_cache_tuning_history(mut self, summary: CacheTuningHistorySummary) -> Self {
        self.cache_tuning_history_summary = Some(summary);
        self
    }

    /// 附加 Memory 评估历史摘要 (builder 模式)
    ///
    /// 从 `MemoryEvaluationHistory` 提取的跨 session 数据,
    /// 由 Orchestrator 在 `final_report` 时调用。
    pub fn with_memory_evaluation_history(
        mut self,
        summary: MemoryEvaluationHistorySummary,
    ) -> Self {
        self.memory_evaluation_history_summary = Some(summary);
        self
    }

    /// 生成可读的报告文本
    pub fn to_report(&self) -> String {
        let mut report = String::new();
        report.push_str("═══════════════════════════════════════════════════\n");
        report.push_str("  📊 DevTrace 开发追踪报告\n");
        report.push_str("═══════════════════════════════════════════════════\n\n");

        report.push_str(&format!("  总条目: {}\n", self.total_entries));
        report.push_str(&format!(
            "  总耗时: {}\n",
            format_duration_human(self.total_duration_ms)
        ));
        report.push_str(&format!(
            "  成功率: {}\n\n",
            format_success_rate_percent(self.success_rate)
        ));

        report.push_str("  ── 按操作类型统计 ──\n");
        for action in TraceAction::all() {
            if let Some(stats) = self.by_action.get(&action) {
                report.push_str(&format_action_stats_line(action, stats));
            }
        }

        // === 增量发送统计 (Session 76) ===
        if let Some(ref inc_stats) = self.incremental_summary {
            report.push_str("\n  ── 增量发送统计 ──\n");
            report.push_str(&format!("  发送次数: {}\n", inc_stats.send_count));
            report.push_str(&format!(
                "  总消息: {} 条 (含重复)\n",
                inc_stats.total_messages
            ));
            report.push_str(&format!(
                "  实际发送: {} 条 (增量)\n",
                inc_stats.sent_messages
            ));
            report.push_str(&format!(
                "  跳过: {} 条 (已发送, 复用)\n",
                inc_stats.skipped_messages
            ));
            report.push_str(&format!(
                "  节省比例: {:.1}%\n",
                inc_stats.saved_ratio() * 100.0
            ));
            report.push_str(&format!(
                "  平均每次: 总 {:.1} 条, 实发 {:.1} 条\n",
                inc_stats.avg_messages_per_send(),
                inc_stats.avg_sent_per_send()
            ));
        }

        // === 搜索缓存统计 (Session 79) ===
        if let Some(ref cache_stats) = self.cache_summary {
            report.push_str("\n  ── 搜索缓存统计 ──\n");
            report.push_str(&format!("  总搜索: {} 次\n", cache_stats.total_searches()));
            report.push_str(&format!("  缓存命中: {} 次\n", cache_stats.cache_hits));
            report.push_str(&format!("  缓存未命中: {} 次\n", cache_stats.cache_misses));
            report.push_str(&format!("  搜索失败: {} 次\n", cache_stats.search_failures));
            report.push_str(&format!(
                "  命中率: {:.1}%\n",
                cache_stats.hit_rate() * 100.0
            ));
            report.push_str(&format!(
                "  节省时间: {}\n",
                format_duration_human(cache_stats.time_saved_ms)
            ));
            report.push_str(&format!(
                "  平均每次命中: {:.0}ms\n",
                cache_stats.avg_time_saved_per_hit()
            ));
        }

        // === 缓存与修复关联分析 (Session 80) ===
        if let Some(ref corr) = self.cache_fix_correlation {
            report.push_str("\n  ── 缓存与修复关联分析 ──\n");
            report.push_str(&format!(
                "  命中后检查: {} 次 (通过 {})\n",
                corr.checks_after_hit, corr.successes_after_hit
            ));
            report.push_str(&format!(
                "  未命中后检查: {} 次 (通过 {})\n",
                corr.checks_after_miss, corr.successes_after_miss
            ));
            report.push_str(&format!(
                "  搜索失败后检查: {} 次 (通过 {})\n",
                corr.checks_after_failure, corr.successes_after_failure
            ));
            if corr.searches_without_check > 0 {
                report.push_str(&format!(
                    "  无后续检查: {} 次\n",
                    corr.searches_without_check
                ));
            }
            report.push_str(&format!(
                "  命中后修复率: {:.1}%\n",
                corr.hit_fix_rate() * 100.0
            ));
            report.push_str(&format!(
                "  未命中后修复率: {:.1}%\n",
                corr.miss_fix_rate() * 100.0
            ));
            if corr.checks_after_hit > 0 && corr.checks_after_miss > 0 {
                report.push_str(&format!(
                    "  差值: {:+.1}% ({})\n",
                    corr.hit_vs_miss_diff() * 100.0,
                    if corr.is_cache_effective() {
                        "缓存有效"
                    } else {
                        "缓存效果不足"
                    }
                ));
            }
        }

        // === 缓存调优效果 (Session 83) ===
        if let Some(ref tuning) = self.cache_tuning_summary {
            report.push_str("\n  ── 缓存调优效果 ──\n");
            report.push_str(&format!("  总评估: {} 次\n", tuning.total_evaluations));
            if tuning.keep_current_count > 0 {
                report.push_str(&format!("  保持当前: {} 次\n", tuning.keep_current_count));
            }
            if tuning.adjust_ttl_count > 0 {
                report.push_str(&format!("  调整 TTL: {} 次\n", tuning.adjust_ttl_count));
            }
            if tuning.disable_count > 0 {
                report.push_str(&format!("  禁用缓存: {} 次\n", tuning.disable_count));
            }
            // TTL 变化轨迹
            if !tuning.ttl_history.is_empty() {
                report.push_str("  TTL 变化: ");
                if let Some(initial) = tuning.initial_ttl() {
                    report.push_str(&format!("{}s", initial));
                }
                for (_, new_ttl) in &tuning.ttl_history {
                    report.push_str(&format!(" → {}s", new_ttl));
                }
                report.push('\n');
            }
            // 最终状态
            if tuning.cache_disabled {
                report.push_str("  缓存状态: 已禁用\n");
            } else if let Some(final_ttl) = tuning.final_ttl {
                report.push_str(&format!("  最终 TTL: {}s\n", final_ttl));
            }
            // 平均差值
            if !tuning.correlation_diffs.is_empty() {
                report.push_str(&format!(
                    "  平均差值: {:+.1}%\n",
                    tuning.avg_correlation_diff() * 100.0
                ));
            }
        }

        // === 搜索质量评估 (Session 85) ===
        if let Some(ref sq) = self.search_quality_summary {
            report.push_str("\n  ── 搜索质量评估 ──\n");
            report.push_str(&format!(
                "  有搜索修复: {} 次检查 (通过 {})\n",
                sq.checks_with_search, sq.successes_with_search
            ));
            report.push_str(&format!(
                "  无搜索修复: {} 次检查 (通过 {})\n",
                sq.checks_without_search, sq.successes_without_search
            ));
            report.push_str(&format!(
                "  搜索次数: {} (成功 {}, 失败 {})\n",
                sq.total_searches, sq.successful_searches, sq.failed_searches
            ));
            if sq.checks_with_search > 0 {
                report.push_str(&format!(
                    "  有搜索修复率: {:.1}%\n",
                    sq.with_search_fix_rate() * 100.0
                ));
            }
            if sq.checks_without_search > 0 {
                report.push_str(&format!(
                    "  无搜索修复率: {:.1}%\n",
                    sq.without_search_fix_rate() * 100.0
                ));
            }
            if sq.checks_with_search > 0 && sq.checks_without_search > 0 {
                report.push_str(&format!(
                    "  差值: {:+.1}% ({})\n",
                    sq.search_vs_no_search_diff() * 100.0,
                    if sq.is_search_beneficial() {
                        "搜索有效"
                    } else {
                        "搜索效果不足"
                    }
                ));
            }
        }

        // === 搜索质量历史 (Session 87) ===
        if let Some(ref sqh) = self.search_quality_history_summary {
            report.push_str("\n  ── 搜索质量历史 (跨 Session) ──\n");
            let initial_str = if sqh.initial_enabled {
                "启用"
            } else {
                "禁用"
            };
            let current_str = if sqh.current_enabled {
                "启用"
            } else {
                "禁用"
            };
            report.push_str(&format!("  初始状态: {}\n", initial_str));
            report.push_str(&format!("  最终状态: {}\n", current_str));
            if sqh.enabled_changed {
                report.push_str("  状态变化: ✅ 已变更\n");
            } else {
                report.push_str("  状态变化: ─ 未变\n");
            }
            report.push_str(&format!("  累计评估: {} 次\n", sqh.evaluation_count));
            if sqh.disable_count > 0 {
                report.push_str(&format!("  累计禁用: {} 次\n", sqh.disable_count));
                report.push_str(&format!("  禁用率: {:.1}%\n", sqh.disable_rate() * 100.0));
            }
            if let Some(ref saved_at) = sqh.saved_at {
                report.push_str(&format!("  保存时间: {}\n", saved_at));
            }
        }

        // === 缓存调优历史 (Session 87) ===
        if let Some(ref cth) = self.cache_tuning_history_summary {
            report.push_str("\n  ── 缓存调优历史 (跨 Session) ──\n");
            report.push_str(&format!("  初始 TTL: {}s\n", cth.initial_ttl));
            report.push_str(&format!("  最终 TTL: {}s\n", cth.current_ttl));
            if cth.ttl_delta != 0 {
                let delta_sign = if cth.ttl_delta > 0 { "+" } else { "" };
                report.push_str(&format!(
                    "  TTL 变化: {}{}s ({:.1}%)\n",
                    delta_sign,
                    cth.ttl_delta,
                    cth.ttl_delta_percent()
                ));
            }
            let status = if cth.enabled { "启用" } else { "已禁用" };
            report.push_str(&format!("  缓存状态: {}\n", status));
            if cth.adjustment_count > 0 {
                report.push_str(&format!("  累计调整: {} 次\n", cth.adjustment_count));
            }
            if cth.disable_count > 0 {
                report.push_str(&format!("  累计禁用: {} 次\n", cth.disable_count));
            }
            report.push_str(&format!("  决策记录: {} 条\n", cth.decision_count));
            if let Some(ref saved_at) = cth.saved_at {
                report.push_str(&format!("  保存时间: {}\n", saved_at));
            }
        }

        // === Memory 评估 (Session 90) ===
        if let Some(ref me) = self.memory_evaluation_summary {
            report.push_str("\n  ── Memory 注入评估 ──\n");
            report.push_str(&format!(
                "  有注入修复: {} 次检查 (通过 {})\n",
                me.checks_with_memory, me.successes_with_memory
            ));
            report.push_str(&format!(
                "  无注入修复: {} 次检查 (通过 {})\n",
                me.checks_without_memory, me.successes_without_memory
            ));
            report.push_str(&format!("  总注入: {} 次\n", me.total_injections));
            if me.checks_with_memory > 0 {
                report.push_str(&format!(
                    "  有注入修复率: {:.1}%\n",
                    me.with_memory_fix_rate() * 100.0
                ));
            }
            if me.checks_without_memory > 0 {
                report.push_str(&format!(
                    "  无注入修复率: {:.1}%\n",
                    me.without_memory_fix_rate() * 100.0
                ));
            }
            if me.checks_with_memory > 0 && me.checks_without_memory > 0 {
                report.push_str(&format!(
                    "  差值: {:+.1}% ({})\n",
                    me.memory_vs_no_memory_diff() * 100.0,
                    if me.is_memory_beneficial() {
                        "注入有效"
                    } else {
                        "注入效果不足"
                    }
                ));
            }
        }

        // === Memory 评估历史 (Session 90) ===
        if let Some(ref meh) = self.memory_evaluation_history_summary {
            report.push_str("\n  ── Memory 评估历史 (跨 Session) ──\n");
            let initial_str = if meh.initial_enabled {
                "启用"
            } else {
                "禁用"
            };
            let current_str = if meh.current_enabled {
                "启用"
            } else {
                "禁用"
            };
            report.push_str(&format!("  初始状态: {}\n", initial_str));
            report.push_str(&format!("  最终状态: {}\n", current_str));
            if meh.enabled_changed {
                report.push_str("  状态变化: ✅ 已变更\n");
            } else {
                report.push_str("  状态变化: ─ 未变\n");
            }
            report.push_str(&format!("  累计评估: {} 次\n", meh.evaluation_count));
            if meh.disable_count > 0 {
                report.push_str(&format!("  累计禁用: {} 次\n", meh.disable_count));
                report.push_str(&format!("  禁用率: {:.1}%\n", meh.disable_rate() * 100.0));
            }
            if let Some(ref saved_at) = meh.saved_at {
                report.push_str(&format!("  保存时间: {}\n", saved_at));
            }
        }

        if !self.timeline.is_empty() {
            report.push_str("\n  ── 时间线 (最近 100 条) ──\n");
            for entry in &self.timeline {
                report.push_str(&format_timeline_line(entry));
            }
        }

        report
    }

    // ===== JSON 导出 (Session 88) =====

    /// 将摘要序列化为 pretty JSON 字符串。
    ///
    /// 使用缩进格式, 便于人工阅读和外部工具解析。
    /// 包含所有字段 (总条目、统计、时间线、缓存、增量、历史等)。
    ///
    /// # 错误
    ///
    /// 仅在序列化失败时返回错误 (理论上不会发生, 因为所有字段都实现了 `Serialize`)。
    ///
    /// # 示例
    ///
    /// ```
    /// # use forge::dev_trace::DevTraceSummary;
    /// let summary = DevTraceSummary::empty();
    /// let json = summary.to_json().unwrap();
    /// assert!(json.contains("\"total_entries\": 0"));
    /// ```
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// 将摘要序列化为 compact JSON 字符串 (无缩进, 节省空间)。
    ///
    /// 适用于存储空间敏感的场景 (如写入小文件或通过网络传输)。
    ///
    /// # 错误
    ///
    /// 仅在序列化失败时返回错误。
    ///
    /// # 示例
    ///
    /// ```
    /// # use forge::dev_trace::DevTraceSummary;
    /// let summary = DevTraceSummary::empty();
    /// let json = summary.to_json_compact().unwrap();
    /// assert!(json.contains("\"total_entries\":0"));
    /// ```
    pub fn to_json_compact(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    /// 将摘要 (含元数据) 序列化为 pretty JSON 字符串。
    ///
    /// 包装在 [`DevTraceJsonExport`] 中, 包含导出时间、Forge 版本和格式版本。
    /// 适用于需要追踪导出上下文的外部工具分析。
    ///
    /// # 参数
    ///
    /// - `timestamp`: ISO 8601 格式的时间戳 (如 `2024-06-01T00:00:00+00:00`)
    ///
    /// # 示例
    ///
    /// ```
    /// # use forge::dev_trace::DevTraceSummary;
    /// let summary = DevTraceSummary::empty();
    /// let json = summary.to_json_with_meta("2024-06-01T00:00:00+00:00").unwrap();
    /// assert!(json.contains("\"exported_at\": \"2024-06-01T00:00:00+00:00\""));
    /// assert!(json.contains("\"format_version\": \"1.0\""));
    /// ```
    pub fn to_json_with_meta(&self, timestamp: &str) -> Result<String> {
        let export = build_dev_trace_json_export(self.clone(), timestamp.to_string());
        Ok(serde_json::to_string_pretty(&export)?)
    }

    /// 将摘要保存为 JSON 文件 (不含元数据)。
    ///
    /// 使用 pretty 格式, 文件路径通常为 `.forge/devtrace_summary.json`。
    ///
    /// # 错误
    ///
    /// 文件写入失败时返回 `io::Error`。
    pub fn save_to_json_file(&self, path: &Path) -> Result<()> {
        let json = self.to_json()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, json)?;
        Ok(())
    }

    /// 将摘要 (含元数据) 保存为 JSON 文件。
    ///
    /// 包含导出时间、Forge 版本和格式版本, 便于外部工具识别。
    /// 文件路径通常为 `.forge/devtrace_summary.json`。
    ///
    /// # 参数
    ///
    /// - `path`: 目标文件路径
    /// - `timestamp`: ISO 8601 格式的时间戳
    ///
    /// # 错误
    ///
    /// 文件写入失败时返回 `io::Error`。
    pub fn save_to_json_file_with_meta(&self, path: &Path, timestamp: &str) -> Result<()> {
        let export = build_dev_trace_json_export(self.clone(), timestamp.to_string());
        let json = serde_json::to_string_pretty(&export)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, json)?;
        Ok(())
    }
}

// ============================================================================
//  JSON 导出 — 元数据与包装结构 (Session 88)
// ============================================================================

/// DevTrace JSON 导出元数据。
///
/// 包含导出时间、Forge 版本和格式版本, 便于外部工具识别和解析。
/// 随 [`DevTraceJsonExport`] 一起序列化。
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::DevTraceExportMeta;
/// let meta = DevTraceExportMeta {
///     exported_at: "2024-06-01T00:00:00+00:00".to_string(),
///     forge_version: "0.1.0".to_string(),
///     format_version: "1.0".to_string(),
/// };
/// assert_eq!(meta.format_version, "1.0");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevTraceExportMeta {
    /// 导出时间 (ISO 8601 格式, 如 `2024-06-01T00:00:00+00:00`)
    pub exported_at: String,
    /// Forge 版本号 (从 `Cargo.toml` 编译时确定)
    pub forge_version: String,
    /// JSON 格式版本 (当前为 `"1.0"`)
    pub format_version: String,
}

/// DevTrace JSON 导出包装。
///
/// 将元数据与追踪摘要组合在一起, 便于外部工具解析和分析。
/// 通过 [`DevTraceSummary::to_json_with_meta`] 或 [`build_dev_trace_json_export`] 生成。
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::{DevTraceSummary, build_dev_trace_json_export};
/// let summary = DevTraceSummary::empty();
/// let export = build_dev_trace_json_export(summary, "2024-06-01T00:00:00+00:00".to_string());
/// assert_eq!(export.meta.format_version, "1.0");
/// assert_eq!(export.summary.total_entries, 0);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevTraceJsonExport {
    /// 导出元数据
    pub meta: DevTraceExportMeta,
    /// 追踪摘要
    pub summary: DevTraceSummary,
}

// ============================================================================
//  纯逻辑函数 — 时间线/统计/格式化
// ============================================================================

/// 构建当前时间的 ISO 8601 时间戳 (纯函数)。
///
/// 返回 RFC 3339 格式的时间戳, 如 `2024-06-01T00:00:00+00:00`。
/// 用于 [`DevTraceJsonExport`] 的 `exported_at` 字段。
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::build_export_timestamp;
/// let ts = build_export_timestamp();
/// assert!(ts.contains('T')); // ISO 8601 格式
/// ```
pub fn build_export_timestamp() -> String {
    Utc::now().to_rfc3339()
}

/// 构建 DevTrace JSON 导出包装 (纯函数, 遵循 DIP)。
///
/// 将 [`DevTraceSummary`] 和时间戳组合为 [`DevTraceJsonExport`],
/// 自动填充 Forge 版本 (`env!("CARGO_PKG_VERSION")`) 和格式版本 (`"1.0"`)。
///
/// # 参数
///
/// - `summary`: 追踪摘要
/// - `timestamp`: ISO 8601 格式的时间戳
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::{DevTraceSummary, build_dev_trace_json_export};
/// let summary = DevTraceSummary::empty();
/// let export = build_dev_trace_json_export(summary, "2024-06-01T00:00:00+00:00".to_string());
/// assert_eq!(export.meta.exported_at, "2024-06-01T00:00:00+00:00");
/// assert_eq!(export.meta.format_version, "1.0");
/// ```
pub fn build_dev_trace_json_export(
    summary: DevTraceSummary,
    timestamp: String,
) -> DevTraceJsonExport {
    DevTraceJsonExport {
        meta: DevTraceExportMeta {
            exported_at: timestamp,
            forge_version: env!("CARGO_PKG_VERSION").to_string(),
            format_version: "1.0".to_string(),
        },
        summary,
    }
}

/// 从 trace 条目列表构建时间线, 限制为最近 `max_entries` 条。
///
/// 当条目数不超过 `max_entries` 时返回全部条目的时间线;
/// 否则只返回最后 `max_entries` 条。
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::{DevTraceEntry, TraceAction, build_timeline};
/// let entries: Vec<DevTraceEntry> = (0..5).map(|i| {
///     DevTraceEntry::new(TraceAction::TaskExecution, Some(0), Some(i), Some("task"), "in", "out", 100, true, None)
/// }).collect();
/// let timeline = build_timeline(&entries, 3);
/// assert_eq!(timeline.len(), 3); // 只保留最后 3 条
/// ```
pub fn build_timeline(entries: &[DevTraceEntry], max_entries: usize) -> Vec<TimelineEntry> {
    if entries.len() <= max_entries {
        entries.iter().map(TimelineEntry::from_entry).collect()
    } else {
        entries[entries.len() - max_entries..]
            .iter()
            .map(TimelineEntry::from_entry)
            .collect()
    }
}

/// 按操作类型分组统计 trace 条目。
///
/// 返回 `HashMap<TraceAction, ActionStats>`, 每种操作类型的统计信息。
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::{DevTraceEntry, TraceAction, group_entries_by_action};
/// let entries = vec![
///     DevTraceEntry::new(TraceAction::TaskExecution, None, None, None, "in", "out", 100, true, None),
///     DevTraceEntry::new(TraceAction::TaskExecution, None, None, None, "in", "out", 200, false, None),
///     DevTraceEntry::new(TraceAction::CompileCheck, None, None, None, "in", "out", 50, true, None),
/// ];
/// let grouped = group_entries_by_action(&entries);
/// assert_eq!(grouped.len(), 2);
/// let task_stats = grouped.get(&TraceAction::TaskExecution).unwrap();
/// assert_eq!(task_stats.count, 2);
/// ```
pub fn group_entries_by_action(entries: &[DevTraceEntry]) -> HashMap<TraceAction, ActionStats> {
    let mut by_action: HashMap<TraceAction, ActionStats> = HashMap::new();
    for entry in entries {
        let stats = by_action.entry(entry.action).or_default();
        stats.record(entry.duration_ms, entry.success);
    }
    by_action
}

/// 格式化单条操作统计行为可读字符串。
///
/// 格式: `"  任务执行           次数:    2  成功:    1 ( 50.0%)  平均:  1500ms\n"`。
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::{ActionStats, TraceAction, format_action_stats_line};
/// let mut stats = ActionStats::new();
/// stats.record(1000, true);
/// stats.record(2000, false);
/// let line = format_action_stats_line(TraceAction::TaskExecution, &stats);
/// assert!(line.contains("任务执行"));
/// assert!(line.contains("次数:"));
/// assert!(line.contains("50.0%"));
/// ```
pub fn format_action_stats_line(action: TraceAction, stats: &ActionStats) -> String {
    format!(
        "  {:20} 次数: {:4}  成功: {:4} ({:5.1}%)  平均: {:5}ms\n",
        action.description(),
        stats.count,
        stats.success_count,
        stats.success_rate() * 100.0,
        stats.avg_duration_ms()
    )
}

// ============================================================================
//  DevTraceWriter — JSONL 写入/读取器
// ============================================================================

/// 开发追踪写入器 — 将 trace 条目流式写入 JSONL 文件
///
/// 文件位于 `<workspace>/.forge/devtrace.jsonl`, 每行一个 JSON 对象。
/// 使用追加模式写入, 支持 24 小时不间断运行。
/// `write_entry` 接受 `&self` (非 `&mut self`), 避免与 Orchestrator 的借用冲突。
pub struct DevTraceWriter {
    /// trace 文件路径 (`workspace`/.forge/devtrace.jsonl 或 devtrace.json)
    pub trace_path: PathBuf,
    /// 存储后端类型 (Session 69: 工厂模式集成)
    ///
    /// 决定写入格式: Jsonl → JSONL 追加, Json → JSON 数组
    /// Sqlite/Postgres 未实现, 回退到 Jsonl
    pub backend: StorageBackend,
}

impl Clone for DevTraceWriter {
    /// 克隆 DevTraceWriter — 共享同一 trace 文件路径和后端配置
    ///
    /// 用于将 DevTraceWriter 共享给 FailoverChatClient,
    /// 使健康检查和网站切换事件也能写入同一 trace 文件。
    fn clone(&self) -> Self {
        Self {
            trace_path: self.trace_path.clone(),
            backend: self.backend,
        }
    }
}

impl DevTraceWriter {
    /// 创建 DevTraceWriter (默认 JSONL 后端)
    ///
    /// trace 文件路径为 `<workspace_root>/.forge/devtrace.jsonl`。
    /// 文件在首次 `write_entry` 时自动创建 (追加模式)。
    pub fn new(workspace_root: &Path) -> Self {
        let trace_path = workspace_root.join(".forge").join("devtrace.jsonl");
        Self {
            trace_path,
            backend: StorageBackend::Jsonl,
        }
    }

    /// 创建 DevTraceWriter 并指定存储后端 (Session 69: 工厂模式集成)
    ///
    /// 根据后端类型选择文件格式:
    /// - `Jsonl` → `devtrace.jsonl` (JSONL 追加模式, 默认)
    /// - `Json` → `devtrace.json` (JSON 数组模式, 便于整体读取)
    /// - `Sqlite`/`Postgres` → 回退到 JSONL (未实现)
    ///
    /// # 示例
    ///
    /// ```
    /// use forge::dev_trace::DevTraceWriter;
    /// use forge::trace_store::StorageBackend;
    /// use std::path::Path;
    ///
    /// let writer = DevTraceWriter::new_with_backend(
    ///     Path::new("/tmp"),
    ///     StorageBackend::Json,
    /// );
    /// assert!(writer.trace_path.ends_with("devtrace.json"));
    /// ```
    pub fn new_with_backend(workspace_root: &Path, backend: StorageBackend) -> Self {
        let filename = match backend {
            StorageBackend::Json => "devtrace.json",
            StorageBackend::Jsonl | StorageBackend::Sqlite | StorageBackend::Postgres => {
                "devtrace.jsonl"
            }
        };
        let trace_path = workspace_root.join(".forge").join(filename);
        Self {
            trace_path,
            // Sqlite/Postgres 回退到 Jsonl
            backend: match backend {
                StorageBackend::Sqlite | StorageBackend::Postgres => StorageBackend::Jsonl,
                other => other,
            },
        }
    }

    /// 从 TraceStorageConfig 创建 DevTraceWriter (Session 69: 工厂模式集成)
    ///
    /// 使用 `trace_store::StorageConfig` 配置创建写入器,
    /// 便于从 `config.toml` 的 `[storage]` 配置直接初始化。
    ///
    /// # 示例
    ///
    /// ```
    /// use forge::dev_trace::DevTraceWriter;
    /// use forge::trace_store::{StorageBackend, StorageConfig};
    /// use std::path::PathBuf;
    ///
    /// let config = StorageConfig {
    ///     backend: StorageBackend::Jsonl,
    ///     path: PathBuf::from("/tmp/.forge/devtrace.jsonl"),
    /// };
    /// let writer = DevTraceWriter::from_storage_config(&config);
    /// assert_eq!(writer.backend_type(), StorageBackend::Jsonl);
    /// ```
    pub fn from_storage_config(config: &TraceStorageConfig) -> Self {
        Self {
            trace_path: config.path.clone(),
            backend: match config.backend {
                StorageBackend::Sqlite | StorageBackend::Postgres => StorageBackend::Jsonl,
                other => other,
            },
        }
    }

    /// 获取存储后端类型
    pub fn backend_type(&self) -> StorageBackend {
        self.backend
    }

    /// 写入一条 trace 条目
    ///
    /// 根据后端类型选择写入方式:
    /// - `Jsonl`: 追加模式, 每行一个 JSON 对象 (高效, 默认)
    /// - `Json`: 读取全部 → 追加 → 写回 (便于整体读取, 适合少量数据)
    ///
    /// 使用 `&self` (非 `&mut self`), 避免与 Orchestrator 的借用冲突。
    pub fn write_entry(&self, entry: &DevTraceEntry) -> Result<()> {
        match self.backend {
            StorageBackend::Jsonl => {
                let line = entry.to_jsonl()?;
                let mut file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.trace_path)?;
                writeln!(file, "{}", line)?;
            }
            StorageBackend::Json => {
                // JSON 模式: 读取现有条目, 追加新条目, 写回
                let mut entries = self.read_all().unwrap_or_default();
                entries.push(entry.clone());
                let json = serde_json::to_string_pretty(&entries)?;
                if let Some(parent) = self.trace_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&self.trace_path, json)?;
            }
            // Sqlite/Postgres 已在构造时回退到 Jsonl, 不会到达这里
            StorageBackend::Sqlite | StorageBackend::Postgres => {
                let line = entry.to_jsonl()?;
                let mut file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.trace_path)?;
                writeln!(file, "{}", line)?;
            }
        }
        Ok(())
    }

    /// 便捷方法: 创建并写入一条 trace 条目
    #[allow(clippy::too_many_arguments)]
    pub fn trace(
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
    ) -> Result<()> {
        let entry = DevTraceEntry::new(
            action,
            phase_idx,
            task_idx,
            task_name,
            input,
            output,
            duration_ms,
            success,
            error,
        );
        self.write_entry(&entry)
    }

    /// 读取所有 trace 条目
    ///
    /// 根据后端类型选择读取方式:
    /// - `Jsonl`: 逐行读取 JSONL 文件并反序列化
    /// - `Json`: 读取整个 JSON 数组并反序列化
    ///
    /// 空行和格式错误的行会被跳过 (不中断读取)。
    /// 文件不存在时返回空 Vec。
    pub fn read_all(&self) -> Result<Vec<DevTraceEntry>> {
        if !self.trace_path.exists() {
            return Ok(vec![]);
        }

        match self.backend {
            StorageBackend::Json => {
                // JSON 模式: 整体读取为 JSON 数组
                let content = std::fs::read_to_string(&self.trace_path)?;
                let entries: Vec<DevTraceEntry> =
                    serde_json::from_str(&content).unwrap_or_default();
                Ok(entries)
            }
            _ => {
                // JSONL 模式: 逐行读取
                let file = std::fs::File::open(&self.trace_path)?;
                let reader = BufReader::new(file);
                let mut entries = Vec::new();

                for (line_num, line) in reader.lines().enumerate() {
                    let line = line?;
                    match parse_jsonl_line(&line) {
                        Some(entry) => entries.push(entry),
                        None => {
                            let trimmed = line.trim();
                            if !trimmed.is_empty() {
                                let preview: String = trimmed.chars().take(100).collect();
                                warn!("DevTrace: 跳过格式错误的行 {}: {}", line_num + 1, preview);
                            }
                        }
                    }
                }

                Ok(entries)
            }
        }
    }

    /// 生成追踪摘要
    ///
    /// 读取所有条目并计算统计信息 (总条目数、总耗时、按操作类型统计、
    /// 成功率、时间线)。
    /// 文件不存在或为空时返回空摘要。
    pub fn summary(&self) -> DevTraceSummary {
        match self.read_all() {
            Ok(entries) => DevTraceSummary::from_entries(&entries),
            Err(e) => {
                warn!("DevTrace: 读取 trace 文件失败: {}", e);
                DevTraceSummary::empty()
            }
        }
    }

    /// 清空 trace 文件 (重新开始时调用)
    ///
    /// JSON 模式下写入空数组 `[]`, JSONL 模式下写入空字符串。
    pub fn clear(&self) -> Result<()> {
        match self.backend {
            StorageBackend::Json => {
                std::fs::write(&self.trace_path, "[]")?;
            }
            _ => {
                std::fs::write(&self.trace_path, "")?;
            }
        }
        Ok(())
    }

    /// 获取当前条目数
    pub fn entry_count(&self) -> usize {
        self.read_all().map(|entries| entries.len()).unwrap_or(0)
    }
}

// ============================================================================
//  纯逻辑函数 — JSONL 行解析
// ============================================================================

/// 解析单行 JSONL 为 `DevTraceEntry`。
///
/// 空行或格式错误的行返回 `None`。
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::{parse_jsonl_line, TraceAction};
/// // 空行返回 None
/// assert!(parse_jsonl_line("").is_none());
/// assert!(parse_jsonl_line("   ").is_none());
///
/// // 格式错误的 JSON 返回 None
/// assert!(parse_jsonl_line("not json").is_none());
///
/// // 有效 JSON 返回 Some
/// let json = r#"{"timestamp":"2024-01-01T00:00:00Z","action":"Planning","input_summary":"in","output_summary":"out","duration_ms":100,"success":true}"#;
/// let entry = parse_jsonl_line(json).unwrap();
/// assert_eq!(entry.action, TraceAction::Planning);
/// ```
pub fn parse_jsonl_line(line: &str) -> Option<DevTraceEntry> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    DevTraceEntry::from_jsonl(trimmed).ok()
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // ===== 辅助函数 =====

    /// 创建临时 DevTraceWriter
    fn make_writer() -> (tempfile::TempDir, DevTraceWriter) {
        let dir = tempdir().unwrap();
        // 创建 .forge 目录
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
        let writer = DevTraceWriter::new(dir.path());
        (dir, writer)
    }

    /// 创建一个简单的 trace 条目
    fn make_entry(action: TraceAction, success: bool) -> DevTraceEntry {
        DevTraceEntry::new(
            action,
            Some(0),
            Some(0),
            Some("测试任务"),
            "输入内容",
            "输出内容",
            1000,
            success,
            if success { None } else { Some("测试错误") },
        )
    }

    // ===== truncate_str =====

    #[test]
    fn test_truncate_str_short() {
        let result = truncate_str("hello", 200);
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_truncate_str_exact_200() {
        let input: String = "x".repeat(200);
        let result = truncate_str(&input, 200);
        assert_eq!(result.len(), 200);
    }

    #[test]
    fn test_truncate_str_long() {
        let input: String = "x".repeat(300);
        let result = truncate_str(&input, 200);
        assert_eq!(result.chars().count(), 200);
    }

    #[test]
    fn test_truncate_str_empty() {
        let result = truncate_str("", 200);
        assert_eq!(result, "");
    }

    #[test]
    fn test_truncate_str_unicode() {
        let input = "你好世界".repeat(100); // 500 chars
        let result = truncate_str(&input, 200);
        assert_eq!(result.chars().count(), 200);
    }

    // ===== TraceAction =====

    #[test]
    fn test_trace_action_display() {
        assert_eq!(TraceAction::Planning.to_string(), "Planning");
        assert_eq!(TraceAction::TaskExecution.to_string(), "TaskExecution");
        assert_eq!(TraceAction::FixAttempt.to_string(), "FixAttempt");
        assert_eq!(TraceAction::Clarification.to_string(), "Clarification");
        assert_eq!(TraceAction::ContextHandoff.to_string(), "ContextHandoff");
        assert_eq!(TraceAction::SteerReminder.to_string(), "SteerReminder");
        assert_eq!(TraceAction::LoopDetection.to_string(), "LoopDetection");
        assert_eq!(TraceAction::CompileCheck.to_string(), "CompileCheck");
        assert_eq!(TraceAction::TestRun.to_string(), "TestRun");
        assert_eq!(TraceAction::E2ETest.to_string(), "E2ETest");
        assert_eq!(
            TraceAction::RequirementChange.to_string(),
            "RequirementChange"
        );
        assert_eq!(TraceAction::SlashCommand.to_string(), "SlashCommand");
        assert_eq!(TraceAction::Recovery.to_string(), "Recovery");
        assert_eq!(TraceAction::HealthCheck.to_string(), "HealthCheck");
        assert_eq!(TraceAction::SiteFailover.to_string(), "SiteFailover");
        assert_eq!(
            TraceAction::PerformanceStats.to_string(),
            "PerformanceStats"
        );
    }

    #[test]
    fn test_trace_action_description() {
        assert_eq!(TraceAction::Planning.description(), "阶段规划");
        assert_eq!(TraceAction::TaskExecution.description(), "任务执行");
        assert_eq!(TraceAction::FixAttempt.description(), "修复尝试");
        assert_eq!(TraceAction::Clarification.description(), "自主追问");
        assert_eq!(TraceAction::ContextHandoff.description(), "上下文衔接");
        assert_eq!(TraceAction::SteerReminder.description(), "转向提醒");
        assert_eq!(TraceAction::LoopDetection.description(), "循环终止检测");
        assert_eq!(TraceAction::CompileCheck.description(), "编译检查");
        assert_eq!(TraceAction::TestRun.description(), "测试运行");
        assert_eq!(TraceAction::E2ETest.description(), "E2E 测试");
        assert_eq!(TraceAction::RequirementChange.description(), "需求变更");
        assert_eq!(TraceAction::SlashCommand.description(), "AI 自主指令");
        assert_eq!(TraceAction::Recovery.description(), "自动恢复");
        assert_eq!(TraceAction::HealthCheck.description(), "健康检查");
        assert_eq!(TraceAction::SiteFailover.description(), "网站切换");
        assert_eq!(TraceAction::PerformanceStats.description(), "性能统计");
    }

    #[test]
    fn test_trace_action_all() {
        let all = TraceAction::all();
        assert_eq!(all.len(), 22);
        assert!(all.contains(&TraceAction::Planning));
        assert!(all.contains(&TraceAction::TaskExecution));
        assert!(all.contains(&TraceAction::FixAttempt));
        assert!(all.contains(&TraceAction::Clarification));
        assert!(all.contains(&TraceAction::ContextHandoff));
        assert!(all.contains(&TraceAction::SteerReminder));
        assert!(all.contains(&TraceAction::LoopDetection));
        assert!(all.contains(&TraceAction::CompileCheck));
        assert!(all.contains(&TraceAction::TestRun));
        assert!(all.contains(&TraceAction::E2ETest));
        assert!(all.contains(&TraceAction::RequirementChange));
        assert!(all.contains(&TraceAction::SlashCommand));
        assert!(all.contains(&TraceAction::Recovery));
        assert!(all.contains(&TraceAction::HealthCheck));
        assert!(all.contains(&TraceAction::SiteFailover));
        assert!(all.contains(&TraceAction::PerformanceStats));
    }

    #[test]
    fn test_trace_action_serde() {
        let action = TraceAction::TaskExecution;
        let json = serde_json::to_string(&action).unwrap();
        assert_eq!(json, "\"TaskExecution\"");

        let parsed: TraceAction = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, action);
    }

    #[test]
    fn test_trace_action_hash_eq() {
        let mut map: HashMap<TraceAction, usize> = HashMap::new();
        map.insert(TraceAction::Planning, 1);
        map.insert(TraceAction::Planning, 2);
        assert_eq!(map.get(&TraceAction::Planning), Some(&2));
        assert_eq!(map.len(), 1);
    }

    // ===== DevTraceEntry =====

    #[test]
    fn test_entry_new_basic() {
        let entry = DevTraceEntry::new(
            TraceAction::TaskExecution,
            Some(0),
            Some(1),
            Some("初始化项目"),
            "请创建项目",
            "已创建项目",
            5000,
            true,
            None,
        );

        assert_eq!(entry.phase_idx, Some(0));
        assert_eq!(entry.task_idx, Some(1));
        assert_eq!(entry.task_name, Some("初始化项目".to_string()));
        assert_eq!(entry.action, TraceAction::TaskExecution);
        assert_eq!(entry.input_summary, "请创建项目");
        assert_eq!(entry.output_summary, "已创建项目");
        assert_eq!(entry.duration_ms, 5000);
        assert!(entry.success);
        assert!(entry.error.is_none());
    }

    #[test]
    fn test_entry_new_truncates_input() {
        let long_input: String = "x".repeat(500);
        let entry = DevTraceEntry::new(
            TraceAction::Planning,
            None,
            None,
            None,
            &long_input,
            "output",
            100,
            true,
            None,
        );
        assert_eq!(entry.input_summary.chars().count(), 200);
    }

    #[test]
    fn test_entry_new_truncates_output() {
        let long_output: String = "y".repeat(500);
        let entry = DevTraceEntry::new(
            TraceAction::Planning,
            None,
            None,
            None,
            "input",
            &long_output,
            100,
            true,
            None,
        );
        assert_eq!(entry.output_summary.chars().count(), 200);
    }

    #[test]
    fn test_entry_new_with_error() {
        let entry = DevTraceEntry::new(
            TraceAction::CompileCheck,
            Some(0),
            Some(0),
            None,
            "check",
            "failed",
            200,
            false,
            Some("E0308: mismatched types"),
        );
        assert!(!entry.success);
        assert_eq!(entry.error, Some("E0308: mismatched types".to_string()));
    }

    #[test]
    fn test_entry_new_none_fields() {
        let entry = DevTraceEntry::new(
            TraceAction::Planning,
            None,
            None,
            None,
            "input",
            "output",
            100,
            true,
            None,
        );
        assert!(entry.phase_idx.is_none());
        assert!(entry.task_idx.is_none());
        assert!(entry.task_name.is_none());
        assert!(entry.error.is_none());
    }

    #[test]
    fn test_entry_to_jsonl() {
        let entry = make_entry(TraceAction::TaskExecution, true);
        let json = entry.to_jsonl().unwrap();
        assert!(json.contains("\"action\":\"TaskExecution\""));
        assert!(json.contains("\"success\":true"));
        assert!(json.contains("\"duration_ms\":1000"));
        assert!(!json.contains('\n')); // 单行
    }

    #[test]
    fn test_entry_from_jsonl() {
        let entry = make_entry(TraceAction::CompileCheck, false);
        let json = entry.to_jsonl().unwrap();
        let parsed = DevTraceEntry::from_jsonl(&json).unwrap();
        assert_eq!(parsed.action, TraceAction::CompileCheck);
        assert!(!parsed.success);
        assert_eq!(parsed.duration_ms, 1000);
        assert_eq!(parsed.error, Some("测试错误".to_string()));
    }

    #[test]
    fn test_entry_jsonl_roundtrip() {
        let entry = DevTraceEntry::new(
            TraceAction::Planning,
            Some(2),
            Some(1),
            Some("测试任务"),
            "输入",
            "输出",
            3000,
            true,
            None,
        );
        let json = entry.to_jsonl().unwrap();
        let parsed = DevTraceEntry::from_jsonl(&json).unwrap();
        assert_eq!(parsed.action, entry.action);
        assert_eq!(parsed.phase_idx, entry.phase_idx);
        assert_eq!(parsed.task_idx, entry.task_idx);
        assert_eq!(parsed.task_name, entry.task_name);
        assert_eq!(parsed.input_summary, entry.input_summary);
        assert_eq!(parsed.output_summary, entry.output_summary);
        assert_eq!(parsed.duration_ms, entry.duration_ms);
        assert_eq!(parsed.success, entry.success);
        assert_eq!(parsed.error, entry.error);
    }

    #[test]
    fn test_entry_jsonl_skip_none_fields() {
        let entry = DevTraceEntry::new(
            TraceAction::Planning,
            None,
            None,
            None,
            "input",
            "output",
            100,
            true,
            None,
        );
        let json = entry.to_jsonl().unwrap();
        assert!(!json.contains("phase_idx"));
        assert!(!json.contains("task_idx"));
        assert!(!json.contains("task_name"));
        assert!(!json.contains("error"));
    }

    #[test]
    fn test_entry_jsonl_includes_none_fields_when_present() {
        let entry = DevTraceEntry::new(
            TraceAction::TaskExecution,
            Some(0),
            Some(0),
            Some("task"),
            "input",
            "output",
            100,
            false,
            Some("error"),
        );
        let json = entry.to_jsonl().unwrap();
        assert!(json.contains("phase_idx"));
        assert!(json.contains("task_idx"));
        assert!(json.contains("task_name"));
        assert!(json.contains("error"));
    }

    // ===== ActionStats =====

    #[test]
    fn test_action_stats_new() {
        let stats = ActionStats::new();
        assert_eq!(stats.count, 0);
        assert_eq!(stats.success_count, 0);
        assert_eq!(stats.total_duration_ms, 0);
    }

    #[test]
    fn test_action_stats_record_success() {
        let mut stats = ActionStats::new();
        stats.record(1000, true);
        assert_eq!(stats.count, 1);
        assert_eq!(stats.success_count, 1);
        assert_eq!(stats.total_duration_ms, 1000);
    }

    #[test]
    fn test_action_stats_record_failure() {
        let mut stats = ActionStats::new();
        stats.record(500, false);
        assert_eq!(stats.count, 1);
        assert_eq!(stats.success_count, 0);
        assert_eq!(stats.total_duration_ms, 500);
    }

    #[test]
    fn test_action_stats_record_multiple() {
        let mut stats = ActionStats::new();
        stats.record(1000, true);
        stats.record(2000, true);
        stats.record(3000, false);
        assert_eq!(stats.count, 3);
        assert_eq!(stats.success_count, 2);
        assert_eq!(stats.total_duration_ms, 6000);
    }

    #[test]
    fn test_action_stats_success_rate() {
        let mut stats = ActionStats::new();
        assert_eq!(stats.success_rate(), 0.0);

        stats.record(100, true);
        stats.record(200, true);
        stats.record(300, false);
        assert!((stats.success_rate() - 2.0 / 3.0).abs() < 0.001);
    }

    #[test]
    fn test_action_stats_avg_duration() {
        let mut stats = ActionStats::new();
        assert_eq!(stats.avg_duration_ms(), 0);

        stats.record(1000, true);
        stats.record(2000, true);
        stats.record(3000, false);
        assert_eq!(stats.avg_duration_ms(), 2000); // 6000 / 3
    }

    #[test]
    fn test_action_stats_default() {
        let stats = ActionStats::default();
        assert_eq!(stats.count, 0);
    }

    // ===== TimelineEntry =====

    #[test]
    fn test_timeline_entry_from_entry() {
        let entry = make_entry(TraceAction::TaskExecution, true);
        let timeline = TimelineEntry::from_entry(&entry);
        assert_eq!(timeline.action, TraceAction::TaskExecution);
        assert_eq!(timeline.task_name, Some("测试任务".to_string()));
        assert!(timeline.success);
        assert_eq!(timeline.duration_ms, 1000);
    }

    #[test]
    fn test_timeline_entry_from_entry_no_task_name() {
        let entry = DevTraceEntry::new(
            TraceAction::Planning,
            None,
            None,
            None,
            "input",
            "output",
            100,
            true,
            None,
        );
        let timeline = TimelineEntry::from_entry(&entry);
        assert!(timeline.task_name.is_none());
    }

    // ===== DevTraceSummary =====

    #[test]
    fn test_summary_empty() {
        let summary = DevTraceSummary::empty();
        assert_eq!(summary.total_entries, 0);
        assert_eq!(summary.total_duration_ms, 0);
        assert_eq!(summary.success_rate, 0.0);
        assert!(summary.timeline.is_empty());
        assert!(summary.by_action.is_empty());
    }

    #[test]
    fn test_summary_from_empty_entries() {
        let summary = DevTraceSummary::from_entries(&[]);
        assert_eq!(summary.total_entries, 0);
        assert_eq!(summary.success_rate, 0.0);
    }

    #[test]
    fn test_summary_from_single_entry() {
        let entry = make_entry(TraceAction::TaskExecution, true);
        let summary = DevTraceSummary::from_entries(&[entry]);
        assert_eq!(summary.total_entries, 1);
        assert_eq!(summary.total_duration_ms, 1000);
        assert!((summary.success_rate - 1.0).abs() < 0.001);
        assert_eq!(summary.timeline.len(), 1);
    }

    #[test]
    fn test_summary_from_multiple_entries() {
        let entries = vec![
            make_entry(TraceAction::TaskExecution, true),
            make_entry(TraceAction::FixAttempt, false),
            make_entry(TraceAction::CompileCheck, false),
            make_entry(TraceAction::TestRun, true),
        ];
        let summary = DevTraceSummary::from_entries(&entries);
        assert_eq!(summary.total_entries, 4);
        assert_eq!(summary.total_duration_ms, 4000);
        assert!((summary.success_rate - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_summary_by_action() {
        let entries = vec![
            make_entry(TraceAction::TaskExecution, true),
            make_entry(TraceAction::TaskExecution, true),
            make_entry(TraceAction::TaskExecution, false),
            make_entry(TraceAction::CompileCheck, true),
        ];
        let summary = DevTraceSummary::from_entries(&entries);

        let task_stats = summary
            .get_action_stats(TraceAction::TaskExecution)
            .unwrap();
        assert_eq!(task_stats.count, 3);
        assert_eq!(task_stats.success_count, 2);

        let check_stats = summary.get_action_stats(TraceAction::CompileCheck).unwrap();
        assert_eq!(check_stats.count, 1);
        assert_eq!(check_stats.success_count, 1);

        assert!(summary.get_action_stats(TraceAction::E2ETest).is_none());
    }

    #[test]
    fn test_summary_timeline_limit_100() {
        let entries: Vec<DevTraceEntry> = (0..150)
            .map(|i| {
                DevTraceEntry::new(
                    TraceAction::TaskExecution,
                    Some(0),
                    Some(i),
                    Some(&format!("任务{}", i)),
                    "input",
                    "output",
                    100 * i as u64,
                    i % 2 == 0,
                    None,
                )
            })
            .collect();

        let summary = DevTraceSummary::from_entries(&entries);
        assert_eq!(summary.total_entries, 150);
        assert_eq!(summary.timeline.len(), 100); // 限制为最近 100 条
    }

    #[test]
    fn test_summary_timeline_under_100() {
        let entries: Vec<DevTraceEntry> = (0..50)
            .map(|_| make_entry(TraceAction::TaskExecution, true))
            .collect();

        let summary = DevTraceSummary::from_entries(&entries);
        assert_eq!(summary.timeline.len(), 50);
    }

    #[test]
    fn test_summary_success_rate_all_success() {
        let entries = vec![
            make_entry(TraceAction::TaskExecution, true),
            make_entry(TraceAction::TestRun, true),
        ];
        let summary = DevTraceSummary::from_entries(&entries);
        assert!((summary.success_rate - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_summary_success_rate_all_failure() {
        let entries = vec![
            make_entry(TraceAction::TaskExecution, false),
            make_entry(TraceAction::TestRun, false),
        ];
        let summary = DevTraceSummary::from_entries(&entries);
        assert!((summary.success_rate - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_summary_to_report() {
        let entries = vec![
            make_entry(TraceAction::TaskExecution, true),
            make_entry(TraceAction::FixAttempt, false),
        ];
        let summary = DevTraceSummary::from_entries(&entries);
        let report = summary.to_report();

        assert!(report.contains("DevTrace 开发追踪报告"));
        assert!(report.contains("总条目: 2"));
        assert!(report.contains("按操作类型统计"));
        assert!(report.contains("任务执行"));
        assert!(report.contains("修复尝试"));
        assert!(report.contains("时间线"));
    }

    #[test]
    fn test_summary_to_report_empty() {
        let summary = DevTraceSummary::empty();
        let report = summary.to_report();

        assert!(report.contains("DevTrace 开发追踪报告"));
        assert!(report.contains("总条目: 0"));
    }

    // ===== DevTraceWriter =====

    #[test]
    fn test_writer_new() {
        let dir = tempdir().unwrap();
        let writer = DevTraceWriter::new(dir.path());
        assert!(writer.trace_path.ends_with(".forge/devtrace.jsonl"));
    }

    #[test]
    fn test_writer_write_and_read_single() {
        let (_dir, writer) = make_writer();
        let entry = make_entry(TraceAction::TaskExecution, true);
        writer.write_entry(&entry).unwrap();

        let entries = writer.read_all().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, TraceAction::TaskExecution);
        assert!(entries[0].success);
    }

    #[test]
    fn test_writer_write_multiple() {
        let (_dir, writer) = make_writer();

        for i in 0..10 {
            let entry = DevTraceEntry::new(
                TraceAction::TaskExecution,
                Some(0),
                Some(i),
                Some(&format!("任务{}", i)),
                "input",
                "output",
                1000 * (i + 1) as u64,
                true,
                None,
            );
            writer.write_entry(&entry).unwrap();
        }

        let entries = writer.read_all().unwrap();
        assert_eq!(entries.len(), 10);
        assert_eq!(entries[0].task_idx, Some(0));
        assert_eq!(entries[9].task_idx, Some(9));
    }

    #[test]
    fn test_writer_write_appends() {
        let (_dir, writer) = make_writer();

        writer
            .write_entry(&make_entry(TraceAction::Planning, true))
            .unwrap();
        writer
            .write_entry(&make_entry(TraceAction::TaskExecution, true))
            .unwrap();
        writer
            .write_entry(&make_entry(TraceAction::CompileCheck, false))
            .unwrap();

        let entries = writer.read_all().unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].action, TraceAction::Planning);
        assert_eq!(entries[1].action, TraceAction::TaskExecution);
        assert_eq!(entries[2].action, TraceAction::CompileCheck);
    }

    #[test]
    fn test_writer_read_empty_file() {
        let (_dir, writer) = make_writer();
        // 文件不存在
        let entries = writer.read_all().unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_writer_read_empty_lines() {
        let (_dir, writer) = make_writer();

        // 写入一条有效条目
        writer
            .write_entry(&make_entry(TraceAction::Planning, true))
            .unwrap();

        // 手动追加空行
        std::fs::OpenOptions::new()
            .append(true)
            .open(&writer.trace_path)
            .unwrap()
            .write_all(b"\n\n\n")
            .unwrap();

        let entries = writer.read_all().unwrap();
        assert_eq!(entries.len(), 1); // 空行被跳过
    }

    #[test]
    fn test_writer_read_malformed_lines() {
        let (_dir, writer) = make_writer();

        // 写入一条有效条目
        writer
            .write_entry(&make_entry(TraceAction::Planning, true))
            .unwrap();

        // 手动追加格式错误的行
        std::fs::OpenOptions::new()
            .append(true)
            .open(&writer.trace_path)
            .unwrap()
            .write_all(b"this is not json\n")
            .unwrap();

        // 再写一条有效条目
        writer
            .write_entry(&make_entry(TraceAction::TaskExecution, true))
            .unwrap();

        let entries = writer.read_all().unwrap();
        assert_eq!(entries.len(), 2); // 格式错误的行被跳过
    }

    #[test]
    fn test_writer_summary() {
        let (_dir, writer) = make_writer();

        writer
            .write_entry(&make_entry(TraceAction::TaskExecution, true))
            .unwrap();
        writer
            .write_entry(&make_entry(TraceAction::TaskExecution, true))
            .unwrap();
        writer
            .write_entry(&make_entry(TraceAction::FixAttempt, false))
            .unwrap();

        let summary = writer.summary();
        assert_eq!(summary.total_entries, 3);
        assert_eq!(summary.total_duration_ms, 3000);
        assert!((summary.success_rate - 2.0 / 3.0).abs() < 0.001);

        let task_stats = summary
            .get_action_stats(TraceAction::TaskExecution)
            .unwrap();
        assert_eq!(task_stats.count, 2);
        assert_eq!(task_stats.success_count, 2);

        let fix_stats = summary.get_action_stats(TraceAction::FixAttempt).unwrap();
        assert_eq!(fix_stats.count, 1);
        assert_eq!(fix_stats.success_count, 0);
    }

    #[test]
    fn test_writer_summary_empty() {
        let (_dir, writer) = make_writer();
        let summary = writer.summary();
        assert_eq!(summary.total_entries, 0);
    }

    #[test]
    fn test_writer_clear() {
        let (_dir, writer) = make_writer();

        writer
            .write_entry(&make_entry(TraceAction::Planning, true))
            .unwrap();
        writer
            .write_entry(&make_entry(TraceAction::TaskExecution, true))
            .unwrap();
        assert_eq!(writer.entry_count(), 2);

        writer.clear().unwrap();
        assert_eq!(writer.entry_count(), 0);
    }

    #[test]
    fn test_writer_entry_count() {
        let (_dir, writer) = make_writer();
        assert_eq!(writer.entry_count(), 0);

        writer
            .write_entry(&make_entry(TraceAction::Planning, true))
            .unwrap();
        assert_eq!(writer.entry_count(), 1);

        writer
            .write_entry(&make_entry(TraceAction::TaskExecution, true))
            .unwrap();
        assert_eq!(writer.entry_count(), 2);
    }

    #[test]
    fn test_writer_trace_helper() {
        let (_dir, writer) = make_writer();
        writer
            .trace(
                TraceAction::CompileCheck,
                Some(0),
                Some(0),
                Some("测试任务"),
                "cargo check",
                "compilation succeeded",
                500,
                true,
                None,
            )
            .unwrap();

        let entries = writer.read_all().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, TraceAction::CompileCheck);
        assert_eq!(entries[0].duration_ms, 500);
        assert!(entries[0].success);
    }

    #[test]
    fn test_writer_trace_helper_with_error() {
        let (_dir, writer) = make_writer();
        writer
            .trace(
                TraceAction::CompileCheck,
                Some(0),
                Some(0),
                None,
                "cargo check",
                "compilation failed",
                500,
                false,
                Some("E0308: type mismatch"),
            )
            .unwrap();

        let entries = writer.read_all().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].success);
        assert_eq!(entries[0].error, Some("E0308: type mismatch".to_string()));
    }

    #[test]
    fn test_writer_creates_file_on_write() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
        let writer = DevTraceWriter::new(dir.path());

        // 文件不存在
        assert!(!writer.trace_path.exists());

        writer
            .write_entry(&make_entry(TraceAction::Planning, true))
            .unwrap();

        // 文件已创建
        assert!(writer.trace_path.exists());
    }

    #[test]
    fn test_writer_all_action_types() {
        let (_dir, writer) = make_writer();

        for action in TraceAction::all() {
            writer
                .trace(action, None, None, None, "input", "output", 100, true, None)
                .unwrap();
        }

        let entries = writer.read_all().unwrap();
        assert_eq!(entries.len(), 22); // 所有 22 种操作类型

        let summary = writer.summary();
        for action in TraceAction::all() {
            assert!(summary.by_action.contains_key(&action));
        }
    }

    #[test]
    fn test_writer_unicode_content() {
        let (_dir, writer) = make_writer();
        writer
            .trace(
                TraceAction::TaskExecution,
                Some(0),
                Some(0),
                Some("初始化项目结构"),
                "请创建一个 Hello World 程序",
                "已创建 src/main.rs 文件",
                3000,
                true,
                None,
            )
            .unwrap();

        let entries = writer.read_all().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].task_name, Some("初始化项目结构".to_string()));
        assert!(entries[0].input_summary.contains("Hello World"));
        assert!(entries[0].output_summary.contains("main.rs"));
    }

    #[test]
    fn test_writer_large_volume() {
        let (_dir, writer) = make_writer();

        // 写入 1000 条
        for i in 0..1000 {
            writer
                .trace(
                    TraceAction::TaskExecution,
                    Some(i / 100),
                    Some(i % 100),
                    Some(&format!("任务{}", i)),
                    &format!("输入{}", i),
                    &format!("输出{}", i),
                    i as u64,
                    i % 3 != 0,
                    if i % 3 == 0 { Some("失败") } else { None },
                )
                .unwrap();
        }

        let entries = writer.read_all().unwrap();
        assert_eq!(entries.len(), 1000);

        let summary = writer.summary();
        assert_eq!(summary.total_entries, 1000);
        assert!(summary.timeline.len() <= 100);
    }

    #[test]
    fn test_summary_to_report_with_all_actions() {
        let (_dir, writer) = make_writer();

        for action in TraceAction::all() {
            writer
                .trace(
                    action,
                    Some(0),
                    Some(0),
                    Some("任务"),
                    "input",
                    "output",
                    100,
                    true,
                    None,
                )
                .unwrap();
        }

        let summary = writer.summary();
        let report = summary.to_report();

        for action in TraceAction::all() {
            assert!(
                report.contains(action.description()),
                "报告应包含操作类型: {}",
                action.description()
            );
        }
    }

    #[test]
    fn test_entry_timestamp_is_recent() {
        let before = Utc::now();
        let entry = DevTraceEntry::new(
            TraceAction::Planning,
            None,
            None,
            None,
            "input",
            "output",
            100,
            true,
            None,
        );
        let after = Utc::now();

        assert!(entry.timestamp >= before);
        assert!(entry.timestamp <= after);
    }

    #[test]
    fn test_writer_read_after_clear_and_rewrite() {
        let (_dir, writer) = make_writer();

        writer
            .write_entry(&make_entry(TraceAction::Planning, true))
            .unwrap();
        writer.clear().unwrap();
        writer
            .write_entry(&make_entry(TraceAction::TaskExecution, true))
            .unwrap();

        let entries = writer.read_all().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, TraceAction::TaskExecution);
    }

    // ===== 集成场景测试 =====

    #[test]
    fn test_scenario_full_development_cycle() {
        let (_dir, writer) = make_writer();

        // 模拟一次完整的开发周期
        // 1. Planning
        writer
            .trace(
                TraceAction::Planning,
                None,
                None,
                None,
                "拆解目标",
                "3个阶段5个任务",
                5000,
                true,
                None,
            )
            .unwrap();

        // 2. Task 1: 执行 → 编译成功 → 测试成功
        writer
            .trace(
                TraceAction::TaskExecution,
                Some(0),
                Some(0),
                Some("初始化"),
                "创建项目",
                "Cargo.toml + main.rs",
                3000,
                true,
                None,
            )
            .unwrap();
        writer
            .trace(
                TraceAction::CompileCheck,
                Some(0),
                Some(0),
                Some("初始化"),
                "cargo check",
                "成功",
                500,
                true,
                None,
            )
            .unwrap();
        writer
            .trace(
                TraceAction::TestRun,
                Some(0),
                Some(0),
                Some("初始化"),
                "cargo test",
                "3 passed",
                1000,
                true,
                None,
            )
            .unwrap();

        // 3. Task 2: 执行 → 编译失败 → 修复 → 编译成功 → 测试成功
        writer
            .trace(
                TraceAction::TaskExecution,
                Some(0),
                Some(1),
                Some("功能实现"),
                "实现功能",
                "代码",
                3000,
                true,
                None,
            )
            .unwrap();
        writer
            .trace(
                TraceAction::CompileCheck,
                Some(0),
                Some(1),
                Some("功能实现"),
                "cargo check",
                "E0308错误",
                500,
                false,
                Some("类型不匹配"),
            )
            .unwrap();
        writer
            .trace(
                TraceAction::FixAttempt,
                Some(0),
                Some(1),
                Some("功能实现"),
                "修复类型",
                "修复后代码",
                2500,
                true,
                None,
            )
            .unwrap();
        writer
            .trace(
                TraceAction::CompileCheck,
                Some(0),
                Some(1),
                Some("功能实现"),
                "cargo check",
                "成功",
                400,
                true,
                None,
            )
            .unwrap();
        writer
            .trace(
                TraceAction::TestRun,
                Some(0),
                Some(1),
                Some("功能实现"),
                "cargo test",
                "5 passed",
                1200,
                true,
                None,
            )
            .unwrap();

        // 4. 自主追问
        writer
            .trace(
                TraceAction::Clarification,
                Some(0),
                Some(1),
                Some("功能实现"),
                "追问类型",
                "AI补充了类型信息",
                2000,
                true,
                None,
            )
            .unwrap();

        // 验证
        let summary = writer.summary();
        assert_eq!(summary.total_entries, 10);
        assert!((summary.success_rate - 9.0 / 10.0).abs() < 0.001);

        let check_stats = summary.get_action_stats(TraceAction::CompileCheck).unwrap();
        assert_eq!(check_stats.count, 3);
        assert_eq!(check_stats.success_count, 2);

        let task_stats = summary
            .get_action_stats(TraceAction::TaskExecution)
            .unwrap();
        assert_eq!(task_stats.count, 2);
        assert_eq!(task_stats.success_count, 2);
    }

    #[test]
    fn test_scenario_24h_simulation() {
        let (_dir, writer) = make_writer();

        // 模拟 24 小时运行: 100 个任务, 每个任务平均 2 次 attempt
        for task_idx in 0..100 {
            let success = task_idx % 5 != 0; // 20% 失败率
            let action = if task_idx % 3 == 0 {
                TraceAction::TaskExecution
            } else {
                TraceAction::FixAttempt
            };

            writer
                .trace(
                    action,
                    Some(task_idx / 10),
                    Some(task_idx % 10),
                    Some(&format!("任务{}", task_idx)),
                    "input",
                    "output",
                    1000 + task_idx as u64 * 10,
                    success,
                    if success { None } else { Some("编译失败") },
                )
                .unwrap();
        }

        let summary = writer.summary();
        assert_eq!(summary.total_entries, 100);
        assert!(summary.timeline.len() <= 100);

        // 验证报告可读
        let report = summary.to_report();
        assert!(report.contains("DevTrace 开发追踪报告"));
        assert!(report.contains("总条目: 100"));
    }

    // ===== DevTraceWriter Clone 测试 =====

    #[test]
    fn test_writer_clone_writes_to_same_file() {
        let (_dir, writer) = make_writer();

        // 克隆 writer
        let cloned = writer.clone();

        // 两个 writer 写入同一文件
        writer
            .trace(
                TraceAction::Planning,
                None,
                None,
                None,
                "原始",
                "输出1",
                100,
                true,
                None,
            )
            .unwrap();
        cloned
            .trace(
                TraceAction::TaskExecution,
                None,
                None,
                None,
                "克隆",
                "输出2",
                200,
                true,
                None,
            )
            .unwrap();

        // 验证两条记录都在同一文件中
        let entries = writer.read_all().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].action, TraceAction::Planning);
        assert_eq!(entries[0].input_summary, "原始");
        assert_eq!(entries[1].action, TraceAction::TaskExecution);
        assert_eq!(entries[1].input_summary, "克隆");
    }

    #[test]
    fn test_writer_clone_independent_summary() {
        let (_dir, writer) = make_writer();
        let cloned = writer.clone();

        // 原始 writer 写入
        writer
            .trace(
                TraceAction::Planning,
                None,
                None,
                None,
                "原始",
                "输出",
                100,
                true,
                None,
            )
            .unwrap();

        // 克隆 writer 读取 (应该看到原始 writer 写入的内容)
        let entries = cloned.read_all().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, TraceAction::Planning);
    }

    #[test]
    fn test_writer_clone_path_matches() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
        let writer = DevTraceWriter::new(dir.path());
        let cloned = writer.clone();
        assert_eq!(writer.trace_path, cloned.trace_path);
    }

    // ===== TraceAction::PerformanceStats 测试 =====

    #[test]
    fn test_trace_action_performance_stats_display() {
        assert_eq!(
            TraceAction::PerformanceStats.to_string(),
            "PerformanceStats"
        );
    }

    #[test]
    fn test_trace_action_performance_stats_description() {
        assert_eq!(TraceAction::PerformanceStats.description(), "性能统计");
    }

    #[test]
    fn test_trace_action_performance_stats_in_all() {
        let all = TraceAction::all();
        assert!(all.contains(&TraceAction::PerformanceStats));
        assert_eq!(all.len(), 22);
    }

    #[test]
    fn test_trace_action_performance_stats_serde() {
        let action = TraceAction::PerformanceStats;
        let json = serde_json::to_string(&action).unwrap();
        assert_eq!(json, "\"PerformanceStats\"");

        let parsed: TraceAction = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, action);
    }

    #[test]
    fn test_writer_performance_stats_trace() {
        let (_dir, writer) = make_writer();
        writer
            .trace(
                TraceAction::PerformanceStats,
                None,
                None,
                None,
                "性能统计 [0] Zai",
                "发送:10 成功:8 失败:2 成功率:80.0%",
                0,
                true,
                None,
            )
            .unwrap();

        let entries = writer.read_all().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, TraceAction::PerformanceStats);
        assert!(entries[0].input_summary.contains("Zai"));
        assert!(entries[0].output_summary.contains("发送:10"));
        assert!(entries[0].output_summary.contains("成功率:80.0%"));
    }

    #[test]
    fn test_writer_performance_stats_in_summary() {
        let (_dir, writer) = make_writer();

        // 写入多种操作类型, 包括 PerformanceStats
        writer
            .trace(
                TraceAction::TaskExecution,
                Some(0),
                Some(0),
                Some("任务"),
                "input",
                "output",
                100,
                true,
                None,
            )
            .unwrap();
        writer
            .trace(
                TraceAction::PerformanceStats,
                None,
                None,
                None,
                "统计 [0]",
                "发送:5",
                0,
                true,
                None,
            )
            .unwrap();

        let summary = writer.summary();
        assert_eq!(summary.total_entries, 2);

        let stats = summary
            .get_action_stats(TraceAction::PerformanceStats)
            .unwrap();
        assert_eq!(stats.count, 1);
        assert_eq!(stats.success_count, 1);
    }

    #[test]
    fn test_writer_performance_stats_in_report() {
        let (_dir, writer) = make_writer();
        writer
            .trace(
                TraceAction::PerformanceStats,
                None,
                None,
                None,
                "统计",
                "发送:5",
                0,
                true,
                None,
            )
            .unwrap();

        let summary = writer.summary();
        let report = summary.to_report();
        assert!(report.contains("性能统计"));
    }

    #[test]
    fn test_writer_all_action_types_includes_performance_stats() {
        let (_dir, writer) = make_writer();

        for action in TraceAction::all() {
            writer
                .trace(action, None, None, None, "input", "output", 100, true, None)
                .unwrap();
        }

        let entries = writer.read_all().unwrap();
        assert_eq!(entries.len(), 22); // 所有 22 种操作类型

        // 确保 PerformanceStats 被包含
        let has_performance_stats = entries
            .iter()
            .any(|e| e.action == TraceAction::PerformanceStats);
        assert!(has_performance_stats);
    }

    #[test]
    fn test_writer_clone_shared_write_for_failover_simulation() {
        // 模拟 FailoverChatClient + Orchestrator 共享同一 DevTraceWriter 的场景:
        // - Orchestrator 通过原始 writer 写入 Planning/TaskExecution 等
        // - FailoverChatClient 通过 cloned writer 写入 HealthCheck/SiteFailover
        // - 两者写入同一文件
        let (_dir, writer) = make_writer();
        let failover_writer = writer.clone();

        // Orchestrator 写入
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

        // FailoverChatClient 写入 (通过克隆的 writer)
        failover_writer
            .trace(
                TraceAction::HealthCheck,
                None,
                None,
                None,
                "检查 [0] Zai",
                "Healthy",
                50,
                true,
                None,
            )
            .unwrap();
        failover_writer
            .trace(
                TraceAction::SiteFailover,
                None,
                None,
                None,
                "切换 [0] Zai → [1] DeepSeek",
                "成功",
                0,
                true,
                None,
            )
            .unwrap();
        failover_writer
            .trace(
                TraceAction::PerformanceStats,
                None,
                None,
                None,
                "统计 [0] Zai",
                "发送:5 成功:4",
                0,
                true,
                None,
            )
            .unwrap();

        // 验证所有条目在同一文件中
        let entries = writer.read_all().unwrap();
        assert_eq!(entries.len(), 5);
        assert_eq!(entries[0].action, TraceAction::Planning);
        assert_eq!(entries[1].action, TraceAction::TaskExecution);
        assert_eq!(entries[2].action, TraceAction::HealthCheck);
        assert_eq!(entries[3].action, TraceAction::SiteFailover);
        assert_eq!(entries[4].action, TraceAction::PerformanceStats);

        // 验证 summary 包含所有类型
        let summary = writer.summary();
        assert!(summary.by_action.contains_key(&TraceAction::HealthCheck));
        assert!(summary.by_action.contains_key(&TraceAction::SiteFailover));
        assert!(summary
            .by_action
            .contains_key(&TraceAction::PerformanceStats));
    }

    // ======================================================================
    //  纯逻辑函数 — calculate_success_rate 边界测试
    // ======================================================================

    #[test]
    fn test_calculate_success_rate_zero_total() {
        assert_eq!(calculate_success_rate(0, 0), 0.0);
    }

    #[test]
    fn test_calculate_success_rate_all_success() {
        assert_eq!(calculate_success_rate(10, 10), 1.0);
    }

    #[test]
    fn test_calculate_success_rate_all_failure() {
        assert_eq!(calculate_success_rate(10, 0), 0.0);
    }

    #[test]
    fn test_calculate_success_rate_half() {
        assert!((calculate_success_rate(10, 5) - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_calculate_success_rate_one_third() {
        assert!((calculate_success_rate(3, 1) - 1.0 / 3.0).abs() < 0.001);
    }

    #[test]
    fn test_calculate_success_rate_large_numbers() {
        let total = 1_000_000;
        let success = 999_999;
        let rate = calculate_success_rate(total, success);
        assert!((rate - 0.999999).abs() < 0.0001);
    }

    #[test]
    fn test_calculate_success_rate_single_success() {
        assert_eq!(calculate_success_rate(1, 1), 1.0);
    }

    #[test]
    fn test_calculate_success_rate_single_failure() {
        assert_eq!(calculate_success_rate(1, 0), 0.0);
    }

    #[test]
    fn test_calculate_success_rate_consistency_with_action_stats() {
        let mut stats = ActionStats::new();
        stats.record(100, true);
        stats.record(200, true);
        stats.record(300, false);

        let rate_from_fn = calculate_success_rate(stats.count, stats.success_count);
        let rate_from_method = stats.success_rate();
        assert!((rate_from_fn - rate_from_method).abs() < 0.0001);
    }

    // ======================================================================
    //  纯逻辑函数 — format_duration_human 边界测试
    // ======================================================================

    #[test]
    fn test_format_duration_human_zero() {
        assert_eq!(format_duration_human(0), "0.0s (0.0m)");
    }

    #[test]
    fn test_format_duration_human_one_second() {
        assert_eq!(format_duration_human(1000), "1.0s (0.0m)");
    }

    #[test]
    fn test_format_duration_human_one_minute() {
        assert_eq!(format_duration_human(60000), "60.0s (1.0m)");
    }

    #[test]
    fn test_format_duration_human_500ms() {
        assert_eq!(format_duration_human(500), "0.5s (0.0m)");
    }

    #[test]
    fn test_format_duration_human_hour() {
        assert_eq!(format_duration_human(3_600_000), "3600.0s (60.0m)");
    }

    #[test]
    fn test_format_duration_human_max() {
        let result = format_duration_human(u64::MAX);
        assert!(result.contains("s"));
        assert!(result.contains("m"));
    }

    // ======================================================================
    //  纯逻辑函数 — format_success_rate_percent 边界测试
    // ======================================================================

    #[test]
    fn test_format_success_rate_percent_zero() {
        assert_eq!(format_success_rate_percent(0.0), "0.0%");
    }

    #[test]
    fn test_format_success_rate_percent_full() {
        assert_eq!(format_success_rate_percent(1.0), "100.0%");
    }

    #[test]
    fn test_format_success_rate_percent_half() {
        assert_eq!(format_success_rate_percent(0.5), "50.0%");
    }

    #[test]
    fn test_format_success_rate_percent_third() {
        assert_eq!(format_success_rate_percent(1.0 / 3.0), "33.3%");
    }

    #[test]
    fn test_format_success_rate_percent_two_thirds() {
        assert_eq!(format_success_rate_percent(2.0 / 3.0), "66.7%");
    }

    #[test]
    fn test_format_success_rate_percent_negative() {
        // 负值虽然是异常输入, 但函数应该仍能格式化
        let result = format_success_rate_percent(-0.5);
        assert!(result.contains("%"));
    }

    // ======================================================================
    //  纯逻辑函数 — format_timeline_line 边界测试
    // ======================================================================

    #[test]
    fn test_format_timeline_line_success() {
        let entry = TimelineEntry {
            timestamp: Utc::now(),
            action: TraceAction::TaskExecution,
            task_name: Some("测试任务".to_string()),
            success: true,
            duration_ms: 3000,
        };
        let line = format_timeline_line(&entry);
        assert!(line.contains("✅"));
        assert!(!line.contains("❌"));
        assert!(line.contains("任务执行"));
        assert!(line.contains("测试任务"));
        assert!(line.contains("3000ms"));
        assert!(line.ends_with('\n'));
    }

    #[test]
    fn test_format_timeline_line_failure() {
        let entry = TimelineEntry {
            timestamp: Utc::now(),
            action: TraceAction::FixAttempt,
            task_name: Some("修复".to_string()),
            success: false,
            duration_ms: 500,
        };
        let line = format_timeline_line(&entry);
        assert!(line.contains("❌"));
        assert!(!line.contains("✅"));
        assert!(line.contains("修复尝试"));
        assert!(line.contains("500ms"));
    }

    #[test]
    fn test_format_timeline_line_no_task_name() {
        let entry = TimelineEntry {
            timestamp: Utc::now(),
            action: TraceAction::Planning,
            task_name: None,
            success: true,
            duration_ms: 100,
        };
        let line = format_timeline_line(&entry);
        assert!(line.contains("-")); // None → "-"
        assert!(line.contains("阶段规划"));
    }

    #[test]
    fn test_format_timeline_line_zero_duration() {
        let entry = TimelineEntry {
            timestamp: Utc::now(),
            action: TraceAction::HealthCheck,
            task_name: None,
            success: true,
            duration_ms: 0,
        };
        let line = format_timeline_line(&entry);
        assert!(line.contains("0ms"));
    }

    #[test]
    fn test_format_timeline_line_all_action_types() {
        for action in TraceAction::all() {
            let entry = TimelineEntry {
                timestamp: Utc::now(),
                action,
                task_name: Some("test".to_string()),
                success: true,
                duration_ms: 100,
            };
            let line = format_timeline_line(&entry);
            assert!(
                line.contains(action.description()),
                "format_timeline_line should contain action description for {:?}",
                action
            );
        }
    }

    // ======================================================================
    //  纯逻辑函数 — build_timeline 边界测试
    // ======================================================================

    #[test]
    fn test_build_timeline_empty() {
        let timeline = build_timeline(&[], 100);
        assert!(timeline.is_empty());
    }

    #[test]
    fn test_build_timeline_fewer_than_max() {
        let entries: Vec<DevTraceEntry> = (0..5)
            .map(|i| {
                DevTraceEntry::new(
                    TraceAction::TaskExecution,
                    Some(0),
                    Some(i),
                    Some("task"),
                    "in",
                    "out",
                    100,
                    true,
                    None,
                )
            })
            .collect();
        let timeline = build_timeline(&entries, 100);
        assert_eq!(timeline.len(), 5);
    }

    #[test]
    fn test_build_timeline_exactly_max() {
        let entries: Vec<DevTraceEntry> = (0..10)
            .map(|i| {
                DevTraceEntry::new(
                    TraceAction::TaskExecution,
                    Some(0),
                    Some(i),
                    Some("task"),
                    "in",
                    "out",
                    100,
                    true,
                    None,
                )
            })
            .collect();
        let timeline = build_timeline(&entries, 10);
        assert_eq!(timeline.len(), 10);
    }

    #[test]
    fn test_build_timeline_more_than_max() {
        let entries: Vec<DevTraceEntry> = (0..20)
            .map(|i| {
                DevTraceEntry::new(
                    TraceAction::TaskExecution,
                    Some(0),
                    Some(i),
                    Some(&format!("task{}", i)),
                    "in",
                    "out",
                    100,
                    true,
                    None,
                )
            })
            .collect();
        let timeline = build_timeline(&entries, 10);
        assert_eq!(timeline.len(), 10);
        // 应该返回最后 10 条
        assert_eq!(timeline[0].task_name, Some("task10".to_string()));
        assert_eq!(timeline[9].task_name, Some("task19".to_string()));
    }

    #[test]
    fn test_build_timeline_max_zero() {
        let entries: Vec<DevTraceEntry> = (0..5)
            .map(|i| {
                DevTraceEntry::new(
                    TraceAction::TaskExecution,
                    Some(0),
                    Some(i),
                    Some("task"),
                    "in",
                    "out",
                    100,
                    true,
                    None,
                )
            })
            .collect();
        let timeline = build_timeline(&entries, 0);
        assert!(timeline.is_empty());
    }

    #[test]
    fn test_build_timeline_single_entry() {
        let entry = DevTraceEntry::new(
            TraceAction::Planning,
            None,
            None,
            None,
            "in",
            "out",
            100,
            true,
            None,
        );
        let timeline = build_timeline(&[entry], 100);
        assert_eq!(timeline.len(), 1);
        assert_eq!(timeline[0].action, TraceAction::Planning);
    }

    #[test]
    fn test_build_timeline_max_one() {
        let entries: Vec<DevTraceEntry> = (0..5)
            .map(|i| {
                DevTraceEntry::new(
                    TraceAction::TaskExecution,
                    Some(0),
                    Some(i),
                    Some(&format!("task{}", i)),
                    "in",
                    "out",
                    100,
                    true,
                    None,
                )
            })
            .collect();
        let timeline = build_timeline(&entries, 1);
        assert_eq!(timeline.len(), 1);
        assert_eq!(timeline[0].task_name, Some("task4".to_string()));
    }

    #[test]
    fn test_build_timeline_consistency_with_summary() {
        let entries: Vec<DevTraceEntry> = (0..150)
            .map(|i| {
                DevTraceEntry::new(
                    TraceAction::TaskExecution,
                    Some(0),
                    Some(i),
                    Some(&format!("task{}", i)),
                    "in",
                    "out",
                    100,
                    i % 2 == 0,
                    None,
                )
            })
            .collect();

        let timeline_fn = build_timeline(&entries, 100);
        let summary = DevTraceSummary::from_entries(&entries);
        assert_eq!(timeline_fn.len(), summary.timeline.len());
        assert_eq!(timeline_fn.len(), 100);
    }

    // ======================================================================
    //  纯逻辑函数 — group_entries_by_action 边界测试
    // ======================================================================

    #[test]
    fn test_group_entries_empty() {
        let grouped = group_entries_by_action(&[]);
        assert!(grouped.is_empty());
    }

    #[test]
    fn test_group_entries_single_action() {
        let entries = vec![DevTraceEntry::new(
            TraceAction::TaskExecution,
            None,
            None,
            None,
            "in",
            "out",
            100,
            true,
            None,
        )];
        let grouped = group_entries_by_action(&entries);
        assert_eq!(grouped.len(), 1);
        let stats = grouped.get(&TraceAction::TaskExecution).unwrap();
        assert_eq!(stats.count, 1);
        assert_eq!(stats.success_count, 1);
    }

    #[test]
    fn test_group_entries_multiple_actions() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::TaskExecution,
                None,
                None,
                None,
                "in",
                "out",
                100,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                None,
                None,
                None,
                "in",
                "out",
                200,
                false,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::TestRun,
                None,
                None,
                None,
                "in",
                "out",
                300,
                true,
                None,
            ),
        ];
        let grouped = group_entries_by_action(&entries);
        assert_eq!(grouped.len(), 3);
        assert!(grouped.contains_key(&TraceAction::TaskExecution));
        assert!(grouped.contains_key(&TraceAction::CompileCheck));
        assert!(grouped.contains_key(&TraceAction::TestRun));
    }

    #[test]
    fn test_group_entries_repeated_action_aggregated() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::TaskExecution,
                None,
                None,
                None,
                "in",
                "out",
                100,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::TaskExecution,
                None,
                None,
                None,
                "in",
                "out",
                200,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::TaskExecution,
                None,
                None,
                None,
                "in",
                "out",
                300,
                false,
                None,
            ),
        ];
        let grouped = group_entries_by_action(&entries);
        assert_eq!(grouped.len(), 1);
        let stats = grouped.get(&TraceAction::TaskExecution).unwrap();
        assert_eq!(stats.count, 3);
        assert_eq!(stats.success_count, 2);
        assert_eq!(stats.total_duration_ms, 600);
    }

    #[test]
    fn test_group_entries_all_action_types() {
        let entries: Vec<DevTraceEntry> = TraceAction::all()
            .iter()
            .map(|action| {
                DevTraceEntry::new(*action, None, None, None, "in", "out", 100, true, None)
            })
            .collect();
        let grouped = group_entries_by_action(&entries);
        assert_eq!(grouped.len(), 22);
        for action in TraceAction::all() {
            assert!(grouped.contains_key(&action));
        }
    }

    #[test]
    fn test_group_entries_consistency_with_summary() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::TaskExecution,
                None,
                None,
                None,
                "in",
                "out",
                100,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::TaskExecution,
                None,
                None,
                None,
                "in",
                "out",
                200,
                false,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                None,
                None,
                None,
                "in",
                "out",
                50,
                true,
                None,
            ),
        ];
        let grouped = group_entries_by_action(&entries);
        let summary = DevTraceSummary::from_entries(&entries);
        assert_eq!(grouped.len(), summary.by_action.len());
        for (action, stats) in &grouped {
            let summary_stats = summary.by_action.get(action).unwrap();
            assert_eq!(stats.count, summary_stats.count);
            assert_eq!(stats.success_count, summary_stats.success_count);
        }
    }

    // ======================================================================
    //  纯逻辑函数 — format_action_stats_line 边界测试
    // ======================================================================

    #[test]
    fn test_format_action_stats_line_zero_count() {
        let stats = ActionStats::new();
        let line = format_action_stats_line(TraceAction::Planning, &stats);
        assert!(line.contains("阶段规划"));
        assert!(line.contains("次数:"));
        assert!(line.contains("成功:"));
        assert!(line.ends_with('\n'));
    }

    #[test]
    fn test_format_action_stats_line_all_success() {
        let mut stats = ActionStats::new();
        stats.record(1000, true);
        stats.record(2000, true);
        let line = format_action_stats_line(TraceAction::TaskExecution, &stats);
        assert!(line.contains("任务执行"));
        assert!(line.contains("100.0%"));
        assert!(line.contains("1500ms")); // (1000+2000)/2 = 1500
                                          // 验证计数和成功数通过格式化后的字段包含
        let count_str = format!("{:4}", 2);
        assert!(line.contains(&format!("次数: {}", count_str)));
        assert!(line.contains(&format!("成功: {}", count_str)));
    }

    #[test]
    fn test_format_action_stats_line_all_failure() {
        let mut stats = ActionStats::new();
        stats.record(500, false);
        stats.record(700, false);
        let line = format_action_stats_line(TraceAction::FixAttempt, &stats);
        assert!(line.contains("修复尝试"));
        assert!(line.contains("0.0%"));
        assert!(line.contains("600ms")); // (500+700)/2 = 600
        let count_str = format!("{:4}", 2);
        let success_str = format!("{:4}", 0);
        assert!(line.contains(&format!("次数: {}", count_str)));
        assert!(line.contains(&format!("成功: {}", success_str)));
    }

    #[test]
    fn test_format_action_stats_line_mixed() {
        let mut stats = ActionStats::new();
        stats.record(1000, true);
        stats.record(2000, false);
        let line = format_action_stats_line(TraceAction::CompileCheck, &stats);
        assert!(line.contains("编译检查"));
        assert!(line.contains("50.0%"));
        assert!(line.contains("1500ms"));
        let count_str = format!("{:4}", 2);
        let success_str = format!("{:4}", 1);
        assert!(line.contains(&format!("次数: {}", count_str)));
        assert!(line.contains(&format!("成功: {}", success_str)));
    }

    #[test]
    fn test_format_action_stats_line_all_action_types() {
        let stats = ActionStats::new();
        for action in TraceAction::all() {
            let line = format_action_stats_line(action, &stats);
            assert!(
                line.contains(action.description()),
                "format_action_stats_line should contain description for {:?}",
                action
            );
        }
    }

    // ======================================================================
    //  纯逻辑函数 — parse_jsonl_line 边界测试
    // ======================================================================

    #[test]
    fn test_parse_jsonl_line_empty() {
        assert!(parse_jsonl_line("").is_none());
    }

    #[test]
    fn test_parse_jsonl_line_whitespace_only() {
        assert!(parse_jsonl_line("   ").is_none());
        assert!(parse_jsonl_line("\t").is_none());
        assert!(parse_jsonl_line(" \n ").is_none());
    }

    #[test]
    fn test_parse_jsonl_line_malformed_json() {
        assert!(parse_jsonl_line("not json").is_none());
        assert!(parse_jsonl_line("{broken").is_none());
        assert!(parse_jsonl_line("12345").is_none());
        assert!(parse_jsonl_line("null").is_none());
        assert!(parse_jsonl_line("[]").is_none());
    }

    #[test]
    fn test_parse_jsonl_line_valid_json() {
        let json = r#"{"timestamp":"2024-01-01T00:00:00Z","action":"Planning","input_summary":"in","output_summary":"out","duration_ms":100,"success":true}"#;
        let entry = parse_jsonl_line(json).unwrap();
        assert_eq!(entry.action, TraceAction::Planning);
        assert_eq!(entry.input_summary, "in");
        assert_eq!(entry.output_summary, "out");
        assert_eq!(entry.duration_ms, 100);
        assert!(entry.success);
    }

    #[test]
    fn test_parse_jsonl_line_with_whitespace_padding() {
        let json = r#"  {"timestamp":"2024-01-01T00:00:00Z","action":"TaskExecution","input_summary":"in","output_summary":"out","duration_ms":200,"success":false}  "#;
        let entry = parse_jsonl_line(json).unwrap();
        assert_eq!(entry.action, TraceAction::TaskExecution);
        assert!(!entry.success);
        assert_eq!(entry.duration_ms, 200);
    }

    #[test]
    fn test_parse_jsonl_line_with_optional_fields() {
        let json = r#"{"timestamp":"2024-01-01T00:00:00Z","action":"TaskExecution","phase_idx":0,"task_idx":1,"task_name":"测试","input_summary":"in","output_summary":"out","duration_ms":500,"success":true,"error":null}"#;
        let entry = parse_jsonl_line(json).unwrap();
        assert_eq!(entry.phase_idx, Some(0));
        assert_eq!(entry.task_idx, Some(1));
        assert_eq!(entry.task_name, Some("测试".to_string()));
    }

    #[test]
    fn test_parse_jsonl_line_unicode_content() {
        let json = r#"{"timestamp":"2024-01-01T00:00:00Z","action":"TaskExecution","input_summary":"请创建一个 Hello World 程序","output_summary":"已创建 src/main.rs","duration_ms":3000,"success":true}"#;
        let entry = parse_jsonl_line(json).unwrap();
        assert!(entry.input_summary.contains("Hello World"));
        assert!(entry.output_summary.contains("main.rs"));
    }

    #[test]
    fn test_parse_jsonl_line_all_action_types() {
        for action in TraceAction::all() {
            let json = format!(
                r#"{{"timestamp":"2024-01-01T00:00:00Z","action":"{}","input_summary":"in","output_summary":"out","duration_ms":100,"success":true}}"#,
                action
            );
            let entry = parse_jsonl_line(&json)
                .unwrap_or_else(|| panic!("parse_jsonl_line failed for action: {}", action));
            assert_eq!(entry.action, action);
        }
    }

    #[test]
    fn test_parse_jsonl_line_consistency_with_read_all() {
        let (_dir, writer) = make_writer();
        // 写入一条有效条目
        writer
            .write_entry(&make_entry(TraceAction::Planning, true))
            .unwrap();
        // 追加一个空行
        std::fs::OpenOptions::new()
            .append(true)
            .open(&writer.trace_path)
            .unwrap()
            .write_all(b"\n")
            .unwrap();
        // 追加一条格式错误的行
        std::fs::OpenOptions::new()
            .append(true)
            .open(&writer.trace_path)
            .unwrap()
            .write_all(b"bad json\n")
            .unwrap();
        // 再写一条有效条目
        writer
            .write_entry(&make_entry(TraceAction::TaskExecution, true))
            .unwrap();

        // read_all 应该和 parse_jsonl_line 一致: 跳过空行和错误行
        let entries = writer.read_all().unwrap();
        assert_eq!(entries.len(), 2);

        // 验证 parse_jsonl_line 对每行的行为一致
        let file = std::fs::File::open(&writer.trace_path).unwrap();
        let reader = BufReader::new(file);
        let mut parsed_count = 0;
        for line in reader.lines() {
            let line = line.unwrap();
            if parse_jsonl_line(&line).is_some() {
                parsed_count += 1;
            }
        }
        assert_eq!(parsed_count, 2);
    }

    // ======================================================================
    //  纯逻辑函数 — to_report 一致性验证
    // ======================================================================

    #[test]
    fn test_to_report_uses_format_duration_human() {
        let entries = vec![DevTraceEntry::new(
            TraceAction::TaskExecution,
            None,
            None,
            None,
            "in",
            "out",
            5000,
            true,
            None,
        )];
        let summary = DevTraceSummary::from_entries(&entries);
        let report = summary.to_report();
        assert!(
            report.contains(&format_duration_human(5000)),
            "to_report should use format_duration_human"
        );
    }

    #[test]
    fn test_to_report_uses_format_success_rate_percent() {
        let entries = vec![DevTraceEntry::new(
            TraceAction::TaskExecution,
            None,
            None,
            None,
            "in",
            "out",
            100,
            true,
            None,
        )];
        let summary = DevTraceSummary::from_entries(&entries);
        let report = summary.to_report();
        assert!(
            report.contains(&format_success_rate_percent(1.0)),
            "to_report should use format_success_rate_percent"
        );
    }

    #[test]
    fn test_to_report_uses_format_action_stats_line() {
        let entries = vec![DevTraceEntry::new(
            TraceAction::TaskExecution,
            None,
            None,
            None,
            "in",
            "out",
            1000,
            true,
            None,
        )];
        let summary = DevTraceSummary::from_entries(&entries);
        let report = summary.to_report();
        let stats = summary
            .get_action_stats(TraceAction::TaskExecution)
            .unwrap();
        let stats_line = format_action_stats_line(TraceAction::TaskExecution, stats);
        assert!(
            report.contains(&stats_line),
            "to_report should use format_action_stats_line"
        );
    }

    #[test]
    fn test_to_report_uses_format_timeline_line() {
        let entries = vec![DevTraceEntry::new(
            TraceAction::Planning,
            None,
            None,
            Some("测试"),
            "in",
            "out",
            100,
            true,
            None,
        )];
        let summary = DevTraceSummary::from_entries(&entries);
        let report = summary.to_report();
        let timeline_line = format_timeline_line(&summary.timeline[0]);
        assert!(
            report.contains(&timeline_line),
            "to_report should use format_timeline_line"
        );
    }

    // ======================================================================
    //  纯逻辑函数 — 大规模集成测试
    // ======================================================================

    #[test]
    fn test_pure_functions_large_scale_24h_simulation() {
        // 模拟 24h 运行: 500 条 trace 条目
        let entries: Vec<DevTraceEntry> = (0..500)
            .map(|i| {
                let action = match i % 5 {
                    0 => TraceAction::TaskExecution,
                    1 => TraceAction::FixAttempt,
                    2 => TraceAction::CompileCheck,
                    3 => TraceAction::TestRun,
                    _ => TraceAction::Clarification,
                };
                DevTraceEntry::new(
                    action,
                    Some(i / 50),
                    Some(i % 50),
                    Some(&format!("任务{}", i)),
                    &format!("输入{}", i),
                    &format!("输出{}", i),
                    (i + 1) as u64 * 100,
                    i % 3 != 0,
                    if i % 3 == 0 { Some("失败") } else { None },
                )
            })
            .collect();

        // 验证所有纯函数协同工作
        let total_duration: u64 = entries.iter().map(|e| e.duration_ms).sum();
        let success_count = entries.iter().filter(|e| e.success).count();
        let rate = calculate_success_rate(entries.len(), success_count);
        let grouped = group_entries_by_action(&entries);
        let timeline = build_timeline(&entries, 100);
        let summary = DevTraceSummary::from_entries(&entries);

        // 验证一致性
        assert_eq!(summary.total_entries, entries.len());
        assert_eq!(summary.total_duration_ms, total_duration);
        assert!((summary.success_rate - rate).abs() < 0.0001);
        assert_eq!(grouped.len(), summary.by_action.len());
        assert_eq!(timeline.len(), summary.timeline.len());
        assert_eq!(timeline.len(), 100); // 限制为最近 100 条

        // 验证报告可读
        let report = summary.to_report();
        assert!(report.contains("DevTrace 开发追踪报告"));
        assert!(report.contains(&format!("总条目: {}", entries.len())));
        assert!(report.contains(&format_duration_human(total_duration)));
        assert!(report.contains(&format_success_rate_percent(rate)));
    }

    #[test]
    fn test_pure_functions_empty_entries_all_consistent() {
        let entries: Vec<DevTraceEntry> = vec![];

        let rate = calculate_success_rate(0, 0);
        let grouped = group_entries_by_action(&entries);
        let timeline = build_timeline(&entries, 100);
        let summary = DevTraceSummary::from_entries(&entries);

        // 所有函数对空输入应返回空/默认值
        assert_eq!(rate, 0.0);
        assert!(grouped.is_empty());
        assert!(timeline.is_empty());
        assert_eq!(summary.total_entries, 0);
        assert_eq!(summary.total_duration_ms, 0);
        assert_eq!(summary.success_rate, 0.0);
        assert!(summary.timeline.is_empty());
        assert!(summary.by_action.is_empty());

        // 报告仍可生成
        let report = summary.to_report();
        assert!(report.contains("DevTrace 开发追踪报告"));
        assert!(report.contains("总条目: 0"));
    }

    // ======================================================================
    //  Session 69: TraceStore 工厂模式集成测试
    // ======================================================================

    #[test]
    fn test_new_with_backend_jsonl() {
        let dir = tempdir().unwrap();
        let writer = DevTraceWriter::new_with_backend(dir.path(), StorageBackend::Jsonl);
        assert_eq!(writer.backend_type(), StorageBackend::Jsonl);
        assert!(writer.trace_path.ends_with("devtrace.jsonl"));
    }

    #[test]
    fn test_new_with_backend_json() {
        let dir = tempdir().unwrap();
        let writer = DevTraceWriter::new_with_backend(dir.path(), StorageBackend::Json);
        assert_eq!(writer.backend_type(), StorageBackend::Json);
        assert!(writer.trace_path.ends_with("devtrace.json"));
    }

    #[test]
    fn test_new_with_backend_sqlite_fallback() {
        let dir = tempdir().unwrap();
        let writer = DevTraceWriter::new_with_backend(dir.path(), StorageBackend::Sqlite);
        // SQLite 应回退到 JSONL
        assert_eq!(writer.backend_type(), StorageBackend::Jsonl);
        assert!(writer.trace_path.ends_with("devtrace.jsonl"));
    }

    #[test]
    fn test_new_with_backend_postgres_fallback() {
        let dir = tempdir().unwrap();
        let writer = DevTraceWriter::new_with_backend(dir.path(), StorageBackend::Postgres);
        // PostgreSQL 应回退到 JSONL
        assert_eq!(writer.backend_type(), StorageBackend::Jsonl);
        assert!(writer.trace_path.ends_with("devtrace.jsonl"));
    }

    #[test]
    fn test_from_storage_config_jsonl() {
        use crate::trace_store::StorageConfig;
        let config = StorageConfig {
            backend: StorageBackend::Jsonl,
            path: PathBuf::from("/tmp/test/.forge/devtrace.jsonl"),
        };
        let writer = DevTraceWriter::from_storage_config(&config);
        assert_eq!(writer.backend_type(), StorageBackend::Jsonl);
        assert_eq!(
            writer.trace_path,
            PathBuf::from("/tmp/test/.forge/devtrace.jsonl")
        );
    }

    #[test]
    fn test_from_storage_config_json() {
        use crate::trace_store::StorageConfig;
        let config = StorageConfig {
            backend: StorageBackend::Json,
            path: PathBuf::from("/tmp/test/.forge/devtrace.json"),
        };
        let writer = DevTraceWriter::from_storage_config(&config);
        assert_eq!(writer.backend_type(), StorageBackend::Json);
    }

    #[test]
    fn test_from_storage_config_sqlite_fallback() {
        use crate::trace_store::StorageConfig;
        let config = StorageConfig {
            backend: StorageBackend::Sqlite,
            path: PathBuf::from("/tmp/test/.forge/devtrace.db"),
        };
        let writer = DevTraceWriter::from_storage_config(&config);
        assert_eq!(writer.backend_type(), StorageBackend::Jsonl);
    }

    #[test]
    fn test_json_backend_write_and_read() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
        let writer = DevTraceWriter::new_with_backend(dir.path(), StorageBackend::Json);

        let entry = make_entry(TraceAction::TaskExecution, true);
        writer.write_entry(&entry).unwrap();

        let entries = writer.read_all().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, TraceAction::TaskExecution);
    }

    #[test]
    fn test_json_backend_multiple_writes() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
        let writer = DevTraceWriter::new_with_backend(dir.path(), StorageBackend::Json);

        for i in 0..5 {
            let entry = DevTraceEntry::new(
                TraceAction::TaskExecution,
                Some(0),
                Some(i),
                Some(&format!("task{}", i)),
                "input",
                "output",
                1000 * (i + 1) as u64,
                true,
                None,
            );
            writer.write_entry(&entry).unwrap();
        }

        let entries = writer.read_all().unwrap();
        assert_eq!(entries.len(), 5);
        assert_eq!(entries[0].task_idx, Some(0));
        assert_eq!(entries[4].task_idx, Some(4));
    }

    #[test]
    fn test_json_backend_clear() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
        let writer = DevTraceWriter::new_with_backend(dir.path(), StorageBackend::Json);

        writer
            .write_entry(&make_entry(TraceAction::Planning, true))
            .unwrap();
        assert_eq!(writer.entry_count(), 1);

        writer.clear().unwrap();
        assert_eq!(writer.entry_count(), 0);

        // 清空后应能继续写入
        writer
            .write_entry(&make_entry(TraceAction::TaskExecution, true))
            .unwrap();
        assert_eq!(writer.entry_count(), 1);
    }

    #[test]
    fn test_json_backend_creates_parent_dir() {
        let dir = tempdir().unwrap();
        let writer = DevTraceWriter::new_with_backend(dir.path(), StorageBackend::Json);

        assert!(!writer.trace_path.exists());

        writer
            .write_entry(&make_entry(TraceAction::Planning, true))
            .unwrap();

        assert!(writer.trace_path.exists());
    }

    #[test]
    fn test_json_backend_read_empty() {
        let dir = tempdir().unwrap();
        let writer = DevTraceWriter::new_with_backend(dir.path(), StorageBackend::Json);

        let entries = writer.read_all().unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_json_backend_summary() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
        let writer = DevTraceWriter::new_with_backend(dir.path(), StorageBackend::Json);

        writer
            .write_entry(&make_entry(TraceAction::Planning, true))
            .unwrap();
        writer
            .write_entry(&make_entry(TraceAction::TaskExecution, true))
            .unwrap();
        writer
            .write_entry(&make_entry(TraceAction::CompileCheck, false))
            .unwrap();

        let summary = writer.summary();
        assert_eq!(summary.total_entries, 3);
        assert!((summary.success_rate - 2.0 / 3.0).abs() < 0.001);
    }

    #[test]
    fn test_clone_preserves_backend_jsonl() {
        let dir = tempdir().unwrap();
        let writer = DevTraceWriter::new_with_backend(dir.path(), StorageBackend::Jsonl);
        let cloned = writer.clone();
        assert_eq!(cloned.backend_type(), StorageBackend::Jsonl);
        assert_eq!(cloned.trace_path, writer.trace_path);
    }

    #[test]
    fn test_clone_preserves_backend_json() {
        let dir = tempdir().unwrap();
        let writer = DevTraceWriter::new_with_backend(dir.path(), StorageBackend::Json);
        let cloned = writer.clone();
        assert_eq!(cloned.backend_type(), StorageBackend::Json);
        assert_eq!(cloned.trace_path, writer.trace_path);
    }

    #[test]
    fn test_json_and_jsonl_write_same_content() {
        // JSON 和 JSONL 后端写入相同条目后, 读取结果应一致
        let dir1 = tempdir().unwrap();
        std::fs::create_dir_all(dir1.path().join(".forge")).unwrap();
        let writer_jsonl = DevTraceWriter::new_with_backend(dir1.path(), StorageBackend::Jsonl);

        let dir2 = tempdir().unwrap();
        std::fs::create_dir_all(dir2.path().join(".forge")).unwrap();
        let writer_json = DevTraceWriter::new_with_backend(dir2.path(), StorageBackend::Json);

        let entry = make_entry(TraceAction::TaskExecution, true);
        writer_jsonl.write_entry(&entry).unwrap();
        writer_json.write_entry(&entry).unwrap();

        let entries_jsonl = writer_jsonl.read_all().unwrap();
        let entries_json = writer_json.read_all().unwrap();

        assert_eq!(entries_jsonl.len(), 1);
        assert_eq!(entries_json.len(), 1);
        assert_eq!(entries_jsonl[0].action, entries_json[0].action);
        assert_eq!(entries_jsonl[0].success, entries_json[0].success);
    }

    #[test]
    fn test_default_new_uses_jsonl_backend() {
        let dir = tempdir().unwrap();
        let writer = DevTraceWriter::new(dir.path());
        assert_eq!(writer.backend_type(), StorageBackend::Jsonl);
    }

    // ===== IncrementalStats 测试 (Session 75) =====

    #[test]
    fn test_incremental_stats_new() {
        let stats = IncrementalStats::new();
        assert_eq!(stats.total_messages, 0);
        assert_eq!(stats.sent_messages, 0);
        assert_eq!(stats.skipped_messages, 0);
        assert_eq!(stats.send_count, 0);
    }

    #[test]
    fn test_incremental_stats_record_full_send() {
        let mut stats = IncrementalStats::new();
        stats.record(5, 5); // 全量发送: total=5, sent=5, skipped=0
        assert_eq!(stats.total_messages, 5);
        assert_eq!(stats.sent_messages, 5);
        assert_eq!(stats.skipped_messages, 0);
        assert_eq!(stats.send_count, 1);
        assert!((stats.saved_ratio() - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_incremental_stats_record_partial_send() {
        let mut stats = IncrementalStats::new();
        stats.record(5, 2); // 增量发送: total=5, sent=2, skipped=3
        assert_eq!(stats.total_messages, 5);
        assert_eq!(stats.sent_messages, 2);
        assert_eq!(stats.skipped_messages, 3);
        assert!((stats.saved_ratio() - 0.6).abs() < 0.001);
    }

    #[test]
    fn test_incremental_stats_record_multiple() {
        let mut stats = IncrementalStats::new();
        stats.record(5, 5); // 第一次全量
        stats.record(5, 2); // 第二次增量
        stats.record(3, 0); // 第三次全部跳过

        assert_eq!(stats.total_messages, 13); // 5 + 5 + 3
        assert_eq!(stats.sent_messages, 7); // 5 + 2 + 0
        assert_eq!(stats.skipped_messages, 6); // 0 + 3 + 3
        assert_eq!(stats.send_count, 3);
        assert!((stats.saved_ratio() - 6.0 / 13.0).abs() < 0.001);
    }

    #[test]
    fn test_incremental_stats_saved_ratio_empty() {
        let stats = IncrementalStats::new();
        assert_eq!(stats.saved_ratio(), 0.0);
    }

    #[test]
    fn test_incremental_stats_avg_messages_per_send() {
        let mut stats = IncrementalStats::new();
        stats.record(5, 5);
        stats.record(3, 1);
        assert!((stats.avg_messages_per_send() - 4.0).abs() < 0.001); // (5+3)/2
    }

    #[test]
    fn test_incremental_stats_avg_sent_per_send() {
        let mut stats = IncrementalStats::new();
        stats.record(5, 5);
        stats.record(3, 1);
        assert!((stats.avg_sent_per_send() - 3.0).abs() < 0.001); // (5+1)/2
    }

    #[test]
    fn test_incremental_stats_avg_empty() {
        let stats = IncrementalStats::new();
        assert_eq!(stats.avg_messages_per_send(), 0.0);
        assert_eq!(stats.avg_sent_per_send(), 0.0);
    }

    #[test]
    fn test_incremental_stats_to_summary() {
        let mut stats = IncrementalStats::new();
        stats.record(10, 7);
        let summary = stats.to_summary();
        assert!(summary.contains("1 次发送"));
        assert!(summary.contains("总消息 10 条"));
        assert!(summary.contains("实际发送 7 条"));
        assert!(summary.contains("跳过 3 条"));
        assert!(summary.contains("30.0%"));
    }

    #[test]
    fn test_incremental_stats_default() {
        let stats = IncrementalStats::default();
        assert_eq!(stats.send_count, 0);
    }

    #[test]
    fn test_incremental_stats_serde_roundtrip() {
        let mut stats = IncrementalStats::new();
        stats.record(10, 7);
        let json = serde_json::to_string(&stats).unwrap();
        let deserialized: IncrementalStats = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.total_messages, 10);
        assert_eq!(deserialized.sent_messages, 7);
        assert_eq!(deserialized.skipped_messages, 3);
        assert_eq!(deserialized.send_count, 1);
    }

    // ===== TraceAction::IncrementalSend 测试 (Session 75) =====

    #[test]
    fn test_trace_action_incremental_send_display() {
        assert_eq!(TraceAction::IncrementalSend.to_string(), "IncrementalSend");
    }

    #[test]
    fn test_trace_action_incremental_send_description() {
        assert_eq!(TraceAction::IncrementalSend.description(), "增量发送");
    }

    #[test]
    fn test_trace_action_incremental_send_in_all() {
        let all = TraceAction::all();
        assert!(
            all.contains(&TraceAction::IncrementalSend),
            "TraceAction::all() should contain IncrementalSend"
        );
    }

    #[test]
    fn test_trace_action_incremental_send_serde() {
        let action = TraceAction::IncrementalSend;
        let json = serde_json::to_string(&action).unwrap();
        assert_eq!(json, "\"IncrementalSend\"");

        let parsed: TraceAction = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, action);
    }

    #[test]
    fn test_trace_action_incremental_send_hash() {
        let mut map: HashMap<TraceAction, usize> = HashMap::new();
        map.insert(TraceAction::IncrementalSend, 1);
        assert_eq!(map.get(&TraceAction::IncrementalSend), Some(&1));
    }

    #[test]
    fn test_dev_trace_entry_with_incremental_send() {
        let entry = DevTraceEntry::new(
            TraceAction::IncrementalSend,
            None,
            None,
            None,
            "total=5, sent=2, skipped=3",
            "AI response",
            500,
            true,
            None,
        );
        assert_eq!(entry.action, TraceAction::IncrementalSend);
        let json = entry.to_jsonl().unwrap();
        assert!(json.contains("IncrementalSend"));

        let parsed = DevTraceEntry::from_jsonl(&json).unwrap();
        assert_eq!(parsed.action, TraceAction::IncrementalSend);
    }

    #[test]
    fn test_incremental_send_trace_in_report() {
        let (_dir, writer) = make_writer();
        writer
            .trace(
                TraceAction::IncrementalSend,
                None,
                None,
                None,
                "total=10, sent=3, skipped=7",
                "response",
                1000,
                true,
                None,
            )
            .unwrap();

        let summary = writer.summary();
        assert_eq!(summary.total_entries, 1);
        let report = summary.to_report();
        assert!(report.contains("增量发送"));
    }

    // ===== Session 76: 增量发送效果可视化 测试 =====

    #[test]
    fn test_parse_incremental_entry_normal() {
        assert_eq!(
            parse_incremental_entry("total=5, sent=2, skipped=3"),
            Some((5, 2, 3))
        );
    }

    #[test]
    fn test_parse_incremental_entry_with_prefix() {
        assert_eq!(
            parse_incremental_entry("[全部跳过] total=10, sent=0, skipped=10"),
            Some((10, 0, 10))
        );
    }

    #[test]
    fn test_parse_incremental_entry_large_numbers() {
        assert_eq!(
            parse_incremental_entry("total=100000, sent=50000, skipped=50000"),
            Some((100000, 50000, 50000))
        );
    }

    #[test]
    fn test_parse_incremental_entry_malformed() {
        assert_eq!(parse_incremental_entry("not a valid format"), None);
        assert_eq!(parse_incremental_entry(""), None);
        assert_eq!(
            parse_incremental_entry("total=abc, sent=2, skipped=3"),
            None
        );
    }

    #[test]
    fn test_parse_incremental_entry_missing_fields() {
        assert_eq!(parse_incremental_entry("total=5, sent=2"), None);
        assert_eq!(parse_incremental_entry("sent=2, skipped=3"), None);
        assert_eq!(parse_incremental_entry("total=5"), None);
    }

    #[test]
    fn test_summary_incremental_summary_none_without_entries() {
        let entries = vec![
            make_entry(TraceAction::TaskExecution, true),
            make_entry(TraceAction::FixAttempt, false),
        ];
        let summary = DevTraceSummary::from_entries(&entries);
        assert!(summary.incremental_summary.is_none());
    }

    #[test]
    fn test_summary_incremental_summary_from_single_entry() {
        let entries = vec![DevTraceEntry::new(
            TraceAction::IncrementalSend,
            None,
            None,
            None,
            "total=10, sent=3, skipped=7",
            "response",
            500,
            true,
            None,
        )];
        let summary = DevTraceSummary::from_entries(&entries);
        let inc = summary.incremental_summary.expect("should be Some");
        assert_eq!(inc.send_count, 1);
        assert_eq!(inc.total_messages, 10);
        assert_eq!(inc.sent_messages, 3);
        assert_eq!(inc.skipped_messages, 7);
    }

    #[test]
    fn test_summary_incremental_summary_from_multiple_entries() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::IncrementalSend,
                None,
                None,
                None,
                "total=5, sent=5, skipped=0",
                "resp1",
                100,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::IncrementalSend,
                None,
                None,
                None,
                "total=5, sent=2, skipped=3",
                "resp2",
                200,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::IncrementalSend,
                None,
                None,
                None,
                "[全部跳过] total=3, sent=0, skipped=3",
                "",
                0,
                true,
                None,
            ),
        ];
        let summary = DevTraceSummary::from_entries(&entries);
        let inc = summary.incremental_summary.expect("should be Some");
        assert_eq!(inc.send_count, 3);
        assert_eq!(inc.total_messages, 13); // 5 + 5 + 3
        assert_eq!(inc.sent_messages, 7); // 5 + 2 + 0
        assert_eq!(inc.skipped_messages, 6); // 0 + 3 + 3
    }

    #[test]
    fn test_summary_incremental_summary_mixed_entries() {
        // IncrementalSend 条目和其他类型混合
        let entries = vec![
            make_entry(TraceAction::TaskExecution, true),
            DevTraceEntry::new(
                TraceAction::IncrementalSend,
                None,
                None,
                None,
                "total=10, sent=7, skipped=3",
                "response",
                300,
                true,
                None,
            ),
            make_entry(TraceAction::CompileCheck, true),
        ];
        let summary = DevTraceSummary::from_entries(&entries);
        assert_eq!(summary.total_entries, 3); // 3 entries total
        let inc = summary.incremental_summary.expect("should be Some");
        assert_eq!(inc.send_count, 1);
        assert_eq!(inc.total_messages, 10);
        assert_eq!(inc.sent_messages, 7);
        assert_eq!(inc.skipped_messages, 3);
    }

    #[test]
    fn test_summary_incremental_summary_empty_stats() {
        let entries: Vec<DevTraceEntry> = vec![];
        let summary = DevTraceSummary::from_entries(&entries);
        assert!(summary.incremental_summary.is_none());
    }

    #[test]
    fn test_summary_incremental_summary_malformed_entry_ignored() {
        // 格式错误的 IncrementalSend 条目应被跳过
        let entries = vec![DevTraceEntry::new(
            TraceAction::IncrementalSend,
            None,
            None,
            None,
            "malformed input",
            "response",
            100,
            true,
            None,
        )];
        let summary = DevTraceSummary::from_entries(&entries);
        // 格式错误 → send_count == 0 → None
        assert!(summary.incremental_summary.is_none());
    }

    #[test]
    fn test_to_report_includes_incremental_stats() {
        let entries = vec![DevTraceEntry::new(
            TraceAction::IncrementalSend,
            None,
            None,
            None,
            "total=20, sent=5, skipped=15",
            "response",
            1000,
            true,
            None,
        )];
        let summary = DevTraceSummary::from_entries(&entries);
        let report = summary.to_report();

        assert!(report.contains("增量发送统计"));
        assert!(report.contains("发送次数: 1"));
        assert!(report.contains("总消息: 20 条"));
        assert!(report.contains("实际发送: 5 条"));
        assert!(report.contains("跳过: 15 条"));
        assert!(report.contains("节省比例"));
        assert!(report.contains("75.0%")); // 15/20 = 75%
    }

    #[test]
    fn test_to_report_no_incremental_section_when_none() {
        let entries = vec![make_entry(TraceAction::TaskExecution, true)];
        let summary = DevTraceSummary::from_entries(&entries);
        let report = summary.to_report();

        assert!(!report.contains("增量发送统计"));
    }

    #[test]
    fn test_summary_incremental_summary_serde_roundtrip() {
        let entries = vec![DevTraceEntry::new(
            TraceAction::IncrementalSend,
            None,
            None,
            None,
            "total=10, sent=7, skipped=3",
            "response",
            500,
            true,
            None,
        )];
        let summary = DevTraceSummary::from_entries(&entries);

        let json = serde_json::to_string(&summary).unwrap();
        let deserialized: DevTraceSummary = serde_json::from_str(&json).unwrap();

        let inc = deserialized.incremental_summary.expect("should be Some");
        assert_eq!(inc.send_count, 1);
        assert_eq!(inc.total_messages, 10);
        assert_eq!(inc.sent_messages, 7);
        assert_eq!(inc.skipped_messages, 3);
    }

    #[test]
    fn test_summary_incremental_summary_serde_skip_none() {
        let summary = DevTraceSummary::empty();
        let json = serde_json::to_string(&summary).unwrap();
        // incremental_summary 为 None 时应被 skip
        assert!(!json.contains("incremental_summary"));
    }

    #[test]
    fn test_parse_incremental_entry_extra_whitespace() {
        assert_eq!(
            parse_incremental_entry("total = 5, sent = 2, skipped = 3"),
            Some((5, 2, 3))
        );
    }

    #[test]
    fn test_summary_incremental_summary_with_all_skipped_entry() {
        let entries = vec![DevTraceEntry::new(
            TraceAction::IncrementalSend,
            None,
            None,
            None,
            "[全部跳过] total=5, sent=0, skipped=5",
            "",
            0,
            true,
            None,
        )];
        let summary = DevTraceSummary::from_entries(&entries);
        let inc = summary.incremental_summary.expect("should be Some");
        assert_eq!(inc.send_count, 1);
        assert_eq!(inc.total_messages, 5);
        assert_eq!(inc.sent_messages, 0);
        assert_eq!(inc.skipped_messages, 5);
        assert!((inc.saved_ratio() - 1.0).abs() < 0.001); // 100% saved
    }

    // ===== Session 79: 搜索缓存统计面板 测试 =====

    // --- parse_cache_hit_duration ---

    #[test]
    fn test_parse_cache_hit_duration_normal() {
        let err = "缓存命中 (key=E0308, 原始耗时=500ms, 命中次数=2)";
        assert_eq!(parse_cache_hit_duration(err), Some(500));
    }

    #[test]
    fn test_parse_cache_hit_duration_large_number() {
        let err = "缓存命中 (key=E0277, 原始耗时=15000ms, 命中次数=10)";
        assert_eq!(parse_cache_hit_duration(err), Some(15000));
    }

    #[test]
    fn test_parse_cache_hit_duration_zero() {
        let err = "缓存命中 (key=EOF, 原始耗时=0ms, 命中次数=1)";
        assert_eq!(parse_cache_hit_duration(err), Some(0));
    }

    #[test]
    fn test_parse_cache_hit_duration_not_cache_hit() {
        assert_eq!(parse_cache_hit_duration("编译错误自动搜索"), None);
        assert_eq!(parse_cache_hit_duration("搜索失败: timeout"), None);
    }

    #[test]
    fn test_parse_cache_hit_duration_missing_duration() {
        // 缓存命中但没有 "原始耗时=" 字段
        let err = "缓存命中 (key=E0308, 命中次数=2)";
        assert_eq!(parse_cache_hit_duration(err), None);
    }

    #[test]
    fn test_parse_cache_hit_duration_empty() {
        assert_eq!(parse_cache_hit_duration(""), None);
    }

    #[test]
    fn test_parse_cache_hit_duration_no_digits() {
        let err = "缓存命中 (key=E0308, 原始耗时=abcms, 命中次数=1)";
        assert_eq!(parse_cache_hit_duration(err), None);
    }

    // --- is_cache_miss ---

    #[test]
    fn test_is_cache_miss_with_cached() {
        assert!(is_cache_miss("编译错误自动搜索 (已缓存)"));
    }

    #[test]
    fn test_is_cache_miss_without_cached() {
        assert!(is_cache_miss("编译错误自动搜索"));
    }

    #[test]
    fn test_is_cache_miss_not_miss() {
        assert!(!is_cache_miss("缓存命中 (key=E0308)"));
        assert!(!is_cache_miss("搜索失败: timeout"));
        assert!(!is_cache_miss(""));
    }

    // --- is_search_failure ---

    #[test]
    fn test_is_search_failure_normal() {
        assert!(is_search_failure("搜索失败: connection refused"));
        assert!(is_search_failure("搜索失败: timeout"));
    }

    #[test]
    fn test_is_search_failure_not_failure() {
        assert!(!is_search_failure("缓存命中 (key=E0308)"));
        assert!(!is_search_failure("编译错误自动搜索"));
        assert!(!is_search_failure(""));
    }

    // --- parse_cache_entry ---

    #[test]
    fn test_parse_cache_entry_hit() {
        let entry = DevTraceEntry::new(
            TraceAction::WebSearch,
            None,
            None,
            None,
            "query",
            "result",
            0,
            true,
            Some("缓存命中 (key=E0308, 原始耗时=300ms, 命中次数=1)"),
        );
        assert_eq!(parse_cache_entry(&entry), CacheEntryInfo::Hit(300));
    }

    #[test]
    fn test_parse_cache_entry_miss() {
        let entry = DevTraceEntry::new(
            TraceAction::WebSearch,
            None,
            None,
            None,
            "query",
            "result",
            500,
            true,
            Some("编译错误自动搜索 (已缓存)"),
        );
        assert_eq!(parse_cache_entry(&entry), CacheEntryInfo::Miss);
    }

    #[test]
    fn test_parse_cache_entry_failure() {
        let entry = DevTraceEntry::new(
            TraceAction::WebSearch,
            None,
            None,
            None,
            "query",
            "",
            3000,
            false,
            Some("搜索失败: connection refused"),
        );
        assert_eq!(parse_cache_entry(&entry), CacheEntryInfo::Failure);
    }

    #[test]
    fn test_parse_cache_entry_non_websearch() {
        let entry = make_entry(TraceAction::TaskExecution, true);
        assert_eq!(parse_cache_entry(&entry), CacheEntryInfo::None);
    }

    #[test]
    fn test_parse_cache_entry_no_error() {
        let entry = DevTraceEntry::new(
            TraceAction::WebSearch,
            None,
            None,
            None,
            "query",
            "result",
            500,
            true,
            None,
        );
        assert_eq!(parse_cache_entry(&entry), CacheEntryInfo::None);
    }

    #[test]
    fn test_parse_cache_entry_unrecognized_error() {
        let entry = DevTraceEntry::new(
            TraceAction::WebSearch,
            None,
            None,
            None,
            "query",
            "result",
            500,
            true,
            Some("some unrecognized error"),
        );
        assert_eq!(parse_cache_entry(&entry), CacheEntryInfo::None);
    }

    // --- build_cache_summary ---

    #[test]
    fn test_build_cache_summary_empty() {
        let entries: Vec<DevTraceEntry> = vec![];
        let summary = build_cache_summary(&entries);
        assert!(summary.is_empty());
        assert_eq!(summary.total_searches(), 0);
    }

    #[test]
    fn test_build_cache_summary_all_hits() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::WebSearch,
                None,
                None,
                None,
                "q1",
                "r1",
                0,
                true,
                Some("缓存命中 (key=E0308, 原始耗时=300ms, 命中次数=1)"),
            ),
            DevTraceEntry::new(
                TraceAction::WebSearch,
                None,
                None,
                None,
                "q2",
                "r2",
                0,
                true,
                Some("缓存命中 (key=E0277, 原始耗时=500ms, 命中次数=2)"),
            ),
        ];
        let summary = build_cache_summary(&entries);
        assert_eq!(summary.cache_hits, 2);
        assert_eq!(summary.cache_misses, 0);
        assert_eq!(summary.search_failures, 0);
        assert_eq!(summary.time_saved_ms, 800); // 300 + 500
    }

    #[test]
    fn test_build_cache_summary_all_misses() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::WebSearch,
                None,
                None,
                None,
                "q1",
                "r1",
                500,
                true,
                Some("编译错误自动搜索 (已缓存)"),
            ),
            DevTraceEntry::new(
                TraceAction::WebSearch,
                None,
                None,
                None,
                "q2",
                "r2",
                300,
                true,
                Some("编译错误自动搜索"),
            ),
        ];
        let summary = build_cache_summary(&entries);
        assert_eq!(summary.cache_hits, 0);
        assert_eq!(summary.cache_misses, 2);
        assert_eq!(summary.search_failures, 0);
        assert_eq!(summary.time_saved_ms, 0);
    }

    #[test]
    fn test_build_cache_summary_mixed() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::WebSearch,
                None,
                None,
                None,
                "q1",
                "r1",
                500,
                true,
                Some("编译错误自动搜索 (已缓存)"),
            ),
            DevTraceEntry::new(
                TraceAction::WebSearch,
                None,
                None,
                None,
                "q2",
                "r2",
                0,
                true,
                Some("缓存命中 (key=E0308, 原始耗时=500ms, 命中次数=1)"),
            ),
            DevTraceEntry::new(
                TraceAction::WebSearch,
                None,
                None,
                None,
                "q3",
                "",
                3000,
                false,
                Some("搜索失败: timeout"),
            ),
            // 非 WebSearch 条目应被忽略
            make_entry(TraceAction::TaskExecution, true),
        ];
        let summary = build_cache_summary(&entries);
        assert_eq!(summary.cache_hits, 1);
        assert_eq!(summary.cache_misses, 1);
        assert_eq!(summary.search_failures, 1);
        assert_eq!(summary.total_searches(), 3);
        assert_eq!(summary.time_saved_ms, 500);
    }

    // --- CacheStatsSummary ---

    #[test]
    fn test_cache_stats_new() {
        let stats = CacheStatsSummary::new();
        assert_eq!(stats.cache_hits, 0);
        assert_eq!(stats.cache_misses, 0);
        assert_eq!(stats.search_failures, 0);
        assert_eq!(stats.time_saved_ms, 0);
        assert!(stats.is_empty());
    }

    #[test]
    fn test_cache_stats_record_hit() {
        let mut stats = CacheStatsSummary::new();
        stats.record_hit(500);
        stats.record_hit(300);
        assert_eq!(stats.cache_hits, 2);
        assert_eq!(stats.time_saved_ms, 800);
    }

    #[test]
    fn test_cache_stats_record_miss() {
        let mut stats = CacheStatsSummary::new();
        stats.record_miss();
        stats.record_miss();
        assert_eq!(stats.cache_misses, 2);
        assert_eq!(stats.time_saved_ms, 0);
    }

    #[test]
    fn test_cache_stats_record_failure() {
        let mut stats = CacheStatsSummary::new();
        stats.record_failure();
        assert_eq!(stats.search_failures, 1);
        assert_eq!(stats.total_searches(), 1);
    }

    #[test]
    fn test_cache_stats_hit_rate() {
        let mut stats = CacheStatsSummary::new();
        stats.record_hit(500);
        stats.record_miss();
        stats.record_hit(300);
        stats.record_miss();
        // hit_rate = 2 / (2+2) = 0.5
        assert!((stats.hit_rate() - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_cache_stats_hit_rate_empty() {
        let stats = CacheStatsSummary::new();
        assert_eq!(stats.hit_rate(), 0.0);
    }

    #[test]
    fn test_cache_stats_hit_rate_failures_excluded() {
        let mut stats = CacheStatsSummary::new();
        stats.record_hit(500);
        stats.record_miss();
        stats.record_failure();
        stats.record_failure();
        // hit_rate = 1 / (1+1) = 0.5, failures excluded
        assert!((stats.hit_rate() - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_cache_stats_avg_time_saved_per_hit() {
        let mut stats = CacheStatsSummary::new();
        stats.record_hit(500);
        stats.record_hit(300);
        stats.record_hit(700);
        // avg = (500+300+700) / 3 = 500
        assert!((stats.avg_time_saved_per_hit() - 500.0).abs() < 0.001);
    }

    #[test]
    fn test_cache_stats_avg_time_saved_no_hits() {
        let stats = CacheStatsSummary::new();
        assert_eq!(stats.avg_time_saved_per_hit(), 0.0);
    }

    #[test]
    fn test_cache_stats_total_searches() {
        let mut stats = CacheStatsSummary::new();
        stats.record_hit(100);
        stats.record_miss();
        stats.record_failure();
        assert_eq!(stats.total_searches(), 3);
    }

    #[test]
    fn test_cache_stats_to_summary() {
        let mut stats = CacheStatsSummary::new();
        stats.record_hit(500);
        stats.record_miss();
        let summary = stats.to_summary();
        assert!(summary.contains("命中 1"));
        assert!(summary.contains("未命中 1"));
        assert!(summary.contains("命中率 50.0%"));
        assert!(summary.contains("节省 500ms"));
    }

    #[test]
    fn test_cache_stats_is_empty() {
        let stats = CacheStatsSummary::new();
        assert!(stats.is_empty());

        let mut stats2 = CacheStatsSummary::new();
        stats2.record_hit(100);
        assert!(!stats2.is_empty());
    }

    // --- DevTraceSummary 集成 ---

    #[test]
    fn test_summary_cache_summary_none_without_websearch() {
        let entries = vec![
            make_entry(TraceAction::TaskExecution, true),
            make_entry(TraceAction::FixAttempt, false),
        ];
        let summary = DevTraceSummary::from_entries(&entries);
        assert!(summary.cache_summary.is_none());
    }

    #[test]
    fn test_summary_cache_summary_from_websearch_entries() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::WebSearch,
                None,
                None,
                None,
                "q1",
                "r1",
                500,
                true,
                Some("编译错误自动搜索 (已缓存)"),
            ),
            DevTraceEntry::new(
                TraceAction::WebSearch,
                None,
                None,
                None,
                "q2",
                "r2",
                0,
                true,
                Some("缓存命中 (key=E0308, 原始耗时=500ms, 命中次数=1)"),
            ),
        ];
        let summary = DevTraceSummary::from_entries(&entries);
        let cache = summary.cache_summary.expect("should be Some");
        assert_eq!(cache.cache_hits, 1);
        assert_eq!(cache.cache_misses, 1);
        assert_eq!(cache.time_saved_ms, 500);
        assert!((cache.hit_rate() - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_summary_cache_summary_empty_websearch_ignored() {
        // WebSearch 条目但没有缓存相关的 error → None
        let entries = vec![DevTraceEntry::new(
            TraceAction::WebSearch,
            None,
            None,
            None,
            "q",
            "r",
            500,
            true,
            None, // no error → None
        )];
        let summary = DevTraceSummary::from_entries(&entries);
        assert!(summary.cache_summary.is_none());
    }

    #[test]
    fn test_to_report_includes_cache_stats() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::WebSearch,
                None,
                None,
                None,
                "q1",
                "r1",
                500,
                true,
                Some("编译错误自动搜索 (已缓存)"),
            ),
            DevTraceEntry::new(
                TraceAction::WebSearch,
                None,
                None,
                None,
                "q2",
                "r2",
                0,
                true,
                Some("缓存命中 (key=E0308, 原始耗时=500ms, 命中次数=1)"),
            ),
            DevTraceEntry::new(
                TraceAction::WebSearch,
                None,
                None,
                None,
                "q3",
                "",
                3000,
                false,
                Some("搜索失败: timeout"),
            ),
        ];
        let summary = DevTraceSummary::from_entries(&entries);
        let report = summary.to_report();

        assert!(report.contains("搜索缓存统计"));
        assert!(report.contains("总搜索: 3 次"));
        assert!(report.contains("缓存命中: 1 次"));
        assert!(report.contains("缓存未命中: 1 次"));
        assert!(report.contains("搜索失败: 1 次"));
        assert!(report.contains("命中率: 50.0%"));
        assert!(report.contains("节省时间:"));
    }

    #[test]
    fn test_to_report_no_cache_section_when_none() {
        let entries = vec![make_entry(TraceAction::TaskExecution, true)];
        let summary = DevTraceSummary::from_entries(&entries);
        let report = summary.to_report();

        assert!(!report.contains("搜索缓存统计"));
    }

    #[test]
    fn test_summary_cache_summary_serde_roundtrip() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::WebSearch,
                None,
                None,
                None,
                "q1",
                "r1",
                500,
                true,
                Some("编译错误自动搜索 (已缓存)"),
            ),
            DevTraceEntry::new(
                TraceAction::WebSearch,
                None,
                None,
                None,
                "q2",
                "r2",
                0,
                true,
                Some("缓存命中 (key=E0308, 原始耗时=500ms, 命中次数=1)"),
            ),
        ];
        let summary = DevTraceSummary::from_entries(&entries);

        let json = serde_json::to_string(&summary).unwrap();
        let deserialized: DevTraceSummary = serde_json::from_str(&json).unwrap();

        let cache = deserialized.cache_summary.expect("should be Some");
        assert_eq!(cache.cache_hits, 1);
        assert_eq!(cache.cache_misses, 1);
        assert_eq!(cache.time_saved_ms, 500);
    }

    #[test]
    fn test_summary_cache_summary_serde_skip_none() {
        let summary = DevTraceSummary::empty();
        let json = serde_json::to_string(&summary).unwrap();
        // cache_summary 为 None 时应被 skip
        assert!(!json.contains("cache_summary"));
    }

    #[test]
    fn test_summary_both_incremental_and_cache() {
        // 同时有增量发送和搜索缓存的条目
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::IncrementalSend,
                None,
                None,
                None,
                "total=10, sent=3, skipped=7",
                "response",
                500,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::WebSearch,
                None,
                None,
                None,
                "q",
                "r",
                0,
                true,
                Some("缓存命中 (key=E0308, 原始耗时=300ms, 命中次数=1)"),
            ),
        ];
        let summary = DevTraceSummary::from_entries(&entries);
        assert!(summary.incremental_summary.is_some());
        assert!(summary.cache_summary.is_some());

        let report = summary.to_report();
        assert!(report.contains("增量发送统计"));
        assert!(report.contains("搜索缓存统计"));
    }

    #[test]
    fn test_summary_cache_stats_realistic_workflow() {
        // 模拟真实工作流: 3次未命中 + 5次命中 + 1次失败
        let mut entries: Vec<DevTraceEntry> = vec![];

        // 3 次缓存未命中 (不同错误代码的首次搜索)
        for (i, code) in ["E0308", "E0277", "E0433"].iter().enumerate() {
            entries.push(DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(i),
                Some(&format!("task{}", i)),
                &format!("rust {}", code),
                "search result",
                500 + i as u64 * 100,
                true,
                Some("编译错误自动搜索 (已缓存)"),
            ));
        }

        // 5 次缓存命中 (相同错误代码重复出现)
        for i in 0..5 {
            entries.push(DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(i),
                Some(&format!("task{}", i)),
                "rust E0308",
                "cached result",
                0,
                true,
                Some("缓存命中 (key=E0308, 原始耗时=500ms, 命中次数=1)"),
            ));
        }

        // 1 次搜索失败
        entries.push(DevTraceEntry::new(
            TraceAction::WebSearch,
            Some(0),
            Some(5),
            Some("task5"),
            "rust E0507",
            "",
            3000,
            false,
            Some("搜索失败: connection refused"),
        ));

        // 添加一些非 WebSearch 条目
        entries.push(make_entry(TraceAction::TaskExecution, true));
        entries.push(make_entry(TraceAction::FixAttempt, false));

        let summary = DevTraceSummary::from_entries(&entries);
        let cache = summary.cache_summary.expect("should be Some");

        assert_eq!(cache.cache_hits, 5);
        assert_eq!(cache.cache_misses, 3);
        assert_eq!(cache.search_failures, 1);
        assert_eq!(cache.total_searches(), 9);
        assert_eq!(cache.time_saved_ms, 2500); // 5 * 500
        assert!((cache.hit_rate() - 5.0 / 8.0).abs() < 0.001); // 5/(5+3)
        assert!((cache.avg_time_saved_per_hit() - 500.0).abs() < 0.001);
    }

    // ===== Session 80: 缓存命中率与修复成功率关联分析 测试 =====

    // --- CacheFixCorrelation 基本方法 ---

    #[test]
    fn test_cache_fix_correlation_new() {
        let corr = CacheFixCorrelation::new();
        assert_eq!(corr.checks_after_hit, 0);
        assert_eq!(corr.successes_after_hit, 0);
        assert_eq!(corr.checks_after_miss, 0);
        assert_eq!(corr.successes_after_miss, 0);
        assert_eq!(corr.checks_after_failure, 0);
        assert_eq!(corr.successes_after_failure, 0);
        assert_eq!(corr.searches_without_check, 0);
        assert!(corr.is_empty());
    }

    #[test]
    fn test_cache_fix_record_hit_success() {
        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(true);
        assert_eq!(corr.checks_after_hit, 1);
        assert_eq!(corr.successes_after_hit, 1);
    }

    #[test]
    fn test_cache_fix_record_hit_failure() {
        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(false);
        assert_eq!(corr.checks_after_hit, 1);
        assert_eq!(corr.successes_after_hit, 0);
    }

    #[test]
    fn test_cache_fix_record_miss_success() {
        let mut corr = CacheFixCorrelation::new();
        corr.record_miss_check(true);
        assert_eq!(corr.checks_after_miss, 1);
        assert_eq!(corr.successes_after_miss, 1);
    }

    #[test]
    fn test_cache_fix_record_miss_failure() {
        let mut corr = CacheFixCorrelation::new();
        corr.record_miss_check(false);
        assert_eq!(corr.checks_after_miss, 1);
        assert_eq!(corr.successes_after_miss, 0);
    }

    #[test]
    fn test_cache_fix_record_failure_success() {
        let mut corr = CacheFixCorrelation::new();
        corr.record_failure_check(true);
        assert_eq!(corr.checks_after_failure, 1);
        assert_eq!(corr.successes_after_failure, 1);
    }

    #[test]
    fn test_cache_fix_record_failure_failure() {
        let mut corr = CacheFixCorrelation::new();
        corr.record_failure_check(false);
        assert_eq!(corr.checks_after_failure, 1);
        assert_eq!(corr.successes_after_failure, 0);
    }

    #[test]
    fn test_cache_fix_record_no_check() {
        let mut corr = CacheFixCorrelation::new();
        corr.record_no_check();
        corr.record_no_check();
        assert_eq!(corr.searches_without_check, 2);
        assert!(!corr.is_empty());
    }

    // --- 修复成功率计算 ---

    #[test]
    fn test_cache_fix_hit_fix_rate() {
        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_hit_check(false);
        // 2/3 = 0.6667
        assert!((corr.hit_fix_rate() - 2.0 / 3.0).abs() < 0.001);
    }

    #[test]
    fn test_cache_fix_hit_fix_rate_empty() {
        let corr = CacheFixCorrelation::new();
        assert_eq!(corr.hit_fix_rate(), 0.0);
    }

    #[test]
    fn test_cache_fix_miss_fix_rate() {
        let mut corr = CacheFixCorrelation::new();
        corr.record_miss_check(true);
        corr.record_miss_check(false);
        // 1/2 = 0.5
        assert!((corr.miss_fix_rate() - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_cache_fix_miss_fix_rate_empty() {
        let corr = CacheFixCorrelation::new();
        assert_eq!(corr.miss_fix_rate(), 0.0);
    }

    #[test]
    fn test_cache_fix_failure_fix_rate() {
        let mut corr = CacheFixCorrelation::new();
        corr.record_failure_check(true);
        corr.record_failure_check(false);
        corr.record_failure_check(false);
        // 1/3 = 0.3333
        assert!((corr.failure_fix_rate() - 1.0 / 3.0).abs() < 0.001);
    }

    #[test]
    fn test_cache_fix_failure_fix_rate_empty() {
        let corr = CacheFixCorrelation::new();
        assert_eq!(corr.failure_fix_rate(), 0.0);
    }

    // --- 差值和有效性 ---

    #[test]
    fn test_cache_fix_hit_vs_miss_diff_positive() {
        // 缓存有效: 命中修复率 > 未命中修复率
        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(false);
        // hit: 2/2 = 100%, miss: 1/2 = 50%, diff: +50%
        assert!((corr.hit_vs_miss_diff() - 0.5).abs() < 0.001);
        assert!(corr.is_cache_effective());
    }

    #[test]
    fn test_cache_fix_hit_vs_miss_diff_negative() {
        // 缓存无效: 命中修复率 < 未命中修复率
        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(true);
        corr.record_hit_check(false);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        // hit: 1/2 = 50%, miss: 2/2 = 100%, diff: -50%
        assert!((corr.hit_vs_miss_diff() - (-0.5)).abs() < 0.001);
        assert!(!corr.is_cache_effective());
    }

    #[test]
    fn test_cache_fix_hit_vs_miss_diff_zero() {
        // 两者修复率相同
        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(true);
        corr.record_hit_check(false);
        corr.record_miss_check(true);
        corr.record_miss_check(false);
        // hit: 1/2 = 50%, miss: 1/2 = 50%, diff: 0%
        assert!((corr.hit_vs_miss_diff()).abs() < 0.001);
        assert!(corr.is_cache_effective()); // >= 包含等于
    }

    #[test]
    fn test_cache_fix_hit_vs_miss_diff_no_data() {
        // hit 有数据, miss 无数据 → 返回 0.0
        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        assert_eq!(corr.hit_vs_miss_diff(), 0.0);
        assert!(!corr.is_cache_effective());
    }

    #[test]
    fn test_cache_fix_is_cache_effective_no_hit_data() {
        let mut corr = CacheFixCorrelation::new();
        corr.record_miss_check(true);
        assert!(!corr.is_cache_effective());
    }

    #[test]
    fn test_cache_fix_is_cache_effective_no_miss_data() {
        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(true);
        assert!(!corr.is_cache_effective());
    }

    // --- 汇总方法 ---

    #[test]
    fn test_cache_fix_total_correlated() {
        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(true);
        corr.record_hit_check(false);
        corr.record_miss_check(true);
        corr.record_failure_check(false);
        corr.record_failure_check(true);
        // 2 + 1 + 2 = 5
        assert_eq!(corr.total_correlated(), 5);
    }

    #[test]
    fn test_cache_fix_is_empty() {
        let corr = CacheFixCorrelation::new();
        assert!(corr.is_empty());

        let mut corr2 = CacheFixCorrelation::new();
        corr2.record_hit_check(true);
        assert!(!corr2.is_empty());

        let mut corr3 = CacheFixCorrelation::new();
        corr3.record_no_check();
        assert!(!corr3.is_empty());
    }

    #[test]
    fn test_cache_fix_to_summary() {
        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(true);
        corr.record_miss_check(false);
        let summary = corr.to_summary();
        assert!(summary.contains("命中后 1 次检查"));
        assert!(summary.contains("通过 1"));
        assert!(summary.contains("未命中后 1 次检查"));
        assert!(summary.contains("通过 0"));
        assert!(summary.contains("命中修复率 100.0%"));
        assert!(summary.contains("未命中修复率 0.0%"));
    }

    // --- find_next_compile_check ---

    #[test]
    fn test_find_next_compile_check_found() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(0),
                Some("task"),
                "query",
                "result",
                100,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::FixAttempt,
                Some(0),
                Some(0),
                Some("task"),
                "fix",
                "response",
                200,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(0),
                Some("task"),
                "check",
                "passed",
                50,
                true,
                None,
            ),
        ];
        assert_eq!(find_next_compile_check(&entries, 0), Some(true));
    }

    #[test]
    fn test_find_next_compile_check_not_found() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(0),
                Some("task"),
                "query",
                "result",
                100,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::FixAttempt,
                Some(0),
                Some(0),
                Some("task"),
                "fix",
                "response",
                200,
                true,
                None,
            ),
        ];
        assert_eq!(find_next_compile_check(&entries, 0), None);
    }

    #[test]
    fn test_find_next_compile_check_wrong_phase() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(0),
                Some("task"),
                "query",
                "result",
                100,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(1), // 不同 phase
                Some(0),
                Some("task"),
                "check",
                "passed",
                50,
                true,
                None,
            ),
        ];
        // phase 不匹配, 不应找到
        assert_eq!(find_next_compile_check(&entries, 0), None);
    }

    #[test]
    fn test_find_next_compile_check_wrong_task() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(0),
                Some("task"),
                "query",
                "result",
                100,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(1), // 不同 task
                Some("task"),
                "check",
                "passed",
                50,
                true,
                None,
            ),
        ];
        // task 不匹配, 不应找到
        assert_eq!(find_next_compile_check(&entries, 0), None);
    }

    #[test]
    fn test_find_next_compile_check_skips_non_compile() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(0),
                Some("task"),
                "query",
                "result",
                100,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::FixAttempt,
                Some(0),
                Some(0),
                Some("task"),
                "fix",
                "response",
                200,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::Clarification,
                Some(0),
                Some(0),
                Some("task"),
                "clarify",
                "response",
                100,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(0),
                Some("task"),
                "check",
                "failed",
                50,
                false,
                None,
            ),
        ];
        // 应跳过 FixAttempt 和 Clarification, 找到 CompileCheck (failed)
        assert_eq!(find_next_compile_check(&entries, 0), Some(false));
    }

    #[test]
    fn test_find_next_compile_check_none_phase_task() {
        // 当 WebSearch 和 CompileCheck 都没有 phase/task 时应匹配
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::WebSearch,
                None,
                None,
                None,
                "query",
                "result",
                100,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                None,
                None,
                None,
                "check",
                "passed",
                50,
                true,
                None,
            ),
        ];
        assert_eq!(find_next_compile_check(&entries, 0), Some(true));
    }

    #[test]
    fn test_find_next_compile_check_out_of_bounds() {
        let entries = vec![DevTraceEntry::new(
            TraceAction::WebSearch,
            None,
            None,
            None,
            "query",
            "result",
            100,
            true,
            None,
        )];
        // from_idx 超出范围
        assert_eq!(find_next_compile_check(&entries, 10), None);
    }

    #[test]
    fn test_find_next_compile_check_empty_entries() {
        let entries: Vec<DevTraceEntry> = vec![];
        assert_eq!(find_next_compile_check(&entries, 0), None);
    }

    // --- build_cache_fix_correlation ---

    #[test]
    fn test_build_correlation_empty() {
        let entries: Vec<DevTraceEntry> = vec![];
        let corr = build_cache_fix_correlation(&entries);
        assert!(corr.is_empty());
    }

    #[test]
    fn test_build_correlation_hit_success() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(0),
                Some("task"),
                "query",
                "result",
                0,
                true,
                Some("缓存命中 (key=E0308, 原始耗时=500ms, 命中次数=1)"),
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(0),
                Some("task"),
                "check",
                "passed",
                50,
                true,
                None,
            ),
        ];
        let corr = build_cache_fix_correlation(&entries);
        assert_eq!(corr.checks_after_hit, 1);
        assert_eq!(corr.successes_after_hit, 1);
        assert!((corr.hit_fix_rate() - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_build_correlation_hit_failure() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(0),
                Some("task"),
                "query",
                "result",
                0,
                true,
                Some("缓存命中 (key=E0308, 原始耗时=500ms, 命中次数=1)"),
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(0),
                Some("task"),
                "check",
                "failed",
                50,
                false,
                None,
            ),
        ];
        let corr = build_cache_fix_correlation(&entries);
        assert_eq!(corr.checks_after_hit, 1);
        assert_eq!(corr.successes_after_hit, 0);
        assert_eq!(corr.hit_fix_rate(), 0.0);
    }

    #[test]
    fn test_build_correlation_miss_success() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(0),
                Some("task"),
                "query",
                "result",
                500,
                true,
                Some("编译错误自动搜索"),
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(0),
                Some("task"),
                "check",
                "passed",
                50,
                true,
                None,
            ),
        ];
        let corr = build_cache_fix_correlation(&entries);
        assert_eq!(corr.checks_after_miss, 1);
        assert_eq!(corr.successes_after_miss, 1);
        assert!((corr.miss_fix_rate() - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_build_correlation_miss_failure() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(0),
                Some("task"),
                "query",
                "result",
                500,
                true,
                Some("编译错误自动搜索 (已缓存)"),
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(0),
                Some("task"),
                "check",
                "failed",
                50,
                false,
                None,
            ),
        ];
        let corr = build_cache_fix_correlation(&entries);
        assert_eq!(corr.checks_after_miss, 1);
        assert_eq!(corr.successes_after_miss, 0);
        assert_eq!(corr.miss_fix_rate(), 0.0);
    }

    #[test]
    fn test_build_correlation_failure_search() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(0),
                Some("task"),
                "query",
                "",
                3000,
                false,
                Some("搜索失败: connection refused"),
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(0),
                Some("task"),
                "check",
                "failed",
                50,
                false,
                None,
            ),
        ];
        let corr = build_cache_fix_correlation(&entries);
        assert_eq!(corr.checks_after_failure, 1);
        assert_eq!(corr.successes_after_failure, 0);
        assert_eq!(corr.failure_fix_rate(), 0.0);
    }

    #[test]
    fn test_build_correlation_no_subsequent_check() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(0),
                Some("task"),
                "query",
                "result",
                500,
                true,
                Some("编译错误自动搜索"),
            ),
            // 没有 CompileCheck 条目
            DevTraceEntry::new(
                TraceAction::FixAttempt,
                Some(0),
                Some(0),
                Some("task"),
                "fix",
                "response",
                200,
                true,
                None,
            ),
        ];
        let corr = build_cache_fix_correlation(&entries);
        assert_eq!(corr.searches_without_check, 1);
        assert_eq!(corr.checks_after_miss, 0);
    }

    #[test]
    fn test_build_correlation_mixed() {
        let entries = vec![
            // Task 0: 缓存命中 → 编译通过
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(0),
                Some("task0"),
                "q1",
                "r1",
                0,
                true,
                Some("缓存命中 (key=E0308, 原始耗时=500ms, 命中次数=1)"),
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(0),
                Some("task0"),
                "check",
                "passed",
                50,
                true,
                None,
            ),
            // Task 1: 缓存未命中 → 编译失败
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(1),
                Some("task1"),
                "q2",
                "r2",
                500,
                true,
                Some("编译错误自动搜索"),
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(1),
                Some("task1"),
                "check",
                "failed",
                50,
                false,
                None,
            ),
            // Task 2: 搜索失败 → 编译通过 (AI 自己修好了)
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(2),
                Some("task2"),
                "q3",
                "",
                3000,
                false,
                Some("搜索失败: timeout"),
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(2),
                Some("task2"),
                "check",
                "passed",
                50,
                true,
                None,
            ),
        ];
        let corr = build_cache_fix_correlation(&entries);
        assert_eq!(corr.checks_after_hit, 1);
        assert_eq!(corr.successes_after_hit, 1);
        assert_eq!(corr.checks_after_miss, 1);
        assert_eq!(corr.successes_after_miss, 0);
        assert_eq!(corr.checks_after_failure, 1);
        assert_eq!(corr.successes_after_failure, 1);
        assert_eq!(corr.total_correlated(), 3);
        // hit: 100%, miss: 0%, diff: +100% → 缓存有效
        assert!(corr.is_cache_effective());
        assert!((corr.hit_vs_miss_diff() - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_build_correlation_ignores_non_websearch() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::TaskExecution,
                Some(0),
                Some(0),
                Some("task"),
                "input",
                "output",
                1000,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(0),
                Some("task"),
                "check",
                "passed",
                50,
                true,
                None,
            ),
        ];
        let corr = build_cache_fix_correlation(&entries);
        // 非 WebSearch 条目应被忽略
        assert!(corr.is_empty());
    }

    #[test]
    fn test_build_correlation_realistic_workflow() {
        // 模拟真实修复工作流: 多轮修复 + 缓存命中/未命中混合
        let entries: Vec<DevTraceEntry> = vec![
            // Task 0: 首次编译失败 → 搜索 (未命中) → 修复 → 编译通过
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(0),
                Some("task0"),
                "rust E0308",
                "search result",
                500,
                true,
                Some("编译错误自动搜索"),
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(0),
                Some("task0"),
                "check",
                "passed",
                50,
                true,
                None,
            ),
            // Task 1: 首次编译失败 → 搜索 (未命中) → 修复 → 编译失败
            //         → 再次搜索 (命中, 相同错误) → 修复 → 编译通过
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(1),
                Some("task1"),
                "rust E0277",
                "search result",
                600,
                true,
                Some("编译错误自动搜索"),
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(1),
                Some("task1"),
                "check",
                "failed",
                50,
                false,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(1),
                Some("task1"),
                "rust E0277",
                "cached result",
                0,
                true,
                Some("缓存命中 (key=E0277, 原始耗时=600ms, 命中次数=1)"),
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(1),
                Some("task1"),
                "check",
                "passed",
                50,
                true,
                None,
            ),
            // Task 2: 搜索失败 → 编译失败 (没有搜索结果, AI 也没修好)
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(2),
                Some("task2"),
                "rust E0507",
                "",
                3000,
                false,
                Some("搜索失败: connection refused"),
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(2),
                Some("task2"),
                "check",
                "failed",
                50,
                false,
                None,
            ),
        ];

        let corr = build_cache_fix_correlation(&entries);

        // 统计验证
        assert_eq!(corr.checks_after_hit, 1); // Task1 第二次搜索
        assert_eq!(corr.successes_after_hit, 1); // Task1 第二次编译通过
        assert_eq!(corr.checks_after_miss, 2); // Task0 + Task1 第一次
        assert_eq!(corr.successes_after_miss, 1); // Task0 通过, Task1 第一次失败
        assert_eq!(corr.checks_after_failure, 1); // Task2
        assert_eq!(corr.successes_after_failure, 0); // Task2 失败
        assert_eq!(corr.total_correlated(), 4);

        // 修复率: hit 100%, miss 50%, failure 0%
        assert!((corr.hit_fix_rate() - 1.0).abs() < 0.001);
        assert!((corr.miss_fix_rate() - 0.5).abs() < 0.001);
        assert_eq!(corr.failure_fix_rate(), 0.0);

        // 差值: 100% - 50% = +50% → 缓存有效
        assert!((corr.hit_vs_miss_diff() - 0.5).abs() < 0.001);
        assert!(corr.is_cache_effective());
    }

    // --- DevTraceSummary 集成 ---

    #[test]
    fn test_summary_correlation_none_without_websearch() {
        let entries = vec![
            make_entry(TraceAction::TaskExecution, true),
            make_entry(TraceAction::CompileCheck, true),
        ];
        let summary = DevTraceSummary::from_entries(&entries);
        assert!(summary.cache_fix_correlation.is_none());
    }

    #[test]
    fn test_summary_correlation_from_entries() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(0),
                Some("task"),
                "query",
                "result",
                0,
                true,
                Some("缓存命中 (key=E0308, 原始耗时=500ms, 命中次数=1)"),
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(0),
                Some("task"),
                "check",
                "passed",
                50,
                true,
                None,
            ),
        ];
        let summary = DevTraceSummary::from_entries(&entries);
        let corr = summary.cache_fix_correlation.expect("should be Some");
        assert_eq!(corr.checks_after_hit, 1);
        assert_eq!(corr.successes_after_hit, 1);
    }

    #[test]
    fn test_summary_correlation_websearch_without_compile_check() {
        // 有 WebSearch 但没有后续 CompileCheck → searches_without_check > 0
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(0),
                Some("task"),
                "query",
                "result",
                500,
                true,
                Some("编译错误自动搜索"),
            ),
            // 没有 CompileCheck
        ];
        let summary = DevTraceSummary::from_entries(&entries);
        let corr = summary.cache_fix_correlation.expect("should be Some");
        assert_eq!(corr.searches_without_check, 1);
        assert_eq!(corr.total_correlated(), 0);
    }

    #[test]
    fn test_summary_correlation_in_report() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(0),
                Some("task"),
                "query",
                "result",
                0,
                true,
                Some("缓存命中 (key=E0308, 原始耗时=500ms, 命中次数=1)"),
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(0),
                Some("task"),
                "check",
                "passed",
                50,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(1),
                Some("task2"),
                "query",
                "result",
                500,
                true,
                Some("编译错误自动搜索"),
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(1),
                Some("task2"),
                "check",
                "failed",
                50,
                false,
                None,
            ),
        ];
        let summary = DevTraceSummary::from_entries(&entries);
        let report = summary.to_report();

        assert!(report.contains("缓存与修复关联分析"));
        assert!(report.contains("命中后检查: 1 次 (通过 1)"));
        assert!(report.contains("未命中后检查: 1 次 (通过 0)"));
        assert!(report.contains("命中后修复率: 100.0%"));
        assert!(report.contains("未命中后修复率: 0.0%"));
        assert!(report.contains("缓存有效"));
    }

    #[test]
    fn test_summary_correlation_not_in_report() {
        // 没有 WebSearch → 报告中不应包含关联分析
        let entries = vec![
            make_entry(TraceAction::TaskExecution, true),
            make_entry(TraceAction::CompileCheck, true),
        ];
        let summary = DevTraceSummary::from_entries(&entries);
        let report = summary.to_report();
        assert!(!report.contains("缓存与修复关联分析"));
    }

    #[test]
    fn test_summary_correlation_serde_roundtrip() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(0),
                Some("task"),
                "query",
                "result",
                0,
                true,
                Some("缓存命中 (key=E0308, 原始耗时=500ms, 命中次数=1)"),
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(0),
                Some("task"),
                "check",
                "passed",
                50,
                true,
                None,
            ),
        ];
        let summary = DevTraceSummary::from_entries(&entries);

        let json = serde_json::to_string(&summary).unwrap();
        let deserialized: DevTraceSummary = serde_json::from_str(&json).unwrap();

        let corr = deserialized.cache_fix_correlation.expect("should be Some");
        assert_eq!(corr.checks_after_hit, 1);
        assert_eq!(corr.successes_after_hit, 1);
    }

    #[test]
    fn test_summary_correlation_serde_skip_none() {
        let summary = DevTraceSummary::empty();
        let json = serde_json::to_string(&summary).unwrap();
        // cache_fix_correlation 为 None 时应被 skip
        assert!(!json.contains("cache_fix_correlation"));
    }

    #[test]
    fn test_summary_all_three_stats() {
        // 同时有增量发送 + 缓存统计 + 关联分析
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::IncrementalSend,
                None,
                None,
                None,
                "total=10, sent=3, skipped=7",
                "response",
                500,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(0),
                Some("task"),
                "query",
                "result",
                0,
                true,
                Some("缓存命中 (key=E0308, 原始耗时=300ms, 命中次数=1)"),
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(0),
                Some("task"),
                "check",
                "passed",
                50,
                true,
                None,
            ),
        ];
        let summary = DevTraceSummary::from_entries(&entries);
        assert!(summary.incremental_summary.is_some());
        assert!(summary.cache_summary.is_some());
        assert!(summary.cache_fix_correlation.is_some());

        let report = summary.to_report();
        assert!(report.contains("增量发送统计"));
        assert!(report.contains("搜索缓存统计"));
        assert!(report.contains("缓存与修复关联分析"));

        let corr = summary.cache_fix_correlation.expect("should be Some");
        assert_eq!(corr.checks_after_hit, 1);
        assert_eq!(corr.successes_after_hit, 1);
        assert!(!corr.is_cache_effective()); // 没有 miss 数据
    }

    // ===== Session 83: CacheTuner 调优效果可视化 测试 =====

    // --- parse_tuning_action 测试 ---

    #[test]
    fn test_parse_tuning_action_keep_current() {
        let action = parse_tuning_action("缓存调优: 保持当前配置 (差值 +5.0%, 原因: 数据不足)");
        assert_eq!(action, Some(TuningActionInfo::KeepCurrent));
    }

    #[test]
    fn test_parse_tuning_action_adjust_ttl() {
        let action =
            parse_tuning_action("缓存调优: 调整 TTL: 1800s → 2700s (差值 +67.0%, 原因: 缓存有效)");
        assert_eq!(
            action,
            Some(TuningActionInfo::AdjustTtl {
                old_ttl: 1800,
                new_ttl: 2700
            })
        );
    }

    #[test]
    fn test_parse_tuning_action_disable_cache() {
        let action = parse_tuning_action("缓存调优: 禁用缓存 (差值 -100.0%, 原因: 缓存有害)");
        assert_eq!(action, Some(TuningActionInfo::DisableCache));
    }

    #[test]
    fn test_parse_tuning_action_invalid() {
        assert_eq!(parse_tuning_action("not a valid format"), None);
        assert_eq!(parse_tuning_action(""), None);
    }

    #[test]
    fn test_parse_tuning_action_reduce_ttl() {
        let action =
            parse_tuning_action("缓存调优: 调整 TTL: 1800s → 900s (差值 -8.0%, 原因: 缓存略差)");
        assert_eq!(
            action,
            Some(TuningActionInfo::AdjustTtl {
                old_ttl: 1800,
                new_ttl: 900
            })
        );
    }

    // --- parse_ttl_value 测试 ---

    #[test]
    fn test_parse_ttl_value_valid() {
        assert_eq!(parse_ttl_value("1800s"), Some(1800));
        assert_eq!(parse_ttl_value(" 2700s "), Some(2700));
        assert_eq!(parse_ttl_value("60s"), Some(60));
    }

    #[test]
    fn test_parse_ttl_value_invalid() {
        assert_eq!(parse_ttl_value("1800"), None); // missing 's'
        assert_eq!(parse_ttl_value("abc"), None);
        assert_eq!(parse_ttl_value(""), None);
    }

    // --- parse_correlation_diff 测试 ---

    #[test]
    fn test_parse_correlation_diff_positive() {
        let diff = parse_correlation_diff(
            "缓存调优: 调整 TTL: 1800s → 2700s (差值 +67.0%, 原因: 缓存有效)",
        );
        assert!(diff.is_some());
        assert!((diff.unwrap() - 0.67).abs() < 0.001);
    }

    #[test]
    fn test_parse_correlation_diff_negative() {
        let diff = parse_correlation_diff("缓存调优: 禁用缓存 (差值 -100.0%, 原因: 缓存有害)");
        assert!(diff.is_some());
        assert!((diff.unwrap() - (-1.0)).abs() < 0.001);
    }

    #[test]
    fn test_parse_correlation_diff_zero() {
        let diff = parse_correlation_diff("缓存调优: 保持当前配置 (差值 +0.0%, 原因: 持平)");
        assert!(diff.is_some());
        assert!((diff.unwrap() - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_parse_correlation_diff_invalid() {
        assert_eq!(parse_correlation_diff("no diff here"), None);
        assert_eq!(parse_correlation_diff(""), None);
    }

    // --- parse_hit_miss 测试 ---

    #[test]
    fn test_parse_hit_miss_valid() {
        let result = parse_hit_miss("hit=2/3 miss=3/3");
        assert_eq!(result, Some((2, 3, 3, 3)));
    }

    #[test]
    fn test_parse_hit_miss_zeros() {
        let result = parse_hit_miss("hit=0/0 miss=0/0");
        assert_eq!(result, Some((0, 0, 0, 0)));
    }

    #[test]
    fn test_parse_hit_miss_invalid() {
        assert_eq!(parse_hit_miss("not a valid format"), None);
        assert_eq!(parse_hit_miss(""), None);
        assert_eq!(parse_hit_miss("hit=2 miss=3"), None); // missing /
    }

    // --- parse_cache_tuning_entry 测试 ---

    #[test]
    fn test_parse_cache_tuning_entry_adjust_ttl() {
        let entry = DevTraceEntry::new(
            TraceAction::CacheTuning,
            Some(0),
            Some(0),
            Some("task"),
            "hit=2/3 miss=1/3",
            "缓存调优: 调整 TTL: 1800s → 2700s (差值 +67.0%, 原因: 缓存有效)",
            0,
            true,
            Some("缓存有效"),
        );
        let info = parse_cache_tuning_entry(&entry).unwrap();
        assert_eq!(
            info.action,
            TuningActionInfo::AdjustTtl {
                old_ttl: 1800,
                new_ttl: 2700
            }
        );
        assert!((info.correlation_diff - 0.67).abs() < 0.001);
        assert_eq!(info.hit_successes, 2);
        assert_eq!(info.hit_checks, 3);
        assert_eq!(info.miss_successes, 1);
        assert_eq!(info.miss_checks, 3);
    }

    #[test]
    fn test_parse_cache_tuning_entry_keep_current() {
        let entry = DevTraceEntry::new(
            TraceAction::CacheTuning,
            None,
            None,
            None,
            "hit=1/3 miss=1/3",
            "缓存调优: 保持当前配置 (差值 +0.0%, 原因: 数据不足)",
            0,
            true,
            Some("数据不足"),
        );
        let info = parse_cache_tuning_entry(&entry).unwrap();
        assert_eq!(info.action, TuningActionInfo::KeepCurrent);
        assert!((info.correlation_diff - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_parse_cache_tuning_entry_disable_cache() {
        let entry = DevTraceEntry::new(
            TraceAction::CacheTuning,
            Some(0),
            Some(0),
            Some("task"),
            "hit=0/3 miss=3/3",
            "缓存调优: 禁用缓存 (差值 -100.0%, 原因: 缓存有害)",
            0,
            true,
            Some("缓存有害"),
        );
        let info = parse_cache_tuning_entry(&entry).unwrap();
        assert_eq!(info.action, TuningActionInfo::DisableCache);
        assert!((info.correlation_diff - (-1.0)).abs() < 0.001);
    }

    #[test]
    fn test_parse_cache_tuning_entry_wrong_action() {
        let entry = DevTraceEntry::new(
            TraceAction::TaskExecution,
            None,
            None,
            None,
            "hit=2/3 miss=3/3",
            "缓存调优: 保持当前配置 (差值 +5.0%, 原因: ...)",
            0,
            true,
            None,
        );
        assert!(parse_cache_tuning_entry(&entry).is_none());
    }

    #[test]
    fn test_parse_cache_tuning_entry_malformed_output() {
        let entry = DevTraceEntry::new(
            TraceAction::CacheTuning,
            None,
            None,
            None,
            "hit=2/3 miss=3/3",
            "not a valid output",
            0,
            true,
            None,
        );
        assert!(parse_cache_tuning_entry(&entry).is_none());
    }

    #[test]
    fn test_parse_cache_tuning_entry_missing_hit_miss_defaults_to_zero() {
        // input_summary 格式错误时, hit/miss 默认为 0
        let entry = DevTraceEntry::new(
            TraceAction::CacheTuning,
            None,
            None,
            None,
            "malformed input",
            "缓存调优: 保持当前配置 (差值 +5.0%, 原因: ...)",
            0,
            true,
            None,
        );
        let info = parse_cache_tuning_entry(&entry).unwrap();
        assert_eq!(info.hit_successes, 0);
        assert_eq!(info.hit_checks, 0);
        assert_eq!(info.miss_successes, 0);
        assert_eq!(info.miss_checks, 0);
    }

    // --- CacheTuningSummary 方法测试 ---

    #[test]
    fn test_cache_tuning_summary_new() {
        let s = CacheTuningSummary::new();
        assert_eq!(s.total_evaluations, 0);
        assert_eq!(s.keep_current_count, 0);
        assert_eq!(s.adjust_ttl_count, 0);
        assert_eq!(s.disable_count, 0);
        assert!(s.ttl_history.is_empty());
        assert!(s.final_ttl.is_none());
        assert!(!s.cache_disabled);
        assert!(s.correlation_diffs.is_empty());
        assert!(s.is_empty());
    }

    #[test]
    fn test_cache_tuning_summary_is_empty() {
        let s = CacheTuningSummary::new();
        assert!(s.is_empty());

        let mut s = CacheTuningSummary::new();
        s.total_evaluations = 1;
        assert!(!s.is_empty());
    }

    #[test]
    fn test_cache_tuning_summary_avg_correlation_diff() {
        let mut s = CacheTuningSummary::new();
        assert_eq!(s.avg_correlation_diff(), 0.0); // empty

        s.correlation_diffs = vec![0.1, -0.2, 0.3];
        // (0.1 - 0.2 + 0.3) / 3 = 0.2 / 3 ≈ 0.0667
        assert!((s.avg_correlation_diff() - 0.0667).abs() < 0.001);
    }

    #[test]
    fn test_cache_tuning_summary_initial_ttl() {
        let s = CacheTuningSummary::new();
        assert!(s.initial_ttl().is_none());

        let mut s = CacheTuningSummary::new();
        s.ttl_history = vec![(1800, 2700), (2700, 4050)];
        assert_eq!(s.initial_ttl(), Some(1800));
    }

    #[test]
    fn test_cache_tuning_summary_record_keep_current() {
        let mut s = CacheTuningSummary::new();
        s.record(CacheTuningEntryInfo {
            action: TuningActionInfo::KeepCurrent,
            correlation_diff: 0.05,
            hit_successes: 2,
            hit_checks: 3,
            miss_successes: 2,
            miss_checks: 3,
        });
        assert_eq!(s.total_evaluations, 1);
        assert_eq!(s.keep_current_count, 1);
        assert_eq!(s.adjust_ttl_count, 0);
        assert_eq!(s.disable_count, 0);
        assert!(!s.cache_disabled);
        assert_eq!(s.correlation_diffs, vec![0.05]);
    }

    #[test]
    fn test_cache_tuning_summary_record_adjust_ttl() {
        let mut s = CacheTuningSummary::new();
        s.record(CacheTuningEntryInfo {
            action: TuningActionInfo::AdjustTtl {
                old_ttl: 1800,
                new_ttl: 2700,
            },
            correlation_diff: 0.67,
            hit_successes: 3,
            hit_checks: 3,
            miss_successes: 1,
            miss_checks: 3,
        });
        assert_eq!(s.total_evaluations, 1);
        assert_eq!(s.adjust_ttl_count, 1);
        assert_eq!(s.ttl_history, vec![(1800, 2700)]);
        assert_eq!(s.final_ttl, Some(2700));
        assert!(!s.cache_disabled);
    }

    #[test]
    fn test_cache_tuning_summary_record_disable_cache() {
        let mut s = CacheTuningSummary::new();
        s.record(CacheTuningEntryInfo {
            action: TuningActionInfo::DisableCache,
            correlation_diff: -1.0,
            hit_successes: 0,
            hit_checks: 3,
            miss_successes: 3,
            miss_checks: 3,
        });
        assert_eq!(s.total_evaluations, 1);
        assert_eq!(s.disable_count, 1);
        assert!(s.cache_disabled);
    }

    #[test]
    fn test_cache_tuning_summary_record_multiple() {
        let mut s = CacheTuningSummary::new();

        s.record(CacheTuningEntryInfo {
            action: TuningActionInfo::KeepCurrent,
            correlation_diff: 0.0,
            hit_successes: 1,
            hit_checks: 3,
            miss_successes: 1,
            miss_checks: 3,
        });
        s.record(CacheTuningEntryInfo {
            action: TuningActionInfo::AdjustTtl {
                old_ttl: 1800,
                new_ttl: 2700,
            },
            correlation_diff: 0.67,
            hit_successes: 3,
            hit_checks: 3,
            miss_successes: 1,
            miss_checks: 3,
        });
        s.record(CacheTuningEntryInfo {
            action: TuningActionInfo::AdjustTtl {
                old_ttl: 2700,
                new_ttl: 4050,
            },
            correlation_diff: 0.8,
            hit_successes: 3,
            hit_checks: 3,
            miss_successes: 0,
            miss_checks: 3,
        });
        s.record(CacheTuningEntryInfo {
            action: TuningActionInfo::DisableCache,
            correlation_diff: -0.5,
            hit_successes: 0,
            hit_checks: 3,
            miss_successes: 3,
            miss_checks: 3,
        });

        assert_eq!(s.total_evaluations, 4);
        assert_eq!(s.keep_current_count, 1);
        assert_eq!(s.adjust_ttl_count, 2);
        assert_eq!(s.disable_count, 1);
        assert_eq!(s.ttl_history, vec![(1800, 2700), (2700, 4050)]);
        assert_eq!(s.final_ttl, Some(4050));
        assert!(s.cache_disabled);
        assert_eq!(s.correlation_diffs, vec![0.0, 0.67, 0.8, -0.5]);
        assert_eq!(s.initial_ttl(), Some(1800));
    }

    // --- build_cache_tuning_summary 测试 ---

    #[test]
    fn test_build_cache_tuning_summary_empty() {
        let summary = build_cache_tuning_summary(&[]);
        assert!(summary.is_empty());
    }

    #[test]
    fn test_build_cache_tuning_summary_no_tuning_entries() {
        let entries = vec![
            make_entry(TraceAction::TaskExecution, true),
            make_entry(TraceAction::CompileCheck, true),
        ];
        let summary = build_cache_tuning_summary(&entries);
        assert!(summary.is_empty());
    }

    #[test]
    fn test_build_cache_tuning_summary_single_keep_current() {
        let entries = vec![DevTraceEntry::new(
            TraceAction::CacheTuning,
            None,
            None,
            None,
            "hit=1/3 miss=1/3",
            "缓存调优: 保持当前配置 (差值 +0.0%, 原因: 数据不足)",
            0,
            true,
            Some("数据不足"),
        )];
        let summary = build_cache_tuning_summary(&entries);
        assert_eq!(summary.total_evaluations, 1);
        assert_eq!(summary.keep_current_count, 1);
        assert_eq!(summary.adjust_ttl_count, 0);
        assert_eq!(summary.disable_count, 0);
    }

    #[test]
    fn test_build_cache_tuning_summary_mixed_actions() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::CacheTuning,
                Some(0),
                Some(0),
                Some("task"),
                "hit=2/3 miss=3/3",
                "缓存调优: 保持当前配置 (差值 +0.0%, 原因: 持平)",
                0,
                true,
                Some("持平"),
            ),
            DevTraceEntry::new(
                TraceAction::CacheTuning,
                Some(0),
                Some(1),
                Some("task2"),
                "hit=3/3 miss=1/3",
                "缓存调优: 调整 TTL: 1800s → 2700s (差值 +67.0%, 原因: 缓存有效)",
                0,
                true,
                Some("缓存有效"),
            ),
            DevTraceEntry::new(
                TraceAction::CacheTuning,
                Some(0),
                Some(2),
                Some("task3"),
                "hit=3/3 miss=1/3",
                "缓存调优: 调整 TTL: 2700s → 4050s (差值 +80.0%, 原因: 缓存有效)",
                0,
                true,
                Some("缓存有效"),
            ),
            DevTraceEntry::new(
                TraceAction::CacheTuning,
                Some(0),
                Some(3),
                Some("task4"),
                "hit=0/3 miss=3/3",
                "缓存调优: 禁用缓存 (差值 -100.0%, 原因: 缓存有害)",
                0,
                true,
                Some("缓存有害"),
            ),
        ];
        let summary = build_cache_tuning_summary(&entries);
        assert_eq!(summary.total_evaluations, 4);
        assert_eq!(summary.keep_current_count, 1);
        assert_eq!(summary.adjust_ttl_count, 2);
        assert_eq!(summary.disable_count, 1);
        assert_eq!(summary.ttl_history, vec![(1800, 2700), (2700, 4050)]);
        assert_eq!(summary.final_ttl, Some(4050));
        assert!(summary.cache_disabled);
        assert_eq!(summary.initial_ttl(), Some(1800));
        assert_eq!(summary.correlation_diffs.len(), 4);
    }

    #[test]
    fn test_build_cache_tuning_summary_ignores_malformed() {
        let entries = vec![
            // 正常条目
            DevTraceEntry::new(
                TraceAction::CacheTuning,
                None,
                None,
                None,
                "hit=2/3 miss=3/3",
                "缓存调优: 保持当前配置 (差值 +5.0%, 原因: ...)",
                0,
                true,
                None,
            ),
            // 格式错误条目 (应被跳过)
            DevTraceEntry::new(
                TraceAction::CacheTuning,
                None,
                None,
                None,
                "input",
                "malformed output",
                0,
                true,
                None,
            ),
            // 非 CacheTuning 条目 (应被跳过)
            make_entry(TraceAction::TaskExecution, true),
        ];
        let summary = build_cache_tuning_summary(&entries);
        assert_eq!(summary.total_evaluations, 1); // 只解析了 1 个
        assert_eq!(summary.keep_current_count, 1);
    }

    // --- DevTraceSummary 集成测试 ---

    #[test]
    fn test_summary_cache_tuning_none_without_entries() {
        let entries = vec![
            make_entry(TraceAction::TaskExecution, true),
            make_entry(TraceAction::FixAttempt, false),
        ];
        let summary = DevTraceSummary::from_entries(&entries);
        assert!(summary.cache_tuning_summary.is_none());
    }

    #[test]
    fn test_summary_cache_tuning_from_single_entry() {
        let entries = vec![DevTraceEntry::new(
            TraceAction::CacheTuning,
            Some(0),
            Some(0),
            Some("task"),
            "hit=2/3 miss=3/3",
            "缓存调优: 调整 TTL: 1800s → 2700s (差值 +67.0%, 原因: 缓存有效)",
            0,
            true,
            Some("缓存有效"),
        )];
        let summary = DevTraceSummary::from_entries(&entries);
        let tuning = summary.cache_tuning_summary.expect("should be Some");
        assert_eq!(tuning.total_evaluations, 1);
        assert_eq!(tuning.adjust_ttl_count, 1);
        assert_eq!(tuning.final_ttl, Some(2700));
    }

    #[test]
    fn test_summary_cache_tuning_from_multiple_entries() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::CacheTuning,
                Some(0),
                Some(0),
                Some("task"),
                "hit=2/3 miss=3/3",
                "缓存调优: 调整 TTL: 1800s → 900s (差值 -8.0%, 原因: 缓存略差)",
                0,
                true,
                Some("缓存略差"),
            ),
            DevTraceEntry::new(
                TraceAction::CacheTuning,
                Some(0),
                Some(1),
                Some("task2"),
                "hit=0/3 miss=3/3",
                "缓存调优: 禁用缓存 (差值 -100.0%, 原因: 缓存有害)",
                0,
                true,
                Some("缓存有害"),
            ),
        ];
        let summary = DevTraceSummary::from_entries(&entries);
        let tuning = summary.cache_tuning_summary.expect("should be Some");
        assert_eq!(tuning.total_evaluations, 2);
        assert_eq!(tuning.adjust_ttl_count, 1);
        assert_eq!(tuning.disable_count, 1);
        assert_eq!(tuning.ttl_history, vec![(1800, 900)]);
        assert!(tuning.cache_disabled);
    }

    #[test]
    fn test_summary_cache_tuning_serde_roundtrip() {
        let entries = vec![DevTraceEntry::new(
            TraceAction::CacheTuning,
            Some(0),
            Some(0),
            Some("task"),
            "hit=2/3 miss=3/3",
            "缓存调优: 调整 TTL: 1800s → 2700s (差值 +67.0%, 原因: 缓存有效)",
            0,
            true,
            Some("缓存有效"),
        )];
        let summary = DevTraceSummary::from_entries(&entries);

        let json = serde_json::to_string(&summary).unwrap();
        let deserialized: DevTraceSummary = serde_json::from_str(&json).unwrap();

        let tuning = deserialized.cache_tuning_summary.expect("should be Some");
        assert_eq!(tuning.total_evaluations, 1);
        assert_eq!(tuning.adjust_ttl_count, 1);
        assert_eq!(tuning.final_ttl, Some(2700));
    }

    #[test]
    fn test_summary_cache_tuning_serde_skip_none() {
        let summary = DevTraceSummary::empty();
        let json = serde_json::to_string(&summary).unwrap();
        // cache_tuning_summary 为 None 时应被 skip
        assert!(!json.contains("cache_tuning_summary"));
    }

    // --- to_report 测试 ---

    #[test]
    fn test_to_report_includes_cache_tuning() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::CacheTuning,
                Some(0),
                Some(0),
                Some("task"),
                "hit=2/3 miss=3/3",
                "缓存调优: 调整 TTL: 1800s → 2700s (差值 +67.0%, 原因: 缓存有效)",
                0,
                true,
                Some("缓存有效"),
            ),
            DevTraceEntry::new(
                TraceAction::CacheTuning,
                Some(0),
                Some(1),
                Some("task2"),
                "hit=0/3 miss=3/3",
                "缓存调优: 禁用缓存 (差值 -100.0%, 原因: 缓存有害)",
                0,
                true,
                Some("缓存有害"),
            ),
        ];
        let summary = DevTraceSummary::from_entries(&entries);
        let report = summary.to_report();

        assert!(report.contains("缓存调优效果"));
        assert!(report.contains("总评估: 2 次"));
        assert!(report.contains("调整 TTL: 1 次"));
        assert!(report.contains("禁用缓存: 1 次"));
        assert!(report.contains("TTL 变化: 1800s → 2700s"));
        assert!(report.contains("缓存状态: 已禁用"));
    }

    #[test]
    fn test_to_report_no_cache_tuning_section_when_none() {
        let entries = vec![make_entry(TraceAction::TaskExecution, true)];
        let summary = DevTraceSummary::from_entries(&entries);
        let report = summary.to_report();

        assert!(!report.contains("缓存调优效果"));
    }

    #[test]
    fn test_to_report_cache_tuning_keep_only() {
        let entries = vec![DevTraceEntry::new(
            TraceAction::CacheTuning,
            None,
            None,
            None,
            "hit=1/3 miss=1/3",
            "缓存调优: 保持当前配置 (差值 +0.0%, 原因: 数据不足)",
            0,
            true,
            Some("数据不足"),
        )];
        let summary = DevTraceSummary::from_entries(&entries);
        let report = summary.to_report();

        assert!(report.contains("缓存调优效果"));
        assert!(report.contains("总评估: 1 次"));
        assert!(report.contains("保持当前: 1 次"));
        assert!(!report.contains("调整 TTL"));
        assert!(!report.contains("禁用缓存"));
    }

    #[test]
    fn test_to_report_cache_tuning_final_ttl() {
        let entries = vec![DevTraceEntry::new(
            TraceAction::CacheTuning,
            None,
            None,
            None,
            "hit=3/3 miss=1/3",
            "缓存调优: 调整 TTL: 1800s → 2700s (差值 +67.0%, 原因: 缓存有效)",
            0,
            true,
            Some("缓存有效"),
        )];
        let summary = DevTraceSummary::from_entries(&entries);
        let report = summary.to_report();

        assert!(report.contains("最终 TTL: 2700s"));
        assert!(!report.contains("缓存状态: 已禁用"));
    }

    #[test]
    fn test_to_report_cache_tuning_avg_diff() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::CacheTuning,
                None,
                None,
                None,
                "hit=2/3 miss=3/3",
                "缓存调优: 保持当前配置 (差值 +10.0%, 原因: ...)",
                0,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::CacheTuning,
                None,
                None,
                None,
                "hit=3/3 miss=1/3",
                "缓存调优: 保持当前配置 (差值 +20.0%, 原因: ...)",
                0,
                true,
                None,
            ),
        ];
        let summary = DevTraceSummary::from_entries(&entries);
        let report = summary.to_report();

        // 平均差值 = (10% + 20%) / 2 = 15%
        assert!(report.contains("平均差值: +15.0%"));
    }

    #[test]
    fn test_summary_all_sections_with_cache_tuning() {
        // 同时有增量发送、搜索缓存、关联分析和缓存调优的条目
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::IncrementalSend,
                None,
                None,
                None,
                "total=10, sent=3, skipped=7",
                "response",
                500,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(0),
                Some("task"),
                "query",
                "result",
                0,
                true,
                Some("缓存命中 (key=E0308, 原始耗时=300ms, 命中次数=1)"),
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(0),
                Some("task"),
                "check",
                "passed",
                50,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::CacheTuning,
                Some(0),
                Some(0),
                Some("task"),
                "hit=2/3 miss=3/3",
                "缓存调优: 调整 TTL: 1800s → 2700s (差值 +67.0%, 原因: 缓存有效)",
                0,
                true,
                Some("缓存有效"),
            ),
        ];
        let summary = DevTraceSummary::from_entries(&entries);
        assert!(summary.incremental_summary.is_some());
        assert!(summary.cache_summary.is_some());
        assert!(summary.cache_fix_correlation.is_some());
        assert!(summary.cache_tuning_summary.is_some());

        let report = summary.to_report();
        assert!(report.contains("增量发送统计"));
        assert!(report.contains("搜索缓存统计"));
        assert!(report.contains("缓存与修复关联分析"));
        assert!(report.contains("缓存调优效果"));
    }

    // ======================================================================
    //  Session 87: 跨 Session 历史摘要测试
    // ======================================================================

    // --- SearchQualityHistorySummary 测试 ---

    #[test]
    fn test_sq_history_summary_new() {
        let s = SearchQualityHistorySummary::new(true, false, 5, 1, None);
        assert!(s.initial_enabled);
        assert!(!s.current_enabled);
        assert_eq!(s.evaluation_count, 5);
        assert_eq!(s.disable_count, 1);
        assert!(s.enabled_changed);
        assert!(s.saved_at.is_none());
    }

    #[test]
    fn test_sq_history_summary_default() {
        let s = SearchQualityHistorySummary::default();
        assert!(s.initial_enabled);
        assert!(s.current_enabled);
        assert_eq!(s.evaluation_count, 0);
        assert!(s.is_empty());
        assert!(!s.enabled_changed);
    }

    #[test]
    fn test_sq_history_summary_is_empty() {
        let s = SearchQualityHistorySummary::new(true, true, 0, 0, None);
        assert!(s.is_empty());

        let s2 = SearchQualityHistorySummary::new(true, true, 1, 0, None);
        assert!(!s2.is_empty());
    }

    #[test]
    fn test_sq_history_summary_enabled_changed() {
        // true → false: changed
        let s = SearchQualityHistorySummary::new(true, false, 1, 1, None);
        assert!(s.enabled_changed);

        // true → true: not changed
        let s2 = SearchQualityHistorySummary::new(true, true, 1, 0, None);
        assert!(!s2.enabled_changed);

        // false → true: changed
        let s3 = SearchQualityHistorySummary::new(false, true, 1, 0, None);
        assert!(s3.enabled_changed);

        // false → false: not changed
        let s4 = SearchQualityHistorySummary::new(false, false, 0, 0, None);
        assert!(!s4.enabled_changed);
    }

    #[test]
    fn test_sq_history_summary_disable_rate() {
        // 1/5 = 20%
        let s = SearchQualityHistorySummary::new(true, false, 5, 1, None);
        assert!((s.disable_rate() - 0.2).abs() < 0.001);

        // 0 evaluations → 0.0
        let s2 = SearchQualityHistorySummary::new(true, true, 0, 0, None);
        assert!((s2.disable_rate() - 0.0).abs() < 0.001);

        // 3/3 = 100%
        let s3 = SearchQualityHistorySummary::new(true, false, 3, 3, None);
        assert!((s3.disable_rate() - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_sq_history_summary_serde_roundtrip() {
        let s = SearchQualityHistorySummary::new(
            true,
            false,
            5,
            1,
            Some("2024-01-01T00:00:00Z".to_string()),
        );
        let json = serde_json::to_string(&s).unwrap();
        let loaded: SearchQualityHistorySummary = serde_json::from_str(&json).unwrap();
        assert!(loaded.initial_enabled);
        assert!(!loaded.current_enabled);
        assert_eq!(loaded.evaluation_count, 5);
        assert_eq!(loaded.disable_count, 1);
        assert!(loaded.enabled_changed);
        assert_eq!(loaded.saved_at, Some("2024-01-01T00:00:00Z".to_string()));
    }

    #[test]
    fn test_sq_history_summary_serde_skip_none() {
        let s = SearchQualityHistorySummary::new(true, true, 0, 0, None);
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("saved_at"));
    }

    // --- CacheTuningHistorySummary 测试 ---

    #[test]
    fn test_ct_history_summary_new() {
        let s = CacheTuningHistorySummary::new(1800, 2700, true, 1, 0, 1, None);
        assert_eq!(s.initial_ttl, 1800);
        assert_eq!(s.current_ttl, 2700);
        assert!(s.enabled);
        assert_eq!(s.adjustment_count, 1);
        assert_eq!(s.disable_count, 0);
        assert_eq!(s.decision_count, 1);
        assert_eq!(s.ttl_delta, 900);
        assert!(s.saved_at.is_none());
    }

    #[test]
    fn test_ct_history_summary_default() {
        let s = CacheTuningHistorySummary::default();
        assert_eq!(s.initial_ttl, 0);
        assert_eq!(s.current_ttl, 0);
        assert!(s.enabled);
        assert_eq!(s.ttl_delta, 0);
        assert!(s.is_empty());
    }

    #[test]
    fn test_ct_history_summary_is_empty() {
        let s = CacheTuningHistorySummary::new(1800, 1800, true, 0, 0, 0, None);
        assert!(s.is_empty());

        let s2 = CacheTuningHistorySummary::new(1800, 2700, true, 1, 0, 1, None);
        assert!(!s2.is_empty());
    }

    #[test]
    fn test_ct_history_summary_ttl_delta_positive() {
        let s = CacheTuningHistorySummary::new(1800, 2700, true, 1, 0, 1, None);
        assert_eq!(s.ttl_delta, 900);
        assert!((s.ttl_delta_percent() - 50.0).abs() < 0.001);
    }

    #[test]
    fn test_ct_history_summary_ttl_delta_negative() {
        let s = CacheTuningHistorySummary::new(1800, 900, true, 1, 0, 1, None);
        assert_eq!(s.ttl_delta, -900);
        assert!((s.ttl_delta_percent() - (-50.0)).abs() < 0.001);
    }

    #[test]
    fn test_ct_history_summary_ttl_delta_zero() {
        let s = CacheTuningHistorySummary::new(1800, 1800, true, 0, 0, 0, None);
        assert_eq!(s.ttl_delta, 0);
        assert!((s.ttl_delta_percent() - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_ct_history_summary_disabled() {
        let s = CacheTuningHistorySummary::new(1800, 1800, false, 0, 1, 1, None);
        assert!(!s.enabled);
        assert_eq!(s.disable_count, 1);
    }

    #[test]
    fn test_ct_history_summary_serde_roundtrip() {
        let s = CacheTuningHistorySummary::new(
            1800,
            2700,
            true,
            2,
            0,
            2,
            Some("2024-01-01T00:00:00Z".to_string()),
        );
        let json = serde_json::to_string(&s).unwrap();
        let loaded: CacheTuningHistorySummary = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.initial_ttl, 1800);
        assert_eq!(loaded.current_ttl, 2700);
        assert!(loaded.enabled);
        assert_eq!(loaded.adjustment_count, 2);
        assert_eq!(loaded.ttl_delta, 900);
        assert_eq!(loaded.saved_at, Some("2024-01-01T00:00:00Z".to_string()));
    }

    #[test]
    fn test_ct_history_summary_serde_skip_none() {
        let s = CacheTuningHistorySummary::new(1800, 1800, true, 0, 0, 0, None);
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("saved_at"));
    }

    // --- 纯函数测试 ---

    #[test]
    fn test_build_search_quality_history_summary() {
        let s = build_search_quality_history_summary(true, false, 3, 1, None);
        assert!(s.initial_enabled);
        assert!(!s.current_enabled);
        assert_eq!(s.evaluation_count, 3);
        assert_eq!(s.disable_count, 1);
        assert!(s.enabled_changed);
    }

    #[test]
    fn test_build_search_quality_history_summary_with_timestamp() {
        let s = build_search_quality_history_summary(
            true,
            true,
            10,
            0,
            Some("2024-06-01T12:00:00Z".to_string()),
        );
        assert_eq!(s.saved_at, Some("2024-06-01T12:00:00Z".to_string()));
        assert!(!s.enabled_changed);
        assert!((s.disable_rate() - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_build_cache_tuning_history_summary() {
        let s = build_cache_tuning_history_summary(1800, 2700, true, 1, 0, 1, None);
        assert_eq!(s.initial_ttl, 1800);
        assert_eq!(s.current_ttl, 2700);
        assert_eq!(s.ttl_delta, 900);
        assert!(!s.is_empty());
    }

    #[test]
    fn test_build_cache_tuning_history_summary_disabled() {
        let s = build_cache_tuning_history_summary(1800, 1800, false, 0, 1, 1, None);
        assert!(!s.enabled);
        assert_eq!(s.disable_count, 1);
        assert_eq!(s.ttl_delta, 0);
    }

    #[test]
    fn test_build_cache_tuning_history_summary_with_timestamp() {
        let s = build_cache_tuning_history_summary(
            600,
            1800,
            true,
            3,
            0,
            3,
            Some("2024-06-01T12:00:00Z".to_string()),
        );
        assert_eq!(s.ttl_delta, 1200);
        assert!((s.ttl_delta_percent() - 200.0).abs() < 0.001);
        assert_eq!(s.saved_at, Some("2024-06-01T12:00:00Z".to_string()));
    }

    // --- to_report: 搜索质量历史面板测试 ---

    #[test]
    fn test_to_report_includes_search_quality_history() {
        let summary = DevTraceSummary::empty()
            .with_search_quality_history(SearchQualityHistorySummary::new(true, false, 5, 1, None));
        let report = summary.to_report();

        assert!(report.contains("搜索质量历史 (跨 Session)"));
        assert!(report.contains("初始状态: 启用"));
        assert!(report.contains("最终状态: 禁用"));
        assert!(report.contains("状态变化: ✅ 已变更"));
        assert!(report.contains("累计评估: 5 次"));
        assert!(report.contains("累计禁用: 1 次"));
        assert!(report.contains("禁用率: 20.0%"));
    }

    #[test]
    fn test_to_report_no_search_quality_history_when_none() {
        let summary = DevTraceSummary::empty();
        let report = summary.to_report();
        assert!(!report.contains("搜索质量历史 (跨 Session)"));
    }

    #[test]
    fn test_to_report_search_quality_history_no_change() {
        let summary = DevTraceSummary::empty()
            .with_search_quality_history(SearchQualityHistorySummary::new(true, true, 10, 0, None));
        let report = summary.to_report();

        assert!(report.contains("初始状态: 启用"));
        assert!(report.contains("最终状态: 启用"));
        assert!(report.contains("状态变化: ─ 未变"));
        assert!(report.contains("累计评估: 10 次"));
        // disable_count == 0 → 不显示禁用相关行
        assert!(!report.contains("累计禁用"));
        assert!(!report.contains("禁用率"));
    }

    #[test]
    fn test_to_report_search_quality_history_with_saved_at() {
        let summary =
            DevTraceSummary::empty().with_search_quality_history(SearchQualityHistorySummary::new(
                false,
                true,
                3,
                0,
                Some("2024-06-01T00:00:00Z".to_string()),
            ));
        let report = summary.to_report();

        assert!(report.contains("初始状态: 禁用"));
        assert!(report.contains("最终状态: 启用"));
        assert!(report.contains("状态变化: ✅ 已变更"));
        assert!(report.contains("保存时间: 2024-06-01T00:00:00Z"));
    }

    #[test]
    fn test_to_report_search_quality_history_empty() {
        let summary = DevTraceSummary::empty()
            .with_search_quality_history(SearchQualityHistorySummary::new(true, true, 0, 0, None));
        let report = summary.to_report();

        assert!(report.contains("搜索质量历史 (跨 Session)"));
        assert!(report.contains("累计评估: 0 次"));
        assert!(!report.contains("累计禁用"));
        assert!(!report.contains("禁用率"));
    }

    // --- to_report: 缓存调优历史面板测试 ---

    #[test]
    fn test_to_report_includes_cache_tuning_history() {
        let summary = DevTraceSummary::empty().with_cache_tuning_history(
            CacheTuningHistorySummary::new(1800, 2700, true, 1, 0, 1, None),
        );
        let report = summary.to_report();

        assert!(report.contains("缓存调优历史 (跨 Session)"));
        assert!(report.contains("初始 TTL: 1800s"));
        assert!(report.contains("最终 TTL: 2700s"));
        assert!(report.contains("TTL 变化: +900s (50.0%)"));
        assert!(report.contains("缓存状态: 启用"));
        assert!(report.contains("累计调整: 1 次"));
        assert!(report.contains("决策记录: 1 条"));
    }

    #[test]
    fn test_to_report_no_cache_tuning_history_when_none() {
        let summary = DevTraceSummary::empty();
        let report = summary.to_report();
        assert!(!report.contains("缓存调优历史 (跨 Session)"));
    }

    #[test]
    fn test_to_report_cache_tuning_history_disabled() {
        let summary = DevTraceSummary::empty().with_cache_tuning_history(
            CacheTuningHistorySummary::new(1800, 1800, false, 0, 1, 1, None),
        );
        let report = summary.to_report();

        assert!(report.contains("缓存状态: 已禁用"));
        assert!(report.contains("累计禁用: 1 次"));
        // ttl_delta == 0 → 不显示 TTL 变化行
        assert!(!report.contains("TTL 变化:"));
    }

    #[test]
    fn test_to_report_cache_tuning_history_negative_delta() {
        let summary = DevTraceSummary::empty().with_cache_tuning_history(
            CacheTuningHistorySummary::new(1800, 900, true, 1, 0, 1, None),
        );
        let report = summary.to_report();

        assert!(report.contains("TTL 变化: -900s (-50.0%)"));
    }

    #[test]
    fn test_to_report_cache_tuning_history_zero_delta() {
        let summary = DevTraceSummary::empty().with_cache_tuning_history(
            CacheTuningHistorySummary::new(1800, 1800, true, 0, 0, 0, None),
        );
        let report = summary.to_report();

        assert!(report.contains("初始 TTL: 1800s"));
        assert!(report.contains("最终 TTL: 1800s"));
        // ttl_delta == 0 → 不显示 TTL 变化行
        assert!(!report.contains("TTL 变化:"));
    }

    #[test]
    fn test_to_report_cache_tuning_history_with_saved_at() {
        let summary =
            DevTraceSummary::empty().with_cache_tuning_history(CacheTuningHistorySummary::new(
                600,
                1800,
                true,
                3,
                0,
                3,
                Some("2024-06-01T00:00:00Z".to_string()),
            ));
        let report = summary.to_report();

        assert!(report.contains("TTL 变化: +1200s (200.0%)"));
        assert!(report.contains("保存时间: 2024-06-01T00:00:00Z"));
    }

    // --- to_report: 两个面板同时存在的测试 ---

    #[test]
    fn test_to_report_both_history_panels() {
        let summary = DevTraceSummary::empty()
            .with_search_quality_history(SearchQualityHistorySummary::new(true, false, 5, 1, None))
            .with_cache_tuning_history(CacheTuningHistorySummary::new(
                1800, 2700, true, 1, 0, 1, None,
            ));
        let report = summary.to_report();

        assert!(report.contains("搜索质量历史 (跨 Session)"));
        assert!(report.contains("缓存调优历史 (跨 Session)"));
        assert!(report.contains("初始状态: 启用"));
        assert!(report.contains("最终状态: 禁用"));
        assert!(report.contains("初始 TTL: 1800s"));
        assert!(report.contains("最终 TTL: 2700s"));
    }

    // --- serde 测试: DevTraceSummary 中的新字段 ---

    #[test]
    fn test_summary_sq_history_serde_roundtrip() {
        let summary = DevTraceSummary::empty()
            .with_search_quality_history(SearchQualityHistorySummary::new(true, false, 5, 1, None));
        let json = serde_json::to_string(&summary).unwrap();
        let loaded: DevTraceSummary = serde_json::from_str(&json).unwrap();
        let sqh = loaded
            .search_quality_history_summary
            .expect("should be Some");
        assert_eq!(sqh.evaluation_count, 5);
        assert_eq!(sqh.disable_count, 1);
        assert!(sqh.enabled_changed);
    }

    #[test]
    fn test_summary_sq_history_serde_skip_none() {
        let summary = DevTraceSummary::empty();
        let json = serde_json::to_string(&summary).unwrap();
        assert!(!json.contains("search_quality_history_summary"));
    }

    #[test]
    fn test_summary_ct_history_serde_roundtrip() {
        let summary = DevTraceSummary::empty().with_cache_tuning_history(
            CacheTuningHistorySummary::new(1800, 2700, true, 1, 0, 1, None),
        );
        let json = serde_json::to_string(&summary).unwrap();
        let loaded: DevTraceSummary = serde_json::from_str(&json).unwrap();
        let cth = loaded.cache_tuning_history_summary.expect("should be Some");
        assert_eq!(cth.initial_ttl, 1800);
        assert_eq!(cth.current_ttl, 2700);
        assert_eq!(cth.ttl_delta, 900);
    }

    #[test]
    fn test_summary_ct_history_serde_skip_none() {
        let summary = DevTraceSummary::empty();
        let json = serde_json::to_string(&summary).unwrap();
        assert!(!json.contains("cache_tuning_history_summary"));
    }

    // --- from_entries 不解析历史 (来自外部文件) ---

    #[test]
    fn test_from_entries_history_summaries_are_none() {
        let entries = vec![
            make_entry(TraceAction::TaskExecution, true),
            make_entry(TraceAction::CompileCheck, true),
        ];
        let summary = DevTraceSummary::from_entries(&entries);
        // from_entries 不应从 trace 条目解析历史摘要
        assert!(summary.search_quality_history_summary.is_none());
        assert!(summary.cache_tuning_history_summary.is_none());
    }

    // ===== JSON 导出测试 (Session 88) =====

    // --- to_json ---

    #[test]
    fn test_to_json_empty_summary() {
        let summary = DevTraceSummary::empty();
        let json = summary.to_json().unwrap();
        assert!(json.contains("\"total_entries\": 0"));
        assert!(json.contains("\"total_duration_ms\": 0"));
        assert!(json.contains("\"success_rate\": 0.0"));
    }

    #[test]
    fn test_to_json_with_entries() {
        let entries = vec![
            make_entry(TraceAction::TaskExecution, true),
            make_entry(TraceAction::CompileCheck, false),
        ];
        let summary = DevTraceSummary::from_entries(&entries);
        let json = summary.to_json().unwrap();
        assert!(json.contains("\"total_entries\": 2"));
        // 确保时间线被序列化
        assert!(json.contains("\"timeline\""));
        assert!(json.contains("\"by_action\""));
    }

    #[test]
    fn test_to_json_includes_optional_fields() {
        let summary = DevTraceSummary::empty()
            .with_search_quality_history(SearchQualityHistorySummary::new(true, false, 5, 1, None))
            .with_cache_tuning_history(CacheTuningHistorySummary::new(
                1800, 2700, true, 1, 0, 1, None,
            ));
        let json = summary.to_json().unwrap();
        assert!(json.contains("search_quality_history_summary"));
        assert!(json.contains("cache_tuning_history_summary"));
        assert!(json.contains("\"evaluation_count\": 5"));
        assert!(json.contains("\"initial_ttl\": 1800"));
    }

    #[test]
    fn test_to_json_skip_none_fields() {
        let summary = DevTraceSummary::empty();
        let json = summary.to_json().unwrap();
        // None 字段应被跳过
        assert!(!json.contains("incremental_summary"));
        assert!(!json.contains("cache_summary"));
        assert!(!json.contains("cache_fix_correlation"));
        assert!(!json.contains("cache_tuning_summary"));
        assert!(!json.contains("search_quality_summary"));
        assert!(!json.contains("search_quality_history_summary"));
        assert!(!json.contains("cache_tuning_history_summary"));
    }

    // --- to_json_compact ---

    #[test]
    fn test_to_json_compact_empty() {
        let summary = DevTraceSummary::empty();
        let json = summary.to_json_compact().unwrap();
        assert!(json.contains("\"total_entries\":0"));
        // compact 格式不应有缩进
        assert!(!json.contains("\n"));
    }

    #[test]
    fn test_to_json_compact_vs_pretty() {
        let summary = DevTraceSummary::empty();
        let compact = summary.to_json_compact().unwrap();
        let pretty = summary.to_json().unwrap();
        // pretty 有换行, compact 没有
        assert!(pretty.contains('\n'));
        assert!(!compact.contains('\n'));
        // 两者反序列化后应相等
        let from_compact: DevTraceSummary = serde_json::from_str(&compact).unwrap();
        let from_pretty: DevTraceSummary = serde_json::from_str(&pretty).unwrap();
        assert_eq!(from_compact.total_entries, from_pretty.total_entries);
    }

    // --- to_json_with_meta ---

    #[test]
    fn test_to_json_with_meta_basic() {
        let summary = DevTraceSummary::empty();
        let json = summary
            .to_json_with_meta("2024-06-01T00:00:00+00:00")
            .unwrap();
        assert!(json.contains("\"meta\""));
        assert!(json.contains("\"summary\""));
        assert!(json.contains("\"exported_at\": \"2024-06-01T00:00:00+00:00\""));
        assert!(json.contains("\"format_version\": \"1.0\""));
        assert!(json.contains("\"forge_version\""));
    }

    #[test]
    fn test_to_json_with_meta_roundtrip() {
        let summary =
            DevTraceSummary::empty().with_search_quality_history(SearchQualityHistorySummary::new(
                true,
                false,
                10,
                2,
                Some("2024-06-01T00:00:00+00:00".to_string()),
            ));
        let json = summary
            .to_json_with_meta("2024-06-02T00:00:00+00:00")
            .unwrap();
        let export: DevTraceJsonExport = serde_json::from_str(&json).unwrap();
        assert_eq!(export.meta.exported_at, "2024-06-02T00:00:00+00:00");
        assert_eq!(export.meta.format_version, "1.0");
        assert_eq!(export.summary.total_entries, 0);
        let sqh = export
            .summary
            .search_quality_history_summary
            .expect("should be Some");
        assert_eq!(sqh.evaluation_count, 10);
        assert_eq!(sqh.disable_count, 2);
    }

    // --- save_to_json_file ---

    #[test]
    fn test_save_to_json_file_basic() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("summary.json");
        let summary = DevTraceSummary::empty();
        summary.save_to_json_file(&path).unwrap();
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("\"total_entries\": 0"));
    }

    #[test]
    fn test_save_to_json_file_with_entries() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("trace_summary.json");
        let entries = vec![
            make_entry(TraceAction::TaskExecution, true),
            make_entry(TraceAction::CompileCheck, true),
        ];
        let summary = DevTraceSummary::from_entries(&entries);
        summary.save_to_json_file(&path).unwrap();
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("\"total_entries\": 2"));
        // 反序列化验证
        let loaded: DevTraceSummary = serde_json::from_str(&content).unwrap();
        assert_eq!(loaded.total_entries, 2);
    }

    #[test]
    fn test_save_to_json_file_overwrite() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("summary.json");
        // 第一次写入
        let summary1 = DevTraceSummary::empty();
        summary1.save_to_json_file(&path).unwrap();
        // 第二次写入 (覆盖)
        let entries = vec![
            make_entry(TraceAction::TaskExecution, true),
            make_entry(TraceAction::CompileCheck, true),
            make_entry(TraceAction::TestRun, false),
        ];
        let summary2 = DevTraceSummary::from_entries(&entries);
        summary2.save_to_json_file(&path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("\"total_entries\": 3"));
    }

    // --- save_to_json_file_with_meta ---

    #[test]
    fn test_save_to_json_file_with_meta_basic() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("summary.json");
        let summary = DevTraceSummary::empty();
        summary
            .save_to_json_file_with_meta(&path, "2024-06-01T00:00:00+00:00")
            .unwrap();
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("\"meta\""));
        assert!(content.contains("\"exported_at\": \"2024-06-01T00:00:00+00:00\""));
        assert!(content.contains("\"format_version\": \"1.0\""));
    }

    #[test]
    fn test_save_to_json_file_with_meta_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("export.json");
        let summary = DevTraceSummary::empty().with_cache_tuning_history(
            CacheTuningHistorySummary::new(600, 1800, true, 2, 0, 2, None),
        );
        summary
            .save_to_json_file_with_meta(&path, "2024-06-01T12:00:00+00:00")
            .unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let export: DevTraceJsonExport = serde_json::from_str(&content).unwrap();
        assert_eq!(export.meta.exported_at, "2024-06-01T12:00:00+00:00");
        assert_eq!(export.meta.format_version, "1.0");
        let cth = export
            .summary
            .cache_tuning_history_summary
            .expect("should be Some");
        assert_eq!(cth.initial_ttl, 600);
        assert_eq!(cth.current_ttl, 1800);
    }

    // --- DevTraceExportMeta ---

    #[test]
    fn test_export_meta_creation() {
        let meta = DevTraceExportMeta {
            exported_at: "2024-06-01T00:00:00+00:00".to_string(),
            forge_version: "0.1.0".to_string(),
            format_version: "1.0".to_string(),
        };
        assert_eq!(meta.exported_at, "2024-06-01T00:00:00+00:00");
        assert_eq!(meta.forge_version, "0.1.0");
        assert_eq!(meta.format_version, "1.0");
    }

    #[test]
    fn test_export_meta_serde_roundtrip() {
        let meta = DevTraceExportMeta {
            exported_at: "2024-06-01T00:00:00+00:00".to_string(),
            forge_version: "0.1.0".to_string(),
            format_version: "1.0".to_string(),
        };
        let json = serde_json::to_string(&meta).unwrap();
        let loaded: DevTraceExportMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.exported_at, meta.exported_at);
        assert_eq!(loaded.forge_version, meta.forge_version);
        assert_eq!(loaded.format_version, meta.format_version);
    }

    // --- DevTraceJsonExport ---

    #[test]
    fn test_json_export_creation() {
        let summary = DevTraceSummary::empty();
        let export = DevTraceJsonExport {
            meta: DevTraceExportMeta {
                exported_at: "2024-06-01T00:00:00+00:00".to_string(),
                forge_version: "0.1.0".to_string(),
                format_version: "1.0".to_string(),
            },
            summary,
        };
        assert_eq!(export.meta.format_version, "1.0");
        assert_eq!(export.summary.total_entries, 0);
    }

    #[test]
    fn test_json_export_serde_roundtrip() {
        let summary = DevTraceSummary::empty()
            .with_search_quality_history(SearchQualityHistorySummary::new(true, true, 3, 0, None));
        let export = DevTraceJsonExport {
            meta: DevTraceExportMeta {
                exported_at: "2024-06-01T00:00:00+00:00".to_string(),
                forge_version: "0.1.0".to_string(),
                format_version: "1.0".to_string(),
            },
            summary,
        };
        let json = serde_json::to_string_pretty(&export).unwrap();
        let loaded: DevTraceJsonExport = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.meta.exported_at, "2024-06-01T00:00:00+00:00");
        assert_eq!(loaded.summary.total_entries, 0);
        let sqh = loaded
            .summary
            .search_quality_history_summary
            .expect("should be Some");
        assert_eq!(sqh.evaluation_count, 3);
    }

    // --- build_export_timestamp ---

    #[test]
    fn test_build_export_timestamp_format() {
        let ts = build_export_timestamp();
        // RFC 3339 格式应包含 'T' 和时区偏移
        assert!(ts.contains('T'));
        // 应包含 +00:00 或 Z (UTC)
        assert!(ts.contains("+00:00") || ts.contains('Z'));
    }

    #[test]
    fn test_build_export_timestamp_unique() {
        let ts1 = build_export_timestamp();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let ts2 = build_export_timestamp();
        // 两次调用应返回不同的时间戳 (极小概率相同)
        assert_ne!(ts1, ts2);
    }

    // --- build_dev_trace_json_export ---

    #[test]
    fn test_build_dev_trace_json_export_basic() {
        let summary = DevTraceSummary::empty();
        let export = build_dev_trace_json_export(summary, "2024-06-01T00:00:00+00:00".to_string());
        assert_eq!(export.meta.exported_at, "2024-06-01T00:00:00+00:00");
        assert_eq!(export.meta.format_version, "1.0");
        assert!(!export.meta.forge_version.is_empty());
        assert_eq!(export.summary.total_entries, 0);
    }

    #[test]
    fn test_build_dev_trace_json_export_with_data() {
        let entries = vec![
            make_entry(TraceAction::TaskExecution, true),
            make_entry(TraceAction::CompileCheck, false),
            make_entry(TraceAction::TestRun, true),
        ];
        let summary = DevTraceSummary::from_entries(&entries).with_cache_tuning_history(
            CacheTuningHistorySummary::new(1800, 900, false, 1, 1, 2, None),
        );
        let export = build_dev_trace_json_export(summary, "2024-06-01T12:00:00+00:00".to_string());
        assert_eq!(export.summary.total_entries, 3);
        let cth = export
            .summary
            .cache_tuning_history_summary
            .expect("should be Some");
        assert!(!cth.enabled);
        assert_eq!(cth.ttl_delta, -900);
    }

    #[test]
    fn test_build_dev_trace_json_export_serde() {
        let summary = DevTraceSummary::empty();
        let export = build_dev_trace_json_export(summary, "2024-06-01T00:00:00+00:00".to_string());
        let json = serde_json::to_string(&export).unwrap();
        let loaded: DevTraceJsonExport = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.meta.exported_at, "2024-06-01T00:00:00+00:00");
        assert_eq!(loaded.meta.format_version, "1.0");
        assert_eq!(loaded.summary.total_entries, 0);
    }

    // --- JSON 完整性验证 ---

    #[test]
    fn test_json_full_roundtrip_all_fields() {
        let entries = vec![
            make_entry(TraceAction::TaskExecution, true),
            make_entry(TraceAction::CompileCheck, false),
            DevTraceEntry::new(
                TraceAction::IncrementalSend,
                Some(0),
                Some(0),
                Some("增量发送"),
                "total=10 sent=3 skipped=7",
                "发送成功",
                500,
                true,
                None,
            ),
        ];
        let summary = DevTraceSummary::from_entries(&entries)
            .with_search_quality_history(SearchQualityHistorySummary::new(
                true,
                false,
                5,
                1,
                Some("2024-06-01T00:00:00+00:00".to_string()),
            ))
            .with_cache_tuning_history(CacheTuningHistorySummary::new(
                1800,
                2700,
                true,
                2,
                0,
                2,
                Some("2024-06-01T00:00:00+00:00".to_string()),
            ));

        // 序列化 → 反序列化 → 验证
        let json = summary.to_json().unwrap();
        let loaded: DevTraceSummary = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.total_entries, 3);
        assert!(loaded.incremental_summary.is_some());
        assert!(loaded.search_quality_history_summary.is_some());
        assert!(loaded.cache_tuning_history_summary.is_some());

        // 验证增量发送统计
        let inc = loaded.incremental_summary.expect("should be Some");
        assert_eq!(inc.send_count, 1);
        assert_eq!(inc.total_messages, 10);
        assert_eq!(inc.sent_messages, 3);

        let sqh = loaded
            .search_quality_history_summary
            .expect("should be Some");
        assert_eq!(sqh.evaluation_count, 5);
        assert!(sqh.enabled_changed);

        let cth = loaded.cache_tuning_history_summary.expect("should be Some");
        assert_eq!(cth.ttl_delta, 900);
        assert_eq!(cth.decision_count, 2);
    }

    #[test]
    fn test_json_meta_export_preserves_timeline() {
        let entries: Vec<DevTraceEntry> = (0..10)
            .map(|i| {
                DevTraceEntry::new(
                    TraceAction::TaskExecution,
                    Some(0),
                    Some(i),
                    Some("task"),
                    "input",
                    "output",
                    500,
                    true,
                    None,
                )
            })
            .collect();
        let summary = DevTraceSummary::from_entries(&entries);
        let export = build_dev_trace_json_export(summary, "2024-06-01T00:00:00+00:00".to_string());
        let json = serde_json::to_string_pretty(&export).unwrap();
        let loaded: DevTraceJsonExport = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.summary.timeline.len(), 10);
        assert_eq!(loaded.summary.total_entries, 10);
    }
}
