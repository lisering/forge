//! 三评估器协同决策引擎 — 联合决策是否升级或进入保守模式
//!
//! 当多个评估器 (CacheTuner, SearchQualityEvaluator, MemoryContextEvaluator)
//! 同时判定功能有害时, 联合决策引擎会:
//!
//! - **升级警告** (2+ 评估器禁用) — 提升日志级别, 在报告中标注
//! - **保守模式** (全部评估器禁用) — 跳过所有自动增强行为, 仅保留基础修复循环
//! - **功能重新启用** (保守模式后修复率仍低) — 尝试恢复某个被禁用的功能
//!
//! ## 设计理念
//!
//! 各评估器独立决策可能存在误判 (样本不足、巧合等)。
//! 当多个评估器**一致**判定有害时, 联合决策引擎提供更高级别的判断,
//! 减少误判风险, 同时允许在整体效果持续不佳时采取更激进的策略。
//!
//! ## 核心数据结构
//!
//! - [`JointDecisionAction`] — 联合决策动作类型
//! - [`JointDecisionConfig`] — 决策引擎配置 (阈值)
//! - [`JointDecision`] — 单次联合决策结果
//! - [`JointDecisionEngine`] — 决策引擎 (状态 + 历史)
//! - [`JointDecisionHistory`] — 跨 session 持久化历史
//! - [`JointDecisionHistorySummary`] — 历史摘要 (用于 DevTraceSummary)
//!
//! ## 纯函数
//!
//! - [`count_disabled_evaluators`] — 统计已禁用评估器数量
//! - [`should_enter_conservative_mode`] — 判断是否进入保守模式
//! - [`should_escalate_warning`] — 判断是否升级警告
//! - [`compute_joint_decision`] — 计算联合决策
//! - [`build_joint_decision_history_entry`] — 构建历史条目
//! - [`build_joint_decision_history_summary`] — 构建历史摘要
//!
//! ## 示例
//!
//! ```
//! # use forge::joint_decision::{
//! #     JointDecisionConfig, JointDecisionEngine, compute_joint_decision,
//! # };
//! # use forge::evaluator_synergy::{EvaluatorSnapshot, EvaluatorType};
//! let snapshots = vec![
//!     EvaluatorSnapshot {
//!         evaluator_type: EvaluatorType::CacheTuner,
//!         enabled: false,
//!         with_fix_rate: 0.3,
//!         without_fix_rate: 0.7,
//!         diff: -0.4,
//!         is_beneficial: false,
//!         total_checks: 10,
//!         evaluation_count: 3,
//!         disable_count: 1,
//!         contribution_score: -0.4,
//!     },
//!     EvaluatorSnapshot {
//!         evaluator_type: EvaluatorType::SearchQuality,
//!         enabled: false,
//!         with_fix_rate: 0.2,
//!         without_fix_rate: 0.6,
//!         diff: -0.4,
//!         is_beneficial: false,
//!         total_checks: 10,
//!         evaluation_count: 2,
//!         disable_count: 1,
//!         contribution_score: -0.4,
//!     },
//! ];
//! let config = JointDecisionConfig::default();
//! let decision = compute_joint_decision(&snapshots, &config);
//! assert!(decision.should_escalate);
//! ```

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::evaluator_synergy::{EvaluatorSnapshot, EvaluatorType, ScoreTrend};

// ============================================================================
//  常量
// ============================================================================

/// 联合决策历史文件名
pub const JOINT_DECISION_HISTORY_FILENAME: &str = "joint_decision_history.json";

/// 最大历史 session 数 (超过后自动淘汰最旧的)
pub const MAX_JOINT_DECISION_HISTORY_SESSIONS: usize = 50;

/// 默认保守模式阈值 — 全部评估器禁用时进入保守模式
pub const DEFAULT_CONSERVATIVE_MODE_THRESHOLD: usize = 3;

/// 默认升级警告阈值 — 2+ 评估器禁用时升级警告
pub const DEFAULT_ESCALATE_WARNING_THRESHOLD: usize = 2;

// ============================================================================
//  JointDecisionAction — 联合决策动作类型
// ============================================================================

/// 联合决策动作 — 引擎根据评估器状态做出的决策
///
/// # 变体
///
/// - `NoAction` — 无需联合行动, 各评估器独立运行
/// - `EscalateWarning` — 2+ 评估器禁用, 升级警告级别
/// - `EnterConservativeMode` — 全部评估器禁用, 进入保守模式
/// - `ReEnableFeature` — 尝试重新启用某个被禁用的评估器功能
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum JointDecisionAction {
    /// 无需联合行动
    #[default]
    NoAction,
    /// 升级警告 — 2+ 评估器禁用但未全部禁用
    EscalateWarning,
    /// 进入保守模式 — 全部评估器禁用
    EnterConservativeMode,
    /// 重新启用功能 — 保守模式后尝试恢复
    ReEnableFeature {
        /// 建议重新启用的评估器类型
        evaluator_type: EvaluatorType,
    },
}

impl std::fmt::Display for JointDecisionAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoAction => write!(f, "NoAction"),
            Self::EscalateWarning => write!(f, "EscalateWarning"),
            Self::EnterConservativeMode => write!(f, "EnterConservativeMode"),
            Self::ReEnableFeature { evaluator_type } => {
                write!(f, "ReEnableFeature({})", evaluator_type)
            }
        }
    }
}

impl JointDecisionAction {
    /// 获取动作的中文描述
    pub fn label(&self) -> &'static str {
        match self {
            Self::NoAction => "无联合行动",
            Self::EscalateWarning => "升级警告",
            Self::EnterConservativeMode => "进入保守模式",
            Self::ReEnableFeature { .. } => "重新启用功能",
        }
    }

    /// 是否为保守模式
    pub fn is_conservative(&self) -> bool {
        matches!(self, Self::EnterConservativeMode)
    }

    /// 是否需要升级
    pub fn is_escalation(&self) -> bool {
        matches!(self, Self::EscalateWarning | Self::EnterConservativeMode)
    }
}

// ============================================================================
//  JointDecisionConfig — 决策引擎配置
// ============================================================================

/// 联合决策引擎配置 — 控制升级和保守模式的阈值
///
/// # 字段
///
/// - `conservative_mode_threshold`: 进入保守模式所需的禁用评估器数量
/// - `escalate_warning_threshold`: 升级警告所需的禁用评估器数量
/// - `total_evaluators`: 评估器总数 (默认 3)
/// - `re_enable_after_rounds`: 保守模式后多少轮尝试重新启用 (默认 5)
///
/// # 示例
///
/// ```
/// # use forge::joint_decision::JointDecisionConfig;
/// let config = JointDecisionConfig::default();
/// assert_eq!(config.conservative_mode_threshold, 3);
/// assert_eq!(config.escalate_warning_threshold, 2);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JointDecisionConfig {
    /// 进入保守模式所需的禁用评估器数量
    pub conservative_mode_threshold: usize,
    /// 升级警告所需的禁用评估器数量
    pub escalate_warning_threshold: usize,
    /// 评估器总数
    pub total_evaluators: usize,
    /// 保守模式后多少轮尝试重新启用
    pub re_enable_after_rounds: usize,
}

impl Default for JointDecisionConfig {
    fn default() -> Self {
        Self {
            conservative_mode_threshold: DEFAULT_CONSERVATIVE_MODE_THRESHOLD,
            escalate_warning_threshold: DEFAULT_ESCALATE_WARNING_THRESHOLD,
            total_evaluators: 3,
            re_enable_after_rounds: 5,
        }
    }
}

impl JointDecisionConfig {
    /// 创建自定义配置
    ///
    /// # 参数
    ///
    /// - `conservative_threshold`: 保守模式阈值
    /// - `escalate_threshold`: 升级警告阈值
    pub fn new(conservative_threshold: usize, escalate_threshold: usize) -> Self {
        Self {
            conservative_mode_threshold: conservative_threshold,
            escalate_warning_threshold: escalate_threshold,
            total_evaluators: 3,
            re_enable_after_rounds: 5,
        }
    }

    /// 设置保守模式阈值 (builder)
    pub fn with_conservative_threshold(mut self, threshold: usize) -> Self {
        self.conservative_mode_threshold = threshold;
        self
    }

    /// 设置升级警告阈值 (builder)
    pub fn with_escalate_threshold(mut self, threshold: usize) -> Self {
        self.escalate_warning_threshold = threshold;
        self
    }

    /// 设置评估器总数 (builder)
    pub fn with_total_evaluators(mut self, total: usize) -> Self {
        self.total_evaluators = total;
        self
    }

    /// 设置重新启用轮次 (builder)
    pub fn with_re_enable_after_rounds(mut self, rounds: usize) -> Self {
        self.re_enable_after_rounds = rounds;
        self
    }
}

// ============================================================================
//  JointDecision — 单次联合决策结果
// ============================================================================

/// 联合决策结果 — 引擎根据评估器状态做出的决策
///
/// # 字段
///
/// - `action`: 决策动作
/// - `reason`: 决策原因
/// - `disabled_count`: 已禁用评估器数量
/// - `total_evaluators`: 评估器总数
/// - `disabled_evaluators`: 被禁用的评估器类型列表
/// - `should_escalate`: 是否升级
/// - `conservative_mode`: 是否进入保守模式
/// - `timestamp`: 决策时间戳
///
/// # 示例
///
/// ```
/// # use forge::joint_decision::{JointDecision, JointDecisionAction};
/// let decision = JointDecision::new(
///     JointDecisionAction::EscalateWarning,
///     "2/3 评估器禁用".to_string(),
///     2, 3,
///     vec![],
/// );
/// assert!(decision.should_escalate);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JointDecision {
    /// 决策动作
    pub action: JointDecisionAction,
    /// 决策原因
    pub reason: String,
    /// 已禁用评估器数量
    pub disabled_count: usize,
    /// 评估器总数
    pub total_evaluators: usize,
    /// 被禁用的评估器类型列表
    pub disabled_evaluators: Vec<EvaluatorType>,
    /// 是否升级
    pub should_escalate: bool,
    /// 是否进入保守模式
    pub conservative_mode: bool,
    /// 决策时间戳 (UTC)
    pub timestamp: DateTime<Utc>,
}

impl JointDecision {
    /// 创建联合决策
    ///
    /// # 参数
    ///
    /// - `action`: 决策动作
    /// - `reason`: 决策原因
    /// - `disabled_count`: 已禁用评估器数量
    /// - `total_evaluators`: 评估器总数
    /// - `disabled_evaluators`: 被禁用的评估器类型列表
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        action: JointDecisionAction,
        reason: String,
        disabled_count: usize,
        total_evaluators: usize,
        disabled_evaluators: Vec<EvaluatorType>,
    ) -> Self {
        let should_escalate = action.is_escalation();
        let conservative_mode = action.is_conservative();
        Self {
            action,
            reason,
            disabled_count,
            total_evaluators,
            disabled_evaluators,
            should_escalate,
            conservative_mode,
            timestamp: Utc::now(),
        }
    }

    /// 获取决策摘要文本 (用于 DevTrace 输出)
    pub fn to_trace_summary(&self) -> String {
        format!(
            "联合决策: {} (禁用 {}/{}, {})",
            self.action.label(),
            self.disabled_count,
            self.total_evaluators,
            self.reason,
        )
    }

    /// 获取决策简要描述
    pub fn to_summary(&self) -> String {
        format!(
            "{} — 禁用 {}/{}",
            self.action.label(),
            self.disabled_count,
            self.total_evaluators,
        )
    }

    /// 创建无行动决策
    pub fn no_action(total_evaluators: usize) -> Self {
        Self::new(
            JointDecisionAction::NoAction,
            "各评估器独立运行, 无需联合行动".to_string(),
            0,
            total_evaluators,
            vec![],
        )
    }

    /// 是否有行动
    pub fn has_action(&self) -> bool {
        self.action != JointDecisionAction::NoAction
    }
}

