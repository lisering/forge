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
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tracing::{info, warn};

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

/// 从调优决策列表中提取 TTL 变化轨迹 (Session 94)
///
/// 遍历决策列表, 返回每次决策后的 TTL 值序列, 用于 sparkline 可视化。
///
/// - `KeepCurrent`: TTL 不变, 使用 `old_ttl`
/// - `AdjustTtl { new_ttl }`: TTL 变为 `new_ttl`
/// - `DisableCache`: TTL 不变 (缓存禁用但 TTL 值保留), 使用 `old_ttl`
///
/// # 参数
///
/// - `decisions`: 调优决策列表 (按时间顺序)
///
/// # 返回
///
/// TTL 值序列 (秒), 每个元素对应一次决策后的 TTL。
/// 空列表返回空 `Vec`。
///
/// # 示例
///
/// ```
/// # use forge::cache_tuning::{extract_ttl_trajectory, CacheTuningDecision};
/// let decisions = vec![
///     CacheTuningDecision::keep_current(1800, 0.0, "正常"),
///     CacheTuningDecision::adjust_ttl(1800, 2700, 0.3, "延长"),
///     CacheTuningDecision::keep_current(2700, 0.1, "稳定"),
/// ];
/// let trajectory = extract_ttl_trajectory(&decisions);
/// assert_eq!(trajectory, vec![1800.0, 2700.0, 2700.0]);
/// ```
pub fn extract_ttl_trajectory(decisions: &[CacheTuningDecision]) -> Vec<f64> {
    decisions
        .iter()
        .map(|d| match &d.action {
            TuningAction::AdjustTtl { new_ttl } => *new_ttl as f64,
            TuningAction::KeepCurrent | TuningAction::DisableCache => d.old_ttl as f64,
        })
        .collect()
}

/// 从调优决策列表中提取关联差值序列 (Session 94)
///
/// 返回每次决策的缓存命中与未命中修复成功率差值, 用于 sparkline 可视化。
///
/// # 参数
///
/// - `decisions`: 调优决策列表 (按时间顺序)
///
/// # 返回
///
/// 差值序列 (-1.0 ~ 1.0), 每个元素对应一次决策的 `correlation_diff`。
/// 空列表返回空 `Vec`。
///
/// # 示例
///
/// ```
/// # use forge::cache_tuning::{extract_correlation_diffs, CacheTuningDecision};
/// let decisions = vec![
///     CacheTuningDecision::keep_current(1800, 0.05, "正常"),
///     CacheTuningDecision::adjust_ttl(1800, 2700, 0.30, "延长"),
///     CacheTuningDecision::disable_cache(2700, -0.50, "有害"),
/// ];
/// let diffs = extract_correlation_diffs(&decisions);
/// assert_eq!(diffs, vec![0.05, 0.30, -0.50]);
/// ```
pub fn extract_correlation_diffs(decisions: &[CacheTuningDecision]) -> Vec<f64> {
    decisions.iter().map(|d| d.correlation_diff).collect()
}

/// 持久化文件名 — 存储在 `.forge/cache_tuning_history.json`
pub const TUNING_HISTORY_FILENAME: &str = "cache_tuning_history.json";

// ============================================================================
//  CacheTuningHistory — 调优历史持久化 (Session 84)
// ============================================================================

/// 缓存调优历史 — 跨 session 持久化调优状态和决策记录
///
/// 存储在 `.forge/cache_tuning_history.json`, 在 Orchestrator 启动时加载,
/// 在 `final_report` 时保存。这使得新 session 可以复用上一 session 的
/// 调优经验 (TTL 值、缓存启用状态、累计统计)。
///
/// # 字段
///
/// | 字段 | 说明 |
/// |------|------|
/// | `initial_ttl` | session 开始时的 TTL |
/// | `current_ttl` | 最终 TTL (下次 session 的起始值) |
/// | `enabled` | 缓存是否启用 |
/// | `adjustment_count` | 累计 TTL 调整次数 |
/// | `disable_count` | 累计禁用次数 |
/// | `decisions` | 所有调优决策记录 |
/// | `saved_at` | 保存时间 (ISO 8601) |
///
/// # 示例
///
/// ```
/// use forge::cache_tuning::{CacheTuner, CacheTuningConfig, CacheTuningHistory};
///
/// let mut tuner = CacheTuner::new(CacheTuningConfig::default(), 1800);
/// let history = tuner.to_history();
/// assert_eq!(history.initial_ttl, 1800);
/// assert_eq!(history.current_ttl, 1800);
/// assert!(history.is_empty());
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheTuningHistory {
    /// session 开始时的 TTL (秒)
    pub initial_ttl: u64,
    /// 最终 TTL (秒) — 下次 session 的起始值
    pub current_ttl: u64,
    /// 缓存是否启用
    pub enabled: bool,
    /// 累计 TTL 调整次数
    pub adjustment_count: u32,
    /// 累计禁用次数
    pub disable_count: u32,
    /// 所有调优决策记录 (按时间顺序)
    pub decisions: Vec<CacheTuningDecision>,
    /// 保存时间 (ISO 8601 格式, 可选)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved_at: Option<String>,
}

