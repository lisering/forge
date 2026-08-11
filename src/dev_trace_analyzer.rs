//! DevTrace 智能分析引擎 — 从 DevTraceSummary 提取洞察并生成可操作建议
//!
//! 本模块不收集数据, 而是对已有的 [`DevTraceSummary`] 进行深度分析,
//! 生成健康度评分、洞察列表和可操作建议, 并输出 Markdown 分析报告。
//!
//! ## 设计理念
//!
//! DevTraceSummary 的 `to_report()` 展示原始数据, `to_html_report()` 可视化数据,
//! 而本模块专注于 **解读** 数据:
//!
//! - **健康度评分** (`HealthScore`) — 将多个维度压缩为 0~100 的单一指标
//! - **洞察列表** (`AnalysisInsight`) — 识别关键指标并给出解读
//! - **可操作建议** (`AnalysisRecommendation`) — 根据数据给出具体的改进入建议
//!
//! ## 核心数据结构
//!
//! - [`RecommendationSeverity`] — 建议严重级别 (Info/Warning/Critical)
//! - [`AnalysisCategory`] — 分析维度分类
//! - [`AnalysisRecommendation`] — 单条可操作建议
//! - [`AnalysisInsight`] — 单条洞察
//! - [`HealthScore`] — 健康度评分 (含分项明细)
//! - [`DevTraceAnalysis`] — 完整分析结果
//!
//! ## 纯函数
//!
//! - [`compute_health_score`] — 计算健康度评分
//! - [`analyze_cache_effectiveness`] — 分析缓存有效性
//! - [`analyze_search_quality`] — 分析搜索质量
//! - [`analyze_memory_evaluation`] — 分析 Memory 评估
//! - [`analyze_evaluator_synergy`] — 分析评估器协同
//! - [`analyze_incremental_sending`] — 分析增量发送
//! - [`generate_recommendations`] — 生成可操作建议
//! - [`analyze_dev_trace_summary`] — 主分析函数
//! - [`generate_analysis_report`] — 生成 Markdown 报告
//! - [`save_analysis_report`] — 保存报告到文件
//!
//! ## 示例
//!
//! ```
//! # use forge::dev_trace::DevTraceSummary;
//! # use forge::dev_trace_analyzer::{analyze_dev_trace_summary, generate_analysis_report};
//! let summary = DevTraceSummary::empty();
//! let analysis = analyze_dev_trace_summary(&summary);
//! let report = generate_analysis_report(&analysis);
//! assert!(report.contains("# DevTrace 智能分析报告"));
//! ```

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::dev_trace::DevTraceSummary;

// ============================================================================
//  常量
// ============================================================================

/// 分析报告文件名
pub const ANALYSIS_REPORT_FILENAME: &str = "devtrace_analysis.md";

/// 分析报告格式版本
pub const ANALYSIS_REPORT_VERSION: &str = "1.0";

// 健康度评分权重
/// 成功率权重 (0.30)
pub const WEIGHT_SUCCESS_RATE: f64 = 0.30;
/// 缓存有效性权重 (0.15)
pub const WEIGHT_CACHE: f64 = 0.15;
/// 搜索质量权重 (0.15)
pub const WEIGHT_SEARCH: f64 = 0.15;
/// Memory 评估权重 (0.10)
pub const WEIGHT_MEMORY: f64 = 0.10;
/// 评估器协同权重 (0.15)
pub const WEIGHT_SYNERGY: f64 = 0.15;
/// 增量发送权重 (0.15)
pub const WEIGHT_INCREMENTAL: f64 = 0.15;

// 建议阈值
/// 缓存命中率低阈值
pub const THRESHOLD_CACHE_HIT_RATE_LOW: f64 = 0.30;
/// 搜索差值有害阈值 (差值低于此值表示搜索有害)
pub const THRESHOLD_SEARCH_DIFF_HARMFUL: f64 = -0.10;
/// Memory 差值有害阈值
pub const THRESHOLD_MEMORY_DIFF_HARMFUL: f64 = -0.10;
/// 协同评分低阈值
pub const THRESHOLD_SYNERGY_LOW: f64 = 0.30;
/// 增量发送节省比例低阈值
pub const THRESHOLD_INCREMENTAL_SAVED_LOW: f64 = 0.10;
/// 成功率低阈值
pub const THRESHOLD_SUCCESS_RATE_LOW: f64 = 0.30;
/// 成功率中等阈值
pub const THRESHOLD_SUCCESS_RATE_MEDIUM: f64 = 0.50;

// ============================================================================
//  RecommendationSeverity — 建议严重级别
// ============================================================================

/// 建议严重级别
///
/// 控制建议的紧急程度和展示方式。
///
/// # 变体
///
/// - [`Info`](Self::Info): 信息性建议 (绿色, 不紧急)
/// - [`Warning`](Self::Warning): 警告 (黄色, 建议关注)
/// - [`Critical`](Self::Critical): 严重 (红色, 建议立即处理)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum RecommendationSeverity {
    /// 信息性建议 — 不紧急, 仅供优化参考
    #[default]
    Info,
    /// 警告 — 建议关注, 可能影响效率
    Warning,
    /// 严重 — 建议立即处理, 影响核心功能
    Critical,
}

impl std::fmt::Display for RecommendationSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

impl RecommendationSeverity {
    /// 获取中文标签
    pub fn label(&self) -> &'static str {
        match self {
            Self::Info => "信息",
            Self::Warning => "警告",
            Self::Critical => "严重",
        }
    }

    /// 获取 emoji 图标
    pub fn icon(&self) -> &'static str {
        match self {
            Self::Info => "ℹ️",
            Self::Warning => "⚠️",
            Self::Critical => "🚨",
        }
    }

    /// 获取 Markdown 颜色标签 (用于 GitHub Flavored Markdown)
    pub fn badge(&self) -> &'static str {
        match self {
            Self::Info => "blue",
            Self::Warning => "yellow",
            Self::Critical => "red",
        }
    }
}

// ============================================================================
//  AnalysisCategory — 分析维度分类
// ============================================================================

/// 分析维度分类 — 标识洞察或建议所属的功能领域
///
/// # 变体
///
/// - [`Overall`](Self::Overall): 整体概览
/// - [`Cache`](Self::Cache): 缓存相关
/// - [`Search`](Self::Search): 搜索质量相关
/// - [`Memory`](Self::Memory): Memory 上下文注入相关
/// - [`Synergy`](Self::Synergy): 评估器协同相关
/// - [`Incremental`](Self::Incremental): 增量发送相关
/// - [`JointDecision`](Self::JointDecision): 联合决策相关
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum AnalysisCategory {
    /// 整体概览
    #[default]
    Overall,
    /// 缓存相关
    Cache,
    /// 搜索质量相关
    Search,
    /// Memory 上下文注入相关
    Memory,
    /// 评估器协同相关
    Synergy,
    /// 增量发送相关
    Incremental,
    /// 联合决策相关
    JointDecision,
}

impl std::fmt::Display for AnalysisCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

impl AnalysisCategory {
    /// 获取中文标签
    pub fn label(&self) -> &'static str {
        match self {
            Self::Overall => "整体",
            Self::Cache => "缓存",
            Self::Search => "搜索",
            Self::Memory => "Memory",
            Self::Synergy => "协同",
            Self::Incremental => "增量",
            Self::JointDecision => "联合决策",
        }
    }
}

// ============================================================================
//  AnalysisRecommendation — 可操作建议
// ============================================================================

/// 单条可操作建议
///
/// 根据 DevTraceSummary 数据生成的具体改进入建议,
/// 包含严重级别、分类、标题和建议内容。
///
/// # 字段
///
/// - `severity`: 严重级别 (Info/Warning/Critical)
/// - `category`: 分析维度分类
/// - `title`: 建议标题 (一句话概述)
/// - `message`: 建议内容 (详细说明 + 具体行动)
///
/// # 示例
///
/// ```
/// # use forge::dev_trace_analyzer::{
/// #     AnalysisRecommendation, AnalysisCategory, RecommendationSeverity,
/// # };
/// let rec = AnalysisRecommendation::new(
///     RecommendationSeverity::Warning,
///     AnalysisCategory::Cache,
///     "缓存命中率偏低",
///     "缓存命中率为 20%, 低于 30% 阈值。建议检查 TTL 配置或禁用缓存。",
/// );
/// assert_eq!(rec.severity, RecommendationSeverity::Warning);
/// assert!(rec.title.contains("缓存命中率"));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisRecommendation {
    /// 严重级别
    pub severity: RecommendationSeverity,
    /// 分析维度分类
    pub category: AnalysisCategory,
    /// 建议标题 (一句话概述)
    pub title: String,
    /// 建议内容 (详细说明 + 具体行动)
    pub message: String,
}

impl AnalysisRecommendation {
    /// 创建新的建议
    ///
    /// # 参数
    ///
    /// - `severity`: 严重级别
    /// - `category`: 分析维度分类
    /// - `title`: 建议标题
    /// - `message`: 建议内容
    pub fn new(
        severity: RecommendationSeverity,
        category: AnalysisCategory,
        title: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity,
            category,
            title: title.into(),
            message: message.into(),
        }
    }

    /// 是否为严重级别
    pub fn is_critical(&self) -> bool {
        self.severity == RecommendationSeverity::Critical
    }

    /// 是否为警告级别
    pub fn is_warning(&self) -> bool {
        self.severity == RecommendationSeverity::Warning
    }
}

// ============================================================================
//  AnalysisInsight — 单条洞察
// ============================================================================

/// 单条洞察 — 从数据中提取的关键发现
///
/// 与 [`AnalysisRecommendation`] 不同, 洞察是 **描述性** 的 (陈述事实),
/// 而建议是 **规范性** 的 (建议行动)。
///
/// # 字段
///
/// - `category`: 分析维度分类
/// - `metric`: 指标名称
/// - `value`: 指标值 (格式化后的字符串)
/// - `interpretation`: 解读说明
///
/// # 示例
///
/// ```
/// # use forge::dev_trace_analyzer::{AnalysisInsight, AnalysisCategory};
/// let insight = AnalysisInsight::new(
///     AnalysisCategory::Cache,
///     "缓存命中率",
///     "65.0%",
///     "命中率在正常范围内, 缓存策略有效",
/// );
/// assert_eq!(insight.category, AnalysisCategory::Cache);
/// assert!(insight.value.contains("65"));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisInsight {
    /// 分析维度分类
    pub category: AnalysisCategory,
    /// 指标名称
    pub metric: String,
    /// 指标值 (格式化后的字符串)
    pub value: String,
    /// 解读说明
    pub interpretation: String,
}

impl AnalysisInsight {
    /// 创建新的洞察
    ///
    /// # 参数
    ///
    /// - `category`: 分析维度分类
    /// - `metric`: 指标名称
    /// - `value`: 指标值 (格式化后的字符串)
    /// - `interpretation`: 解读说明
    pub fn new(
        category: AnalysisCategory,
        metric: impl Into<String>,
        value: impl Into<String>,
        interpretation: impl Into<String>,
    ) -> Self {
        Self {
            category,
            metric: metric.into(),
            value: value.into(),
            interpretation: interpretation.into(),
        }
    }
}

// ============================================================================
//  HealthScore — 健康度评分
// ============================================================================

