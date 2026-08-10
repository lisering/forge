//! 三评估器协同分析 — CacheTuner + SearchQualityEvaluator + MemoryContextEvaluator
//!
//! 当三个评估器同时启用时, 它们的决策会相互影响:
//! - CacheTuner 禁用缓存 → SearchQualityEvaluator 的搜索次数减少
//! - SearchQualityEvaluator 禁用搜索 → CacheTuner 无数据可评估
//! - MemoryContextEvaluator 禁用注入 → 影响整体修复成功率
//!
//! 本模块提供协同分析摘要, 展示三个评估器的交互影响和整体效果。
//!
//! ## 核心数据结构
//!
//! - [`EvaluatorType`] — 评估器类型枚举
//! - [`EvaluatorState`] — 单个评估器的输入状态
//! - [`EvaluatorSnapshot`] — 单个评估器的状态快照
//! - [`EvaluatorTimelineAction`] — 决策时间线动作类型
//! - [`EvaluatorTimelineEntry`] — 决策时间线条目
//! - [`EvaluatorSynergySummary`] — 协同分析摘要
//!
//! ## 纯函数
//!
//! - [`parse_evaluator_timeline_action`] — 解析 DevTrace 条目的输出摘要为时间线动作
//! - [`build_evaluator_timeline`] — 从 DevTrace 条目构建评估器决策时间线
//! - [`compute_synergy_score`] — 计算协同评分 (0.0~1.0)
//! - [`build_evaluator_synergy_summary`] — 构建三评估器协同分析摘要

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::dev_trace::{DevTraceEntry, TraceAction};

// ============================================================================
//  EvaluatorType — 评估器类型
// ============================================================================

/// 评估器类型 — 标识三种自动评估器
///
/// 用于在协同分析中区分不同评估器的决策和状态。
///
/// # 变体
///
/// - `CacheTuner` — 缓存策略调优器 (Session 81)
/// - `SearchQuality` — 搜索质量评估器 (Session 85)
/// - `MemoryContext` — Memory 上下文注入评估器 (Session 90)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EvaluatorType {
    /// 缓存策略调优器
    CacheTuner,
    /// 搜索质量评估器
    SearchQuality,
    /// Memory 上下文注入评估器
    MemoryContext,
}

impl std::fmt::Display for EvaluatorType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CacheTuner => write!(f, "CacheTuner"),
            Self::SearchQuality => write!(f, "SearchQuality"),
            Self::MemoryContext => write!(f, "MemoryContext"),
        }
    }
}

impl EvaluatorType {
    /// 获取评估器的中文名称
    pub fn label(&self) -> &'static str {
        match self {
            Self::CacheTuner => "缓存调优",
            Self::SearchQuality => "搜索质量",
            Self::MemoryContext => "Memory 注入",
        }
    }

    /// 所有评估器类型
    pub fn all() -> Vec<EvaluatorType> {
        vec![Self::CacheTuner, Self::SearchQuality, Self::MemoryContext]
    }
}

// ============================================================================
//  EvaluatorState — 单个评估器的输入状态
// ============================================================================

/// 单个评估器的输入状态 — 传递给协同分析构建函数
///
/// 由调用方 (Orchestrator) 从评估器实例和 DevTrace 统计中提取。
///
/// # 字段
///
/// - `evaluator_type`: 评估器类型
/// - `enabled`: 当前是否启用
/// - `with_fix_rate`: 有功能时的修复成功率 (0.0~1.0)
/// - `without_fix_rate`: 无功能时的修复成功率 (0.0~1.0)
/// - `diff`: 差值 (with - without), 正=有效, 负=有害
/// - `is_beneficial`: 功能是否有效
/// - `total_checks`: 总编译检查次数
/// - `evaluation_count`: 评估次数
/// - `disable_count`: 禁用次数
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluatorState {
    /// 评估器类型
    pub evaluator_type: EvaluatorType,
    /// 当前是否启用
    pub enabled: bool,
    /// 有功能时的修复成功率 (0.0~1.0)
    pub with_fix_rate: f64,
    /// 无功能时的修复成功率 (0.0~1.0)
    pub without_fix_rate: f64,
    /// 差值 (with - without), 正=有效, 负=有害
    pub diff: f64,
    /// 功能是否有效
    pub is_beneficial: bool,
    /// 总编译检查次数
    pub total_checks: usize,
    /// 评估次数
    pub evaluation_count: usize,
    /// 禁用次数
    pub disable_count: usize,
}

impl EvaluatorState {
    /// 创建新的评估器状态
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        evaluator_type: EvaluatorType,
        enabled: bool,
        with_fix_rate: f64,
        without_fix_rate: f64,
        diff: f64,
        is_beneficial: bool,
        total_checks: usize,
        evaluation_count: usize,
        disable_count: usize,
    ) -> Self {
        Self {
            evaluator_type,
            enabled,
            with_fix_rate,
            without_fix_rate,
            diff,
            is_beneficial,
            total_checks,
            evaluation_count,
            disable_count,
        }
    }

    /// 是否有足够的评估数据
    pub fn has_data(&self) -> bool {
        self.total_checks > 0
    }
}

// ============================================================================
//  EvaluatorSnapshot — 评估器状态快照
// ============================================================================

/// 单个评估器的状态快照 — 协同分析摘要中的输出
///
/// 从 [`EvaluatorState`] 构建, 附加协同分析计算结果。
///
/// # 字段
///
/// - `evaluator_type`: 评估器类型
/// - `enabled`: 当前是否启用
/// - `with_fix_rate`: 有功能时的修复成功率
/// - `without_fix_rate`: 无功能时的修复成功率
/// - `diff`: 差值 (正=有效, 负=有害)
/// - `is_beneficial`: 功能是否有效
/// - `total_checks`: 总编译检查次数
/// - `evaluation_count`: 评估次数
/// - `disable_count`: 禁用次数
/// - `contribution_score`: 贡献评分 (0.0~1.0)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluatorSnapshot {
    /// 评估器类型
    pub evaluator_type: EvaluatorType,
    /// 当前是否启用
    pub enabled: bool,
    /// 有功能时的修复成功率 (0.0~1.0)
    pub with_fix_rate: f64,
    /// 无功能时的修复成功率 (0.0~1.0)
    pub without_fix_rate: f64,
    /// 差值 (正=有效, 负=有害)
    pub diff: f64,
    /// 功能是否有效
    pub is_beneficial: bool,
    /// 总编译检查次数
    pub total_checks: usize,
    /// 评估次数
    pub evaluation_count: usize,
    /// 禁用次数
    pub disable_count: usize,
    /// 贡献评分 (0.0~1.0, 基于差值和评估次数)
    pub contribution_score: f64,
}

impl EvaluatorSnapshot {
    /// 从评估器状态构建快照
    pub fn from_state(state: &EvaluatorState) -> Self {
        let contribution_score = compute_contribution_score(state);
        Self {
            evaluator_type: state.evaluator_type,
            enabled: state.enabled,
            with_fix_rate: state.with_fix_rate,
            without_fix_rate: state.without_fix_rate,
            diff: state.diff,
            is_beneficial: state.is_beneficial,
            total_checks: state.total_checks,
            evaluation_count: state.evaluation_count,
            disable_count: state.disable_count,
            contribution_score,
        }
    }

    /// 是否有评估数据
    pub fn has_data(&self) -> bool {
        self.total_checks > 0
    }

    /// 是否已禁用功能
    pub fn is_disabled(&self) -> bool {
        self.disable_count > 0 && !self.enabled
    }

    /// 格式化为简要摘要
    pub fn to_summary(&self) -> String {
        format!(
            "{}: {} (差值 {:+.1}%, 评估 {} 次, 禁用 {} 次, 贡献 {:.0}%)",
            self.evaluator_type.label(),
            if self.enabled { "启用" } else { "已禁用" },
            self.diff * 100.0,
            self.evaluation_count,
            self.disable_count,
            self.contribution_score * 100.0,
        )
    }
}

/// 计算单个评估器的贡献评分 (0.0~1.0)
///
/// 贡献评分基于:
/// - 差值 (正差值=功能有效, 负差值=功能有害)
/// - 评估次数 (更多评估=更可靠)
///
/// # 参数
///
/// - `state`: 评估器状态
///
/// # 返回
///
/// - 无数据时返回 0.0
/// - 功能有效且差值高 → 接近 1.0
/// - 功能无效且差值低 → 接近 0.0
fn compute_contribution_score(state: &EvaluatorState) -> f64 {
    if !state.has_data() {
        return 0.0;
    }
    // 基础分 = 差值映射到 0.0~1.0 (差值 -1.0 → 0.0, 差值 +1.0 → 1.0)
    let base = (state.diff + 1.0) / 2.0;
    // 评估次数加成 (最多 +0.1)
    let eval_bonus = (state.evaluation_count as f64 / 100.0).min(0.1);
    (base + eval_bonus).clamp(0.0, 1.0)
}

// ============================================================================
//  EvaluatorTimelineAction — 决策时间线动作类型
// ============================================================================

/// 评估器决策时间线动作类型 — 所有可能的三评估器决策动作
///
/// 每个动作对应一个评估器的决策结果。
///
/// # 变体
///
/// ## CacheTuner
/// - `KeepCurrent` — 保持当前缓存配置
/// - `AdjustTtl` — 调整 TTL
/// - `DisableCache` — 禁用缓存
///
/// ## SearchQuality
/// - `KeepSearching` — 保持搜索
/// - `DisableSearch` — 禁用搜索
/// - `InsufficientSearchData` — 数据不足
///
/// ## MemoryContext
/// - `KeepInjecting` — 继续注入
/// - `DisableInjection` — 禁用注入
/// - `InsufficientMemoryData` — 数据不足
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EvaluatorTimelineAction {
    /// CacheTuner: 保持当前配置
    KeepCurrent,
    /// CacheTuner: 调整 TTL
    AdjustTtl,
    /// CacheTuner: 禁用缓存
    DisableCache,
    /// SearchQuality: 保持搜索
    KeepSearching,
    /// SearchQuality: 禁用搜索
    DisableSearch,
    /// SearchQuality: 数据不足
    InsufficientSearchData,
    /// MemoryContext: 继续注入
    KeepInjecting,
    /// MemoryContext: 禁用注入
    DisableInjection,
    /// MemoryContext: 数据不足
    InsufficientMemoryData,
}

