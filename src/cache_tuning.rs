//! Cache Tuning — 搜索缓存策略自动调优 (Session 81)
//!
//! 基于 `CacheFixCorrelation` 分析结果, 自动调整缓存 TTL 或禁用缓存,
//! 当缓存效果不足时减少对陈旧搜索结果的依赖。
//!
//! ## 设计理念
//!
//! Session 80 引入了 `CacheFixCorrelation`, 可以判断缓存命中的搜索结果
//! 是否和新鲜搜索一样有效 (通过比较后续编译检查的通过率)。
//!
//! 本模块利用该分析结果, 实现缓存策略的自动调优:
//!
//! 1. **缓存有害** (`diff < disable_threshold`): 缓存命中修复率远低于未命中 → 禁用缓存
//! 2. **缓存略差** (`diff < reduce_ttl_threshold`): 缓存命中修复率略低 → 缩短 TTL, 加快刷新
//! 3. **缓存有效** (`diff >= increase_ttl_threshold`): 缓存命中修复率优于未命中 → 延长 TTL, 节省更多时间
//! 4. **数据不足** 或 **持平**: 保持当前配置
//!
//! ## 纯函数架构 (SRP)
//!
//! - [`has_sufficient_data`][]: 判断关联分析是否有足够数据做决策
//! - [`should_disable_cache`][]: 判断是否应该禁用缓存
//! - [`should_adjust_ttl`][]: 判断是否应该调整 TTL (缩短或延长)
//! - [`compute_new_ttl`][]: 计算新的 TTL 值
//! - [`make_tuning_decision`][]: 综合分析, 生成调优决策
//!
//! ## 示例
//!
//! ```
//! use forge::cache_tuning::{CacheTuningConfig, make_tuning_decision};
//! use forge::dev_trace::CacheFixCorrelation;
//! use forge::search_cache::CacheStats;
//!
//! let mut corr = CacheFixCorrelation::new();
//! // 缓存命中后修复率低 (3次中1次通过)
//! corr.record_hit_check(true);
//! corr.record_hit_check(false);
//! corr.record_hit_check(false);
//! // 新鲜搜索修复率高 (3次中3次通过)
//! corr.record_miss_check(true);
//! corr.record_miss_check(true);
//! corr.record_miss_check(true);
//!
//! let config = CacheTuningConfig::default();
//! let stats = CacheStats::new();
//! let decision = make_tuning_decision(&corr, &stats, 1800, &config);
//!
//! // 缓存命中修复率 33% vs 未命中 100% → diff = -67% → 禁用缓存
//! assert!(decision.is_disable());
//! ```

use crate::dev_trace::CacheFixCorrelation;
use crate::search_cache::CacheStats;
use serde::{Deserialize, Serialize};

// ============================================================================
//  常量
// ============================================================================

/// 默认最小样本数 — 至少需要这么多数据点才做决策
pub const DEFAULT_MIN_SAMPLES: usize = 3;

/// 默认禁用阈值 — diff 低于此值 (如 -0.15 = -15%) 时禁用缓存
pub const DEFAULT_DISABLE_THRESHOLD: f64 = -0.15;

/// 默认缩短 TTL 阈值 — diff 低于此值 (如 -0.05 = -5%) 时缩短 TTL
pub const DEFAULT_REDUCE_TTL_THRESHOLD: f64 = -0.05;

/// 默认延长 TTL 阈值 — diff 高于此值 (如 0.05 = 5%) 时延长 TTL
pub const DEFAULT_INCREASE_TTL_THRESHOLD: f64 = 0.05;

/// 默认 TTL 缩短因子 (乘以当前 TTL)
pub const DEFAULT_TTL_REDUCE_FACTOR: f64 = 0.5;

/// 默认 TTL 延长因子 (乘以当前 TTL)
pub const DEFAULT_TTL_INCREASE_FACTOR: f64 = 1.5;

/// 默认最小 TTL (秒, 1 分钟)
pub const DEFAULT_MIN_TTL_SECS: u64 = 60;

/// 默认最大 TTL (秒, 2 小时)
pub const DEFAULT_MAX_TTL_SECS: u64 = 7200;

// ============================================================================
//  CacheTuningConfig — 调优配置
// ============================================================================

/// 缓存调优配置 — 控制自动调优的阈值和参数
///
/// 所有阈值基于 `hit_vs_miss_diff()` (缓存命中修复率 - 未命中修复率):
/// - 正值: 缓存命中比未命中有更好的修复效果
/// - 负值: 缓存命中效果不如未命中
///
/// # 默认值
///
/// | 参数 | 默认值 | 含义 |
/// |------|--------|------|
/// | `min_samples` | 3 | 命中/未命中各至少3次才做决策 |
/// | `disable_threshold` | -0.15 | diff < -15% → 禁用缓存 |
/// | `reduce_ttl_threshold` | -0.05 | diff < -5% → 缩短 TTL |
/// | `increase_ttl_threshold` | 0.05 | diff > 5% → 延长 TTL |
/// | `ttl_reduce_factor` | 0.5 | TTL × 0.5 |
/// | `ttl_increase_factor` | 1.5 | TTL × 1.5 |
/// | `min_ttl_secs` | 60 | TTL 不低于 1 分钟 |
/// | `max_ttl_secs` | 7200 | TTL 不超过 2 小时 |
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheTuningConfig {
    /// 最小样本数 — 命中/未命中各至少这么多次才做决策
    pub min_samples: usize,
    /// 禁用阈值 — diff 低于此值时禁用缓存
    pub disable_threshold: f64,
    /// 缩短 TTL 阈值 — diff 低于此值 (但高于 disable_threshold) 时缩短 TTL
    pub reduce_ttl_threshold: f64,
    /// 延长 TTL 阈值 — diff 高于此值时延长 TTL
    pub increase_ttl_threshold: f64,
    /// TTL 缩短因子 (0.0 ~ 1.0)
    pub ttl_reduce_factor: f64,
    /// TTL 延长因子 (> 1.0)
    pub ttl_increase_factor: f64,
    /// 最小 TTL (秒)
    pub min_ttl_secs: u64,
    /// 最大 TTL (秒)
    pub max_ttl_secs: u64,
}

impl Default for CacheTuningConfig {
    fn default() -> Self {
        Self {
            min_samples: DEFAULT_MIN_SAMPLES,
            disable_threshold: DEFAULT_DISABLE_THRESHOLD,
            reduce_ttl_threshold: DEFAULT_REDUCE_TTL_THRESHOLD,
            increase_ttl_threshold: DEFAULT_INCREASE_TTL_THRESHOLD,
            ttl_reduce_factor: DEFAULT_TTL_REDUCE_FACTOR,
            ttl_increase_factor: DEFAULT_TTL_INCREASE_FACTOR,
            min_ttl_secs: DEFAULT_MIN_TTL_SECS,
            max_ttl_secs: DEFAULT_MAX_TTL_SECS,
        }
    }
}

