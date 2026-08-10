//! Search Quality — 自动搜索结果质量评估 (Session 85)
//!
//! 基于 `SearchQualityStats` 分析结果, 评估自动搜索是否真正提高了
//! 修复成功率, 并在搜索效果不足时自动禁用搜索功能。
//!
//! ## 设计理念
//!
//! Session 77 引入了编译错误自动搜索 (web_tool 深度集成),
//! Session 78-84 逐步完善了搜索缓存和缓存调优。
//!
//! 然而, 一个关键问题尚未回答: **自动搜索本身是否真的有效?**
//!
//! 本模块利用 `SearchQualityStats` 的 "有搜索 vs 无搜索" 修复成功率对比,
//! 实现搜索策略的自动评估:
//!
//! 1. **搜索有害** (`diff < disable_threshold`): 搜索后修复率低于不搜索 → 禁用搜索
//! 2. **搜索有效** (`diff >= beneficial_threshold`): 搜索后修复率高于不搜索 → 保持搜索
//! 3. **数据不足** 或 **持平**: 保持当前配置
//!
//! ## 纯函数架构 (SRP)
//!
//! - [`has_sufficient_search_data`][]: 判断统计是否有足够数据做决策
//! - [`should_disable_search`][]: 判断是否应该禁用搜索
//! - [`compute_search_quality_decision`][]: 综合分析, 生成质量决策
//!
//! ## 示例
//!
//! ```
//! use forge::search_quality::{SearchQualityConfig, compute_search_quality_decision};
//! use forge::dev_trace::SearchQualityStats;
//!
//! let mut stats = SearchQualityStats::new();
//! // 搜索后修复率低 (3次中0次通过)
//! stats.record_with_search(false);
//! stats.record_with_search(false);
//! stats.record_with_search(false);
//! // 不搜索修复率高 (3次中3次通过)
//! stats.record_without_search(true);
//! stats.record_without_search(true);
//! stats.record_without_search(true);
//!
//! let config = SearchQualityConfig::default();
//! let decision = compute_search_quality_decision(&stats, &config);
//! assert!(decision.is_disable());
//! ```

use crate::dev_trace::SearchQualityStats;
use serde::{Deserialize, Serialize};

// ============================================================================
//  常量
// ============================================================================

/// 默认最小样本数 — 至少需要这么多数据点才做决策
pub const DEFAULT_MIN_SAMPLES: usize = 5;

/// 默认禁用阈值 — diff 低于此值 (如 -0.10 = -10%) 时禁用搜索
///
/// 搜索后修复率比不搜索低 10% 以上 → 搜索有害, 禁用
pub const DEFAULT_DISABLE_THRESHOLD: f64 = -0.10;

/// 默认有益阈值 — diff 高于此值 (如 0.05 = 5%) 时搜索明确有效
///
/// 搜索后修复率比不搜索高 5% 以上 → 搜索有效, 保持
pub const DEFAULT_BENEFICIAL_THRESHOLD: f64 = 0.05;

// ============================================================================
//  SearchQualityConfig — 质量评估配置
// ============================================================================

/// 搜索质量评估配置
///
/// 控制何时禁用搜索、何时保持搜索、最少需要多少样本。
///
/// # 字段
///
/// | 字段 | 说明 | 默认值 |
/// |------|------|--------|
/// | `min_samples` | 最少样本数 | 5 |
/// | `disable_threshold` | 禁用阈值 (diff 低于此值则禁用) | -0.10 |
/// | `beneficial_threshold` | 有益阈值 (diff 高于此值则明确有效) | 0.05 |
///
/// # 示例
///
/// ```
/// use forge::search_quality::SearchQualityConfig;
///
/// let config = SearchQualityConfig::default();
/// assert_eq!(config.min_samples, 5);
/// assert!((config.disable_threshold - (-0.10)).abs() < 0.001);
/// assert!((config.beneficial_threshold - 0.05).abs() < 0.001);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchQualityConfig {
    /// 最少样本数 (总编译检查次数)
    pub min_samples: usize,
    /// 禁用阈值 — diff 低于此值时禁用搜索
    pub disable_threshold: f64,
    /// 有益阈值 — diff 高于此值时搜索明确有效
    pub beneficial_threshold: f64,
}

impl Default for SearchQualityConfig {
    fn default() -> Self {
        Self {
            min_samples: DEFAULT_MIN_SAMPLES,
            disable_threshold: DEFAULT_DISABLE_THRESHOLD,
            beneficial_threshold: DEFAULT_BENEFICIAL_THRESHOLD,
        }
    }
}

impl SearchQualityConfig {
    /// 创建严格配置 (更早禁用, 更多样本要求)
    ///
    /// 适用于对搜索质量要求高的场景:
    /// - min_samples = 3 (更早评估)
    /// - disable_threshold = -0.05 (更严格, 稍差就禁用)
    /// - beneficial_threshold = 0.10 (更高标准才算有效)
    pub fn strict() -> Self {
        Self {
            min_samples: 3,
            disable_threshold: -0.05,
            beneficial_threshold: 0.10,
        }
    }

    /// 创建宽松配置 (更晚禁用, 更少样本要求)
    ///
    /// 适用于希望尽可能保留搜索功能的场景:
    /// - min_samples = 8 (更多数据)
    /// - disable_threshold = -0.20 (更宽松, 差很多才禁用)
    /// - beneficial_threshold = 0.02 (更低标准就算有效)
    pub fn lenient() -> Self {
        Self {
            min_samples: 8,
            disable_threshold: -0.20,
            beneficial_threshold: 0.02,
        }
    }
}

// ============================================================================
//  SearchQualityDecision — 质量决策
// ============================================================================