// ============================================================================
//  纯函数 — 决策逻辑
// ============================================================================

/// 统计已禁用的评估器数量
///
/// 遍历评估器快照列表, 统计 `enabled == false` 的数量。
///
/// # 参数
///
/// - `snapshots`: 评估器快照列表
///
/// # 返回
///
/// 已禁用的评估器数量
///
/// # 示例
///
/// ```
/// # use forge::joint_decision::count_disabled_evaluators;
/// # use forge::evaluator_synergy::{EvaluatorSnapshot, EvaluatorType};
/// let snapshots = vec![
///     EvaluatorSnapshot {
///         evaluator_type: EvaluatorType::CacheTuner,
///         enabled: true,
///         with_fix_rate: 0.8,
///         without_fix_rate: 0.6,
///         diff: 0.2,
///         is_beneficial: true,
///         total_checks: 10,
///         evaluation_count: 3,
///         disable_count: 0,
///         contribution_score: 0.2,
///     },
///     EvaluatorSnapshot {
///         evaluator_type: EvaluatorType::SearchQuality,
///         enabled: false,
///         with_fix_rate: 0.3,
///         without_fix_rate: 0.7,
///         diff: -0.4,
///         is_beneficial: false,
///         total_checks: 10,
///         evaluation_count: 2,
///         disable_count: 1,
///         contribution_score: -0.4,
///     },
/// ];
/// assert_eq!(count_disabled_evaluators(&snapshots), 1);
/// ```
pub fn count_disabled_evaluators(snapshots: &[EvaluatorSnapshot]) -> usize {
    snapshots.iter().filter(|s| !s.enabled).count()
}

/// 获取被禁用的评估器类型列表
///
/// # 参数
///
/// - `snapshots`: 评估器快照列表
///
/// # 返回
///
/// 被禁用的评估器类型列表
pub fn get_disabled_evaluator_types(snapshots: &[EvaluatorSnapshot]) -> Vec<EvaluatorType> {
    snapshots
        .iter()
        .filter(|s| !s.enabled)
        .map(|s| s.evaluator_type)
        .collect()
}

/// 判断是否应该进入保守模式
///
/// 当禁用评估器数量达到或超过 `threshold` 时返回 `true`。
///
/// # 参数
///
/// - `disabled_count`: 已禁用评估器数量
/// - `total`: 评估器总数
/// - `threshold`: 保守模式阈值
///
/// # 返回
///
/// 是否进入保守模式
///
/// # 示例
///
/// ```
/// # use forge::joint_decision::should_enter_conservative_mode;
/// assert!(!should_enter_conservative_mode(2, 3, 3));
/// assert!(should_enter_conservative_mode(3, 3, 3));
/// assert!(should_enter_conservative_mode(3, 3, 2)); // 阈值更低
/// ```
pub fn should_enter_conservative_mode(
    disabled_count: usize,
    total: usize,
    threshold: usize,
) -> bool {
    if total == 0 {
        return false;
    }
    disabled_count >= threshold.min(total)
}

/// 判断是否应该升级警告
///
/// 当禁用评估器数量达到或超过 `threshold` 但未达到保守模式阈值时返回 `true`。
///
/// # 参数
///
/// - `disabled_count`: 已禁用评估器数量
/// - `total`: 评估器总数
/// - `escalate_threshold`: 升级警告阈值
/// - `conservative_threshold`: 保守模式阈值
///
/// # 返回
///
/// 是否升级警告 (但未进入保守模式)
///
/// # 示例
///
/// ```
/// # use forge::joint_decision::should_escalate_warning;
/// assert!(!should_escalate_warning(1, 3, 2, 3));
/// assert!(should_escalate_warning(2, 3, 2, 3));
/// assert!(!should_escalate_warning(3, 3, 2, 3)); // 进入保守模式而非升级
/// ```
pub fn should_escalate_warning(
    disabled_count: usize,
    total: usize,
    escalate_threshold: usize,
    conservative_threshold: usize,
) -> bool {
    if total == 0 {
        return false;
    }
    let effective_escalate = escalate_threshold.min(total);
    let effective_conservative = conservative_threshold.min(total);
    disabled_count >= effective_escalate && disabled_count < effective_conservative
}

/// 选择建议重新启用的评估器
///
/// 在保守模式下, 选择贡献度最高的被禁用评估器尝试重新启用。
/// 贡献度 = `diff` (with - without 修复率差值), 差值越大说明功能越有效。
///
/// # 参数
///
/// - `snapshots`: 评估器快照列表
///
/// # 返回
///
/// `Some(EvaluatorType)` — 建议重新启用的评估器, `None` — 无可用候选
///
/// # 示例
///
/// ```
/// # use forge::joint_decision::select_re_enable_candidate;
/// # use forge::evaluator_synergy::{EvaluatorSnapshot, EvaluatorType};
/// let snapshots = vec![
///     EvaluatorSnapshot {
///         evaluator_type: EvaluatorType::CacheTuner,
///         enabled: false,
///         with_fix_rate: 0.5,
///         without_fix_rate: 0.4,
///         diff: 0.1,
///         is_beneficial: true,
///         total_checks: 10,
///         evaluation_count: 3,
///         disable_count: 1,
///         contribution_score: 0.1,
///     },
///     EvaluatorSnapshot {
///         evaluator_type: EvaluatorType::SearchQuality,
///         enabled: false,
///         with_fix_rate: 0.3,
///         without_fix_rate: 0.7,
///         diff: -0.4,
///         is_beneficial: false,
///         total_checks: 10,
///         evaluation_count: 2,
///         disable_count: 1,
///         contribution_score: -0.4,
///     },
/// ];
/// // CacheTuner diff=0.1 > SearchQuality diff=-0.4 → 选择 CacheTuner
/// let candidate = select_re_enable_candidate(&snapshots);
/// assert_eq!(candidate, Some(EvaluatorType::CacheTuner));
/// ```
pub fn select_re_enable_candidate(snapshots: &[EvaluatorSnapshot]) -> Option<EvaluatorType> {
    snapshots
        .iter()
        .filter(|s| !s.enabled)
        .max_by(|a, b| {
            a.diff
                .partial_cmp(&b.diff)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|s| s.evaluator_type)
}

/// 计算联合决策 — 核心纯函数
///
/// 根据评估器快照列表和配置, 计算联合决策。
///
/// # 决策规则
///
/// 1. 禁用数 >= `conservative_mode_threshold` → `EnterConservativeMode`
/// 2. 禁用数 >= `escalate_warning_threshold` → `EscalateWarning`
/// 3. 否则 → `NoAction`
///
/// # 参数
///
/// - `snapshots`: 评估器快照列表
/// - `config`: 决策配置
///
/// # 返回
///
/// 联合决策结果
///
/// # 示例
///
/// ```
/// # use forge::joint_decision::{compute_joint_decision, JointDecisionConfig};
/// # use forge::evaluator_synergy::{EvaluatorSnapshot, EvaluatorType};
/// // 全部启用 → NoAction
/// let snapshots = vec![
///     EvaluatorSnapshot {
///         evaluator_type: EvaluatorType::CacheTuner,
///         enabled: true,
///         with_fix_rate: 0.8, without_fix_rate: 0.6,
///         diff: 0.2, is_beneficial: true,
///         total_checks: 10, evaluation_count: 3,
///         disable_count: 0, contribution_score: 0.2,
///     },
/// ];
/// let config = JointDecisionConfig::default();
/// let decision = compute_joint_decision(&snapshots, &config);
/// assert!(!decision.should_escalate);
/// ```
pub fn compute_joint_decision(
    snapshots: &[EvaluatorSnapshot],
    config: &JointDecisionConfig,
) -> JointDecision {
    let disabled_count = count_disabled_evaluators(snapshots);
    let total = snapshots.len().max(config.total_evaluators);
    let disabled_evaluators = get_disabled_evaluator_types(snapshots);

    if should_enter_conservative_mode(disabled_count, total, config.conservative_mode_threshold) {
        let reason = format!(
            "{}/{} 评估器已禁用, 进入保守模式 (跳过所有自动增强)",
            disabled_count, total,
        );
        JointDecision::new(
            JointDecisionAction::EnterConservativeMode,
            reason,
            disabled_count,
            total,
            disabled_evaluators,
        )
    } else if should_escalate_warning(
        disabled_count,
        total,
        config.escalate_warning_threshold,
        config.conservative_mode_threshold,
    ) {
        let disabled_names: Vec<String> = disabled_evaluators
            .iter()
            .map(|t| t.label().to_string())
            .collect();
        let reason = format!(
            "{}/{} 评估器已禁用 ({}), 升级警告",
            disabled_count,
            total,
            disabled_names.join(", "),
        );
        JointDecision::new(
            JointDecisionAction::EscalateWarning,
            reason,
            disabled_count,
            total,
            disabled_evaluators,
        )
    } else {
        JointDecision::no_action(total)
    }
}

// ============================================================================
//  JointDecisionEngine — 决策引擎
// ============================================================================

/// 联合决策引擎 — 在每次编译检查后评估三评估器状态, 做出联合决策
///
/// 引擎维护当前是否处于保守模式, 以及决策历史。
/// 在保守模式下, 引擎会在 `re_enable_after_rounds` 轮后
/// 尝试建议重新启用某个被禁用的功能。
///
/// # 字段
///
/// - `config`: 决策配置
/// - `conservative_mode`: 当前是否处于保守模式
/// - `conservative_rounds`: 保守模式持续的轮次数
/// - `decisions`: 本 session 的决策列表
/// - `history`: 跨 session 持久化历史
///
/// # 示例
///
/// ```
/// # use forge::joint_decision::{JointDecisionEngine, JointDecisionConfig};
/// let engine = JointDecisionEngine::new(JointDecisionConfig::default());
/// assert!(!engine.is_conservative_mode());
/// assert!(engine.decisions().is_empty());
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JointDecisionEngine {
    /// 决策配置
    pub config: JointDecisionConfig,
    /// 当前是否处于保守模式
    pub conservative_mode: bool,
    /// 保守模式持续的轮次数
    pub conservative_rounds: usize,
    /// 本 session 的决策列表
    pub decisions: Vec<JointDecision>,
    /// 跨 session 持久化历史
    #[serde(default)]
    pub history: JointDecisionHistory,
}

impl Default for JointDecisionEngine {
    fn default() -> Self {
        Self::new(JointDecisionConfig::default())
    }
}

impl JointDecisionEngine {
    /// 创建决策引擎
    ///
    /// # 参数
    ///
    /// - `config`: 决策配置
    pub fn new(config: JointDecisionConfig) -> Self {
        Self {
            config,
            conservative_mode: false,
            conservative_rounds: 0,
            decisions: Vec::new(),
            history: JointDecisionHistory::new(),
        }
    }

    /// 是否处于保守模式
    pub fn is_conservative_mode(&self) -> bool {
        self.conservative_mode
    }

    /// 获取本 session 的决策列表
    pub fn decisions(&self) -> &[JointDecision] {
        &self.decisions
    }

    /// 获取决策数量
    pub fn decision_count(&self) -> usize {
        self.decisions.len()
    }

    /// 获取升级警告次数
    pub fn escalate_count(&self) -> usize {
        self.decisions
            .iter()
            .filter(|d| d.action == JointDecisionAction::EscalateWarning)
            .count()
    }

    /// 获取保守模式激活次数
    pub fn conservative_mode_count(&self) -> usize {
        self.decisions
            .iter()
            .filter(|d| d.action == JointDecisionAction::EnterConservativeMode)
            .count()
    }

    /// 获取最新决策
    pub fn latest_decision(&self) -> Option<&JointDecision> {
        self.decisions.last()
    }

    /// 是否有决策记录
    pub fn has_decisions(&self) -> bool {
        !self.decisions.is_empty()
    }

    /// 评估并应用联合决策
    ///
    /// 在每次编译检查后调用, 根据评估器快照计算联合决策。
    ///
    /// # 参数
    ///
    /// - `snapshots`: 评估器快照列表
    ///
    /// # 返回
    ///
    /// 联合决策结果
    pub fn evaluate(&mut self, snapshots: &[EvaluatorSnapshot]) -> JointDecision {
        let mut decision = compute_joint_decision(snapshots, &self.config);

        // 记录本轮评估前是否已处于保守模式
        let was_in_conservative = self.conservative_mode;

        // 更新保守模式状态
        if decision.action.is_conservative() {
            self.conservative_mode = true;
            self.conservative_rounds += 1;
        } else if self.conservative_mode {
            // 已在保守模式中但当前决策非保守 (部分评估器可能已恢复)
            self.conservative_rounds += 1;
        }

        // 仅在进入保守模式之前的轮次就已在保守模式中时, 才检查是否应重新启用
        // 这确保第一次进入保守模式的那一轮不会立即触发重新启用
        if was_in_conservative
            && self.conservative_mode
            && self.conservative_rounds >= self.config.re_enable_after_rounds
        {
            if let Some(candidate) = select_re_enable_candidate(snapshots) {
                decision = JointDecision::new(
                    JointDecisionAction::ReEnableFeature {
                        evaluator_type: candidate,
                    },
                    format!(
                        "保守模式 {} 轮后, 尝试重新启用 {}",
                        self.conservative_rounds,
                        candidate.label()
                    ),
                    decision.disabled_count,
                    decision.total_evaluators,
                    decision.disabled_evaluators.clone(),
                );
                // 退出保守模式
                self.conservative_mode = false;
                self.conservative_rounds = 0;
            }
        }

        self.decisions.push(decision.clone());
        decision
    }

    /// 设置时间戳并构建历史条目
    ///
    /// 在 session 结束时调用, 将当前 session 的决策摘要追加到历史。
    ///
    /// # 参数
    ///
    /// - `timestamp`: session 结束时间
    pub fn finalize_session(&mut self, timestamp: DateTime<Utc>) {
        let entry = build_joint_decision_history_entry(self, timestamp);
        self.history.add_entry(entry);
    }

    /// 设置历史保存时间戳 (builder)
    pub fn with_timestamp(mut self, timestamp: DateTime<Utc>) -> Self {
        self.history = self.history.with_timestamp(timestamp);
        self
    }

    /// 获取历史摘要
    pub fn to_history_summary(&self) -> JointDecisionHistorySummary {
        build_joint_decision_history_summary(&self.history)
    }

    // --- 持久化方法 ---

    /// 从工作区加载历史
    ///
    /// # 参数
    ///
    /// - `workspace_root`: 工作区根目录路径
    pub fn load_history_from_workspace(&mut self, workspace_root: &std::path::Path) {
        if let Some(history) = JointDecisionHistory::load_from_workspace(workspace_root) {
            self.history = history;
        }
    }

    /// 保存历史到工作区
    ///
    /// # 参数
    ///
    /// - `workspace_root`: 工作区根目录路径
    pub fn save_history_to_workspace(&self, workspace_root: &std::path::Path) -> Result<()> {
        self.history.save_to_workspace(workspace_root)
    }
}

// ============================================================================
//  JointDecisionHistoryEntry — 历史条目
// ============================================================================

/// 联合决策历史条目 — 单个 session 的决策摘要
///
/// # 字段
///
/// - `session_index`: session 序号 (从 1 开始)
/// - `timestamp`: session 结束时间
/// - `action`: 最终决策动作
/// - `decision_count`: 本 session 决策总数
/// - `escalate_count`: 升级警告次数
/// - `conservative_mode_count`: 保守模式激活次数
/// - `final_conservative_mode`: session 结束时是否处于保守模式
/// - `disabled_evaluators`: 被禁用的评估器类型列表
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JointDecisionHistoryEntry {
    /// session 序号 (从 1 开始)
    pub session_index: usize,
    /// session 结束时间 (UTC)
    pub timestamp: DateTime<Utc>,
    /// 最终决策动作 (本 session 最后一次)
    pub action: JointDecisionAction,
    /// 本 session 决策总数
    pub decision_count: usize,
    /// 升级警告次数
    pub escalate_count: usize,
    /// 保守模式激活次数
    pub conservative_mode_count: usize,
    /// session 结束时是否处于保守模式
    pub final_conservative_mode: bool,
    /// 被禁用的评估器类型列表
    pub disabled_evaluators: Vec<EvaluatorType>,
}

impl JointDecisionHistoryEntry {
    /// 创建历史条目
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_index: usize,
        timestamp: DateTime<Utc>,
        action: JointDecisionAction,
        decision_count: usize,
        escalate_count: usize,
        conservative_mode_count: usize,
        final_conservative_mode: bool,
        disabled_evaluators: Vec<EvaluatorType>,
    ) -> Self {
        Self {
            session_index,
            timestamp,
            action,
            decision_count,
            escalate_count,
            conservative_mode_count,
            final_conservative_mode,
            disabled_evaluators,
        }
    }

    /// 格式化为简要摘要
    pub fn to_summary(&self) -> String {
        format!(
            "Session {}: {} 决策, {} 升级, {} 保守模式, 最终 {}",
            self.session_index,
            self.decision_count,
            self.escalate_count,
            self.conservative_mode_count,
            self.action.label(),
        )
    }
}