impl CacheTuningHistory {
    /// 创建空的调优历史
    ///
    /// 默认值: TTL=0, enabled=true, 无决策记录
    pub fn new() -> Self {
        Self {
            initial_ttl: 0,
            current_ttl: 0,
            enabled: true,
            adjustment_count: 0,
            disable_count: 0,
            decisions: vec![],
            saved_at: None,
        }
    }

    /// 是否为空 (无决策记录)
    pub fn is_empty(&self) -> bool {
        self.decisions.is_empty()
    }

    /// 决策记录数
    pub fn decision_count(&self) -> usize {
        self.decisions.len()
    }

    /// TTL 变化量 (current - initial)
    pub fn ttl_delta(&self) -> i64 {
        self.current_ttl as i64 - self.initial_ttl as i64
    }

    /// 格式化为可读摘要字符串
    pub fn to_summary(&self) -> String {
        let status = if self.enabled { "启用" } else { "禁用" };
        let delta = self.ttl_delta();
        let delta_str = if delta > 0 {
            format!(" (+{}s)", delta)
        } else if delta < 0 {
            format!(" ({}s)", delta)
        } else {
            String::new()
        };
        format!(
            "缓存调优历史: 初始TTL={}s, 当前TTL={}s{}, 状态={}, 调整{}次, 禁用{}次, 决策{}条",
            self.initial_ttl,
            self.current_ttl,
            delta_str,
            status,
            self.adjustment_count,
            self.disable_count,
            self.decisions.len()
        )
    }

    /// 从文件加载调优历史
    ///
    /// # 参数
    ///
    /// - `path`: JSON 文件路径
    ///
    /// # 返回
    ///
    /// - `Ok(history)`: 成功加载
    /// - `Err`: 文件不存在或解析失败
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Err(anyhow!("调优历史文件不存在: {}", path.display()));
        }
        let content =
            std::fs::read_to_string(path).map_err(|e| anyhow!("读取调优历史失败: {}", e))?;
        let history: CacheTuningHistory =
            serde_json::from_str(&content).map_err(|e| anyhow!("解析调优历史 JSON 失败: {}", e))?;
        Ok(history)
    }

    /// 保存调优历史到文件
    ///
    /// # 参数
    ///
    /// - `path`: JSON 文件路径
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content =
            serde_json::to_string_pretty(self).map_err(|e| anyhow!("序列化调优历史失败: {}", e))?;
        std::fs::write(path, content).map_err(|e| anyhow!("写入调优历史失败: {}", e))?;
        Ok(())
    }

    /// 从工作区加载 (查找 `.forge/cache_tuning_history.json`)
    ///
    /// # 参数
    ///
    /// - `workspace_root`: 工作区根目录
    ///
    /// # 返回
    ///
    /// - `Some(history)`: 文件存在且加载成功
    /// - `None`: 文件不存在或加载失败 (静默降级, 不报错)
    pub fn load_from_workspace(workspace_root: &Path) -> Option<Self> {
        let path = workspace_root.join(".forge").join(TUNING_HISTORY_FILENAME);
        match Self::load(&path) {
            Ok(h) => Some(h),
            Err(e) => {
                warn!("加载缓存调优历史失败, 使用默认配置: {}", e);
                None
            }
        }
    }

    /// 保存到工作区 (`.forge/cache_tuning_history.json`)
    ///
    /// # 参数
    ///
    /// - `workspace_root`: 工作区根目录
    pub fn save_to_workspace(&self, workspace_root: &Path) -> Result<()> {
        let path = workspace_root.join(".forge").join(TUNING_HISTORY_FILENAME);
        self.save(&path)
    }

    /// 创建带时间戳的副本 (用于保存时自动添加保存时间)
    pub fn with_timestamp(mut self) -> Self {
        self.saved_at = Some(chrono::Utc::now().to_rfc3339());
        self
    }
}