/// 搜索质量决策 — 评估搜索效果后的行动建议
///
/// # 变体
///
/// | 变体 | 说明 |
/// |------|------|
/// | `KeepSearching` | 搜索有效或中性, 继续搜索 |
/// | `DisableSearch` | 搜索有害, 应禁用 |
/// | `InsufficientData` | 数据不足, 无法判断 |
///
/// # 示例
///
/// ```
/// use forge::search_quality::{SearchQualityDecision, SearchQualityConfig};
/// use forge::dev_trace::SearchQualityStats;
///
/// let mut stats = SearchQualityStats::new();
/// // 数据不足
/// stats.record_with_search(true);
/// let config = SearchQualityConfig::default();
/// let decision = forge::search_quality::compute_search_quality_decision(&stats, &config);
/// assert!(decision.is_insufficient_data());
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchQualityDecision {
    /// 决策动作
    pub action: SearchQualityAction,
    /// 搜索与不搜索的修复成功率差值 (-1.0 ~ 1.0)
    pub diff: f64,
    /// 决策原因
    pub reason: String,
}

/// 搜索质量决策动作
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchQualityAction {
    /// 保持搜索 (搜索有效或中性)
    KeepSearching,
    /// 禁用搜索 (搜索有害)
    DisableSearch,
    /// 数据不足, 无法判断
    InsufficientData,
}

impl SearchQualityDecision {
    /// 创建 "保持搜索" 决策
    pub fn keep_searching(diff: f64, reason: &str) -> Self {
        Self {
            action: SearchQualityAction::KeepSearching,
            diff,
            reason: reason.to_string(),
        }
    }

    /// 创建 "禁用搜索" 决策
    pub fn disable_search(diff: f64, reason: &str) -> Self {
        Self {
            action: SearchQualityAction::DisableSearch,
            diff,
            reason: reason.to_string(),
        }
    }

    /// 创建 "数据不足" 决策
    pub fn insufficient_data(diff: f64, reason: &str) -> Self {
        Self {
            action: SearchQualityAction::InsufficientData,
            diff,
            reason: reason.to_string(),
        }
    }

    /// 是否为 "禁用搜索" 决策
    pub fn is_disable(&self) -> bool {
        self.action == SearchQualityAction::DisableSearch
    }

    /// 是否为 "保持搜索" 决策
    pub fn is_keep(&self) -> bool {
        self.action == SearchQualityAction::KeepSearching
    }

    /// 是否为 "数据不足" 决策
    pub fn is_insufficient_data(&self) -> bool {
        self.action == SearchQualityAction::InsufficientData
    }

    /// 格式化为可读字符串 (用于 DevTrace 记录)
    pub fn to_trace_summary(&self) -> String {
        let action_str = match self.action {
            SearchQualityAction::KeepSearching => "保持搜索",
            SearchQualityAction::DisableSearch => "禁用搜索",
            SearchQualityAction::InsufficientData => "数据不足",
        };
        format!(
            "搜索质量: {} (差值 {:+.1}%, 原因: {})",
            action_str,
            self.diff * 100.0,
            self.reason
        )
    }
}

// ============================================================================
//  纯函数 — 质量评估决策
// ============================================================================

/// 判断统计是否有足够的数据做决策
///
/// 需要同时有 with_search 和 without_search 的数据,
/// 且总编译检查次数 >= `min_samples`。
///
/// # 参数
///
/// - `stats`: 搜索质量统计
/// - `min_samples`: 最少样本数
///
/// # 示例
///
/// ```
/// # use forge::search_quality::has_sufficient_search_data;
/// # use forge::dev_trace::SearchQualityStats;
/// let mut stats = SearchQualityStats::new();
/// assert!(!has_sufficient_search_data(&stats, 5));
///
/// stats.record_with_search(true);
/// stats.record_without_search(false);
/// assert!(!has_sufficient_search_data(&stats, 5)); // 只有 2 个样本
///
/// for _ in 0..3 {
///     stats.record_with_search(true);
///     stats.record_without_search(false);
/// }
/// assert!(has_sufficient_search_data(&stats, 5)); // 8 个样本
/// ```
pub fn has_sufficient_search_data(stats: &SearchQualityStats, min_samples: usize) -> bool {
    stats.has_sufficient_data(min_samples)
}

/// 判断是否应该禁用搜索
///
/// 当搜索修复率明显低于不搜索修复率 (diff < disable_threshold) 时返回 true。
///
/// # 参数
///
/// - `stats`: 搜索质量统计
/// - `config`: 质量评估配置
///
/// # 示例
///
/// ```
/// # use forge::search_quality::{should_disable_search, SearchQualityConfig};
/// # use forge::dev_trace::SearchQualityStats;
/// let mut stats = SearchQualityStats::new();
/// // 搜索后全部失败
/// stats.record_with_search(false);
/// stats.record_with_search(false);
/// stats.record_with_search(false);
/// // 不搜索全部成功
/// stats.record_without_search(true);
/// stats.record_without_search(true);
/// stats.record_without_search(true);
///
/// let config = SearchQualityConfig::default();
/// assert!(should_disable_search(&stats, &config));
/// ```
pub fn should_disable_search(stats: &SearchQualityStats, config: &SearchQualityConfig) -> bool {
    if !has_sufficient_search_data(stats, config.min_samples) {
        return false;
    }
    stats.search_vs_no_search_diff() < config.disable_threshold
}