impl CacheTuningConfig {
    /// 创建默认配置
    pub fn new() -> Self {
        Self::default()
    }

    /// 创建保守配置 — 更不容易禁用缓存, 调整幅度更小
    ///
    /// 适合缓存命中修复率略低但仍有价值的场景。
    pub fn conservative() -> Self {
        Self {
            min_samples: 5,
            disable_threshold: -0.25,
            reduce_ttl_threshold: -0.10,
            increase_ttl_threshold: 0.10,
            ttl_reduce_factor: 0.75,
            ttl_increase_factor: 1.25,
            min_ttl_secs: 120,
            max_ttl_secs: 3600,
        }
    }

    /// 创建激进配置 — 更容易禁用缓存, 调整幅度更大
    ///
    /// 适合对修复成功率要求严格的场景。
    pub fn aggressive() -> Self {
        Self {
            min_samples: 2,
            disable_threshold: -0.05,
            reduce_ttl_threshold: -0.02,
            increase_ttl_threshold: 0.02,
            ttl_reduce_factor: 0.25,
            ttl_increase_factor: 2.0,
            min_ttl_secs: 30,
            max_ttl_secs: 14400,
        }
    }

    /// 设置最小样本数
    pub fn with_min_samples(mut self, n: usize) -> Self {
        self.min_samples = n;
        self
    }

    /// 设置禁用阈值
    pub fn with_disable_threshold(mut self, threshold: f64) -> Self {
        self.disable_threshold = threshold;
        self
    }
}

// ============================================================================
//  TuningAction — 调优动作枚举
// ============================================================================

/// 缓存调优动作 — 表示对缓存配置的具体调整
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TuningAction {
    /// 保持当前配置不变
    KeepCurrent,

    /// 调整 TTL (缩短或延长)
    AdjustTtl {
        /// 新的 TTL 值 (秒)
        new_ttl: u64,
    },

    /// 禁用缓存
    DisableCache,
}

// ============================================================================
//  CacheTuningDecision — 调优决策
// ============================================================================

/// 缓存调优决策 — 包含动作、原因和上下文信息
///
/// 由 [`make_tuning_decision`][] 生成, 传递给 [`CacheTuner::apply_decision`][] 执行。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheTuningDecision {
    /// 调优动作
    pub action: TuningAction,
    /// 决策原因 (人类可读)
    pub reason: String,
    /// 调整前的 TTL (秒)
    pub old_ttl: u64,
    /// 缓存命中与未命中的修复成功率差值 (-1.0 ~ 1.0)
    pub correlation_diff: f64,
}

impl CacheTuningDecision {
    /// 创建 "保持当前" 决策
    pub fn keep_current(old_ttl: u64, diff: f64, reason: impl Into<String>) -> Self {
        Self {
            action: TuningAction::KeepCurrent,
            reason: reason.into(),
            old_ttl,
            correlation_diff: diff,
        }
    }

    /// 创建 "调整 TTL" 决策
    pub fn adjust_ttl(old_ttl: u64, new_ttl: u64, diff: f64, reason: impl Into<String>) -> Self {
        Self {
            action: TuningAction::AdjustTtl { new_ttl },
            reason: reason.into(),
            old_ttl,
            correlation_diff: diff,
        }
    }

    /// 创建 "禁用缓存" 决策
    pub fn disable_cache(old_ttl: u64, diff: f64, reason: impl Into<String>) -> Self {
        Self {
            action: TuningAction::DisableCache,
            reason: reason.into(),
            old_ttl,
            correlation_diff: diff,
        }
    }

    /// 是否为 "保持当前" 决策
    pub fn is_keep(&self) -> bool {
        matches!(self.action, TuningAction::KeepCurrent)
    }

    /// 是否为 "调整 TTL" 决策
    pub fn is_adjust(&self) -> bool {
        matches!(self.action, TuningAction::AdjustTtl { .. })
    }

    /// 是否为 "禁用缓存" 决策
    pub fn is_disable(&self) -> bool {
        matches!(self.action, TuningAction::DisableCache)
    }

    /// 如果是调整 TTL 决策, 返回新 TTL
    pub fn new_ttl(&self) -> Option<u64> {
        match self.action {
            TuningAction::AdjustTtl { new_ttl } => Some(new_ttl),
            _ => None,
        }
    }

    /// 格式化为可读字符串
    pub fn to_summary(&self) -> String {
        let action_str = match &self.action {
            TuningAction::KeepCurrent => "保持当前配置".to_string(),
            TuningAction::AdjustTtl { new_ttl } => {
                format!("调整 TTL: {}s → {}s", self.old_ttl, new_ttl)
            }
            TuningAction::DisableCache => "禁用缓存".to_string(),
        };
        format!(
            "缓存调优: {} (差值 {:+.1}%, 原因: {})",
            action_str,
            self.correlation_diff * 100.0,
            self.reason
        )
    }
}

// ============================================================================
//  纯函数 — 调优决策
// ============================================================================

/// 判断关联分析是否有足够数据做决策
///
/// 要求缓存命中和未命中各有至少 `min_samples` 次后续编译检查。
///
/// # 参数
///
/// - `corr`: 缓存修复关联分析
/// - `min_samples`: 最小样本数
///
/// # 示例
///
/// ```
/// # use forge::cache_tuning::has_sufficient_data;
/// # use forge::dev_trace::CacheFixCorrelation;
/// let mut corr = CacheFixCorrelation::new();
/// assert!(!has_sufficient_data(&corr, 3)); // 无数据
///
/// corr.record_hit_check(true);
/// corr.record_hit_check(true);
/// corr.record_hit_check(true);
/// assert!(!has_sufficient_data(&corr, 3)); // 缺少未命中数据
///
/// corr.record_miss_check(true);
/// corr.record_miss_check(true);
/// corr.record_miss_check(true);
/// assert!(has_sufficient_data(&corr, 3)); // 数据充足
/// ```
pub fn has_sufficient_data(corr: &CacheFixCorrelation, min_samples: usize) -> bool {
    corr.checks_after_hit >= min_samples && corr.checks_after_miss >= min_samples
}