impl Default for CacheTuningHistory {
    fn default() -> Self {
        Self::new()
    }
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
/// - `initial_ttl`: 初始 TTL (秒), 用于跟踪 TTL 变化
/// - `current_ttl`: 当前 TTL (秒), 随调优决策动态更新
/// - `enabled`: 缓存是否启用 (禁用后不再缓存)
/// - `adjustment_count`: 累计调整次数
/// - `disable_count`: 累计禁用次数
/// - `last_decision`: 最近一次调优决策
/// - `decisions`: 所有调优决策历史 (Session 84 持久化支持)
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
    /// 初始 TTL (秒) — session 开始时的值, 用于跟踪变化
    initial_ttl: u64,
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
    /// 所有调优决策历史 (Session 84)
    decisions: Vec<CacheTuningDecision>,
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
            initial_ttl,
            current_ttl: initial_ttl,
            enabled: true,
            adjustment_count: 0,
            disable_count: 0,
            last_decision: None,
            decisions: vec![],
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
    /// 同时将决策追加到 `decisions` 历史记录中 (Session 84)。
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
        self.decisions.push(decision.clone());
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

    /// 所有调优决策历史 (按时间顺序)
    ///
    /// 返回所有 `apply_decision` 调用记录的决策列表。
    pub fn decisions(&self) -> &[CacheTuningDecision] {
        &self.decisions
    }

    /// 初始 TTL (秒) — session 开始时的值
    pub fn initial_ttl(&self) -> u64 {
        self.initial_ttl
    }

    /// 导出为调优历史 (用于持久化)
    ///
    /// 将当前 CacheTuner 状态导出为 `CacheTuningHistory`,
    /// 可通过 `save_to_workspace()` 持久化到 `.forge/` 目录。
    ///
    /// # 示例
    ///
    /// ```
    /// use forge::cache_tuning::{CacheTuner, CacheTuningConfig};
    ///
    /// let tuner = CacheTuner::new(CacheTuningConfig::default(), 1800);
    /// let history = tuner.to_history();
    /// assert_eq!(history.initial_ttl, 1800);
    /// assert_eq!(history.current_ttl, 1800);
    /// assert!(history.is_empty());
    /// ```
    pub fn to_history(&self) -> CacheTuningHistory {
        CacheTuningHistory {
            initial_ttl: self.initial_ttl,
            current_ttl: self.current_ttl,
            enabled: self.enabled,
            adjustment_count: self.adjustment_count,
            disable_count: self.disable_count,
            decisions: self.decisions.clone(),
            saved_at: None,
        }
    }

    /// 从调优历史恢复 CacheTuner 状态
    ///
    /// 使用历史记录中的 `current_ttl` 作为新的初始 TTL,
    /// 保留 `enabled` 状态和累计计数, 但清空决策历史
    /// (新 session 的决策将重新积累)。
    ///
    /// # 参数
    ///
    /// - `history`: 调优历史
    /// - `config`: 调优配置 (历史不保存配置, 由调用方提供)
    ///
    /// # 示例
    ///
    /// ```
    /// use forge::cache_tuning::{CacheTuner, CacheTuningConfig, CacheTuningHistory};
    ///
    /// let mut history = CacheTuningHistory::new();
    /// history.current_ttl = 2700;
    /// history.enabled = true;
    /// history.adjustment_count = 2;
    ///
    /// let tuner = CacheTuner::from_history(history, CacheTuningConfig::default());
    /// assert_eq!(tuner.current_ttl(), 2700); // 从历史 TTL 继续
    /// assert_eq!(tuner.initial_ttl(), 2700); // 初始 = 历史 TTL
    /// assert!(tuner.is_enabled());
    /// assert_eq!(tuner.adjustment_count(), 0); // 新 session, 计数从 0 开始
    /// ```
    pub fn from_history(history: CacheTuningHistory, config: CacheTuningConfig) -> Self {
        Self {
            config,
            initial_ttl: history.current_ttl, // 新 session 的初始 = 上次的最终
            current_ttl: history.current_ttl,
            enabled: history.enabled,
            adjustment_count: 0, // 新 session, 重新计数
            disable_count: 0,
            last_decision: None,
            decisions: vec![], // 新 session, 重新积累
        }
    }

    /// 保存调优历史到工作区 (`.forge/cache_tuning_history.json`)
    ///
    /// 将当前状态导出为 `CacheTuningHistory` 并保存到工作区,
    /// 包含自动时间戳。
    ///
    /// # 参数
    ///
    /// - `workspace_root`: 工作区根目录
    pub fn save_to_workspace(&self, workspace_root: &Path) -> Result<()> {
        let history = self.to_history().with_timestamp();
        history.save_to_workspace(workspace_root)
    }