// ============================================================================
//  JointDecisionHistory — 跨 session 持久化历史
// ============================================================================

/// 跨 session 联合决策历史 — 追踪决策变化趋势
///
/// 每次 session 结束时将 `JointDecisionEngine` 的关键指标
/// 追加到历史记录中, 持久化到 `.forge/joint_decision_history.json`。
///
/// # 字段
///
/// - `sessions`: 各 session 的决策摘要列表 (按时间顺序)
/// - `saved_at`: 最后保存时间
///
/// # 示例
///
/// ```
/// # use forge::joint_decision::{
/// #     JointDecisionHistory, JointDecisionHistoryEntry, JointDecisionAction,
/// # };
/// # use chrono::Utc;
/// let mut history = JointDecisionHistory::new();
/// assert!(history.is_empty());
///
/// let entry = JointDecisionHistoryEntry::new(
///     1, Utc::now(), JointDecisionAction::NoAction,
///     5, 1, 0, false, vec![],
/// );
/// history.add_entry(entry);
/// assert_eq!(history.session_count(), 1);
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JointDecisionHistory {
    /// 各 session 的决策摘要列表 (按时间顺序)
    pub sessions: Vec<JointDecisionHistoryEntry>,
    /// 最后保存时间 (ISO 8601)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved_at: Option<String>,
}

impl JointDecisionHistory {
    /// 创建空历史
    pub fn new() -> Self {
        Self {
            sessions: Vec::new(),
            saved_at: None,
        }
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// session 数量
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// 下一个 session 序号
    pub fn next_session_index(&self) -> usize {
        self.sessions.len() + 1
    }

    /// 添加历史条目 (自动淘汰超过上限的旧记录)
    pub fn add_entry(&mut self, entry: JointDecisionHistoryEntry) {
        self.sessions.push(entry);
        if self.sessions.len() > MAX_JOINT_DECISION_HISTORY_SESSIONS {
            self.sessions.remove(0);
        }
    }

    /// 获取最新条目
    pub fn latest(&self) -> Option<&JointDecisionHistoryEntry> {
        self.sessions.last()
    }

    /// 累计升级警告次数
    pub fn total_escalations(&self) -> usize {
        self.sessions.iter().map(|e| e.escalate_count).sum()
    }

    /// 累计保守模式次数
    pub fn total_conservative_modes(&self) -> usize {
        self.sessions
            .iter()
            .map(|e| e.conservative_mode_count)
            .sum()
    }

    /// 累计决策总数
    pub fn total_decisions(&self) -> usize {
        self.sessions.iter().map(|e| e.decision_count).sum()
    }

    /// 保守模式趋势 — 最近 N 个 session 中保守模式占比
    pub fn conservative_mode_rate(&self) -> f64 {
        if self.sessions.is_empty() {
            return 0.0;
        }
        let count = self
            .sessions
            .iter()
            .filter(|e| e.final_conservative_mode)
            .count();
        count as f64 / self.sessions.len() as f64
    }

    /// 决策趋势 — 升级+保守 vs 无行动
    pub fn escalation_rate(&self) -> f64 {
        if self.sessions.is_empty() {
            return 0.0;
        }
        let count = self
            .sessions
            .iter()
            .filter(|e| e.escalate_count > 0 || e.conservative_mode_count > 0)
            .count();
        count as f64 / self.sessions.len() as f64
    }

    /// 设置保存时间戳
    pub fn with_timestamp(mut self, timestamp: DateTime<Utc>) -> Self {
        self.saved_at = Some(timestamp.to_rfc3339());
        self
    }

    /// 格式化为简要摘要
    pub fn to_summary(&self) -> String {
        if self.is_empty() {
            return "联合决策历史: 无记录".to_string();
        }
        format!(
            "联合决策历史: {} 个 session, {} 决策, {} 升级, {} 保守模式",
            self.session_count(),
            self.total_decisions(),
            self.total_escalations(),
            self.total_conservative_modes(),
        )
    }

    // --- 持久化方法 ---

    /// 从 JSON 文件加载历史
    pub fn load(path: &std::path::Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::new());
        }
        let content = std::fs::read_to_string(path)?;
        if content.trim().is_empty() {
            return Ok(Self::new());
        }
        let history: Self = serde_json::from_str(&content)?;
        Ok(history)
    }

    /// 保存历史到 JSON 文件
    pub fn save(&self, path: &std::path::Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// 从工作区加载历史
    pub fn load_from_workspace(workspace_root: &std::path::Path) -> Option<Self> {
        let path = workspace_root
            .join(".forge")
            .join(JOINT_DECISION_HISTORY_FILENAME);
        match Self::load(&path) {
            Ok(h) if !h.is_empty() => Some(h),
            Ok(_) => None,
            Err(_) => None,
        }
    }

    /// 保存历史到工作区
    pub fn save_to_workspace(&self, workspace_root: &std::path::Path) -> Result<()> {
        let path = workspace_root
            .join(".forge")
            .join(JOINT_DECISION_HISTORY_FILENAME);
        self.save(&path)
    }
}