/// 判断是否应该禁用缓存
///
/// 当缓存命中修复率远低于未命中 (diff < disable_threshold) 时, 缓存可能
/// 在提供过时的搜索结果, 导致修复效果变差, 应该禁用。
///
/// # 参数
///
/// - `corr`: 缓存修复关联分析
/// - `config`: 调优配置
///
/// # 示例
///
/// ```
/// # use forge::cache_tuning::{should_disable_cache, CacheTuningConfig};
/// # use forge::dev_trace::CacheFixCorrelation;
/// let mut corr = CacheFixCorrelation::new();
/// // 命中后全部失败
/// corr.record_hit_check(false);
/// corr.record_hit_check(false);
/// corr.record_hit_check(false);
/// // 未命中后全部成功
/// corr.record_miss_check(true);
/// corr.record_miss_check(true);
/// corr.record_miss_check(true);
///
/// let config = CacheTuningConfig::default();
/// assert!(should_disable_cache(&corr, &config)); // diff = -100% < -15%
/// ```
pub fn should_disable_cache(corr: &CacheFixCorrelation, config: &CacheTuningConfig) -> bool {
    if !has_sufficient_data(corr, config.min_samples) {
        return false;
    }
    corr.hit_vs_miss_diff() < config.disable_threshold
}

/// 判断是否应该调整 TTL (缩短或延长)
///
/// 当 diff 在 `disable_threshold` 和 `increase_ttl_threshold` 之间时,
/// 不需要调整。当 diff 低于 `reduce_ttl_threshold` 但不低于 `disable_threshold` 时,
/// 应缩短 TTL。当 diff 高于 `increase_ttl_threshold` 时, 应延长 TTL。
///
/// # 参数
///
/// - `corr`: 缓存修复关联分析
/// - `config`: 调优配置
///
/// # 示例
///
/// ```
/// # use forge::cache_tuning::{should_adjust_ttl, CacheTuningConfig};
/// # use forge::dev_trace::CacheFixCorrelation;
/// let mut corr = CacheFixCorrelation::new();
/// // 缓存有效: 命中后全部成功, 未命中后部分成功
/// corr.record_hit_check(true);
/// corr.record_hit_check(true);
/// corr.record_hit_check(true);
/// corr.record_miss_check(true);
/// corr.record_miss_check(true);
/// corr.record_miss_check(false);
///
/// let config = CacheTuningConfig::default();
/// assert!(should_adjust_ttl(&corr, &config)); // diff = 100%-67% = 33% > 5%
/// ```
pub fn should_adjust_ttl(corr: &CacheFixCorrelation, config: &CacheTuningConfig) -> bool {
    if !has_sufficient_data(corr, config.min_samples) {
        return false;
    }
    let diff = corr.hit_vs_miss_diff();
    // 排除应禁用的情况 (由 should_disable_cache 处理)
    if diff < config.disable_threshold {
        return false;
    }
    // 需要缩短或延长
    diff < config.reduce_ttl_threshold || diff > config.increase_ttl_threshold
}

/// 计算新的 TTL 值
///
/// 根据 diff 的方向调整 TTL:
/// - diff < 0 (缓存略差): 缩短 TTL (× ttl_reduce_factor)
/// - diff > 0 (缓存有效): 延长 TTL (× ttl_increase_factor)
/// - diff ≈ 0: 返回 None (无需调整)
///
/// 结果会被 clamp 到 `[min_ttl_secs, max_ttl_secs]` 范围。
///
/// # 参数
///
/// - `current_ttl`: 当前 TTL (秒)
/// - `corr`: 缓存修复关联分析
/// - `config`: 调优配置
///
/// # 返回
///
/// - `Some(new_ttl)`: 新的 TTL 值
/// - `None`: 无需调整或数据不足
///
/// # 示例
///
/// ```
/// # use forge::cache_tuning::{compute_new_ttl, CacheTuningConfig};
/// # use forge::dev_trace::CacheFixCorrelation;
/// let mut corr = CacheFixCorrelation::new();
/// corr.record_hit_check(true);
/// corr.record_hit_check(true);
/// corr.record_hit_check(true);
/// corr.record_miss_check(true);
/// corr.record_miss_check(true);
/// corr.record_miss_check(false);
///
/// let config = CacheTuningConfig::default();
/// // diff = 33% > 5% → 延长 TTL: 1800 × 1.5 = 2700
/// let new_ttl = compute_new_ttl(1800, &corr, &config).unwrap();
/// assert_eq!(new_ttl, 2700);
/// ```
pub fn compute_new_ttl(
    current_ttl: u64,
    corr: &CacheFixCorrelation,
    config: &CacheTuningConfig,
) -> Option<u64> {
    if !has_sufficient_data(corr, config.min_samples) {
        return None;
    }

    let diff = corr.hit_vs_miss_diff();

    // 排除应禁用的情况
    if diff < config.disable_threshold {
        return None;
    }

    let new_ttl = if diff < config.reduce_ttl_threshold {
        // 缓存略差 → 缩短 TTL
        (current_ttl as f64 * config.ttl_reduce_factor) as u64
    } else if diff > config.increase_ttl_threshold {
        // 缓存有效 → 延长 TTL
        (current_ttl as f64 * config.ttl_increase_factor) as u64
    } else {
        // 持平 → 无需调整
        return None;
    };

    // Clamp 到 [min_ttl_secs, max_ttl_secs]
    Some(new_ttl.clamp(config.min_ttl_secs, config.max_ttl_secs))
}

/// 综合分析, 生成调优决策
///
/// 这是主入口函数, 按优先级判断:
/// 1. 数据不足 → `KeepCurrent`
/// 2. `should_disable_cache` → `DisableCache`
/// 3. `should_adjust_ttl` → `AdjustTtl`
/// 4. 否则 → `KeepCurrent`
///
/// # 参数
///
/// - `corr`: 缓存修复关联分析
/// - `stats`: 缓存统计 (当前未用于决策, 保留用于未来扩展)
/// - `current_ttl`: 当前 TTL (秒)
/// - `config`: 调优配置
///
/// # 示例
///
/// ```
/// # use forge::cache_tuning::{make_tuning_decision, CacheTuningConfig};
/// # use forge::dev_trace::CacheFixCorrelation;
/// # use forge::search_cache::CacheStats;
/// let corr = CacheFixCorrelation::new();
/// let stats = CacheStats::new();
/// let config = CacheTuningConfig::default();
///
/// // 无数据 → 保持当前
/// let decision = make_tuning_decision(&corr, &stats, 1800, &config);
/// assert!(decision.is_keep());
/// ```
pub fn make_tuning_decision(
    corr: &CacheFixCorrelation,
    _stats: &CacheStats,
    current_ttl: u64,
    config: &CacheTuningConfig,
) -> CacheTuningDecision {
    let diff = corr.hit_vs_miss_diff();

    // 1. 数据不足
    if !has_sufficient_data(corr, config.min_samples) {
        return CacheTuningDecision::keep_current(current_ttl, diff, "数据不足, 保持当前配置");
    }

    // 2. 禁用缓存
    if should_disable_cache(corr, config) {
        return CacheTuningDecision::disable_cache(
            current_ttl,
            diff,
            format!(
                "缓存命中修复率 {:.1}% 远低于未命中 {:.1}% (差值 {:+.1}%)",
                corr.hit_fix_rate() * 100.0,
                corr.miss_fix_rate() * 100.0,
                diff * 100.0
            ),
        );
    }

    // 3. 调整 TTL
    if let Some(new_ttl) = compute_new_ttl(current_ttl, corr, config) {
        if new_ttl == current_ttl {
            // Clamp 后与当前相同
            return CacheTuningDecision::keep_current(
                current_ttl,
                diff,
                "TTL 已在最优范围, 无需调整",
            );
        }
        let direction = if new_ttl < current_ttl {
            "缩短"
        } else {
            "延长"
        };
        return CacheTuningDecision::adjust_ttl(
            current_ttl,
            new_ttl,
            diff,
            format!(
                "差值 {:+.1}%, {} TTL ({}s → {}s)",
                diff * 100.0,
                direction,
                current_ttl,
                new_ttl
            ),
        );
    }

    // 4. 保持当前
    CacheTuningDecision::keep_current(
        current_ttl,
        diff,
        format!(
            "差值 {:+.1}% 在正常范围 [{:+.0}%, {:+.0}%], 无需调整",
            diff * 100.0,
            config.reduce_ttl_threshold * 100.0,
            config.increase_ttl_threshold * 100.0
        ),
    )
}

