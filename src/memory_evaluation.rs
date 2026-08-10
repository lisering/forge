//! Memory 上下文注入效果评估 — 评估 Memory 上下文注入对修复成功率的影响 (Session 90)
//!
//! 类似 [`search_quality`] 模块, 本模块比较:
//! - **有 Memory 注入** 的修复 (with memory) 编译通过率
//! - **无 Memory 注入** 的修复 (without memory) 编译通过率
//!
//! 当注入有害 (修复成功率反而下降) 时, 自动禁用 Memory 上下文注入。
//!
//! # 设计
//!
//! 在 Orchestrator 的修复流程中:
//! 1. 修复轮次 → `send_attempt_prompt` 注入 Memory 上下文 → 记录 `MemoryInjection` trace
//! 2. AI 修复 → 编译检查 → 记录 `CompileCheck` trace (success=true/false)
//! 3. `evaluate_memory_context` 评估 → 记录 `MemoryEvaluation` trace
//!
//! # 核心类型
//!
//! - [`MemoryEvaluationConfig`] — 评估配置 (阈值)
//! - [`MemoryEvaluationDecision`] — 评估决策
//! - [`MemoryContextEvaluator`] — 评估器 (可自动禁用注入)
//! - [`MemoryEvaluationHistory`] — 跨 session 持久化

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tracing::{debug, info, warn};

// ============================================================================
//  常量
// ============================================================================

/// 历史文件名 — 存储在 `.forge/` 目录下
pub const MEMORY_EVALUATION_HISTORY_FILENAME: &str = "memory_evaluation_history.json";

/// 默认最小样本数 — 低于此数不生成 Disable 决策
pub const DEFAULT_MIN_SAMPLES: usize = 5;

/// 默认禁用阈值 — 差值低于此值时禁用注入 (负值 = 注入有害)
pub const DEFAULT_DISABLE_THRESHOLD: f64 = -0.10;

/// 默认有益阈值 — 差值高于此值时明确保持注入
pub const DEFAULT_BENEFICIAL_THRESHOLD: f64 = 0.05;

// ============================================================================
//  MemoryEvaluationConfig — 评估配置
// ============================================================================

/// Memory 上下文注入效果评估配置
///
/// 控制评估器何时禁用注入、何时保持注入。
///
/// # 字段
///
/// | 字段 | 说明 | 默认值 |
/// |------|------|--------|
/// | `min_samples` | 最小样本数 | 5 |
/// | `disable_threshold` | 禁用阈值 (差值 < 此值时禁用) | -0.10 |
/// | `beneficial_threshold` | 有益阈值 (差值 >= 此值时保持) | 0.05 |
///
/// # 示例
///
/// ```
/// use forge::memory_evaluation::MemoryEvaluationConfig;
///
/// let config = MemoryEvaluationConfig::default();
/// assert_eq!(config.min_samples, 5);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEvaluationConfig {
    /// 最小样本数 — 低于此数不生成 Disable 决策
    pub min_samples: usize,
    /// 禁用阈值 — 差值 < 此值时禁用注入 (负值 = 注入有害)
    pub disable_threshold: f64,
    /// 有益阈值 — 差值 >= 此值时明确保持注入
    pub beneficial_threshold: f64,
}

impl MemoryEvaluationConfig {
    /// 创建默认配置
    pub fn new() -> Self {
        Self {
            min_samples: DEFAULT_MIN_SAMPLES,
            disable_threshold: DEFAULT_DISABLE_THRESHOLD,
            beneficial_threshold: DEFAULT_BENEFICIAL_THRESHOLD,
        }
    }

    /// 严格配置 — 更少样本即可决策, 阈值更紧
    pub fn strict() -> Self {
        Self {
            min_samples: 3,
            disable_threshold: -0.05,
            beneficial_threshold: 0.10,
        }
    }

    /// 宽松配置 — 需要更多样本, 阈值更松
    pub fn lenient() -> Self {
        Self {
            min_samples: 8,
            disable_threshold: -0.20,
            beneficial_threshold: 0.02,
        }
    }
}

impl Default for MemoryEvaluationConfig {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
//  MemoryEvaluationAction — 评估动作
// ============================================================================

/// Memory 上下文注入评估动作
///
/// # 变体
///
/// - `KeepInjecting` — 保持 Memory 注入 (注入有效或中性)
/// - `DisableInjection` — 禁用 Memory 注入 (注入有害)
/// - `InsufficientData` — 数据不足, 无法决策
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryEvaluationAction {
    /// 保持 Memory 注入
    KeepInjecting,
    /// 禁用 Memory 注入
    DisableInjection,
    /// 数据不足
    InsufficientData,
}

impl std::fmt::Display for MemoryEvaluationAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::KeepInjecting => write!(f, "KeepInjecting"),
            Self::DisableInjection => write!(f, "DisableInjection"),
            Self::InsufficientData => write!(f, "InsufficientData"),
        }
    }
}

// ============================================================================
//  MemoryEvaluationDecision — 评估决策
// ============================================================================

/// Memory 上下文注入评估决策
///
/// 包含决策动作、修复成功率差值和原因。
///
/// # 字段
///
/// - `action`: 评估动作
/// - `diff`: 有注入修复率 - 无注入修复率 (正=有效, 负=有害)
/// - `reason`: 决策原因
///
/// # 示例
///
/// ```
/// use forge::memory_evaluation::{MemoryEvaluationAction, MemoryEvaluationDecision};
///
/// let decision = MemoryEvaluationDecision::keep_injecting(0.15, "注入有效");
/// assert!(decision.is_keep());
/// assert!((decision.diff - 0.15).abs() < 0.001);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEvaluationDecision {
    /// 评估动作
    pub action: MemoryEvaluationAction,
    /// 修复成功率差值 (有注入 - 无注入)
    pub diff: f64,
    /// 决策原因
    pub reason: String,
}

impl MemoryEvaluationDecision {
    /// 创建 "保持注入" 决策
    pub fn keep_injecting(diff: f64, reason: &str) -> Self {
        Self {
            action: MemoryEvaluationAction::KeepInjecting,
            diff,
            reason: reason.to_string(),
        }
    }