// ============================================================================
//  JointDecisionHistorySummary — 历史摘要 (用于 DevTraceSummary)
// ============================================================================

/// 联合决策历史摘要 — 用于 DevTraceSummary 面板展示
///
/// 从 `JointDecisionHistory` 提取关键趋势信息,
/// 展示联合决策的跨 session 变化趋势。
///
/// # 字段
///
/// - `session_count`: session 数量
/// - `latest_action`: 最新 session 的最终决策动作
/// - `total_decisions`: 累计决策总数
/// - `total_escalations`: 累计升级警告次数
/// - `total_conservative_modes`: 累计保守模式次数
/// - `conservative_mode_rate`: 保守模式占比 (0.0~1.0)
/// - `escalation_rate`: 升级率 (0.0~1.0)
/// - `current_conservative_mode`: 当前是否处于保守模式
/// - `saved_at`: 最后保存时间
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JointDecisionHistorySummary {
    /// session 数量
    pub session_count: usize,
    /// 最新 session 的最终决策动作
    pub latest_action: JointDecisionAction,
    /// 累计决策总数
    pub total_decisions: usize,
    /// 累计升级警告次数
    pub total_escalations: usize,
    /// 累计保守模式次数
    pub total_conservative_modes: usize,
    /// 保守模式占比 (0.0~1.0)
    pub conservative_mode_rate: f64,
    /// 升级率 (0.0~1.0)
    pub escalation_rate: f64,
    /// 当前是否处于保守模式
    pub current_conservative_mode: bool,
    /// 最后保存时间 (ISO 8601)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved_at: Option<String>,
}

impl Default for JointDecisionHistorySummary {
    fn default() -> Self {
        Self::empty()
    }
}

impl JointDecisionHistorySummary {
    /// 创建摘要
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_count: usize,
        latest_action: JointDecisionAction,
        total_decisions: usize,
        total_escalations: usize,
        total_conservative_modes: usize,
        conservative_mode_rate: f64,
        escalation_rate: f64,
        current_conservative_mode: bool,
        saved_at: Option<String>,
    ) -> Self {
        Self {
            session_count,
            latest_action,
            total_decisions,
            total_escalations,
            total_conservative_modes,
            conservative_mode_rate,
            escalation_rate,
            current_conservative_mode,
            saved_at,
        }
    }

    /// 创建空摘要
    pub fn empty() -> Self {
        Self::new(
            0,
            JointDecisionAction::NoAction,
            0,
            0,
            0,
            0.0,
            0.0,
            false,
            None,
        )
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.session_count == 0
    }

    /// 格式化为报告文本
    pub fn to_report_section(&self) -> String {
        if self.is_empty() {
            return String::new();
        }

        let mut report = String::new();
        report.push_str("=== 三评估器协同决策 (跨 Session 历史) ===\n");
        report.push_str(&format!("  Session 数: {}\n", self.session_count));
        report.push_str(&format!("  最新决策: {}\n", self.latest_action.label()));
        report.push_str(&format!("  累计决策: {} 次\n", self.total_decisions));
        report.push_str(&format!("  累计升级警告: {} 次\n", self.total_escalations));
        report.push_str(&format!(
            "  累计保守模式: {} 次\n",
            self.total_conservative_modes
        ));
        report.push_str(&format!(
            "  保守模式占比: {:.1}%\n",
            self.conservative_mode_rate * 100.0
        ));
        report.push_str(&format!("  升级率: {:.1}%\n", self.escalation_rate * 100.0));
        if self.current_conservative_mode {
            report.push_str("  当前模式: 🔒 保守模式\n");
        } else {
            report.push_str("  当前模式: ✅ 正常模式\n");
        }
        if let Some(ref saved_at) = self.saved_at {
            report.push_str(&format!("  保存时间: {}\n", saved_at));
        }

        report
    }
}

// ============================================================================
//  纯函数 — 历史构建
// ============================================================================

/// 从 JointDecisionEngine 构建历史条目
///
/// # 参数
///
/// - `engine`: 联合决策引擎
/// - `timestamp`: session 结束时间
///
/// # 返回
///
/// 历史条目
pub fn build_joint_decision_history_entry(
    engine: &JointDecisionEngine,
    timestamp: DateTime<Utc>,
) -> JointDecisionHistoryEntry {
    let session_index = engine.history.next_session_index();
    let latest_action = engine
        .latest_decision()
        .map(|d| d.action.clone())
        .unwrap_or_default();
    let disabled_evaluators = engine
        .latest_decision()
        .map(|d| d.disabled_evaluators.clone())
        .unwrap_or_default();

    JointDecisionHistoryEntry::new(
        session_index,
        timestamp,
        latest_action,
        engine.decision_count(),
        engine.escalate_count(),
        engine.conservative_mode_count(),
        engine.is_conservative_mode(),
        disabled_evaluators,
    )
}

/// 从 JointDecisionHistory 构建历史摘要
///
/// # 参数
///
/// - `history`: 联合决策历史
///
/// # 返回
///
/// 历史摘要
pub fn build_joint_decision_history_summary(
    history: &JointDecisionHistory,
) -> JointDecisionHistorySummary {
    if history.is_empty() {
        return JointDecisionHistorySummary::empty();
    }

    let latest = history.latest();
    let latest_action = latest
        .map(|e| e.action.clone())
        .unwrap_or(JointDecisionAction::NoAction);
    let current_conservative = latest.map(|e| e.final_conservative_mode).unwrap_or(false);

    JointDecisionHistorySummary::new(
        history.session_count(),
        latest_action,
        history.total_decisions(),
        history.total_escalations(),
        history.total_conservative_modes(),
        history.conservative_mode_rate(),
        history.escalation_rate(),
        current_conservative,
        history.saved_at.clone(),
    )
}

/// 解析联合决策动作的趋势
///
/// 从历史中分析决策趋势: 升级/保守模式是否在增加。
///
/// # 参数
///
/// - `history`: 联合决策历史
///
/// # 返回
///
/// `ScoreTrend` — Improving (升级减少), Declining (升级增加), Stable, Insufficient
pub fn compute_decision_trend(history: &JointDecisionHistory) -> ScoreTrend {
    if history.sessions.len() < 2 {
        return ScoreTrend::Insufficient;
    }

    let midpoint = history.sessions.len() / 2;
    let early: usize = history.sessions[..midpoint]
        .iter()
        .map(|e| e.escalate_count + e.conservative_mode_count)
        .sum();
    let late: usize = history.sessions[midpoint..]
        .iter()
        .map(|e| e.escalate_count + e.conservative_mode_count)
        .sum();

    let early_avg = early as f64 / midpoint as f64;
    let late_avg = late as f64 / (history.sessions.len() - midpoint) as f64;

    let threshold = 0.5; // 平均差值阈值
    if late_avg < early_avg - threshold {
        ScoreTrend::Improving
    } else if late_avg > early_avg + threshold {
        ScoreTrend::Declining
    } else {
        ScoreTrend::Stable
    }
}