impl EvaluatorTimelineAction {
    /// 获取所属的评估器类型
    pub fn evaluator_type(&self) -> EvaluatorType {
        match self {
            Self::KeepCurrent | Self::AdjustTtl | Self::DisableCache => EvaluatorType::CacheTuner,
            Self::KeepSearching | Self::DisableSearch | Self::InsufficientSearchData => {
                EvaluatorType::SearchQuality
            }
            Self::KeepInjecting | Self::DisableInjection | Self::InsufficientMemoryData => {
                EvaluatorType::MemoryContext
            }
        }
    }

    /// 是否为禁用动作
    pub fn is_disable(&self) -> bool {
        matches!(
            self,
            Self::DisableCache | Self::DisableSearch | Self::DisableInjection
        )
    }

    /// 获取中文描述
    pub fn label(&self) -> &'static str {
        match self {
            Self::KeepCurrent => "保持当前配置",
            Self::AdjustTtl => "调整 TTL",
            Self::DisableCache => "禁用缓存",
            Self::KeepSearching => "保持搜索",
            Self::DisableSearch => "禁用搜索",
            Self::InsufficientSearchData => "数据不足",
            Self::KeepInjecting => "继续注入",
            Self::DisableInjection => "禁用注入",
            Self::InsufficientMemoryData => "数据不足",
        }
    }
}

impl std::fmt::Display for EvaluatorTimelineAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

// ============================================================================
//  EvaluatorTimelineEntry — 决策时间线条目
// ============================================================================

/// 评估器决策时间线条目 — 单次评估器决策的记录
///
/// 从 DevTrace 条目中的 `CacheTuning`, `SearchQuality`, `MemoryEvaluation` 动作解析。
///
/// # 字段
///
/// - `timestamp`: 时间戳
/// - `evaluator_type`: 评估器类型
/// - `action`: 决策动作
/// - `diff`: 修复成功率差值
/// - `phase_idx`: 阶段索引
/// - `task_idx`: 任务索引
/// - `task_name`: 任务名称
/// - `reason`: 决策原因
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluatorTimelineEntry {
    /// 时间戳 (UTC)
    pub timestamp: DateTime<Utc>,
    /// 评估器类型
    pub evaluator_type: EvaluatorType,
    /// 决策动作
    pub action: EvaluatorTimelineAction,
    /// 修复成功率差值 (with - without)
    pub diff: f64,
    /// 阶段索引
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase_idx: Option<usize>,
    /// 任务索引
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_idx: Option<usize>,
    /// 任务名称
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_name: Option<String>,
    /// 决策原因
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

// ============================================================================
//  EvaluatorSynergySummary — 协同分析摘要
// ============================================================================

/// 三评估器协同分析摘要 — 跨评估器的综合分析
///
/// 展示 CacheTuner + SearchQualityEvaluator + MemoryContextEvaluator
/// 的交互影响和整体效果。
///
/// # 字段
///
/// - `active_evaluators`: 活跃评估器数量 (0~3)
/// - `snapshots`: 各评估器状态快照
/// - `timeline`: 决策时间线 (按时间排序)
/// - `total_decisions`: 总决策数
/// - `total_disables`: 总禁用数
/// - `overall_fix_rate`: 总体修复率
/// - `synergy_score`: 协同评分 (0.0~1.0)
/// - `any_disabled`: 是否有评估器禁用了功能
/// - `all_beneficial`: 是否所有评估器都判定功能有效
///
/// # 示例
///
/// ```
/// # use forge::evaluator_synergy::{
/// #     EvaluatorState, EvaluatorType, build_evaluator_synergy_summary,
/// # };
/// # use forge::dev_trace::DevTraceEntry;
/// let states = vec![
///     EvaluatorState::new(
///         EvaluatorType::CacheTuner, true,
///         0.8, 0.6, 0.2, true, 10, 3, 0,
///     ),
///     EvaluatorState::new(
///         EvaluatorType::SearchQuality, true,
///         0.7, 0.5, 0.2, true, 10, 3, 0,
///     ),
/// ];
/// let summary = build_evaluator_synergy_summary(&states, 20, 15, &[]);
/// assert_eq!(summary.active_evaluators, 2);
/// assert!(!summary.any_disabled);
/// assert!(summary.all_beneficial);
/// assert!(summary.synergy_score > 0.0 && summary.synergy_score <= 1.0);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluatorSynergySummary {
    /// 活跃评估器数量 (0~3)
    pub active_evaluators: usize,
    /// 各评估器状态快照
    pub snapshots: Vec<EvaluatorSnapshot>,
    /// 决策时间线 (按时间排序)
    pub timeline: Vec<EvaluatorTimelineEntry>,
    /// 总决策数 (三个评估器合计)
    pub total_decisions: usize,
    /// 总禁用数 (三个评估器合计)
    pub total_disables: usize,
    /// 总体修复率 (所有编译检查的成功率, 0.0~1.0)
    pub overall_fix_rate: f64,
    /// 协同评分 (0.0~1.0)
    pub synergy_score: f64,
    /// 是否有评估器禁用了功能
    pub any_disabled: bool,
    /// 是否所有有数据的评估器都判定功能有效
    pub all_beneficial: bool,
}

impl EvaluatorSynergySummary {
    /// 创建空的协同分析摘要
    pub fn empty() -> Self {
        Self {
            active_evaluators: 0,
            snapshots: vec![],
            timeline: vec![],
            total_decisions: 0,
            total_disables: 0,
            overall_fix_rate: 0.0,
            synergy_score: 0.0,
            any_disabled: false,
            all_beneficial: true,
        }
    }

    /// 是否为空 (无评估器数据)
    pub fn is_empty(&self) -> bool {
        self.active_evaluators == 0
    }

    /// 是否有决策记录
    pub fn has_decisions(&self) -> bool {
        self.total_decisions > 0
    }

    /// 格式化为简要摘要
    pub fn to_summary(&self) -> String {
        format!(
            "协同分析: {} 个评估器活跃, {} 个决策, {} 个禁用, \
             协同评分 {:.0}%, 总体修复率 {:.1}%",
            self.active_evaluators,
            self.total_decisions,
            self.total_disables,
            self.synergy_score * 100.0,
            self.overall_fix_rate * 100.0,
        )
    }
}

impl Default for EvaluatorSynergySummary {
    fn default() -> Self {
        Self::empty()
    }
}

// ============================================================================
//  纯函数 — 解析与构建
// ============================================================================

/// 从 DevTrace 条目解析评估器时间线动作
///
/// 根据 `TraceAction` 类型和 `output_summary` 内容判断具体的评估器决策。
///
/// # 参数
///
/// - `entry`: DevTrace 条目
///
/// # 返回
///
/// - `Some((action, diff))`: 成功解析出动作和差值
/// - `None`: 该条目不是评估器决策条目
///
/// # 解析规则
///
/// - `TraceAction::CacheTuning` → 解析 "保持当前配置" / "调整 TTL" / "禁用缓存"
/// - `TraceAction::SearchQuality` → 解析 "保持搜索" / "禁用搜索" / "数据不足"
/// - `TraceAction::MemoryEvaluation` → 解析 "KeepInjecting" / "DisableInjection" / "InsufficientData"
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::{DevTraceEntry, TraceAction};
/// # use forge::evaluator_synergy::{parse_evaluator_timeline_action, EvaluatorTimelineAction};
/// let entry = DevTraceEntry::new(
///     TraceAction::CacheTuning, Some(0), Some(0), Some("task"),
///     "hit=2/3 miss=3/3",
///     "缓存调优: 保持当前配置 (差值 +10.0%, 原因: ...)",
///     0, true, None,
/// );
/// let (action, diff) = parse_evaluator_timeline_action(&entry).unwrap();
/// assert_eq!(action, EvaluatorTimelineAction::KeepCurrent);
/// assert!((diff - 0.1).abs() < 0.001);
/// ```
pub fn parse_evaluator_timeline_action(
    entry: &DevTraceEntry,
) -> Option<(EvaluatorTimelineAction, f64)> {
    let diff = parse_diff_value(&entry.output_summary);
    match entry.action {
        TraceAction::CacheTuning => {
            if entry.output_summary.contains("禁用缓存") {
                Some((EvaluatorTimelineAction::DisableCache, diff))
            } else if entry.output_summary.contains("调整 TTL") {
                Some((EvaluatorTimelineAction::AdjustTtl, diff))
            } else {
                Some((EvaluatorTimelineAction::KeepCurrent, diff))
            }
        }
        TraceAction::SearchQuality => {
            if entry.output_summary.contains("禁用搜索") {
                Some((EvaluatorTimelineAction::DisableSearch, diff))
            } else if entry.output_summary.contains("数据不足") {
                Some((EvaluatorTimelineAction::InsufficientSearchData, diff))
            } else {
                Some((EvaluatorTimelineAction::KeepSearching, diff))
            }
        }
        TraceAction::MemoryEvaluation => {
            if entry.output_summary.contains("DisableInjection") {
                Some((EvaluatorTimelineAction::DisableInjection, diff))
            } else if entry.output_summary.contains("InsufficientData") {
                Some((EvaluatorTimelineAction::InsufficientMemoryData, diff))
            } else {
                Some((EvaluatorTimelineAction::KeepInjecting, diff))
            }
        }
        _ => None,
    }
}

/// 从输出摘要中解析差值
///
/// 格式: "差值 {:+.1}%"
///
/// # 参数
///
/// - `output`: 输出摘要文本
///
/// # 返回
///
/// 解析出的差值 (如 "差值 +10.0%" → 0.1), 解析失败返回 0.0
///
/// # 示例
///
/// ```
/// # use forge::evaluator_synergy::parse_diff_value;
/// assert!((parse_diff_value("差值 +10.0%") - 0.1).abs() < 0.001);
/// assert!((parse_diff_value("差值 -5.0%") - (-0.05)).abs() < 0.001);
/// assert!((parse_diff_value("无差值") - 0.0).abs() < 0.001);
/// ```
pub fn parse_diff_value(output: &str) -> f64 {
    let marker = "差值 ";
    // 查找 "差值 " 后的数字
    if let Some(pos) = output.find(marker) {
        let rest = &output[pos + marker.len()..];
        // 查找 '%' 前的部分
        if let Some(end) = rest.find('%') {
            let num_str = rest[..end].trim();
            if let Ok(v) = num_str.parse::<f64>() {
                return v / 100.0;
            }
        }
    }
    0.0
}