    /// 创建 "禁用注入" 决策
    pub fn disable_injection(diff: f64, reason: &str) -> Self {
        Self {
            action: MemoryEvaluationAction::DisableInjection,
            diff,
            reason: reason.to_string(),
        }
    }

    /// 创建 "数据不足" 决策
    pub fn insufficient_data(diff: f64, reason: &str) -> Self {
        Self {
            action: MemoryEvaluationAction::InsufficientData,
            diff,
            reason: reason.to_string(),
        }
    }

    /// 是否为 "保持注入" 决策
    pub fn is_keep(&self) -> bool {
        self.action == MemoryEvaluationAction::KeepInjecting
    }

    /// 是否为 "禁用注入" 决策
    pub fn is_disable(&self) -> bool {
        self.action == MemoryEvaluationAction::DisableInjection
    }

    /// 是否为 "数据不足" 决策
    pub fn is_insufficient_data(&self) -> bool {
        self.action == MemoryEvaluationAction::InsufficientData
    }

    /// 格式化为 trace 摘要 (简短一行)
    pub fn to_trace_summary(&self) -> String {
        format!(
            "Memory 评估: {} (差值 {:+.1}%, {})",
            self.action,
            self.diff * 100.0,
            self.reason
        )
    }
}

// ============================================================================
//  纯函数 — 决策计算
// ============================================================================

/// 检查是否有足够的评估数据
///
/// 需要同时有 with_memory 和 without_memory 的数据, 且总数 >= min_samples。
///
/// # 参数
///
/// - `checks_with`: 有注入的编译检查次数
/// - `checks_without`: 无注入的编译检查次数
/// - `min_samples`: 最小样本数
///
/// # 示例
///
/// ```
/// use forge::memory_evaluation::has_sufficient_evaluation_data;
///
/// assert!(!has_sufficient_evaluation_data(2, 2, 5)); // 总数 4 < 5
/// assert!(has_sufficient_evaluation_data(3, 3, 5));  // 总数 6 >= 5
/// ```
pub fn has_sufficient_evaluation_data(
    checks_with: usize,
    checks_without: usize,
    min_samples: usize,
) -> bool {
    checks_with > 0 && checks_without > 0 && checks_with + checks_without >= min_samples
}

/// 检查是否应禁用注入
///
/// 当差值低于禁用阈值时返回 true。
///
/// # 参数
///
/// - `diff`: 修复成功率差值 (有注入 - 无注入)
/// - `disable_threshold`: 禁用阈值
///
/// # 示例
///
/// ```
/// use forge::memory_evaluation::should_disable_injection;
///
/// assert!(should_disable_injection(-0.15, -0.10));  // -15% < -10% → 禁用
/// assert!(!should_disable_injection(0.05, -0.10));  // +5% > -10% → 不禁用
/// ```
pub fn should_disable_injection(diff: f64, disable_threshold: f64) -> bool {
    diff < disable_threshold
}

/// 计算 Memory 上下文注入效果评估决策 (纯函数)
///
/// 基于 `MemoryEvaluationStats` 数据和配置, 生成评估决策。
///
/// # 决策逻辑
///
/// 1. 数据不足 → `InsufficientData`
/// 2. 差值 < disable_threshold → `DisableInjection` (注入有害)
/// 3. 差值 >= beneficial_threshold → `KeepInjecting` (注入有效)
/// 4. 否则 → `KeepInjecting` (中性, 偏向保持)
///
/// # 参数
///
/// - `checks_with`: 有注入的编译检查次数
/// - `checks_without`: 无注入的编译检查次数
/// - `successes_with`: 有注入的编译通过次数
/// - `successes_without`: 无注入的编译通过次数
/// - `config`: 评估配置
///
/// # 示例
///
/// ```
/// use forge::memory_evaluation::{compute_memory_evaluation_decision, MemoryEvaluationConfig};
///
/// // 注入有害: with 1/5=20%, without 4/5=80%, diff=-60%
/// let decision = compute_memory_evaluation_decision(5, 5, 1, 4, &MemoryEvaluationConfig::default());
/// assert!(decision.is_disable());
/// ```
pub fn compute_memory_evaluation_decision(
    checks_with: usize,
    checks_without: usize,
    successes_with: usize,
    successes_without: usize,
    config: &MemoryEvaluationConfig,
) -> MemoryEvaluationDecision {
    // 数据不足
    if !has_sufficient_evaluation_data(checks_with, checks_without, config.min_samples) {
        return MemoryEvaluationDecision::insufficient_data(
            0.0,
            &format!(
                "样本不足: with={}, without={}, 需要 {} 个",
                checks_with, checks_without, config.min_samples
            ),
        );
    }

    let with_rate = successes_with as f64 / checks_with as f64;
    let without_rate = successes_without as f64 / checks_without as f64;
    let diff = with_rate - without_rate;

    // 注入有害
    if should_disable_injection(diff, config.disable_threshold) {
        return MemoryEvaluationDecision::disable_injection(
            diff,
            &format!(
                "注入有害: 有注入修复率 {:.1}% < 无注入修复率 {:.1}%",
                with_rate * 100.0,
                without_rate * 100.0
            ),
        );
    }

    // 注入有效
    if diff >= config.beneficial_threshold {
        return MemoryEvaluationDecision::keep_injecting(
            diff,
            &format!(
                "注入有效: 有注入修复率 {:.1}% > 无注入修复率 {:.1}%",
                with_rate * 100.0,
                without_rate * 100.0
            ),
        );
    }

    // 中性 — 偏向保持
    MemoryEvaluationDecision::keep_injecting(
        diff,
        &format!(
            "注入中性: 有注入修复率 {:.1}% ≈ 无注入修复率 {:.1}%",
            with_rate * 100.0,
            without_rate * 100.0
        ),
    )
}

// ============================================================================
//  MemoryContextEvaluator — 评估器
// ============================================================================