// ============================================================================
//  测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ======================================================================
    //  辅助函数
    // ======================================================================

    fn make_snapshot(evaluator_type: EvaluatorType, enabled: bool, diff: f64) -> EvaluatorSnapshot {
        EvaluatorSnapshot {
            evaluator_type,
            enabled,
            with_fix_rate: if diff >= 0.0 { 0.7 } else { 0.3 },
            without_fix_rate: if diff >= 0.0 { 0.5 } else { 0.7 },
            diff,
            is_beneficial: diff > 0.0,
            total_checks: 10,
            evaluation_count: 3,
            disable_count: if enabled { 0 } else { 1 },
            contribution_score: diff,
        }
    }

    fn make_all_enabled() -> Vec<EvaluatorSnapshot> {
        vec![
            make_snapshot(EvaluatorType::CacheTuner, true, 0.2),
            make_snapshot(EvaluatorType::SearchQuality, true, 0.2),
            make_snapshot(EvaluatorType::MemoryContext, true, 0.2),
        ]
    }

    fn make_one_disabled() -> Vec<EvaluatorSnapshot> {
        vec![
            make_snapshot(EvaluatorType::CacheTuner, false, -0.4),
            make_snapshot(EvaluatorType::SearchQuality, true, 0.2),
            make_snapshot(EvaluatorType::MemoryContext, true, 0.2),
        ]
    }

    fn make_two_disabled() -> Vec<EvaluatorSnapshot> {
        vec![
            make_snapshot(EvaluatorType::CacheTuner, false, -0.4),
            make_snapshot(EvaluatorType::SearchQuality, false, -0.3),
            make_snapshot(EvaluatorType::MemoryContext, true, 0.2),
        ]
    }

    fn make_all_disabled() -> Vec<EvaluatorSnapshot> {
        vec![
            make_snapshot(EvaluatorType::CacheTuner, false, -0.4),
            make_snapshot(EvaluatorType::SearchQuality, false, -0.3),
            make_snapshot(EvaluatorType::MemoryContext, false, -0.2),
        ]
    }

    // ======================================================================
    //  JointDecisionAction 测试
    // ======================================================================

    #[test]
    fn test_action_default() {
        assert_eq!(
            JointDecisionAction::default(),
            JointDecisionAction::NoAction
        );
    }

    #[test]
    fn test_action_display() {
        assert_eq!(format!("{}", JointDecisionAction::NoAction), "NoAction");
        assert_eq!(
            format!("{}", JointDecisionAction::EscalateWarning),
            "EscalateWarning"
        );
        assert_eq!(
            format!("{}", JointDecisionAction::EnterConservativeMode),
            "EnterConservativeMode"
        );
        let re = JointDecisionAction::ReEnableFeature {
            evaluator_type: EvaluatorType::CacheTuner,
        };
        assert!(format!("{}", re).contains("ReEnableFeature"));
        assert!(format!("{}", re).contains("CacheTuner"));
    }

    #[test]
    fn test_action_label() {
        assert_eq!(JointDecisionAction::NoAction.label(), "无联合行动");
        assert_eq!(JointDecisionAction::EscalateWarning.label(), "升级警告");
        assert_eq!(
            JointDecisionAction::EnterConservativeMode.label(),
            "进入保守模式"
        );
        assert_eq!(
            JointDecisionAction::ReEnableFeature {
                evaluator_type: EvaluatorType::CacheTuner
            }
            .label(),
            "重新启用功能"
        );
    }

    #[test]
    fn test_action_is_conservative() {
        assert!(!JointDecisionAction::NoAction.is_conservative());
        assert!(!JointDecisionAction::EscalateWarning.is_conservative());
        assert!(JointDecisionAction::EnterConservativeMode.is_conservative());
        assert!(!JointDecisionAction::ReEnableFeature {
            evaluator_type: EvaluatorType::CacheTuner
        }
        .is_conservative());
    }

    #[test]
    fn test_action_is_escalation() {
        assert!(!JointDecisionAction::NoAction.is_escalation());
        assert!(JointDecisionAction::EscalateWarning.is_escalation());
        assert!(JointDecisionAction::EnterConservativeMode.is_escalation());
        assert!(!JointDecisionAction::ReEnableFeature {
            evaluator_type: EvaluatorType::CacheTuner
        }
        .is_escalation());
    }

    #[test]
    fn test_action_serde_roundtrip() {
        let actions = vec![
            JointDecisionAction::NoAction,
            JointDecisionAction::EscalateWarning,
            JointDecisionAction::EnterConservativeMode,
            JointDecisionAction::ReEnableFeature {
                evaluator_type: EvaluatorType::CacheTuner,
            },
        ];
        for action in actions {
            let json = serde_json::to_string(&action).unwrap();
            let loaded: JointDecisionAction = serde_json::from_str(&json).unwrap();
            assert_eq!(action, loaded);
        }
    }

    // ======================================================================
    //  JointDecisionConfig 测试
    // ======================================================================

    #[test]
    fn test_config_default() {
        let config = JointDecisionConfig::default();
        assert_eq!(config.conservative_mode_threshold, 3);
        assert_eq!(config.escalate_warning_threshold, 2);
        assert_eq!(config.total_evaluators, 3);
        assert_eq!(config.re_enable_after_rounds, 5);
    }

    #[test]
    fn test_config_new() {
        let config = JointDecisionConfig::new(2, 1);
        assert_eq!(config.conservative_mode_threshold, 2);
        assert_eq!(config.escalate_warning_threshold, 1);
    }

    #[test]
    fn test_config_builders() {
        let config = JointDecisionConfig::default()
            .with_conservative_threshold(2)
            .with_escalate_threshold(1)
            .with_total_evaluators(5)
            .with_re_enable_after_rounds(10);
        assert_eq!(config.conservative_mode_threshold, 2);
        assert_eq!(config.escalate_warning_threshold, 1);
        assert_eq!(config.total_evaluators, 5);
        assert_eq!(config.re_enable_after_rounds, 10);
    }

    #[test]
    fn test_config_serde_roundtrip() {
        let config = JointDecisionConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let loaded: JointDecisionConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(
            config.conservative_mode_threshold,
            loaded.conservative_mode_threshold
        );
    }

    // ======================================================================
    //  JointDecision 测试
    // ======================================================================

    #[test]
    fn test_decision_new() {
        let decision = JointDecision::new(
            JointDecisionAction::EscalateWarning,
            "test".to_string(),
            2,
            3,
            vec![EvaluatorType::CacheTuner],
        );
        assert!(decision.should_escalate);
        assert!(!decision.conservative_mode);
        assert_eq!(decision.disabled_count, 2);
        assert_eq!(decision.total_evaluators, 3);
        assert_eq!(
            decision.disabled_evaluators,
            vec![EvaluatorType::CacheTuner]
        );
    }

    #[test]
    fn test_decision_no_action() {
        let decision = JointDecision::no_action(3);
        assert!(!decision.should_escalate);
        assert!(!decision.conservative_mode);
        assert_eq!(decision.disabled_count, 0);
        assert!(!decision.has_action());
    }

    #[test]
    fn test_decision_conservative() {
        let decision = JointDecision::new(
            JointDecisionAction::EnterConservativeMode,
            "all disabled".to_string(),
            3,
            3,
            vec![
                EvaluatorType::CacheTuner,
                EvaluatorType::SearchQuality,
                EvaluatorType::MemoryContext,
            ],
        );
        assert!(decision.conservative_mode);
        assert!(decision.should_escalate);
        assert!(decision.has_action());
    }

    #[test]
    fn test_decision_to_trace_summary() {
        let decision = JointDecision::new(
            JointDecisionAction::EscalateWarning,
            "2/3 禁用".to_string(),
            2,
            3,
            vec![],
        );
        let summary = decision.to_trace_summary();
        assert!(summary.contains("升级警告"));
        assert!(summary.contains("2/3"));
    }

    #[test]
    fn test_decision_to_summary() {
        let decision = JointDecision::no_action(3);
        let summary = decision.to_summary();
        assert!(summary.contains("无联合行动"));
        assert!(summary.contains("0/3"));
    }

    #[test]
    fn test_decision_has_action() {
        assert!(!JointDecision::no_action(3).has_action());
        let d = JointDecision::new(
            JointDecisionAction::EscalateWarning,
            "test".to_string(),
            2,
            3,
            vec![],
        );
        assert!(d.has_action());
    }

    #[test]
    fn test_decision_serde_roundtrip() {
        let decision = JointDecision::new(
            JointDecisionAction::EnterConservativeMode,
            "all disabled".to_string(),
            3,
            3,
            vec![EvaluatorType::CacheTuner],
        );
        let json = serde_json::to_string(&decision).unwrap();
        let loaded: JointDecision = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.action, JointDecisionAction::EnterConservativeMode);
        assert_eq!(loaded.disabled_count, 3);
    }

    // ======================================================================
    //  纯函数测试 — count_disabled_evaluators
    // ======================================================================

    #[test]
    fn test_count_disabled_empty() {
        assert_eq!(count_disabled_evaluators(&[]), 0);
    }

    #[test]
    fn test_count_disabled_all_enabled() {
        assert_eq!(count_disabled_evaluators(&make_all_enabled()), 0);
    }

    #[test]
    fn test_count_disabled_one() {
        assert_eq!(count_disabled_evaluators(&make_one_disabled()), 1);
    }

    #[test]
    fn test_count_disabled_two() {
        assert_eq!(count_disabled_evaluators(&make_two_disabled()), 2);
    }

    #[test]
    fn test_count_disabled_all() {
        assert_eq!(count_disabled_evaluators(&make_all_disabled()), 3);
    }

    // ======================================================================
    //  纯函数测试 — get_disabled_evaluator_types
    // ======================================================================

    #[test]
    fn test_get_disabled_types_empty() {
        assert!(get_disabled_evaluator_types(&[]).is_empty());
    }

    #[test]
    fn test_get_disabled_types_all_enabled() {
        assert!(get_disabled_evaluator_types(&make_all_enabled()).is_empty());
    }

    #[test]
    fn test_get_disabled_types_one() {
        let types = get_disabled_evaluator_types(&make_one_disabled());
        assert_eq!(types, vec![EvaluatorType::CacheTuner]);
    }

    #[test]
    fn test_get_disabled_types_all() {
        let types = get_disabled_evaluator_types(&make_all_disabled());
        assert_eq!(types.len(), 3);
        assert!(types.contains(&EvaluatorType::CacheTuner));
        assert!(types.contains(&EvaluatorType::SearchQuality));
        assert!(types.contains(&EvaluatorType::MemoryContext));
    }

    // ======================================================================
    //  纯函数测试 — should_enter_conservative_mode
    // ======================================================================

    #[test]
    fn test_conservative_mode_zero_total() {
        assert!(!should_enter_conservative_mode(0, 0, 3));
    }

    #[test]
    fn test_conservative_mode_below_threshold() {
        assert!(!should_enter_conservative_mode(2, 3, 3));
    }

    #[test]
    fn test_conservative_mode_at_threshold() {
        assert!(should_enter_conservative_mode(3, 3, 3));
    }

    #[test]
    fn test_conservative_mode_above_threshold() {
        assert!(should_enter_conservative_mode(3, 3, 2));
    }

    #[test]
    fn test_conservative_mode_threshold_clamped() {
        // threshold > total → clamped to total
        assert!(should_enter_conservative_mode(2, 2, 5));
    }

    // ======================================================================
    //  纯函数测试 — should_escalate_warning
    // ======================================================================

    #[test]
    fn test_escalate_warning_zero_total() {
        assert!(!should_escalate_warning(0, 0, 2, 3));
    }

    #[test]
    fn test_escalate_warning_below_threshold() {
        assert!(!should_escalate_warning(1, 3, 2, 3));
    }

    #[test]
    fn test_escalate_warning_at_threshold() {
        assert!(should_escalate_warning(2, 3, 2, 3));
    }

    #[test]
    fn test_escalate_warning_at_conservative() {
        // disabled == conservative_threshold → not escalate, but conservative
        assert!(!should_escalate_warning(3, 3, 2, 3));
    }

    #[test]
    fn test_escalate_warning_above_conservative() {
        assert!(!should_escalate_warning(3, 3, 2, 2));
    }

    // ======================================================================
    //  纯函数测试 — select_re_enable_candidate
    // ======================================================================

    #[test]
    fn test_select_re_enable_empty() {
        assert!(select_re_enable_candidate(&[]).is_none());
    }

    #[test]
    fn test_select_re_enable_all_enabled() {
        assert!(select_re_enable_candidate(&make_all_enabled()).is_none());
    }

    #[test]
    fn test_select_re_enable_one_disabled() {
        let snapshots = make_one_disabled();
        let candidate = select_re_enable_candidate(&snapshots);
        assert_eq!(candidate, Some(EvaluatorType::CacheTuner));
    }

    #[test]
    fn test_select_re_enable_multiple_disabled() {
        // CacheTuner diff=-0.4, SearchQuality diff=-0.3, MemoryContext diff=0.2 (enabled)
        let snapshots = vec![
            make_snapshot(EvaluatorType::CacheTuner, false, -0.4),
            make_snapshot(EvaluatorType::SearchQuality, false, -0.3),
            make_snapshot(EvaluatorType::MemoryContext, true, 0.2),
        ];
        // -0.3 > -0.4 → SearchQuality
        let candidate = select_re_enable_candidate(&snapshots);
        assert_eq!(candidate, Some(EvaluatorType::SearchQuality));
    }

    #[test]
    fn test_select_re_enable_all_disabled_picks_best() {
        let snapshots = make_all_disabled();
        // CacheTuner=-0.4, SearchQuality=-0.3, MemoryContext=-0.2
        // -0.2 is the highest → MemoryContext
        let candidate = select_re_enable_candidate(&snapshots);
        assert_eq!(candidate, Some(EvaluatorType::MemoryContext));
    }

    // ======================================================================
    //  纯函数测试 — compute_joint_decision
    // ======================================================================

    #[test]
    fn test_compute_decision_all_enabled() {
        let config = JointDecisionConfig::default();
        let decision = compute_joint_decision(&make_all_enabled(), &config);
        assert_eq!(decision.action, JointDecisionAction::NoAction);
        assert!(!decision.should_escalate);
        assert!(!decision.conservative_mode);
        assert_eq!(decision.disabled_count, 0);
    }

    #[test]
    fn test_compute_decision_one_disabled() {
        let config = JointDecisionConfig::default();
        let decision = compute_joint_decision(&make_one_disabled(), &config);
        assert_eq!(decision.action, JointDecisionAction::NoAction);
        assert!(!decision.should_escalate);
    }

    #[test]
    fn test_compute_decision_two_disabled() {
        let config = JointDecisionConfig::default();
        let decision = compute_joint_decision(&make_two_disabled(), &config);
        assert_eq!(decision.action, JointDecisionAction::EscalateWarning);
        assert!(decision.should_escalate);
        assert!(!decision.conservative_mode);
        assert_eq!(decision.disabled_count, 2);
    }

    #[test]
    fn test_compute_decision_all_disabled() {
        let config = JointDecisionConfig::default();
        let decision = compute_joint_decision(&make_all_disabled(), &config);
        assert_eq!(decision.action, JointDecisionAction::EnterConservativeMode);
        assert!(decision.conservative_mode);
        assert!(decision.should_escalate);
        assert_eq!(decision.disabled_count, 3);
    }

    #[test]
    fn test_compute_decision_empty_snapshots() {
        let config = JointDecisionConfig::default();
        let decision = compute_joint_decision(&[], &config);
        assert_eq!(decision.action, JointDecisionAction::NoAction);
    }

    #[test]
    fn test_compute_decision_custom_thresholds() {
        // Lower thresholds: conservative=2, escalate=1
        let config = JointDecisionConfig::new(2, 1);
        let decision = compute_joint_decision(&make_one_disabled(), &config);
        assert_eq!(decision.action, JointDecisionAction::EscalateWarning);

        let decision = compute_joint_decision(&make_two_disabled(), &config);
        assert_eq!(decision.action, JointDecisionAction::EnterConservativeMode);
    }

    #[test]
    fn test_compute_decision_reason_contains_info() {
        let config = JointDecisionConfig::default();
        let decision = compute_joint_decision(&make_two_disabled(), &config);
        assert!(decision.reason.contains("2/3"));
        assert!(decision.reason.contains("升级警告"));
    }

    #[test]
    fn test_compute_decision_conservative_reason() {
        let config = JointDecisionConfig::default();
        let decision = compute_joint_decision(&make_all_disabled(), &config);
        assert!(decision.reason.contains("3/3"));
        assert!(decision.reason.contains("保守模式"));
    }

    #[test]
    fn test_compute_decision_disabled_evaluators_populated() {
        let config = JointDecisionConfig::default();
        let decision = compute_joint_decision(&make_two_disabled(), &config);
        assert_eq!(decision.disabled_evaluators.len(), 2);
        assert!(decision
            .disabled_evaluators
            .contains(&EvaluatorType::CacheTuner));
        assert!(decision
            .disabled_evaluators
            .contains(&EvaluatorType::SearchQuality));
    }

    // ======================================================================
    //  JointDecisionEngine 测试
    // ======================================================================

    #[test]
    fn test_engine_new() {
        let engine = JointDecisionEngine::new(JointDecisionConfig::default());
        assert!(!engine.is_conservative_mode());
        assert!(!engine.has_decisions());
        assert_eq!(engine.decision_count(), 0);
        assert!(engine.history.is_empty());
    }

    #[test]
    fn test_engine_default() {
        let engine = JointDecisionEngine::default();
        assert!(!engine.is_conservative_mode());
    }

    #[test]
    fn test_engine_evaluate_no_action() {
        let mut engine = JointDecisionEngine::default();
        let decision = engine.evaluate(&make_all_enabled());
        assert_eq!(decision.action, JointDecisionAction::NoAction);
        assert_eq!(engine.decision_count(), 1);
        assert!(!engine.is_conservative_mode());
    }

    #[test]
    fn test_engine_evaluate_escalate() {
        let mut engine = JointDecisionEngine::default();
        let decision = engine.evaluate(&make_two_disabled());
        assert_eq!(decision.action, JointDecisionAction::EscalateWarning);
        assert_eq!(engine.escalate_count(), 1);
        assert!(!engine.is_conservative_mode());
    }

    #[test]
    fn test_engine_evaluate_conservative() {
        let mut engine = JointDecisionEngine::default();
        let decision = engine.evaluate(&make_all_disabled());
        assert_eq!(decision.action, JointDecisionAction::EnterConservativeMode);
        assert!(engine.is_conservative_mode());
        assert_eq!(engine.conservative_mode_count(), 1);
        assert_eq!(engine.conservative_rounds, 1);
    }

    #[test]
    fn test_engine_conservative_rounds_increment() {
        let mut engine = JointDecisionEngine::default();
        // Enter conservative mode
        engine.evaluate(&make_all_disabled());
        assert_eq!(engine.conservative_rounds, 1);
        // Continue in conservative mode (all still disabled → conservative again)
        engine.evaluate(&make_all_disabled());
        assert_eq!(engine.conservative_rounds, 2);
    }

    #[test]
    fn test_engine_re_enable_after_rounds() {
        let config = JointDecisionConfig::default().with_re_enable_after_rounds(2);
        let mut engine = JointDecisionEngine::new(config);

        // Round 1: Enter conservative mode
        let d1 = engine.evaluate(&make_all_disabled());
        assert_eq!(d1.action, JointDecisionAction::EnterConservativeMode);
        assert_eq!(engine.conservative_rounds, 1);

        // Round 2: Still conservative, reaches threshold → ReEnableFeature
        let d2 = engine.evaluate(&make_all_disabled());
        assert_eq!(
            d2.action,
            JointDecisionAction::ReEnableFeature {
                evaluator_type: EvaluatorType::MemoryContext
            }
        );
        assert!(!engine.is_conservative_mode());
        assert_eq!(engine.conservative_rounds, 0);
    }

    #[test]
    fn test_engine_re_enable_picks_best_candidate() {
        let config = JointDecisionConfig::default().with_re_enable_after_rounds(1);
        let mut engine = JointDecisionEngine::new(config);

        // Enter conservative mode
        engine.evaluate(&make_all_disabled());

        // Next round → should re-enable MemoryContext (diff=-0.2, highest among disabled)
        let d = engine.evaluate(&make_all_disabled());
        if let JointDecisionAction::ReEnableFeature { evaluator_type } = &d.action {
            assert_eq!(*evaluator_type, EvaluatorType::MemoryContext);
        } else {
            panic!("Expected ReEnableFeature, got {:?}", d.action);
        }
    }

    #[test]
    fn test_engine_latest_decision() {
        let mut engine = JointDecisionEngine::default();
        engine.evaluate(&make_all_enabled());
        engine.evaluate(&make_two_disabled());
        let latest = engine.latest_decision().unwrap();
        assert_eq!(latest.action, JointDecisionAction::EscalateWarning);
    }

    #[test]
    fn test_engine_latest_decision_none() {
        let engine = JointDecisionEngine::default();
        assert!(engine.latest_decision().is_none());
    }

    #[test]
    fn test_engine_has_decisions() {
        let mut engine = JointDecisionEngine::default();
        assert!(!engine.has_decisions());
        engine.evaluate(&make_all_enabled());
        assert!(engine.has_decisions());
    }

    #[test]
    fn test_engine_counts() {
        let mut engine = JointDecisionEngine::default();
        engine.evaluate(&make_all_enabled()); // NoAction
        engine.evaluate(&make_two_disabled()); // EscalateWarning
        engine.evaluate(&make_two_disabled()); // EscalateWarning
        engine.evaluate(&make_all_disabled()); // EnterConservativeMode

        assert_eq!(engine.decision_count(), 4);
        assert_eq!(engine.escalate_count(), 2);
        assert_eq!(engine.conservative_mode_count(), 1);
    }

    #[test]
    fn test_engine_decisions_list() {
        let mut engine = JointDecisionEngine::default();
        engine.evaluate(&make_all_enabled());
        engine.evaluate(&make_two_disabled());
        let decisions = engine.decisions();
        assert_eq!(decisions.len(), 2);
        assert_eq!(decisions[0].action, JointDecisionAction::NoAction);
        assert_eq!(decisions[1].action, JointDecisionAction::EscalateWarning);
    }

    #[test]
    fn test_engine_finalize_session() {
        let mut engine = JointDecisionEngine::default();
        engine.evaluate(&make_two_disabled());
        engine.evaluate(&make_all_disabled());

        engine.finalize_session(Utc::now());

        assert_eq!(engine.history.session_count(), 1);
        let entry = engine.history.latest().unwrap();
        assert_eq!(entry.decision_count, 2);
        assert_eq!(entry.escalate_count, 1);
        assert_eq!(entry.conservative_mode_count, 1);
        assert!(entry.final_conservative_mode);
    }

    #[test]
    fn test_engine_finalize_multiple_sessions() {
        let mut engine = JointDecisionEngine::default();

        // Session 1
        engine.evaluate(&make_two_disabled());
        engine.finalize_session(Utc::now());
        assert_eq!(engine.history.session_count(), 1);

        // Session 2
        engine.evaluate(&make_all_enabled());
        engine.finalize_session(Utc::now());
        assert_eq!(engine.history.session_count(), 2);
        let latest = engine.history.latest().unwrap();
        assert_eq!(latest.session_index, 2);
        assert_eq!(latest.action, JointDecisionAction::NoAction);
    }

    #[test]
    fn test_engine_with_timestamp() {
        let engine = JointDecisionEngine::default().with_timestamp(Utc::now());
        assert!(engine.history.saved_at.is_some());
    }

    #[test]
    fn test_engine_to_history_summary_empty() {
        let engine = JointDecisionEngine::default();
        let summary = engine.to_history_summary();
        assert!(summary.is_empty());
    }

    #[test]
    fn test_engine_to_history_summary_with_data() {
        let mut engine = JointDecisionEngine::default();
        engine.evaluate(&make_two_disabled());
        engine.finalize_session(Utc::now());
        let summary = engine.to_history_summary();
        assert!(!summary.is_empty());
        assert_eq!(summary.session_count, 1);
        assert_eq!(summary.total_escalations, 1);
    }

    #[test]
    fn test_engine_serde_roundtrip() {
        let mut engine = JointDecisionEngine::default();
        engine.evaluate(&make_two_disabled());
        let json = serde_json::to_string(&engine).unwrap();
        let loaded: JointDecisionEngine = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.decision_count(), 1);
        assert_eq!(loaded.escalate_count(), 1);
    }

    // ======================================================================
    //  JointDecisionHistoryEntry 测试
    // ======================================================================

    #[test]
    fn test_history_entry_new() {
        let entry = JointDecisionHistoryEntry::new(
            1,
            Utc::now(),
            JointDecisionAction::EscalateWarning,
            5,
            2,
            1,
            false,
            vec![EvaluatorType::CacheTuner],
        );
        assert_eq!(entry.session_index, 1);
        assert_eq!(entry.action, JointDecisionAction::EscalateWarning);
        assert_eq!(entry.decision_count, 5);
        assert_eq!(entry.escalate_count, 2);
        assert_eq!(entry.conservative_mode_count, 1);
        assert!(!entry.final_conservative_mode);
        assert_eq!(entry.disabled_evaluators, vec![EvaluatorType::CacheTuner]);
    }

    #[test]
    fn test_history_entry_to_summary() {
        let entry = JointDecisionHistoryEntry::new(
            1,
            Utc::now(),
            JointDecisionAction::EscalateWarning,
            5,
            2,
            1,
            false,
            vec![],
        );
        let summary = entry.to_summary();
        assert!(summary.contains("Session 1"));
        assert!(summary.contains("5 决策"));
        assert!(summary.contains("2 升级"));
        assert!(summary.contains("1 保守模式"));
    }

    #[test]
    fn test_history_entry_serde_roundtrip() {
        let entry = JointDecisionHistoryEntry::new(
            1,
            Utc::now(),
            JointDecisionAction::EnterConservativeMode,
            3,
            0,
            1,
            true,
            vec![EvaluatorType::CacheTuner, EvaluatorType::SearchQuality],
        );
        let json = serde_json::to_string(&entry).unwrap();
        let loaded: JointDecisionHistoryEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.session_index, 1);
        assert_eq!(loaded.action, JointDecisionAction::EnterConservativeMode);
        assert!(loaded.final_conservative_mode);
        assert_eq!(loaded.disabled_evaluators.len(), 2);
    }

    // ======================================================================
    //  JointDecisionHistory 测试
    // ======================================================================

    #[test]
    fn test_history_new() {
        let history = JointDecisionHistory::new();
        assert!(history.is_empty());
        assert_eq!(history.session_count(), 0);
        assert!(history.saved_at.is_none());
    }

    #[test]
    fn test_history_default() {
        let history = JointDecisionHistory::default();
        assert!(history.is_empty());
    }

    #[test]
    fn test_history_add_entry() {
        let mut history = JointDecisionHistory::new();
        history.add_entry(JointDecisionHistoryEntry::new(
            1,
            Utc::now(),
            JointDecisionAction::NoAction,
            5,
            0,
            0,
            false,
            vec![],
        ));
        assert_eq!(history.session_count(), 1);
        assert!(!history.is_empty());
    }

    #[test]
    fn test_history_next_session_index() {
        let mut history = JointDecisionHistory::new();
        assert_eq!(history.next_session_index(), 1);
        history.add_entry(JointDecisionHistoryEntry::new(
            1,
            Utc::now(),
            JointDecisionAction::NoAction,
            1,
            0,
            0,
            false,
            vec![],
        ));
        assert_eq!(history.next_session_index(), 2);
    }

    #[test]
    fn test_history_max_sessions() {
        let mut history = JointDecisionHistory::new();
        for i in 0..(MAX_JOINT_DECISION_HISTORY_SESSIONS + 10) {
            history.add_entry(JointDecisionHistoryEntry::new(
                i + 1,
                Utc::now(),
                JointDecisionAction::NoAction,
                1,
                0,
                0,
                false,
                vec![],
            ));
        }
        assert_eq!(history.session_count(), MAX_JOINT_DECISION_HISTORY_SESSIONS);
        // First entry should be removed (session 11)
        assert_eq!(history.sessions.first().unwrap().session_index, 11);
    }

    #[test]
    fn test_history_latest() {
        let mut history = JointDecisionHistory::new();
        assert!(history.latest().is_none());
        history.add_entry(JointDecisionHistoryEntry::new(
            1,
            Utc::now(),
            JointDecisionAction::NoAction,
            1,
            0,
            0,
            false,
            vec![],
        ));
        history.add_entry(JointDecisionHistoryEntry::new(
            2,
            Utc::now(),
            JointDecisionAction::EscalateWarning,
            3,
            1,
            0,
            false,
            vec![],
        ));
        let latest = history.latest().unwrap();
        assert_eq!(latest.session_index, 2);
        assert_eq!(latest.action, JointDecisionAction::EscalateWarning);
    }

    #[test]
    fn test_history_total_escalations() {
        let mut history = JointDecisionHistory::new();
        history.add_entry(JointDecisionHistoryEntry::new(
            1,
            Utc::now(),
            JointDecisionAction::EscalateWarning,
            5,
            2,
            0,
            false,
            vec![],
        ));
        history.add_entry(JointDecisionHistoryEntry::new(
            2,
            Utc::now(),
            JointDecisionAction::NoAction,
            3,
            1,
            0,
            false,
            vec![],
        ));
        assert_eq!(history.total_escalations(), 3);
    }

    #[test]
    fn test_history_total_conservative_modes() {
        let mut history = JointDecisionHistory::new();
        history.add_entry(JointDecisionHistoryEntry::new(
            1,
            Utc::now(),
            JointDecisionAction::EnterConservativeMode,
            5,
            0,
            2,
            true,
            vec![],
        ));
        history.add_entry(JointDecisionHistoryEntry::new(
            2,
            Utc::now(),
            JointDecisionAction::NoAction,
            3,
            0,
            1,
            false,
            vec![],
        ));
        assert_eq!(history.total_conservative_modes(), 3);
    }

    #[test]
    fn test_history_total_decisions() {
        let mut history = JointDecisionHistory::new();
        history.add_entry(JointDecisionHistoryEntry::new(
            1,
            Utc::now(),
            JointDecisionAction::NoAction,
            5,
            0,
            0,
            false,
            vec![],
        ));
        history.add_entry(JointDecisionHistoryEntry::new(
            2,
            Utc::now(),
            JointDecisionAction::NoAction,
            3,
            0,
            0,
            false,
            vec![],
        ));
        assert_eq!(history.total_decisions(), 8);
    }

    #[test]
    fn test_history_conservative_mode_rate() {
        let mut history = JointDecisionHistory::new();
        // 0 sessions
        assert_eq!(history.conservative_mode_rate(), 0.0);

        // 2 sessions, 1 conservative
        history.add_entry(JointDecisionHistoryEntry::new(
            1,
            Utc::now(),
            JointDecisionAction::EnterConservativeMode,
            5,
            0,
            1,
            true,
            vec![],
        ));
        history.add_entry(JointDecisionHistoryEntry::new(
            2,
            Utc::now(),
            JointDecisionAction::NoAction,
            3,
            0,
            0,
            false,
            vec![],
        ));
        assert!((history.conservative_mode_rate() - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_history_escalation_rate() {
        let mut history = JointDecisionHistory::new();
        // 0 sessions
        assert_eq!(history.escalation_rate(), 0.0);

        // 4 sessions, 2 with escalations
        history.add_entry(JointDecisionHistoryEntry::new(
            1,
            Utc::now(),
            JointDecisionAction::EscalateWarning,
            5,
            1,
            0,
            false,
            vec![],
        ));
        history.add_entry(JointDecisionHistoryEntry::new(
            2,
            Utc::now(),
            JointDecisionAction::NoAction,
            3,
            0,
            0,
            false,
            vec![],
        ));
        history.add_entry(JointDecisionHistoryEntry::new(
            3,
            Utc::now(),
            JointDecisionAction::EnterConservativeMode,
            5,
            0,
            1,
            true,
            vec![],
        ));
        history.add_entry(JointDecisionHistoryEntry::new(
            4,
            Utc::now(),
            JointDecisionAction::NoAction,
            3,
            0,
            0,
            false,
            vec![],
        ));
        assert!((history.escalation_rate() - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_history_with_timestamp() {
        let history = JointDecisionHistory::new().with_timestamp(Utc::now());
        assert!(history.saved_at.is_some());
    }

    #[test]
    fn test_history_to_summary_empty() {
        let history = JointDecisionHistory::new();
        assert_eq!(history.to_summary(), "联合决策历史: 无记录");
    }

    #[test]
    fn test_history_to_summary_with_data() {
        let mut history = JointDecisionHistory::new();
        history.add_entry(JointDecisionHistoryEntry::new(
            1,
            Utc::now(),
            JointDecisionAction::EscalateWarning,
            5,
            2,
            1,
            false,
            vec![],
        ));
        let s = history.to_summary();
        assert!(s.contains("1 个 session"));
        assert!(s.contains("5 决策"));
        assert!(s.contains("2 升级"));
        assert!(s.contains("1 保守模式"));
    }

    #[test]
    fn test_history_serde_roundtrip() {
        let mut history = JointDecisionHistory::new();
        history.add_entry(JointDecisionHistoryEntry::new(
            1,
            Utc::now(),
            JointDecisionAction::EscalateWarning,
            5,
            2,
            1,
            false,
            vec![EvaluatorType::CacheTuner],
        ));
        let json = serde_json::to_string(&history).unwrap();
        let loaded: JointDecisionHistory = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.session_count(), 1);
        assert_eq!(loaded.total_escalations(), 2);
    }

    // ======================================================================
    //  JointDecisionHistory 持久化测试
    // ======================================================================

    #[test]
    fn test_history_load_nonexistent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.json");
        let history = JointDecisionHistory::load(&path).unwrap();
        assert!(history.is_empty());
    }

    #[test]
    fn test_history_load_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.json");
        std::fs::write(&path, "").unwrap();
        let history = JointDecisionHistory::load(&path).unwrap();
        assert!(history.is_empty());
    }

    #[test]
    fn test_history_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("joint_history.json");

        let mut history = JointDecisionHistory::new();
        history.add_entry(JointDecisionHistoryEntry::new(
            1,
            Utc::now(),
            JointDecisionAction::EscalateWarning,
            5,
            2,
            1,
            false,
            vec![],
        ));
        history.save(&path).unwrap();
        assert!(path.exists());

        let loaded = JointDecisionHistory::load(&path).unwrap();
        assert_eq!(loaded.session_count(), 1);
        assert_eq!(loaded.total_escalations(), 2);
    }

    #[test]
    fn test_history_save_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("subdir")
            .join("nested")
            .join("joint_history.json");

        let history = JointDecisionHistory::new();
        history.save(&path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn test_history_load_from_workspace_nonexistent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(JointDecisionHistory::load_from_workspace(dir.path()).is_none());
    }

    #[test]
    fn test_history_load_from_workspace_existing() {
        let dir = tempfile::tempdir().unwrap();

        let mut history = JointDecisionHistory::new();
        history.add_entry(JointDecisionHistoryEntry::new(
            1,
            Utc::now(),
            JointDecisionAction::EscalateWarning,
            5,
            2,
            1,
            false,
            vec![],
        ));
        history.save_to_workspace(dir.path()).unwrap();

        let loaded = JointDecisionHistory::load_from_workspace(dir.path());
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().session_count(), 1);
    }

    #[test]
    fn test_history_load_from_workspace_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();

        let history = JointDecisionHistory::new();
        history.save_to_workspace(dir.path()).unwrap();

        assert!(JointDecisionHistory::load_from_workspace(dir.path()).is_none());
    }

    #[test]
    fn test_history_save_to_workspace_creates_forge_dir() {
        let dir = tempfile::tempdir().unwrap();
        let history = JointDecisionHistory::new();
        history.save_to_workspace(dir.path()).unwrap();

        let path = dir
            .path()
            .join(".forge")
            .join(JOINT_DECISION_HISTORY_FILENAME);
        assert!(path.exists());
    }

    #[test]
    fn test_history_cross_session_persistence() {
        let dir = tempfile::tempdir().unwrap();

        // Session 1: Save
        {
            let mut engine = JointDecisionEngine::default();
            engine.evaluate(&make_two_disabled());
            engine.finalize_session(Utc::now());
            engine.save_history_to_workspace(dir.path()).unwrap();
        }

        // Session 2: Load and append
        {
            let mut engine = JointDecisionEngine::default();
            engine.load_history_from_workspace(dir.path());
            assert_eq!(engine.history.session_count(), 1);

            engine.evaluate(&make_all_disabled());
            engine.finalize_session(Utc::now());
            engine.save_history_to_workspace(dir.path()).unwrap();
        }

        // Session 3: Verify
        {
            let _engine = JointDecisionEngine::default();
            let history = JointDecisionHistory::load_from_workspace(dir.path()).unwrap();
            assert_eq!(history.session_count(), 2);
            assert_eq!(history.total_escalations(), 1); // From session 1
            assert_eq!(history.total_conservative_modes(), 1); // From session 2
        }
    }

    // ======================================================================
    //  JointDecisionHistorySummary 测试
    // ======================================================================

    #[test]
    fn test_summary_empty() {
        let summary = JointDecisionHistorySummary::empty();
        assert!(summary.is_empty());
        assert_eq!(summary.session_count, 0);
        assert_eq!(summary.latest_action, JointDecisionAction::NoAction);
    }

    #[test]
    fn test_summary_default() {
        let summary = JointDecisionHistorySummary::default();
        assert!(summary.is_empty());
    }

    #[test]
    fn test_summary_new() {
        let summary = JointDecisionHistorySummary::new(
            3,
            JointDecisionAction::EnterConservativeMode,
            15,
            5,
            2,
            0.33,
            0.67,
            true,
            Some("2024-01-01T00:00:00Z".to_string()),
        );
        assert_eq!(summary.session_count, 3);
        assert_eq!(
            summary.latest_action,
            JointDecisionAction::EnterConservativeMode
        );
        assert_eq!(summary.total_decisions, 15);
        assert_eq!(summary.total_escalations, 5);
        assert_eq!(summary.total_conservative_modes, 2);
        assert!((summary.conservative_mode_rate - 0.33).abs() < 0.01);
        assert!((summary.escalation_rate - 0.67).abs() < 0.01);
        assert!(summary.current_conservative_mode);
    }

    #[test]
    fn test_summary_is_empty() {
        assert!(JointDecisionHistorySummary::empty().is_empty());
        let summary = JointDecisionHistorySummary::new(
            1,
            JointDecisionAction::NoAction,
            1,
            0,
            0,
            0.0,
            0.0,
            false,
            None,
        );
        assert!(!summary.is_empty());
    }

    #[test]
    fn test_summary_to_report_section_empty() {
        let summary = JointDecisionHistorySummary::empty();
        assert!(summary.to_report_section().is_empty());
    }

    #[test]
    fn test_summary_to_report_section_with_data() {
        let summary = JointDecisionHistorySummary::new(
            3,
            JointDecisionAction::EscalateWarning,
            15,
            5,
            2,
            0.33,
            0.67,
            false,
            Some("2024-01-01T00:00:00Z".to_string()),
        );
        let report = summary.to_report_section();
        assert!(report.contains("协同决策"));
        assert!(report.contains("3"));
        assert!(report.contains("升级警告"));
        assert!(report.contains("15"));
        assert!(report.contains("5"));
        assert!(report.contains("2"));
        assert!(report.contains("33.0%"));
        assert!(report.contains("正常模式"));
    }

    #[test]
    fn test_summary_to_report_conservative_mode() {
        let summary = JointDecisionHistorySummary::new(
            1,
            JointDecisionAction::EnterConservativeMode,
            5,
            0,
            1,
            1.0,
            1.0,
            true,
            None,
        );
        let report = summary.to_report_section();
        assert!(report.contains("保守模式"));
        assert!(report.contains("🔒"));
    }

    #[test]
    fn test_summary_serde_roundtrip() {
        let summary = JointDecisionHistorySummary::new(
            3,
            JointDecisionAction::EscalateWarning,
            15,
            5,
            2,
            0.33,
            0.67,
            false,
            Some("2024-01-01T00:00:00Z".to_string()),
        );
        let json = serde_json::to_string(&summary).unwrap();
        let loaded: JointDecisionHistorySummary = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.session_count, 3);
        assert_eq!(loaded.latest_action, JointDecisionAction::EscalateWarning);
    }

    // ======================================================================
    //  纯函数测试 — build_joint_decision_history_entry
    // ======================================================================

    #[test]
    fn test_build_history_entry() {
        let mut engine = JointDecisionEngine::default();
        engine.evaluate(&make_two_disabled());
        engine.evaluate(&make_all_disabled());

        let entry = build_joint_decision_history_entry(&engine, Utc::now());
        assert_eq!(entry.session_index, 1);
        assert_eq!(entry.decision_count, 2);
        assert_eq!(entry.escalate_count, 1);
        assert_eq!(entry.conservative_mode_count, 1);
        assert!(entry.final_conservative_mode);
        assert_eq!(entry.action, JointDecisionAction::EnterConservativeMode);
    }

    #[test]
    fn test_build_history_entry_empty_engine() {
        let engine = JointDecisionEngine::default();
        let entry = build_joint_decision_history_entry(&engine, Utc::now());
        assert_eq!(entry.session_index, 1);
        assert_eq!(entry.decision_count, 0);
        assert_eq!(entry.action, JointDecisionAction::NoAction);
        assert!(!entry.final_conservative_mode);
    }

    // ======================================================================
    //  纯函数测试 — build_joint_decision_history_summary
    // ======================================================================

    #[test]
    fn test_build_summary_empty_history() {
        let history = JointDecisionHistory::new();
        let summary = build_joint_decision_history_summary(&history);
        assert!(summary.is_empty());
    }

    #[test]
    fn test_build_summary_with_data() {
        let mut history = JointDecisionHistory::new();
        history.add_entry(JointDecisionHistoryEntry::new(
            1,
            Utc::now(),
            JointDecisionAction::EscalateWarning,
            5,
            2,
            1,
            false,
            vec![],
        ));
        history.add_entry(JointDecisionHistoryEntry::new(
            2,
            Utc::now(),
            JointDecisionAction::EnterConservativeMode,
            3,
            0,
            1,
            true,
            vec![],
        ));

        let summary = build_joint_decision_history_summary(&history);
        assert_eq!(summary.session_count, 2);
        assert_eq!(summary.total_decisions, 8);
        assert_eq!(summary.total_escalations, 2);
        assert_eq!(summary.total_conservative_modes, 2);
        assert_eq!(
            summary.latest_action,
            JointDecisionAction::EnterConservativeMode
        );
        assert!(summary.current_conservative_mode);
        assert!((summary.conservative_mode_rate - 0.5).abs() < 0.001);
    }

    // ======================================================================
    //  纯函数测试 — compute_decision_trend
    // ======================================================================

    #[test]
    fn test_decision_trend_insufficient() {
        let history = JointDecisionHistory::new();
        assert_eq!(compute_decision_trend(&history), ScoreTrend::Insufficient);

        let mut history = JointDecisionHistory::new();
        history.add_entry(JointDecisionHistoryEntry::new(
            1,
            Utc::now(),
            JointDecisionAction::NoAction,
            1,
            0,
            0,
            false,
            vec![],
        ));
        assert_eq!(compute_decision_trend(&history), ScoreTrend::Insufficient);
    }

    #[test]
    fn test_decision_trend_improving() {
        let mut history = JointDecisionHistory::new();
        // Early sessions: high escalation
        for i in 0..4 {
            history.add_entry(JointDecisionHistoryEntry::new(
                i + 1,
                Utc::now(),
                JointDecisionAction::EscalateWarning,
                5,
                3,
                1,
                false,
                vec![],
            ));
        }
        // Late sessions: low escalation
        for i in 4..8 {
            history.add_entry(JointDecisionHistoryEntry::new(
                i + 1,
                Utc::now(),
                JointDecisionAction::NoAction,
                5,
                0,
                0,
                false,
                vec![],
            ));
        }
        assert_eq!(compute_decision_trend(&history), ScoreTrend::Improving);
    }

    #[test]
    fn test_decision_trend_declining() {
        let mut history = JointDecisionHistory::new();
        // Early sessions: no escalation
        for i in 0..4 {
            history.add_entry(JointDecisionHistoryEntry::new(
                i + 1,
                Utc::now(),
                JointDecisionAction::NoAction,
                5,
                0,
                0,
                false,
                vec![],
            ));
        }
        // Late sessions: high escalation
        for i in 4..8 {
            history.add_entry(JointDecisionHistoryEntry::new(
                i + 1,
                Utc::now(),
                JointDecisionAction::EscalateWarning,
                5,
                3,
                1,
                false,
                vec![],
            ));
        }
        assert_eq!(compute_decision_trend(&history), ScoreTrend::Declining);
    }

    #[test]
    fn test_decision_trend_stable() {
        let mut history = JointDecisionHistory::new();
        for i in 0..8 {
            history.add_entry(JointDecisionHistoryEntry::new(
                i + 1,
                Utc::now(),
                JointDecisionAction::NoAction,
                5,
                1,
                0,
                false,
                vec![],
            ));
        }
        assert_eq!(compute_decision_trend(&history), ScoreTrend::Stable);
    }

    // ======================================================================
    //  集成场景测试
    // ======================================================================

    #[test]
    fn test_integration_full_workflow() {
        let config = JointDecisionConfig::default().with_re_enable_after_rounds(3);
        let mut engine = JointDecisionEngine::new(config);

        // Round 1: All enabled → NoAction
        let d1 = engine.evaluate(&make_all_enabled());
        assert_eq!(d1.action, JointDecisionAction::NoAction);

        // Round 2: Two disabled → EscalateWarning
        let d2 = engine.evaluate(&make_two_disabled());
        assert_eq!(d2.action, JointDecisionAction::EscalateWarning);

        // Round 3: All disabled → EnterConservativeMode
        let d3 = engine.evaluate(&make_all_disabled());
        assert_eq!(d3.action, JointDecisionAction::EnterConservativeMode);
        assert!(engine.is_conservative_mode());

        // Round 4: Still all disabled, conservative_rounds=2
        let d4 = engine.evaluate(&make_all_disabled());
        assert_eq!(d4.action, JointDecisionAction::EnterConservativeMode);
        assert_eq!(engine.conservative_rounds, 2);

        // Round 5: Still all disabled, conservative_rounds=3 → ReEnableFeature
        let d5 = engine.evaluate(&make_all_disabled());
        assert_eq!(
            d5.action,
            JointDecisionAction::ReEnableFeature {
                evaluator_type: EvaluatorType::MemoryContext
            }
        );
        assert!(!engine.is_conservative_mode());

        // Finalize session
        engine.finalize_session(Utc::now());

        // Verify history
        assert_eq!(engine.history.session_count(), 1);
        let entry = engine.history.latest().unwrap();
        assert_eq!(entry.decision_count, 5);
        assert_eq!(entry.escalate_count, 1);
        assert_eq!(entry.conservative_mode_count, 2);
        assert!(!entry.final_conservative_mode); // Exited after ReEnable
    }

    #[test]
    fn test_integration_no_action_when_below_thresholds() {
        let mut engine = JointDecisionEngine::default();

        // Only 1 disabled → NoAction (escalate threshold = 2)
        for _ in 0..5 {
            let d = engine.evaluate(&make_one_disabled());
            assert_eq!(d.action, JointDecisionAction::NoAction);
        }

        assert_eq!(engine.decision_count(), 5);
        assert_eq!(engine.escalate_count(), 0);
        assert_eq!(engine.conservative_mode_count(), 0);
    }

    #[test]
    fn test_integration_engine_persistence() {
        let dir = tempfile::tempdir().unwrap();

        // Session 1: Create and save
        {
            let mut engine = JointDecisionEngine::default();
            engine.evaluate(&make_two_disabled());
            engine.evaluate(&make_all_disabled());
            engine.finalize_session(Utc::now());
            engine.save_history_to_workspace(dir.path()).unwrap();
        }

        // Session 2: Load and verify
        {
            let mut engine = JointDecisionEngine::default();
            engine.load_history_from_workspace(dir.path());
            assert_eq!(engine.history.session_count(), 1);
            assert_eq!(engine.history.total_escalations(), 1);
            assert_eq!(engine.history.total_conservative_modes(), 1);

            // Add new decisions
            engine.evaluate(&make_all_enabled());
            engine.finalize_session(Utc::now());
            engine.save_history_to_workspace(dir.path()).unwrap();
        }

        // Session 3: Verify accumulated
        {
            let mut engine = JointDecisionEngine::default();
            engine.load_history_from_workspace(dir.path());
            assert_eq!(engine.history.session_count(), 2);
        }
    }
}