/// 综合分析搜索质量, 生成决策
///
/// # 决策逻辑
///
/// 1. 数据不足 → `InsufficientData`
/// 2. diff < disable_threshold → `DisableSearch` (搜索有害)
/// 3. diff >= beneficial_threshold → `KeepSearching` (搜索有效)
/// 4. 其他 → `KeepSearching` (中性, 不禁用)
///
/// # 参数
///
/// - `stats`: 搜索质量统计
/// - `config`: 质量评估配置
///
/// # 示例
///
/// ```
/// # use forge::search_quality::{compute_search_quality_decision, SearchQualityConfig};
/// # use forge::dev_trace::SearchQualityStats;
/// let mut stats = SearchQualityStats::new();
/// // 搜索后修复率高
/// stats.record_with_search(true);
/// stats.record_with_search(true);
/// stats.record_with_search(true);
/// // 不搜索修复率低
/// stats.record_without_search(false);
/// stats.record_without_search(false);
/// stats.record_without_search(false);
///
/// let config = SearchQualityConfig::default();
/// let decision = compute_search_quality_decision(&stats, &config);
/// assert!(decision.is_keep()); // 搜索有效
/// ```
pub fn compute_search_quality_decision(
    stats: &SearchQualityStats,
    config: &SearchQualityConfig,
) -> SearchQualityDecision {
    let diff = stats.search_vs_no_search_diff();

    // 1. 数据不足
    if !has_sufficient_search_data(stats, config.min_samples) {
        return SearchQualityDecision::insufficient_data(
            diff,
            &format!(
                "样本不足 (有搜索 {}, 无搜索 {}, 需要至少 {})",
                stats.checks_with_search, stats.checks_without_search, config.min_samples
            ),
        );
    }

    // 2. 搜索有害 → 禁用
    if diff < config.disable_threshold {
        return SearchQualityDecision::disable_search(
            diff,
            &format!(
                "搜索修复率 {:.1}% 低于无搜索 {:.1}%, 差值 {:+.1}% < {:+.0}%",
                stats.with_search_fix_rate() * 100.0,
                stats.without_search_fix_rate() * 100.0,
                diff * 100.0,
                config.disable_threshold * 100.0
            ),
        );
    }

    // 3. 搜索有效 → 保持
    if diff >= config.beneficial_threshold {
        return SearchQualityDecision::keep_searching(
            diff,
            &format!(
                "搜索修复率 {:.1}% 高于无搜索 {:.1}%, 差值 {:+.1}% >= {:+.0}%",
                stats.with_search_fix_rate() * 100.0,
                stats.without_search_fix_rate() * 100.0,
                diff * 100.0,
                config.beneficial_threshold * 100.0
            ),
        );
    }

    // 4. 中性 → 保持 (不禁用)
    SearchQualityDecision::keep_searching(
        diff,
        &format!(
            "差值 {:+.1}% 在正常范围 [{:+.0}%, {:+.0}%], 无需调整",
            diff * 100.0,
            config.disable_threshold * 100.0,
            config.beneficial_threshold * 100.0
        ),
    )
}

// ============================================================================
//  SearchQualityEvaluator — 搜索质量评估器 (状态管理)
// ============================================================================

/// 搜索质量评估器 — 管理自动搜索的启用/禁用状态
///
/// # 设计
///
/// `SearchQualityEvaluator` 封装了搜索质量评估的状态和逻辑:
/// - `config`: 评估配置 (阈值和参数)
/// - `enabled`: 搜索是否启用 (禁用后不再执行自动搜索)
/// - `evaluation_count`: 累计评估次数
/// - `disable_count`: 累计禁用次数
/// - `last_decision`: 最近一次质量决策
///
/// # 用法
///
/// ```
/// use forge::search_quality::{SearchQualityEvaluator, SearchQualityConfig};
/// use forge::dev_trace::SearchQualityStats;
///
/// let mut evaluator = SearchQualityEvaluator::new(SearchQualityConfig::default());
///
/// let mut stats = SearchQualityStats::new();
/// // 搜索有效
/// stats.record_with_search(true);
/// stats.record_with_search(true);
/// stats.record_with_search(true);
/// stats.record_without_search(false);
/// stats.record_without_search(false);
/// stats.record_without_search(false);
///
/// let decision = evaluator.evaluate_and_apply(&stats);
/// assert!(evaluator.is_enabled()); // 搜索有效, 保持启用
/// ```
#[derive(Debug, Clone)]
pub struct SearchQualityEvaluator {
    /// 评估配置
    config: SearchQualityConfig,
    /// 搜索是否启用
    enabled: bool,
    /// 累计评估次数
    evaluation_count: u32,
    /// 累计禁用次数
    disable_count: u32,
    /// 最近一次质量决策
    last_decision: Option<SearchQualityDecision>,
}

impl SearchQualityEvaluator {
    /// 创建新的搜索质量评估器
    ///
    /// # 参数
    ///
    /// - `config`: 评估配置
    pub fn new(config: SearchQualityConfig) -> Self {
        Self {
            config,
            enabled: true,
            evaluation_count: 0,
            disable_count: 0,
            last_decision: None,
        }
    }

    /// 使用默认配置创建评估器
    pub fn with_default_config() -> Self {
        Self::new(SearchQualityConfig::default())
    }

    /// 评估当前搜索质量统计, 生成决策
    ///
    /// 这是一个纯函数调用, 不修改评估器状态。
    /// 调用 [`apply_decision`][] 来应用决策。
    ///
    /// # 参数
    ///
    /// - `stats`: 搜索质量统计
    pub fn evaluate(&self, stats: &SearchQualityStats) -> SearchQualityDecision {
        // 如果搜索已禁用, 不再生成禁用决策
        if !self.enabled {
            return SearchQualityDecision::keep_searching(
                stats.search_vs_no_search_diff(),
                "搜索已禁用",
            );
        }
        compute_search_quality_decision(stats, &self.config)
    }

    /// 应用质量决策, 更新内部状态
    ///
    /// # 参数
    ///
    /// - `decision`: 质量决策
    pub fn apply_decision(&mut self, decision: &SearchQualityDecision) {
        match &decision.action {
            SearchQualityAction::KeepSearching | SearchQualityAction::InsufficientData => {}
            SearchQualityAction::DisableSearch => {
                self.enabled = false;
                self.disable_count += 1;
            }
        }
        self.last_decision = Some(decision.clone());
    }