/// Memory 上下文注入效果评估器
///
/// 在每次编译检查后评估 Memory 上下文注入的效果,
/// 当注入有害时自动禁用注入功能。
///
/// # 字段
///
/// - `config`: 评估配置
/// - `enabled`: 注入是否启用
/// - `initial_enabled`: session 开始时的启用状态
/// - `evaluation_count`: 累计评估次数
/// - `disable_count`: 累计禁用次数
/// - `last_decision`: 最近一次评估决策
///
/// # 用法
///
/// ```
/// use forge::memory_evaluation::{MemoryContextEvaluator, MemoryEvaluationConfig};
///
/// let mut evaluator = MemoryContextEvaluator::new(MemoryEvaluationConfig::default());
///
/// // 注入有效: with 3/3=100%, without 0/3=0%, diff=+100%
/// let decision = evaluator.evaluate_and_apply(3, 3, 0, 3);
/// assert!(evaluator.is_enabled()); // 保持启用
/// ```
#[derive(Debug, Clone)]
pub struct MemoryContextEvaluator {
    /// 评估配置
    config: MemoryEvaluationConfig,
    /// 注入是否启用
    enabled: bool,
    /// session 开始时的启用状态
    initial_enabled: bool,
    /// 累计评估次数
    evaluation_count: u32,
    /// 累计禁用次数
    disable_count: u32,
    /// 最近一次评估决策
    last_decision: Option<MemoryEvaluationDecision>,
}

impl MemoryContextEvaluator {
    /// 创建新的评估器
    pub fn new(config: MemoryEvaluationConfig) -> Self {
        Self {
            config,
            enabled: true,
            initial_enabled: true,
            evaluation_count: 0,
            disable_count: 0,
            last_decision: None,
        }
    }

    /// 使用默认配置创建评估器
    pub fn with_default_config() -> Self {
        Self::new(MemoryEvaluationConfig::default())
    }

    /// 评估当前注入效果, 生成决策 (不修改状态)
    ///
    /// # 参数
    ///
    /// - `checks_with`: 有注入的编译检查次数
    /// - `successes_with`: 有注入的编译通过次数
    /// - `checks_without`: 无注入的编译检查次数
    /// - `successes_without`: 无注入的编译通过次数
    pub fn evaluate(
        &self,
        checks_with: usize,
        successes_with: usize,
        checks_without: usize,
        successes_without: usize,
    ) -> MemoryEvaluationDecision {
        if !self.enabled {
            let diff = if checks_with > 0 && checks_without > 0 {
                successes_with as f64 / checks_with as f64
                    - successes_without as f64 / checks_without as f64
            } else {
                0.0
            };
            return MemoryEvaluationDecision::keep_injecting(diff, "注入已禁用");
        }
        compute_memory_evaluation_decision(
            checks_with,
            checks_without,
            successes_with,
            successes_without,
            &self.config,
        )
    }

    /// 应用评估决策, 更新内部状态
    pub fn apply_decision(&mut self, decision: &MemoryEvaluationDecision) {
        match &decision.action {
            MemoryEvaluationAction::KeepInjecting | MemoryEvaluationAction::InsufficientData => {}
            MemoryEvaluationAction::DisableInjection => {
                self.enabled = false;
                self.disable_count += 1;
            }
        }
        self.last_decision = Some(decision.clone());
    }

    /// 一步评估并应用 — 等价于 `evaluate` + `apply_decision`
    ///
    /// 返回生成的评估决策, 同时递增 `evaluation_count`。
    pub fn evaluate_and_apply(
        &mut self,
        checks_with: usize,
        successes_with: usize,
        checks_without: usize,
        successes_without: usize,
    ) -> MemoryEvaluationDecision {
        let decision = self.evaluate(
            checks_with,
            successes_with,
            checks_without,
            successes_without,
        );
        self.apply_decision(&decision);
        self.evaluation_count += 1;
        decision
    }

    /// 重新启用注入
    pub fn re_enable(&mut self) {
        self.enabled = true;
    }

    /// 获取评估配置引用
    pub fn config(&self) -> &MemoryEvaluationConfig {
        &self.config
    }

    /// 注入是否启用
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// session 开始时的启用状态
    pub fn initial_enabled(&self) -> bool {
        self.initial_enabled
    }

    /// 累计评估次数
    pub fn evaluation_count(&self) -> u32 {
        self.evaluation_count
    }

    /// 累计禁用次数
    pub fn disable_count(&self) -> u32 {
        self.disable_count
    }

    /// 最近一次评估决策
    pub fn last_decision(&self) -> Option<&MemoryEvaluationDecision> {
        self.last_decision.as_ref()
    }

    /// 格式化为可读摘要
    pub fn to_summary(&self) -> String {
        format!(
            "Memory 评估器: 启用={}, 评估 {} 次, 禁用 {} 次, 最近决策: {}",
            self.enabled,
            self.evaluation_count,
            self.disable_count,
            self.last_decision
                .as_ref()
                .map(|d| d.to_trace_summary())
                .unwrap_or_else(|| "无".to_string())
        )
    }

    // ===== 持久化方法 (Session 90) =====

    /// 导出为历史记录 (用于持久化)
    ///
    /// # 示例
    ///
    /// ```
    /// use forge::memory_evaluation::MemoryContextEvaluator;
    ///
    /// let evaluator = MemoryContextEvaluator::with_default_config();
    /// let history = evaluator.to_history();
    /// assert!(history.initial_enabled);
    /// ```
    pub fn to_history(&self) -> MemoryEvaluationHistory {
        MemoryEvaluationHistory {
            initial_enabled: self.initial_enabled,
            current_enabled: self.enabled,
            evaluation_count: self.evaluation_count,
            disable_count: self.disable_count,
            last_decision: self.last_decision.clone(),
            saved_at: None,
        }
    }