/// 健康度评分 — 0~100 的综合指标, 含分项明细
///
/// 通过加权多个维度 (成功率、缓存、搜索、Memory、协同、增量)
/// 压缩为单一指标, 便于快速评估 session 质量。
/// 无数据的维度权重会按比例重新分配到其他维度。
///
/// # 评分区间
///
/// - 80~100: 🟢 优秀 — 各维度表现良好
/// - 60~79: 🟡 良好 — 存在可优化项
/// - 40~59: 🟠 一般 — 建议关注警告项
/// - 0~39: 🔴 差 — 建议处理严重问题
///
/// # 字段
///
/// - `score`: 综合评分 (0.0~100.0)
/// - `success_rate_score`: 成功率维度评分 (0.0~100.0)
/// - `cache_score`: 缓存维度评分 (0.0~100.0, 无数据为 None)
/// - `search_score`: 搜索维度评分 (0.0~100.0, 无数据为 None)
/// - `memory_score`: Memory 维度评分 (0.0~100.0, 无数据为 None)
/// - `synergy_score`: 协同维度评分 (0.0~100.0, 无数据为 None)
/// - `incremental_score`: 增量维度评分 (0.0~100.0, 无数据为 None)
///
/// # 示例
///
/// ```
/// # use forge::dev_trace_analyzer::HealthScore;
/// let hs = HealthScore::new(75.0);
/// assert!((hs.score - 75.0).abs() < 0.001);
/// assert_eq!(hs.grade(), "良好");
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthScore {
    /// 综合评分 (0.0~100.0)
    pub score: f64,
    /// 成功率维度评分 (0.0~100.0)
    pub success_rate_score: f64,
    /// 缓存维度评分 (0.0~100.0, 无数据为 None)
    pub cache_score: Option<f64>,
    /// 搜索维度评分 (0.0~100.0, 无数据为 None)
    pub search_score: Option<f64>,
    /// Memory 维度评分 (0.0~100.0, 无数据为 None)
    pub memory_score: Option<f64>,
    /// 协同维度评分 (0.0~100.0, 无数据为 None)
    pub synergy_score: Option<f64>,
    /// 增量维度评分 (0.0~100.0, 无数据为 None)
    pub incremental_score: Option<f64>,
}

impl Default for HealthScore {
    fn default() -> Self {
        Self::new(0.0)
    }
}

impl HealthScore {
    /// 创建指定评分的健康度 (其他维度为默认值)
    pub fn new(score: f64) -> Self {
        Self {
            score,
            success_rate_score: 0.0,
            cache_score: None,
            search_score: None,
            memory_score: None,
            synergy_score: None,
            incremental_score: None,
        }
    }

    /// 获取评级标签
    ///
    /// - 80~100: "优秀"
    /// - 60~79: "良好"
    /// - 40~59: "一般"
    /// - 0~39: "差"
    pub fn grade(&self) -> &'static str {
        if self.score >= 80.0 {
            "优秀"
        } else if self.score >= 60.0 {
            "良好"
        } else if self.score >= 40.0 {
            "一般"
        } else {
            "差"
        }
    }

    /// 获取评级颜色 (用于 Markdown/HTML)
    pub fn grade_color(&self) -> &'static str {
        if self.score >= 80.0 {
            "green"
        } else if self.score >= 60.0 {
            "yellow"
        } else if self.score >= 40.0 {
            "orange"
        } else {
            "red"
        }
    }

    /// 获取评级 emoji
    pub fn grade_icon(&self) -> &'static str {
        if self.score >= 80.0 {
            "🟢"
        } else if self.score >= 60.0 {
            "🟡"
        } else if self.score >= 40.0 {
            "🟠"
        } else {
            "🔴"
        }
    }
}

// ============================================================================
//  DevTraceAnalysis — 完整分析结果
// ============================================================================

/// DevTrace 完整分析结果
///
/// 由 [`analyze_dev_trace_summary`] 生成, 包含健康度评分、洞察列表和建议列表。
///
/// # 字段
///
/// - `health_score`: 健康度评分
/// - `insights`: 洞察列表 (描述性发现)
/// - `recommendations`: 建议列表 (规范性建议)
/// - `summary_overview`: 概览信息 (总条目数、总耗时、成功率)
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::DevTraceSummary;
/// # use forge::dev_trace_analyzer::analyze_dev_trace_summary;
/// let summary = DevTraceSummary::empty();
/// let analysis = analyze_dev_trace_summary(&summary);
/// assert!(analysis.insights.is_empty() || !analysis.insights.is_empty()); // 总有结果
/// assert!(analysis.health_score.score >= 0.0);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DevTraceAnalysis {
    /// 健康度评分
    pub health_score: HealthScore,
    /// 洞察列表 (描述性发现)
    pub insights: Vec<AnalysisInsight>,
    /// 建议列表 (规范性建议)
    pub recommendations: Vec<AnalysisRecommendation>,
    /// 概览信息
    pub summary_overview: SummaryOverview,
}

/// 概览信息 — 从 DevTraceSummary 提取的关键概要
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SummaryOverview {
    /// 总条目数
    pub total_entries: usize,
    /// 总耗时 (毫秒)
    pub total_duration_ms: u64,
    /// 整体成功率 (0.0~1.0)
    pub success_rate: f64,
}

impl Default for SummaryOverview {
    fn default() -> Self {
        Self {
            total_entries: 0,
            total_duration_ms: 0,
            success_rate: 0.0,
        }
    }
}

// ============================================================================
//  纯函数 — 健康度评分
// ============================================================================

/// 计算健康度评分
///
/// 通过加权多个维度 (成功率、缓存、搜索、Memory、协同、增量)
/// 压缩为 0~100 的单一指标。无数据的维度权重会按比例重新分配。
///
/// # 权重分配
///
/// | 维度 | 默认权重 |
/// |------|----------|
/// | 成功率 | 0.30 |
/// | 缓存 | 0.15 |
/// | 搜索 | 0.15 |
/// | Memory | 0.10 |
/// | 协同 | 0.15 |
/// | 增量 | 0.15 |
///
/// # 参数
///
/// - `summary`: DevTraceSummary 引用
///
/// # 返回
///
/// 健康度评分 (含分项明细)
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::DevTraceSummary;
/// # use forge::dev_trace_analyzer::compute_health_score;
/// let summary = DevTraceSummary::empty();
/// let hs = compute_health_score(&summary);
/// // 空摘要: 成功率=0 → 成功率评分=0, 其他维度无数据
/// assert!(hs.score >= 0.0 && hs.score <= 100.0);
/// assert!(hs.cache_score.is_none()); // 无缓存数据
/// ```
pub fn compute_health_score(summary: &DevTraceSummary) -> HealthScore {
    // 成功率维度 (始终有数据)
    let success_rate_score = summary.success_rate * 100.0;

    // 缓存维度
    let cache_score = summary.cache_fix_correlation.as_ref().map(|corr| {
        let hit_rate = corr.hit_fix_rate();
        let diff = corr.hit_vs_miss_diff();
        // 缓存评分 = 50% 命中后修复率 + 50% 差值贡献
        let base = hit_rate * 50.0;
        let contribution = if diff > 0.0 {
            (diff * 100.0).min(50.0)
        } else {
            // 负差值扣分
            (hit_rate * 50.0 + diff * 100.0).max(0.0)
        };
        (base + contribution).clamp(0.0, 100.0).abs()
    });

    // 搜索维度
    let search_score = summary.search_quality_summary.as_ref().map(|sq| {
        let diff = sq.search_vs_no_search_diff();
        let search_rate = sq.search_success_rate();
        // 搜索评分 = 50% 搜索成功率 + 50% 差值贡献
        let base = search_rate * 50.0;
        let contribution = if diff >= 0.0 {
            (diff * 100.0).min(50.0)
        } else {
            (search_rate * 50.0 + diff * 100.0).max(0.0)
        };
        (base + contribution).clamp(0.0, 100.0).abs()
    });

    // Memory 维度
    let memory_score = summary.memory_evaluation_summary.as_ref().map(|me| {
        let diff = me.memory_vs_no_memory_diff();
        // Memory 评分基于差值: 正差值→高分, 负差值→低分
        let base = 50.0 + diff * 100.0;
        base.clamp(0.0, 100.0)
    });

    // 协同维度
    let synergy_score = summary
        .evaluator_synergy_summary
        .as_ref()
        .map(|es| es.synergy_score * 100.0);

    // 增量维度
    let incremental_score = summary.incremental_summary.as_ref().map(|inc| {
        // 增量评分 = 节省比例 * 100 (节省越多越好)
        inc.saved_ratio() * 100.0
    });

    // 计算加权评分 (无数据的维度权重重新分配)
    let mut weighted_sum = success_rate_score * WEIGHT_SUCCESS_RATE;
    let mut active_weight = WEIGHT_SUCCESS_RATE;

    if let Some(cs) = cache_score {
        weighted_sum += cs * WEIGHT_CACHE;
        active_weight += WEIGHT_CACHE;
    }
    if let Some(ss) = search_score {
        weighted_sum += ss * WEIGHT_SEARCH;
        active_weight += WEIGHT_SEARCH;
    }
    if let Some(ms) = memory_score {
        weighted_sum += ms * WEIGHT_MEMORY;
        active_weight += WEIGHT_MEMORY;
    }
    if let Some(sy) = synergy_score {
        weighted_sum += sy * WEIGHT_SYNERGY;
        active_weight += WEIGHT_SYNERGY;
    }
    if let Some(is) = incremental_score {
        weighted_sum += is * WEIGHT_INCREMENTAL;
        active_weight += WEIGHT_INCREMENTAL;
    }

    let score = (weighted_sum / active_weight).clamp(0.0, 100.0);

    HealthScore {
        score,
        success_rate_score,
        cache_score,
        search_score,
        memory_score,
        synergy_score,
        incremental_score,
    }
}

// ============================================================================
//  纯函数 — 各维度洞察分析
// ============================================================================