    /// 从工作区加载调优历史并恢复 CacheTuner
    ///
    /// 查找 `.forge/cache_tuning_history.json`, 如果存在则恢复状态。
    /// 文件不存在时返回 `None` (调用方使用默认配置)。
    ///
    /// # 参数
    ///
    /// - `workspace_root`: 工作区根目录
    /// - `config`: 调优配置
    /// - `default_ttl`: 如果无历史记录, 使用的默认 TTL
    ///
    /// # 返回
    ///
    /// - `Some(tuner)`: 成功从历史恢复
    /// - `None`: 无历史文件或加载失败
    pub fn load_from_workspace(
        workspace_root: &Path,
        config: CacheTuningConfig,
        default_ttl: u64,
    ) -> Option<Self> {
        match CacheTuningHistory::load_from_workspace(workspace_root) {
            Some(history) => {
                info!(
                    "📥 加载缓存调优历史: 初始TTL={}s, 当前TTL={}s, 状态={}",
                    history.initial_ttl,
                    history.current_ttl,
                    if history.enabled { "启用" } else { "禁用" }
                );
                Some(Self::from_history(history, config))
            }
            None => {
                // 无历史文件, 使用默认 TTL
                if default_ttl > 0 {
                    Some(Self::new(config, default_ttl))
                } else {
                    None
                }
            }
        }
    }