// ============================================================================
//  CacheTuner — 缓存调优器 (状态管理)
// ============================================================================

/// 缓存调优器 — 管理缓存 TTL 的自动调整状态
///
/// # 设计
///
/// `CacheTuner` 封装了缓存调优的状态和逻辑:
/// - `config`: 调优配置 (阈值和参数)
/// - `current_ttl`: 当前 TTL (秒), 随调优决策动态更新
/// - `enabled`: 缓存是否启用 (禁用后不再缓存)
/// - `adjustment_count`: 累计调整次数
/// - `last_decision`: 最近一次调优决策
///
/// # 用法
///
/// ```
/// use forge::cache_tuning::{CacheTuner, CacheTuningConfig};
/// use forge::dev_trace::CacheFixCorrelation;
/// use forge::search_cache::CacheStats;
///
/// let mut tuner = CacheTuner::new(CacheTuningConfig::default(), 1800);
///
/// let mut corr = CacheFixCorrelation::new();
/// corr.record_hit_check(true);
/// corr.record_hit_check(true);
/// corr.record_hit_check(true);
/// corr.record_miss_check(true);
/// corr.record_miss_check(true);
/// corr.record_miss_check(false);
///
/// let stats = CacheStats::new();
/// let decision = tuner.evaluate(&corr, &stats);
/// tuner.apply_decision(&decision);
///
/// assert!(tuner.is_enabled());
/// assert!(tuner.current_ttl() > 1800); // 延长了 TTL
/// ```
#[derive(Debug, Clone)]
pub struct CacheTuner {
    /// 调优配置
    config: CacheTuningConfig,
    /// 当前 TTL (秒)
    current_ttl: u64,
    /// 缓存是否启用
    enabled: bool,
    /// 累计调整次数
    adjustment_count: u32,
    /// 累计禁用次数
    disable_count: u32,
    /// 最近一次调优决策
    last_decision: Option<CacheTuningDecision>,
}

impl CacheTuner {
    /// 创建新的缓存调优器
    ///
    /// # 参数
    ///
    /// - `config`: 调优配置
    /// - `initial_ttl`: 初始 TTL (秒)
    pub fn new(config: CacheTuningConfig, initial_ttl: u64) -> Self {
        Self {
            config,
            current_ttl: initial_ttl,
            enabled: true,
            adjustment_count: 0,
            disable_count: 0,
            last_decision: None,
        }
    }

    /// 使用默认配置创建调优器
    ///
    /// 初始 TTL = 1800s (30 分钟)
    pub fn with_default_config(initial_ttl: u64) -> Self {
        Self::new(CacheTuningConfig::default(), initial_ttl)
    }

    /// 评估当前关联分析, 生成调优决策
    ///
    /// 这是一个纯函数调用, 不修改 `CacheTuner` 状态。
    /// 调用 [`apply_decision`][] 来应用决策。
    ///
    /// # 参数
    ///
    /// - `corr`: 缓存修复关联分析
    /// - `stats`: 缓存统计
    pub fn evaluate(&self, corr: &CacheFixCorrelation, stats: &CacheStats) -> CacheTuningDecision {
        // 如果缓存已禁用, 不再生成调整决策 (但可以生成 KeepCurrent)
        if !self.enabled {
            return CacheTuningDecision::keep_current(
                self.current_ttl,
                corr.hit_vs_miss_diff(),
                "缓存已禁用",
            );
        }
        make_tuning_decision(corr, stats, self.current_ttl, &self.config)
    }

    /// 应用调优决策, 更新内部状态
    ///
    /// # 参数
    ///
    /// - `decision`: 调优决策
    pub fn apply_decision(&mut self, decision: &CacheTuningDecision) {
        match &decision.action {
            TuningAction::KeepCurrent => {}
            TuningAction::AdjustTtl { new_ttl } => {
                self.current_ttl = *new_ttl;
                self.adjustment_count += 1;
            }
            TuningAction::DisableCache => {
                self.enabled = false;
                self.disable_count += 1;
            }
        }
        self.last_decision = Some(decision.clone());
    }

    /// 一步评估并应用 — 等价于 `evaluate` + `apply_decision`
    ///
    /// 返回生成的调优决策。
    pub fn evaluate_and_apply(
        &mut self,
        corr: &CacheFixCorrelation,
        stats: &CacheStats,
    ) -> CacheTuningDecision {
        let decision = self.evaluate(corr, stats);
        self.apply_decision(&decision);
        decision
    }

    /// 重新启用缓存 (如果之前被禁用)
    ///
    /// 重置 `enabled = true`, 不改变 TTL。
    pub fn re_enable(&mut self) {
        self.enabled = true;
    }

    /// 获取调优配置引用
    pub fn config(&self) -> &CacheTuningConfig {
        &self.config
    }

    /// 当前 TTL (秒)
    pub fn current_ttl(&self) -> u64 {
        self.current_ttl
    }

    /// 缓存是否启用
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// 累计调整次数 (TTL 调整)
    pub fn adjustment_count(&self) -> u32 {
        self.adjustment_count
    }

    /// 累计禁用次数
    pub fn disable_count(&self) -> u32 {
        self.disable_count
    }

    /// 最近一次调优决策
    pub fn last_decision(&self) -> Option<&CacheTuningDecision> {
        self.last_decision.as_ref()
    }