/// 分析缓存有效性
///
/// 从 DevTraceSummary 提取缓存相关洞察, 包括:
/// - 缓存命中率
/// - 缓存节省时间
/// - 缓存命中 vs 未命中修复率对比
///
/// # 参数
///
/// - `summary`: DevTraceSummary 引用
///
/// # 返回
///
/// 缓存维度洞察列表 (无数据时返回空列表)
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::DevTraceSummary;
/// # use forge::dev_trace_analyzer::analyze_cache_effectiveness;
/// let summary = DevTraceSummary::empty();
/// let insights = analyze_cache_effectiveness(&summary);
/// assert!(insights.is_empty()); // 无缓存数据
/// ```
pub fn analyze_cache_effectiveness(summary: &DevTraceSummary) -> Vec<AnalysisInsight> {
    let mut insights = vec![];

    // 缓存统计
    if let Some(ref cache) = summary.cache_summary {
        let hit_rate = cache.hit_rate();
        insights.push(AnalysisInsight::new(
            AnalysisCategory::Cache,
            "缓存命中率",
            format!("{:.1}%", hit_rate * 100.0),
            if hit_rate >= THRESHOLD_CACHE_HIT_RATE_LOW {
                "命中率在正常范围内, 缓存策略有效".to_string()
            } else {
                "命中率偏低, 建议检查 TTL 配置或考虑禁用缓存".to_string()
            },
        ));

        if cache.time_saved_ms > 0 {
            insights.push(AnalysisInsight::new(
                AnalysisCategory::Cache,
                "缓存节省时间",
                format_duration_human(cache.time_saved_ms),
                format!(
                    "累计节省 {} 搜索时间",
                    format_duration_human(cache.time_saved_ms)
                ),
            ));
        }
    }

    // 缓存与修复关联
    if let Some(ref corr) = summary.cache_fix_correlation {
        let hit_rate = corr.hit_fix_rate();
        let miss_rate = corr.miss_fix_rate();
        let diff = corr.hit_vs_miss_diff();

        insights.push(AnalysisInsight::new(
            AnalysisCategory::Cache,
            "命中后修复率 vs 未命中",
            format!("{:.1}% vs {:.1}%", hit_rate * 100.0, miss_rate * 100.0),
            if diff > 0.0 {
                format!(
                    "缓存命中后修复率高 {:.1}%, 缓存存储的搜索结果有效",
                    diff * 100.0
                )
            } else if diff < 0.0 {
                format!(
                    "缓存命中后修复率低 {:.1}%, 缓存可能存储了过时结果",
                    diff.abs() * 100.0
                )
            } else {
                "缓存命中与未命中修复率相同, 缓存无明显影响".to_string()
            },
        ));
    }

    // 缓存调优状态
    if let Some(ref tuning) = summary.cache_tuning_summary {
        if tuning.cache_disabled {
            insights.push(AnalysisInsight::new(
                AnalysisCategory::Cache,
                "缓存调优状态",
                "已禁用".to_string(),
                "CacheTuner 已自动禁用缓存 (基于效果分析)".to_string(),
            ));
        } else if let Some(ttl) = tuning.final_ttl {
            insights.push(AnalysisInsight::new(
                AnalysisCategory::Cache,
                "最终 TTL",
                format!("{}s", ttl),
                format!("CacheTuner 调整了 {} 次 TTL", tuning.ttl_history.len()),
            ));
        }
    }

    insights
}

/// 分析搜索质量
///
/// 从 DevTraceSummary 提取搜索相关洞察, 包括:
/// - 搜索 vs 不搜索修复率对比
/// - 搜索成功率
/// - 搜索差值趋势
///
/// # 参数
///
/// - `summary`: DevTraceSummary 引用
///
/// # 返回
///
/// 搜索维度洞察列表 (无数据时返回空列表)
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::DevTraceSummary;
/// # use forge::dev_trace_analyzer::analyze_search_quality;
/// let summary = DevTraceSummary::empty();
/// let insights = analyze_search_quality(&summary);
/// assert!(insights.is_empty()); // 无搜索数据
/// ```
pub fn analyze_search_quality(summary: &DevTraceSummary) -> Vec<AnalysisInsight> {
    let mut insights = vec![];

    if let Some(ref sq) = summary.search_quality_summary {
        let with_rate = sq.with_search_fix_rate();
        let without_rate = sq.without_search_fix_rate();
        let diff = sq.search_vs_no_search_diff();

        insights.push(AnalysisInsight::new(
            AnalysisCategory::Search,
            "搜索 vs 不搜索修复率",
            format!("{:.1}% vs {:.1}%", with_rate * 100.0, without_rate * 100.0),
            if diff > 0.0 {
                format!("搜索使修复率提升 {:.1}%, 搜索功能有效", diff * 100.0)
            } else if diff < 0.0 {
                format!(
                    "搜索使修复率降低 {:.1}%, 搜索结果可能引入噪声",
                    diff.abs() * 100.0
                )
            } else {
                "搜索对修复率无明显影响".to_string()
            },
        ));

        let search_rate = sq.search_success_rate();
        insights.push(AnalysisInsight::new(
            AnalysisCategory::Search,
            "搜索成功率",
            format!("{:.1}%", search_rate * 100.0),
            if search_rate >= 0.5 {
                "搜索成功率正常, 网络和目标网站稳定".to_string()
            } else {
                "搜索成功率偏低, 网络或目标网站可能不稳定".to_string()
            },
        ));

        if sq.total_searches > 0 {
            insights.push(AnalysisInsight::new(
                AnalysisCategory::Search,
                "总搜索次数",
                sq.total_searches.to_string(),
                format!(
                    "成功 {} 次, 失败 {} 次",
                    sq.successful_searches, sq.failed_searches
                ),
            ));
        }
    }

    insights
}

/// 分析 Memory 上下文注入效果
///
/// 从 DevTraceSummary 提取 Memory 相关洞察, 包括:
/// - 有/无注入修复率对比
/// - 注入差值
/// - 总注入次数
///
/// # 参数
///
/// - `summary`: DevTraceSummary 引用
///
/// # 返回
///
/// Memory 维度洞察列表 (无数据时返回空列表)
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::DevTraceSummary;
/// # use forge::dev_trace_analyzer::analyze_memory_evaluation;
/// let summary = DevTraceSummary::empty();
/// let insights = analyze_memory_evaluation(&summary);
/// assert!(insights.is_empty()); // 无 Memory 数据
/// ```
pub fn analyze_memory_evaluation(summary: &DevTraceSummary) -> Vec<AnalysisInsight> {
    let mut insights = vec![];

    if let Some(ref me) = summary.memory_evaluation_summary {
        let with_rate = me.with_memory_fix_rate();
        let without_rate = me.without_memory_fix_rate();
        let diff = me.memory_vs_no_memory_diff();

        insights.push(AnalysisInsight::new(
            AnalysisCategory::Memory,
            "注入 vs 不注入修复率",
            format!("{:.1}% vs {:.1}%", with_rate * 100.0, without_rate * 100.0),
            if diff > 0.0 {
                format!("Memory 注入使修复率提升 {:.1}%, 注入有效", diff * 100.0)
            } else if diff < 0.0 {
                format!(
                    "Memory 注入使修复率降低 {:.1}%, 注入可能引入噪声",
                    diff.abs() * 100.0
                )
            } else {
                "Memory 注入对修复率无明显影响".to_string()
            },
        ));

        if me.total_injections > 0 {
            insights.push(AnalysisInsight::new(
                AnalysisCategory::Memory,
                "总注入次数",
                me.total_injections.to_string(),
                format!("有 {} 次编译检查有 Memory 注入", me.checks_with_memory),
            ));
        }
    }

    insights
}

/// 分析评估器协同
///
/// 从 DevTraceSummary 提取评估器协同相关洞察, 包括:
/// - 协同评分
/// - 活跃评估器数量
/// - 是否有评估器禁用功能
///
/// # 参数
///
/// - `summary`: DevTraceSummary 引用
///
/// # 返回
///
/// 协同维度洞察列表 (无数据时返回空列表)
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::DevTraceSummary;
/// # use forge::dev_trace_analyzer::analyze_evaluator_synergy;
/// let summary = DevTraceSummary::empty();
/// let insights = analyze_evaluator_synergy(&summary);
/// assert!(insights.is_empty()); // 无协同数据
/// ```
pub fn analyze_evaluator_synergy(summary: &DevTraceSummary) -> Vec<AnalysisInsight> {
    let mut insights = vec![];

    if let Some(ref es) = summary.evaluator_synergy_summary {
        insights.push(AnalysisInsight::new(
            AnalysisCategory::Synergy,
            "协同评分",
            format!("{:.1}%", es.synergy_score * 100.0),
            if es.synergy_score >= THRESHOLD_SYNERGY_LOW {
                "协同评分在正常范围, 评估器配合良好".to_string()
            } else {
                "协同评分偏低, 评估器可能存在配置冲突".to_string()
            },
        ));

        insights.push(AnalysisInsight::new(
            AnalysisCategory::Synergy,
            "活跃评估器数",
            es.active_evaluators.to_string(),
            format!(
                "总决策 {} 次, 禁用 {} 次",
                es.total_decisions, es.total_disables
            ),
        ));

        if es.any_disabled {
            insights.push(AnalysisInsight::new(
                AnalysisCategory::Synergy,
                "功能禁用状态",
                "有禁用".to_string(),
                "至少一个评估器禁用了某项功能 (自适应调整)".to_string(),
            ));
        }

        if es.all_beneficial {
            insights.push(AnalysisInsight::new(
                AnalysisCategory::Synergy,
                "功能有效性",
                "全部有效".to_string(),
                "所有评估器都判定其功能有效, 无需禁用".to_string(),
            ));
        }
    }

    insights
}

/// 分析增量发送效果
///
/// 从 DevTraceSummary 提取增量发送相关洞察, 包括:
/// - 节省比例
/// - 总/发送/跳过消息数
/// - 平均每次发送消息数
///
/// # 参数
///
/// - `summary`: DevTraceSummary 引用
///
/// # 返回
///
/// 增量维度洞察列表 (无数据时返回空列表)
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::DevTraceSummary;
/// # use forge::dev_trace_analyzer::analyze_incremental_sending;
/// let summary = DevTraceSummary::empty();
/// let insights = analyze_incremental_sending(&summary);
/// assert!(insights.is_empty()); // 无增量数据
/// ```
pub fn analyze_incremental_sending(summary: &DevTraceSummary) -> Vec<AnalysisInsight> {
    let mut insights = vec![];

    if let Some(ref inc) = summary.incremental_summary {
        let saved = inc.saved_ratio();

        insights.push(AnalysisInsight::new(
            AnalysisCategory::Incremental,
            "节省比例",
            format!("{:.1}%", saved * 100.0),
            if saved >= 0.5 {
                "增量发送效果良好, 大量消息被跳过".to_string()
            } else if saved >= THRESHOLD_INCREMENTAL_SAVED_LOW {
                "增量发送有一定效果, 部分消息被跳过".to_string()
            } else {
                "增量发送节省比例偏低, 可能存在大量新内容".to_string()
            },
        ));

        insights.push(AnalysisInsight::new(
            AnalysisCategory::Incremental,
            "消息统计",
            format!(
                "总 {} / 发送 {} / 跳过 {}",
                inc.total_messages, inc.sent_messages, inc.skipped_messages
            ),
            format!(
                "共 {} 次增量发送, 平均每次发送 {:.1} 条",
                inc.send_count,
                inc.avg_sent_per_send()
            ),
        ));
    }

    insights
}

// ============================================================================
//  纯函数 — 生成建议
// ============================================================================