/// 从 DevTrace 条目列表构建评估器决策时间线
///
/// 遍历所有条目, 提取 `CacheTuning`, `SearchQuality`, `MemoryEvaluation` 动作,
/// 解析每条决策的动作和差值, 按时间排序。
///
/// # 参数
///
/// - `entries`: DevTrace 条目列表
///
/// # 返回
///
/// 按时间排序的评估器决策时间线
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::{DevTraceEntry, TraceAction};
/// # use forge::evaluator_synergy::{build_evaluator_timeline, EvaluatorTimelineAction};
/// let entries = vec![
///     DevTraceEntry::new(
///         TraceAction::CacheTuning, Some(0), Some(0), Some("t1"),
///         "hit=2/3", "缓存调优: 调整 TTL (差值 +20.0%, 原因: 有效)",
///         0, true, None,
///     ),
///     DevTraceEntry::new(
///         TraceAction::SearchQuality, Some(0), Some(0), Some("t1"),
///         "with=2/3", "搜索质量: 保持搜索 (差值 +10.0%, 原因: 有效)",
///         0, true, None,
///     ),
/// ];
/// let timeline = build_evaluator_timeline(&entries);
/// assert_eq!(timeline.len(), 2);
/// assert_eq!(timeline[0].action, EvaluatorTimelineAction::AdjustTtl);
/// assert_eq!(timeline[1].action, EvaluatorTimelineAction::KeepSearching);
/// ```
pub fn build_evaluator_timeline(entries: &[DevTraceEntry]) -> Vec<EvaluatorTimelineEntry> {
    let mut timeline: Vec<EvaluatorTimelineEntry> = entries
        .iter()
        .filter_map(|entry| {
            parse_evaluator_timeline_action(entry).map(|(action, diff)| EvaluatorTimelineEntry {
                timestamp: entry.timestamp,
                evaluator_type: action.evaluator_type(),
                action,
                diff,
                phase_idx: entry.phase_idx,
                task_idx: entry.task_idx,
                task_name: entry.task_name.clone(),
                reason: entry.error.clone(),
            })
        })
        .collect();

    // 按时间戳排序
    timeline.sort_by_key(|a| a.timestamp);
    timeline
}

/// 计算协同评分 (0.0~1.0)
///
/// 协同评分基于:
/// 1. 各评估器的贡献评分平均值 (差值越正、评估次数越多 → 分越高)
/// 2. 禁用惩罚 (有评估器禁用功能 → 降低评分)
/// 3. 全部有效奖励 (所有评估器都判定功能有效 → 提升评分)
///
/// # 参数
///
/// - `snapshots`: 各评估器状态快照
/// - `any_disabled`: 是否有评估器禁用了功能
/// - `all_beneficial`: 是否所有评估器都判定功能有效
///
/// # 返回
///
/// - 无评估器 → 0.0
/// - 有评估器 → 平均贡献评分 * 惩罚/奖励系数
///
/// # 示例
///
/// ```
/// # use forge::evaluator_synergy::{compute_synergy_score, EvaluatorSnapshot, EvaluatorType};
/// let snapshots = vec![
///     EvaluatorSnapshot {
///         evaluator_type: EvaluatorType::CacheTuner,
///         enabled: true, with_fix_rate: 0.8, without_fix_rate: 0.6,
///         diff: 0.2, is_beneficial: true, total_checks: 10,
///         evaluation_count: 3, disable_count: 0, contribution_score: 0.6,
///     },
/// ];
/// let score = compute_synergy_score(&snapshots, false, true);
/// assert!((score - 0.66).abs() < 0.01); // 0.6 * 1.1 = 0.66
/// ```
pub fn compute_synergy_score(
    snapshots: &[EvaluatorSnapshot],
    any_disabled: bool,
    all_beneficial: bool,
) -> f64 {
    if snapshots.is_empty() {
        return 0.0;
    }

    let avg_contribution: f64 =
        snapshots.iter().map(|s| s.contribution_score).sum::<f64>() / snapshots.len() as f64;

    let mut score = avg_contribution;

    if any_disabled {
        score *= 0.8;
    }

    if all_beneficial {
        score *= 1.1;
    }

    score.clamp(0.0, 1.0)
}

/// 构建三评估器协同分析摘要
///
/// 从评估器状态列表和 DevTrace 条目构建完整的协同分析摘要。
///
/// # 参数
///
/// - `states`: 各评估器的状态 (只有有数据的评估器应传入)
/// - `total_compile_checks`: 总编译检查次数
/// - `total_compile_successes`: 总编译通过次数
/// - `entries`: DevTrace 条目列表 (用于构建时间线)
///
/// # 返回
///
/// 三评估器协同分析摘要
///
/// # 示例
///
/// ```
/// # use forge::evaluator_synergy::{
/// #     EvaluatorState, EvaluatorType, build_evaluator_synergy_summary,
/// # };
/// let states = vec![
///     EvaluatorState::new(
///         EvaluatorType::CacheTuner, true,
///         0.8, 0.6, 0.2, true, 10, 3, 0,
///     ),
///     EvaluatorState::new(
///         EvaluatorType::SearchQuality, false,
///         0.3, 0.7, -0.4, false, 10, 2, 1,
///     ),
/// ];
/// let summary = build_evaluator_synergy_summary(&states, 20, 12, &[]);
/// assert_eq!(summary.active_evaluators, 2);
/// assert!(summary.any_disabled);
/// assert!(!summary.all_beneficial);
/// assert!(summary.total_disables >= 1);
/// ```
pub fn build_evaluator_synergy_summary(
    states: &[EvaluatorState],
    total_compile_checks: usize,
    total_compile_successes: usize,
    entries: &[DevTraceEntry],
) -> EvaluatorSynergySummary {
    let snapshots: Vec<EvaluatorSnapshot> =
        states.iter().map(EvaluatorSnapshot::from_state).collect();

    let active_evaluators = snapshots.len();

    let total_decisions: usize = snapshots.iter().map(|s| s.evaluation_count).sum();
    let total_disables: usize = snapshots.iter().map(|s| s.disable_count).sum();

    let overall_fix_rate = if total_compile_checks == 0 {
        0.0
    } else {
        total_compile_successes as f64 / total_compile_checks as f64
    };

    let any_disabled = snapshots
        .iter()
        .any(|s| s.is_disabled() || s.disable_count > 0);

    let evaluators_with_data: Vec<&EvaluatorSnapshot> =
        snapshots.iter().filter(|s| s.has_data()).collect();
    let all_beneficial = if evaluators_with_data.is_empty() {
        true
    } else {
        evaluators_with_data.iter().all(|s| s.is_beneficial)
    };

    let synergy_score = compute_synergy_score(&snapshots, any_disabled, all_beneficial);

    let timeline = build_evaluator_timeline(entries);

    EvaluatorSynergySummary {
        active_evaluators,
        snapshots,
        timeline,
        total_decisions,
        total_disables,
        overall_fix_rate,
        synergy_score,
        any_disabled,
        all_beneficial,
    }
}

// ============================================================================
//  交互影响分析
// ============================================================================

/// 评估器交互影响摘要 — 展示评估器之间的相互影响
///
/// 分析当某个评估器禁用功能后, 其他评估器的数据量变化。
///
/// # 字段
///
/// - `evaluator_type`: 被禁用的评估器
/// - `affected_evaluator`: 受影响的评估器
/// - `description`: 交互影响描述
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluatorInteraction {
    /// 触发交互的评估器 (执行了禁用动作)
    pub source_evaluator: EvaluatorType,
    /// 受影响的评估器
    pub affected_evaluator: EvaluatorType,
    /// 交互影响描述
    pub description: String,
}

/// 构建评估器交互影响列表
///
/// 分析三个评估器之间的潜在交互影响关系。
///
/// # 参数
///
/// - `snapshots`: 各评估器状态快照
/// - `timeline`: 评估器决策时间线
///
/// # 返回
///
/// 交互影响列表 (如果没有任何禁用, 返回空列表)
///
/// # 交互规则
///
/// - CacheTuner 禁用缓存 → SearchQuality 搜索数据减少
/// - SearchQuality 禁用搜索 → CacheTuner 无数据可缓存
/// - MemoryContext 禁用注入 → 影响整体修复率, 可能影响其他评估器的差值
///
/// # 示例
///
/// ```
/// # use forge::evaluator_synergy::{
/// #     EvaluatorSnapshot, EvaluatorType, build_evaluator_interactions,
/// # };
/// let snapshots = vec![
///     EvaluatorSnapshot {
///         evaluator_type: EvaluatorType::CacheTuner,
///         enabled: false, with_fix_rate: 0.3, without_fix_rate: 0.7,
///         diff: -0.4, is_beneficial: false, total_checks: 10,
///         evaluation_count: 2, disable_count: 1, contribution_score: 0.3,
///     },
/// ];
/// let interactions = build_evaluator_interactions(&snapshots);
/// assert!(!interactions.is_empty());
/// // CacheTuner 禁用 → 影响 SearchQuality
/// assert!(interactions.iter().any(|i| i.source_evaluator == EvaluatorType::CacheTuner));
/// ```
pub fn build_evaluator_interactions(snapshots: &[EvaluatorSnapshot]) -> Vec<EvaluatorInteraction> {
    let mut interactions = vec![];

    for snapshot in snapshots {
        if !snapshot.is_disabled() && snapshot.disable_count == 0 {
            continue;
        }

        match snapshot.evaluator_type {
            EvaluatorType::CacheTuner => {
                interactions.push(EvaluatorInteraction {
                    source_evaluator: EvaluatorType::CacheTuner,
                    affected_evaluator: EvaluatorType::SearchQuality,
                    description: "缓存禁用后, 搜索结果不再被缓存, \
                         SearchQualityEvaluator 的搜索次数可能增加"
                        .to_string(),
                });
            }
            EvaluatorType::SearchQuality => {
                interactions.push(EvaluatorInteraction {
                    source_evaluator: EvaluatorType::SearchQuality,
                    affected_evaluator: EvaluatorType::CacheTuner,
                    description: "搜索禁用后, 无新搜索结果产生, \
                         CacheTuner 无新数据可评估缓存效果"
                        .to_string(),
                });
            }
            EvaluatorType::MemoryContext => {
                interactions.push(EvaluatorInteraction {
                    source_evaluator: EvaluatorType::MemoryContext,
                    affected_evaluator: EvaluatorType::CacheTuner,
                    description: "Memory 注入禁用后, 修复成功率可能变化, \
                         影响 CacheTuner 的关联分析差值"
                        .to_string(),
                });
                interactions.push(EvaluatorInteraction {
                    source_evaluator: EvaluatorType::MemoryContext,
                    affected_evaluator: EvaluatorType::SearchQuality,
                    description: "Memory 注入禁用后, 修复成功率可能变化, \
                         影响 SearchQualityEvaluator 的搜索质量差值"
                        .to_string(),
                });
            }
        }
    }

    interactions
}