    /// 一步评估并应用 — 等价于 `evaluate` + `apply_decision`
    ///
    /// 返回生成的质量决策。
    pub fn evaluate_and_apply(&mut self, stats: &SearchQualityStats) -> SearchQualityDecision {
        let decision = self.evaluate(stats);
        self.apply_decision(&decision);
        decision
    }

    /// 重新启用搜索 (如果之前被禁用)
    pub fn re_enable(&mut self) {
        self.enabled = true;
    }

    /// 获取评估配置引用
    pub fn config(&self) -> &SearchQualityConfig {
        &self.config
    }

    /// 搜索是否启用
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// 累计评估次数
    pub fn evaluation_count(&self) -> u32 {
        self.evaluation_count
    }

    /// 累计禁用次数
    pub fn disable_count(&self) -> u32 {
        self.disable_count
    }

    /// 最近一次质量决策
    pub fn last_decision(&self) -> Option<&SearchQualityDecision> {
        self.last_decision.as_ref()
    }

    /// 格式化为可读摘要
    pub fn to_summary(&self) -> String {
        format!(
            "搜索质量评估器: 启用={}, 评估 {} 次, 禁用 {} 次, 最近决策: {}",
            self.enabled,
            self.evaluation_count,
            self.disable_count,
            self.last_decision
                .as_ref()
                .map(|d| d.to_trace_summary())
                .unwrap_or_else(|| "无".to_string())
        )
    }
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dev_trace::{DevTraceEntry, TraceAction};

    // ===== SearchQualityConfig 测试 =====

    #[test]
    fn test_config_default() {
        let config = SearchQualityConfig::default();
        assert_eq!(config.min_samples, 5);
        assert!((config.disable_threshold - (-0.10)).abs() < 0.001);
        assert!((config.beneficial_threshold - 0.05).abs() < 0.001);
    }

    #[test]
    fn test_config_strict() {
        let config = SearchQualityConfig::strict();
        assert_eq!(config.min_samples, 3);
        assert!((config.disable_threshold - (-0.05)).abs() < 0.001);
        assert!((config.beneficial_threshold - 0.10).abs() < 0.001);
    }

    #[test]
    fn test_config_lenient() {
        let config = SearchQualityConfig::lenient();
        assert_eq!(config.min_samples, 8);
        assert!((config.disable_threshold - (-0.20)).abs() < 0.001);
        assert!((config.beneficial_threshold - 0.02).abs() < 0.001);
    }