/// 根据分析结果生成可操作建议
///
/// 遍历各维度数据, 根据阈值生成具体建议。
/// 建议按严重级别排序 (Critical > Warning > Info)。
///
/// # 参数
///
/// - `summary`: DevTraceSummary 引用
///
/// # 返回
///
/// 建议列表, 按严重级别降序排列
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::DevTraceSummary;
/// # use forge::dev_trace_analyzer::generate_recommendations;
/// let summary = DevTraceSummary::empty();
/// let recs = generate_recommendations(&summary);
/// // 空摘要: 成功率=0 → Critical 建议
/// assert!(recs.iter().any(|r| r.severity == forge::dev_trace_analyzer::RecommendationSeverity::Critical));
/// ```
pub fn generate_recommendations(summary: &DevTraceSummary) -> Vec<AnalysisRecommendation> {
    let mut recs = vec![];

    // === 整体成功率 ===
    if summary.success_rate < THRESHOLD_SUCCESS_RATE_LOW {
        recs.push(AnalysisRecommendation::new(
            RecommendationSeverity::Critical,
            AnalysisCategory::Overall,
            "整体成功率很低",
            format!(
                "成功率为 {:.1}%, 低于 {}% 阈值。建议检查错误模式、网络连接和 AI 响应质量。",
                summary.success_rate * 100.0,
                (THRESHOLD_SUCCESS_RATE_LOW * 100.0) as u32,
            ),
        ));
    } else if summary.success_rate < THRESHOLD_SUCCESS_RATE_MEDIUM {
        recs.push(AnalysisRecommendation::new(
            RecommendationSeverity::Warning,
            AnalysisCategory::Overall,
            "整体成功率偏低",
            format!(
                "成功率为 {:.1}%, 低于 {}% 中等阈值。建议关注高频失败的操作类型。",
                summary.success_rate * 100.0,
                (THRESHOLD_SUCCESS_RATE_MEDIUM * 100.0) as u32,
            ),
        ));
    } else {
        recs.push(AnalysisRecommendation::new(
            RecommendationSeverity::Info,
            AnalysisCategory::Overall,
            "整体成功率良好",
            format!(
                "成功率为 {:.1}%, 在正常范围内。",
                summary.success_rate * 100.0
            ),
        ));
    }

    // === 缓存 ===
    if let Some(ref cache) = summary.cache_summary {
        let hit_rate = cache.hit_rate();
        if hit_rate < THRESHOLD_CACHE_HIT_RATE_LOW {
            recs.push(AnalysisRecommendation::new(
                RecommendationSeverity::Warning,
                AnalysisCategory::Cache,
                "缓存命中率偏低",
                format!(
                    "缓存命中率为 {:.1}%, 低于 {}% 阈值。建议调整 TTL 或考虑禁用缓存。",
                    hit_rate * 100.0,
                    (THRESHOLD_CACHE_HIT_RATE_LOW * 100.0) as u32,
                ),
            ));
        } else {
            recs.push(AnalysisRecommendation::new(
                RecommendationSeverity::Info,
                AnalysisCategory::Cache,
                "缓存命中率正常",
                format!("缓存命中率为 {:.1}%, 缓存策略有效。", hit_rate * 100.0),
            ));
        }
    }

    if let Some(ref corr) = summary.cache_fix_correlation {
        let diff = corr.hit_vs_miss_diff();
        if diff < 0.0 {
            recs.push(AnalysisRecommendation::new(
                RecommendationSeverity::Warning,
                AnalysisCategory::Cache,
                "缓存命中后修复率低于未命中",
                format!(
                    "缓存命中后修复率低 {:.1}%。缓存可能存储了过时的搜索结果, 建议缩短 TTL 或清理缓存。",
                    diff.abs() * 100.0,
                ),
            ));
        }
    }

    // === 搜索 ===
    if let Some(ref sq) = summary.search_quality_summary {
        let diff = sq.search_vs_no_search_diff();
        if diff < THRESHOLD_SEARCH_DIFF_HARMFUL {
            recs.push(AnalysisRecommendation::new(
                RecommendationSeverity::Critical,
                AnalysisCategory::Search,
                "搜索降低了修复率",
                format!(
                    "搜索使修复率降低 {:.1}%, 建议禁用自动搜索功能或更换搜索源。",
                    diff.abs() * 100.0,
                ),
            ));
        } else if diff > 0.0 {
            recs.push(AnalysisRecommendation::new(
                RecommendationSeverity::Info,
                AnalysisCategory::Search,
                "搜索提升了修复率",
                format!(
                    "搜索使修复率提升 {:.1}%, 搜索功能有效, 建议保持启用。",
                    diff * 100.0
                ),
            ));
        }

        let search_rate = sq.search_success_rate();
        if search_rate < 0.5 && sq.total_searches > 0 {
            recs.push(AnalysisRecommendation::new(
                RecommendationSeverity::Warning,
                AnalysisCategory::Search,
                "搜索成功率偏低",
                format!(
                    "搜索成功率为 {:.1}%。网络或目标网站可能不稳定, 建议检查代理或更换搜索引擎。",
                    search_rate * 100.0,
                ),
            ));
        }
    }

    // === Memory ===
    if let Some(ref me) = summary.memory_evaluation_summary {
        let diff = me.memory_vs_no_memory_diff();
        if diff < THRESHOLD_MEMORY_DIFF_HARMFUL {
            recs.push(AnalysisRecommendation::new(
                RecommendationSeverity::Critical,
                AnalysisCategory::Memory,
                "Memory 注入降低了修复率",
                format!(
                    "Memory 注入使修复率降低 {:.1}%, 建议禁用 Memory 上下文注入 (--memory-context 0)。",
                    diff.abs() * 100.0,
                ),
            ));
        } else if diff > 0.0 {
            recs.push(AnalysisRecommendation::new(
                RecommendationSeverity::Info,
                AnalysisCategory::Memory,
                "Memory 注入提升了修复率",
                format!(
                    "Memory 注入使修复率提升 {:.1}%, 注入有效, 建议保持启用。",
                    diff * 100.0
                ),
            ));
        }
    }

    // === 协同 ===
    if let Some(ref es) = summary.evaluator_synergy_summary {
        if es.synergy_score < THRESHOLD_SYNERGY_LOW {
            recs.push(AnalysisRecommendation::new(
                RecommendationSeverity::Warning,
                AnalysisCategory::Synergy,
                "评估器协同评分偏低",
                format!(
                    "协同评分为 {:.1}%, 低于 {}% 阈值。评估器可能存在配置冲突, 建议检查各评估器参数。",
                    es.synergy_score * 100.0,
                    (THRESHOLD_SYNERGY_LOW * 100.0) as u32,
                ),
            ));
        }

        if es.any_disabled {
            recs.push(AnalysisRecommendation::new(
                RecommendationSeverity::Info,
                AnalysisCategory::Synergy,
                "有评估器禁用了功能",
                "至少一个评估器自适应地禁用了某项功能。这是系统在低效数据下的自动调整, 无需干预。"
                    .to_string(),
            ));
        }
    }

    // === 增量发送 ===
    if let Some(ref inc) = summary.incremental_summary {
        let saved = inc.saved_ratio();
        if saved < THRESHOLD_INCREMENTAL_SAVED_LOW {
            recs.push(AnalysisRecommendation::new(
                RecommendationSeverity::Warning,
                AnalysisCategory::Incremental,
                "增量发送节省比例偏低",
                format!(
                    "节省比例为 {:.1}%, 低于 {}% 阈值。可能存在大量新内容, 增量发送效果有限。",
                    saved * 100.0,
                    (THRESHOLD_INCREMENTAL_SAVED_LOW * 100.0) as u32,
                ),
            ));
        } else if saved >= 0.5 {
            recs.push(AnalysisRecommendation::new(
                RecommendationSeverity::Info,
                AnalysisCategory::Incremental,
                "增量发送效果良好",
                format!(
                    "节省比例 {:.1}%, 大量消息被跳过, 增量发送有效。",
                    saved * 100.0
                ),
            ));
        }
    }

    // === 联合决策 ===
    if let Some(ref jd) = summary.joint_decision_history_summary {
        if jd.current_conservative_mode {
            recs.push(AnalysisRecommendation::new(
                RecommendationSeverity::Warning,
                AnalysisCategory::JointDecision,
                "系统运行在保守模式",
                "所有评估器已禁用自动增强功能。系统仅保留基础修复循环, 建议检查评估器配置或重置历史数据。".to_string(),
            ));
        } else if jd.escalation_rate > 0.5 {
            recs.push(AnalysisRecommendation::new(
                RecommendationSeverity::Info,
                AnalysisCategory::JointDecision,
                "多个评估器频繁升级警告",
                format!(
                    "升级率为 {:.1}%, 多个评估器经常一致判定功能有害。建议关注评估器的数据样本量。",
                    jd.escalation_rate * 100.0,
                ),
            ));
        }
    }

    // 按严重级别排序: Critical > Warning > Info
    recs.sort_by(|a, b| {
        let order = |sev: RecommendationSeverity| -> u8 {
            match sev {
                RecommendationSeverity::Critical => 0,
                RecommendationSeverity::Warning => 1,
                RecommendationSeverity::Info => 2,
            }
        };
        order(a.severity).cmp(&order(b.severity))
    });

    recs
}

// ============================================================================
//  纯函数 — 主分析函数
// ============================================================================

/// 主分析函数 — 从 DevTraceSummary 生成完整分析结果
///
/// 整合所有维度的洞察分析和建议生成, 计算健康度评分,
/// 返回完整的 [`DevTraceAnalysis`]。
///
/// # 参数
///
/// - `summary`: DevTraceSummary 引用
///
/// # 返回
///
/// 完整分析结果
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::DevTraceSummary;
/// # use forge::dev_trace_analyzer::analyze_dev_trace_summary;
/// let summary = DevTraceSummary::empty();
/// let analysis = analyze_dev_trace_summary(&summary);
/// assert!(analysis.health_score.score >= 0.0);
/// assert!(!analysis.recommendations.is_empty()); // 总有至少一条整体建议
/// ```
pub fn analyze_dev_trace_summary(summary: &DevTraceSummary) -> DevTraceAnalysis {
    let health_score = compute_health_score(summary);

    let mut insights = vec![];
    insights.extend(analyze_cache_effectiveness(summary));
    insights.extend(analyze_search_quality(summary));
    insights.extend(analyze_memory_evaluation(summary));
    insights.extend(analyze_evaluator_synergy(summary));
    insights.extend(analyze_incremental_sending(summary));

    let recommendations = generate_recommendations(summary);

    let summary_overview = SummaryOverview {
        total_entries: summary.total_entries,
        total_duration_ms: summary.total_duration_ms,
        success_rate: summary.success_rate,
    };

    DevTraceAnalysis {
        health_score,
        insights,
        recommendations,
        summary_overview,
    }
}

// ============================================================================
//  纯函数 — Markdown 报告生成
// ============================================================================