/// 计算评估器决策统计 — 按评估器类型分组统计各动作次数
///
/// # 参数
///
/// - `timeline`: 评估器决策时间线
///
/// # 返回
///
/// 按评估器类型分组的动作统计: `HashMap<EvaluatorType, HashMap<动作标签, 次数>>`
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::{DevTraceEntry, TraceAction};
/// # use forge::evaluator_synergy::{
/// #     build_evaluator_timeline, compute_evaluator_action_stats, EvaluatorType,
/// };
/// let entries = vec![
///     DevTraceEntry::new(
///         TraceAction::CacheTuning, Some(0), Some(0), Some("t1"),
///         "", "缓存调优: 保持当前配置 (差值 +10.0%, 原因: ...)",
///         0, true, None,
///     ),
///     DevTraceEntry::new(
///         TraceAction::CacheTuning, Some(0), Some(1), Some("t2"),
///         "", "缓存调优: 禁用缓存 (差值 -20.0%, 原因: ...)",
///         0, true, None,
///     ),
/// ];
/// let timeline = build_evaluator_timeline(&entries);
/// let stats = compute_evaluator_action_stats(&timeline);
/// let cache_stats = stats.get(&EvaluatorType::CacheTuner).unwrap();
/// assert_eq!(cache_stats.get("保持当前配置"), Some(&1));
/// assert_eq!(cache_stats.get("禁用缓存"), Some(&1));
/// ```
pub fn compute_evaluator_action_stats(
    timeline: &[EvaluatorTimelineEntry],
) -> HashMap<EvaluatorType, HashMap<String, u32>> {
    let mut stats: HashMap<EvaluatorType, HashMap<String, u32>> = HashMap::new();

    for entry in timeline {
        let action_label = entry.action.label().to_string();
        *stats
            .entry(entry.evaluator_type)
            .or_default()
            .entry(action_label)
            .or_insert(0) += 1;
    }

    stats
}

/// 计算各评估器的平均差值
///
/// # 参数
///
/// - `timeline`: 评估器决策时间线
///
/// # 返回
///
/// 按评估器类型分组的平均差值: `HashMap<EvaluatorType, f64>`
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::{DevTraceEntry, TraceAction};
/// # use forge::evaluator_synergy::{
/// #     build_evaluator_timeline, compute_evaluator_avg_diffs, EvaluatorType,
/// };
/// let entries = vec![
///     DevTraceEntry::new(
///         TraceAction::CacheTuning, Some(0), Some(0), Some("t1"),
///         "", "缓存调优: 保持当前配置 (差值 +10.0%, 原因: ...)",
///         0, true, None,
///     ),
///     DevTraceEntry::new(
///         TraceAction::CacheTuning, Some(0), Some(1), Some("t2"),
///         "", "缓存调优: 保持当前配置 (差值 +30.0%, 原因: ...)",
///         0, true, None,
///     ),
/// ];
/// let timeline = build_evaluator_timeline(&entries);
/// let diffs = compute_evaluator_avg_diffs(&timeline);
/// let cache_diff = diffs.get(&EvaluatorType::CacheTuner).unwrap();
/// assert!((cache_diff - 0.2).abs() < 0.001); // (0.1 + 0.3) / 2 = 0.2
/// ```
pub fn compute_evaluator_avg_diffs(
    timeline: &[EvaluatorTimelineEntry],
) -> HashMap<EvaluatorType, f64> {
    let mut sums: HashMap<EvaluatorType, (f64, usize)> = HashMap::new();

    for entry in timeline {
        let (sum, count) = sums.entry(entry.evaluator_type).or_insert((0.0, 0));
        *sum += entry.diff;
        *count += 1;
    }

    sums.into_iter()
        .map(|(k, (sum, count))| {
            let avg = if count == 0 { 0.0 } else { sum / count as f64 };
            (k, avg)
        })
        .collect()
}

// ============================================================================
//  JSON 导出
// ============================================================================

/// 将协同分析摘要序列化为 pretty JSON 字符串
///
/// # 参数
///
/// - `summary`: 协同分析摘要
///
/// # 返回
///
/// JSON 字符串
///
/// # 示例
///
/// ```
/// # use forge::evaluator_synergy::{EvaluatorSynergySummary, synergy_summary_to_json};
/// let summary = EvaluatorSynergySummary::empty();
/// let json = synergy_summary_to_json(&summary).unwrap();
/// assert!(json.contains("\"active_evaluators\": 0"));
/// ```
pub fn synergy_summary_to_json(summary: &EvaluatorSynergySummary) -> Result<String> {
    Ok(serde_json::to_string_pretty(summary)?)
}

/// 将协同分析摘要序列化为 compact JSON 字符串
///
/// # 参数
///
/// - `summary`: 协同分析摘要
///
/// # 返回
///
/// Compact JSON 字符串
pub fn synergy_summary_to_json_compact(summary: &EvaluatorSynergySummary) -> Result<String> {
    Ok(serde_json::to_string(summary)?)
}

/// 将协同分析摘要保存到 JSON 文件
///
/// # 参数
///
/// - `summary`: 协同分析摘要
/// - `path`: 文件路径
///
/// # 错误
///
/// 文件写入或序列化失败时返回错误。
pub fn save_synergy_summary_to_json(
    summary: &EvaluatorSynergySummary,
    path: &std::path::Path,
) -> Result<()> {
    let json = synergy_summary_to_json(summary)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, json)?;
    Ok(())
}