    /// 格式化为可读字符串
    pub fn to_summary(&self) -> String {
        let status = if self.enabled { "启用" } else { "禁用" };
        let last = match &self.last_decision {
            Some(d) => d.to_summary(),
            None => "无".to_string(),
        };
        format!(
            "缓存调优器: 状态={}, 初始TTL={}s, 当前TTL={}s, 调整{}次, 禁用{}次, 决策{}条, 最近决策: {}",
            status, self.initial_ttl, self.current_ttl, self.adjustment_count, self.disable_count, self.decisions.len(), last
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

    // ===== extract_ttl_trajectory / extract_correlation_diffs (Session 94) =====

    #[test]
    fn test_extract_ttl_trajectory_empty() {
        let trajectory = extract_ttl_trajectory(&[]);
        assert!(trajectory.is_empty());
    }

    #[test]
    fn test_extract_ttl_trajectory_keep_only() {
        let decisions = vec![
            CacheTuningDecision::keep_current(1800, 0.0, "a"),
            CacheTuningDecision::keep_current(1800, 0.05, "b"),
        ];
        let trajectory = extract_ttl_trajectory(&decisions);
        assert_eq!(trajectory, vec![1800.0, 1800.0]);
    }

    #[test]
    fn test_extract_ttl_trajectory_with_adjust() {
        let decisions = vec![
            CacheTuningDecision::keep_current(1800, 0.0, "a"),
            CacheTuningDecision::adjust_ttl(1800, 2700, 0.3, "b"),
            CacheTuningDecision::keep_current(2700, 0.1, "c"),
            CacheTuningDecision::adjust_ttl(2700, 4050, 0.5, "d"),
        ];
        let trajectory = extract_ttl_trajectory(&decisions);
        assert_eq!(trajectory, vec![1800.0, 2700.0, 2700.0, 4050.0]);
    }

    #[test]
    fn test_extract_ttl_trajectory_with_disable() {
        let decisions = vec![
            CacheTuningDecision::adjust_ttl(1800, 900, -0.1, "a"),
            CacheTuningDecision::disable_cache(900, -0.5, "b"),
        ];
        let trajectory = extract_ttl_trajectory(&decisions);
        // DisableCache 保留 old_ttl
        assert_eq!(trajectory, vec![900.0, 900.0]);
    }

    #[test]
    fn test_extract_ttl_trajectory_mixed() {
        let decisions = vec![
            CacheTuningDecision::keep_current(600, 0.0, "a"),
            CacheTuningDecision::adjust_ttl(600, 900, 0.2, "b"),
            CacheTuningDecision::adjust_ttl(900, 1350, 0.3, "c"),
            CacheTuningDecision::disable_cache(1350, -0.4, "d"),
            CacheTuningDecision::keep_current(1350, 0.0, "e"),
        ];
        let trajectory = extract_ttl_trajectory(&decisions);
        assert_eq!(trajectory, vec![600.0, 900.0, 1350.0, 1350.0, 1350.0]);
    }

    #[test]
    fn test_extract_correlation_diffs_empty() {
        let diffs = extract_correlation_diffs(&[]);
        assert!(diffs.is_empty());
    }

    #[test]
    fn test_extract_correlation_diffs_basic() {
        let decisions = vec![
            CacheTuningDecision::keep_current(1800, 0.05, "a"),
            CacheTuningDecision::adjust_ttl(1800, 2700, 0.30, "b"),
            CacheTuningDecision::disable_cache(2700, -0.50, "c"),
        ];
        let diffs = extract_correlation_diffs(&decisions);
        assert_eq!(diffs, vec![0.05, 0.30, -0.50]);
    }

    #[test]
    fn test_extract_correlation_diffs_all_keep() {
        let decisions = vec![
            CacheTuningDecision::keep_current(1800, 0.0, "a"),
            CacheTuningDecision::keep_current(1800, 0.1, "b"),
            CacheTuningDecision::keep_current(1800, -0.05, "c"),
        ];
        let diffs = extract_correlation_diffs(&decisions);
        assert_eq!(diffs, vec![0.0, 0.1, -0.05]);
    }

    #[test]
    fn test_extract_correlation_diffs_with_negatives() {
        let decisions = vec![
            CacheTuningDecision::keep_current(1800, -0.8, "a"),
            CacheTuningDecision::disable_cache(1800, -1.0, "b"),
        ];
        let diffs = extract_correlation_diffs(&decisions);
        assert_eq!(diffs, vec![-0.8, -1.0]);
    }

    // ===== CacheTuningHistory (Session 84: 持久化) =====

    #[test]
    fn test_history_new() {
        let h = CacheTuningHistory::new();
        assert_eq!(h.initial_ttl, 0);
        assert_eq!(h.current_ttl, 0);
        assert!(h.enabled);
        assert_eq!(h.adjustment_count, 0);
        assert_eq!(h.disable_count, 0);
        assert!(h.decisions.is_empty());
        assert!(h.saved_at.is_none());
    }

    #[test]
    fn test_history_default() {
        let h = CacheTuningHistory::default();
        assert!(h.is_empty());
        assert_eq!(h.decision_count(), 0);
    }

    #[test]
    fn test_history_is_empty() {
        let h = CacheTuningHistory::new();
        assert!(h.is_empty());

        let mut h2 = CacheTuningHistory::new();
        h2.decisions
            .push(CacheTuningDecision::keep_current(1800, 0.0, "test"));
        assert!(!h2.is_empty());
    }

    #[test]
    fn test_history_decision_count() {
        let mut h = CacheTuningHistory::new();
        assert_eq!(h.decision_count(), 0);

        h.decisions
            .push(CacheTuningDecision::keep_current(1800, 0.0, "a"));
        h.decisions
            .push(CacheTuningDecision::adjust_ttl(1800, 900, -0.1, "b"));
        assert_eq!(h.decision_count(), 2);
    }

    #[test]
    fn test_history_ttl_delta_zero() {
        let h = CacheTuningHistory {
            initial_ttl: 1800,
            current_ttl: 1800,
            ..CacheTuningHistory::new()
        };
        assert_eq!(h.ttl_delta(), 0);
    }

    #[test]
    fn test_history_ttl_delta_positive() {
        let h = CacheTuningHistory {
            initial_ttl: 1800,
            current_ttl: 2700,
            ..CacheTuningHistory::new()
        };
        assert_eq!(h.ttl_delta(), 900);
    }

    #[test]
    fn test_history_ttl_delta_negative() {
        let h = CacheTuningHistory {
            initial_ttl: 1800,
            current_ttl: 900,
            ..CacheTuningHistory::new()
        };
        assert_eq!(h.ttl_delta(), -900);
    }

    #[test]
    fn test_history_to_summary_empty() {
        let h = CacheTuningHistory::new();
        let s = h.to_summary();
        assert!(s.contains("初始TTL=0s"));
        assert!(s.contains("当前TTL=0s"));
        assert!(s.contains("启用"));
        assert!(s.contains("决策0条"));
    }

    #[test]
    fn test_history_to_summary_with_data() {
        let h = CacheTuningHistory {
            initial_ttl: 1800,
            current_ttl: 2700,
            enabled: true,
            adjustment_count: 1,
            disable_count: 0,
            decisions: vec![CacheTuningDecision::adjust_ttl(1800, 2700, 0.33, "延长")],
            saved_at: None,
        };
        let s = h.to_summary();
        assert!(s.contains("初始TTL=1800s"));
        assert!(s.contains("当前TTL=2700s"));
        assert!(s.contains("+900s"));
        assert!(s.contains("调整1次"));
        assert!(s.contains("决策1条"));
    }

    #[test]
    fn test_history_to_summary_disabled() {
        let h = CacheTuningHistory {
            initial_ttl: 1800,
            current_ttl: 1800,
            enabled: false,
            ..CacheTuningHistory::new()
        };
        let s = h.to_summary();
        assert!(s.contains("禁用"));
    }

    #[test]
    fn test_history_with_timestamp() {
        let h = CacheTuningHistory::new().with_timestamp();
        assert!(h.saved_at.is_some());
        // 验证是有效的 ISO 8601 格式
        assert!(h.saved_at.as_ref().unwrap().contains('T'));
    }

    #[test]
    fn test_history_serde_roundtrip() {
        let h = CacheTuningHistory {
            initial_ttl: 1800,
            current_ttl: 2700,
            enabled: true,
            adjustment_count: 2,
            disable_count: 0,
            decisions: vec![
                CacheTuningDecision::adjust_ttl(1800, 2700, 0.33, "延长"),
                CacheTuningDecision::keep_current(2700, 0.0, "正常"),
            ],
            saved_at: Some("2024-01-01T00:00:00Z".to_string()),
        };

        let json = serde_json::to_string_pretty(&h).unwrap();
        let loaded: CacheTuningHistory = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.initial_ttl, 1800);
        assert_eq!(loaded.current_ttl, 2700);
        assert!(loaded.enabled);
        assert_eq!(loaded.adjustment_count, 2);
        assert_eq!(loaded.decisions.len(), 2);
        assert_eq!(loaded.saved_at, Some("2024-01-01T00:00:00Z".to_string()));
    }

    #[test]
    fn test_history_serde_skip_none_timestamp() {
        let h = CacheTuningHistory {
            initial_ttl: 1800,
            current_ttl: 1800,
            enabled: true,
            adjustment_count: 0,
            disable_count: 0,
            decisions: vec![],
            saved_at: None,
        };

        let json = serde_json::to_string(&h).unwrap();
        // saved_at 为 None 时不应出现在 JSON 中
        assert!(!json.contains("saved_at"));
    }

    #[test]
    fn test_history_serde_empty_decisions() {
        let h = CacheTuningHistory::new();
        let json = serde_json::to_string(&h).unwrap();
        let loaded: CacheTuningHistory = serde_json::from_str(&json).unwrap();
        assert!(loaded.decisions.is_empty());
    }

    // ===== CacheTuningHistory: save / load =====

    #[test]
    fn test_history_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache_tuning_history.json");

        let h = CacheTuningHistory {
            initial_ttl: 1800,
            current_ttl: 2700,
            enabled: true,
            adjustment_count: 1,
            disable_count: 0,
            decisions: vec![CacheTuningDecision::adjust_ttl(1800, 2700, 0.33, "延长")],
            saved_at: None,
        };

        h.save(&path).unwrap();
        assert!(path.exists());

        let loaded = CacheTuningHistory::load(&path).unwrap();
        assert_eq!(loaded.initial_ttl, 1800);
        assert_eq!(loaded.current_ttl, 2700);
        assert!(loaded.enabled);
        assert_eq!(loaded.adjustment_count, 1);
        assert_eq!(loaded.decisions.len(), 1);
    }