/// 生成 Markdown 分析报告
///
/// 将 [`DevTraceAnalysis`] 转换为人类可读的 Markdown 格式报告,
/// 包含概览、健康度评分、洞察列表和建议列表。
///
/// # 参数
///
/// - `analysis`: 完整分析结果
///
/// # 返回
///
/// Markdown 格式字符串
///
/// # 示例
///
/// ```
/// # use forge::dev_trace::DevTraceSummary;
/// # use forge::dev_trace_analyzer::{analyze_dev_trace_summary, generate_analysis_report};
/// let summary = DevTraceSummary::empty();
/// let analysis = analyze_dev_trace_summary(&summary);
/// let report = generate_analysis_report(&analysis);
/// assert!(report.contains("# DevTrace 智能分析报告"));
/// assert!(report.contains("健康度评分"));
/// ```
pub fn generate_analysis_report(analysis: &DevTraceAnalysis) -> String {
    let mut md = String::with_capacity(8192);

    // 标题
    md.push_str("# DevTrace 智能分析报告\n\n");
    md.push_str(&format!("> 格式版本: {}\n\n", ANALYSIS_REPORT_VERSION));

    // === 概览 ===
    md.push_str("## 概览\n\n");
    md.push_str(&format!(
        "| 指标 | 值 |\n|------|----|\n| 总条目数 | {} |\n| 总耗时 | {} |\n| 成功率 | {:.1}% |\n\n",
        analysis.summary_overview.total_entries,
        format_duration_human(analysis.summary_overview.total_duration_ms),
        analysis.summary_overview.success_rate * 100.0,
    ));

    // === 健康度评分 ===
    let hs = &analysis.health_score;
    md.push_str("## 健康度评分\n\n");
    md.push_str(&format!(
        "**{} {}** — 评分: **{:.1}/100**\n\n",
        hs.grade_icon(),
        hs.grade(),
        hs.score,
    ));

    // 评分明细表
    md.push_str("| 维度 | 评分 | 权重 |\n|------|------|------|\n");
    md.push_str(&format!(
        "| 成功率 | {:.1} | {:.0}% |\n",
        hs.success_rate_score,
        WEIGHT_SUCCESS_RATE * 100.0,
    ));
    if let Some(cs) = hs.cache_score {
        md.push_str(&format!(
            "| 缓存 | {:.1} | {:.0}% |\n",
            cs,
            WEIGHT_CACHE * 100.0,
        ));
    }
    if let Some(ss) = hs.search_score {
        md.push_str(&format!(
            "| 搜索 | {:.1} | {:.0}% |\n",
            ss,
            WEIGHT_SEARCH * 100.0,
        ));
    }
    if let Some(ms) = hs.memory_score {
        md.push_str(&format!(
            "| Memory | {:.1} | {:.0}% |\n",
            ms,
            WEIGHT_MEMORY * 100.0,
        ));
    }
    if let Some(sy) = hs.synergy_score {
        md.push_str(&format!(
            "| 协同 | {:.1} | {:.0}% |\n",
            sy,
            WEIGHT_SYNERGY * 100.0,
        ));
    }
    if let Some(is) = hs.incremental_score {
        md.push_str(&format!(
            "| 增量 | {:.1} | {:.0}% |\n",
            is,
            WEIGHT_INCREMENTAL * 100.0,
        ));
    }
    md.push_str(&format!("| **综合** | **{:.1}** | 100% |\n\n", hs.score));

    // === 洞察列表 ===
    if !analysis.insights.is_empty() {
        md.push_str("## 洞察列表\n\n");
        for insight in &analysis.insights {
            md.push_str(&format!(
                "- **[{}] {}**: `{}` — {}\n",
                insight.category, insight.metric, insight.value, insight.interpretation,
            ));
        }
        md.push('\n');
    }

    // === 建议 ===
    if !analysis.recommendations.is_empty() {
        md.push_str("## 可操作建议\n\n");
        for rec in &analysis.recommendations {
            md.push_str(&format!(
                "### {} {} [{}]\n\n{}\n\n",
                rec.severity.icon(),
                rec.title,
                rec.category,
                rec.message,
            ));
        }
    }

    md
}

/// 保存分析报告到文件
///
/// 将 Markdown 格式的分析报告写入指定路径, 自动创建父目录。
///
/// # 参数
///
/// - `report`: Markdown 报告内容
/// - `path`: 目标文件路径
///
/// # 返回
///
/// 成功返回 `Ok(())`, 失败返回错误
///
/// # 示例
///
/// ```
/// # use forge::dev_trace_analyzer::save_analysis_report;
/// # use std::fs;
/// # let dir = tempfile::tempdir().unwrap();
/// # let path = dir.path().join("analysis.md");
/// save_analysis_report("# Test", &path).unwrap();
/// assert!(path.exists());
/// let content = fs::read_to_string(&path).unwrap();
/// assert_eq!(content, "# Test");
/// ```
pub fn save_analysis_report(report: &str, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, report)?;
    Ok(())
}

/// 保存分析报告到工作区 (便捷方法)
///
/// 将报告保存到 `{workspace}/.forge/devtrace_analysis.md`。
///
/// # 参数
///
/// - `report`: Markdown 报告内容
/// - `workspace`: 工作区路径
///
/// # 返回
///
/// 成功返回 `Ok(())`, 失败返回错误
pub fn save_analysis_to_workspace(report: &str, workspace: &Path) -> Result<()> {
    let path = workspace.join(".forge").join(ANALYSIS_REPORT_FILENAME);
    save_analysis_report(report, &path)
}

// ============================================================================
//  辅助函数
// ============================================================================