// ============================================================================
//  测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ======================================================================
    //  EvaluatorType 测试
    // ======================================================================

    #[test]
    fn test_evaluator_type_display() {
        assert_eq!(format!("{}", EvaluatorType::CacheTuner), "CacheTuner");
        assert_eq!(format!("{}", EvaluatorType::SearchQuality), "SearchQuality");
        assert_eq!(format!("{}", EvaluatorType::MemoryContext), "MemoryContext");
    }

    #[test]
    fn test_evaluator_type_label() {
        assert_eq!(EvaluatorType::CacheTuner.label(), "缓存调优");
        assert_eq!(EvaluatorType::SearchQuality.label(), "搜索质量");
        assert_eq!(EvaluatorType::MemoryContext.label(), "Memory 注入");
    }

    #[test]
    fn test_evaluator_type_all() {
        let all = EvaluatorType::all();
        assert_eq!(all.len(), 3);
        assert!(all.contains(&EvaluatorType::CacheTuner));
        assert!(all.contains(&EvaluatorType::SearchQuality));
        assert!(all.contains(&EvaluatorType::MemoryContext));
    }

    #[test]
    fn test_evaluator_type_serde() {
        let json = serde_json::to_string(&EvaluatorType::CacheTuner).unwrap();
        let loaded: EvaluatorType = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded, EvaluatorType::CacheTuner);
    }

    // ======================================================================
    //  EvaluatorState 测试
    // ======================================================================

    #[test]
    fn test_evaluator_state_new() {
        let state = EvaluatorState::new(
            EvaluatorType::CacheTuner,
            true,
            0.8,
            0.6,
            0.2,
            true,
            10,
            3,
            0,
        );
        assert_eq!(state.evaluator_type, EvaluatorType::CacheTuner);
        assert!(state.enabled);
        assert!((state.with_fix_rate - 0.8).abs() < 0.001);
        assert!((state.without_fix_rate - 0.6).abs() < 0.001);
        assert!((state.diff - 0.2).abs() < 0.001);
        assert!(state.is_beneficial);
        assert_eq!(state.total_checks, 10);
        assert_eq!(state.evaluation_count, 3);
        assert_eq!(state.disable_count, 0);
    }

    #[test]
    fn test_evaluator_state_has_data() {
        let state = EvaluatorState::new(
            EvaluatorType::CacheTuner,
            true,
            0.8,
            0.6,
            0.2,
            true,
            10,
            3,
            0,
        );
        assert!(state.has_data());

        let empty_state = EvaluatorState::new(
            EvaluatorType::SearchQuality,
            true,
            0.0,
            0.0,
            0.0,
            false,
            0,
            0,
            0,
        );
        assert!(!empty_state.has_data());
    }

    #[test]
    fn test_evaluator_state_serde() {
        let state = EvaluatorState::new(
            EvaluatorType::MemoryContext,
            false,
            0.3,
            0.7,
            -0.4,
            false,
            5,
            2,
            1,
        );
        let json = serde_json::to_string(&state).unwrap();
        let loaded: EvaluatorState = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.evaluator_type, EvaluatorType::MemoryContext);
        assert!(!loaded.enabled);
        assert!((loaded.diff - (-0.4)).abs() < 0.001);
        assert_eq!(loaded.disable_count, 1);
    }

    // ======================================================================
    //  EvaluatorSnapshot 测试
    // ======================================================================

    #[test]
    fn test_evaluator_snapshot_from_state() {
        let state = EvaluatorState::new(
            EvaluatorType::CacheTuner,
            true,
            0.8,
            0.6,
            0.2,
            true,
            10,
            3,
            0,
        );
        let snap = EvaluatorSnapshot::from_state(&state);
        assert_eq!(snap.evaluator_type, EvaluatorType::CacheTuner);
        assert!(snap.enabled);
        assert!((snap.with_fix_rate - 0.8).abs() < 0.001);
        assert!((snap.without_fix_rate - 0.6).abs() < 0.001);
        assert!((snap.diff - 0.2).abs() < 0.001);
        assert!(snap.is_beneficial);
        assert_eq!(snap.total_checks, 10);
        assert_eq!(snap.evaluation_count, 3);
        assert_eq!(snap.disable_count, 0);
        assert!(snap.contribution_score > 0.0);
    }

    #[test]
    fn test_evaluator_snapshot_has_data() {
        let state = EvaluatorState::new(
            EvaluatorType::CacheTuner,
            true,
            0.8,
            0.6,
            0.2,
            true,
            10,
            3,
            0,
        );
        let snap = EvaluatorSnapshot::from_state(&state);
        assert!(snap.has_data());

        let empty_state = EvaluatorState::new(
            EvaluatorType::SearchQuality,
            true,
            0.0,
            0.0,
            0.0,
            false,
            0,
            0,
            0,
        );
        let empty_snap = EvaluatorSnapshot::from_state(&empty_state);
        assert!(!empty_snap.has_data());
    }

    #[test]
    fn test_evaluator_snapshot_is_disabled() {
        let state_enabled = EvaluatorState::new(
            EvaluatorType::CacheTuner,
            true,
            0.8,
            0.6,
            0.2,
            true,
            10,
            3,
            0,
        );
        let snap = EvaluatorSnapshot::from_state(&state_enabled);
        assert!(!snap.is_disabled());

        let state_disabled = EvaluatorState::new(
            EvaluatorType::CacheTuner,
            false,
            0.3,
            0.7,
            -0.4,
            false,
            10,
            2,
            1,
        );
        let snap_disabled = EvaluatorSnapshot::from_state(&state_disabled);
        assert!(snap_disabled.is_disabled());
    }

    #[test]
    fn test_evaluator_snapshot_to_summary() {
        let state = EvaluatorState::new(
            EvaluatorType::CacheTuner,
            true,
            0.8,
            0.6,
            0.2,
            true,
            10,
            3,
            0,
        );
        let snap = EvaluatorSnapshot::from_state(&state);
        let summary = snap.to_summary();
        assert!(summary.contains("缓存调优"));
        assert!(summary.contains("启用"));
        assert!(summary.contains("+20.0%"));
        assert!(summary.contains("评估 3 次"));
    }

    #[test]
    fn test_evaluator_snapshot_serde() {
        let state = EvaluatorState::new(
            EvaluatorType::MemoryContext,
            false,
            0.3,
            0.7,
            -0.4,
            false,
            5,
            2,
            1,
        );
        let snap = EvaluatorSnapshot::from_state(&state);
        let json = serde_json::to_string(&snap).unwrap();
        let loaded: EvaluatorSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.evaluator_type, EvaluatorType::MemoryContext);
        assert!(!loaded.enabled);
        assert_eq!(loaded.disable_count, 1);
    }

    // ======================================================================
    //  compute_contribution_score 测试
    // ======================================================================

    #[test]
    fn test_contribution_score_no_data() {
        let state = EvaluatorState::new(
            EvaluatorType::CacheTuner,
            true,
            0.0,
            0.0,
            0.0,
            false,
            0,
            0,
            0,
        );
        assert!((compute_contribution_score(&state) - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_contribution_score_positive_diff() {
        let state = EvaluatorState::new(
            EvaluatorType::CacheTuner,
            true,
            0.8,
            0.6,
            0.2,
            true,
            10,
            3,
            0,
        );
        // base = (0.2 + 1.0) / 2.0 = 0.6, eval_bonus = 3/100 = 0.03
        let score = compute_contribution_score(&state);
        assert!((score - 0.63).abs() < 0.001);
    }

    #[test]
    fn test_contribution_score_negative_diff() {
        let state = EvaluatorState::new(
            EvaluatorType::CacheTuner,
            false,
            0.3,
            0.7,
            -0.4,
            false,
            10,
            2,
            1,
        );
        // base = (-0.4 + 1.0) / 2.0 = 0.3, eval_bonus = 2/100 = 0.02
        let score = compute_contribution_score(&state);
        assert!((score - 0.32).abs() < 0.001);
    }

    #[test]
    fn test_contribution_score_large_diff() {
        let state = EvaluatorState::new(
            EvaluatorType::SearchQuality,
            true,
            1.0,
            0.0,
            1.0,
            true,
            100,
            50,
            0,
        );
        // base = (1.0 + 1.0) / 2.0 = 1.0, eval_bonus = 50/100 = 0.5 → clamped to 0.1
        let score = compute_contribution_score(&state);
        assert!((score - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_contribution_score_extreme_negative() {
        let state = EvaluatorState::new(
            EvaluatorType::MemoryContext,
            false,
            0.0,
            1.0,
            -1.0,
            false,
            10,
            1,
            1,
        );
        // base = (-1.0 + 1.0) / 2.0 = 0.0, eval_bonus = 1/100 = 0.01
        let score = compute_contribution_score(&state);
        assert!((score - 0.01).abs() < 0.001);
    }

    // ======================================================================
    //  EvaluatorTimelineAction 测试
    // ======================================================================

    #[test]
    fn test_timeline_action_evaluator_type() {
        assert_eq!(
            EvaluatorTimelineAction::KeepCurrent.evaluator_type(),
            EvaluatorType::CacheTuner
        );
        assert_eq!(
            EvaluatorTimelineAction::AdjustTtl.evaluator_type(),
            EvaluatorType::CacheTuner
        );
        assert_eq!(
            EvaluatorTimelineAction::DisableCache.evaluator_type(),
            EvaluatorType::CacheTuner
        );
        assert_eq!(
            EvaluatorTimelineAction::KeepSearching.evaluator_type(),
            EvaluatorType::SearchQuality
        );
        assert_eq!(
            EvaluatorTimelineAction::DisableSearch.evaluator_type(),
            EvaluatorType::SearchQuality
        );
        assert_eq!(
            EvaluatorTimelineAction::InsufficientSearchData.evaluator_type(),
            EvaluatorType::SearchQuality
        );
        assert_eq!(
            EvaluatorTimelineAction::KeepInjecting.evaluator_type(),
            EvaluatorType::MemoryContext
        );
        assert_eq!(
            EvaluatorTimelineAction::DisableInjection.evaluator_type(),
            EvaluatorType::MemoryContext
        );
        assert_eq!(
            EvaluatorTimelineAction::InsufficientMemoryData.evaluator_type(),
            EvaluatorType::MemoryContext
        );
    }

    #[test]
    fn test_timeline_action_is_disable() {
        assert!(EvaluatorTimelineAction::DisableCache.is_disable());
        assert!(EvaluatorTimelineAction::DisableSearch.is_disable());
        assert!(EvaluatorTimelineAction::DisableInjection.is_disable());
        assert!(!EvaluatorTimelineAction::KeepCurrent.is_disable());
        assert!(!EvaluatorTimelineAction::AdjustTtl.is_disable());
        assert!(!EvaluatorTimelineAction::KeepSearching.is_disable());
        assert!(!EvaluatorTimelineAction::KeepInjecting.is_disable());
        assert!(!EvaluatorTimelineAction::InsufficientSearchData.is_disable());
        assert!(!EvaluatorTimelineAction::InsufficientMemoryData.is_disable());
    }

    #[test]
    fn test_timeline_action_label() {
        assert_eq!(EvaluatorTimelineAction::KeepCurrent.label(), "保持当前配置");
        assert_eq!(EvaluatorTimelineAction::AdjustTtl.label(), "调整 TTL");
        assert_eq!(EvaluatorTimelineAction::DisableCache.label(), "禁用缓存");
        assert_eq!(EvaluatorTimelineAction::KeepSearching.label(), "保持搜索");
        assert_eq!(EvaluatorTimelineAction::DisableSearch.label(), "禁用搜索");
        assert_eq!(
            EvaluatorTimelineAction::InsufficientSearchData.label(),
            "数据不足"
        );
        assert_eq!(EvaluatorTimelineAction::KeepInjecting.label(), "继续注入");
        assert_eq!(
            EvaluatorTimelineAction::DisableInjection.label(),
            "禁用注入"
        );
        assert_eq!(
            EvaluatorTimelineAction::InsufficientMemoryData.label(),
            "数据不足"
        );
    }

    #[test]
    fn test_timeline_action_display() {
        assert_eq!(
            format!("{}", EvaluatorTimelineAction::KeepCurrent),
            "保持当前配置"
        );
        assert_eq!(
            format!("{}", EvaluatorTimelineAction::DisableCache),
            "禁用缓存"
        );
    }

    #[test]
    fn test_timeline_action_serde() {
        let json = serde_json::to_string(&EvaluatorTimelineAction::DisableCache).unwrap();
        let loaded: EvaluatorTimelineAction = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded, EvaluatorTimelineAction::DisableCache);
    }

    // ======================================================================
    //  parse_diff_value 测试
    // ======================================================================

    #[test]
    fn test_parse_diff_value_positive() {
        assert!((parse_diff_value("差值 +10.0%") - 0.1).abs() < 0.001);
    }

    #[test]
    fn test_parse_diff_value_negative() {
        assert!((parse_diff_value("差值 -5.0%") - (-0.05)).abs() < 0.001);
    }

    #[test]
    fn test_parse_diff_value_zero() {
        assert!((parse_diff_value("差值 +0.0%") - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_parse_diff_value_large() {
        assert!((parse_diff_value("差值 +67.0%") - 0.67).abs() < 0.001);
    }

    #[test]
    fn test_parse_diff_value_no_match() {
        assert!((parse_diff_value("无差值信息") - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_parse_diff_value_empty() {
        assert!((parse_diff_value("") - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_parse_diff_value_in_context() {
        let output = "缓存调优: 保持当前配置 (差值 +20.0%, 原因: 缓存有效)";
        assert!((parse_diff_value(output) - 0.2).abs() < 0.001);
    }

    // ======================================================================
    //  parse_evaluator_timeline_action 测试
    // ======================================================================

    #[test]
    fn test_parse_cache_tuning_keep_current() {
        let entry = DevTraceEntry::new(
            TraceAction::CacheTuning,
            Some(0),
            Some(0),
            Some("t1"),
            "hit=2/3",
            "缓存调优: 保持当前配置 (差值 +10.0%, 原因: ...)",
            0,
            true,
            None,
        );
        let (action, diff) = parse_evaluator_timeline_action(&entry).unwrap();
        assert_eq!(action, EvaluatorTimelineAction::KeepCurrent);
        assert!((diff - 0.1).abs() < 0.001);
    }

    #[test]
    fn test_parse_cache_tuning_adjust_ttl() {
        let entry = DevTraceEntry::new(
            TraceAction::CacheTuning,
            Some(0),
            Some(0),
            Some("t1"),
            "hit=2/3",
            "缓存调优: 调整 TTL: 1800s → 2700s (差值 +67.0%, 原因: ...)",
            0,
            true,
            None,
        );
        let (action, diff) = parse_evaluator_timeline_action(&entry).unwrap();
        assert_eq!(action, EvaluatorTimelineAction::AdjustTtl);
        assert!((diff - 0.67).abs() < 0.001);
    }

    #[test]
    fn test_parse_cache_tuning_disable() {
        let entry = DevTraceEntry::new(
            TraceAction::CacheTuning,
            Some(0),
            Some(0),
            Some("t1"),
            "hit=1/3",
            "缓存调优: 禁用缓存 (差值 -20.0%, 原因: ...)",
            0,
            true,
            None,
        );
        let (action, diff) = parse_evaluator_timeline_action(&entry).unwrap();
        assert_eq!(action, EvaluatorTimelineAction::DisableCache);
        assert!((diff - (-0.2)).abs() < 0.001);
    }

    #[test]
    fn test_parse_search_quality_keep() {
        let entry = DevTraceEntry::new(
            TraceAction::SearchQuality,
            Some(0),
            Some(0),
            Some("t1"),
            "with=2/3",
            "搜索质量: 保持搜索 (差值 +10.0%, 原因: ...)",
            0,
            true,
            None,
        );
        let (action, diff) = parse_evaluator_timeline_action(&entry).unwrap();
        assert_eq!(action, EvaluatorTimelineAction::KeepSearching);
        assert!((diff - 0.1).abs() < 0.001);
    }

    #[test]
    fn test_parse_search_quality_disable() {
        let entry = DevTraceEntry::new(
            TraceAction::SearchQuality,
            Some(0),
            Some(0),
            Some("t1"),
            "with=1/3",
            "搜索质量: 禁用搜索 (差值 -15.0%, 原因: ...)",
            0,
            true,
            None,
        );
        let (action, diff) = parse_evaluator_timeline_action(&entry).unwrap();
        assert_eq!(action, EvaluatorTimelineAction::DisableSearch);
        assert!((diff - (-0.15)).abs() < 0.001);
    }

    #[test]
    fn test_parse_search_quality_insufficient() {
        let entry = DevTraceEntry::new(
            TraceAction::SearchQuality,
            Some(0),
            Some(0),
            Some("t1"),
            "with=1/1",
            "搜索质量: 数据不足 (差值 +0.0%, 原因: ...)",
            0,
            true,
            None,
        );
        let (action, _diff) = parse_evaluator_timeline_action(&entry).unwrap();
        assert_eq!(action, EvaluatorTimelineAction::InsufficientSearchData);
    }

    #[test]
    fn test_parse_memory_evaluation_keep() {
        let entry = DevTraceEntry::new(
            TraceAction::MemoryEvaluation,
            Some(0),
            Some(0),
            Some("t1"),
            "with=2/3",
            "Memory 评估: KeepInjecting (差值 +10.0%, 有效)",
            0,
            true,
            None,
        );
        let (action, diff) = parse_evaluator_timeline_action(&entry).unwrap();
        assert_eq!(action, EvaluatorTimelineAction::KeepInjecting);
        assert!((diff - 0.1).abs() < 0.001);
    }

    #[test]
    fn test_parse_memory_evaluation_disable() {
        let entry = DevTraceEntry::new(
            TraceAction::MemoryEvaluation,
            Some(0),
            Some(0),
            Some("t1"),
            "with=1/3",
            "Memory 评估: DisableInjection (差值 -20.0%, 有害)",
            0,
            true,
            None,
        );
        let (action, diff) = parse_evaluator_timeline_action(&entry).unwrap();
        assert_eq!(action, EvaluatorTimelineAction::DisableInjection);
        assert!((diff - (-0.2)).abs() < 0.001);
    }

    #[test]
    fn test_parse_memory_evaluation_insufficient() {
        let entry = DevTraceEntry::new(
            TraceAction::MemoryEvaluation,
            Some(0),
            Some(0),
            Some("t1"),
            "with=1/1",
            "Memory 评估: InsufficientData (差值 +0.0%, ...)",
            0,
            true,
            None,
        );
        let (action, _diff) = parse_evaluator_timeline_action(&entry).unwrap();
        assert_eq!(action, EvaluatorTimelineAction::InsufficientMemoryData);
    }

    #[test]
    fn test_parse_non_evaluator_action() {
        let entry = DevTraceEntry::new(
            TraceAction::CompileCheck,
            Some(0),
            Some(0),
            Some("t1"),
            "check",
            "passed",
            50,
            true,
            None,
        );
        assert!(parse_evaluator_timeline_action(&entry).is_none());
    }

    #[test]
    fn test_parse_web_search_action() {
        let entry = DevTraceEntry::new(
            TraceAction::WebSearch,
            Some(0),
            Some(0),
            Some("t1"),
            "query",
            "result",
            100,
            true,
            None,
        );
        assert!(parse_evaluator_timeline_action(&entry).is_none());
    }

    // ======================================================================
    //  build_evaluator_timeline 测试
    // ======================================================================

    #[test]
    fn test_build_timeline_empty() {
        let timeline = build_evaluator_timeline(&[]);
        assert!(timeline.is_empty());
    }

    #[test]
    fn test_build_timeline_single_evaluator() {
        let entries = vec![DevTraceEntry::new(
            TraceAction::CacheTuning,
            Some(0),
            Some(0),
            Some("t1"),
            "hit=2/3",
            "缓存调优: 保持当前配置 (差值 +10.0%, 原因: ...)",
            0,
            true,
            None,
        )];
        let timeline = build_evaluator_timeline(&entries);
        assert_eq!(timeline.len(), 1);
        assert_eq!(timeline[0].action, EvaluatorTimelineAction::KeepCurrent);
        assert_eq!(timeline[0].evaluator_type, EvaluatorType::CacheTuner);
    }

    #[test]
    fn test_build_timeline_multiple_evaluators() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::CacheTuning,
                Some(0),
                Some(0),
                Some("t1"),
                "hit=2/3",
                "缓存调优: 调整 TTL (差值 +20.0%, 原因: ...)",
                0,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::SearchQuality,
                Some(0),
                Some(0),
                Some("t1"),
                "with=2/3",
                "搜索质量: 保持搜索 (差值 +10.0%, 原因: ...)",
                0,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::MemoryEvaluation,
                Some(0),
                Some(0),
                Some("t1"),
                "with=2/3",
                "Memory 评估: KeepInjecting (差值 +5.0%, ...)",
                0,
                true,
                None,
            ),
        ];
        let timeline = build_evaluator_timeline(&entries);
        assert_eq!(timeline.len(), 3);
        assert_eq!(timeline[0].action, EvaluatorTimelineAction::AdjustTtl);
        assert_eq!(timeline[1].action, EvaluatorTimelineAction::KeepSearching);
        assert_eq!(timeline[2].action, EvaluatorTimelineAction::KeepInjecting);
    }

    #[test]
    fn test_build_timeline_filters_non_evaluator() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(0),
                Some("t1"),
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
                Some("t1"),
                "hit=2/3",
                "缓存调优: 保持当前配置 (差值 +10.0%, 原因: ...)",
                0,
                true,
                None,
            ),
        ];
        let timeline = build_evaluator_timeline(&entries);
        assert_eq!(timeline.len(), 1);
    }

    #[test]
    fn test_build_timeline_preserves_task_info() {
        let entries = vec![DevTraceEntry::new(
            TraceAction::CacheTuning,
            Some(1),
            Some(2),
            Some("task_name"),
            "hit=2/3",
            "缓存调优: 保持当前配置 (差值 +10.0%, 原因: test)",
            0,
            true,
            Some("test reason"),
        )];
        let timeline = build_evaluator_timeline(&entries);
        assert_eq!(timeline.len(), 1);
        assert_eq!(timeline[0].phase_idx, Some(1));
        assert_eq!(timeline[0].task_idx, Some(2));
        assert_eq!(timeline[0].task_name, Some("task_name".to_string()));
        assert_eq!(timeline[0].reason, Some("test reason".to_string()));
    }

    #[test]
    fn test_build_timeline_sorted_by_timestamp() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::SearchQuality,
                Some(0),
                Some(0),
                Some("t1"),
                "with=2/3",
                "搜索质量: 保持搜索 (差值 +10.0%, 原因: ...)",
                0,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::CacheTuning,
                Some(0),
                Some(0),
                Some("t1"),
                "hit=2/3",
                "缓存调优: 保持当前配置 (差值 +20.0%, 原因: ...)",
                0,
                true,
                None,
            ),
        ];
        let timeline = build_evaluator_timeline(&entries);
        assert_eq!(timeline.len(), 2);
        // 两条条目的时间戳几乎相同 (都是 Utc::now()), 排序应保持顺序
        assert_eq!(timeline[0].action, EvaluatorTimelineAction::KeepSearching);
        assert_eq!(timeline[1].action, EvaluatorTimelineAction::KeepCurrent);
    }

    // ======================================================================
    //  compute_synergy_score 测试
    // ======================================================================

    #[test]
    fn test_synergy_score_empty() {
        let score = compute_synergy_score(&[], false, true);
        assert!((score - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_synergy_score_all_beneficial_no_disable() {
        let snapshots = vec![
            EvaluatorSnapshot {
                evaluator_type: EvaluatorType::CacheTuner,
                enabled: true,
                with_fix_rate: 0.8,
                without_fix_rate: 0.6,
                diff: 0.2,
                is_beneficial: true,
                total_checks: 10,
                evaluation_count: 3,
                disable_count: 0,
                contribution_score: 0.6,
            },
            EvaluatorSnapshot {
                evaluator_type: EvaluatorType::SearchQuality,
                enabled: true,
                with_fix_rate: 0.7,
                without_fix_rate: 0.5,
                diff: 0.2,
                is_beneficial: true,
                total_checks: 10,
                evaluation_count: 3,
                disable_count: 0,
                contribution_score: 0.6,
            },
        ];
        // avg = 0.6, all_beneficial → *1.1 = 0.66
        let score = compute_synergy_score(&snapshots, false, true);
        assert!((score - 0.66).abs() < 0.01);
    }

    #[test]
    fn test_synergy_score_with_disable() {
        let snapshots = vec![EvaluatorSnapshot {
            evaluator_type: EvaluatorType::CacheTuner,
            enabled: true,
            with_fix_rate: 0.8,
            without_fix_rate: 0.6,
            diff: 0.2,
            is_beneficial: true,
            total_checks: 10,
            evaluation_count: 3,
            disable_count: 0,
            contribution_score: 0.6,
        }];
        // avg = 0.6, any_disabled → *0.8 = 0.48
        let score = compute_synergy_score(&snapshots, true, false);
        assert!((score - 0.48).abs() < 0.01);
    }

    #[test]
    fn test_synergy_score_clamped() {
        let snapshots = vec![EvaluatorSnapshot {
            evaluator_type: EvaluatorType::CacheTuner,
            enabled: true,
            with_fix_rate: 1.0,
            without_fix_rate: 0.0,
            diff: 1.0,
            is_beneficial: true,
            total_checks: 100,
            evaluation_count: 50,
            disable_count: 0,
            contribution_score: 1.0,
        }];
        // avg = 1.0, all_beneficial → *1.1 = 1.1 → clamped to 1.0
        let score = compute_synergy_score(&snapshots, false, true);
        assert!((score - 1.0).abs() < 0.001);
    }

    // ======================================================================
    //  build_evaluator_synergy_summary 测试
    // ======================================================================

    #[test]
    fn test_build_summary_empty() {
        let summary = build_evaluator_synergy_summary(&[], 0, 0, &[]);
        assert_eq!(summary.active_evaluators, 0);
        assert!(summary.snapshots.is_empty());
        assert!(summary.timeline.is_empty());
        assert_eq!(summary.total_decisions, 0);
        assert_eq!(summary.total_disables, 0);
        assert!((summary.overall_fix_rate - 0.0).abs() < 0.001);
        assert!(!summary.any_disabled);
        assert!(summary.all_beneficial);
    }

    #[test]
    fn test_build_summary_single_evaluator() {
        let states = vec![EvaluatorState::new(
            EvaluatorType::CacheTuner,
            true,
            0.8,
            0.6,
            0.2,
            true,
            10,
            3,
            0,
        )];
        let summary = build_evaluator_synergy_summary(&states, 10, 8, &[]);
        assert_eq!(summary.active_evaluators, 1);
        assert_eq!(summary.snapshots.len(), 1);
        assert_eq!(summary.total_decisions, 3);
        assert_eq!(summary.total_disables, 0);
        assert!((summary.overall_fix_rate - 0.8).abs() < 0.001);
        assert!(!summary.any_disabled);
        assert!(summary.all_beneficial);
    }

    #[test]
    fn test_build_summary_all_three_evaluators() {
        let states = vec![
            EvaluatorState::new(
                EvaluatorType::CacheTuner,
                true,
                0.8,
                0.6,
                0.2,
                true,
                10,
                3,
                0,
            ),
            EvaluatorState::new(
                EvaluatorType::SearchQuality,
                true,
                0.7,
                0.5,
                0.2,
                true,
                10,
                3,
                0,
            ),
            EvaluatorState::new(
                EvaluatorType::MemoryContext,
                true,
                0.6,
                0.4,
                0.2,
                true,
                10,
                3,
                0,
            ),
        ];
        let summary = build_evaluator_synergy_summary(&states, 30, 21, &[]);
        assert_eq!(summary.active_evaluators, 3);
        assert_eq!(summary.snapshots.len(), 3);
        assert_eq!(summary.total_decisions, 9);
        assert_eq!(summary.total_disables, 0);
        assert!((summary.overall_fix_rate - 0.7).abs() < 0.001);
        assert!(!summary.any_disabled);
        assert!(summary.all_beneficial);
    }

    #[test]
    fn test_build_summary_with_disabled() {
        let states = vec![
            EvaluatorState::new(
                EvaluatorType::CacheTuner,
                true,
                0.8,
                0.6,
                0.2,
                true,
                10,
                3,
                0,
            ),
            EvaluatorState::new(
                EvaluatorType::SearchQuality,
                false,
                0.3,
                0.7,
                -0.4,
                false,
                10,
                2,
                1,
            ),
        ];
        let summary = build_evaluator_synergy_summary(&states, 20, 12, &[]);
        assert_eq!(summary.active_evaluators, 2);
        assert!(summary.any_disabled);
        assert!(!summary.all_beneficial);
        assert_eq!(summary.total_disables, 1);
    }

    #[test]
    fn test_build_summary_overall_fix_rate_zero_checks() {
        let states = vec![EvaluatorState::new(
            EvaluatorType::CacheTuner,
            true,
            0.8,
            0.6,
            0.2,
            true,
            10,
            3,
            0,
        )];
        let summary = build_evaluator_synergy_summary(&states, 0, 0, &[]);
        assert!((summary.overall_fix_rate - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_build_summary_with_timeline() {
        let states = vec![EvaluatorState::new(
            EvaluatorType::CacheTuner,
            true,
            0.8,
            0.6,
            0.2,
            true,
            10,
            3,
            0,
        )];
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::CacheTuning,
                Some(0),
                Some(0),
                Some("t1"),
                "hit=2/3",
                "缓存调优: 保持当前配置 (差值 +10.0%, 原因: ...)",
                0,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::SearchQuality,
                Some(0),
                Some(0),
                Some("t1"),
                "with=2/3",
                "搜索质量: 保持搜索 (差值 +20.0%, 原因: ...)",
                0,
                true,
                None,
            ),
        ];
        let summary = build_evaluator_synergy_summary(&states, 10, 8, &entries);
        assert_eq!(summary.timeline.len(), 2);
    }

    #[test]
    fn test_build_summary_all_beneficial_no_data() {
        let states = vec![EvaluatorState::new(
            EvaluatorType::CacheTuner,
            true,
            0.0,
            0.0,
            0.0,
            false,
            0,
            0,
            0,
        )];
        let summary = build_evaluator_synergy_summary(&states, 0, 0, &[]);
        // No data → all_beneficial defaults to true
        assert!(summary.all_beneficial);
    }

    // ======================================================================
    //  EvaluatorSynergySummary 测试
    // ======================================================================

    #[test]
    fn test_summary_empty() {
        let summary = EvaluatorSynergySummary::empty();
        assert_eq!(summary.active_evaluators, 0);
        assert!(summary.is_empty());
        assert!(!summary.has_decisions());
    }

    #[test]
    fn test_summary_default() {
        let summary = EvaluatorSynergySummary::default();
        assert!(summary.is_empty());
    }

    #[test]
    fn test_summary_to_summary() {
        let states = vec![
            EvaluatorState::new(
                EvaluatorType::CacheTuner,
                true,
                0.8,
                0.6,
                0.2,
                true,
                10,
                3,
                0,
            ),
            EvaluatorState::new(
                EvaluatorType::SearchQuality,
                true,
                0.7,
                0.5,
                0.2,
                true,
                10,
                3,
                0,
            ),
        ];
        let summary = build_evaluator_synergy_summary(&states, 20, 15, &[]);
        let s = summary.to_summary();
        assert!(s.contains("2 个评估器活跃"));
        assert!(s.contains("6 个决策"));
        assert!(s.contains("0 个禁用"));
        assert!(s.contains("协同评分"));
        assert!(s.contains("总体修复率"));
    }

    #[test]
    fn test_summary_serde_roundtrip() {
        let states = vec![EvaluatorState::new(
            EvaluatorType::CacheTuner,
            true,
            0.8,
            0.6,
            0.2,
            true,
            10,
            3,
            0,
        )];
        let summary = build_evaluator_synergy_summary(&states, 10, 8, &[]);
        let json = serde_json::to_string(&summary).unwrap();
        let loaded: EvaluatorSynergySummary = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.active_evaluators, 1);
        assert_eq!(loaded.total_decisions, 3);
        assert!((loaded.overall_fix_rate - 0.8).abs() < 0.001);
    }

    // ======================================================================
    //  build_evaluator_interactions 测试
    // ======================================================================

    #[test]
    fn test_interactions_no_disabled() {
        let snapshots = vec![EvaluatorSnapshot {
            evaluator_type: EvaluatorType::CacheTuner,
            enabled: true,
            with_fix_rate: 0.8,
            without_fix_rate: 0.6,
            diff: 0.2,
            is_beneficial: true,
            total_checks: 10,
            evaluation_count: 3,
            disable_count: 0,
            contribution_score: 0.6,
        }];
        let interactions = build_evaluator_interactions(&snapshots);
        assert!(interactions.is_empty());
    }

    #[test]
    fn test_interactions_cache_disabled() {
        let snapshots = vec![EvaluatorSnapshot {
            evaluator_type: EvaluatorType::CacheTuner,
            enabled: false,
            with_fix_rate: 0.3,
            without_fix_rate: 0.7,
            diff: -0.4,
            is_beneficial: false,
            total_checks: 10,
            evaluation_count: 2,
            disable_count: 1,
            contribution_score: 0.3,
        }];
        let interactions = build_evaluator_interactions(&snapshots);
        assert_eq!(interactions.len(), 1);
        assert_eq!(interactions[0].source_evaluator, EvaluatorType::CacheTuner);
        assert_eq!(
            interactions[0].affected_evaluator,
            EvaluatorType::SearchQuality
        );
    }

    #[test]
    fn test_interactions_search_disabled() {
        let snapshots = vec![EvaluatorSnapshot {
            evaluator_type: EvaluatorType::SearchQuality,
            enabled: false,
            with_fix_rate: 0.3,
            without_fix_rate: 0.7,
            diff: -0.4,
            is_beneficial: false,
            total_checks: 10,
            evaluation_count: 2,
            disable_count: 1,
            contribution_score: 0.3,
        }];
        let interactions = build_evaluator_interactions(&snapshots);
        assert_eq!(interactions.len(), 1);
        assert_eq!(
            interactions[0].source_evaluator,
            EvaluatorType::SearchQuality
        );
        assert_eq!(
            interactions[0].affected_evaluator,
            EvaluatorType::CacheTuner
        );
    }

    #[test]
    fn test_interactions_memory_disabled() {
        let snapshots = vec![EvaluatorSnapshot {
            evaluator_type: EvaluatorType::MemoryContext,
            enabled: false,
            with_fix_rate: 0.3,
            without_fix_rate: 0.7,
            diff: -0.4,
            is_beneficial: false,
            total_checks: 10,
            evaluation_count: 2,
            disable_count: 1,
            contribution_score: 0.3,
        }];
        let interactions = build_evaluator_interactions(&snapshots);
        assert_eq!(interactions.len(), 2);
        // Memory disabled affects both CacheTuner and SearchQuality
        assert!(interactions.iter().any(|i| {
            i.source_evaluator == EvaluatorType::MemoryContext
                && i.affected_evaluator == EvaluatorType::CacheTuner
        }));
        assert!(interactions.iter().any(|i| {
            i.source_evaluator == EvaluatorType::MemoryContext
                && i.affected_evaluator == EvaluatorType::SearchQuality
        }));
    }

    #[test]
    fn test_interactions_all_disabled() {
        let snapshots = vec![
            EvaluatorSnapshot {
                evaluator_type: EvaluatorType::CacheTuner,
                enabled: false,
                with_fix_rate: 0.3,
                without_fix_rate: 0.7,
                diff: -0.4,
                is_beneficial: false,
                total_checks: 10,
                evaluation_count: 2,
                disable_count: 1,
                contribution_score: 0.3,
            },
            EvaluatorSnapshot {
                evaluator_type: EvaluatorType::SearchQuality,
                enabled: false,
                with_fix_rate: 0.3,
                without_fix_rate: 0.7,
                diff: -0.4,
                is_beneficial: false,
                total_checks: 10,
                evaluation_count: 2,
                disable_count: 1,
                contribution_score: 0.3,
            },
            EvaluatorSnapshot {
                evaluator_type: EvaluatorType::MemoryContext,
                enabled: false,
                with_fix_rate: 0.3,
                without_fix_rate: 0.7,
                diff: -0.4,
                is_beneficial: false,
                total_checks: 10,
                evaluation_count: 2,
                disable_count: 1,
                contribution_score: 0.3,
            },
        ];
        let interactions = build_evaluator_interactions(&snapshots);
        // CacheTuner → SearchQuality (1)
        // SearchQuality → CacheTuner (1)
        // MemoryContext → CacheTuner + SearchQuality (2)
        assert_eq!(interactions.len(), 4);
    }

    #[test]
    fn test_interactions_with_disable_count_but_enabled() {
        let snapshots = vec![EvaluatorSnapshot {
            evaluator_type: EvaluatorType::CacheTuner,
            enabled: true, // Currently enabled
            with_fix_rate: 0.8,
            without_fix_rate: 0.6,
            diff: 0.2,
            is_beneficial: true,
            total_checks: 10,
            evaluation_count: 5,
            disable_count: 1, // But had a disable in the past
            contribution_score: 0.6,
        }];
        let interactions = build_evaluator_interactions(&snapshots);
        // disable_count > 0 → should generate interaction
        assert!(!interactions.is_empty());
    }

    #[test]
    fn test_interaction_serde() {
        let interaction = EvaluatorInteraction {
            source_evaluator: EvaluatorType::CacheTuner,
            affected_evaluator: EvaluatorType::SearchQuality,
            description: "test".to_string(),
        };
        let json = serde_json::to_string(&interaction).unwrap();
        let loaded: EvaluatorInteraction = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.source_evaluator, EvaluatorType::CacheTuner);
        assert_eq!(loaded.affected_evaluator, EvaluatorType::SearchQuality);
    }

    // ======================================================================
    //  compute_evaluator_action_stats 测试
    // ======================================================================

    #[test]
    fn test_action_stats_empty() {
        let stats = compute_evaluator_action_stats(&[]);
        assert!(stats.is_empty());
    }

    #[test]
    fn test_action_stats_single() {
        let entries = vec![DevTraceEntry::new(
            TraceAction::CacheTuning,
            Some(0),
            Some(0),
            Some("t1"),
            "",
            "缓存调优: 保持当前配置 (差值 +10.0%, 原因: ...)",
            0,
            true,
            None,
        )];
        let timeline = build_evaluator_timeline(&entries);
        let stats = compute_evaluator_action_stats(&timeline);
        let cache_stats = stats.get(&EvaluatorType::CacheTuner).unwrap();
        assert_eq!(cache_stats.get("保持当前配置"), Some(&1));
    }

    #[test]
    fn test_action_stats_multiple() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::CacheTuning,
                Some(0),
                Some(0),
                Some("t1"),
                "",
                "缓存调优: 保持当前配置 (差值 +10.0%, 原因: ...)",
                0,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::CacheTuning,
                Some(0),
                Some(1),
                Some("t2"),
                "",
                "缓存调优: 禁用缓存 (差值 -20.0%, 原因: ...)",
                0,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::SearchQuality,
                Some(0),
                Some(0),
                Some("t1"),
                "",
                "搜索质量: 保持搜索 (差值 +15.0%, 原因: ...)",
                0,
                true,
                None,
            ),
        ];
        let timeline = build_evaluator_timeline(&entries);
        let stats = compute_evaluator_action_stats(&timeline);
        let cache_stats = stats.get(&EvaluatorType::CacheTuner).unwrap();
        assert_eq!(cache_stats.get("保持当前配置"), Some(&1));
        assert_eq!(cache_stats.get("禁用缓存"), Some(&1));

        let search_stats = stats.get(&EvaluatorType::SearchQuality).unwrap();
        assert_eq!(search_stats.get("保持搜索"), Some(&1));
    }

    // ======================================================================
    //  compute_evaluator_avg_diffs 测试
    // ======================================================================

    #[test]
    fn test_avg_diffs_empty() {
        let diffs = compute_evaluator_avg_diffs(&[]);
        assert!(diffs.is_empty());
    }

    #[test]
    fn test_avg_diffs_single() {
        let entries = vec![DevTraceEntry::new(
            TraceAction::CacheTuning,
            Some(0),
            Some(0),
            Some("t1"),
            "",
            "缓存调优: 保持当前配置 (差值 +10.0%, 原因: ...)",
            0,
            true,
            None,
        )];
        let timeline = build_evaluator_timeline(&entries);
        let diffs = compute_evaluator_avg_diffs(&timeline);
        let cache_diff = diffs.get(&EvaluatorType::CacheTuner).unwrap();
        assert!((cache_diff - 0.1).abs() < 0.001);
    }

    #[test]
    fn test_avg_diffs_multiple() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::CacheTuning,
                Some(0),
                Some(0),
                Some("t1"),
                "",
                "缓存调优: 保持当前配置 (差值 +10.0%, 原因: ...)",
                0,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::CacheTuning,
                Some(0),
                Some(1),
                Some("t2"),
                "",
                "缓存调优: 保持当前配置 (差值 +30.0%, 原因: ...)",
                0,
                true,
                None,
            ),
        ];
        let timeline = build_evaluator_timeline(&entries);
        let diffs = compute_evaluator_avg_diffs(&timeline);
        let cache_diff = diffs.get(&EvaluatorType::CacheTuner).unwrap();
        assert!((cache_diff - 0.2).abs() < 0.001); // (0.1 + 0.3) / 2
    }

    #[test]
    fn test_avg_diffs_multiple_evaluators() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::CacheTuning,
                Some(0),
                Some(0),
                Some("t1"),
                "",
                "缓存调优: 保持当前配置 (差值 +20.0%, 原因: ...)",
                0,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::SearchQuality,
                Some(0),
                Some(0),
                Some("t1"),
                "",
                "搜索质量: 保持搜索 (差值 +10.0%, 原因: ...)",
                0,
                true,
                None,
            ),
        ];
        let timeline = build_evaluator_timeline(&entries);
        let diffs = compute_evaluator_avg_diffs(&timeline);
        assert_eq!(diffs.len(), 2);
        let cache_diff = diffs.get(&EvaluatorType::CacheTuner).unwrap();
        assert!((cache_diff - 0.2).abs() < 0.001);
        let search_diff = diffs.get(&EvaluatorType::SearchQuality).unwrap();
        assert!((search_diff - 0.1).abs() < 0.001);
    }

    // ======================================================================
    //  JSON 导出测试
    // ======================================================================

    #[test]
    fn test_synergy_to_json() {
        let summary = EvaluatorSynergySummary::empty();
        let json = synergy_summary_to_json(&summary).unwrap();
        assert!(json.contains("\"active_evaluators\": 0"));
        assert!(json.contains("\"synergy_score\": 0.0"));
    }

    #[test]
    fn test_synergy_to_json_compact() {
        let summary = EvaluatorSynergySummary::empty();
        let json = synergy_summary_to_json_compact(&summary).unwrap();
        assert!(json.contains("\"active_evaluators\":0"));
        assert!(!json.contains("\n"));
    }

    #[test]
    fn test_synergy_to_json_with_data() {
        let states = vec![EvaluatorState::new(
            EvaluatorType::CacheTuner,
            true,
            0.8,
            0.6,
            0.2,
            true,
            10,
            3,
            0,
        )];
        let summary = build_evaluator_synergy_summary(&states, 10, 8, &[]);
        let json = synergy_summary_to_json(&summary).unwrap();
        assert!(json.contains("\"active_evaluators\": 1"));
        assert!(json.contains("\"total_decisions\": 3"));
        assert!(json.contains("\"snapshots\""));
    }

    #[test]
    fn test_save_synergy_to_json_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("synergy.json");
        let summary = EvaluatorSynergySummary::empty();
        save_synergy_summary_to_json(&summary, &path).unwrap();
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("\"active_evaluators\": 0"));
    }

    #[test]
    fn test_save_synergy_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("subdir")
            .join("nested")
            .join("synergy.json");
        let summary = EvaluatorSynergySummary::empty();
        save_synergy_summary_to_json(&summary, &path).unwrap();
        assert!(path.exists());
    }

    // ======================================================================
    //  文档测试辅助
    // ======================================================================

    #[test]
    fn test_doc_evaluator_snapshot_from_state() {
        let state = EvaluatorState::new(
            EvaluatorType::CacheTuner,
            true,
            0.8,
            0.6,
            0.2,
            true,
            10,
            3,
            0,
        );
        let snap = EvaluatorSnapshot::from_state(&state);
        assert!(snap.contribution_score > 0.0);
    }

    #[test]
    fn test_doc_build_summary_with_interactions() {
        let states = vec![
            EvaluatorState::new(
                EvaluatorType::CacheTuner,
                false,
                0.3,
                0.7,
                -0.4,
                false,
                10,
                2,
                1,
            ),
            EvaluatorState::new(
                EvaluatorType::MemoryContext,
                true,
                0.8,
                0.6,
                0.2,
                true,
                10,
                3,
                0,
            ),
        ];
        let summary = build_evaluator_synergy_summary(&states, 20, 12, &[]);
        assert!(summary.any_disabled);
        assert!(!summary.all_beneficial);
    }
}