    /// 从历史记录恢复评估器状态
    ///
    /// # 参数
    ///
    /// - `history`: 历史记录
    /// - `config`: 评估配置 (历史不保存配置)
    ///
    /// # 示例
    ///
    /// ```
    /// use forge::memory_evaluation::{
    ///     MemoryContextEvaluator, MemoryEvaluationConfig, MemoryEvaluationHistory,
    /// };
    ///
    /// let mut history = MemoryEvaluationHistory::new();
    /// history.current_enabled = false;
    /// history.evaluation_count = 3;
    ///
    /// let evaluator = MemoryContextEvaluator::from_history(history, MemoryEvaluationConfig::default());
    /// assert!(!evaluator.is_enabled());
    /// assert_eq!(evaluator.evaluation_count(), 3);
    /// ```
    pub fn from_history(history: MemoryEvaluationHistory, config: MemoryEvaluationConfig) -> Self {
        Self {
            config,
            enabled: history.current_enabled,
            initial_enabled: history.current_enabled,
            evaluation_count: history.evaluation_count,
            disable_count: history.disable_count,
            last_decision: history.last_decision,
        }
    }

    /// 保存到工作区 (`.forge/memory_evaluation_history.json`)
    pub fn save_to_workspace(&self, workspace_root: &Path) -> Result<()> {
        let history = self.to_history().with_timestamp();
        history.save_to_workspace(workspace_root)
    }

    /// 从工作区加载并恢复评估器
    ///
    /// 文件不存在时返回 `None`。
    pub fn load_from_workspace(
        workspace_root: &Path,
        config: MemoryEvaluationConfig,
    ) -> Option<Self> {
        match MemoryEvaluationHistory::load_from_workspace(workspace_root) {
            Some(history) => {
                info!(
                    "📥 加载 Memory 评估历史: 启用={}, 评估 {} 次, 禁用 {} 次",
                    history.current_enabled, history.evaluation_count, history.disable_count
                );
                Some(Self::from_history(history, config))
            }
            None => {
                debug!("无 Memory 评估历史文件, 使用默认配置");
                None
            }
        }
    }
}

// ============================================================================
//  MemoryEvaluationHistory — 跨 session 持久化
// ============================================================================

/// Memory 评估历史 — 跨 session 持久化评估状态
///
/// 存储在 `.forge/memory_evaluation_history.json`。
///
/// # 字段
///
/// | 字段 | 说明 |
/// |------|------|
/// | `initial_enabled` | session 开始时的启用状态 |
/// | `current_enabled` | 最终启用状态 |
/// | `evaluation_count` | 累计评估次数 |
/// | `disable_count` | 累计禁用次数 |
/// | `last_decision` | 最近一次评估决策 |
/// | `saved_at` | 保存时间 (ISO 8601) |
///
/// # 示例
///
/// ```
/// use forge::memory_evaluation::{MemoryContextEvaluator, MemoryEvaluationHistory};
///
/// let evaluator = MemoryContextEvaluator::with_default_config();
/// let history = evaluator.to_history();
/// assert!(history.is_empty());
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEvaluationHistory {
    /// session 开始时的启用状态
    pub initial_enabled: bool,
    /// 最终启用状态
    pub current_enabled: bool,
    /// 累计评估次数
    pub evaluation_count: u32,
    /// 累计禁用次数
    pub disable_count: u32,
    /// 最近一次评估决策
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_decision: Option<MemoryEvaluationDecision>,
    /// 保存时间 (ISO 8601)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved_at: Option<String>,
}

impl MemoryEvaluationHistory {
    /// 创建空的历史记录
    pub fn new() -> Self {
        Self {
            initial_enabled: true,
            current_enabled: true,
            evaluation_count: 0,
            disable_count: 0,
            last_decision: None,
            saved_at: None,
        }
    }

    /// 是否为空 (无评估记录)
    pub fn is_empty(&self) -> bool {
        self.evaluation_count == 0
    }

    /// 启用状态是否发生变化
    pub fn enabled_changed(&self) -> bool {
        self.initial_enabled != self.current_enabled
    }

    /// 格式化为可读摘要
    pub fn to_summary(&self) -> String {
        let status = if self.current_enabled {
            "启用"
        } else {
            "禁用"
        };
        let changed = if self.enabled_changed() {
            if self.current_enabled {
                " (本 session 重新启用)"
            } else {
                " (本 session 已禁用)"
            }
        } else {
            ""
        };
        format!(
            "Memory 评估历史: 状态={}{}, 评估 {} 次, 禁用 {} 次, 最近决策: {}",
            status,
            changed,
            self.evaluation_count,
            self.disable_count,
            self.last_decision
                .as_ref()
                .map(|d| d.to_trace_summary())
                .unwrap_or_else(|| "无".to_string())
        )
    }

    /// 从文件加载
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Err(anyhow!("Memory 评估历史文件不存在: {}", path.display()));
        }
        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow!("读取 Memory 评估历史失败: {}", e))?;
        let history: MemoryEvaluationHistory = serde_json::from_str(&content)
            .map_err(|e| anyhow!("解析 Memory 评估历史 JSON 失败: {}", e))?;
        Ok(history)
    }

    /// 保存到文件
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(self)
            .map_err(|e| anyhow!("序列化 Memory 评估历史失败: {}", e))?;
        std::fs::write(path, content).map_err(|e| anyhow!("写入 Memory 评估历史失败: {}", e))?;
        Ok(())
    }

    /// 从工作区加载 (`.forge/memory_evaluation_history.json`)
    pub fn load_from_workspace(workspace_root: &Path) -> Option<Self> {
        let path = workspace_root
            .join(".forge")
            .join(MEMORY_EVALUATION_HISTORY_FILENAME);
        match Self::load(&path) {
            Ok(h) => Some(h),
            Err(e) => {
                warn!("加载 Memory 评估历史失败, 使用默认配置: {}", e);
                None
            }
        }
    }

    /// 保存到工作区
    pub fn save_to_workspace(&self, workspace_root: &Path) -> Result<()> {
        let path = workspace_root
            .join(".forge")
            .join(MEMORY_EVALUATION_HISTORY_FILENAME);
        self.save(&path)
    }

    /// 创建带时间戳的副本
    pub fn with_timestamp(mut self) -> Self {
        self.saved_at = Some(chrono::Utc::now().to_rfc3339());
        self
    }
}