    #[test]
    fn test_config_serde_roundtrip() {
        let config = SearchQualityConfig::strict();
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: SearchQualityConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.min_samples, config.min_samples);
        assert!((deserialized.disable_threshold - config.disable_threshold).abs() < 0.001);
    }

    // ===== SearchQualityDecision 测试 =====

    #[test]
    fn test_decision_keep_searching() {
        let d = SearchQualityDecision::keep_searching(0.1, "有效");
        assert!(d.is_keep());
        assert!(!d.is_disable());
        assert!(!d.is_insufficient_data());
        assert!((d.diff - 0.1).abs() < 0.001);
    }

    #[test]
    fn test_decision_disable_search() {
        let d = SearchQualityDecision::disable_search(-0.2, "有害");
        assert!(d.is_disable());
        assert!(!d.is_keep());
        assert!((d.diff - (-0.2)).abs() < 0.001);
    }

    #[test]
    fn test_decision_insufficient_data() {
        let d = SearchQualityDecision::insufficient_data(0.0, "样本不足");
        assert!(d.is_insufficient_data());
        assert!(!d.is_disable());
    }

    #[test]
    fn test_decision_to_trace_summary() {
        let d = SearchQualityDecision::disable_search(-0.15, "搜索有害");
        let summary = d.to_trace_summary();
        assert!(summary.contains("禁用搜索"));
        assert!(summary.contains("-15.0%"));
        assert!(summary.contains("搜索有害"));
    }

    #[test]
    fn test_decision_serde_roundtrip() {
        let d = SearchQualityDecision::keep_searching(0.33, "搜索有效");
        let json = serde_json::to_string(&d).unwrap();
        let deserialized: SearchQualityDecision = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.action, d.action);
        assert!((deserialized.diff - d.diff).abs() < 0.001);
    }

    // ===== 纯函数测试 =====

    #[test]
    fn test_has_sufficient_data_empty() {
        let stats = SearchQualityStats::new();
        assert!(!has_sufficient_search_data(&stats, 5));
    }

    #[test]
    fn test_has_sufficient_data_partial() {
        let mut stats = SearchQualityStats::new();
        stats.record_with_search(true);
        stats.record_without_search(false);
        assert!(!has_sufficient_search_data(&stats, 5));
    }

    #[test]
    fn test_has_sufficient_data_sufficient() {
        let mut stats = SearchQualityStats::new();
        for _ in 0..3 {
            stats.record_with_search(true);
            stats.record_without_search(false);
        }
        assert!(has_sufficient_search_data(&stats, 5));
    }

    #[test]
    fn test_has_sufficient_data_only_with() {
        let mut stats = SearchQualityStats::new();
        for _ in 0..10 {
            stats.record_with_search(true);
        }
        // 没有 without_search 数据
        assert!(!has_sufficient_search_data(&stats, 5));
    }

    #[test]
    fn test_should_disable_search_harmful() {
        let mut stats = SearchQualityStats::new();
        // 搜索后全部失败 (0/3)
        for _ in 0..3 {
            stats.record_with_search(false);
        }
        // 不搜索全部成功 (3/3)
        for _ in 0..3 {
            stats.record_without_search(true);
        }
        // diff = 0% - 100% = -100% < -10% → 禁用
        let config = SearchQualityConfig::default();
        assert!(should_disable_search(&stats, &config));
    }

    #[test]
    fn test_should_disable_search_beneficial() {
        let mut stats = SearchQualityStats::new();
        // 搜索后全部成功 (3/3)
        for _ in 0..3 {
            stats.record_with_search(true);
        }
        // 不搜索全部失败 (0/3)
        for _ in 0..3 {
            stats.record_without_search(false);
        }
        // diff = 100% - 0% = +100% > -10% → 不禁用
        let config = SearchQualityConfig::default();
        assert!(!should_disable_search(&stats, &config));
    }

    #[test]
    fn test_should_disable_search_insufficient_data() {
        let mut stats = SearchQualityStats::new();
        stats.record_with_search(false);
        stats.record_without_search(true);
        // 只有 2 个样本, 不足 min_samples=5
        let config = SearchQualityConfig::default();
        assert!(!should_disable_search(&stats, &config));
    }

    #[test]
    fn test_should_disable_search_neutral() {
        let mut stats = SearchQualityStats::new();
        // 搜索和不搜索修复率相同
        for _ in 0..3 {
            stats.record_with_search(true);
            stats.record_without_search(true);
        }
        // diff = 0% → 不禁用
        let config = SearchQualityConfig::default();
        assert!(!should_disable_search(&stats, &config));
    }

    // ===== compute_search_quality_decision 测试 =====

    #[test]
    fn test_compute_decision_insufficient_data() {
        let mut stats = SearchQualityStats::new();
        stats.record_with_search(true);
        stats.record_without_search(false);
        let config = SearchQualityConfig::default();
        let decision = compute_search_quality_decision(&stats, &config);
        assert!(decision.is_insufficient_data());
    }

    #[test]
    fn test_compute_decision_disable() {
        let mut stats = SearchQualityStats::new();
        // 搜索后修复率 0%, 不搜索 100% → diff = -100% < -10%
        for _ in 0..3 {
            stats.record_with_search(false);
            stats.record_without_search(true);
        }
        let config = SearchQualityConfig::default();
        let decision = compute_search_quality_decision(&stats, &config);
        assert!(decision.is_disable());
        assert!(decision.reason.contains("搜索修复率"));
    }

    #[test]
    fn test_compute_decision_keep_beneficial() {
        let mut stats = SearchQualityStats::new();
        // 搜索后修复率 100%, 不搜索 0% → diff = +100% >= 5%
        for _ in 0..3 {
            stats.record_with_search(true);
            stats.record_without_search(false);
        }
        let config = SearchQualityConfig::default();
        let decision = compute_search_quality_decision(&stats, &config);
        assert!(decision.is_keep());
        assert!(decision.reason.contains("高于"));
    }

    #[test]
    fn test_compute_decision_keep_neutral() {
        let mut stats = SearchQualityStats::new();
        // diff = 0% → 中性 → keep
        for _ in 0..3 {
            stats.record_with_search(true);
            stats.record_without_search(true);
        }
        let config = SearchQualityConfig::default();
        let decision = compute_search_quality_decision(&stats, &config);
        assert!(decision.is_keep());
        assert!(decision.reason.contains("正常范围"));
    }

    #[test]
    fn test_compute_decision_strict_config() {
        let mut stats = SearchQualityStats::new();
        // diff = -8% → 默认配置不禁用 (-8% > -10%), 但严格配置禁用 (-8% < -5%)
        // hit: 2/5 = 40%, miss: 3/5 = 60%, diff = -20% → 两种配置都禁用
        // 调整: hit: 3/5=60%, miss: 4/5=80%, diff = -20% → 仍然禁用
        // 调整: hit: 4/5=80%, miss: 4/5=80%, diff = 0% → 保持
        // 需要 diff 在 -10% 和 -5% 之间
        // hit: 3/5=60%, miss: 3/5=60% → diff = 0% → 不行
        // hit: 2/5=40%, miss: 3/5=60% → diff = -20% → 都禁用
        // hit: 3/5=60%, miss: 4/5=80% → diff = -20% → 都禁用
        // 用更大样本: hit: 5/10=50%, miss: 6/10=60% → diff = -10% → 默认不禁用, 严格禁用
        for _ in 0..5 {
            stats.record_with_search(true);
        }
        for _ in 0..5 {
            stats.record_with_search(false);
        }
        for _ in 0..6 {
            stats.record_without_search(true);
        }
        for _ in 0..4 {
            stats.record_without_search(false);
        }
        // diff = 50% - 60% = -10% → 默认: -10% 不 < -10% → keep; 严格: -10% < -5% → disable
        let default_decision =
            compute_search_quality_decision(&stats, &SearchQualityConfig::default());
        let strict_decision =
            compute_search_quality_decision(&stats, &SearchQualityConfig::strict());
        assert!(default_decision.is_keep()); // -10% 不低于 -10% (边界)
        assert!(strict_decision.is_disable()); // -10% 低于 -5%
    }

    #[test]
    fn test_compute_decision_lenient_config() {
        let mut stats = SearchQualityStats::new();
        // diff = -15% → 默认禁用 (-15% < -10%), 宽松不禁用 (-15% > -20%)
        // hit: 3/10=30%, miss: 4.5/10 → 不行, 用整数
        // hit: 3/10=30%, miss: 5/10=50% → diff = -20% → 都禁用
        // hit: 4/10=40%, miss: 5/10=50% → diff = -10% → 默认keep, 严格disable
        // hit: 3/10=30%, miss: 4/10=40% → diff = -10% → 同上
        // 用 8 个样本 (lenient 要求 8): hit: 3/8=37.5%, miss: 5/8=62.5% → diff = -25% → 都禁用
        // hit: 4/8=50%, miss: 5/8=62.5% → diff = -12.5% → 默认disable, lenient keep
        for _ in 0..4 {
            stats.record_with_search(true);
        }
        for _ in 0..4 {
            stats.record_with_search(false);
        }
        for _ in 0..5 {
            stats.record_without_search(true);
        }
        for _ in 0..3 {
            stats.record_without_search(false);
        }
        // diff = 50% - 62.5% = -12.5% → 默认 disable (< -10%), lenient keep (> -20%)
        let default_decision =
            compute_search_quality_decision(&stats, &SearchQualityConfig::default());
        let lenient_decision =
            compute_search_quality_decision(&stats, &SearchQualityConfig::lenient());
        assert!(default_decision.is_disable());
        assert!(lenient_decision.is_keep());
    }

    // ===== SearchQualityEvaluator 测试 =====

    #[test]
    fn test_evaluator_new() {
        let evaluator = SearchQualityEvaluator::new(SearchQualityConfig::default());
        assert!(evaluator.is_enabled());
        assert_eq!(evaluator.evaluation_count(), 0);
        assert_eq!(evaluator.disable_count(), 0);
        assert!(evaluator.last_decision().is_none());
    }

    #[test]
    fn test_evaluator_with_default_config() {
        let evaluator = SearchQualityEvaluator::with_default_config();
        assert!(evaluator.is_enabled());
    }

    #[test]
    fn test_evaluator_evaluate_no_mutation() {
        let evaluator = SearchQualityEvaluator::new(SearchQualityConfig::default());
        let stats = SearchQualityStats::new();
        let _decision = evaluator.evaluate(&stats);
        // evaluate 不应该修改状态
        assert_eq!(evaluator.evaluation_count(), 0);
        assert!(evaluator.last_decision().is_none());
    }

    #[test]
    fn test_evaluator_apply_keep() {
        let mut evaluator = SearchQualityEvaluator::new(SearchQualityConfig::default());
        let decision = SearchQualityDecision::keep_searching(0.1, "有效");
        evaluator.apply_decision(&decision);
        assert!(evaluator.is_enabled());
        assert_eq!(evaluator.disable_count(), 0);
        assert!(evaluator.last_decision().is_some());
    }

    #[test]
    fn test_evaluator_apply_disable() {
        let mut evaluator = SearchQualityEvaluator::new(SearchQualityConfig::default());
        let decision = SearchQualityDecision::disable_search(-0.2, "有害");
        evaluator.apply_decision(&decision);
        assert!(!evaluator.is_enabled());
        assert_eq!(evaluator.disable_count(), 1);
    }

    #[test]
    fn test_evaluator_apply_insufficient_data() {
        let mut evaluator = SearchQualityEvaluator::new(SearchQualityConfig::default());
        let decision = SearchQualityDecision::insufficient_data(0.0, "不足");
        evaluator.apply_decision(&decision);
        assert!(evaluator.is_enabled());
        assert_eq!(evaluator.disable_count(), 0);
    }

    #[test]
    fn test_evaluator_evaluate_and_apply_keep() {
        let mut evaluator = SearchQualityEvaluator::new(SearchQualityConfig::default());
        let mut stats = SearchQualityStats::new();
        // 搜索有效
        for _ in 0..3 {
            stats.record_with_search(true);
            stats.record_without_search(false);
        }
        let decision = evaluator.evaluate_and_apply(&stats);
        assert!(decision.is_keep());
        assert!(evaluator.is_enabled());
    }

    #[test]
    fn test_evaluator_evaluate_and_apply_disable() {
        let mut evaluator = SearchQualityEvaluator::new(SearchQualityConfig::default());
        let mut stats = SearchQualityStats::new();
        // 搜索有害
        for _ in 0..3 {
            stats.record_with_search(false);
            stats.record_without_search(true);
        }
        let decision = evaluator.evaluate_and_apply(&stats);
        assert!(decision.is_disable());
        assert!(!evaluator.is_enabled());
    }

    #[test]
    fn test_evaluator_evaluate_and_apply_insufficient() {
        let mut evaluator = SearchQualityEvaluator::new(SearchQualityConfig::default());
        let mut stats = SearchQualityStats::new();
        stats.record_with_search(true);
        stats.record_without_search(false);
        let decision = evaluator.evaluate_and_apply(&stats);
        assert!(decision.is_insufficient_data());
        assert!(evaluator.is_enabled());
    }

    #[test]
    fn test_evaluator_evaluate_when_disabled() {
        let mut evaluator = SearchQualityEvaluator::new(SearchQualityConfig::default());
        // 先禁用
        evaluator.apply_decision(&SearchQualityDecision::disable_search(-0.2, "有害"));
        assert!(!evaluator.is_enabled());

        // 再评估时应该返回 keep (不再生成 disable)
        let mut stats = SearchQualityStats::new();
        for _ in 0..3 {
            stats.record_with_search(false);
            stats.record_without_search(true);
        }
        let decision = evaluator.evaluate(&stats);
        assert!(decision.is_keep());
        assert!(decision.reason.contains("已禁用"));
    }

    #[test]
    fn test_evaluator_re_enable() {
        let mut evaluator = SearchQualityEvaluator::new(SearchQualityConfig::default());
        evaluator.apply_decision(&SearchQualityDecision::disable_search(-0.2, "有害"));
        assert!(!evaluator.is_enabled());
        evaluator.re_enable();
        assert!(evaluator.is_enabled());
    }

    #[test]
    fn test_evaluator_multiple_evaluations() {
        let mut evaluator = SearchQualityEvaluator::new(SearchQualityConfig::default());

        // 第一次: 数据不足
        let mut stats1 = SearchQualityStats::new();
        stats1.record_with_search(true);
        stats1.record_without_search(false);
        let d1 = evaluator.evaluate_and_apply(&stats1);
        assert!(d1.is_insufficient_data());

        // 第二次: 搜索有害 → 禁用
        let mut stats2 = SearchQualityStats::new();
        for _ in 0..3 {
            stats2.record_with_search(false);
            stats2.record_without_search(true);
        }
        let d2 = evaluator.evaluate_and_apply(&stats2);
        assert!(d2.is_disable());
        assert!(!evaluator.is_enabled());

        // 第三次: 已禁用 → keep
        let d3 = evaluator.evaluate_and_apply(&stats2);
        assert!(d3.is_keep());
    }

    #[test]
    fn test_evaluator_to_summary() {
        let mut evaluator = SearchQualityEvaluator::new(SearchQualityConfig::default());
        let stats = SearchQualityStats::new();
        evaluator.evaluate_and_apply(&stats);
        let summary = evaluator.to_summary();
        assert!(summary.contains("搜索质量评估器"));
        assert!(summary.contains("启用=true"));
    }

    // ===== 集成场景测试 =====

    #[test]
    fn test_full_workflow_beneficial_search() {
        let mut evaluator = SearchQualityEvaluator::new(SearchQualityConfig::default());

        // 模拟: 搜索帮助修复 (with: 4/5=80%, without: 2/5=40%, diff=+40%)
        let mut stats = SearchQualityStats::new();
        for _ in 0..4 {
            stats.record_with_search(true);
        }
        stats.record_with_search(false);
        for _ in 0..2 {
            stats.record_without_search(true);
        }
        for _ in 0..3 {
            stats.record_without_search(false);
        }

        let decision = evaluator.evaluate_and_apply(&stats);
        assert!(decision.is_keep());
        assert!(evaluator.is_enabled());
        assert!((decision.diff - 0.4).abs() < 0.001);
    }

    #[test]
    fn test_full_workflow_harmful_search() {
        let mut evaluator = SearchQualityEvaluator::new(SearchQualityConfig::default());

        // 模拟: 搜索有害 (with: 1/5=20%, without: 4/5=80%, diff=-60%)
        let mut stats = SearchQualityStats::new();
        stats.record_with_search(true);
        for _ in 0..4 {
            stats.record_with_search(false);
        }
        for _ in 0..4 {
            stats.record_without_search(true);
        }
        stats.record_without_search(false);

        let decision = evaluator.evaluate_and_apply(&stats);
        assert!(decision.is_disable());
        assert!(!evaluator.is_enabled());
        assert!((decision.diff - (-0.6)).abs() < 0.001);
    }

    #[test]
    fn test_full_workflow_neutral_search() {
        let mut evaluator = SearchQualityEvaluator::new(SearchQualityConfig::default());

        // 模拟: 搜索中性 (with: 3/5=60%, without: 3/5=60%, diff=0%)
        let mut stats = SearchQualityStats::new();
        for _ in 0..3 {
            stats.record_with_search(true);
        }
        for _ in 0..2 {
            stats.record_with_search(false);
        }
        for _ in 0..3 {
            stats.record_without_search(true);
        }
        for _ in 0..2 {
            stats.record_without_search(false);
        }

        let decision = evaluator.evaluate_and_apply(&stats);
        assert!(decision.is_keep());
        assert!(evaluator.is_enabled());
        assert!((decision.diff - 0.0).abs() < 0.001);
    }

    // ===== build_search_quality_stats 从 DevTrace 条目构建测试 =====

    #[test]
    fn test_build_stats_empty_entries() {
        let entries: Vec<DevTraceEntry> = vec![];
        let stats = crate::dev_trace::build_search_quality_stats(&entries);
        assert!(stats.is_empty());
    }

    #[test]
    fn test_build_stats_only_compile_checks() {
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(0),
                Some("task1"),
                "check",
                "passed",
                50,
                true,
                None,
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
        let stats = crate::dev_trace::build_search_quality_stats(&entries);
        assert_eq!(stats.checks_without_search, 2);
        assert_eq!(stats.checks_with_search, 0);
        assert_eq!(stats.total_searches, 0);
    }

    #[test]
    fn test_build_stats_with_search() {
        let entries = vec![
            // Task 1: 搜索后编译通过
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(0),
                Some("task1"),
                "query",
                "result",
                500,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(0),
                Some("task1"),
                "check",
                "passed",
                50,
                true,
                None,
            ),
        ];
        let stats = crate::dev_trace::build_search_quality_stats(&entries);
        assert_eq!(stats.checks_with_search, 1);
        assert_eq!(stats.successes_with_search, 1);
        assert_eq!(stats.checks_without_search, 0);
        assert_eq!(stats.total_searches, 1);
        assert_eq!(stats.successful_searches, 1);
    }

    #[test]
    fn test_build_stats_mixed() {
        let entries = vec![
            // Task 1: 首次编译 (无搜索) → 失败
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(0),
                Some("task1"),
                "check",
                "failed",
                50,
                false,
                None,
            ),
            // Task 1: 搜索后编译 → 通过
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(0),
                Some("task1"),
                "query",
                "result",
                500,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(0),
                Some("task1"),
                "check",
                "passed",
                50,
                true,
                None,
            ),
            // Task 2: 首次编译 (无搜索) → 通过
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(1),
                Some("task2"),
                "check",
                "passed",
                50,
                true,
                None,
            ),
        ];
        let stats = crate::dev_trace::build_search_quality_stats(&entries);
        assert_eq!(stats.checks_without_search, 2); // Task1 第一次 + Task2
        assert_eq!(stats.checks_with_search, 1); // Task1 第二次
        assert_eq!(stats.successes_with_search, 1);
        assert_eq!(stats.total_searches, 1);
    }

    #[test]
    fn test_build_stats_failed_search() {
        let entries = vec![
            // 搜索失败
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(0),
                Some("task1"),
                "query",
                "",
                3000,
                false,
                Some("搜索失败: timeout"),
            ),
            // 编译检查 (有前置 WebSearch, 即使搜索失败)
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(0),
                Some("task1"),
                "check",
                "failed",
                50,
                false,
                None,
            ),
        ];
        let stats = crate::dev_trace::build_search_quality_stats(&entries);
        assert_eq!(stats.total_searches, 1);
        assert_eq!(stats.failed_searches, 1);
        assert_eq!(stats.successful_searches, 0);
        assert_eq!(stats.checks_with_search, 1);
        assert_eq!(stats.successes_with_search, 0);
    }

    #[test]
    fn test_build_stats_multiple_rounds_same_task() {
        let entries = vec![
            // Task 1, 第1轮: 编译失败 (无搜索)
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(0),
                Some("task1"),
                "check1",
                "failed",
                50,
                false,
                None,
            ),
            // Task 1, 第2轮: 搜索 → 编译通过 (有搜索)
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(0),
                Some("task1"),
                "query",
                "result",
                500,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(0),
                Some("task1"),
                "check2",
                "passed",
                50,
                true,
                None,
            ),
            // Task 1, 第3轮: 搜索 → 编译失败 (有搜索)
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(0),
                Some("task1"),
                "query2",
                "result2",
                500,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(0),
                Some("task1"),
                "check3",
                "failed",
                50,
                false,
                None,
            ),
        ];
        let stats = crate::dev_trace::build_search_quality_stats(&entries);
        assert_eq!(stats.checks_without_search, 1); // 第1轮
        assert_eq!(stats.checks_with_search, 2); // 第2轮 + 第3轮
        assert_eq!(stats.successes_with_search, 1);
        assert_eq!(stats.total_searches, 2);
    }

    // ===== DevTraceSummary 集成测试 =====

    #[test]
    fn test_summary_search_quality_none_without_entries() {
        let entries = vec![crate::dev_trace::DevTraceEntry::new(
            TraceAction::TaskExecution,
            None,
            None,
            None,
            "input",
            "output",
            100,
            true,
            None,
        )];
        let summary = crate::dev_trace::DevTraceSummary::from_entries(&entries);
        assert!(summary.search_quality_summary.is_none());
    }

    #[test]
    fn test_summary_search_quality_from_entries() {
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
        let summary = crate::dev_trace::DevTraceSummary::from_entries(&entries);
        let sq = summary.search_quality_summary.expect("should be Some");
        assert_eq!(sq.checks_with_search, 1);
        assert_eq!(sq.checks_without_search, 1);
        assert_eq!(sq.total_searches, 1);
    }

    #[test]
    fn test_summary_search_quality_serde_roundtrip() {
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
        let summary = crate::dev_trace::DevTraceSummary::from_entries(&entries);

        let json = serde_json::to_string(&summary).unwrap();
        let deserialized: crate::dev_trace::DevTraceSummary = serde_json::from_str(&json).unwrap();

        let sq = deserialized.search_quality_summary.expect("should be Some");
        assert_eq!(sq.checks_with_search, 1);
        assert_eq!(sq.total_searches, 1);
    }

    #[test]
    fn test_summary_search_quality_serde_skip_none() {
        let summary = crate::dev_trace::DevTraceSummary::empty();
        let json = serde_json::to_string(&summary).unwrap();
        assert!(!json.contains("search_quality_summary"));
    }

    #[test]
    fn test_to_report_includes_search_quality() {
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
        let summary = crate::dev_trace::DevTraceSummary::from_entries(&entries);
        let report = summary.to_report();

        assert!(report.contains("搜索质量评估"));
        assert!(report.contains("有搜索修复: 1 次检查 (通过 1)"));
        assert!(report.contains("无搜索修复: 1 次检查 (通过 0)"));
    }

    #[test]
    fn test_to_report_no_search_quality_when_none() {
        let entries = vec![DevTraceEntry::new(
            TraceAction::TaskExecution,
            None,
            None,
            None,
            "input",
            "output",
            100,
            true,
            None,
        )];
        let summary = crate::dev_trace::DevTraceSummary::from_entries(&entries);
        let report = summary.to_report();
        assert!(!report.contains("搜索质量评估"));
    }

    #[test]
    fn test_to_report_search_quality_diff() {
        let entries = vec![
            // 有搜索: 2/3 通过
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(0),
                Some("task1"),
                "q1",
                "r1",
                500,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(0),
                Some("task1"),
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
                "q2",
                "r2",
                500,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(1),
                Some("task2"),
                "check",
                "passed",
                50,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(2),
                Some("task3"),
                "q3",
                "r3",
                500,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(2),
                Some("task3"),
                "check",
                "failed",
                50,
                false,
                None,
            ),
            // 无搜索: 1/3 通过
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(3),
                Some("task4"),
                "check",
                "passed",
                50,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(4),
                Some("task5"),
                "check",
                "failed",
                50,
                false,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(5),
                Some("task6"),
                "check",
                "failed",
                50,
                false,
                None,
            ),
        ];
        let summary = crate::dev_trace::DevTraceSummary::from_entries(&entries);
        let report = summary.to_report();

        assert!(report.contains("搜索质量评估"));
        assert!(report.contains("有搜索修复率: 66.7%"));
        assert!(report.contains("无搜索修复率: 33.3%"));
        assert!(report.contains("差值: +33.3%"));
        assert!(report.contains("搜索有效"));
    }

    #[test]
    fn test_summary_all_sections_with_search_quality() {
        // 同时有增量发送、搜索缓存、关联分析、缓存调优和搜索质量
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
        let summary = crate::dev_trace::DevTraceSummary::from_entries(&entries);
        assert!(summary.incremental_summary.is_some());
        assert!(summary.cache_summary.is_some());
        assert!(summary.cache_fix_correlation.is_some());
        assert!(summary.cache_tuning_summary.is_some());
        assert!(summary.search_quality_summary.is_some());

        let report = summary.to_report();
        assert!(report.contains("增量发送统计"));
        assert!(report.contains("搜索缓存统计"));
        assert!(report.contains("缓存与修复关联分析"));
        assert!(report.contains("缓存调优效果"));
        assert!(report.contains("搜索质量评估"));
    }
}