    #[test]
    fn test_history_load_nonexistent() {
        let path = std::path::Path::new("/nonexistent/cache_tuning_history.json");
        let result = CacheTuningHistory::load(path);
        assert!(result.is_err());
    }

    #[test]
    fn test_history_load_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("malformed.json");
        std::fs::write(&path, "{invalid json}").unwrap();

        let result = CacheTuningHistory::load(&path);
        assert!(result.is_err());
    }

    #[test]
    fn test_history_save_creates_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("subdir").join("cache_tuning_history.json");

        let h = CacheTuningHistory::new();
        h.save(&path).unwrap();

        assert!(path.exists());
    }

    #[test]
    fn test_history_save_to_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let forge_dir = dir.path().join(".forge");
        std::fs::create_dir_all(&forge_dir).unwrap();

        let h = CacheTuningHistory {
            initial_ttl: 1800,
            current_ttl: 900,
            enabled: false,
            ..CacheTuningHistory::new()
        };

        h.save_to_workspace(dir.path()).unwrap();

        let path = forge_dir.join("cache_tuning_history.json");
        assert!(path.exists());

        let loaded = CacheTuningHistory::load(&path).unwrap();
        assert_eq!(loaded.current_ttl, 900);
        assert!(!loaded.enabled);
    }

    #[test]
    fn test_history_load_from_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let forge_dir = dir.path().join(".forge");
        std::fs::create_dir_all(&forge_dir).unwrap();
        let path = forge_dir.join("cache_tuning_history.json");

        let h = CacheTuningHistory {
            initial_ttl: 1800,
            current_ttl: 2700,
            enabled: true,
            adjustment_count: 2,
            ..CacheTuningHistory::new()
        };
        h.save(&path).unwrap();

        let loaded = CacheTuningHistory::load_from_workspace(dir.path());
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().current_ttl, 2700);
    }

    #[test]
    fn test_history_load_from_workspace_nonexistent() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = CacheTuningHistory::load_from_workspace(dir.path());
        assert!(loaded.is_none());
    }

    // ===== CacheTuner: to_history / from_history =====

    #[test]
    fn test_tuner_to_history_empty() {
        let tuner = CacheTuner::with_default_config(1800);
        let h = tuner.to_history();

        assert_eq!(h.initial_ttl, 1800);
        assert_eq!(h.current_ttl, 1800);
        assert!(h.enabled);
        assert_eq!(h.adjustment_count, 0);
        assert_eq!(h.disable_count, 0);
        assert!(h.decisions.is_empty());
        assert!(h.is_empty());
    }

    #[test]
    fn test_tuner_to_history_after_adjustment() {
        let mut tuner = CacheTuner::with_default_config(1800);

        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(false);
        let stats = CacheStats::new();

        tuner.evaluate_and_apply(&corr, &stats);

        let h = tuner.to_history();
        assert_eq!(h.initial_ttl, 1800);
        assert_eq!(h.current_ttl, 2700); // 1800 × 1.5
        assert!(h.enabled);
        assert_eq!(h.adjustment_count, 1);
        assert_eq!(h.decisions.len(), 1);
        assert!(!h.is_empty());
    }

    #[test]
    fn test_tuner_to_history_after_disable() {
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

        let h = tuner.to_history();
        assert!(!h.enabled);
        assert_eq!(h.disable_count, 1);
        assert_eq!(h.decisions.len(), 1);
    }

    #[test]
    fn test_tuner_to_history_multiple_decisions() {
        let mut tuner = CacheTuner::with_default_config(600);

        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(false);
        let stats = CacheStats::new();

        // 三次延长
        tuner.evaluate_and_apply(&corr, &stats); // 600 → 900
        tuner.evaluate_and_apply(&corr, &stats); // 900 → 1350
        tuner.evaluate_and_apply(&corr, &stats); // 1350 → 2025

        let h = tuner.to_history();
        assert_eq!(h.initial_ttl, 600);
        assert_eq!(h.current_ttl, 2025);
        assert_eq!(h.adjustment_count, 3);
        assert_eq!(h.decisions.len(), 3);
    }

    #[test]
    fn test_tuner_from_history_restores_ttl() {
        let mut history = CacheTuningHistory::new();
        history.current_ttl = 2700;
        history.enabled = true;

        let tuner = CacheTuner::from_history(history, CacheTuningConfig::default());

        // 从历史 TTL 继续
        assert_eq!(tuner.current_ttl(), 2700);
        assert_eq!(tuner.initial_ttl(), 2700); // 新 session 的初始 = 上次的最终
        assert!(tuner.is_enabled());
    }

    #[test]
    fn test_tuner_from_history_restores_disabled() {
        let mut history = CacheTuningHistory::new();
        history.current_ttl = 1800;
        history.enabled = false;

        let tuner = CacheTuner::from_history(history, CacheTuningConfig::default());

        assert!(!tuner.is_enabled());
        assert_eq!(tuner.current_ttl(), 1800);
    }

    #[test]
    fn test_tuner_from_history_resets_counts() {
        let mut history = CacheTuningHistory::new();
        history.current_ttl = 2700;
        history.adjustment_count = 5;
        history.disable_count = 2;
        history.decisions = vec![
            CacheTuningDecision::adjust_ttl(1800, 2700, 0.33, "a"),
            CacheTuningDecision::disable_cache(2700, -0.2, "b"),
        ];

        let tuner = CacheTuner::from_history(history, CacheTuningConfig::default());

        // 新 session, 计数从 0 开始
        assert_eq!(tuner.adjustment_count(), 0);
        assert_eq!(tuner.disable_count(), 0);
        // 决策历史清空
        assert_eq!(tuner.decisions().len(), 0);
        assert!(tuner.last_decision().is_none());
    }

    #[test]
    fn test_tuner_from_history_preserves_config() {
        let history = CacheTuningHistory::new();
        let config = CacheTuningConfig::aggressive();

        let tuner = CacheTuner::from_history(history, config);

        assert_eq!(tuner.config().min_samples, 2); // aggressive
        assert_eq!(tuner.config().disable_threshold, -0.05);
    }

    #[test]
    fn test_tuner_decisions_tracked() {
        let mut tuner = CacheTuner::with_default_config(1800);

        // KeepCurrent
        let corr1 = CacheFixCorrelation::new();
        let stats = CacheStats::new();
        tuner.evaluate_and_apply(&corr1, &stats);
        assert_eq!(tuner.decisions().len(), 1);
        assert!(tuner.decisions()[0].is_keep());

        // AdjustTtl
        let mut corr2 = CacheFixCorrelation::new();
        corr2.record_hit_check(true);
        corr2.record_hit_check(true);
        corr2.record_hit_check(true);
        corr2.record_miss_check(true);
        corr2.record_miss_check(true);
        corr2.record_miss_check(false);
        tuner.evaluate_and_apply(&corr2, &stats);
        assert_eq!(tuner.decisions().len(), 2);
        assert!(tuner.decisions()[1].is_adjust());
    }

    #[test]
    fn test_tuner_initial_ttl_getter() {
        let tuner = CacheTuner::new(CacheTuningConfig::default(), 3600);
        assert_eq!(tuner.initial_ttl(), 3600);
    }

    #[test]
    fn test_tuner_to_summary_includes_initial_ttl() {
        let tuner = CacheTuner::with_default_config(1800);
        let s = tuner.to_summary();
        assert!(s.contains("初始TTL=1800s"));
        assert!(s.contains("当前TTL=1800s"));
    }

    // ===== CacheTuner: save_to_workspace / load_from_workspace =====

    #[test]
    fn test_tuner_save_to_workspace() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();

        let mut tuner = CacheTuner::with_default_config(1800);

        // 执行一次调整
        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(false);
        let stats = CacheStats::new();
        tuner.evaluate_and_apply(&corr, &stats);

        tuner.save_to_workspace(dir.path()).unwrap();

        let path = dir.path().join(".forge").join("cache_tuning_history.json");
        assert!(path.exists());

        // 验证文件内容
        let loaded = CacheTuningHistory::load(&path).unwrap();
        assert_eq!(loaded.initial_ttl, 1800);
        assert_eq!(loaded.current_ttl, 2700);
        assert_eq!(loaded.decisions.len(), 1);
        // save_to_workspace 会添加时间戳
        assert!(loaded.saved_at.is_some());
    }

    #[test]
    fn test_tuner_load_from_workspace_with_history() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();

        // 先保存一个历史
        let mut tuner = CacheTuner::with_default_config(1800);
        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(false);
        let stats = CacheStats::new();
        tuner.evaluate_and_apply(&corr, &stats);
        tuner.save_to_workspace(dir.path()).unwrap();

        // 从历史加载
        let loaded =
            CacheTuner::load_from_workspace(dir.path(), CacheTuningConfig::default(), 1800);

        assert!(loaded.is_some());
        let loaded_tuner = loaded.unwrap();
        assert_eq!(loaded_tuner.current_ttl(), 2700); // 从历史 TTL 继续
        assert_eq!(loaded_tuner.initial_ttl(), 2700); // 新初始 = 历史最终
        assert!(loaded_tuner.is_enabled());
        assert_eq!(loaded_tuner.adjustment_count(), 0); // 新 session, 重新计数
    }

    #[test]
    fn test_tuner_load_from_workspace_without_history() {
        let dir = tempfile::tempdir().unwrap();

        // 无历史文件, 使用默认 TTL
        let loaded =
            CacheTuner::load_from_workspace(dir.path(), CacheTuningConfig::default(), 1800);

        assert!(loaded.is_some());
        let loaded_tuner = loaded.unwrap();
        assert_eq!(loaded_tuner.current_ttl(), 1800);
        assert_eq!(loaded_tuner.initial_ttl(), 1800);
        assert!(loaded_tuner.is_enabled());
    }

    #[test]
    fn test_tuner_load_from_workspace_disabled_history() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();

        // 保存一个被禁用的历史
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
        tuner.save_to_workspace(dir.path()).unwrap();

        // 从历史加载 — 应保留禁用状态
        let loaded =
            CacheTuner::load_from_workspace(dir.path(), CacheTuningConfig::default(), 1800);

        assert!(loaded.is_some());
        let loaded_tuner = loaded.unwrap();
        assert!(!loaded_tuner.is_enabled()); // 禁用状态被保留
        assert_eq!(loaded_tuner.current_ttl(), 1800);
    }

    #[test]
    fn test_tuner_save_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();

        // 创建并调优
        let mut tuner = CacheTuner::with_default_config(600);
        let mut corr = CacheFixCorrelation::new();
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_hit_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(true);
        corr.record_miss_check(false);
        let stats = CacheStats::new();

        // 多次调整
        tuner.evaluate_and_apply(&corr, &stats); // 600 → 900
        tuner.evaluate_and_apply(&corr, &stats); // 900 → 1350

        // 保存
        tuner.save_to_workspace(dir.path()).unwrap();

        // 加载
        let loaded =
            CacheTuner::load_from_workspace(dir.path(), CacheTuningConfig::default(), 600).unwrap();

        // 验证状态恢复
        assert_eq!(loaded.current_ttl(), 1350);
        assert_eq!(loaded.initial_ttl(), 1350); // 新初始 = 历史最终
        assert!(loaded.is_enabled());

        // 新 session 继续调优
        let mut new_tuner = loaded;
        new_tuner.evaluate_and_apply(&corr, &stats); // 1350 → 2025

        assert_eq!(new_tuner.current_ttl(), 2025);
        assert_eq!(new_tuner.adjustment_count(), 1); // 新 session 只调了 1 次
        assert_eq!(new_tuner.decisions().len(), 1);
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