impl Default for MemoryEvaluationHistory {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ===== MemoryEvaluationConfig 测试 =====

    #[test]
    fn test_config_default() {
        let config = MemoryEvaluationConfig::default();
        assert_eq!(config.min_samples, 5);
        assert!((config.disable_threshold - (-0.10)).abs() < 0.001);
        assert!((config.beneficial_threshold - 0.05).abs() < 0.001);
    }

    #[test]
    fn test_config_strict() {
        let config = MemoryEvaluationConfig::strict();
        assert_eq!(config.min_samples, 3);
        assert!((config.disable_threshold - (-0.05)).abs() < 0.001);
        assert!((config.beneficial_threshold - 0.10).abs() < 0.001);
    }

    #[test]
    fn test_config_lenient() {
        let config = MemoryEvaluationConfig::lenient();
        assert_eq!(config.min_samples, 8);
        assert!((config.disable_threshold - (-0.20)).abs() < 0.001);
        assert!((config.beneficial_threshold - 0.02).abs() < 0.001);
    }

    #[test]
    fn test_config_serde_roundtrip() {
        let config = MemoryEvaluationConfig::strict();
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: MemoryEvaluationConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.min_samples, config.min_samples);
    }

    // ===== MemoryEvaluationAction 测试 =====

    #[test]
    fn test_action_display() {
        assert_eq!(
            MemoryEvaluationAction::KeepInjecting.to_string(),
            "KeepInjecting"
        );
        assert_eq!(
            MemoryEvaluationAction::DisableInjection.to_string(),
            "DisableInjection"
        );
        assert_eq!(
            MemoryEvaluationAction::InsufficientData.to_string(),
            "InsufficientData"
        );
    }

    #[test]
    fn test_action_serde_roundtrip() {
        let json = serde_json::to_string(&MemoryEvaluationAction::DisableInjection).unwrap();
        let deserialized: MemoryEvaluationAction = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, MemoryEvaluationAction::DisableInjection);
    }

    // ===== MemoryEvaluationDecision 测试 =====

    #[test]
    fn test_decision_keep() {
        let d = MemoryEvaluationDecision::keep_injecting(0.15, "有效");
        assert!(d.is_keep());
        assert!(!d.is_disable());
        assert!((d.diff - 0.15).abs() < 0.001);
    }

    #[test]
    fn test_decision_disable() {
        let d = MemoryEvaluationDecision::disable_injection(-0.2, "有害");
        assert!(d.is_disable());
        assert!(!d.is_keep());
        assert!((d.diff - (-0.2)).abs() < 0.001);
    }

    #[test]
    fn test_decision_insufficient() {
        let d = MemoryEvaluationDecision::insufficient_data(0.0, "不足");
        assert!(d.is_insufficient_data());
        assert!(!d.is_keep());
        assert!(!d.is_disable());
    }

    #[test]
    fn test_decision_to_trace_summary() {
        let d = MemoryEvaluationDecision::keep_injecting(0.15, "注入有效");
        let summary = d.to_trace_summary();
        assert!(summary.contains("KeepInjecting"));
        assert!(summary.contains("+15.0%"));
        assert!(summary.contains("注入有效"));
    }

    #[test]
    fn test_decision_serde_roundtrip() {
        let d = MemoryEvaluationDecision::disable_injection(-0.15, "有害");
        let json = serde_json::to_string(&d).unwrap();
        let loaded: MemoryEvaluationDecision = serde_json::from_str(&json).unwrap();
        assert!(loaded.is_disable());
        assert!((loaded.diff - (-0.15)).abs() < 0.001);
    }

    // ===== 纯函数测试 =====

    #[test]
    fn test_has_sufficient_data() {
        assert!(!has_sufficient_evaluation_data(0, 5, 5)); // with=0
        assert!(!has_sufficient_evaluation_data(5, 0, 5)); // without=0
        assert!(!has_sufficient_evaluation_data(2, 2, 5)); // total=4 < 5
        assert!(has_sufficient_evaluation_data(3, 3, 5)); // total=6 >= 5
        assert!(has_sufficient_evaluation_data(5, 5, 5)); // total=10 >= 5
    }

    #[test]
    fn test_should_disable_injection() {
        assert!(should_disable_injection(-0.15, -0.10)); // -15% < -10%
        assert!(should_disable_injection(-0.50, -0.10)); // -50% < -10%
        assert!(!should_disable_injection(0.05, -0.10)); // +5% > -10%
        assert!(!should_disable_injection(-0.05, -0.10)); // -5% > -10%
        assert!(!should_disable_injection(0.0, -0.10)); // 0% > -10%
    }

    #[test]
    fn test_compute_decision_insufficient_data() {
        let config = MemoryEvaluationConfig::default();
        let d = compute_memory_evaluation_decision(2, 1, 2, 1, &config);
        assert!(d.is_insufficient_data());
    }

    #[test]
    fn test_compute_decision_disable() {
        let config = MemoryEvaluationConfig::default();
        // with: 1/5=20%, without: 4/5=80%, diff=-60%
        let d = compute_memory_evaluation_decision(5, 5, 1, 4, &config);
        assert!(d.is_disable());
        assert!((d.diff - (-0.6)).abs() < 0.001);
    }

    #[test]
    fn test_compute_decision_keep_beneficial() {
        let config = MemoryEvaluationConfig::default();
        // with: 4/5=80%, without: 1/5=20%, diff=+60%
        let d = compute_memory_evaluation_decision(5, 5, 4, 1, &config);
        assert!(d.is_keep());
        assert!((d.diff - 0.6).abs() < 0.001);
    }

    #[test]
    fn test_compute_decision_keep_neutral() {
        let config = MemoryEvaluationConfig::default();
        // with: 3/5=60%, without: 3/5=60%, diff=0%
        let d = compute_memory_evaluation_decision(5, 3, 5, 3, &config);
        assert!(d.is_keep());
        assert!((d.diff - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_compute_decision_strict_config() {
        // strict: min_samples=3, disable_threshold=-0.05
        let config = MemoryEvaluationConfig::strict();
        // with: 1/3=33%, without: 3/3=100%, diff=-67% < -5%
        let d = compute_memory_evaluation_decision(3, 1, 3, 3, &config);
        assert!(d.is_disable());
    }

    #[test]
    fn test_compute_decision_lenient_config() {
        // lenient: min_samples=8, disable_threshold=-0.20
        let config = MemoryEvaluationConfig::lenient();
        // with: 3/8=37.5%, without: 5/8=62.5%, diff=-25% < -20%
        let d = compute_memory_evaluation_decision(8, 8, 3, 5, &config);
        assert!(d.is_disable());
        // diff=-12.5% > -20% → keep
        let d2 = compute_memory_evaluation_decision(8, 8, 4, 5, &config);
        assert!(d2.is_keep());
    }

    // ===== MemoryContextEvaluator 测试 =====

    #[test]
    fn test_evaluator_new() {
        let evaluator = MemoryContextEvaluator::new(MemoryEvaluationConfig::default());
        assert!(evaluator.is_enabled());
        assert_eq!(evaluator.evaluation_count(), 0);
        assert_eq!(evaluator.disable_count(), 0);
        assert!(evaluator.last_decision().is_none());
    }

    #[test]
    fn test_evaluator_with_default_config() {
        let evaluator = MemoryContextEvaluator::with_default_config();
        assert!(evaluator.is_enabled());
    }

    #[test]
    fn test_evaluator_evaluate_no_mutation() {
        let evaluator = MemoryContextEvaluator::new(MemoryEvaluationConfig::default());
        let _decision = evaluator.evaluate(5, 3, 5, 2);
        assert_eq!(evaluator.evaluation_count(), 0);
        assert!(evaluator.last_decision().is_none());
    }

    #[test]
    fn test_evaluator_apply_keep() {
        let mut evaluator = MemoryContextEvaluator::new(MemoryEvaluationConfig::default());
        let decision = MemoryEvaluationDecision::keep_injecting(0.1, "有效");
        evaluator.apply_decision(&decision);
        assert!(evaluator.is_enabled());
        assert_eq!(evaluator.disable_count(), 0);
    }

    #[test]
    fn test_evaluator_apply_disable() {
        let mut evaluator = MemoryContextEvaluator::new(MemoryEvaluationConfig::default());
        let decision = MemoryEvaluationDecision::disable_injection(-0.2, "有害");
        evaluator.apply_decision(&decision);
        assert!(!evaluator.is_enabled());
        assert_eq!(evaluator.disable_count(), 1);
    }

    #[test]
    fn test_evaluator_apply_insufficient() {
        let mut evaluator = MemoryContextEvaluator::new(MemoryEvaluationConfig::default());
        let decision = MemoryEvaluationDecision::insufficient_data(0.0, "不足");
        evaluator.apply_decision(&decision);
        assert!(evaluator.is_enabled());
        assert_eq!(evaluator.disable_count(), 0);
    }

    #[test]
    fn test_evaluator_evaluate_and_apply_keep() {
        let mut evaluator = MemoryContextEvaluator::new(MemoryEvaluationConfig::default());
        // with: 3/3=100%, without: 0/3=0%, diff=+100%
        let decision = evaluator.evaluate_and_apply(3, 3, 3, 0);
        assert!(decision.is_keep());
        assert!(evaluator.is_enabled());
        assert_eq!(evaluator.evaluation_count(), 1);
    }

    #[test]
    fn test_evaluator_evaluate_and_apply_disable() {
        let mut evaluator = MemoryContextEvaluator::new(MemoryEvaluationConfig::default());
        // with: 0/3=0%, without: 3/3=100%, diff=-100%
        let decision = evaluator.evaluate_and_apply(3, 0, 3, 3);
        assert!(decision.is_disable());
        assert!(!evaluator.is_enabled());
        assert_eq!(evaluator.evaluation_count(), 1);
        assert_eq!(evaluator.disable_count(), 1);
    }

    #[test]
    fn test_evaluator_evaluate_and_apply_insufficient() {
        let mut evaluator = MemoryContextEvaluator::new(MemoryEvaluationConfig::default());
        let decision = evaluator.evaluate_and_apply(1, 1, 1, 0);
        assert!(decision.is_insufficient_data());
        assert!(evaluator.is_enabled());
    }

    #[test]
    fn test_evaluator_evaluate_when_disabled() {
        let mut evaluator = MemoryContextEvaluator::new(MemoryEvaluationConfig::default());
        evaluator.apply_decision(&MemoryEvaluationDecision::disable_injection(-0.2, "有害"));
        assert!(!evaluator.is_enabled());

        // 再评估时返回 keep
        let decision = evaluator.evaluate(3, 0, 3, 3);
        assert!(decision.is_keep());
        assert!(decision.reason.contains("已禁用"));
    }

    #[test]
    fn test_evaluator_re_enable() {
        let mut evaluator = MemoryContextEvaluator::new(MemoryEvaluationConfig::default());
        evaluator.apply_decision(&MemoryEvaluationDecision::disable_injection(-0.2, "有害"));
        assert!(!evaluator.is_enabled());
        evaluator.re_enable();
        assert!(evaluator.is_enabled());
    }

    #[test]
    fn test_evaluator_multiple_evaluations() {
        let mut evaluator = MemoryContextEvaluator::new(MemoryEvaluationConfig::default());

        // 第一次: 数据不足
        let d1 = evaluator.evaluate_and_apply(1, 1, 1, 0);
        assert!(d1.is_insufficient_data());

        // 第二次: 注入有害 → 禁用
        let d2 = evaluator.evaluate_and_apply(3, 0, 3, 3);
        assert!(d2.is_disable());
        assert!(!evaluator.is_enabled());

        // 第三次: 已禁用 → keep
        let d3 = evaluator.evaluate_and_apply(3, 0, 3, 3);
        assert!(d3.is_keep());

        assert_eq!(evaluator.evaluation_count(), 3);
        assert_eq!(evaluator.disable_count(), 1);
    }

    #[test]
    fn test_evaluator_to_summary() {
        let mut evaluator = MemoryContextEvaluator::new(MemoryEvaluationConfig::default());
        evaluator.evaluate_and_apply(1, 1, 1, 0);
        let summary = evaluator.to_summary();
        assert!(summary.contains("Memory 评估器"));
        assert!(summary.contains("启用=true"));
    }

    #[test]
    fn test_evaluator_initial_enabled() {
        let evaluator = MemoryContextEvaluator::new(MemoryEvaluationConfig::default());
        assert!(evaluator.initial_enabled());

        let mut e2 = MemoryContextEvaluator::new(MemoryEvaluationConfig::default());
        e2.apply_decision(&MemoryEvaluationDecision::disable_injection(-0.2, "有害"));
        assert!(!e2.is_enabled());
        assert!(e2.initial_enabled());
    }

    // ===== MemoryEvaluationHistory 测试 =====

    #[test]
    fn test_history_new() {
        let h = MemoryEvaluationHistory::new();
        assert!(h.initial_enabled);
        assert!(h.current_enabled);
        assert_eq!(h.evaluation_count, 0);
        assert!(h.is_empty());
    }

    #[test]
    fn test_history_default() {
        let h = MemoryEvaluationHistory::default();
        assert!(h.is_empty());
    }

    #[test]
    fn test_history_enabled_changed() {
        let h = MemoryEvaluationHistory::new();
        assert!(!h.enabled_changed());

        let h2 = MemoryEvaluationHistory {
            initial_enabled: true,
            current_enabled: false,
            ..MemoryEvaluationHistory::new()
        };
        assert!(h2.enabled_changed());
    }

    #[test]
    fn test_history_to_summary_enabled() {
        let h = MemoryEvaluationHistory::new();
        let summary = h.to_summary();
        assert!(summary.contains("状态=启用"));
    }

    #[test]
    fn test_history_to_summary_disabled() {
        let h = MemoryEvaluationHistory {
            initial_enabled: true,
            current_enabled: false,
            evaluation_count: 5,
            disable_count: 1,
            ..MemoryEvaluationHistory::new()
        };
        let summary = h.to_summary();
        assert!(summary.contains("状态=禁用"));
        assert!(summary.contains("本 session 已禁用"));
        assert!(summary.contains("评估 5 次"));
    }

    #[test]
    fn test_history_serde_roundtrip() {
        let h = MemoryEvaluationHistory {
            initial_enabled: true,
            current_enabled: false,
            evaluation_count: 5,
            disable_count: 2,
            last_decision: Some(MemoryEvaluationDecision::disable_injection(-0.15, "有害")),
            saved_at: Some("2026-01-01T00:00:00Z".to_string()),
        };
        let json = serde_json::to_string(&h).unwrap();
        let loaded: MemoryEvaluationHistory = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.initial_enabled, h.initial_enabled);
        assert_eq!(loaded.current_enabled, h.current_enabled);
        assert_eq!(loaded.evaluation_count, h.evaluation_count);
        assert_eq!(loaded.disable_count, h.disable_count);
        assert_eq!(loaded.saved_at, h.saved_at);
    }

    #[test]
    fn test_history_serde_skip_none() {
        let h = MemoryEvaluationHistory::new();
        let json = serde_json::to_string(&h).unwrap();
        assert!(!json.contains("last_decision"));
        assert!(!json.contains("saved_at"));
    }

    #[test]
    fn test_history_with_timestamp() {
        let h = MemoryEvaluationHistory::new().with_timestamp();
        assert!(h.saved_at.is_some());
        assert!(h.saved_at.as_ref().unwrap().contains('T'));
    }

    #[test]
    fn test_history_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.json");

        let h = MemoryEvaluationHistory {
            initial_enabled: true,
            current_enabled: false,
            evaluation_count: 3,
            disable_count: 1,
            last_decision: Some(MemoryEvaluationDecision::disable_injection(-0.2, "有害")),
            saved_at: None,
        };
        h.save(&path).unwrap();
        assert!(path.exists());

        let loaded = MemoryEvaluationHistory::load(&path).unwrap();
        assert!(loaded.initial_enabled);
        assert!(!loaded.current_enabled);
        assert_eq!(loaded.evaluation_count, 3);
    }

    #[test]
    fn test_history_load_nonexistent() {
        let path = std::path::Path::new("/nonexistent/memory_eval_history.json");
        let result = MemoryEvaluationHistory::load(path);
        assert!(result.is_err());
    }

    #[test]
    fn test_history_save_creates_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("subdir").join("history.json");

        MemoryEvaluationHistory::new().save(&path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn test_history_load_from_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let forge_dir = dir.path().join(".forge");
        std::fs::create_dir_all(&forge_dir).unwrap();
        let path = forge_dir.join(MEMORY_EVALUATION_HISTORY_FILENAME);

        let h = MemoryEvaluationHistory {
            current_enabled: false,
            evaluation_count: 7,
            disable_count: 2,
            ..MemoryEvaluationHistory::new()
        };
        h.save(&path).unwrap();

        let loaded = MemoryEvaluationHistory::load_from_workspace(dir.path()).unwrap();
        assert!(!loaded.current_enabled);
        assert_eq!(loaded.evaluation_count, 7);
    }

    #[test]
    fn test_history_load_from_workspace_missing() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = MemoryEvaluationHistory::load_from_workspace(dir.path());
        assert!(loaded.is_none());
    }

    #[test]
    fn test_history_save_to_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let h = MemoryEvaluationHistory {
            current_enabled: false,
            evaluation_count: 2,
            ..MemoryEvaluationHistory::new()
        };
        h.save_to_workspace(dir.path()).unwrap();

        let path = dir
            .path()
            .join(".forge")
            .join(MEMORY_EVALUATION_HISTORY_FILENAME);
        assert!(path.exists());

        let loaded = MemoryEvaluationHistory::load_from_workspace(dir.path()).unwrap();
        assert!(!loaded.current_enabled);
        assert_eq!(loaded.evaluation_count, 2);
    }

    // ===== to_history / from_history 测试 =====

    #[test]
    fn test_evaluator_to_history_default() {
        let evaluator = MemoryContextEvaluator::new(MemoryEvaluationConfig::default());
        let history = evaluator.to_history();
        assert!(history.initial_enabled);
        assert!(history.current_enabled);
        assert_eq!(history.evaluation_count, 0);
        assert!(history.is_empty());
    }

    #[test]
    fn test_evaluator_to_history_after_disable() {
        let mut evaluator = MemoryContextEvaluator::new(MemoryEvaluationConfig::default());
        evaluator.evaluate_and_apply(3, 0, 3, 3);

        let history = evaluator.to_history();
        assert!(history.initial_enabled);
        assert!(!history.current_enabled);
        assert_eq!(history.evaluation_count, 1);
        assert_eq!(history.disable_count, 1);
        assert!(history.enabled_changed());
    }

    #[test]
    fn test_evaluator_from_history_enabled() {
        let history = MemoryEvaluationHistory {
            current_enabled: true,
            evaluation_count: 5,
            disable_count: 0,
            ..MemoryEvaluationHistory::new()
        };
        let evaluator =
            MemoryContextEvaluator::from_history(history, MemoryEvaluationConfig::default());
        assert!(evaluator.is_enabled());
        assert!(evaluator.initial_enabled());
        assert_eq!(evaluator.evaluation_count(), 5);
    }

    #[test]
    fn test_evaluator_from_history_disabled() {
        let history = MemoryEvaluationHistory {
            initial_enabled: true,
            current_enabled: false,
            evaluation_count: 3,
            disable_count: 1,
            last_decision: Some(MemoryEvaluationDecision::disable_injection(-0.2, "有害")),
            saved_at: None,
        };
        let evaluator =
            MemoryContextEvaluator::from_history(history, MemoryEvaluationConfig::default());
        assert!(!evaluator.is_enabled());
        assert!(!evaluator.initial_enabled());
        assert_eq!(evaluator.evaluation_count(), 3);
        assert_eq!(evaluator.disable_count(), 1);
    }

    #[test]
    fn test_evaluator_save_and_load_workspace() {
        let dir = tempfile::tempdir().unwrap();

        let mut evaluator = MemoryContextEvaluator::new(MemoryEvaluationConfig::default());
        evaluator.evaluate_and_apply(3, 0, 3, 3);
        assert!(!evaluator.is_enabled());

        evaluator.save_to_workspace(dir.path()).unwrap();

        let loaded = MemoryContextEvaluator::load_from_workspace(
            dir.path(),
            MemoryEvaluationConfig::default(),
        )
        .unwrap();
        assert!(!loaded.is_enabled());
        assert_eq!(loaded.evaluation_count(), 1);
        assert_eq!(loaded.disable_count(), 1);
    }

    #[test]
    fn test_evaluator_load_workspace_missing() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = MemoryContextEvaluator::load_from_workspace(
            dir.path(),
            MemoryEvaluationConfig::default(),
        );
        assert!(loaded.is_none());
    }

    #[test]
    fn test_evaluator_history_roundtrip_preserves_state() {
        let dir = tempfile::tempdir().unwrap();

        // Session 1: 评估并禁用
        let mut evaluator = MemoryContextEvaluator::new(MemoryEvaluationConfig::default());
        evaluator.evaluate_and_apply(3, 0, 3, 3);
        evaluator.save_to_workspace(dir.path()).unwrap();

        // Session 2: 从历史恢复
        let mut evaluator2 = MemoryContextEvaluator::load_from_workspace(
            dir.path(),
            MemoryEvaluationConfig::default(),
        )
        .unwrap();
        assert!(!evaluator2.is_enabled());
        assert_eq!(evaluator2.evaluation_count(), 1);
        assert!(!evaluator2.initial_enabled());

        // Session 2: 再次评估 (已禁用 → keep)
        let decision = evaluator2.evaluate_and_apply(0, 0, 0, 0);
        assert!(decision.is_keep());
        assert_eq!(evaluator2.evaluation_count(), 2);

        // 保存 Session 2
        evaluator2.save_to_workspace(dir.path()).unwrap();

        // Session 3: 从历史恢复
        let evaluator3 = MemoryContextEvaluator::load_from_workspace(
            dir.path(),
            MemoryEvaluationConfig::default(),
        )
        .unwrap();
        assert!(!evaluator3.is_enabled());
        assert_eq!(evaluator3.evaluation_count(), 2);
    }

    // ===== 集成场景测试 =====

    #[test]
    fn test_full_workflow_beneficial_injection() {
        let mut evaluator = MemoryContextEvaluator::new(MemoryEvaluationConfig::default());

        // with: 4/5=80%, without: 2/5=40%, diff=+40%
        let decision = evaluator.evaluate_and_apply(5, 4, 5, 2);
        assert!(decision.is_keep());
        assert!(evaluator.is_enabled());
        assert!((decision.diff - 0.4).abs() < 0.001);
    }

    #[test]
    fn test_full_workflow_harmful_injection() {
        let mut evaluator = MemoryContextEvaluator::new(MemoryEvaluationConfig::default());

        // with: 1/5=20%, without: 4/5=80%, diff=-60%
        let decision = evaluator.evaluate_and_apply(5, 1, 5, 4);
        assert!(decision.is_disable());
        assert!(!evaluator.is_enabled());
        assert!((decision.diff - (-0.6)).abs() < 0.001);
    }

    #[test]
    fn test_full_workflow_neutral_injection() {
        let mut evaluator = MemoryContextEvaluator::new(MemoryEvaluationConfig::default());

        // with: 3/5=60%, without: 3/5=60%, diff=0%
        let decision = evaluator.evaluate_and_apply(5, 3, 5, 3);
        assert!(decision.is_keep());
        assert!(evaluator.is_enabled());
        assert!((decision.diff - 0.0).abs() < 0.001);
    }
}