    /// 格式化为可读字符串
    pub fn to_summary(&self) -> String {
        let status = if self.enabled { "启用" } else { "禁用" };
        let last = match &self.last_decision {
            Some(d) => d.to_summary(),
            None => "无".to_string(),
        };
        format!(
            "缓存调优器: 状态={}, TTL={}s, 调整{}次, 禁用{}次, 最近决策: {}",
            status, self.current_ttl, self.adjustment_count, self.disable_count, last
        )
    }
}

// ============================================================================
//  测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // ===== CacheTuningConfig =====

    #[test]
    fn test_config_default() {
        let config = CacheTuningConfig::default();
        assert_eq!(config.min_samples, 3);
        assert!((config.disable_threshold - (-0.15)).abs() < 0.001);
        assert!((config.reduce_ttl_threshold - (-0.05)).abs() < 0.001);
        assert!((config.increase_ttl_threshold - 0.05).abs() < 0.001);
        assert!((config.ttl_reduce_factor - 0.5).abs() < 0.001);
        assert!((config.ttl_increase_factor - 1.5).abs() < 0.001);
        assert_eq!(config.min_ttl_secs, 60);
        assert_eq!(config.max_ttl_secs, 7200);
    }

    #[test]
    fn test_config_new() {
        let config = CacheTuningConfig::new();
        assert_eq!(config.min_samples, DEFAULT_MIN_SAMPLES);
    }

    #[test]
    fn test_config_conservative() {
        let config = CacheTuningConfig::conservative();
        assert_eq!(config.min_samples, 5);
        assert!((config.disable_threshold - (-0.25)).abs() < 0.001);
        assert!((config.ttl_reduce_factor - 0.75).abs() < 0.001);
    }

    #[test]
    fn test_config_aggressive() {
        let config = CacheTuningConfig::aggressive();
        assert_eq!(config.min_samples, 2);
        assert!((config.disable_threshold - (-0.05)).abs() < 0.001);
        assert!((config.ttl_increase_factor - 2.0).abs() < 0.001);
    }

    #[test]
    fn test_config_builder_methods() {
        let config = CacheTuningConfig::default()
            .with_min_samples(5)
            .with_disable_threshold(-0.20);
        assert_eq!(config.min_samples, 5);
        assert!((config.disable_threshold - (-0.20)).abs() < 0.001);
    }

    // ===== TuningAction =====

    #[test]
    fn test_tuning_action_equality() {
        assert_eq!(TuningAction::KeepCurrent, TuningAction::KeepCurrent);
        assert_eq!(
            TuningAction::AdjustTtl { new_ttl: 900 },
            TuningAction::AdjustTtl { new_ttl: 900 }
        );
        assert_ne!(
            TuningAction::AdjustTtl { new_ttl: 900 },
            TuningAction::AdjustTtl { new_ttl: 1800 }
        );
        assert_eq!(TuningAction::DisableCache, TuningAction::DisableCache);
        assert_ne!(TuningAction::KeepCurrent, TuningAction::DisableCache);
    }

    // ===== CacheTuningDecision =====

    #[test]
    fn test_decision_keep_current() {
        let d = CacheTuningDecision::keep_current(1800, 0.0, "测试");
        assert!(d.is_keep());
        assert!(!d.is_adjust());
        assert!(!d.is_disable());
        assert_eq!(d.new_ttl(), None);
        assert_eq!(d.old_ttl, 1800);
    }

    #[test]
    fn test_decision_adjust_ttl() {
        let d = CacheTuningDecision::adjust_ttl(1800, 900, -0.1, "缩短");
        assert!(!d.is_keep());
        assert!(d.is_adjust());
        assert!(!d.is_disable());
        assert_eq!(d.new_ttl(), Some(900));
    }

    #[test]
    fn test_decision_disable_cache() {
        let d = CacheTuningDecision::disable_cache(1800, -0.2, "缓存有害");
        assert!(!d.is_keep());
        assert!(!d.is_adjust());
        assert!(d.is_disable());
        assert_eq!(d.new_ttl(), None);
    }

    #[test]
    fn test_decision_to_summary_keep() {
        let d = CacheTuningDecision::keep_current(1800, 0.0, "正常");
        let s = d.to_summary();
        assert!(s.contains("保持当前配置"));
        assert!(s.contains("正常"));
    }

    #[test]
    fn test_decision_to_summary_adjust() {
        let d = CacheTuningDecision::adjust_ttl(1800, 2700, 0.1, "延长");
        let s = d.to_summary();
        assert!(s.contains("1800s → 2700s"));
        assert!(s.contains("延长"));
    }

    #[test]
    fn test_decision_to_summary_disable() {
        let d = CacheTuningDecision::disable_cache(1800, -0.3, "有害");
        let s = d.to_summary();
        assert!(s.contains("禁用缓存"));
        assert!(s.contains("有害"));
    }

    // ===== has_sufficient_data =====

    #[test]
    fn test_sufficient_data_empty() {
        let corr = CacheFixCorrelation::new();
        assert!(!has_sufficient_data(&corr, 3));
    }

    #[test]
    fn test_sufficient_data_only_hit() {
        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        assert!(!has_sufficient_data(&corr, 3)); // 缺少 miss
    }

    #[test]
    fn test_sufficient_data_only_miss() {
        let mut corr = CacheFixCorrelation::new();
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        assert!(!has_sufficient_data(&corr, 3)); // 缺少 hit
    }

    #[test]
    fn test_sufficient_data_both() {
        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        assert!(has_sufficient_data(&corr, 3));
    }

    #[test]
    fn test_sufficient_data_partial() {
        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        assert!(!has_sufficient_data(&corr, 3)); // hit 只有 2 次
    }

    #[test]
    fn test_sufficient_data_min_samples_1() {
        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(true);
        corr.record_miss_check(true);
        assert!(has_sufficient_data(&corr, 1));
    }

    // ===== should_disable_cache =====

    #[test]
    fn test_should_disable_cache_harmful() {
        let mut corr = CacheFixCorrelation::new();
        // 命中: 0/3 = 0%, 未命中: 3/3 = 100%, diff = -100%
        corr.record_hit_check(false);
        corr.record_hit_check(false);
        corr.record_hit_check(false);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);

        let config = CacheTuningConfig::default();
        assert!(should_disable_cache(&corr, &config));
    }

    #[test]
    fn test_should_disable_cache_slightly_harmful() {
        let mut corr = CacheFixCorrelation::new();
        // 命中: 1/3 ≈ 33%, 未命中: 3/3 = 100%, diff ≈ -67%
        corr.record_hit_check(true);
        corr.record_hit_check(false);
        corr.record_hit_check(false);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);

        let config = CacheTuningConfig::default();
        assert!(should_disable_cache(&corr, &config)); // -67% < -15%
    }

    #[test]
    fn test_should_disable_cache_effective() {
        let mut corr = CacheFixCorrelation::new();
        // 命中: 3/3 = 100%, 未命中: 3/3 = 100%, diff = 0%
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);

        let config = CacheTuningConfig::default();
        assert!(!should_disable_cache(&corr, &config)); // 0% > -15%
    }

    #[test]
    fn test_should_disable_cache_insufficient_data() {
        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(false);
        corr.record_miss_check(true);

        let config = CacheTuningConfig::default();
        assert!(!should_disable_cache(&corr, &config)); // 数据不足
    }

    #[test]
    fn test_should_disable_cache_borderline() {
        let mut corr = CacheFixCorrelation::new();
        // 命中: 2/3 ≈ 67%, 未命中: 3/3 = 100%, diff ≈ -33%
        // -33% < -15% → 禁用
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_hit_check(false);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);

        let config = CacheTuningConfig::default();
        assert!(should_disable_cache(&corr, &config));
    }

    // ===== should_adjust_ttl =====

    #[test]
    fn test_should_adjust_ttl_increase() {
        let mut corr = CacheFixCorrelation::new();
        // 命中: 3/3 = 100%, 未命中: 2/3 ≈ 67%, diff ≈ 33% > 5%
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(false);

        let config = CacheTuningConfig::default();
        assert!(should_adjust_ttl(&corr, &config));
    }

    #[test]
    fn test_should_adjust_ttl_reduce() {
        // 命中: 9/10=90%, 未命中: 10/10=100%, diff=-10%
        // diff 在 [disable_threshold=-15%, reduce_ttl_threshold=-5%] → 缩短 TTL
        let mut corr = CacheFixCorrelation::new();
        for _ in 0..9 {
            corr.record_hit_check(true);
        }
        corr.record_hit_check(false);
        for _ in 0..10 {
            corr.record_miss_check(true);
        }
        let config = CacheTuningConfig::default();
        assert!(should_adjust_ttl(&corr, &config));
    }

    #[test]
    fn test_should_adjust_ttl_no_need() {
        let mut corr = CacheFixCorrelation::new();
        // 命中: 3/3 = 100%, 未命中: 3/3 = 100%, diff = 0% → 无需调整
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);

        let config = CacheTuningConfig::default();
        assert!(!should_adjust_ttl(&corr, &config));
    }

    #[test]
    fn test_should_adjust_ttl_insufficient_data() {
        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(true);
        corr.record_miss_check(true);

        let config = CacheTuningConfig::default();
        assert!(!should_adjust_ttl(&corr, &config));
    }

    #[test]
    fn test_should_adjust_ttl_should_disable_instead() {
        let mut corr = CacheFixCorrelation::new();
        // diff = -100% → 应该禁用, 不是调整
        corr.record_hit_check(false);
        corr.record_hit_check(false);
        corr.record_hit_check(false);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);

        let config = CacheTuningConfig::default();
        assert!(!should_adjust_ttl(&corr, &config)); // 应禁用而非调整
    }

    // ===== compute_new_ttl =====

    #[test]
    fn test_compute_new_ttl_increase() {
        let mut corr = CacheFixCorrelation::new();
        // diff = 33% > 5% → 延长: 1800 × 1.5 = 2700
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(false);

        let config = CacheTuningConfig::default();
        let new_ttl = compute_new_ttl(1800, &corr, &config).unwrap();
        assert_eq!(new_ttl, 2700);
    }

    #[test]
    fn test_compute_new_ttl_reduce() {
        let mut corr = CacheFixCorrelation::new();
        // diff = -10% → 缩短: 1800 × 0.5 = 900
        for _ in 0..9 {
            corr.record_hit_check(true);
        }
        corr.record_hit_check(false);
        for _ in 0..10 {
            corr.record_miss_check(true);
        }

        let config = CacheTuningConfig::default();
        let new_ttl = compute_new_ttl(1800, &corr, &config).unwrap();
        assert_eq!(new_ttl, 900);
    }

    #[test]
    fn test_compute_new_ttl_no_change() {
        let mut corr = CacheFixCorrelation::new();
        // diff = 0 → 无需调整
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);

        let config = CacheTuningConfig::default();
        assert!(compute_new_ttl(1800, &corr, &config).is_none());
    }

    #[test]
    fn test_compute_new_ttl_insufficient_data() {
        let corr = CacheFixCorrelation::new();
        let config = CacheTuningConfig::default();
        assert!(compute_new_ttl(1800, &corr, &config).is_none());
    }

    #[test]
    fn test_compute_new_ttl_should_disable() {
        let mut corr = CacheFixCorrelation::new();
        // diff = -100% → 应禁用, compute_new_ttl 返回 None
        corr.record_hit_check(false);
        corr.record_hit_check(false);
        corr.record_hit_check(false);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);

        let config = CacheTuningConfig::default();
        assert!(compute_new_ttl(1800, &corr, &config).is_none());
    }

    #[test]
    fn test_compute_new_ttl_clamp_min() {
        let mut corr = CacheFixCorrelation::new();
        // diff = -10% → 缩短: 100 × 0.5 = 50 → clamp to 60
        for _ in 0..9 {
            corr.record_hit_check(true);
        }
        corr.record_hit_check(false);
        for _ in 0..10 {
            corr.record_miss_check(true);
        }

        let config = CacheTuningConfig::default();
        let new_ttl = compute_new_ttl(100, &corr, &config).unwrap();
        assert_eq!(new_ttl, 60); // clamped to min_ttl_secs
    }

    #[test]
    fn test_compute_new_ttl_clamp_max() {
        let mut corr = CacheFixCorrelation::new();
        // diff = 33% → 延长: 5000 × 1.5 = 7500 → clamp to 7200
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(false);

        let config = CacheTuningConfig::default();
        let new_ttl = compute_new_ttl(5000, &corr, &config).unwrap();
        assert_eq!(new_ttl, 7200); // clamped to max_ttl_secs
    }

    // ===== make_tuning_decision =====

    #[test]
    fn test_make_decision_insufficient_data() {
        let corr = CacheFixCorrelation::new();
        let stats = CacheStats::new();
        let config = CacheTuningConfig::default();

        let decision = make_tuning_decision(&corr, &stats, 1800, &config);
        assert!(decision.is_keep());
        assert_eq!(decision.old_ttl, 1800);
    }

    #[test]
    fn test_make_decision_disable() {
        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(false);
        corr.record_hit_check(false);
        corr.record_hit_check(false);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);

        let stats = CacheStats::new();
        let config = CacheTuningConfig::default();

        let decision = make_tuning_decision(&corr, &stats, 1800, &config);
        assert!(decision.is_disable());
    }

    #[test]
    fn test_make_decision_increase_ttl() {
        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(false);

        let stats = CacheStats::new();
        let config = CacheTuningConfig::default();

        let decision = make_tuning_decision(&corr, &stats, 1800, &config);
        assert!(decision.is_adjust());
        assert_eq!(decision.new_ttl(), Some(2700));
    }

    #[test]
    fn test_make_decision_reduce_ttl() {
        let mut corr = CacheFixCorrelation::new();
        for _ in 0..9 {
            corr.record_hit_check(true);
        }
        corr.record_hit_check(false);
        for _ in 0..10 {
            corr.record_miss_check(true);
        }

        let stats = CacheStats::new();
        let config = CacheTuningConfig::default();

        let decision = make_tuning_decision(&corr, &stats, 1800, &config);
        assert!(decision.is_adjust());
        assert_eq!(decision.new_ttl(), Some(900));
    }

    #[test]
    fn test_make_decision_keep_when_equal() {
        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);

        let stats = CacheStats::new();
        let config = CacheTuningConfig::default();

        let decision = make_tuning_decision(&corr, &stats, 1800, &config);
        assert!(decision.is_keep());
    }

    #[test]
    fn test_make_decision_keep_when_clamped_same() {
        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(false);

        let stats = CacheStats::new();
        let config = CacheTuningConfig::default();

        // TTL 已经是 max, 延长后 clamp 还是 max → 保持
        let decision = make_tuning_decision(&corr, &stats, 7200, &config);
        // 7200 × 1.5 = 10800 → clamp to 7200 → 与当前相同 → KeepCurrent
        assert!(decision.is_keep());
    }

    #[test]
    fn test_make_decision_reason_not_empty() {
        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(false);
        corr.record_hit_check(false);
        corr.record_hit_check(false);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);

        let stats = CacheStats::new();
        let config = CacheTuningConfig::default();

        let decision = make_tuning_decision(&corr, &stats, 1800, &config);
        assert!(!decision.reason.is_empty());
    }

    // ===== CacheTuner =====

    #[test]
    fn test_tuner_new() {
        let tuner = CacheTuner::new(CacheTuningConfig::default(), 1800);
        assert_eq!(tuner.current_ttl(), 1800);
        assert!(tuner.is_enabled());
        assert_eq!(tuner.adjustment_count(), 0);
        assert_eq!(tuner.disable_count(), 0);
        assert!(tuner.last_decision().is_none());
    }

    #[test]
    fn test_tuner_with_default_config() {
        let tuner = CacheTuner::with_default_config(1800);
        assert_eq!(tuner.current_ttl(), 1800);
        assert!(tuner.is_enabled());
    }

    #[test]
    fn test_tuner_evaluate_keep() {
        let mut tuner = CacheTuner::with_default_config(1800);
        let corr = CacheFixCorrelation::new();
        let stats = CacheStats::new();

        let decision = tuner.evaluate_and_apply(&corr, &stats);
        assert!(decision.is_keep());
        assert_eq!(tuner.current_ttl(), 1800);
        assert!(tuner.is_enabled());
    }

    #[test]
    fn test_tuner_evaluate_disable() {
        let mut tuner = CacheTuner::with_default_config(1800);

        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(false);
        corr.record_hit_check(false);
        corr.record_hit_check(false);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        let stats = CacheStats::new();

        let decision = tuner.evaluate_and_apply(&corr, &stats);
        assert!(decision.is_disable());
        assert!(!tuner.is_enabled());
        assert_eq!(tuner.disable_count(), 1);
    }

    #[test]
    fn test_tuner_evaluate_increase_ttl() {
        let mut tuner = CacheTuner::with_default_config(1800);

        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(false);
        let stats = CacheStats::new();

        let decision = tuner.evaluate_and_apply(&corr, &stats);
        assert!(decision.is_adjust());
        assert_eq!(tuner.current_ttl(), 2700);
        assert_eq!(tuner.adjustment_count(), 1);
    }

    #[test]
    fn test_tuner_evaluate_reduce_ttl() {
        let mut tuner = CacheTuner::with_default_config(1800);

        let mut corr = CacheFixCorrelation::new();
        for _ in 0..9 {
            corr.record_hit_check(true);
        }
        corr.record_hit_check(false);
        for _ in 0..10 {
            corr.record_miss_check(true);
        }
        let stats = CacheStats::new();

        let decision = tuner.evaluate_and_apply(&corr, &stats);
        assert!(decision.is_adjust());
        assert_eq!(tuner.current_ttl(), 900);
        assert_eq!(tuner.adjustment_count(), 1);
    }

    #[test]
    fn test_tuner_multiple_adjustments() {
        let mut tuner = CacheTuner::with_default_config(1800);

        // 第一次: 延长 TTL
        let mut corr1 = CacheFixCorrelation::new();
        corr1.record_hit_check(true);
        corr1.record_hit_check(true);
        corr1.record_hit_check(true);
        corr1.record_miss_check(true);
        corr1.record_miss_check(true);
        corr1.record_miss_check(false);
        let stats = CacheStats::new();
        tuner.evaluate_and_apply(&corr1, &stats);
        assert_eq!(tuner.current_ttl(), 2700);

        // 第二次: 再延长 TTL (从 2700)
        tuner.evaluate_and_apply(&corr1, &stats);
        assert_eq!(tuner.current_ttl(), 4050); // 2700 × 1.5
        assert_eq!(tuner.adjustment_count(), 2);
    }

    #[test]
    fn test_tuner_disable_then_re_enable() {
        let mut tuner = CacheTuner::with_default_config(1800);

        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(false);
        corr.record_hit_check(false);
        corr.record_hit_check(false);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        let stats = CacheStats::new();

        tuner.evaluate_and_apply(&corr, &stats);
        assert!(!tuner.is_enabled());

        // 禁用后 evaluate 返回 KeepCurrent
        let decision = tuner.evaluate(&corr, &stats);
        assert!(decision.is_keep());

        // 重新启用
        tuner.re_enable();
        assert!(tuner.is_enabled());
    }

    #[test]
    fn test_tuner_apply_decision_directly() {
        let mut tuner = CacheTuner::with_default_config(1800);
        let decision = CacheTuningDecision::adjust_ttl(1800, 600, -0.1, "缩短");
        tuner.apply_decision(&decision);
        assert_eq!(tuner.current_ttl(), 600);
        assert_eq!(tuner.adjustment_count(), 1);
        assert!(tuner.last_decision().is_some());
    }

    #[test]
    fn test_tuner_to_summary() {
        let tuner = CacheTuner::with_default_config(1800);
        let s = tuner.to_summary();
        assert!(s.contains("启用"));
        assert!(s.contains("1800s"));
    }

    #[test]
    fn test_tuner_to_summary_disabled() {
        let mut tuner = CacheTuner::with_default_config(1800);
        let decision = CacheTuningDecision::disable_cache(1800, -0.2, "有害");
        tuner.apply_decision(&decision);
        let s = tuner.to_summary();
        assert!(s.contains("禁用"));
    }

    // ===== 集成场景测试 =====

    #[test]
    fn test_integration_harmful_then_fix() {
        // 场景: 缓存有害 → 禁用 → 修复后关联变好 → 重新启用 → 延长 TTL
        let mut tuner = CacheTuner::with_default_config(1800);

        // 阶段1: 缓存有害
        let mut corr_bad = CacheFixCorrelation::new();
        corr_bad.record_hit_check(false);
        corr_bad.record_hit_check(false);
        corr_bad.record_hit_check(false);
        corr_bad.record_miss_check(true);
        corr_bad.record_miss_check(true);
        corr_bad.record_miss_check(true);
        let stats = CacheStats::new();

        let d1 = tuner.evaluate_and_apply(&corr_bad, &stats);
        assert!(d1.is_disable());
        assert!(!tuner.is_enabled());

        // 阶段2: 重新启用
        tuner.re_enable();

        // 阶段3: 缓存变好
        let mut corr_good = CacheFixCorrelation::new();
        corr_good.record_hit_check(true);
        corr_good.record_hit_check(true);
        corr_good.record_hit_check(true);
        corr_good.record_miss_check(true);
        corr_good.record_miss_check(true);
        corr_good.record_miss_check(false);

        let d2 = tuner.evaluate_and_apply(&corr_good, &stats);
        assert!(d2.is_adjust());
        assert!(tuner.current_ttl() > 1800);
    }

    #[test]
    fn test_integration_progressive_ttl_increase() {
        // 场景: 缓存持续有效, TTL 逐步延长
        let mut tuner = CacheTuner::with_default_config(600);

        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(false);
        let stats = CacheStats::new();

        // 600 → 900 → 1350 → 2025
        tuner.evaluate_and_apply(&corr, &stats);
        assert_eq!(tuner.current_ttl(), 900);

        tuner.evaluate_and_apply(&corr, &stats);
        assert_eq!(tuner.current_ttl(), 1350);

        tuner.evaluate_and_apply(&corr, &stats);
        assert_eq!(tuner.current_ttl(), 2025);

        assert_eq!(tuner.adjustment_count(), 3);
    }

    #[test]
    fn test_integration_conservative_config() {
        let mut tuner = CacheTuner::new(CacheTuningConfig::conservative(), 1800);

        // 用 conservative 配置, 需要更多样本 (5)
        let mut corr = CacheFixCorrelation::new();
        // 只有 3 个样本, 不足 5
        corr.record_hit_check(false);
        corr.record_hit_check(false);
        corr.record_hit_check(false);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        let stats = CacheStats::new();

        let decision = tuner.evaluate_and_apply(&corr, &stats);
        assert!(decision.is_keep()); // 数据不足 (min_samples=5)
    }

    #[test]
    fn test_integration_aggressive_config() {
        let mut tuner = CacheTuner::new(CacheTuningConfig::aggressive(), 1800);

        // 用 aggressive 配置, 只需 2 个样本
        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(false);
        corr.record_hit_check(false);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        let stats = CacheStats::new();

        let decision = tuner.evaluate_and_apply(&corr, &stats);
        // diff = 0% - 100% = -100% < -5% (aggressive disable_threshold) → 禁用
        assert!(decision.is_disable());
    }

    #[test]
    fn test_integration_ttl_at_boundary() {
        // TTL 在边界值时不超出
        let mut tuner = CacheTuner::with_default_config(7200); // max_ttl

        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(false);
        let stats = CacheStats::new();

        let decision = tuner.evaluate_and_apply(&corr, &stats);
        // 7200 × 1.5 = 10800 → clamp to 7200 → 与当前相同 → keep
        assert!(decision.is_keep());
        assert_eq!(tuner.current_ttl(), 7200);
    }

    // ===== proptest 属性测试 =====

    proptest! {
        #[test]
        fn prop_sufficient_data_symmetric(
            hits in 0u32..20,
            misses in 0u32..20,
            min_samples in 1usize..10,
        ) {
            let mut corr = CacheFixCorrelation::new();
            for _ in 0..hits {
                corr.record_hit_check(true);
            }
            for _ in 0..misses {
                corr.record_miss_check(true);
            }
            let result = has_sufficient_data(&corr, min_samples);
            prop_assert_eq!(result, hits as usize >= min_samples && misses as usize >= min_samples);
        }

        #[test]
        fn prop_disable_implies_negative_diff(
            hit_successes in 0u32..10,
            hit_checks in 1u32..10,
            miss_successes in 0u32..10,
            miss_checks in 1u32..10,
        ) {
            // 确保 hit_successes <= hit_checks, miss_successes <= miss_checks
            let hit_successes = hit_successes.min(hit_checks);
            let miss_successes = miss_successes.min(miss_checks);

            let mut corr = CacheFixCorrelation::new();
            for i in 0..hit_checks {
                corr.record_hit_check(i < hit_successes);
            }
            for i in 0..miss_checks {
                corr.record_miss_check(i < miss_successes);
            }

            let config = CacheTuningConfig::default();
            if should_disable_cache(&corr, &config) {
                // 如果决定禁用, diff 必须为负
                prop_assert!(corr.hit_vs_miss_diff() < 0.0);
            }
        }

        #[test]
        fn prop_new_ttl_in_range(
            current_ttl in 60u64..7200,
            hit_successes in 0u32..10,
            hit_checks in 3u32..10,
            miss_successes in 0u32..10,
            miss_checks in 3u32..10,
        ) {
            let hit_successes = hit_successes.min(hit_checks);
            let miss_successes = miss_successes.min(miss_checks);

            let mut corr = CacheFixCorrelation::new();
            for i in 0..hit_checks {
                corr.record_hit_check(i < hit_successes);
            }
            for i in 0..miss_checks {
                corr.record_miss_check(i < miss_successes);
            }

            let config = CacheTuningConfig::default();
            if let Some(new_ttl) = compute_new_ttl(current_ttl, &corr, &config) {
                prop_assert!(new_ttl >= config.min_ttl_secs);
                prop_assert!(new_ttl <= config.max_ttl_secs);
            }
        }

        #[test]
        fn prop_keep_decision_preserves_ttl(
            current_ttl in 60u64..7200,
        ) {
            let corr = CacheFixCorrelation::new(); // 空数据
            let stats = CacheStats::new();
            let config = CacheTuningConfig::default();
            let decision = make_tuning_decision(&corr, &stats, current_ttl, &config);
            prop_assert!(decision.is_keep());
            prop_assert_eq!(decision.old_ttl, current_ttl);
        }
    }
}