/// 将毫秒格式化为人类可读的时长字符串
///
/// - < 1000ms: "123ms"
/// - < 60s: "12.3s"
/// - < 60min: "5m 30s"
/// - >= 60min: "2h 15m"
fn format_duration_human(ms: u64) -> String {
    if ms < 1000 {
        format!("{}ms", ms)
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else if ms < 3_600_000 {
        let secs = ms / 1000;
        let m = secs / 60;
        let s = secs % 60;
        format!("{}m {}s", m, s)
    } else {
        let secs = ms / 1000;
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        format!("{}h {}m", h, m)
    }
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dev_trace::{
        CacheFixCorrelation, CacheStatsSummary, CacheTuningSummary, DevTraceSummary,
        IncrementalStats, MemoryEvaluationStats, SearchQualityStats,
    };
    use crate::evaluator_synergy::{EvaluatorSnapshot, EvaluatorSynergySummary, EvaluatorType};

    // === RecommendationSeverity 测试 ===

    #[test]
    fn test_severity_labels() {
        assert_eq!(RecommendationSeverity::Info.label(), "信息");
        assert_eq!(RecommendationSeverity::Warning.label(), "警告");
        assert_eq!(RecommendationSeverity::Critical.label(), "严重");
    }

    #[test]
    fn test_severity_icons() {
        assert_eq!(RecommendationSeverity::Info.icon(), "ℹ️");
        assert_eq!(RecommendationSeverity::Warning.icon(), "⚠️");
        assert_eq!(RecommendationSeverity::Critical.icon(), "🚨");
    }

    #[test]
    fn test_severity_badges() {
        assert_eq!(RecommendationSeverity::Info.badge(), "blue");
        assert_eq!(RecommendationSeverity::Warning.badge(), "yellow");
        assert_eq!(RecommendationSeverity::Critical.badge(), "red");
    }

    #[test]
    fn test_severity_default() {
        assert_eq!(
            RecommendationSeverity::default(),
            RecommendationSeverity::Info
        );
    }

    #[test]
    fn test_severity_display() {
        assert_eq!(format!("{}", RecommendationSeverity::Info), "信息");
        assert_eq!(format!("{}", RecommendationSeverity::Warning), "警告");
        assert_eq!(format!("{}", RecommendationSeverity::Critical), "严重");
    }

    // === AnalysisCategory 测试 ===

    #[test]
    fn test_category_labels() {
        assert_eq!(AnalysisCategory::Overall.label(), "整体");
        assert_eq!(AnalysisCategory::Cache.label(), "缓存");
        assert_eq!(AnalysisCategory::Search.label(), "搜索");
        assert_eq!(AnalysisCategory::Memory.label(), "Memory");
        assert_eq!(AnalysisCategory::Synergy.label(), "协同");
        assert_eq!(AnalysisCategory::Incremental.label(), "增量");
        assert_eq!(AnalysisCategory::JointDecision.label(), "联合决策");
    }

    #[test]
    fn test_category_default() {
        assert_eq!(AnalysisCategory::default(), AnalysisCategory::Overall);
    }

    #[test]
    fn test_category_display() {
        assert_eq!(format!("{}", AnalysisCategory::Cache), "缓存");
        assert_eq!(format!("{}", AnalysisCategory::Search), "搜索");
    }

    // === AnalysisRecommendation 测试 ===

    #[test]
    fn test_recommendation_new() {
        let rec = AnalysisRecommendation::new(
            RecommendationSeverity::Warning,
            AnalysisCategory::Cache,
            "测试标题",
            "测试内容",
        );
        assert_eq!(rec.severity, RecommendationSeverity::Warning);
        assert_eq!(rec.category, AnalysisCategory::Cache);
        assert_eq!(rec.title, "测试标题");
        assert_eq!(rec.message, "测试内容");
    }

    #[test]
    fn test_recommendation_is_critical() {
        let rec = AnalysisRecommendation::new(
            RecommendationSeverity::Critical,
            AnalysisCategory::Overall,
            "严重",
            "内容",
        );
        assert!(rec.is_critical());
        assert!(!rec.is_warning());
    }

    #[test]
    fn test_recommendation_is_warning() {
        let rec = AnalysisRecommendation::new(
            RecommendationSeverity::Warning,
            AnalysisCategory::Overall,
            "警告",
            "内容",
        );
        assert!(rec.is_warning());
        assert!(!rec.is_critical());
    }

    #[test]
    fn test_recommendation_serde() {
        let rec = AnalysisRecommendation::new(
            RecommendationSeverity::Info,
            AnalysisCategory::Search,
            "搜索正常",
            "搜索功能正常",
        );
        let json = serde_json::to_string(&rec).unwrap();
        let de: AnalysisRecommendation = serde_json::from_str(&json).unwrap();
        assert_eq!(rec, de);
    }

    // === AnalysisInsight 测试 ===

    #[test]
    fn test_insight_new() {
        let insight = AnalysisInsight::new(AnalysisCategory::Cache, "命中率", "65.0%", "正常范围");
        assert_eq!(insight.category, AnalysisCategory::Cache);
        assert_eq!(insight.metric, "命中率");
        assert_eq!(insight.value, "65.0%");
        assert_eq!(insight.interpretation, "正常范围");
    }

    #[test]
    fn test_insight_serde() {
        let insight =
            AnalysisInsight::new(AnalysisCategory::Memory, "注入差值", "+10%", "注入有效");
        let json = serde_json::to_string(&insight).unwrap();
        let de: AnalysisInsight = serde_json::from_str(&json).unwrap();
        assert_eq!(insight, de);
    }

    // === HealthScore 测试 ===

    #[test]
    fn test_health_score_new() {
        let hs = HealthScore::new(75.0);
        assert!((hs.score - 75.0).abs() < 0.001);
        assert!((hs.success_rate_score - 0.0).abs() < 0.001);
        assert!(hs.cache_score.is_none());
    }

    #[test]
    fn test_health_score_grade_excellent() {
        let hs = HealthScore::new(85.0);
        assert_eq!(hs.grade(), "优秀");
        assert_eq!(hs.grade_color(), "green");
        assert_eq!(hs.grade_icon(), "🟢");
    }

    #[test]
    fn test_health_score_grade_good() {
        let hs = HealthScore::new(65.0);
        assert_eq!(hs.grade(), "良好");
        assert_eq!(hs.grade_color(), "yellow");
        assert_eq!(hs.grade_icon(), "🟡");
    }

    #[test]
    fn test_health_score_grade_average() {
        let hs = HealthScore::new(45.0);
        assert_eq!(hs.grade(), "一般");
        assert_eq!(hs.grade_color(), "orange");
        assert_eq!(hs.grade_icon(), "🟠");
    }

    #[test]
    fn test_health_score_grade_poor() {
        let hs = HealthScore::new(25.0);
        assert_eq!(hs.grade(), "差");
        assert_eq!(hs.grade_color(), "red");
        assert_eq!(hs.grade_icon(), "🔴");
    }

    #[test]
    fn test_health_score_grade_boundary_80() {
        let hs = HealthScore::new(80.0);
        assert_eq!(hs.grade(), "优秀");
    }

    #[test]
    fn test_health_score_grade_boundary_60() {
        let hs = HealthScore::new(60.0);
        assert_eq!(hs.grade(), "良好");
    }

    #[test]
    fn test_health_score_grade_boundary_40() {
        let hs = HealthScore::new(40.0);
        assert_eq!(hs.grade(), "一般");
    }

    #[test]
    fn test_health_score_grade_boundary_0() {
        let hs = HealthScore::new(0.0);
        assert_eq!(hs.grade(), "差");
    }

    #[test]
    fn test_health_score_serde() {
        let hs = HealthScore {
            score: 75.0,
            success_rate_score: 80.0,
            cache_score: Some(70.0),
            search_score: Some(60.0),
            memory_score: None,
            synergy_score: Some(85.0),
            incremental_score: None,
        };
        let json = serde_json::to_string(&hs).unwrap();
        let de: HealthScore = serde_json::from_str(&json).unwrap();
        assert_eq!(hs, de);
    }

    // === compute_health_score 测试 ===

    #[test]
    fn test_compute_health_score_empty() {
        let summary = DevTraceSummary::empty();
        let hs = compute_health_score(&summary);
        // 空摘要: 成功率=0, 所有维度 None
        assert!((hs.score - 0.0).abs() < 0.001 || hs.score >= 0.0);
        assert!(hs.cache_score.is_none());
        assert!(hs.search_score.is_none());
        assert!(hs.memory_score.is_none());
        assert!(hs.synergy_score.is_none());
        assert!(hs.incremental_score.is_none());
    }

    #[test]
    fn test_compute_health_score_full() {
        let mut summary = DevTraceSummary::empty();
        summary.success_rate = 0.8;
        summary.cache_fix_correlation = Some(CacheFixCorrelation::new());
        summary
            .cache_fix_correlation
            .as_mut()
            .unwrap()
            .record_hit_check(true);
        summary
            .cache_fix_correlation
            .as_mut()
            .unwrap()
            .record_miss_check(false);
        summary.search_quality_summary = Some(SearchQualityStats::new());
        summary
            .search_quality_summary
            .as_mut()
            .unwrap()
            .record_with_search(true);
        summary
            .search_quality_summary
            .as_mut()
            .unwrap()
            .record_without_search(false);
        summary
            .search_quality_summary
            .as_mut()
            .unwrap()
            .record_search(true);
        summary.memory_evaluation_summary = Some(MemoryEvaluationStats::new());
        summary
            .memory_evaluation_summary
            .as_mut()
            .unwrap()
            .record_with_memory(true);
        summary
            .memory_evaluation_summary
            .as_mut()
            .unwrap()
            .record_without_memory(false);
        summary.evaluator_synergy_summary = Some(EvaluatorSynergySummary::empty());
        summary
            .evaluator_synergy_summary
            .as_mut()
            .unwrap()
            .synergy_score = 0.9;
        summary.incremental_summary = Some(IncrementalStats::new());
        summary
            .incremental_summary
            .as_mut()
            .unwrap()
            .record(100, 30); // 70% saved

        let hs = compute_health_score(&summary);
        assert!(hs.score > 0.0 && hs.score <= 100.0);
        assert!((hs.success_rate_score - 80.0).abs() < 0.001);
        assert!(hs.cache_score.is_some());
        assert!(hs.search_score.is_some());
        assert!(hs.memory_score.is_some());
        assert!(hs.synergy_score.is_some());
        assert!(hs.incremental_score.is_some());
    }

    #[test]
    fn test_compute_health_score_high_success_rate() {
        let mut summary = DevTraceSummary::empty();
        summary.success_rate = 1.0;
        let hs = compute_health_score(&summary);
        assert!((hs.success_rate_score - 100.0).abs() < 0.001);
        // 只有成功率维度, 权重不重新分配
        assert!((hs.score - 100.0).abs() < 0.001);
    }

    #[test]
    fn test_compute_health_score_partial_data() {
        let mut summary = DevTraceSummary::empty();
        summary.success_rate = 0.5;
        summary.incremental_summary = Some(IncrementalStats::new());
        summary
            .incremental_summary
            .as_mut()
            .unwrap()
            .record(100, 50); // 50% saved

        let hs = compute_health_score(&summary);
        // 只有成功率和增量维度, 权重重新分配
        assert!((hs.success_rate_score - 50.0).abs() < 0.001);
        assert!(hs.incremental_score.is_some());
        assert!((hs.incremental_score.unwrap() - 50.0).abs() < 0.001);
        // 加权: (50 * 0.30 + 50 * 0.15) / (0.30 + 0.15) = 50
        assert!((hs.score - 50.0).abs() < 0.001);
    }

    // === analyze_cache_effectiveness 测试 ===

    #[test]
    fn test_analyze_cache_empty() {
        let summary = DevTraceSummary::empty();
        let insights = analyze_cache_effectiveness(&summary);
        assert!(insights.is_empty());
    }

    #[test]
    fn test_analyze_cache_with_hit_rate() {
        let mut summary = DevTraceSummary::empty();
        let mut cache = CacheStatsSummary::new();
        cache.record_hit(500);
        cache.record_hit(300);
        cache.record_miss();
        summary.cache_summary = Some(cache);

        let insights = analyze_cache_effectiveness(&summary);
        assert!(!insights.is_empty());

        // 应该有命中率洞察
        let hit_rate_insight = insights.iter().find(|i| i.metric.contains("命中率"));
        assert!(hit_rate_insight.is_some());
        // 2/3 = 66.7%
        assert!(hit_rate_insight.unwrap().value.contains("66.7"));
    }

    #[test]
    fn test_analyze_cache_low_hit_rate() {
        let mut summary = DevTraceSummary::empty();
        let mut cache = CacheStatsSummary::new();
        cache.record_hit(100);
        cache.record_miss();
        cache.record_miss();
        cache.record_miss();
        cache.record_miss(); // 1/5 = 20% < 30%
        summary.cache_summary = Some(cache);

        let insights = analyze_cache_effectiveness(&summary);
        let hit_rate_insight = insights.iter().find(|i| i.metric.contains("命中率"));
        assert!(hit_rate_insight.is_some());
        assert!(hit_rate_insight.unwrap().interpretation.contains("偏低"));
    }

    #[test]
    fn test_analyze_cache_with_correlation() {
        let mut summary = DevTraceSummary::empty();
        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_miss_check(false);
        summary.cache_fix_correlation = Some(corr);

        let insights = analyze_cache_effectiveness(&summary);
        let corr_insight = insights.iter().find(|i| i.metric.contains("vs"));
        assert!(corr_insight.is_some());
        // hit: 2/2 = 100%, miss: 0/1 = 0%
        assert!(corr_insight.unwrap().value.contains("100.0%"));
    }

    #[test]
    fn test_analyze_cache_time_saved() {
        let mut summary = DevTraceSummary::empty();
        let mut cache = CacheStatsSummary::new();
        cache.record_hit(5000);
        summary.cache_summary = Some(cache);

        let insights = analyze_cache_effectiveness(&summary);
        let time_insight = insights.iter().find(|i| i.metric.contains("节省时间"));
        assert!(time_insight.is_some());
        assert!(time_insight.unwrap().value.contains("5.0s"));
    }

    #[test]
    fn test_analyze_cache_tuning_disabled() {
        let mut summary = DevTraceSummary::empty();
        let tuning = CacheTuningSummary {
            total_evaluations: 5,
            keep_current_count: 2,
            adjust_ttl_count: 1,
            disable_count: 2,
            ttl_history: vec![(1800, 900)],
            final_ttl: Some(900),
            cache_disabled: true,
            correlation_diffs: vec![-0.1, -0.2],
        };
        summary.cache_tuning_summary = Some(tuning);

        let insights = analyze_cache_effectiveness(&summary);
        let disabled_insight = insights.iter().find(|i| i.metric.contains("调优状态"));
        assert!(disabled_insight.is_some());
        assert_eq!(disabled_insight.unwrap().value, "已禁用");
    }

    // === analyze_search_quality 测试 ===

    #[test]
    fn test_analyze_search_empty() {
        let summary = DevTraceSummary::empty();
        let insights = analyze_search_quality(&summary);
        assert!(insights.is_empty());
    }

    #[test]
    fn test_analyze_search_with_data() {
        let mut summary = DevTraceSummary::empty();
        let mut sq = SearchQualityStats::new();
        sq.record_with_search(true);
        sq.record_with_search(true);
        sq.record_without_search(false);
        sq.record_search(true);
        sq.record_search(true);
        summary.search_quality_summary = Some(sq);

        let insights = analyze_search_quality(&summary);
        assert!(!insights.is_empty());

        // 应有搜索 vs 不搜索修复率洞察
        let diff_insight = insights.iter().find(|i| i.metric.contains("vs"));
        assert!(diff_insight.is_some());
        // with: 2/2=100%, without: 0/1=0% → diff=+100%
        assert!(diff_insight.unwrap().interpretation.contains("提升"));
    }

    #[test]
    fn test_analyze_search_harmful() {
        let mut summary = DevTraceSummary::empty();
        let mut sq = SearchQualityStats::new();
        sq.record_with_search(false);
        sq.record_with_search(false);
        sq.record_without_search(true);
        sq.record_without_search(true);
        // with: 0%, without: 100% → diff=-100% → harmful
        summary.search_quality_summary = Some(sq);

        let insights = analyze_search_quality(&summary);
        let diff_insight = insights.iter().find(|i| i.metric.contains("vs"));
        assert!(diff_insight.is_some());
        assert!(diff_insight.unwrap().interpretation.contains("降低"));
    }

    #[test]
    fn test_analyze_search_success_rate() {
        let mut summary = DevTraceSummary::empty();
        let mut sq = SearchQualityStats::new();
        sq.record_search(true);
        sq.record_search(false);
        sq.record_search(false);
        // 1/3 = 33% < 50%
        sq.record_with_search(true);
        sq.record_without_search(true);
        summary.search_quality_summary = Some(sq);

        let insights = analyze_search_quality(&summary);
        let rate_insight = insights.iter().find(|i| i.metric.contains("搜索成功率"));
        assert!(rate_insight.is_some());
        assert!(rate_insight.unwrap().interpretation.contains("偏低"));
    }

    // === analyze_memory_evaluation 测试 ===

    #[test]
    fn test_analyze_memory_empty() {
        let summary = DevTraceSummary::empty();
        let insights = analyze_memory_evaluation(&summary);
        assert!(insights.is_empty());
    }

    #[test]
    fn test_analyze_memory_beneficial() {
        let mut summary = DevTraceSummary::empty();
        let mut me = MemoryEvaluationStats::new();
        me.record_with_memory(true);
        me.record_with_memory(true);
        me.record_without_memory(false);
        me.record_injection();
        // with: 100%, without: 0% → diff=+100%
        summary.memory_evaluation_summary = Some(me);

        let insights = analyze_memory_evaluation(&summary);
        assert!(!insights.is_empty());

        let diff_insight = insights.iter().find(|i| i.metric.contains("注入"));
        assert!(diff_insight.is_some());
        assert!(diff_insight.unwrap().interpretation.contains("提升"));
    }

    #[test]
    fn test_analyze_memory_harmful() {
        let mut summary = DevTraceSummary::empty();
        let mut me = MemoryEvaluationStats::new();
        me.record_with_memory(false);
        me.record_without_memory(true);
        me.record_without_memory(true);
        // with: 0%, without: 100% → diff=-100%
        summary.memory_evaluation_summary = Some(me);

        let insights = analyze_memory_evaluation(&summary);
        let diff_insight = insights.iter().find(|i| i.metric.contains("注入"));
        assert!(diff_insight.is_some());
        assert!(diff_insight.unwrap().interpretation.contains("降低"));
    }

    #[test]
    fn test_analyze_memory_injection_count() {
        let mut summary = DevTraceSummary::empty();
        let mut me = MemoryEvaluationStats::new();
        me.record_with_memory(true);
        me.record_injection();
        me.record_injection();
        me.record_injection();
        summary.memory_evaluation_summary = Some(me);

        let insights = analyze_memory_evaluation(&summary);
        let count_insight = insights.iter().find(|i| i.metric.contains("注入次数"));
        assert!(count_insight.is_some());
        assert_eq!(count_insight.unwrap().value, "3");
    }

    // === analyze_evaluator_synergy 测试 ===

    #[test]
    fn test_analyze_synergy_empty() {
        let summary = DevTraceSummary::empty();
        let insights = analyze_evaluator_synergy(&summary);
        assert!(insights.is_empty());
    }

    #[test]
    fn test_analyze_synergy_with_data() {
        let mut summary = DevTraceSummary::empty();
        let mut es = EvaluatorSynergySummary::empty();
        es.synergy_score = 0.85;
        es.active_evaluators = 3;
        es.total_decisions = 10;
        es.total_disables = 1;
        es.any_disabled = true;
        es.all_beneficial = false;
        summary.evaluator_synergy_summary = Some(es);

        let insights = analyze_evaluator_synergy(&summary);
        assert!(!insights.is_empty());

        let score_insight = insights.iter().find(|i| i.metric.contains("协同评分"));
        assert!(score_insight.is_some());
        assert!(score_insight.unwrap().interpretation.contains("正常"));

        let disabled_insight = insights.iter().find(|i| i.metric.contains("禁用状态"));
        assert!(disabled_insight.is_some());
    }

    #[test]
    fn test_analyze_synergy_low_score() {
        let mut summary = DevTraceSummary::empty();
        let mut es = EvaluatorSynergySummary::empty();
        es.synergy_score = 0.2; // < 0.3
        es.active_evaluators = 1;
        summary.evaluator_synergy_summary = Some(es);

        let insights = analyze_evaluator_synergy(&summary);
        let score_insight = insights.iter().find(|i| i.metric.contains("协同评分"));
        assert!(score_insight.is_some());
        assert!(score_insight.unwrap().interpretation.contains("偏低"));
    }

    #[test]
    fn test_analyze_synergy_all_beneficial() {
        let mut summary = DevTraceSummary::empty();
        let mut es = EvaluatorSynergySummary::empty();
        es.synergy_score = 0.9;
        es.all_beneficial = true;
        es.active_evaluators = 2;
        summary.evaluator_synergy_summary = Some(es);

        let insights = analyze_evaluator_synergy(&summary);
        let beneficial_insight = insights.iter().find(|i| i.metric.contains("有效性"));
        assert!(beneficial_insight.is_some());
        assert_eq!(beneficial_insight.unwrap().value, "全部有效");
    }

    // === analyze_incremental_sending 测试 ===

    #[test]
    fn test_analyze_incremental_empty() {
        let summary = DevTraceSummary::empty();
        let insights = analyze_incremental_sending(&summary);
        assert!(insights.is_empty());
    }

    #[test]
    fn test_analyze_incremental_high_saved() {
        let mut summary = DevTraceSummary::empty();
        let mut inc = IncrementalStats::new();
        inc.record(100, 20); // 80% saved
        summary.incremental_summary = Some(inc);

        let insights = analyze_incremental_sending(&summary);
        assert!(!insights.is_empty());

        let saved_insight = insights.iter().find(|i| i.metric.contains("节省比例"));
        assert!(saved_insight.is_some());
        assert!(saved_insight.unwrap().interpretation.contains("良好"));
    }

    #[test]
    fn test_analyze_incremental_low_saved() {
        let mut summary = DevTraceSummary::empty();
        let mut inc = IncrementalStats::new();
        inc.record(100, 95); // 5% saved < 10%
        summary.incremental_summary = Some(inc);

        let insights = analyze_incremental_sending(&summary);
        let saved_insight = insights.iter().find(|i| i.metric.contains("节省比例"));
        assert!(saved_insight.is_some());
        assert!(saved_insight.unwrap().interpretation.contains("偏低"));
    }

    #[test]
    fn test_analyze_incremental_medium_saved() {
        let mut summary = DevTraceSummary::empty();
        let mut inc = IncrementalStats::new();
        inc.record(100, 70); // 30% saved (10%~50%)
        summary.incremental_summary = Some(inc);

        let insights = analyze_incremental_sending(&summary);
        let saved_insight = insights.iter().find(|i| i.metric.contains("节省比例"));
        assert!(saved_insight.is_some());
        assert!(saved_insight.unwrap().interpretation.contains("一定效果"));
    }

    #[test]
    fn test_analyze_incremental_message_stats() {
        let mut summary = DevTraceSummary::empty();
        let mut inc = IncrementalStats::new();
        inc.record(100, 30);
        inc.record(200, 60);
        summary.incremental_summary = Some(inc);

        let insights = analyze_incremental_sending(&summary);
        let stats_insight = insights.iter().find(|i| i.metric.contains("消息统计"));
        assert!(stats_insight.is_some());
        assert!(stats_insight.unwrap().value.contains("300"));
        assert!(stats_insight.unwrap().value.contains("90"));
        assert!(stats_insight.unwrap().value.contains("210"));
    }

    // === generate_recommendations 测试 ===

    #[test]
    fn test_recommendations_empty_summary() {
        let summary = DevTraceSummary::empty();
        let recs = generate_recommendations(&summary);
        // 空摘要: 成功率=0 → Critical
        assert!(recs
            .iter()
            .any(|r| r.severity == RecommendationSeverity::Critical));
        assert!(recs.iter().any(|r| r.title.contains("成功率")));
    }

    #[test]
    fn test_recommendations_high_success_rate() {
        let mut summary = DevTraceSummary::empty();
        summary.success_rate = 0.9;
        let recs = generate_recommendations(&summary);
        let overall = recs
            .iter()
            .find(|r| r.category == AnalysisCategory::Overall);
        assert!(overall.is_some());
        assert_eq!(overall.unwrap().severity, RecommendationSeverity::Info);
    }

    #[test]
    fn test_recommendations_medium_success_rate() {
        let mut summary = DevTraceSummary::empty();
        summary.success_rate = 0.4; // 30% < 40% < 50%
        let recs = generate_recommendations(&summary);
        let overall = recs
            .iter()
            .find(|r| r.category == AnalysisCategory::Overall);
        assert!(overall.is_some());
        assert_eq!(overall.unwrap().severity, RecommendationSeverity::Warning);
    }

    #[test]
    fn test_recommendations_low_success_rate() {
        let mut summary = DevTraceSummary::empty();
        summary.success_rate = 0.2; // < 30%
        let recs = generate_recommendations(&summary);
        let overall = recs
            .iter()
            .find(|r| r.category == AnalysisCategory::Overall);
        assert!(overall.is_some());
        assert_eq!(overall.unwrap().severity, RecommendationSeverity::Critical);
    }

    #[test]
    fn test_recommendations_cache_low_hit_rate() {
        let mut summary = DevTraceSummary::empty();
        summary.success_rate = 0.8;
        let mut cache = CacheStatsSummary::new();
        cache.record_hit(100);
        cache.record_miss();
        cache.record_miss();
        cache.record_miss();
        cache.record_miss(); // 1/5 = 20% < 30%
        summary.cache_summary = Some(cache);

        let recs = generate_recommendations(&summary);
        assert!(recs.iter().any(|r| r.category == AnalysisCategory::Cache
            && r.severity == RecommendationSeverity::Warning));
    }

    #[test]
    fn test_recommendations_search_harmful() {
        let mut summary = DevTraceSummary::empty();
        summary.success_rate = 0.8;
        let mut sq = SearchQualityStats::new();
        sq.record_with_search(false);
        sq.record_without_search(true);
        sq.record_without_search(true);
        // diff = -100% < -10%
        summary.search_quality_summary = Some(sq);

        let recs = generate_recommendations(&summary);
        assert!(recs.iter().any(|r| r.category == AnalysisCategory::Search
            && r.severity == RecommendationSeverity::Critical));
    }

    #[test]
    fn test_recommendations_memory_harmful() {
        let mut summary = DevTraceSummary::empty();
        summary.success_rate = 0.8;
        let mut me = MemoryEvaluationStats::new();
        me.record_with_memory(false);
        me.record_without_memory(true);
        // diff = -100% < -10%
        summary.memory_evaluation_summary = Some(me);

        let recs = generate_recommendations(&summary);
        assert!(recs.iter().any(|r| r.category == AnalysisCategory::Memory
            && r.severity == RecommendationSeverity::Critical));
    }

    #[test]
    fn test_recommendations_synergy_low() {
        let mut summary = DevTraceSummary::empty();
        summary.success_rate = 0.8;
        let mut es = EvaluatorSynergySummary::empty();
        es.synergy_score = 0.2; // < 0.3
        es.active_evaluators = 2;
        summary.evaluator_synergy_summary = Some(es);

        let recs = generate_recommendations(&summary);
        assert!(recs.iter().any(|r| r.category == AnalysisCategory::Synergy
            && r.severity == RecommendationSeverity::Warning));
    }

    #[test]
    fn test_recommendations_incremental_low_saved() {
        let mut summary = DevTraceSummary::empty();
        summary.success_rate = 0.8;
        let mut inc = IncrementalStats::new();
        inc.record(100, 95); // 5% < 10%
        summary.incremental_summary = Some(inc);

        let recs = generate_recommendations(&summary);
        assert!(recs
            .iter()
            .any(|r| r.category == AnalysisCategory::Incremental
                && r.severity == RecommendationSeverity::Warning));
    }

    #[test]
    fn test_recommendations_sorted_by_severity() {
        let summary = DevTraceSummary::empty();
        let recs = generate_recommendations(&summary);
        // 至少有一条 Critical (成功率=0)
        assert!(!recs.is_empty());
        // 验证排序: Critical 在 Warning 之前
        let critical_pos = recs
            .iter()
            .position(|r| r.severity == RecommendationSeverity::Critical);
        let warning_pos = recs
            .iter()
            .position(|r| r.severity == RecommendationSeverity::Warning);
        if let (Some(c), Some(w)) = (critical_pos, warning_pos) {
            assert!(c < w);
        }
    }

    #[test]
    fn test_recommendations_cache_harmful_correlation() {
        let mut summary = DevTraceSummary::empty();
        summary.success_rate = 0.8;
        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(false);
        corr.record_miss_check(true);
        // hit: 0%, miss: 100% → diff = -100% < 0
        summary.cache_fix_correlation = Some(corr);

        let recs = generate_recommendations(&summary);
        assert!(recs
            .iter()
            .any(|r| r.category == AnalysisCategory::Cache && r.title.contains("修复率低于")));
    }

    // === analyze_dev_trace_summary 测试 ===

    #[test]
    fn test_analyze_empty() {
        let summary = DevTraceSummary::empty();
        let analysis = analyze_dev_trace_summary(&summary);

        assert!(analysis.health_score.score >= 0.0);
        assert!(analysis.insights.is_empty());
        assert!(!analysis.recommendations.is_empty()); // 至少有整体建议
        assert_eq!(analysis.summary_overview.total_entries, 0);
    }

    #[test]
    fn test_analyze_full() {
        let mut summary = DevTraceSummary::empty();
        summary.success_rate = 0.85;
        summary.total_entries = 100;
        summary.total_duration_ms = 3_600_000;

        let mut cache = CacheStatsSummary::new();
        cache.record_hit(500);
        cache.record_miss();
        summary.cache_summary = Some(cache);

        let mut sq = SearchQualityStats::new();
        sq.record_with_search(true);
        sq.record_without_search(false);
        sq.record_search(true);
        summary.search_quality_summary = Some(sq);

        let analysis = analyze_dev_trace_summary(&summary);

        assert_eq!(analysis.summary_overview.total_entries, 100);
        assert!(!analysis.insights.is_empty());
        assert!(!analysis.recommendations.is_empty());
        assert!(analysis.health_score.score > 0.0);
    }

    #[test]
    fn test_analyze_serde() {
        let summary = DevTraceSummary::empty();
        let analysis = analyze_dev_trace_summary(&summary);
        let json = serde_json::to_string(&analysis).unwrap();
        let de: DevTraceAnalysis = serde_json::from_str(&json).unwrap();
        assert_eq!(analysis, de);
    }

    // === generate_analysis_report 测试 ===

    #[test]
    fn test_report_contains_title() {
        let summary = DevTraceSummary::empty();
        let analysis = analyze_dev_trace_summary(&summary);
        let report = generate_analysis_report(&analysis);
        assert!(report.contains("# DevTrace 智能分析报告"));
    }

    #[test]
    fn test_report_contains_version() {
        let summary = DevTraceSummary::empty();
        let analysis = analyze_dev_trace_summary(&summary);
        let report = generate_analysis_report(&analysis);
        assert!(report.contains(ANALYSIS_REPORT_VERSION));
    }

    #[test]
    fn test_report_contains_overview() {
        let mut summary = DevTraceSummary::empty();
        summary.total_entries = 42;
        summary.success_rate = 0.75;
        let analysis = analyze_dev_trace_summary(&summary);
        let report = generate_analysis_report(&analysis);
        assert!(report.contains("概览"));
        assert!(report.contains("42"));
        assert!(report.contains("75.0%"));
    }

    #[test]
    fn test_report_contains_health_score() {
        let summary = DevTraceSummary::empty();
        let analysis = analyze_dev_trace_summary(&summary);
        let report = generate_analysis_report(&analysis);
        assert!(report.contains("健康度评分"));
        assert!(report.contains("成功率"));
    }

    #[test]
    fn test_report_contains_insights() {
        let mut summary = DevTraceSummary::empty();
        summary.success_rate = 0.8;
        let mut cache = CacheStatsSummary::new();
        cache.record_hit(500);
        cache.record_miss();
        summary.cache_summary = Some(cache);
        let analysis = analyze_dev_trace_summary(&summary);
        let report = generate_analysis_report(&analysis);
        assert!(report.contains("洞察列表"));
        assert!(report.contains("缓存"));
    }

    #[test]
    fn test_report_contains_recommendations() {
        let summary = DevTraceSummary::empty();
        let analysis = analyze_dev_trace_summary(&summary);
        let report = generate_analysis_report(&analysis);
        assert!(report.contains("可操作建议"));
    }

    #[test]
    fn test_report_no_insights_section_when_empty() {
        let summary = DevTraceSummary::empty();
        let analysis = analyze_dev_trace_summary(&summary);
        let report = generate_analysis_report(&analysis);
        // 空摘要没有洞察
        assert!(!report.contains("洞察列表"));
    }

    #[test]
    fn test_report_health_score_dimensions() {
        let mut summary = DevTraceSummary::empty();
        summary.success_rate = 0.8;
        let mut cache = CacheStatsSummary::new();
        cache.record_hit(500);
        cache.record_miss();
        summary.cache_summary = Some(cache);
        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(true);
        corr.record_miss_check(false);
        summary.cache_fix_correlation = Some(corr);
        let mut inc = IncrementalStats::new();
        inc.record(100, 30);
        summary.incremental_summary = Some(inc);

        let analysis = analyze_dev_trace_summary(&summary);
        let report = generate_analysis_report(&analysis);
        assert!(report.contains("| 缓存 |"));
        assert!(report.contains("| 增量 |"));
    }

    // === save_analysis_report 测试 ===

    #[test]
    fn test_save_analysis_report() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("analysis.md");
        save_analysis_report("# Test Report", &path).unwrap();
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "# Test Report");
    }

    #[test]
    fn test_save_analysis_report_creates_parent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("subdir").join("nested").join("analysis.md");
        save_analysis_report("# Nested", &path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn test_save_analysis_to_workspace() {
        let dir = tempfile::tempdir().unwrap();
        save_analysis_to_workspace("# Workspace", dir.path()).unwrap();
        let path = dir.path().join(".forge").join(ANALYSIS_REPORT_FILENAME);
        assert!(path.exists());
    }

    // === format_duration_human 测试 ===

    #[test]
    fn test_format_duration_ms() {
        assert_eq!(format_duration_human(500), "500ms");
        assert_eq!(format_duration_human(999), "999ms");
    }

    #[test]
    fn test_format_duration_seconds() {
        assert_eq!(format_duration_human(1000), "1.0s");
        assert_eq!(format_duration_human(5500), "5.5s");
        assert_eq!(format_duration_human(59000), "59.0s");
    }

    #[test]
    fn test_format_duration_minutes() {
        assert_eq!(format_duration_human(60000), "1m 0s");
        assert_eq!(format_duration_human(90000), "1m 30s");
        assert_eq!(format_duration_human(330000), "5m 30s");
    }

    #[test]
    fn test_format_duration_hours() {
        assert_eq!(format_duration_human(3_600_000), "1h 0m");
        assert_eq!(format_duration_human(8_100_000), "2h 15m");
    }

    #[test]
    fn test_format_duration_zero() {
        assert_eq!(format_duration_human(0), "0ms");
    }

    // === SummaryOverview 测试 ===

    #[test]
    fn test_summary_overview_default() {
        let so = SummaryOverview::default();
        assert_eq!(so.total_entries, 0);
        assert_eq!(so.total_duration_ms, 0);
        assert!((so.success_rate - 0.0).abs() < 0.001);
    }

    // === DevTraceAnalysis 测试 ===

    #[test]
    fn test_analysis_default() {
        let analysis = DevTraceAnalysis::default();
        assert!((analysis.health_score.score - 0.0).abs() < 0.001);
        assert!(analysis.insights.is_empty());
        assert!(analysis.recommendations.is_empty());
    }

    #[test]
    fn test_analysis_with_joint_decision() {
        use crate::joint_decision::{JointDecisionAction, JointDecisionHistorySummary};

        let mut summary = DevTraceSummary::empty();
        summary.success_rate = 0.8;
        summary.joint_decision_history_summary = Some(JointDecisionHistorySummary {
            session_count: 3,
            latest_action: JointDecisionAction::EnterConservativeMode,
            total_decisions: 15,
            total_escalations: 5,
            total_conservative_modes: 2,
            conservative_mode_rate: 0.4,
            escalation_rate: 0.33,
            current_conservative_mode: true,
            saved_at: None,
        });

        let recs = generate_recommendations(&summary);
        assert!(
            recs.iter()
                .any(|r| r.category == AnalysisCategory::JointDecision
                    && r.title.contains("保守模式"))
        );
    }

    #[test]
    fn test_analysis_with_joint_decision_high_escalation() {
        use crate::joint_decision::{JointDecisionAction, JointDecisionHistorySummary};

        let mut summary = DevTraceSummary::empty();
        summary.success_rate = 0.8;
        summary.joint_decision_history_summary = Some(JointDecisionHistorySummary {
            session_count: 5,
            latest_action: JointDecisionAction::EscalateWarning,
            total_decisions: 20,
            total_escalations: 15,
            total_conservative_modes: 0,
            conservative_mode_rate: 0.0,
            escalation_rate: 0.75, // > 0.5
            current_conservative_mode: false,
            saved_at: None,
        });

        let recs = generate_recommendations(&summary);
        assert!(
            recs.iter()
                .any(|r| r.category == AnalysisCategory::JointDecision
                    && r.title.contains("升级警告"))
        );
    }

    // === 集成式测试 ===

    #[test]
    fn test_full_analysis_pipeline() {
        let mut summary = DevTraceSummary::empty();
        summary.success_rate = 0.75;
        summary.total_entries = 200;
        summary.total_duration_ms = 7_200_000; // 2h

        let mut cache = CacheStatsSummary::new();
        cache.record_hit(3000);
        cache.record_hit(2000);
        cache.record_miss();
        summary.cache_summary = Some(cache);

        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_miss_check(false);
        summary.cache_fix_correlation = Some(corr);

        let mut sq = SearchQualityStats::new();
        sq.record_with_search(true);
        sq.record_without_search(false);
        sq.record_search(true);
        sq.record_search(false);
        summary.search_quality_summary = Some(sq);

        let mut me = MemoryEvaluationStats::new();
        me.record_with_memory(true);
        me.record_without_memory(false);
        me.record_injection();
        summary.memory_evaluation_summary = Some(me);

        let mut es = EvaluatorSynergySummary::empty();
        es.synergy_score = 0.85;
        es.active_evaluators = 3;
        es.total_decisions = 12;
        es.total_disables = 0;
        es.all_beneficial = true;
        summary.evaluator_synergy_summary = Some(es);

        let mut inc = IncrementalStats::new();
        inc.record(100, 30);
        inc.record(200, 60);
        summary.incremental_summary = Some(inc);

        // 完整分析管道
        let analysis = analyze_dev_trace_summary(&summary);
        assert!(analysis.health_score.score > 0.0);

        // 生成报告
        let report = generate_analysis_report(&analysis);
        assert!(report.contains("# DevTrace 智能分析报告"));
        assert!(report.contains("概览"));
        assert!(report.contains("健康度评分"));
        assert!(report.contains("洞察列表"));
        assert!(report.contains("可操作建议"));

        // 保存报告
        let dir = tempfile::tempdir().unwrap();
        save_analysis_to_workspace(&report, dir.path()).unwrap();
        let path = dir.path().join(".forge").join(ANALYSIS_REPORT_FILENAME);
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("# DevTrace 智能分析报告"));
    }

    #[test]
    fn test_analysis_with_evaluator_snapshots() {
        let mut summary = DevTraceSummary::empty();
        summary.success_rate = 0.8;
        let mut es = EvaluatorSynergySummary::empty();
        es.synergy_score = 0.6;
        es.active_evaluators = 2;
        es.snapshots = vec![
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
                contribution_score: 0.2,
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
                contribution_score: -0.4,
            },
        ];
        es.any_disabled = true;
        summary.evaluator_synergy_summary = Some(es);

        let analysis = analyze_dev_trace_summary(&summary);
        assert!(!analysis.insights.is_empty());

        // 应有协同评分洞察
        assert!(analysis
            .insights
            .iter()
            .any(|i| i.metric.contains("协同评分")));
        // 应有功能禁用洞察
        assert!(analysis.insights.iter().any(|i| i.metric.contains("禁用")));
        // 应有协同建议
        assert!(analysis
            .recommendations
            .iter()
            .any(|r| r.category == AnalysisCategory::Synergy));
    }
}
