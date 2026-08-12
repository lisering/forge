//! Orchestrator — 自主多阶段开发引擎
//!
//! 给定终极目标,自动:
//! 1. 让 AI 拆解开发阶段
//! 2. 逐个阶段执行任务
//! 3. 每个任务: 发 prompt → 提取代码 → 编译测试 → 反馈 → 修复
//! 4. 阶段完成后进入下一阶段
//! 5. 所有阶段完成后输出最终报告
//!
//! ## SOLID 架构
//!
//! ### DIP (依赖倒置)
//! Orchestrator 依赖 trait 抽象 (`ChatClient`, `TestRunner`, `FileExtractor`)
//! 而非具体类型 (`ChatTab`, `cargo_check`, `extract_files`),
//! 使核心编排逻辑可在无 Chrome 环境下测试。
//!
//! ### SRP (单一职责)
//! 从原 God Object 拆分为:
//! - `Orchestrator` — 纯编排 (阶段/任务排序, 断点续传)
//! - `FixPromptBuilder` — 构建增量修复 prompt, 路径规范化
//! - `ContextBuilder` — 构建给 AI 的项目上下文摘要
//! - `VersionManager` — 封装快照保存/回滚操作

use crate::auto_recovery::{AutoRecovery, RecoveryConfig};
use crate::cache_tuning::{extract_correlation_diffs, extract_ttl_trajectory, CacheTuner};
use crate::clarify::HeuristicClarificationChecker;
use crate::connection_monitor::ConnectionMonitor;
use crate::context_handoff::ContextHandoff;
use crate::dev_trace::{
    build_cache_fix_correlation, build_cache_tuning_history_summary, build_export_timestamp,
    build_memory_evaluation_history_summary, build_memory_evaluation_stats,
    build_search_quality_history_summary, build_search_quality_stats, DevTraceWriter, TraceAction,
};
use crate::dev_trace::{extract_memory_diff_history, extract_search_diff_history};
use crate::error_diagnosis::{DiagnosisContext, DiagnosisResult, ErrorDiagnoser, ErrorHistory};
use crate::error_search;
use crate::interaction::AutoApprove;
use crate::joint_decision::{build_joint_decision_history_summary, JointDecisionEngine};
use crate::loop_detector::LoopDetector;
use crate::memory::{Memory, Phase, PhaseStatus, Task, TaskStatus};
use crate::memory_evaluation::MemoryContextEvaluator;
use crate::prompt_builder::SystemPrompt;
use crate::response_handler::{HandlerChain, TaskContext};
use crate::search_cache::{self, CachedSearchEntry, SearchCache};
use crate::search_quality::SearchQualityEvaluator;
use crate::slash_command::{self, SlashCommand, SlashCommandAction};
use crate::steer_reminder::SteerReminder;
use crate::task_graph::TaskGraph;
use crate::testrunner::{load_e2e_tests_from_workspace, CompileError, E2ETestSummary};
use crate::traits::{
    ChatClient, ClarificationChecker, ClarificationContext, FileExtractor, FixContext,
    HumanInteraction, PhaseInfo, PlanInfo, TaskAction, TaskInfo, TestRunner, WebTool,
};
use crate::workspace::Workspace;
use anyhow::Result;
use std::collections::HashSet;
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

// ============================================================================
//  SRP 拆分: FixPromptBuilder, ContextBuilder, VersionManager
// ============================================================================

/// 修复 prompt 构建器 (SRP: 从 Orchestrator 拆出)
///
/// 负责构建增量修复 prompt: 只发送有错误的文件 + 错误信息,
/// 以及路径规范化和文件内容读取。
pub struct FixPromptBuilder;

impl FixPromptBuilder {
    /// 构建增量修复 prompt — 只发送有错误的文件 + 错误信息
    ///
    /// 当修复轮次 > 1 时调用,替代之前发送全部代码摘要的方式:
    /// - 有编译错误时: 只发送出错文件的完整内容 + 具体错误信息
    /// - 无特定错误文件 (如测试运行时失败): 发送本任务生成的文件 + 测试输出
    /// - 无任何反馈 (如 AI 未返回代码): 回退到全量代码摘要
    pub fn build_fix_prompt(
        workspace: &Workspace,
        memory: &Memory,
        errors: &[CompileError],
        feedback: &str,
        phase_idx: usize,
        task_idx: usize,
    ) -> String {
        // 无任何错误反馈时,回退到全量代码摘要
        if feedback.is_empty() {
            let current_code = ContextBuilder::get_current_code_summary(workspace);
            return format!(
                "之前的尝试未成功。当前代码:\n{}\n\n\
                 请重新执行任务并输出所有文件 (用 ```file:路径``` 格式)。",
                current_code
            );
        }

        // 从错误中提取涉及的文件路径 (去重)
        let error_files: Vec<String> = {
            let mut seen = std::collections::HashSet::new();
            let mut files = Vec::new();
            for err in errors {
                if let Some(normalized) = Self::normalize_error_path(workspace, &err.file) {
                    if seen.insert(normalized.clone()) {
                        files.push(normalized);
                    }
                }
            }
            files
        };

        if !error_files.is_empty() {
            // 增量修复: 只发送出错的文件
            info!("    增量修复: 只发送 {} 个出错文件", error_files.len());
            let files_content = Self::get_files_full_content(workspace, &error_files);
            let project_files = ContextBuilder::get_project_file_list(memory);
            format!(
                "之前的代码有编译/测试错误:\n{}\n\n\
                 以下是出错的文件 (完整内容):\n{}\n\
                 当前项目文件列表:\n{}\n\n\
                 请根据错误信息修复代码。只输出你修改过的文件,\n\
                 用 ```file:路径``` 格式输出完整文件内容 (不要省略任何部分)。",
                feedback, files_content, project_files
            )
        } else {
            // 无特定错误文件 (如测试运行时失败),回退到发送本任务生成的文件
            info!("    增量修复: 无特定错误文件,发送本任务文件");
            let task_files = &memory.phases[phase_idx].tasks[task_idx].files_written;
            let files_content = Self::get_files_full_content(workspace, task_files);
            format!(
                "之前的代码有测试错误:\n{}\n\n\
                 以下是本次任务生成的文件 (完整内容):\n{}\n\n\
                 请根据测试错误修复代码。只输出你修改过的文件,\n\
                 用 ```file:路径``` 格式输出完整文件内容 (不要省略任何部分)。",
                feedback, files_content
            )
        }
    }

    /// 将 cargo 输出中的文件路径规范化为工作区相对路径
    ///
    /// cargo 可能输出:
    /// - 相对路径: "src/main.rs" → 直接返回
    /// - 绝对路径: "/tmp/xxx/src/main.rs" → 去除工作区根目录前缀
    /// - 依赖路径: "/Users/.../.cargo/registry/..." → 返回 None (无法修复)
    pub fn normalize_error_path(workspace: &Workspace, file: &str) -> Option<String> {
        let file = file.trim();
        if file.is_empty() {
            return None;
        }

        // 相对路径: 直接使用
        if !file.starts_with('/') && !file.starts_with('\\') {
            return Some(file.to_string());
        }

        // 绝对路径: 尝试去除工作区根目录前缀
        let root = workspace.root.display().to_string();
        if file.starts_with(&root) {
            let rel = &file[root.len()..];
            return Some(rel.trim_start_matches('/').to_string());
        }

        // 无法规范化的绝对路径 (如依赖库源码),跳过
        None
    }

    /// 读取多个文件的完整内容 (用于增量修复 prompt)
    pub fn get_files_full_content(workspace: &Workspace, paths: &[String]) -> String {
        let mut result = String::new();
        for path in paths {
            match workspace.read_file(path) {
                Ok(content) => {
                    let lines = content.lines().count();
                    result.push_str(&format!("--- {} ({}行) ---\n", path, lines));
                    result.push_str(&content);
                    if !content.ends_with('\n') {
                        result.push('\n');
                    }
                    result.push('\n');
                }
                Err(_) => {
                    result.push_str(&format!("--- {} (文件不存在) ---\n\n", path));
                }
            }
        }
        if result.is_empty() {
            "(无文件)".to_string()
        } else {
            result
        }
    }
}

/// 上下文构建器 (SRP: 从 Orchestrator 拆出)
///
/// 负责构建给 AI 的项目上下文: 代码摘要、文件列表。
pub struct ContextBuilder;

impl ContextBuilder {
    /// 获取当前代码摘要 (给 AI 的上下文)
    ///
    /// 列出项目文件,每个文件显示行数和前 200 字符预览。
    pub fn get_current_code_summary(workspace: &Workspace) -> String {
        let files = workspace.list_files().unwrap_or_default();
        let code_files: Vec<_> = files
            .iter()
            .filter(|f| !f.starts_with("target/") && !f.starts_with("Cargo.lock"))
            .collect();

        if code_files.is_empty() {
            return "(空项目)".to_string();
        }

        let mut summary = String::new();
        for f in code_files.iter().take(10) {
            let content = workspace.read_file(f).unwrap_or_default();
            let lines = content.lines().count();
            let preview: String = content.chars().take(200).collect();
            summary.push_str(&format!("  {} ({}行): {}...\n", f, lines, preview));
        }
        summary
    }

    /// 获取项目文件列表 (仅路径,不含内容)
    pub fn get_project_file_list(memory: &Memory) -> String {
        if memory.workspace_files.is_empty() {
            return "(空项目)".to_string();
        }
        memory
            .workspace_files
            .iter()
            .map(|f| format!("  {}", f))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// 版本管理器 (SRP: 从 Orchestrator 拆出)
///
/// 封装 Workspace 的快照保存/回滚操作。
pub struct VersionManager;

impl VersionManager {
    /// 保存当前状态为 known good 快照
    ///
    /// 在 cargo check 通过后调用。返回快照 ID。
    pub fn save_known_good(workspace: &Workspace) -> Result<Option<u32>> {
        let good_id = workspace.snapshot_all("known_good")?;
        workspace.save_known_good(good_id)?;
        debug!("    known good 快照 #{}", good_id);
        Ok(Some(good_id))
    }

    /// 回滚到最近的 known good 快照
    ///
    /// 在任务最终失败时调用。返回回滚到的快照 ID (None 表示无 known good)。
    pub fn rollback_to_known_good(workspace: &Workspace) -> Result<Option<u32>> {
        workspace.rollback_to_known_good()
    }
}

// ============================================================================
//  MemoryContextStats — Memory 上下文注入统计 (Session 89)
// ============================================================================

/// Memory 上下文注入统计 — 追踪 `build_messages_from_memory` 注入效果
///
/// 记录在修复轮次中从 Memory 注入对话历史的次数和消息数,
/// 便于在 DevTrace 报告中展示注入效果。
///
/// # 字段
///
/// - `injection_count`: memory context 被注入的次数 (每次修复轮次注入算 1 次)
/// - `total_messages_injected`: 注入的总消息数 (所有注入次数的消息数之和)
/// - `total_messages_skipped`: 注入的消息中被增量跟踪跳过的数量
///
/// # 示例
///
/// ```
/// # use forge::orchestrator::MemoryContextStats;
/// let mut stats = MemoryContextStats::new();
/// stats.record_injection(3, 2); // 注入3条, 跳过2条
/// assert_eq!(stats.injection_count, 1);
/// assert_eq!(stats.total_messages_injected, 3);
/// assert_eq!(stats.total_messages_skipped, 2);
/// assert_eq!(stats.avg_messages_per_injection(), 3.0);
/// ```
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct MemoryContextStats {
    /// memory context 被注入的次数
    pub injection_count: usize,
    /// 注入的总消息数
    pub total_messages_injected: usize,
    /// 注入的消息中被增量跟踪跳过的数量
    pub total_messages_skipped: usize,
}

impl MemoryContextStats {
    /// 创建空的统计
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一次 memory context 注入
    ///
    /// # 参数
    /// - `messages_injected`: 本次注入的消息数
    /// - `messages_skipped`: 本次注入中被跳过的消息数
    pub fn record_injection(&mut self, messages_injected: usize, messages_skipped: usize) {
        self.injection_count += 1;
        self.total_messages_injected += messages_injected;
        self.total_messages_skipped += messages_skipped;
    }

    /// 平均每次注入的消息数
    pub fn avg_messages_per_injection(&self) -> f64 {
        if self.injection_count == 0 {
            0.0
        } else {
            self.total_messages_injected as f64 / self.injection_count as f64
        }
    }

    /// 跳过率 (跳过的消息占注入消息的比例)
    pub fn skip_rate(&self) -> f64 {
        if self.total_messages_injected == 0 {
            0.0
        } else {
            self.total_messages_skipped as f64 / self.total_messages_injected as f64
        }
    }

    /// 是否有注入记录
    pub fn has_data(&self) -> bool {
        self.injection_count > 0
    }

    /// 生成摘要字符串 (用于 DevTrace 报告)
    pub fn to_summary(&self) -> String {
        if !self.has_data() {
            return String::new();
        }
        format!(
            "注入次数: {}, 总消息: {}, 跳过: {}, 跳过率: {:.1}%",
            self.injection_count,
            self.total_messages_injected,
            self.total_messages_skipped,
            self.skip_rate() * 100.0
        )
    }
}

// ============================================================================
//  纯函数: build_fix_messages_with_memory — 构建修复轮次的消息列表
// ============================================================================

/// 构建修复轮次的消息列表 (含 Memory 上下文注入)
///
/// 在修复轮次中, 将 Memory 中的近期对话历史注入消息列表,
/// 使 AI 获得更完整的上下文。结合增量发送机制,
/// 已发送的历史消息会被自动跳过, 只发送新增的修复 prompt。
///
/// # 消息列表结构
///
/// ```text
/// [first_prompt?, ...memory_messages, fix_prompt]
/// ```
///
/// - `first_prompt`: 首次尝试的 prompt (如有, 已发送过会被跳过)
/// - `memory_messages`: 从 Memory 提取的近期对话 (已发送过会被跳过)
/// - `fix_prompt`: 当前修复 prompt (新增内容, 实际发送)
///
/// # 参数
///
/// - `first_prompt`: 首次尝试的 prompt (`None` 表示未存储, 如上下文衔接后)
/// - `fix_prompt`: 当前修复 prompt
/// - `memory_messages`: 从 Memory 提取的近期对话消息列表
///
/// # 返回
///
/// 有序的消息列表, 供 `send_with_continuation` 使用。
///
/// # 示例
///
/// ```
/// # use forge::orchestrator::build_fix_messages_with_memory;
/// // 有 first_prompt + memory
/// let msgs = build_fix_messages_with_memory(
///     &Some("first".to_string()),
///     "fix",
///     &["hist1".to_string(), "hist2".to_string()],
/// );
/// assert_eq!(msgs, vec!["first", "hist1", "hist2", "fix"]);
///
/// // 无 first_prompt
/// let msgs = build_fix_messages_with_memory(
///     &None,
///     "fix",
///     &["hist1".to_string()],
/// );
/// assert_eq!(msgs, vec!["hist1", "fix"]);
///
/// // 无 memory
/// let msgs = build_fix_messages_with_memory(
///     &Some("first".to_string()),
///     "fix",
///     &[],
/// );
/// assert_eq!(msgs, vec!["first", "fix"]);
/// ```
pub fn build_fix_messages_with_memory(
    first_prompt: &Option<String>,
    fix_prompt: &str,
    memory_messages: &[String],
) -> Vec<String> {
    let mut messages = Vec::with_capacity(memory_messages.len() + 2);

    // 添加首次 prompt (如有, 已发送过会被增量跟踪跳过)
    if let Some(ref first) = first_prompt {
        messages.push(first.clone());
    }

    // 添加 Memory 上下文 (已发送过会被增量跟踪跳过)
    messages.extend(memory_messages.iter().cloned());

    // 添加修复 prompt (新增内容, 实际发送)
    messages.push(fix_prompt.to_string());

    messages
}

// ============================================================================
//  截断 JSON 恢复 (AI 回复超长导致 JSON 不完整时的容错处理)
// ============================================================================

/// 修复截断的 JSON 字符串
///
/// 当 AI 生成超长 JSON 回复时, 可能因为以下原因导致 JSON 不完整:
/// 1. CDP `Runtime.evaluate` 的 `returnByValue` 在超大响应时可能截断
/// 2. 稳定性检测提前判定完成 (AI 暂停生成时误判)
/// 3. AI 本身输出被 token 限制截断
///
/// 本函数通过以下策略修复:
/// 1. 找到最后一个完整的 `}` (一个完整对象的结束)
/// 2. 截断到该位置
/// 3. 关闭所有未闭合的数组 `]`
/// 4. 尝试解析
///
/// # 示例
/// ```
/// // 截断的 JSON: [{"name":"阶段1"}, {"name":"阶段2
/// // 修复后:     [{"name":"阶段1"}]
/// ```
fn repair_truncated_json(json_str: &str) -> String {
    let trimmed = json_str.trim();

    // 如果已经是有效的 JSON, 直接返回
    if serde_json::from_str::<serde_json::Value>(trimmed).is_ok() {
        return trimmed.to_string();
    }

    // 找到最后一个完整的对象结束位置 (最后一个 `}`)
    // 我们需要找到一个 `}` 后面跟 `,` 或 `]` 的位置, 表示一个完整对象
    let mut last_complete_end = None;
    let mut brace_depth = 0i32;
    let mut in_string = false;
    let mut escape = false;

    for (i, ch) in trimmed.char_indices() {
        if escape {
            escape = false;
            continue;
        }
        if ch == '\\' {
            escape = true;
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        match ch {
            '{' => brace_depth += 1,
            '}' => {
                brace_depth -= 1;
                if brace_depth == 0 {
                    // 一个完整的顶层对象结束了
                    last_complete_end = Some(i + 1);
                }
            }
            _ => {}
        }
    }

    if let Some(end) = last_complete_end {
        let mut repaired = trimmed[..end].to_string();
        // 只在输入是数组时关闭未闭合的数组
        if repaired.starts_with('[') && !repaired.ends_with(']') {
            repaired.push(']');
        }
        // 尝试再次修复: 移除末尾的逗号
        if repaired.ends_with(",]") {
            repaired = repaired.trim_end_matches(",]").to_string();
            repaired.push(']');
        }
        // 验证修复后的 JSON 是否有效, 无效则回退到空数组
        if serde_json::from_str::<serde_json::Value>(&repaired).is_ok() {
            repaired
        } else {
            "[]".to_string()
        }
    } else {
        // 无法找到完整对象, 返回空数组
        "[]".to_string()
    }
}

// ============================================================================
//  Orchestrator (DIP: 依赖 trait 抽象)
// ============================================================================

/// 自主开发编排器
///
/// 泛型参数 (DIP):
/// - `C`: `ChatClient` — AI 对话能力 (ChatTab / MockChatClient)
/// - `T`: `TestRunner` — 编译测试能力 (CargoTestRunner / MockTestRunner)
/// - `E`: `FileExtractor` — 文件提取能力 (DefaultExtractor / MockExtractor)
///
/// 编排逻辑通过 trait 抽象与具体实现解耦,
/// 测试时可注入 Mock,无需 Chrome/浏览器。
pub struct Orchestrator<'a, C, T, E, Q = HeuristicClarificationChecker>
where
    C: ChatClient,
    T: TestRunner,
    E: FileExtractor,
    Q: ClarificationChecker,
{
    /// AI 聊天客户端 (DIP: trait 抽象)
    pub chat: &'a C,
    /// 编译测试运行器 (DIP: trait 抽象)
    pub test_runner: T,
    /// 文件提取器 (DIP: trait 抽象)
    pub extractor: E,
    /// 自主追问检查器 (DIP: trait 抽象) — 核心中的核心
    pub clarification_checker: Q,
    /// 人工干预接口 (DIP: trait 抽象) — 方向 A
    pub interaction: Box<dyn HumanInteraction>,
    pub workspace: Workspace,
    pub memory: Memory,
    pub max_rounds_per_task: u32,
    pub timeout_secs: u64,
    /// 是否从断点恢复
    pub resume: bool,
    /// 是否启用并行任务执行 (方向 C)
    ///
    /// 启用后, 使用 TaskGraph 分析任务依赖关系,
    /// 按并行分组执行任务 (同组内任务无依赖, 可按顺序快速执行)。
    /// 禁用时, 按原有顺序执行 (向后兼容)。
    pub parallel: bool,

    /// 智能错误诊断器 (方向 F) — None 表示不启用
    ///
    /// 启用后, 在编译/测试失败时自动分析错误根因,
    /// 生成精准修复指令, 并记录错误历史用于学习。
    pub error_diagnoser: Option<Box<dyn ErrorDiagnoser>>,
    /// 错误历史 — 记录错误模式, 支持持久化到 .forge/error_history.json
    pub error_history: ErrorHistory,
    /// 上下文衔接最大对话轮数 (借鉴方向 1)
    ///
    /// 对话轮数超过此阈值时, 自动新开对话并交接上下文。
    /// 0 表示禁用上下文衔接。
    pub max_context_turns: usize,

    /// 转向提醒间隔 (借鉴方向 2)
    ///
    /// 每隔此轮数, 在发送给 AI 的消息前注入"转向提醒", 重新锚定 AI 注意力。
    /// 0 表示禁用转向提醒 (默认)。
    /// 推荐值: 10 (小于 max_context_turns, 如 10 < 30)。
    pub steer_interval: usize,

    /// 循环终止检测器 (借鉴方向 3) — None 表示禁用
    ///
    /// 启用后, 在修复循环中检测 AI 是否在原地打转 (同样的编译错误反复出现)。
    /// 检测到死循环时主动改变策略 (换角度提问), 策略改变后仍失败则建议跳过。
    pub loop_detector: Option<LoopDetector>,

    /// 结构化开发追踪写入器 (借鉴方向 4) — None 表示禁用
    ///
    /// 启用后, 在关键操作点 (planning, task execution, fix, clarify,
    /// context handoff, steer reminder, loop detection, compile/test)
    /// 写入 trace 条目到 `.forge/devtrace.jsonl`,
    /// 提供 24 小时运行的可观测性。
    pub dev_trace: Option<DevTraceWriter>,

    /// AI 自主指令 (Slash Commands) 是否启用 (借鉴方向 5)
    ///
    /// 启用后, 在 AI 回复中检测特殊的指令标记 (如 `/compact`、`/skip`、`/refocus`),
    /// Forge 解析这些指令并执行对应操作:
    /// - `/compact` → 触发上下文衔接
    /// - `/skip` → 跳过当前任务
    /// - `/refocus` → 注入转向提醒
    /// - `/retry` → 重置循环终止检测器
    /// - `/escalate` → 触发人工干预
    pub slash_commands_enabled: bool,

    /// 连接监控器 — Chrome 连接状态监控 (24h 可靠性)
    ///
    /// 启用后, 在每次 send_message 前检查 Chrome 连接状态,
    /// 检测到断连时触发自动恢复。
    /// None 表示禁用 (默认, 向后兼容)。
    pub connection_monitor: Option<ConnectionMonitor>,

    /// 自动恢复器 — Chrome 断连后自动重连 (24h 可靠性)
    ///
    /// 启用后, 检测到 Chrome 断连时使用指数退避策略重试连接,
    /// 恢复成功后从 Memory 断点续传。
    /// None 表示禁用 (默认, 向后兼容)。
    pub auto_recovery: Option<AutoRecovery>,

    /// 回调处理器链 — 借鉴 MediaCrawler callback 模式 (Session 69)
    ///
    /// 启用后, 在每次 AI 回复后通过 handler 链处理:
    /// - CodeExtractorHandler: 提取代码文件
    /// - TraceWriterHandler: 记录开发追踪
    /// - MemoryUpdaterHandler: 更新项目记忆
    ///
    /// None 表示禁用 (默认, 向后兼容, 使用原有的直接提取逻辑)。
    pub handler_chain: Option<HandlerChain>,

    /// Web 工具 — AI 自主网页搜索/文档查阅能力 (Session 73)
    ///
    /// 启用后, Forge 可以在开发流程中自主搜索文档、查阅网页内容，
    /// 借鉴 ds4 `ds4_web.c` 的智能滚动、内容提取等技术。
    ///
    /// None 表示禁用 (默认, 向后兼容)。启用后可配合 Slash Commands
    /// 中的 `/search` 指令使用。
    pub web_tool: Option<Box<dyn WebTool>>,

    /// 上下文压缩配置 — ds4 风格软/硬触发机制
    ///
    /// 启用后, 基于 token 数量而非对话轮数触发上下文压缩。
    /// None 表示禁用 (使用原有的基于对话轮数的上下文衔接)。
    pub compaction_config: Option<crate::context_handoff::CompactionConfig>,

    /// Live Continuation — 借鉴 ds4 `ANTHROPIC_LIVE_CONTINUATION.md`
    ///
    /// 追踪已发送的消息 ID，只发送增量部分而非完整上下文。
    /// 启用后, 在每次发送消息前检查是否已发送过，跳过重复消息。
    /// None 表示禁用 (默认, 向后兼容)。
    pub live_continuation: Option<crate::live_continuation::LiveContinuation>,

    /// Radix Tree 对话状态跟踪 — 借鉴 ds4 `rax.h`
    ///
    /// 用基数树存储对话前缀，避免重复发送相同上下文。
    /// 启用后, 在对话级增量计算中使用 Radix Tree 查找最长公共前缀。
    /// None 表示禁用 (默认, 向后兼容)。
    pub conversation_tracker: Option<crate::radix_tree::ConversationTracker>,

    /// 增量发送统计 — 追踪 LiveContinuation / RadixTree 的节省效果 (Session 75)
    ///
    /// 记录每次增量发送的总消息数、实际发送数和跳过数。
    /// 在 `send_with_continuation` 方法中自动更新。
    pub incremental_stats: crate::dev_trace::IncrementalStats,

    /// 搜索结果缓存 — 编译错误自动搜索结果缓存 (Session 78)
    ///
    /// 对相同错误代码的搜索结果进行缓存, 避免重复搜索相同错误,
    /// 减少搜索延迟和带宽消耗。
    /// 基于 TTL + LRU 策略, 默认 TTL=30分钟, 最大50条。
    pub search_cache: SearchCache,

    /// 缓存调优器 — 基于 CacheFixCorrelation 自动调优缓存策略 (Session 82)
    ///
    /// 启用后, 在每次编译检查后评估缓存命中与未命中的修复成功率差值,
    /// 自动调整 TTL (缩短/延长) 或禁用缓存。
    /// None 表示禁用 (默认, 向后兼容)。
    pub cache_tuner: Option<CacheTuner>,

    /// 搜索质量评估器 — 评估自动搜索对修复成功率的影响 (Session 85)
    ///
    /// 启用后, 在每次编译检查后评估搜索质量,
    /// 当搜索有害时自动禁用搜索功能。
    /// None 表示禁用 (默认, 向后兼容)。
    pub search_quality_evaluator: Option<SearchQualityEvaluator>,

    /// Memory 上下文注入条数 — 修复轮次中注入近期对话历史 (Session 89)
    ///
    /// 启用后 (>0), 在 `send_attempt_prompt` 的修复轮次中,
    /// 从 Memory 对话历史提取最近 N 条对话注入消息列表。
    /// 结合增量发送机制, 已发送的历史消息会被自动跳过,
    /// 只发送新增的修复 prompt, 同时为 AI 提供更完整的上下文。
    /// 0 表示禁用 (默认, 向后兼容)。
    pub memory_context_count: usize,

    /// Memory 上下文注入统计 — 追踪注入次数和消息数 (Session 89)
    ///
    /// 记录 memory context 被注入的次数和总消息数,
    /// 用于 DevTrace 报告中展示注入效果。
    pub memory_context_stats: MemoryContextStats,

    /// Memory 评估器 — 评估 Memory 上下文注入效果并自动禁用 (Session 90)
    ///
    /// 启用后, 在每次编译检查后评估 Memory 注入对修复成功率的影响,
    /// 当注入有害时自动禁用 Memory 上下文注入。
    /// None 表示禁用 (默认, 向后兼容)。
    pub memory_evaluator: Option<MemoryContextEvaluator>,

    /// 联合决策引擎 — 三评估器协同决策 (Session 99)
    ///
    /// 启用后, 在每次编译检查后综合 CacheTuner, SearchQualityEvaluator,
    /// MemoryContextEvaluator 的状态, 做出联合决策:
    /// - 2+ 评估器禁用 → 升级警告
    /// - 全部评估器禁用 → 进入保守模式
    /// - 保守模式 N 轮后 → 尝试重新启用功能
    ///
    /// `None` 表示禁用 (默认, 向后兼容)。
    pub joint_decision_engine: Option<JointDecisionEngine>,

    /// 自动修复开关 — 代码提取后自动应用 apply_fixes (Session 118)
    ///
    /// 启用后, 在从 AI 回复提取代码文件后, 对 Rust (.rs) 文件自动调用
    /// `apply_fixes` 修复质量问题 (unwrap → ?, 添加 #[must_use], 文档注释等),
    /// 并打印修复摘要。默认 false (向后兼容)。
    pub auto_fix_enabled: bool,

    /// clippy 检查开关 — 代码写入后自动运行 cargo clippy (Session 120)
    ///
    /// 启用后, 在代码写入工作区后自动运行 `cargo clippy`,
    /// 打印 clippy 警告和错误。默认 false (向后兼容)。
    pub clippy_check_enabled: bool,

    /// 分阶段修复开关 — 按优先级分批修复 (Session 121)
    ///
    /// 启用后, 替代 `apply_fixes` 的一次性修复, 改为三阶段:
    /// 1. 高优先级 (unwrap/expect/todo/panic 等)
    /// 2. 中优先级 (unsafe/unwrap_or/#[must_use] 等)
    /// 3. 低优先级 (文档注释)
    ///
    /// 默认 false (向后兼容, 使用一次性修复)。
    pub staged_fix_enabled: bool,

    /// 修复预览开关 — 显示分阶段修复预览而不实际修改 (Session 123)
    ///
    /// 启用后, 使用 `apply_staged_fixes_preview` 显示每个阶段的修复预览,
    /// 包括每个阶段是否有变化、变化内容摘要, 但不实际修改文件内容。
    /// 需要同时启用 `auto_fix_enabled` 和 `staged_fix_enabled`。
    ///
    /// 默认 false (向后兼容)。
    pub fix_preview_enabled: bool,
}

/// 默认构造 — 使用 HeuristicClarificationChecker
impl<'a, C, T, E> Orchestrator<'a, C, T, E, HeuristicClarificationChecker>
where
    C: ChatClient,
    T: TestRunner,
    E: FileExtractor,
{
    pub fn new(
        chat: &'a C,
        test_runner: T,
        extractor: E,
        workspace_dir: &str,
        goal: &str,
        max_rounds: u32,
        timeout: u64,
    ) -> Self {
        Self {
            chat,
            test_runner,
            extractor,
            clarification_checker: HeuristicClarificationChecker::new(),
            interaction: Box::new(AutoApprove),
            workspace: Workspace::new(workspace_dir),
            memory: Memory::new(goal),
            max_rounds_per_task: max_rounds,
            timeout_secs: timeout,
            resume: false,
            parallel: false,
            error_diagnoser: None,
            error_history: ErrorHistory::new(),
            max_context_turns: 0,
            steer_interval: 0,
            loop_detector: None,
            dev_trace: None,
            slash_commands_enabled: false,
            connection_monitor: None,
            auto_recovery: None,
            handler_chain: None,
            web_tool: None,
            compaction_config: None,
            fix_preview_enabled: false,
            live_continuation: None,
            conversation_tracker: None,
            incremental_stats: crate::dev_trace::IncrementalStats::new(),
            search_cache: SearchCache::default_config(),
            cache_tuner: None,
            search_quality_evaluator: None,
            memory_context_count: 0,
            memory_context_stats: MemoryContextStats::new(),
            memory_evaluator: None,
            joint_decision_engine: None,
            auto_fix_enabled: false,
            clippy_check_enabled: false,
            staged_fix_enabled: false,
        }
    }
}

/// 通用方法 — 对所有 Q 实现都有效
impl<'a, C, T, E, Q> Orchestrator<'a, C, T, E, Q>
where
    C: ChatClient,
    T: TestRunner,
    E: FileExtractor,
    Q: ClarificationChecker,
{
    /// 设置断点恢复模式
    pub fn with_resume(mut self, resume: bool) -> Self {
        self.resume = resume;
        self
    }

    /// 启用/禁用并行任务执行 (方向 C)
    ///
    /// 启用后, 使用 TaskGraph 分析任务依赖关系并按并行分组执行。
    /// 禁用时, 按原有顺序执行 (向后兼容)。
    pub fn with_parallel(mut self, parallel: bool) -> Self {
        self.parallel = parallel;
        self
    }

    /// 替换自主追问检查器 (DIP: 可注入 Mock)
    ///
    /// 用于测试时注入 MockClarificationChecker。
    pub fn with_clarification<Q2: ClarificationChecker>(
        self,
        checker: Q2,
    ) -> Orchestrator<'a, C, T, E, Q2> {
        Orchestrator {
            chat: self.chat,
            test_runner: self.test_runner,
            extractor: self.extractor,
            clarification_checker: checker,
            interaction: self.interaction,
            workspace: self.workspace,
            memory: self.memory,
            max_rounds_per_task: self.max_rounds_per_task,
            timeout_secs: self.timeout_secs,
            resume: self.resume,
            parallel: self.parallel,
            error_diagnoser: self.error_diagnoser,
            error_history: self.error_history,
            max_context_turns: self.max_context_turns,
            steer_interval: self.steer_interval,
            loop_detector: self.loop_detector,
            dev_trace: self.dev_trace,
            slash_commands_enabled: self.slash_commands_enabled,
            connection_monitor: self.connection_monitor,
            auto_recovery: self.auto_recovery,
            handler_chain: self.handler_chain,
            web_tool: self.web_tool,
            compaction_config: self.compaction_config,
            live_continuation: self.live_continuation,
            conversation_tracker: self.conversation_tracker,
            incremental_stats: self.incremental_stats,
            search_cache: self.search_cache,
            cache_tuner: self.cache_tuner,
            search_quality_evaluator: self.search_quality_evaluator,
            memory_context_count: self.memory_context_count,
            memory_context_stats: self.memory_context_stats,
            memory_evaluator: self.memory_evaluator,
            joint_decision_engine: self.joint_decision_engine,
            auto_fix_enabled: self.auto_fix_enabled,
            clippy_check_enabled: self.clippy_check_enabled,
            staged_fix_enabled: self.staged_fix_enabled,
            fix_preview_enabled: self.fix_preview_enabled,
        }
    }

    /// 替换人工干预接口 (DIP: 可注入 Mock)
    ///
    /// 用于设置 CLI 交互模式或测试时注入 MockInteraction。
    pub fn with_interaction(mut self, interaction: Box<dyn HumanInteraction>) -> Self {
        self.interaction = interaction;
        self
    }

    /// 启用智能错误诊断 (方向 F)
    ///
    /// 启用后, 在编译/测试失败时自动分析错误根因,
    /// 生成精准修复指令, 并记录错误历史用于学习。
    pub fn with_error_diagnosis(mut self, diagnoser: Box<dyn ErrorDiagnoser>) -> Self {
        self.error_diagnoser = Some(diagnoser);
        self
    }

    /// 启用智能错误诊断 (方向 F) — Option 版本
    ///
    /// 传入 None 时不启用, 传入 Some 时启用。
    /// 用于 CLI 中根据 --error-diagnosis 标志决定是否启用。
    pub fn with_error_diagnosis_opt(mut self, diagnoser: Option<Box<dyn ErrorDiagnoser>>) -> Self {
        self.error_diagnoser = diagnoser;
        self
    }

    /// 启用上下文衔接 (借鉴方向 1)
    ///
    /// 启用后, 对话轮数超过 max_turns 时自动新开对话并交接上下文。
    /// 设为 0 禁用 (默认)。
    pub fn with_context_handoff(mut self, max_turns: usize) -> Self {
        self.max_context_turns = max_turns;
        self
    }

    /// 启用转向提醒 (借鉴方向 2)
    ///
    /// 启用后, 每隔 interval 轮对话, 在发送给 AI 的消息前注入"转向提醒",
    /// 重新锚定 AI 注意力, 防止长时间运行后偏离目标。
    /// 设为 0 禁用 (默认)。推荐值: 10 (小于 max_context_turns)。
    pub fn with_steer_reminder(mut self, interval: usize) -> Self {
        self.steer_interval = interval;
        self
    }

    /// 启用循环终止检测 (借鉴方向 3)
    ///
    /// 启用后, 在修复循环中检测 AI 是否在原地打转 (同样的编译错误反复出现)。
    /// 检测到死循环时主动改变策略 (换角度提问), 策略改变后仍失败则建议跳过。
    /// 设为 0 禁用 (默认)。
    pub fn with_loop_detection(mut self, max_repeats: usize) -> Self {
        if max_repeats > 0 {
            self.loop_detector = Some(LoopDetector::new(max_repeats));
        } else {
            self.loop_detector = None;
        }
        self
    }

    /// 启用结构化开发追踪 (借鉴方向 4)
    ///
    /// 启用后, 在关键操作点写入 trace 条目到 `.forge/devtrace.jsonl`,
    /// 提供 24 小时运行的可观测性。记录每轮 AI 交互的详细信息
    /// (时间戳、阶段、任务、操作类型、输入/输出摘要、耗时、结果)。
    pub fn with_dev_trace(mut self, enabled: bool) -> Self {
        if enabled {
            self.dev_trace = Some(DevTraceWriter::new(&self.workspace.root));
        } else {
            self.dev_trace = None;
        }
        self
    }

    /// 启用结构化开发追踪并指定存储后端 (Session 69: 工厂模式集成)
    ///
    /// 根据后端类型选择 trace 文件格式:
    /// - `Jsonl` → JSONL 追加模式 (默认, 高效)
    /// - `Json` → JSON 数组模式 (便于整体读取)
    /// - `Sqlite`/`Postgres` → 回退到 JSONL (未实现)
    pub fn with_dev_trace_backend(mut self, backend: crate::trace_store::StorageBackend) -> Self {
        self.dev_trace = Some(DevTraceWriter::new_with_backend(
            &self.workspace.root,
            backend,
        ));
        self
    }

    /// 启用 AI 自主指令 (Slash Commands) (借鉴方向 5)
    ///
    /// 启用后, 在 AI 回复中检测特殊的指令标记 (如 `/compact`、`/skip`、`/refocus`),
    /// Forge 解析这些指令并执行对应操作:
    /// - `/compact` → 强制触发上下文衔接 (新开对话 + 交接)
    /// - `/skip` → 跳过当前任务 (标记为 Failed)
    /// - `/refocus` → 注入转向提醒 (重新锚定 AI 注意力)
    /// - `/retry` → 重置循环终止检测器 (允许全新方法重试)
    /// - `/escalate` → 触发人工干预 (请求人类决策)
    pub fn with_slash_commands(mut self, enabled: bool) -> Self {
        self.slash_commands_enabled = enabled;
        self
    }

    /// 启用 Web 工具 — AI 自主网页搜索/文档查阅能力 (Session 113)
    ///
    /// 启用后, Forge 可以在开发流程中自主搜索文档、查阅网页内容:
    /// - 编译错误自动搜索: 通过 WebTool 搜索解决方案
    /// - `/search` slash command: AI 自主请求搜索
    ///
    /// 需要传入实现 `WebTool` trait 的实例 (如 `CdpWebTool` 或 `MockWebTool`)。
    pub fn with_web_tool(mut self, tool: Box<dyn WebTool>) -> Self {
        self.web_tool = Some(tool);
        self
    }

    /// 启用自动恢复 — Chrome 断连后自动重连 (24h 可靠性)
    ///
    /// 启用后, 在每次 send_message 前检查 Chrome 连接状态,
    /// 检测到断连时使用指数退避策略重试连接。
    /// 恢复成功后从 Memory 断点续传, 恢复失败则返回错误。
    ///
    /// - `port`: Chrome 调试端口 (如 9222)
    /// - `max_retries`: 最大重试次数 (如 10)
    pub fn with_auto_recovery(mut self, port: u16, max_retries: u32) -> Self {
        self.connection_monitor = Some(ConnectionMonitor::new(port));
        self.auto_recovery = Some(AutoRecovery::new(RecoveryConfig::new(port, max_retries)));
        self
    }

    /// 启用上下文压缩 (ds4 风格) — Session 73
    ///
    /// 启用后, 基于 token 数量而非对话轮数触发上下文压缩。
    /// 使用 ds4 的软/硬触发机制:
    /// - Soft: 对话达到上下文窗口 85% 时, 在下一轮用户消息前压缩
    /// - Hard: 当工具结果无法放入剩余空间时, 立即压缩
    ///
    /// # 参数
    /// - `context_window`: 上下文窗口大小 (token 数, 如 128000)
    /// - `soft_threshold_pct`: 软触发阈值 (0.0-1.0, 默认 0.85)
    /// - `hard_threshold_tokens`: 硬触发阈值 (默认 8192)
    /// - `tail_preserve_tokens`: 压缩时保留的尾部 token 数 (默认 50000)
    /// - `min_tokens_for_soft_trigger`: 小上下文保护的最小 token 数 (默认 512)
    pub fn with_context_compaction(
        mut self,
        _context_window: usize,
        soft_threshold_pct: Option<f64>,
        hard_threshold_tokens: Option<usize>,
        tail_preserve_tokens: Option<usize>,
        min_tokens_for_soft_trigger: Option<usize>,
    ) -> Self {
        let config = crate::context_handoff::CompactionConfig {
            soft_threshold_pct: soft_threshold_pct.unwrap_or(0.85),
            hard_threshold_tokens: hard_threshold_tokens.unwrap_or(8192),
            tail_preserve_tokens: tail_preserve_tokens.unwrap_or(50000),
            min_tokens_for_soft_trigger: min_tokens_for_soft_trigger.unwrap_or(512),
        };
        self.compaction_config = Some(config);
        self
    }

    /// 创建全局取消令牌 — 基于 Orchestrator 的超时配置
    ///
    /// 用于长操作（如网页搜索、编译检查、测试运行）的取消感知。
    /// 自动在 Orchestrator 超时后取消所有操作。
    pub fn create_cancellation_token(&self) -> crate::cancellation_token::CancellationTokenSource {
        let timeout = std::time::Duration::from_secs(self.timeout_secs);
        crate::cancellation_token::CancellationTokenSource::with_timeout(timeout)
    }

    /// 启用回调处理器链 — 借鉴 MediaCrawler callback 模式 (Session 69)
    ///
    /// 启用后, 在每次 AI 回复后通过 handler 链处理:
    /// - CodeExtractorHandler: 从 AI 回复提取代码文件
    /// - TraceWriterHandler: 记录开发追踪 (轻量计数)
    /// - MemoryUpdaterHandler: 更新项目记忆
    ///
    /// 处理器链按顺序执行, 某个 handler 返回 stop_chain 时中断后续。
    /// 如果未启用, 则使用原有的直接提取逻辑 (向后兼容)。
    pub fn with_response_handlers(mut self, chain: HandlerChain) -> Self {
        self.handler_chain = Some(chain);
        self
    }

    /// 启用 Live Continuation — 借鉴 ds4 `ANTHROPIC_LIVE_CONTINUATION.md`
    ///
    /// 启用后, 追踪已发送的消息 ID，只发送增量部分而非完整上下文。
    /// 在对话被压缩或重置时自动重置跟踪状态。
    pub fn with_live_continuation(mut self) -> Self {
        self.live_continuation = Some(crate::live_continuation::LiveContinuation::new());
        self
    }

    /// 启用 Radix Tree 对话状态跟踪 — 借鉴 ds4 `rax.h`
    ///
    /// 启用后, 用基数树存储对话前缀，在对话级增量计算中
    /// 查找最长公共前缀，避免重复发送相同上下文。
    pub fn with_conversation_tracker(mut self) -> Self {
        self.conversation_tracker = Some(crate::radix_tree::ConversationTracker::new());
        self
    }

    /// 配置搜索结果缓存 — Session 78
    ///
    /// 自定义缓存 TTL 和最大条目数。
    /// 默认: TTL=1800s (30分钟), max=50。
    ///
    /// # 参数
    ///
    /// - `ttl_secs`: 缓存生存时间 (秒), 0 表示立即过期
    /// - `max_size`: 最大缓存条目数, 0 表示无限制
    pub fn with_search_cache_config(mut self, ttl_secs: u64, max_size: usize) -> Self {
        self.search_cache = SearchCache::new(ttl_secs, max_size);
        self
    }

    /// 启用缓存调优器 — 自动调整缓存 TTL 或禁用缓存 (Session 82)
    ///
    /// 启用后, 在每次编译检查后读取 DevTrace 条目, 构建 CacheFixCorrelation,
    /// 评估缓存命中与未命中的修复成功率差值, 自动调整 TTL 或禁用缓存。
    ///
    /// 需要同时启用 DevTrace (`with_dev_trace(true)`) 才能工作。
    ///
    /// # 参数
    ///
    /// - `tuner`: 已配置的 CacheTuner (含初始 TTL 和调优配置)
    pub fn with_cache_tuner(mut self, tuner: CacheTuner) -> Self {
        self.cache_tuner = Some(tuner);
        self
    }

    /// 启用搜索质量评估器 — 评估自动搜索对修复成功率的影响 (Session 85)
    ///
    /// 启用后, 在每次编译检查后评估搜索质量,
    /// 当搜索有害 (搜索后修复率明显低于不搜索) 时自动禁用搜索功能。
    ///
    /// 需要同时启用 DevTrace (`with_dev_trace(true)`) 才能工作。
    ///
    /// # 参数
    ///
    /// - `evaluator`: 已配置的 SearchQualityEvaluator
    pub fn with_search_quality_evaluator(mut self, evaluator: SearchQualityEvaluator) -> Self {
        self.search_quality_evaluator = Some(evaluator);
        self
    }

    /// 启用 Memory 上下文注入 — 修复轮次中注入近期对话历史 (Session 89)
    ///
    /// 启用后 (>0), 在 `send_attempt_prompt` 的修复轮次中,
    /// 从 Memory 对话历史提取最近 `count` 条对话注入消息列表。
    /// 结合增量发送机制, 已发送的历史消息会被自动跳过,
    /// 只发送新增的修复 prompt, 同时为 AI 提供更完整的上下文。
    ///
    /// # 设计理念
    ///
    /// 在多轮修复中, AI 的对话历史已经在上下文中。
    /// 将历史消息加入消息列表后, 增量跟踪会自动跳过已发送的部分,
    /// 避免重复发送相同的上下文, 同时确保 AI 有足够的上下文进行修复。
    ///
    /// # 参数
    ///
    /// - `count`: 提取最近多少条对话 (0 = 禁用, 建议值 3~5)
    ///
    /// # 示例
    ///
    /// ```
    /// # use forge::orchestrator::Orchestrator;
    /// # // builder 模式配置
    /// # // let orch = Orchestrator::new(...).with_memory_context(3);
    /// ```
    pub fn with_memory_context(mut self, count: usize) -> Self {
        self.memory_context_count = count;
        self
    }

    /// 启用 Memory 评估器 — 评估 Memory 上下文注入效果 (Session 90)
    ///
    /// 启用后, 在每次编译检查后评估 Memory 注入对修复成功率的影响,
    /// 当注入有害时自动禁用 Memory 上下文注入。
    pub fn with_memory_evaluator(mut self, evaluator: MemoryContextEvaluator) -> Self {
        self.memory_evaluator = Some(evaluator);
        self
    }

    /// 启用联合决策引擎 — 三评估器协同决策 (Session 99)
    ///
    /// 启用后, 在每次编译检查后综合三个评估器的状态, 做出联合决策:
    /// - 2+ 评估器禁用 → 升级警告
    /// - 全部评估器禁用 → 进入保守模式 (跳过所有自动增强)
    /// - 保守模式 N 轮后 → 尝试重新启用功能
    ///
    /// 需要同时启用 DevTrace (`with_dev_trace(true)`) 才能工作。
    ///
    /// # 参数
    ///
    /// - `engine`: 已配置的 JointDecisionEngine
    pub fn with_joint_decision_engine(mut self, engine: JointDecisionEngine) -> Self {
        self.joint_decision_engine = Some(engine);
        self
    }

    /// 启用/禁用自动修复 — 代码提取后自动应用 apply_fixes (Session 118)
    ///
    /// 启用后, 在从 AI 回复提取代码文件后, 对 Rust (.rs) 文件自动调用
    /// `apply_fixes` 修复质量问题, 并打印修复摘要。
    pub fn with_auto_fix(mut self, enabled: bool) -> Self {
        self.auto_fix_enabled = enabled;
        self
    }

    /// 启用/禁用 clippy 检查 (Session 120)
    ///
    /// 启用后, 在代码写入工作区后自动运行 `cargo clippy`,
    /// 打印 clippy 警告和错误, 帮助及早发现代码质量问题。
    pub fn with_clippy_check(mut self, enabled: bool) -> Self {
        self.clippy_check_enabled = enabled;
        self
    }

    /// 启用/禁用分阶段修复 (Session 121)
    ///
    /// 启用后, 替代 `apply_fixes` 的一次性修复, 改为三阶段按优先级分批修复:
    /// 1. 高优先级 (unwrap/expect/todo/panic/unreachable/missing_result_return)
    /// 2. 中优先级 (unsafe/unwrap_or/#[must_use])
    /// 3. 低优先级 (文档注释)
    ///
    /// 优势: 高优先级修复不会因低优先级修复的行号偏移而错位。
    pub fn with_staged_fix(mut self, enabled: bool) -> Self {
        self.staged_fix_enabled = enabled;
        self
    }

    /// 启用/禁用修复预览 (Session 123)
    ///
    /// 启用后, 在分阶段修复时显示每个阶段的预览信息,
    /// 包括变化行数和 diff 摘要, 但不实际修改文件内容。
    /// 需要同时启用 `auto_fix_enabled` 和 `staged_fix_enabled`。
    pub fn with_fix_preview(mut self, enabled: bool) -> Self {
        self.fix_preview_enabled = enabled;
        self
    }

    /// 对项目运行 clippy 检查并打印结果 (Session 120)
    ///
    /// 在代码写入工作区后调用, 如果 clippy 发现问题则打印警告和错误。
    /// 此方法不会因 clippy 失败而中断流程, 仅打印信息供参考。
    fn run_clippy_on_project(&self) {
        use crate::extract::run_clippy_check;

        let project_dir = self.workspace.root.to_str().unwrap_or(".");
        match run_clippy_check(project_dir, false) {
            Ok(messages) => {
                if messages.is_empty() {
                    println!("    ✅ clippy 检查通过, 无警告");
                } else {
                    println!("    ⚠️  clippy 发现 {} 个问题:", messages.len());
                    for msg in &messages {
                        println!("      {}", msg);
                    }
                }
            }
            Err(e) => {
                debug!("clippy 检查跳过: {}", e);
            }
        }
    }

    /// 对提取的文件应用自动修复 (Session 118, Session 121 增强分阶段修复)
    ///
    /// 遍历所有提取的文件, 对 Rust (.rs) 文件调用 `apply_fixes` 或 `apply_staged_fixes`,
    /// 修复质量问题 (unwrap → ?, 添加 #[must_use], 文档注释等)。
    /// 非 Rust 文件不做处理。
    ///
    /// 当 `staged_fix_enabled` 为 true 时, 使用 `apply_staged_fixes` 分阶段修复,
    /// 否则使用 `apply_fixes` 一次性修复。
    ///
    /// 返回修复后的文件列表, 并打印修复摘要。
    fn apply_auto_fixes_to_files(
        &self,
        files: Vec<crate::extract::ExtractedFile>,
    ) -> Vec<crate::extract::ExtractedFile> {
        use crate::extract::{
            apply_fixes_dry_run, apply_staged_fixes, apply_staged_fixes_preview,
            compute_line_diff_unified, format_diff_summary, format_diff_unified_with_options,
        };

        let mut fixed_files = Vec::with_capacity(files.len());
        let mut total_fixes = 0usize;
        let mut fixed_count = 0usize;

        for file in files {
            if file.path.ends_with(".rs") {
                // Session 123: 预览模式 — 显示分阶段修复预览但不修改
                if self.fix_preview_enabled && self.staged_fix_enabled {
                    let preview = apply_staged_fixes_preview(&file.content);
                    if preview.total_changed {
                        println!("    👁 修复预览 {}:", file.path);
                        if preview.stage1_changed {
                            println!("      阶段 1 (高优先级): 有变化");
                            let diff = compute_line_diff_unified(
                                &preview.original_content,
                                &preview.stage1_result,
                            );
                            let summary = format_diff_summary(&diff);
                            for line in summary.lines().take(5) {
                                println!("        {}", line);
                            }
                        }
                        if preview.stage2_changed {
                            println!("      阶段 2 (中优先级): 有变化");
                        }
                        if preview.stage3_changed {
                            println!("      阶段 3 (低优先级): 有变化");
                        }
                    }
                    // 预览模式: 不修改文件内容
                    fixed_files.push(file);
                    continue;
                }

                // Session 121: 分阶段修复时直接使用 apply_staged_fixes
                let fixed_content = if self.staged_fix_enabled {
                    apply_staged_fixes(&file.content)
                } else {
                    let preview = apply_fixes_dry_run(&file.content);
                    if preview.is_changed {
                        total_fixes += preview.fixes_applied;
                        fixed_count += 1;
                        println!(
                            "    🔧 自动修复 {}: {} 处修复",
                            file.path, preview.fixes_applied
                        );
                        for issue in &preview.issues {
                            debug!(
                                "  {}:{} — {:?}: {}",
                                file.path, issue.line, issue.issue_type, issue.message
                            );
                        }
                        preview.fixed_content
                    } else {
                        fixed_files.push(file);
                        continue;
                    }
                };

                // 分阶段修复模式: 检查是否有变化
                if self.staged_fix_enabled {
                    if fixed_content != file.content {
                        fixed_count += 1;
                        println!("    🔧 分阶段修复 {} (高→中→低优先级)", file.path);

                        // Session 123: 打印 diff 摘要
                        let diffs = compute_line_diff_unified(&file.content, &fixed_content);
                        let summary = format_diff_summary(&diffs);
                        for line in summary.lines().take(10) {
                            println!("        {}", line);
                        }

                        // Session 125: 打印统一 diff (类似 git diff)
                        let unified = format_diff_unified_with_options(
                            &file.content,
                            &fixed_content,
                            &file.path,
                            &file.path,
                            3,
                        );
                        if !unified.is_empty() {
                            println!("        📝 统一 diff:");
                            for line in unified.lines().take(20) {
                                println!("          {}", line);
                            }
                        }

                        fixed_files.push(crate::extract::ExtractedFile {
                            content: fixed_content,
                            ..file
                        });
                    } else {
                        fixed_files.push(file);
                    }
                } else {
                    // Session 123: 非分阶段模式也打印 diff 摘要
                    if fixed_content != file.content {
                        let diffs = compute_line_diff_unified(&file.content, &fixed_content);
                        let summary = format_diff_summary(&diffs);
                        for line in summary.lines().take(10) {
                            println!("        {}", line);
                        }

                        // Session 125: 打印统一 diff
                        let unified = format_diff_unified_with_options(
                            &file.content,
                            &fixed_content,
                            &file.path,
                            &file.path,
                            3,
                        );
                        if !unified.is_empty() {
                            println!("        📝 统一 diff:");
                            for line in unified.lines().take(20) {
                                println!("          {}", line);
                            }
                        }
                    }
                    fixed_files.push(crate::extract::ExtractedFile {
                        content: fixed_content,
                        ..file
                    });
                }
            } else {
                fixed_files.push(file);
            }
        }

        if total_fixes > 0 || (self.staged_fix_enabled && fixed_count > 0) {
            if self.staged_fix_enabled {
                println!("    🔧 分阶段修复摘要: {} 个文件已修复", fixed_count);
            } else {
                println!(
                    "    🔧 自动修复摘要: {} 个文件, {} 处修复",
                    fixed_count, total_fixes
                );
            }
        }

        fixed_files
    }

    /// 计算增量消息 — 使用 LiveContinuation 和 ConversationTracker
    ///
    /// 1. 如果启用了 ConversationTracker, 先用 Radix Tree 查找最长公共前缀
    /// 2. 对增量部分, 用 LiveContinuation 检查单条消息是否已发送
    /// 3. 返回最终的增量消息
    ///
    /// 如果两者都未启用, 返回完整消息 (向后兼容)。
    pub fn compute_incremental_messages(&self, messages: &[String]) -> Vec<String> {
        if messages.is_empty() {
            return vec![];
        }

        // 第一步: 对话级增量 (Radix Tree)
        let after_conversation_delta = if let Some(ref tracker) = self.conversation_tracker {
            tracker.compute_delta(messages)
        } else {
            messages.to_vec()
        };

        // 第二步: 消息级增量 (LiveContinuation)
        if let Some(ref lc) = self.live_continuation {
            lc.compute_delta(&after_conversation_delta).delta_messages
        } else {
            after_conversation_delta
        }
    }

    /// 标记消息为已发送 — 更新 LiveContinuation 和 ConversationTracker
    ///
    /// 在消息成功发送后调用, 将消息注册到跟踪器中。
    pub fn mark_messages_sent(&mut self, messages: &[String]) {
        if let Some(ref mut lc) = self.live_continuation {
            lc.mark_sent(messages);
        }
        if let Some(ref mut tracker) = self.conversation_tracker {
            tracker.mark_sent(messages);
        }
    }

    /// 重置增量跟踪状态 — 对话被压缩或新开后调用
    ///
    /// 清空 LiveContinuation 和 ConversationTracker 的跟踪状态,
    /// 使下次发送为全量发送。
    pub fn reset_continuation(&mut self) {
        if let Some(ref mut lc) = self.live_continuation {
            lc.reset();
        }
        if let Some(ref mut tracker) = self.conversation_tracker {
            tracker.clear();
        }
    }

    /// 带增量跟踪的消息发送 — Session 75 实战集成
    ///
    /// 结合 LiveContinuation 和 ConversationTracker 的增量计算能力,
    /// 只发送未发送过的增量消息, 而非完整上下文。
    ///
    /// ## 工作流
    ///
    /// 1. 调用 `compute_incremental_messages` 计算增量
    /// 2. 将增量消息拼接为单条消息发送 (通过 `send_message_safe`)
    /// 3. 发送成功后调用 `mark_messages_sent` 更新跟踪器状态
    /// 4. 更新 `incremental_stats` 统计计数器
    /// 5. 通过 `trace_dev` 记录增量发送统计到 DevTrace
    ///
    /// ## 向后兼容
    ///
    /// 如果 `live_continuation` 和 `conversation_tracker` 均未启用,
    /// 退化为将所有消息拼接后通过 `send_message_safe` 发送。
    ///
    /// ## 参数
    ///
    /// - `messages`: 要发送的消息列表 (按顺序)
    /// - `timeout`: 发送超时 (秒)
    ///
    /// ## 返回
    ///
    /// 返回 `ChatResult` 或错误。
    pub async fn send_with_continuation(
        &mut self,
        messages: &[String],
        timeout: u64,
    ) -> Result<crate::traits::ChatResult> {
        if messages.is_empty() {
            return Err(anyhow::anyhow!("send_with_continuation: 消息列表为空"));
        }

        let total_count = messages.len();

        // 1. 计算增量消息
        let delta_messages = self.compute_incremental_messages(messages);
        let sent_count = delta_messages.len();
        let skipped_count = total_count.saturating_sub(sent_count);

        // 2. 更新统计计数器
        self.incremental_stats.record(total_count, sent_count);

        // 3. 如果增量为空 (全部已发送), 返回一个虚拟结果
        if delta_messages.is_empty() {
            info!(
                "📦 增量发送: 全部 {} 条消息已发送过, 跳过 (节省 100%)",
                total_count
            );

            // 记录 DevTrace
            self.trace_dev(
                TraceAction::IncrementalSend,
                None,
                None,
                None,
                &format!(
                    "[全部跳过] total={}, sent=0, skipped={}",
                    total_count, total_count
                ),
                "[无增量发送]",
                0,
                true,
                None,
            );

            // 返回一个空结果 (调用方应检查 text 是否为空)
            return Ok(crate::traits::ChatResult {
                text: String::new(),
                timed_out: false,
            });
        }

        // 4. 将增量消息拼接为单条消息
        let combined_msg = if delta_messages.len() == 1 {
            delta_messages[0].clone()
        } else {
            // 多条消息用换行分隔
            delta_messages.join("\n\n")
        };

        // 5. 记录增量发送信息
        if skipped_count > 0 {
            info!(
                "📦 增量发送: 总 {} 条, 发送 {} 条, 跳过 {} 条 (节省 {:.1}%)",
                total_count,
                sent_count,
                skipped_count,
                (skipped_count as f64 / total_count as f64) * 100.0
            );
        } else {
            info!("📦 全量发送: {} 条消息 (无增量优化)", total_count);
        }

        // 6. 发送消息 (通过 send_message_safe 带连接监控)
        let send_start = Instant::now();
        let result = self.send_message_safe(&combined_msg, timeout).await?;
        let send_duration = send_start.elapsed().as_millis() as u64;

        // 7. 发送成功后, 标记所有消息为已发送
        self.mark_messages_sent(messages);

        // 8. 记录 DevTrace
        let stats_summary = format!(
            "total={}, sent={}, skipped={}, saved_ratio={:.1}%, cumulative: total={}, skipped={}",
            total_count,
            sent_count,
            skipped_count,
            if total_count > 0 {
                skipped_count as f64 / total_count as f64 * 100.0
            } else {
                0.0
            },
            self.incremental_stats.total_messages,
            self.incremental_stats.skipped_messages
        );

        self.trace_dev(
            TraceAction::IncrementalSend,
            None,
            None,
            None,
            &stats_summary,
            &result.text,
            send_duration,
            !result.timed_out,
            if result.timed_out {
                Some("增量发送超时")
            } else {
                None
            },
        );

        Ok(result)
    }

    /// 获取增量发送统计的只读引用
    ///
    /// 返回累计的增量发送统计数据, 包括总消息数、实际发送数、
    /// 跳过数和节省比例。
    pub fn incremental_stats(&self) -> &crate::dev_trace::IncrementalStats {
        &self.incremental_stats
    }

    /// 从 Memory 对话历史构建消息列表 (Task 5 — 多轮对话增量优化)
    ///
    /// 提取最近 `recent_count` 条对话, 构建为消息列表。
    /// 结合 `send_with_continuation` 使用, 可实现增量发送:
    /// - 已发送的对话消息会被增量跟踪跳过
    /// - 只发送新增的对话消息 + 当前 prompt
    ///
    /// # 设计理念
    ///
    /// 在多轮修复中, AI 的对话历史已经在上下文中。
    /// 将历史消息加入消息列表后, 增量跟踪会自动跳过已发送的部分,
    /// 避免重复发送相同的上下文。
    ///
    /// # 参数
    ///
    /// - `recent_count`: 提取最近多少条对话 (0 = 不提取)
    ///
    /// # 示例
    ///
    /// ```
    /// # use forge::orchestrator::Orchestrator;
    /// # // build_messages_from_memory 是内部方法, 此处仅展示概念
    /// # // let messages = orch.build_messages_from_memory(3);
    /// # // assert!(messages.len() <= 3);
    /// ```
    fn build_messages_from_memory(&self, recent_count: usize) -> Vec<String> {
        if recent_count == 0 {
            return vec![];
        }
        let conversations = &self.memory.conversations;
        let start = conversations.len().saturating_sub(recent_count);
        conversations[start..]
            .iter()
            .filter(|c| !c.content.is_empty())
            .map(|c| c.content.clone())
            .collect()
    }

    /// 发送尝试 prompt — 支持增量发送 (Task 3 — send_with_continuation 实际调用)
    ///
    /// 在 `execute_task` 的修复循环中替代 `send_message_safe`:
    ///
    /// ## 增量跟踪启用时 (live_continuation 或 conversation_tracker)
    ///
    /// - **首次尝试** (attempt == 1):
    ///   构建 `[steered_prompt]` 单条消息列表 → 全量发送
    /// - **修复轮次** (attempt > 1):
    ///   构建 `[first_prompt, fix_prompt]` 消息列表 →
    ///   `first_prompt` 已发送过被跳过, 只发送 `fix_prompt`
    ///
    /// 这样系统提示 + 上下文 + 任务 prompt 等稳定部分在修复轮次中被跳过,
    /// 只发送变化的修复 prompt, 大幅减少 token 消耗。
    ///
    /// ## 增量跟踪未启用时 (向后兼容)
    ///
    /// 退化为 `send_message_safe`, 行为与之前完全一致。
    ///
    /// # 参数
    ///
    /// - `steered_prompt`: 经转向提醒处理后的 prompt
    /// - `first_prompt`: 首次尝试的 prompt (用于修复轮次的增量跳过)
    ///   `None` 表示尚未存储首次 prompt
    /// - `is_fix`: 是否为修复轮次 (attempt > 1)
    /// - `timeout`: 发送超时 (秒)
    async fn send_attempt_prompt(
        &mut self,
        steered_prompt: &str,
        first_prompt: &Option<String>,
        is_fix: bool,
        timeout: u64,
    ) -> Result<crate::traits::ChatResult> {
        if self.live_continuation.is_some() || self.conversation_tracker.is_some() {
            // 构建增量发送的消息列表
            let (messages, memory_injected) = if is_fix {
                // === Memory 上下文注入 (Session 89) ===
                // 修复轮次中, 从 Memory 提取近期对话历史注入消息列表
                let memory_messages = if self.memory_context_count > 0 {
                    let msgs = self.build_messages_from_memory(self.memory_context_count);
                    if !msgs.is_empty() {
                        info!("    📝 Memory 上下文注入: {} 条近期对话", msgs.len());
                    }
                    msgs
                } else {
                    vec![]
                };

                let injected = memory_messages.len();
                // 使用纯函数构建消息列表: [first_prompt?, ...memory_messages, fix_prompt]
                let msgs =
                    build_fix_messages_with_memory(first_prompt, steered_prompt, &memory_messages);
                (msgs, injected)
            } else {
                // 首次尝试: [完整 prompt] → 全量发送
                (vec![steered_prompt.to_string()], 0)
            };

            // 快照增量统计 (用于计算 memory context 的跳过数)
            let skipped_before = self.incremental_stats.skipped_messages;

            // 通过 send_with_continuation 发送 (自动计算增量 + 更新统计 + DevTrace)
            let result = self.send_with_continuation(&messages, timeout).await?;

            // 更新 Memory 上下文注入统计 (跳过数 = 本次发送的总跳过数)
            if memory_injected > 0 {
                let skipped_after = self.incremental_stats.skipped_messages;
                let skipped_delta = skipped_after.saturating_sub(skipped_before);
                self.memory_context_stats
                    .record_injection(memory_injected, skipped_delta);

                // DevTrace: 记录 Memory 注入 (Session 90)
                self.trace_dev(
                    TraceAction::MemoryInjection,
                    None,
                    None,
                    None,
                    &format!("注入 {} 条消息", memory_injected),
                    &format!("跳过 {} 条", skipped_delta),
                    0,
                    true,
                    None,
                );
            }

            Ok(result)
        } else {
            // 向后兼容: 增量跟踪未启用, 直接发送
            self.send_message_safe(steered_prompt, timeout).await
        }
    }

    /// 编译错误自动搜索 — web_tool 深度集成 + 搜索结果缓存 (Session 78)
    ///
    /// 当编译/测试失败时, 自动从错误信息中提取关键词,
    /// 通过 WebTool 搜索解决方案, 返回格式化的搜索结果段落。
    ///
    /// ## 设计理念
    ///
    /// 借鉴 ds4 的 "auto-search on error" 模式:
    /// - 编译错误 → 提取关键词 → Google 搜索 → 结果注入修复 prompt
    /// - AI 获得额外上下文, 提高首次修复成功率
    /// - 与 `/search` slash command 互补: 主动搜索 vs AI 自主搜索
    ///
    /// ## 搜索结果缓存 (Session 78)
    ///
    /// 相同错误代码 (如 E0308) 的搜索结果会被缓存, 避免重复搜索:
    /// 1. 从错误列表构建缓存键 (优先使用 error_code)
    /// 2. 检查缓存 → 命中则直接返回 (0ms, 节省搜索时间)
    /// 3. 未命中 → 执行 WebTool 搜索 → 结果存入缓存
    /// 4. DevTrace 记录缓存命中/未命中
    ///
    /// ## 条件
    ///
    /// - `web_tool` 必须已启用 (否则返回 None)
    /// - 错误列表非空
    /// - 非首次尝试 (attempt > 1, 首次失败直接让 AI 修复)
    /// - 非网络错误
    ///
    /// # 参数
    ///
    /// - `errors`: 编译错误列表
    /// - `attempt`: 当前尝试轮次 (1-based)
    /// - `is_network_error`: 是否为网络错误
    /// - `phase_idx`: 阶段索引 (DevTrace)
    /// - `task_idx`: 任务索引 (DevTrace)
    ///
    /// # 返回
    ///
    /// - `Ok(Some(section))`: 搜索结果格式化段落, 追加到修复 prompt
    /// - `Ok(None)`: 未搜索 (条件不满足或搜索结果为空)
    /// - `Err(e)`: 搜索过程出错 (非致命, 调用方应忽略)
    async fn auto_search_error_solutions(
        &mut self,
        errors: &[CompileError],
        attempt: u32,
        is_network_error: bool,
        phase_idx: usize,
        task_idx: usize,
    ) -> Result<Option<String>> {
        // 条件检查: 是否应该搜索
        if !error_search::should_search_errors(errors, attempt, is_network_error) {
            return Ok(None);
        }

        // === 搜索质量评估器禁用检查 (Session 85) ===
        // 当搜索质量评估器判定搜索有害并禁用后, 跳过自动搜索
        if self
            .search_quality_evaluator
            .as_ref()
            .is_some_and(|e| !e.is_enabled())
        {
            debug!("    🔍 自动搜索: 搜索质量评估器已禁用搜索, 跳过");
            return Ok(None);
        }

        // 构建搜索查询
        let query = match error_search::build_error_search_query(errors) {
            Some(q) => q,
            None => {
                debug!("    🔍 自动搜索: 无法从错误中构建搜索查询");
                return Ok(None);
            }
        };

        // WebTool 必须已启用
        let web_tool = match self.web_tool.as_ref() {
            Some(tool) => tool,
            None => {
                debug!("    🔍 自动搜索: WebTool 未启用, 跳过");
                return Ok(None);
            }
        };

        // === 搜索结果缓存: 构建缓存键并检查缓存 (Session 78) ===
        // CacheTuner 禁用缓存时, 跳过缓存查找和插入 (Session 82)
        let cache_enabled = self.cache_tuner.as_ref().is_none_or(|t| t.is_enabled());
        let cache_key = if cache_enabled {
            search_cache::build_cache_key(errors)
        } else {
            None
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        if let Some(ref key) = cache_key {
            if let Some(cached) = self.search_cache.get(key, now) {
                info!(
                    "    🔍 自动搜索: 缓存命中 (key={}, 命中次数={})",
                    key, cached.hit_count
                );
                println!("    🔍 搜索缓存命中 ({}), 跳过网络搜索", key);

                // DevTrace: 记录缓存命中
                let as_task_name = self.memory.phases[phase_idx].tasks[task_idx].name.clone();
                self.trace_dev(
                    TraceAction::WebSearch,
                    Some(phase_idx),
                    Some(task_idx),
                    Some(&as_task_name),
                    &query,
                    &cached.content,
                    0, // duration=0 for cache hit
                    true,
                    Some(&format!(
                        "缓存命中 (key={}, 原始耗时={}ms, 命中次数={})",
                        key, cached.duration_ms, cached.hit_count
                    )),
                );

                // 格式化缓存的搜索结果段落
                let section = error_search::format_search_results_section(
                    &cached.query,
                    &cached.content,
                    cached.duration_ms,
                );

                return Ok(Some(section));
            }
        }

        info!("    🔍 自动搜索: '{}'", query);
        println!("    🔍 自动搜索错误解决方案: {}", query);

        // 创建取消令牌
        let cancel_source = self.create_cancellation_token();
        let cancel_token = cancel_source.token();

        // 执行搜索
        let search_start = Instant::now();
        let search_result = web_tool.search_web(&query, None, Some(&cancel_token)).await;
        let search_duration = search_start.elapsed().as_millis() as u64;

        match search_result {
            Ok(result) => {
                if result.content.is_empty() {
                    debug!("    🔍 自动搜索: 搜索结果为空");
                    return Ok(None);
                }

                // === 搜索结果缓存: 存入缓存 (Session 78) ===
                // cache_key 为 None 时表示缓存已禁用 (Session 82)
                if let Some(ref key) = cache_key {
                    let entry = CachedSearchEntry::with_timestamp(
                        result.query.clone(),
                        result.content.clone(),
                        result.duration_ms,
                        now,
                    );
                    self.search_cache.insert(key.clone(), entry, now);
                    debug!("    🔍 自动搜索: 结果已缓存 (key={})", key);
                }

                info!(
                    "    ✅ 自动搜索完成: {}ms, {} 字符",
                    search_duration,
                    result.content.len()
                );
                println!("    ✅ 搜索结果已获取 ({} 字符)", result.content.len());

                // DevTrace: 记录自动搜索
                let as_task_name = self.memory.phases[phase_idx].tasks[task_idx].name.clone();
                let cache_note = if cache_key.is_some() {
                    Some("编译错误自动搜索 (已缓存)")
                } else if !cache_enabled {
                    Some("编译错误自动搜索 (缓存已禁用)")
                } else {
                    Some("编译错误自动搜索")
                };
                self.trace_dev(
                    TraceAction::WebSearch,
                    Some(phase_idx),
                    Some(task_idx),
                    Some(&as_task_name),
                    &query,
                    &result.content,
                    search_duration,
                    true,
                    cache_note,
                );

                // 格式化搜索结果段落
                let section = error_search::format_search_results_section(
                    &result.query,
                    &result.content,
                    result.duration_ms,
                );

                Ok(Some(section))
            }
            Err(e) => {
                warn!("    ❌ 自动搜索失败: {}", e);
                println!("    ❌ 自动搜索失败: {}", e);

                // DevTrace: 记录搜索失败
                let as_task_name = self.memory.phases[phase_idx].tasks[task_idx].name.clone();
                self.trace_dev(
                    TraceAction::WebSearch,
                    Some(phase_idx),
                    Some(task_idx),
                    Some(&as_task_name),
                    &query,
                    "",
                    search_duration,
                    false,
                    Some(&format!("搜索失败: {}", e)),
                );

                // 搜索失败是非致命的, 返回 None 不影响修复流程
                Ok(None)
            }
        }
    }

    /// 缓存策略自动调优 — 评估并应用缓存调优决策 (Session 82)
    ///
    /// 在每次编译检查后调用, 基于 DevTrace 中的 WebSearch + CompileCheck 条目
    /// 构建 `CacheFixCorrelation`, 评估缓存命中与未命中的修复成功率差值,
    /// 自动调整 TTL (缩短/延长) 或禁用缓存。
    ///
    /// ## 条件
    ///
    /// - `cache_tuner` 必须已启用 (否则跳过)
    /// - `dev_trace` 必须已启用 (否则跳过, 无数据可分析)
    ///
    /// ## 决策应用
    ///
    /// - `AdjustTtl { new_ttl }` → `search_cache.set_ttl(new_ttl)`
    /// - `DisableCache` → `search_cache.clear()` (清除缓存条目)
    /// - `KeepCurrent` → 无操作
    ///
    /// # 参数
    ///
    /// - `phase_idx`: 阶段索引 (DevTrace)
    /// - `task_idx`: 任务索引 (DevTrace)
    fn evaluate_cache_tuning(&mut self, phase_idx: usize, task_idx: usize) {
        let (tuner, trace_writer) = match (&mut self.cache_tuner, &self.dev_trace) {
            (Some(t), Some(w)) => (t, w),
            _ => return, // 未启用, 跳过
        };

        // 读取 DevTrace 条目, 构建缓存修复关联分析
        let entries = match trace_writer.read_all() {
            Ok(e) => e,
            Err(e) => {
                warn!("    📊 缓存调优: 读取 DevTrace 失败: {}", e);
                return;
            }
        };

        if entries.is_empty() {
            debug!("    📊 缓存调优: 无 DevTrace 条目, 跳过");
            return;
        }

        let corr = build_cache_fix_correlation(&entries);
        let stats = self.search_cache.stats().clone();

        // 评估并应用调优决策
        let decision = tuner.evaluate_and_apply(&corr, &stats);

        // 应用决策到 SearchCache
        match &decision.action {
            crate::cache_tuning::TuningAction::KeepCurrent => {
                debug!("    📊 缓存调优: {}", decision.reason);
            }
            crate::cache_tuning::TuningAction::AdjustTtl { new_ttl } => {
                info!(
                    "    📊 缓存调优: 调整 TTL {}s → {}s (差值 {:+.1}%)",
                    decision.old_ttl,
                    new_ttl,
                    decision.correlation_diff * 100.0
                );
                println!("    📊 缓存调优: TTL {}s → {}s", decision.old_ttl, new_ttl);
                self.search_cache.set_ttl(*new_ttl);
            }
            crate::cache_tuning::TuningAction::DisableCache => {
                info!(
                    "    📊 缓存调优: 禁用缓存 (差值 {:+.1}%)",
                    decision.correlation_diff * 100.0
                );
                println!(
                    "    📊 缓存调优: 禁用缓存 ({:.1}% 差值)",
                    decision.correlation_diff * 100.0
                );
                self.search_cache.clear();
            }
        }

        // DevTrace: 记录缓存调优决策
        let ct_task_name = self.memory.phases[phase_idx].tasks[task_idx].name.clone();
        self.trace_dev(
            TraceAction::CacheTuning,
            Some(phase_idx),
            Some(task_idx),
            Some(&ct_task_name),
            &format!(
                "hit={}/{} miss={}/{}",
                corr.successes_after_hit,
                corr.checks_after_hit,
                corr.successes_after_miss,
                corr.checks_after_miss
            ),
            &decision.to_summary(),
            0,
            true,
            Some(&decision.reason),
        );
    }

    /// 搜索质量评估 — 评估并应用搜索质量决策 (Session 85)
    ///
    /// 在每次编译检查后调用, 基于 DevTrace 中的 WebSearch + CompileCheck 条目
    /// 构建 `SearchQualityStats`, 评估搜索与不搜索的修复成功率差值,
    /// 当搜索有害时自动禁用搜索功能。
    ///
    /// ## 条件
    ///
    /// - `search_quality_evaluator` 必须已启用 (否则跳过)
    /// - `dev_trace` 必须已启用 (否则跳过, 无数据可分析)
    ///
    /// ## 决策应用
    ///
    /// - `DisableSearch` → 后续 `auto_search_error_solutions` 跳过搜索
    /// - `KeepSearching` / `InsufficientData` → 无操作
    ///
    /// # 参数
    ///
    /// - `phase_idx`: 阶段索引 (DevTrace)
    /// - `task_idx`: 任务索引 (DevTrace)
    fn evaluate_search_quality(&mut self, phase_idx: usize, task_idx: usize) {
        let (evaluator, trace_writer) = match (&mut self.search_quality_evaluator, &self.dev_trace)
        {
            (Some(e), Some(w)) => (e, w),
            _ => return, // 未启用, 跳过
        };

        // 读取 DevTrace 条目, 构建搜索质量统计
        let entries = match trace_writer.read_all() {
            Ok(e) => e,
            Err(e) => {
                warn!("    🔍 搜索质量: 读取 DevTrace 失败: {}", e);
                return;
            }
        };

        if entries.is_empty() {
            debug!("    🔍 搜索质量: 无 DevTrace 条目, 跳过");
            return;
        }

        let stats = build_search_quality_stats(&entries);

        // 评估并应用质量决策
        let decision = evaluator.evaluate_and_apply(&stats);

        // 应用决策
        match &decision.action {
            crate::search_quality::SearchQualityAction::KeepSearching => {
                debug!("    🔍 搜索质量: {}", decision.reason);
            }
            crate::search_quality::SearchQualityAction::InsufficientData => {
                debug!("    🔍 搜索质量: {}", decision.reason);
            }
            crate::search_quality::SearchQualityAction::DisableSearch => {
                info!(
                    "    🔍 搜索质量: 禁用搜索 (差值 {:+.1}%)",
                    decision.diff * 100.0
                );
                println!(
                    "    🔍 搜索质量: 禁用自动搜索 ({:+.1}% 差值, 搜索有害)",
                    decision.diff * 100.0
                );
            }
        }

        // DevTrace: 记录搜索质量决策
        let sq_task_name = self.memory.phases[phase_idx].tasks[task_idx].name.clone();
        self.trace_dev(
            TraceAction::SearchQuality,
            Some(phase_idx),
            Some(task_idx),
            Some(&sq_task_name),
            &format!(
                "with={}/{} without={}/{}",
                stats.successes_with_search,
                stats.checks_with_search,
                stats.successes_without_search,
                stats.checks_without_search,
            ),
            &decision.to_trace_summary(),
            0,
            true,
            Some(&decision.reason),
        );
    }

    /// Memory 上下文注入效果评估 — 评估并应用决策 (Session 90)
    ///
    /// 在每次编译检查后调用, 基于 DevTrace 中的 MemoryInjection + CompileCheck 条目
    /// 构建 `MemoryEvaluationStats`, 评估有注入 vs 无注入的修复成功率差值,
    /// 当注入有害时自动禁用 Memory 上下文注入。
    ///
    /// ## 条件
    ///
    /// - `memory_evaluator` 必须已启用 (否则跳过)
    /// - `dev_trace` 必须已启用 (否则跳过)
    ///
    /// ## 决策应用
    ///
    /// - `DisableInjection` → 后续 `send_attempt_prompt` 不注入 Memory 上下文
    /// - `KeepInjecting` / `InsufficientData` → 无操作
    ///
    /// # 参数
    ///
    /// - `phase_idx`: 阶段索引 (DevTrace)
    /// - `task_idx`: 任务索引 (DevTrace)
    fn evaluate_memory_context(&mut self, phase_idx: usize, task_idx: usize) {
        let (evaluator, trace_writer) = match (&mut self.memory_evaluator, &self.dev_trace) {
            (Some(e), Some(w)) => (e, w),
            _ => return, // 未启用, 跳过
        };

        // 读取 DevTrace 条目, 构建 Memory 评估统计
        let entries = match trace_writer.read_all() {
            Ok(e) => e,
            Err(e) => {
                warn!("    📝 Memory 评估: 读取 DevTrace 失败: {}", e);
                return;
            }
        };

        if entries.is_empty() {
            debug!("    📝 Memory 评估: 无 DevTrace 条目, 跳过");
            return;
        }

        let stats = build_memory_evaluation_stats(&entries);

        // 评估并应用决策
        let decision = evaluator.evaluate_and_apply(
            stats.checks_with_memory,
            stats.successes_with_memory,
            stats.checks_without_memory,
            stats.successes_without_memory,
        );

        // 应用决策
        match &decision.action {
            crate::memory_evaluation::MemoryEvaluationAction::KeepInjecting => {
                debug!("    📝 Memory 评估: {}", decision.reason);
            }
            crate::memory_evaluation::MemoryEvaluationAction::InsufficientData => {
                debug!("    📝 Memory 评估: {}", decision.reason);
            }
            crate::memory_evaluation::MemoryEvaluationAction::DisableInjection => {
                info!(
                    "    📝 Memory 评估: 禁用注入 (差值 {:+.1}%)",
                    decision.diff * 100.0
                );
                println!(
                    "    📝 Memory 评估: 禁用 Memory 上下文注入 ({:+.1}% 差值, 注入有害)",
                    decision.diff * 100.0
                );
                // 禁用 Memory 上下文注入
                self.memory_context_count = 0;
            }
        }

        // DevTrace: 记录 Memory 评估决策
        let me_task_name = self.memory.phases[phase_idx].tasks[task_idx].name.clone();
        self.trace_dev(
            TraceAction::MemoryEvaluation,
            Some(phase_idx),
            Some(task_idx),
            Some(&me_task_name),
            &format!(
                "with={}/{} without={}/{} injections={}",
                stats.successes_with_memory,
                stats.checks_with_memory,
                stats.successes_without_memory,
                stats.checks_without_memory,
                stats.total_injections,
            ),
            &decision.to_trace_summary(),
            0,
            true,
            Some(&decision.reason),
        );
    }

    /// 联合决策评估 — 综合三评估器状态做出联合决策 (Session 99)
    ///
    /// 在每次编译检查后 (三个评估器各自评估之后) 调用,
    /// 从 CacheTuner, SearchQualityEvaluator, MemoryContextEvaluator
    /// 构建评估器快照, 由联合决策引擎计算联合决策。
    ///
    /// ## 条件
    ///
    /// - `joint_decision_engine` 必须已启用 (否则跳过)
    ///
    /// ## 决策应用
    ///
    /// - `EnterConservativeMode` → 打印保守模式警告
    /// - `EscalateWarning` → 打印升级警告
    /// - `ReEnableFeature` → 打印重新启用建议
    /// - `NoAction` → 无操作
    ///
    /// # 参数
    ///
    /// - `phase_idx`: 阶段索引 (DevTrace)
    /// - `task_idx`: 任务索引 (DevTrace)
    fn evaluate_joint_decision(&mut self, phase_idx: usize, task_idx: usize) {
        use crate::evaluator_synergy::{EvaluatorSnapshot, EvaluatorType};
        use crate::joint_decision::JointDecisionAction;

        let engine = match &mut self.joint_decision_engine {
            Some(e) => e,
            None => return, // 未启用, 跳过
        };

        // 构建评估器快照列表
        let mut snapshots = Vec::new();
        let entries = self
            .dev_trace
            .as_ref()
            .and_then(|w| w.read_all().ok())
            .unwrap_or_default();

        // CacheTuner 快照
        if let Some(ref tuner) = self.cache_tuner {
            let corr = build_cache_fix_correlation(&entries);
            let with_rate = corr.hit_fix_rate();
            let without_rate = corr.miss_fix_rate();
            let diff = corr.hit_vs_miss_diff();
            snapshots.push(EvaluatorSnapshot {
                evaluator_type: EvaluatorType::CacheTuner,
                enabled: tuner.is_enabled(),
                with_fix_rate: with_rate,
                without_fix_rate: without_rate,
                diff,
                is_beneficial: corr.is_cache_effective(),
                total_checks: corr.checks_after_hit + corr.checks_after_miss,
                evaluation_count: tuner.decisions().len(),
                disable_count: tuner.to_history().disable_count as usize,
                contribution_score: diff,
            });
        }

        // SearchQualityEvaluator 快照
        if let Some(ref evaluator) = self.search_quality_evaluator {
            let stats = build_search_quality_stats(&entries);
            let with_rate = stats.with_search_fix_rate();
            let without_rate = stats.without_search_fix_rate();
            let diff = stats.search_vs_no_search_diff();
            snapshots.push(EvaluatorSnapshot {
                evaluator_type: EvaluatorType::SearchQuality,
                enabled: evaluator.is_enabled(),
                with_fix_rate: with_rate,
                without_fix_rate: without_rate,
                diff,
                is_beneficial: stats.is_search_beneficial(),
                total_checks: stats.checks_with_search + stats.checks_without_search,
                evaluation_count: evaluator.evaluation_count() as usize,
                disable_count: evaluator.to_history().disable_count as usize,
                contribution_score: diff,
            });
        }

        // MemoryContextEvaluator 快照
        if let Some(ref evaluator) = self.memory_evaluator {
            let stats = build_memory_evaluation_stats(&entries);
            let with_rate = stats.with_memory_fix_rate();
            let without_rate = stats.without_memory_fix_rate();
            let diff = stats.memory_vs_no_memory_diff();
            snapshots.push(EvaluatorSnapshot {
                evaluator_type: EvaluatorType::MemoryContext,
                enabled: evaluator.is_enabled(),
                with_fix_rate: with_rate,
                without_fix_rate: without_rate,
                diff,
                is_beneficial: stats.is_memory_beneficial(),
                total_checks: stats.total_checks(),
                evaluation_count: evaluator.evaluation_count() as usize,
                disable_count: evaluator.to_history().disable_count as usize,
                contribution_score: diff,
            });
        }

        // 如果没有评估器快照, 跳过
        if snapshots.is_empty() {
            return;
        }

        // 评估并获取联合决策
        let decision = engine.evaluate(&snapshots);

        // DevTrace: 记录联合决策
        let jd_task_name = self.memory.phases[phase_idx].tasks[task_idx].name.clone();
        self.trace_dev(
            TraceAction::JointDecision,
            Some(phase_idx),
            Some(task_idx),
            Some(&jd_task_name),
            &format!(
                "snapshots={}, disabled={}",
                snapshots.len(),
                decision.disabled_count,
            ),
            &decision.to_trace_summary(),
            0,
            true,
            Some(&decision.reason),
        );

        // 保守模式日志
        match &decision.action {
            JointDecisionAction::EnterConservativeMode => {
                println!(
                    "    🔒 联合决策: 进入保守模式 ({}/{} 评估器已禁用)",
                    decision.disabled_count, decision.total_evaluators,
                );
            }
            JointDecisionAction::EscalateWarning => {
                println!(
                    "    ⚠️ 联合决策: 升级警告 ({}/{} 评估器已禁用)",
                    decision.disabled_count, decision.total_evaluators,
                );
            }
            JointDecisionAction::ReEnableFeature { evaluator_type } => {
                println!(
                    "    🔄 联合决策: 尝试重新启用 {} (保守模式后恢复)",
                    evaluator_type.label(),
                );
            }
            JointDecisionAction::NoAction => {
                debug!(
                    "    联合决策: 无需行动 ({}/{} 评估器已禁用)",
                    decision.disabled_count, decision.total_evaluators,
                );
            }
        }
    }

    /// 构建三评估器协同分析摘要 (Session 91)
    ///
    /// 从 CacheTuner, SearchQualityEvaluator, MemoryContextEvaluator
    /// 的状态和 DevTrace 条目构建协同分析摘要。
    /// 只有至少一个评估器启用时才返回 Some。
    fn build_evaluator_synergy(&self) -> Option<crate::evaluator_synergy::EvaluatorSynergySummary> {
        use crate::evaluator_synergy::{
            build_evaluator_synergy_summary, EvaluatorState, EvaluatorType,
        };

        let mut states = vec![];
        let mut has_any = false;

        // CacheTuner 状态
        if let Some(ref tuner) = self.cache_tuner {
            has_any = true;
            let entries = self
                .dev_trace
                .as_ref()
                .and_then(|w| w.read_all().ok())
                .unwrap_or_default();
            let corr = build_cache_fix_correlation(&entries);
            let with_rate = corr.hit_fix_rate();
            let without_rate = corr.miss_fix_rate();
            let diff = corr.hit_vs_miss_diff();
            let is_beneficial = corr.is_cache_effective();
            let total_checks = corr.checks_after_hit + corr.checks_after_miss;
            states.push(EvaluatorState::new(
                EvaluatorType::CacheTuner,
                tuner.is_enabled(),
                with_rate,
                without_rate,
                diff,
                is_beneficial,
                total_checks,
                tuner.decisions().len(),
                tuner.to_history().disable_count as usize,
            ));
        }

        // SearchQualityEvaluator 状态
        if let Some(ref evaluator) = self.search_quality_evaluator {
            has_any = true;
            let entries = self
                .dev_trace
                .as_ref()
                .and_then(|w| w.read_all().ok())
                .unwrap_or_default();
            let stats = build_search_quality_stats(&entries);
            let with_rate = stats.with_search_fix_rate();
            let without_rate = stats.without_search_fix_rate();
            let diff = stats.search_vs_no_search_diff();
            let is_beneficial = stats.is_search_beneficial();
            let total_checks = stats.checks_with_search + stats.checks_without_search;
            states.push(EvaluatorState::new(
                EvaluatorType::SearchQuality,
                evaluator.is_enabled(),
                with_rate,
                without_rate,
                diff,
                is_beneficial,
                total_checks,
                evaluator.evaluation_count() as usize,
                evaluator.to_history().disable_count as usize,
            ));
        }

        // MemoryContextEvaluator 状态
        if let Some(ref evaluator) = self.memory_evaluator {
            has_any = true;
            let entries = self
                .dev_trace
                .as_ref()
                .and_then(|w| w.read_all().ok())
                .unwrap_or_default();
            let stats = build_memory_evaluation_stats(&entries);
            let with_rate = stats.with_memory_fix_rate();
            let without_rate = stats.without_memory_fix_rate();
            let diff = stats.memory_vs_no_memory_diff();
            let is_beneficial = stats.is_memory_beneficial();
            let total_checks = stats.total_checks();
            states.push(EvaluatorState::new(
                EvaluatorType::MemoryContext,
                evaluator.is_enabled(),
                with_rate,
                without_rate,
                diff,
                is_beneficial,
                total_checks,
                evaluator.evaluation_count() as usize,
                evaluator.to_history().disable_count as usize,
            ));
        }

        if !has_any {
            return None;
        }

        // 读取 DevTrace 条目用于时间线构建
        let entries = self
            .dev_trace
            .as_ref()
            .and_then(|w| w.read_all().ok())
            .unwrap_or_default();

        // 计算总编译检查次数和成功次数
        let total_compile_checks = entries
            .iter()
            .filter(|e| e.action == TraceAction::CompileCheck)
            .count();
        let total_compile_successes = entries
            .iter()
            .filter(|e| e.action == TraceAction::CompileCheck && e.success)
            .count();

        Some(build_evaluator_synergy_summary(
            &states,
            total_compile_checks,
            total_compile_successes,
            &entries,
        ))
    }

    /// memory.json 路径
    fn memory_path(&self) -> std::path::PathBuf {
        self.workspace.root.join(".forge").join("memory.json")
    }

    /// 保存 Memory 到磁盘 (断点续传)
    ///
    /// 在关键节点调用: planning 后、每 task 后、每 phase 后
    fn save_memory(&self) {
        let path = self.memory_path();
        if let Err(e) = self.memory.save(&path) {
            warn!("保存 Memory 失败: {}", e);
        }
    }

    /// 写入 DevTrace 条目 (如果启用) (借鉴方向 4)
    ///
    /// 在关键操作点调用, 记录操作类型、阶段/任务索引、输入/输出摘要、
    /// 耗时和结果。如果 `dev_trace` 为 None 则不执行任何操作。
    #[allow(clippy::too_many_arguments)]
    fn trace_dev(
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
        if let Some(ref trace_writer) = self.dev_trace {
            if let Err(e) = trace_writer.trace(
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

    /// 带连接监控的消息发送 — 24h 可靠性强化
    ///
    /// 包装 `chat.send_message()`, 在发送前检查 Chrome 连接状态,
    /// 如果检测到断连则触发自动恢复, 恢复后重试发送。
    ///
    /// 如果 `connection_monitor` 或 `auto_recovery` 为 None (禁用),
    /// 直接调用 `chat.send_message()` (向后兼容)。
    ///
    /// 返回 `ChatResult` 或错误 (恢复失败时)。
    async fn send_message_safe(
        &mut self,
        msg: &str,
        timeout: u64,
    ) -> Result<crate::traits::ChatResult> {
        // 如果未启用自动恢复, 直接发送
        if self.connection_monitor.is_none() || self.auto_recovery.is_none() {
            return self.chat.send_message(msg, timeout).await;
        }

        // 1. 发送前检查连接状态
        let needs_recovery = if let Some(ref mut monitor) = self.connection_monitor {
            let status = monitor.check_connection().await;
            if status.needs_recovery() {
                warn!("⚠️ 连接异常: {}, 触发自动恢复", status.description());

                // === DevTrace: 自动恢复 (24h 可靠性) ===
                self.trace_dev(
                    TraceAction::Recovery,
                    None,
                    None,
                    None,
                    "连接检查",
                    status.description(),
                    0,
                    false,
                    Some(&format!("检测到连接异常: {}", status)),
                );
                true
            } else {
                false
            }
        } else {
            false
        };

        // 2. 如果需要恢复, 执行自动恢复
        if needs_recovery {
            let recovery_start = Instant::now();

            // 分离 monitor 和 recovery 的可变引用
            let recovery_result = {
                let monitor = self.connection_monitor.as_mut().unwrap();
                let recovery = self.auto_recovery.as_mut().unwrap();
                recovery.recover(monitor).await
            };

            let recovery_duration = recovery_start.elapsed().as_millis() as u64;

            // 记数恢复结果
            match &recovery_result {
                crate::auto_recovery::RecoveryResult::Success { attempts, .. } => {
                    info!(
                        "✅ 自动恢复成功 ({} 次重试, {}ms)",
                        attempts, recovery_duration
                    );
                    self.trace_dev(
                        TraceAction::Recovery,
                        None,
                        None,
                        None,
                        "自动恢复",
                        &format!("恢复成功 ({} 次)", attempts),
                        recovery_duration,
                        true,
                        None,
                    );
                }
                crate::auto_recovery::RecoveryResult::Failed { error, .. } => {
                    error!("❌ 自动恢复失败: {}", error);
                    self.trace_dev(
                        TraceAction::Recovery,
                        None,
                        None,
                        None,
                        "自动恢复",
                        "恢复失败",
                        recovery_duration,
                        false,
                        Some(error),
                    );
                    return Err(anyhow::anyhow!("Chrome 自动恢复失败: {}", error));
                }
            }
        }

        // 3. 连接正常 (或已恢复), 发送消息
        // 如果 send_message 失败, 尝试一次恢复后重试
        match self.chat.send_message(msg, timeout).await {
            Ok(result) => Ok(result),
            Err(e) => {
                warn!("send_message 失败: {}, 尝试恢复后重试...", e);

                let recovery_start = Instant::now();
                let recovery_result = {
                    let monitor = self.connection_monitor.as_mut().unwrap();
                    let recovery = self.auto_recovery.as_mut().unwrap();
                    recovery.recover(monitor).await
                };
                let recovery_duration = recovery_start.elapsed().as_millis() as u64;

                match &recovery_result {
                    crate::auto_recovery::RecoveryResult::Success { attempts, .. } => {
                        info!("✅ 恢复成功, 重试 send_message");
                        self.trace_dev(
                            TraceAction::Recovery,
                            None,
                            None,
                            None,
                            "send_message 失败后恢复",
                            &format!("恢复成功 ({} 次)", attempts),
                            recovery_duration,
                            true,
                            None,
                        );
                        // 重试发送
                        self.chat.send_message(msg, timeout).await
                    }
                    crate::auto_recovery::RecoveryResult::Failed { error, .. } => {
                        self.trace_dev(
                            TraceAction::Recovery,
                            None,
                            None,
                            None,
                            "send_message 失败后恢复",
                            "恢复失败",
                            recovery_duration,
                            false,
                            Some(error),
                        );
                        Err(anyhow::anyhow!(
                            "send_message 失败且自动恢复也失败: {} | {}",
                            e,
                            error
                        ))
                    }
                }
            }
        }
    }

    /// 上下文衔接检查 — 对话过长时新开对话并交接上下文 (借鉴方向 1)
    ///
    /// 在每次 send_message 后调用:
    /// 1. 检查对话轮数是否超过 max_context_turns
    /// 2. 超过则构建 ContextHandoff (从 memory + workspace + error_history)
    /// 3. 调用 chat.start_new_conversation() 新开对话
    /// 4. 发送交接 prompt 作为新对话的第一条消息
    ///
    /// 上下文处理检查 — 检查是否需要上下文衔接或压缩
    ///
    /// 检查两个触发条件:
    /// 1. 基于对话轮数的上下文衔接 (原有功能)
    /// 2. 基于 token 数量的上下文压缩 (ds4 风格)
    ///
    /// 优先级: token 压缩 > 对话轮数衔接
    async fn maybe_context_handoff(&mut self) -> Result<()> {
        // 检查基于 token 的上下文压缩 (更高优先级)
        if let Some(ref config) = self.compaction_config {
            // 克隆 config 以避免借用冲突
            let config_clone = config.clone();
            if let Some(trigger) = self.check_compaction_trigger(&config_clone)? {
                self.execute_context_compaction(trigger, &config_clone)
                    .await?;
                return Ok(());
            }
        }

        // 检查基于对话轮数的上下文衔接 (原有功能)
        if self.max_context_turns == 0 {
            return Ok(());
        }

        let turn_count = self.chat.conversation_turn_count();
        if turn_count < self.max_context_turns {
            return Ok(());
        }

        info!(
            "🔄 对话轮数 {} 超过阈值 {}, 执行上下文衔接",
            turn_count, self.max_context_turns
        );
        println!(
            "\n  🔄 上下文衔接: 对话轮数 {} 超过阈值 {}, 新开对话...",
            turn_count, self.max_context_turns
        );

        // 1. 构建交接上下文 (先完成不可变借用, 再释放)
        let handoff_prompt = {
            let handoff = ContextHandoff::build_from_memory(
                &self.memory,
                &self.workspace,
                &self.error_history,
            );
            handoff.to_prompt()
        };

        // 2. 记录交接决策
        let current_phase = self.memory.current_phase;
        let current_task = self.memory.current_task.clone();
        self.memory.add_decision(
            current_phase,
            current_task.as_deref(),
            "上下文衔接",
            &format!(
                "对话轮数 {} 超过阈值 {}, 新开对话",
                turn_count, self.max_context_turns
            ),
        );

        // 3. 新开对话 (CDP 导航到 chat.z.ai/)
        self.chat.start_new_conversation().await?;

        // 3.5 重置增量跟踪状态 — 新开对话后, 之前发送的消息不再有效
        if self.live_continuation.is_some() || self.conversation_tracker.is_some() {
            info!("🔄 重置增量跟踪状态 (上下文衔接)");
            self.reset_continuation();
        }

        // 4. 发送交接 prompt 作为新对话的第一条消息
        info!("发送交接 prompt ({}字符)...", handoff_prompt.len());
        let handoff_start = Instant::now();
        let result = self
            .send_message_safe(&handoff_prompt, self.timeout_secs)
            .await?;
        let handoff_duration = handoff_start.elapsed().as_millis() as u64;

        // === DevTrace: 上下文衔接 (借鉴方向 4) ===
        self.trace_dev(
            TraceAction::ContextHandoff,
            Some(current_phase),
            None,
            None,
            &handoff_prompt,
            &result.text,
            handoff_duration,
            true,
            None,
        );

        // 5. 记录交接对话
        self.memory.add_conversation(
            "user",
            &format!("[上下文衔接 prompt - {}字符]", handoff_prompt.len()),
            current_task.as_deref(),
        );
        self.memory
            .add_conversation("assistant", &result.text, current_task.as_deref());
        self.save_memory();

        println!("  ✅ 上下文衔接完成, 对话轮数已重置");
        Ok(())
    }

    /// 转向提醒检查 — 在发送消息前注入提醒 (借鉴方向 2)
    ///
    /// 在每次 send_message 之前调用:
    /// 1. 检查 steer_interval 是否启用 (> 0)
    /// 2. 获取当前对话轮数
    /// 3. 如果轮数是 interval 的倍数 (> 0), 构建提醒并前置到消息中
    /// 4. 返回可能包含提醒的消息
    ///
    /// steer_interval == 0 时禁用 (返回原始消息)。
    /// 与 maybe_context_handoff 互补: 先做转向提醒, 如果继续变长则触发上下文衔接。
    fn maybe_steer_reminder(&self, prompt: &str) -> String {
        if self.steer_interval == 0 {
            return prompt.to_string();
        }

        let turn_count = self.chat.conversation_turn_count();
        let mut reminder = SteerReminder::build_from_memory(&self.memory);
        reminder.interval = self.steer_interval;

        let result = reminder.inject(turn_count, prompt);
        if result != prompt {
            info!(
                "🧭 转向提醒: 对话轮数 {} 是间隔 {} 的倍数, 注入提醒",
                turn_count, self.steer_interval
            );
            println!(
                "  🧭 转向提醒: 第 {} 轮对话, 重新锚定 AI 注意力",
                turn_count
            );
            // === DevTrace: 转向提醒 (借鉴方向 4) ===
            self.trace_dev(
                TraceAction::SteerReminder,
                Some(self.memory.current_phase),
                None,
                None,
                prompt,
                &result,
                0,
                true,
                None,
            );
        }
        result
    }

    /// AI 自主指令处理 — 检测并执行 AI 回复中的 slash commands (借鉴方向 5)
    ///
    /// 在每次 AI 回复后 (check_and_clarify 之后) 调用:
    /// 1. 从回复中解析所有 slash commands
    /// 2. 逐个执行对应操作:
    ///    - `/compact` → 强制触发上下文衔接 (即使未达阈值)
    ///    - `/skip` → 返回 SkipTask (调用方负责标记任务失败)
    ///    - `/refocus` → 立即注入转向提醒到下一次发送
    ///    - `/retry` → 重置循环终止检测器 (允许全新方法)
    ///    - `/escalate` → 触发人工干预确认
    /// 3. 记录决策到 memory + DevTrace
    ///
    /// `slash_commands_enabled == false` 时直接返回 Continue。
    async fn process_slash_commands(
        &mut self,
        phase_idx: usize,
        task_idx: usize,
        response: &str,
    ) -> Result<SlashCommandAction> {
        if !self.slash_commands_enabled {
            return Ok(SlashCommandAction::Continue);
        }

        let commands = slash_command::parse_from_response(response);
        if commands.is_empty() {
            return Ok(SlashCommandAction::Continue);
        }

        let task_id = format!("{}-{}", phase_idx, task_idx);
        let mut action = SlashCommandAction::Continue;

        for cmd in &commands {
            info!("    ⚡ Slash Command: {} ({})", cmd, cmd.description());
            println!("    ⚡ AI 自主指令: {} ({})", cmd, cmd.description());

            // === DevTrace: Slash Command (借鉴方向 4) ===
            let sc_task_name = self.memory.phases[phase_idx].tasks[task_idx].name.clone();
            self.trace_dev(
                TraceAction::SlashCommand,
                Some(phase_idx),
                Some(task_idx),
                Some(&sc_task_name),
                response,
                &cmd.to_string(),
                0,
                true,
                None,
            );

            match cmd {
                SlashCommand::Skip => {
                    // /skip — 跳过当前任务
                    self.memory.add_decision(
                        phase_idx,
                        Some(&task_id),
                        "AI 自主指令: /skip",
                        "AI 主动建议跳过当前任务",
                    );
                    action = SlashCommandAction::SkipTask;
                }
                SlashCommand::Search(query) => {
                    // /search — 触发网页搜索
                    self.memory.add_decision(
                        phase_idx,
                        Some(&task_id),
                        "AI 自主指令: /search",
                        &format!("AI 请求搜索: {}", query),
                    );

                    // 执行网页搜索
                    if let Some(ref web_tool) = self.web_tool {
                        info!("    🔍 /search: 执行网页搜索 '{}'", query);
                        println!("    🔍 AI 请求搜索: {}", query);

                        // 创建取消令牌，确保搜索操作在全局超时内完成
                        let cancel_source = self.create_cancellation_token();
                        let cancel_token = cancel_source.token();

                        match web_tool.search_web(query, None, Some(&cancel_token)).await {
                            Ok(search_result) => {
                                // 将搜索结果作为新的 AI 消息添加到对话中
                                let search_summary = format!(
                                    "# 网页搜索结果\\n\\n查询: {}\\n耗时: {}ms\\n\\n{}",
                                    search_result.query,
                                    search_result.duration_ms,
                                    search_result.content
                                );

                                // 添加搜索结果到对话历史
                                self.memory.add_conversation(
                                    "assistant",
                                    &search_summary,
                                    Some(&task_id),
                                );

                                info!(
                                    "    ✅ /search: 搜索完成 ({} 字符)",
                                    search_result.content.len()
                                );
                                println!(
                                    "    ✅ 搜索结果已添加到对话中 ({} 字符)",
                                    search_result.content.len()
                                );

                                // === DevTrace: 网页搜索 (Session 73) ===
                                let sc_task_name =
                                    self.memory.phases[phase_idx].tasks[task_idx].name.clone();
                                self.trace_dev(
                                    TraceAction::WebSearch,
                                    Some(phase_idx),
                                    Some(task_idx),
                                    Some(&sc_task_name),
                                    query,
                                    &search_result.content,
                                    search_result.duration_ms,
                                    true,
                                    Some("AI 通过 /search 指令触发"),
                                );
                            }
                            Err(e) => {
                                error!("    ❌ /search: 搜索失败: {}", e);
                                println!("    ❌ 网页搜索失败: {}", e);

                                // 添加错误信息到对话
                                let error_msg = format!("搜索失败: {}", e);
                                self.memory.add_conversation(
                                    "assistant",
                                    &error_msg,
                                    Some(&task_id),
                                );
                            }
                        }
                    } else {
                        warn!("    ⚠️ /search: Web 工具未启用");
                        println!("    ⚠️ Web 工具未启用，无法执行搜索");

                        let no_tool_msg = "Web 搜索工具未启用，无法执行搜索请求。";
                        self.memory
                            .add_conversation("assistant", no_tool_msg, Some(&task_id));
                    }
                }
                SlashCommand::Compact => {
                    // /compact — 强制触发上下文衔接
                    self.memory.add_decision(
                        phase_idx,
                        Some(&task_id),
                        "AI 自主指令: /compact",
                        "AI 建议压缩上下文, 触发上下文衔接",
                    );
                    // 强制执行上下文衔接 (即使未达阈值)
                    self.force_context_handoff().await?;
                }
                SlashCommand::Refocus => {
                    // /refocus — 注入转向提醒 (通过设置 turn_count 为间隔的倍数)
                    // 由于 maybe_steer_reminder 在发送前检查, 这里记录决策
                    // 实际效果: 下一次发送时会注入额外的转向提醒
                    self.memory.add_decision(
                        phase_idx,
                        Some(&task_id),
                        "AI 自主指令: /refocus",
                        "AI 建议重新聚焦, 注入转向提醒",
                    );
                    // 立即构建并发送一条转向提醒消息
                    let reminder = SteerReminder::build_from_memory(&self.memory);
                    let reminder_prompt = reminder.to_prompt();
                    info!("    🧭 /refocus: 注入转向提醒");
                    let refocus_start = Instant::now();
                    let refocus_result = self
                        .send_message_safe(&reminder_prompt, self.timeout_secs)
                        .await?;
                    let refocus_duration = refocus_start.elapsed().as_millis() as u64;

                    // === DevTrace: 转向提醒 (借鉴方向 4) ===
                    self.trace_dev(
                        TraceAction::SteerReminder,
                        Some(phase_idx),
                        Some(task_idx),
                        Some(&sc_task_name),
                        &reminder_prompt,
                        &refocus_result.text,
                        refocus_duration,
                        true,
                        None,
                    );
                }
                SlashCommand::Retry => {
                    // /retry — 重置循环终止检测器
                    self.memory.add_decision(
                        phase_idx,
                        Some(&task_id),
                        "AI 自主指令: /retry",
                        "AI 建议用不同方法重试, 重置循环终止检测器",
                    );
                    if let Some(ref mut detector) = self.loop_detector {
                        detector.reset();
                        info!("    🔄 /retry: 循环终止检测器已重置");
                    }
                }
                SlashCommand::Escalate => {
                    // /escalate — 触发人工干预
                    self.memory.add_decision(
                        phase_idx,
                        Some(&task_id),
                        "AI 自主指令: /escalate",
                        "AI 请求人工干预",
                    );
                    let fix_context = FixContext {
                        phase_idx,
                        task_idx,
                        attempt: self.memory.phases[phase_idx].tasks[task_idx].attempts,
                        max_attempts: self.max_rounds_per_task,
                        feedback: "AI 请求人工干预 (/escalate)".to_string(),
                    };
                    let should_continue = self.interaction.confirm_fix(&fix_context).await?;
                    if !should_continue {
                        action = SlashCommandAction::SkipTask;
                    }
                }
                SlashCommand::Unknown(_) => {
                    // 未知指令 — 不执行任何操作
                    debug!("    未知 slash command: {}", cmd);
                }
            }
        }

        self.save_memory();
        Ok(action)
    }

    /// 强制上下文衔接 — 不检查阈值, 直接新开对话 (借鉴方向 5: /compact 触发)
    ///
    /// 与 `maybe_context_handoff` 类似, 但不检查 turn_count 是否达到阈值。
    async fn force_context_handoff(&mut self) -> Result<()> {
        info!("🔄 /compact 触发强制上下文衔接");
        println!("  🔄 /compact: 强制上下文衔接, 新开对话...");

        let current_phase = self.memory.current_phase;
        let current_task = self.memory.current_task.clone();

        // 1. 构建交接上下文
        let handoff_prompt = {
            let handoff = ContextHandoff::build_from_memory(
                &self.memory,
                &self.workspace,
                &self.error_history,
            );
            handoff.to_prompt()
        };

        // 2. 记录交接决策
        self.memory.add_decision(
            current_phase,
            current_task.as_deref(),
            "强制上下文衔接 (/compact)",
            "AI 通过 /compact 指令触发上下文衔接",
        );

        // 3. 新开对话
        self.chat.start_new_conversation().await?;

        // 3.5 重置增量跟踪状态 — 新开对话后, 之前发送的消息不再有效
        if self.live_continuation.is_some() || self.conversation_tracker.is_some() {
            info!("🔄 重置增量跟踪状态 (/compact 强制上下文衔接)");
            self.reset_continuation();
        }

        // 4. 发送交接 prompt
        let handoff_start = Instant::now();
        let result = self
            .send_message_safe(&handoff_prompt, self.timeout_secs)
            .await?;
        let handoff_duration = handoff_start.elapsed().as_millis() as u64;

        // === DevTrace: 上下文衔接 (借鉴方向 4) ===
        self.trace_dev(
            TraceAction::ContextHandoff,
            Some(current_phase),
            None,
            None,
            &handoff_prompt,
            &result.text,
            handoff_duration,
            true,
            None,
        );

        // 5. 记录交接对话
        self.memory.add_conversation(
            "user",
            &format!("[强制上下文衔接 prompt - {}字符]", handoff_prompt.len()),
            current_task.as_deref(),
        );
        self.memory
            .add_conversation("assistant", &result.text, current_task.as_deref());
        self.save_memory();

        Ok(())
    }

    /// 将 Memory 中的 phases 转换为 PlanInfo (给 HumanInteraction 使用)
    fn build_plan_info(&self) -> PlanInfo {
        let phases: Vec<PhaseInfo> = self
            .memory
            .phases
            .iter()
            .map(|p| PhaseInfo {
                name: p.name.clone(),
                description: p.description.clone(),
                tasks: p
                    .tasks
                    .iter()
                    .map(|t| TaskInfo {
                        id: t.id.clone(),
                        name: t.name.clone(),
                        prompt: t.prompt.clone(),
                    })
                    .collect(),
            })
            .collect();
        PlanInfo {
            goal: self.memory.goal.clone(),
            phases,
        }
    }

    /// 将单个 Task 转换为 TaskInfo
    fn build_task_info(&self, phase_idx: usize, task_idx: usize) -> TaskInfo {
        let task = &self.memory.phases[phase_idx].tasks[task_idx];
        TaskInfo {
            id: task.id.clone(),
            name: task.name.clone(),
            prompt: task.prompt.clone(),
        }
    }

    /// 自主追问 — 检查 AI 回复是否需要追问, 如需则发送追问消息
    ///
    /// **核心中的核心** — Agent 自主判断 AI 回复是否含疑问/不确定/超时,
    /// 并生成追问消息要求 AI 自行决策继续编码。
    ///
    /// 返回最终文本 (原始回复或追问后的回复)。
    /// 追问历史记录在 Task.clarifications 中, 防止无限循环。
    async fn check_and_clarify(
        &mut self,
        phase_idx: usize,
        task_idx: usize,
        response: &str,
        timed_out: bool,
    ) -> Result<String> {
        let task_id = format!("{}-{}", phase_idx, task_idx);

        // 构建澄清上下文 (clone 避免借用冲突)
        let context = {
            let task = &self.memory.phases[phase_idx].tasks[task_idx];
            ClarificationContext {
                task_prompt: task.prompt.clone(),
                timed_out,
                questions_asked: task.clarifications.len() as u32,
                max_questions: self.memory.max_clarifications,
                previous_questions: task.clarifications.clone(),
            }
        };

        // 检查是否需要追问
        let result = self.clarification_checker.check(response, &context).await;

        if !result.needs_clarification {
            return Ok(response.to_string());
        }

        // 记录追问
        info!("    💬 自主追问: {}", result.reason);
        println!("    💬 自主追问: {}", result.reason);
        self.memory.phases[phase_idx].tasks[task_idx]
            .clarifications
            .push(result.question.clone());
        self.memory.add_decision(
            phase_idx,
            Some(&task_id),
            "自主追问",
            &format!("原因: {}", result.reason),
        );
        self.memory
            .add_conversation("user", &result.question, Some(&task_id));

        // 发送追问并等待回复 (注入转向提醒)
        let steered_question = self.maybe_steer_reminder(&result.question);
        let clarify_start = Instant::now();
        let follow_up = self
            .send_message_safe(&steered_question, self.timeout_secs)
            .await?;
        let clarify_duration = clarify_start.elapsed().as_millis() as u64;

        // === DevTrace: 自主追问 (借鉴方向 4) ===
        let task_name = self.memory.phases[phase_idx].tasks[task_idx].name.clone();
        self.trace_dev(
            TraceAction::Clarification,
            Some(phase_idx),
            Some(task_idx),
            Some(&task_name),
            &result.question,
            &follow_up.text,
            clarify_duration,
            true,
            None,
        );

        self.memory
            .add_conversation("assistant", &follow_up.text, Some(&task_id));
        self.save_memory();

        // === 上下文衔接检查 (借鉴方向 1) ===
        self.maybe_context_handoff().await?;

        Ok(follow_up.text)
    }

    /// 自主追问 (planning 阶段) — 检查 AI 规划回复是否需要追问
    ///
    /// planning 阶段没有具体 task, 使用简化版上下文。
    async fn check_and_clarify_planning(
        &mut self,
        response: &str,
        timed_out: bool,
    ) -> Result<String> {
        let context = ClarificationContext {
            task_prompt: self.memory.goal.clone(),
            timed_out,
            questions_asked: 0,
            max_questions: self.memory.max_clarifications,
            previous_questions: vec![],
        };

        let result = self.clarification_checker.check(response, &context).await;

        if !result.needs_clarification {
            return Ok(response.to_string());
        }

        info!("  💬 自主追问 (planning): {}", result.reason);
        println!("  💬 自主追问 (planning): {}", result.reason);
        self.memory
            .add_decision(0, None, "planning 自主追问", &result.reason);
        self.memory.add_conversation("user", &result.question, None);

        let steered_question = self.maybe_steer_reminder(&result.question);
        let clarify_start = Instant::now();
        let follow_up = self
            .send_message_safe(&steered_question, self.timeout_secs)
            .await?;
        let clarify_duration = clarify_start.elapsed().as_millis() as u64;

        // === DevTrace: 自主追问 (planning) (借鉴方向 4) ===
        self.trace_dev(
            TraceAction::Clarification,
            None,
            None,
            None,
            &result.question,
            &follow_up.text,
            clarify_duration,
            true,
            None,
        );

        self.memory
            .add_conversation("assistant", &follow_up.text, None);
        self.save_memory();

        // === 上下文衔接检查 (借鉴方向 1) ===
        self.maybe_context_handoff().await?;

        Ok(follow_up.text)
    }

    /// 启动自主开发流程
    ///
    /// 若 resume=true 且 .forge/memory.json 存在,则从断点恢复:
    /// - 跳过已完成的阶段
    /// - 阶段内跳过已完成的任务
    /// - 从第一个未完成的任务继续执行
    pub async fn run(&mut self) -> Result<()> {
        self.workspace.init()?;
        info!("工作区: {}", self.workspace.root.display());

        // 初始化错误历史路径 (方向 F)
        let error_history_path = self
            .workspace
            .root
            .join(".forge")
            .join("error_history.json");
        self.error_history.history_path = Some(error_history_path.clone());

        // === 加载缓存调优历史 (Session 84) ===
        // 如果启用了 cache_tuner, 尝试从 .forge/cache_tuning_history.json 恢复
        if let Some(ref tuner) = self.cache_tuner {
            let config = tuner.config().clone();
            let default_ttl = tuner.current_ttl();
            if let Some(loaded_tuner) =
                CacheTuner::load_from_workspace(&self.workspace.root, config, default_ttl)
            {
                let new_ttl = loaded_tuner.current_ttl();
                let enabled = loaded_tuner.is_enabled();
                self.cache_tuner = Some(loaded_tuner);
                // 同步 search_cache 的 TTL 和状态
                self.search_cache.set_ttl(new_ttl);
                if !enabled {
                    // 历史显示缓存被禁用, 清除当前缓存
                    self.search_cache.clear();
                    println!("  📊 缓存调优: 从历史恢复 (缓存已禁用)");
                } else if new_ttl != default_ttl {
                    println!("  📊 缓存调优: 从历史恢复 TTL={}s", new_ttl);
                }
            }
        }

        // === 加载搜索质量历史 (Session 86) ===
        // 如果启用了 search_quality_evaluator, 尝试从 .forge/search_quality_history.json 恢复
        if let Some(ref evaluator) = self.search_quality_evaluator {
            let config = evaluator.config().clone();
            if let Some(loaded_evaluator) =
                SearchQualityEvaluator::load_from_workspace(&self.workspace.root, config)
            {
                let enabled = loaded_evaluator.is_enabled();
                let eval_count = loaded_evaluator.evaluation_count();
                self.search_quality_evaluator = Some(loaded_evaluator);
                if !enabled {
                    println!(
                        "  🔍 搜索质量: 从历史恢复 (搜索已禁用, 评估 {} 次)",
                        eval_count
                    );
                } else {
                    println!("  🔍 搜索质量: 从历史恢复 (评估 {} 次)", eval_count);
                }
            }
        }

        // === 加载 Memory 评估历史 (Session 90) ===
        // 如果启用了 memory_evaluator, 尝试从 .forge/memory_evaluation_history.json 恢复
        if let Some(ref evaluator) = self.memory_evaluator {
            let config = evaluator.config().clone();
            if let Some(loaded_evaluator) =
                MemoryContextEvaluator::load_from_workspace(&self.workspace.root, config)
            {
                let enabled = loaded_evaluator.is_enabled();
                let eval_count = loaded_evaluator.evaluation_count();
                self.memory_evaluator = Some(loaded_evaluator);
                if !enabled {
                    println!(
                        "  📝 Memory 评估: 从历史恢复 (注入已禁用, 评估 {} 次)",
                        eval_count
                    );
                    // 历史显示注入已禁用, 禁用 memory_context_count
                    self.memory_context_count = 0;
                } else {
                    println!("  📝 Memory 评估: 从历史恢复 (评估 {} 次)", eval_count);
                }
            }
        }

        // === 加载联合决策历史 (Session 99) ===
        // 如果启用了 joint_decision_engine, 尝试从 .forge/joint_decision_history.json 恢复
        if let Some(ref mut engine) = self.joint_decision_engine {
            engine.load_history_from_workspace(&self.workspace.root);
            let h = &engine.history;
            if !h.is_empty() {
                println!(
                    "  🔗 联合决策: 从历史恢复 ({} session, {} 决策)",
                    h.session_count(),
                    h.total_decisions(),
                );
            }
        }

        let memory_path = self.memory_path();

        // ========== 断点恢复检查 ==========
        if self.resume && memory_path.exists() {
            println!("\n{}", "═".repeat(60));
            println!("  📥 从断点恢复...");
            println!("{}", "═".repeat(60));

            self.memory = Memory::load(&memory_path)?;
            println!("  终极目标: {}", self.memory.goal);
            println!("  阶段: {} 个", self.memory.phases.len());
            println!("  对话: {} 轮", self.memory.conversations.len());
            println!("  决策: {} 条", self.memory.decisions.len());
            println!(
                "  进度: {}/{} 任务完成",
                self.memory.completed_task_count(),
                self.memory.total_task_count()
            );

            // 与版本管理协同: 检查 known good 快照状态
            if let Some(kg_id) = self.workspace.get_known_good_id() {
                println!("  known good 快照: #{}", kg_id);
            }

            // 更新工作区文件列表 (可能外部有变更)
            self.memory.workspace_files = self
                .workspace
                .list_files()
                .unwrap_or_default()
                .into_iter()
                .filter(|f| !f.starts_with("target/"))
                .collect();

            // 所有阶段已完成时直接出报告
            if self.memory.all_phases_completed() {
                println!("\n  ✅ 所有阶段已完成,输出最终报告");
                self.final_report()?;
                return Ok(());
            }
        } else {
            // ========== 全新开始 ==========
            if memory_path.exists() {
                info!("发现旧 memory.json,覆盖重新开始");
            }

            // === DevTrace: 全新开始时清空 trace 文件 (借鉴方向 4) ===
            if let Some(ref trace) = self.dev_trace {
                let _ = trace.clear();
            }

            println!("\n{}", "═".repeat(60));
            println!("  🎯 终极目标: {}", self.memory.goal);
            println!("{}", "═".repeat(60));

            // 步骤 1: 让 AI 拆解终极目标为开发阶段
            println!("\n▶ 步骤 1: AI 分析目标,拆解开发阶段...");
            self.planning_phase().await?;
            self.save_memory(); // 保存时机: planning 完成后

            // === 人工干预: 确认计划 (方向 A) ===
            let plan_info = self.build_plan_info();
            let approved = self.interaction.confirm_planning(&plan_info).await?;
            if !approved {
                println!("\n  🛑 计划被拒绝, 终止开发");
                self.memory
                    .add_decision(0, None, "计划被人类拒绝", "开发终止");
                self.save_memory();
                return Ok(());
            }
            self.memory
                .add_decision(0, None, "计划已确认", "人类已批准开发计划");
        }

        // ========== 步骤 2: 逐个阶段执行 ==========
        // 注意: 使用 while 循环而非 for 循环, 因为需求变更可能追加新阶段
        let mut phase_idx = 0;
        while phase_idx < self.memory.phases.len() {
            let total_phases = self.memory.phases.len();

            // === 需求变更检查 (方向 B) ===
            // 在每个阶段开始前, 检查是否有待处理的需求变更
            if self.memory.has_pending_changes() {
                self.handle_requirement_changes().await?;
            }

            // 断点恢复: 跳过已完成的阶段
            if self.memory.phases[phase_idx].status == PhaseStatus::Completed {
                println!(
                    "\n  ⏭ 跳过已完成阶段 {}/{}: {}",
                    phase_idx + 1,
                    total_phases,
                    self.memory.phases[phase_idx].name
                );
                phase_idx += 1;
                continue;
            }

            self.memory.current_phase = phase_idx;

            let phase = &self.memory.phases[phase_idx].clone();
            println!("\n{}", "─".repeat(60));
            println!(
                "  📋 阶段 {}/{}: {}",
                phase_idx + 1,
                total_phases,
                phase.name
            );
            println!("  {}", phase.description);
            println!("{}", "─".repeat(60));

            self.execute_phase(phase_idx).await?;
            self.save_memory(); // 保存时机: 每个 phase 完成后

            // 阶段间汇总
            let completed = self.memory.phases[phase_idx]
                .tasks
                .iter()
                .filter(|t| t.status == TaskStatus::Completed)
                .count();
            let total = self.memory.phases[phase_idx].tasks.len();
            println!("\n  阶段完成: {}/{} 任务成功", completed, total);

            if completed == 0 && total > 0 {
                warn!("阶段 {} 所有任务失败,继续下一阶段", phase_idx + 1);
            }

            phase_idx += 1;
        }

        // ========== 步骤 3: 最终报告 ==========
        self.final_report()?;
        Ok(())
    }

    /// 输出最终报告并保存
    fn final_report(&mut self) -> Result<()> {
        println!("\n{}", "═".repeat(60));
        println!("{}", self.memory.execution_report());
        println!("{}", "═".repeat(60));

        // 保存报告到工作区
        let report_path = self.workspace.root.join("FORGE_REPORT.md");
        std::fs::write(&report_path, self.memory.execution_report())?;
        println!("报告已保存: {}", report_path.display());

        // === 保存缓存调优历史 (Session 84) ===
        if let Some(ref tuner) = self.cache_tuner {
            match tuner.save_to_workspace(&self.workspace.root) {
                Ok(()) => {
                    let h = tuner.to_history();
                    if !h.is_empty() {
                        println!("\n  📊 缓存调优历史已保存: {}", h.to_summary());
                    }
                }
                Err(e) => {
                    warn!("保存缓存调优历史失败: {}", e);
                }
            }
        }

        // === 保存搜索质量历史 (Session 86) ===
        if let Some(ref evaluator) = self.search_quality_evaluator {
            match evaluator.save_to_workspace(&self.workspace.root) {
                Ok(()) => {
                    let h = evaluator.to_history();
                    if !h.is_empty() {
                        println!("\n  🔍 搜索质量历史已保存: {}", h.to_summary());
                    }
                }
                Err(e) => {
                    warn!("保存搜索质量历史失败: {}", e);
                }
            }
        }

        // === 打印 Memory 上下文注入统计 (Session 89) ===
        if self.memory_context_stats.has_data() {
            println!(
                "\n  📝 Memory 上下文注入: {}",
                self.memory_context_stats.to_summary()
            );
        }

        // === 保存 Memory 评估历史 (Session 90) ===
        if let Some(ref evaluator) = self.memory_evaluator {
            match evaluator.save_to_workspace(&self.workspace.root) {
                Ok(()) => {
                    let h = evaluator.to_history();
                    if !h.is_empty() {
                        println!("\n  📝 Memory 评估历史已保存: {}", h.to_summary());
                    }
                }
                Err(e) => {
                    warn!("保存 Memory 评估历史失败: {}", e);
                }
            }
        }

        // === 保存联合决策历史 (Session 99) ===
        if let Some(ref mut engine) = self.joint_decision_engine {
            // 加载已有历史 (确保跨 session 累积, 即使 run() 未调用)
            engine.load_history_from_workspace(&self.workspace.root);
            let timestamp = chrono::Utc::now();
            engine.finalize_session(timestamp);
            let history = std::mem::take(&mut engine.history);
            engine.history = history.with_timestamp(timestamp);
            engine.save_history_to_workspace(&self.workspace.root)?;
            let h = &engine.history;
            if !h.is_empty() {
                println!("\n  🔗 联合决策历史已保存: {}", h.to_summary());
            }
        }

        // === DevTrace: 打印追踪摘要 (借鉴方向 4) ===
        if let Some(ref trace) = self.dev_trace {
            let mut summary = trace.summary();

            // 附加缓存调优历史摘要 (Session 87)
            if let Some(ref tuner) = self.cache_tuner {
                let h = tuner.to_history();
                let ct_summary = build_cache_tuning_history_summary(
                    h.initial_ttl,
                    h.current_ttl,
                    h.enabled,
                    h.adjustment_count,
                    h.disable_count,
                    h.decisions.len(),
                    h.saved_at.clone(),
                );
                summary = summary.with_cache_tuning_history(ct_summary);

                // 附加缓存调优 sparkline 数据 (Session 94)
                let ttl_values = extract_ttl_trajectory(&h.decisions);
                let diff_values = extract_correlation_diffs(&h.decisions);
                if !ttl_values.is_empty() || !diff_values.is_empty() {
                    summary = summary.with_cache_tuning_sparkline(ttl_values, diff_values);
                }
            }

            // 附加搜索质量历史摘要 (Session 87)
            if let Some(ref evaluator) = self.search_quality_evaluator {
                let h = evaluator.to_history();
                let sq_summary = build_search_quality_history_summary(
                    h.initial_enabled,
                    h.current_enabled,
                    h.evaluation_count,
                    h.disable_count,
                    h.saved_at.clone(),
                );
                summary = summary.with_search_quality_history(sq_summary);

                // 附加搜索质量 sparkline 数据 (Session 95)
                let entries = trace.read_all().unwrap_or_default();
                let search_diffs = extract_search_diff_history(&entries);
                if !search_diffs.is_empty() {
                    summary = summary.with_search_quality_sparkline(search_diffs);
                }
            }

            // 附加 Memory 评估历史摘要 (Session 90)
            if let Some(ref evaluator) = self.memory_evaluator {
                let h = evaluator.to_history();
                let me_summary = build_memory_evaluation_history_summary(
                    h.initial_enabled,
                    h.current_enabled,
                    h.evaluation_count,
                    h.disable_count,
                    h.saved_at.clone(),
                );
                summary = summary.with_memory_evaluation_history(me_summary);

                // 附加 Memory 评估 sparkline 数据 (Session 95)
                let entries = trace.read_all().unwrap_or_default();
                let memory_diffs = extract_memory_diff_history(&entries);
                if !memory_diffs.is_empty() {
                    summary = summary.with_memory_evaluation_sparkline(memory_diffs);
                }
            }

            // 附加三评估器协同分析摘要 (Session 91) + 历史持久化 (Session 92)
            let synergy = self.build_evaluator_synergy();
            if let Some(s) = synergy {
                // 加载历史, 追加当前 session, 保存历史
                use crate::evaluator_synergy::{
                    build_synergy_history_summary, EvaluatorSynergyHistory,
                };
                let mut history =
                    EvaluatorSynergyHistory::load_from_workspace(&self.workspace.root)
                        .unwrap_or_default();
                let now = chrono::Utc::now();
                history.add_from_summary(&s, now);
                history = history.with_timestamp(now);

                // 构建历史摘要并附加到 DevTraceSummary
                let history_summary = build_synergy_history_summary(&history);
                if !history_summary.is_empty() {
                    summary = summary.with_evaluator_synergy_history(history_summary);
                }

                // 附加 sparkline 数据 (Session 93)
                let scores = history.synergy_scores();
                let fix_rates = history.fix_rates();
                if scores.len() >= 2 {
                    summary = summary.with_synergy_sparkline(scores, fix_rates);
                }

                // 保存历史到工作区
                match history.save_to_workspace(&self.workspace.root) {
                    Ok(()) => {
                        if !history.is_empty() {
                            println!("\n  🔗 协同分析历史已保存: {}", history.to_summary());
                        }
                    }
                    Err(e) => {
                        warn!("保存协同分析历史失败: {}", e);
                    }
                }

                summary = summary.with_evaluator_synergy(s);
            }

            // 附加联合决策历史摘要 (Session 99)
            if let Some(ref engine) = self.joint_decision_engine {
                let jd_summary = build_joint_decision_history_summary(&engine.history);
                if !jd_summary.is_empty() {
                    summary = summary.with_joint_decision_history(jd_summary);
                }
            }

            // === 保存 DevTrace 智能分析 (Session 99 + 100 增强) ===
            // 加载自定义阈值配置 (如果存在)
            let config = crate::dev_trace_analyzer::AnalysisConfig::load_from_workspace(
                &self.workspace.root,
            );
            let analysis =
                crate::dev_trace_analyzer::analyze_dev_trace_summary_with_config(&summary, &config);

            // === 健康度评分历史持久化 (Session 100) ===
            use crate::dev_trace_analyzer::{
                build_health_score_history_summary, HealthScoreHistory,
            };
            let mut hs_history =
                HealthScoreHistory::load_from_workspace(&self.workspace.root).unwrap_or_default();
            let now = chrono::Utc::now();
            hs_history.add_from_analysis(&analysis, now);
            hs_history = hs_history.with_timestamp(now);

            // 构建历史摘要并附加到 DevTraceSummary (在 JSON/HTML 保存之前)
            let hs_history_summary = build_health_score_history_summary(&hs_history);
            if !hs_history_summary.is_empty() {
                summary = summary.with_health_score_history(hs_history_summary);
            }

            // 保存历史到工作区
            match hs_history.save_to_workspace(&self.workspace.root) {
                Ok(()) => {
                    if !hs_history.is_empty() {
                        println!("\n  📊 健康度评分历史已保存: {}", hs_history.to_summary());
                    }
                }
                Err(e) => {
                    warn!("保存健康度评分历史失败: {}", e);
                }
            }

            println!("\n{}", summary.to_report());

            // === 保存 DevTraceSummary JSON 导出 (Session 88) ===
            let json_path = self.workspace.root.join(".forge/devtrace_summary.json");
            let timestamp = build_export_timestamp();
            match summary.save_to_json_file_with_meta(&json_path, &timestamp) {
                Ok(()) => {
                    println!("📊 DevTrace JSON 已保存: {}", json_path.display());
                }
                Err(e) => {
                    warn!("保存 DevTrace JSON 失败: {}", e);
                }
            }

            // === 保存 DevTraceSummary HTML 报告 (Session 93) ===
            let html_path = self.workspace.root.join(".forge/devtrace_report.html");
            match summary.save_to_html_file(&html_path) {
                Ok(()) => {
                    println!("📊 DevTrace HTML 已保存: {}", html_path.display());
                }
                Err(e) => {
                    warn!("保存 DevTrace HTML 失败: {}", e);
                }
            }

            // 生成并保存 Markdown 分析报告
            let report = crate::dev_trace_analyzer::generate_analysis_report(&analysis);
            match crate::dev_trace_analyzer::save_analysis_to_workspace(
                &report,
                &self.workspace.root,
            ) {
                Ok(()) => {
                    let hs = &analysis.health_score;
                    println!(
                        "📊 DevTrace 分析报告已保存: {}/.forge/devtrace_analysis.md",
                        self.workspace.root.display()
                    );
                    println!(
                        "  {} 健康度评分: {:.1}/100 ({}) — {} 条建议 ({} 严重, {} 警告, {} 信息)",
                        hs.grade_icon(),
                        hs.score,
                        hs.grade(),
                        analysis.recommendations.len(),
                        analysis
                            .recommendations
                            .iter()
                            .filter(|r| r.is_critical())
                            .count(),
                        analysis
                            .recommendations
                            .iter()
                            .filter(|r| r.is_warning())
                            .count(),
                        analysis
                            .recommendations
                            .iter()
                            .filter(|r| !r.is_critical() && !r.is_warning())
                            .count(),
                    );
                }
                Err(e) => {
                    warn!("保存 DevTrace 分析报告失败: {}", e);
                }
            }

            // === 保存 DevTrace 智能分析 HTML 报告 (Session 100) ===
            let html_report = crate::dev_trace_analyzer::generate_analysis_html_report(&analysis);
            match crate::dev_trace_analyzer::save_analysis_html_to_workspace(
                &html_report,
                &self.workspace.root,
            ) {
                Ok(()) => {
                    println!(
                        "📊 DevTrace 分析 HTML 已保存: {}/.forge/devtrace_analysis.html",
                        self.workspace.root.display()
                    );
                }
                Err(e) => {
                    warn!("保存 DevTrace 分析 HTML 失败: {}", e);
                }
            }
        }

        Ok(())
    }

    /// 阶段 1: 让 AI 拆解目标为开发阶段
    ///
    /// 注入 SystemPrompt 系统级约束, 确保 AI 在拆解目标时:
    /// - 使用最新最前沿的技术栈
    /// - 遵循 Spec-Driven Development 流程 (Mission → Tech Stack → Roadmap → Feature Phase)
    /// - 遵循 SOLID 原则
    /// - 遵循 TDD 模式
    async fn planning_phase(&mut self) -> Result<()> {
        let system_prompt = SystemPrompt::build_for_planning();
        let prompt = format!(
            "{}\n\
             我要实现以下终极目标:\n\
             \"\"\"\n{}\n\"\"\"\n\n\
             请将这个目标拆解为 3-6 个开发阶段,每个阶段包含 1-4 个具体任务。\n\
             \n\
             ⚠️ 重要: 此阶段 ONLY 输出 JSON 格式的开发计划,不要输出任何代码文件。\n\
             不要使用 ```file:路径``` 格式,只输出 JSON。\n\
             \n\
             输出格式 (严格遵循,只输出 JSON,不要输出其他内容):\n\
             ```json\n\
             [\n\
               {{\n\
                 \"name\": \"阶段名称\",\n\
                 \"description\": \"阶段描述\",\n\
                 \"tasks\": [\n\
                   {{\n\
                     \"name\": \"任务名称\",\n\
                     \"prompt\": \"给AI的具体指令,包含需求、格式要求等\",\n\
                     \"depends_on\": [\"任务ID\"]\n\
                   }}\n\
                 ]\n\
               }}\n\
             ]\n\
             ```\n\
             \n\
             注意:\n\
             - 第一个阶段应该是项目初始化 (Cargo.toml + 基本结构)\n\
             - 后续阶段逐步实现功能,遵循 Spec → Impl → Validation 流程\n\
             - 每个任务都要产出可编译的代码\n\
             - 最后阶段包含测试和文档\n\
             - prompt 要足够详细,包含功能规范、接口定义、测试要求、技术要求\n\
             - depends_on 是可选字段,指定此任务依赖的其他任务ID (如 \"0-1\")\n\
             - 无依赖关系的任务可以并行执行,请合理安排依赖以最大化并行度\n\
             - 任务ID格式为 \"阶段索引-任务索引\" (如第一阶段第二个任务为 \"0-1\")\n\
             - 技术选型请使用最新最前沿的方案",
            system_prompt, self.memory.goal
        );

        self.memory.add_conversation("user", &prompt, None);
        let steered_prompt = self.maybe_steer_reminder(&prompt);
        let plan_start = Instant::now();
        let result = self
            .send_message_safe(&steered_prompt, self.timeout_secs)
            .await?;
        let plan_duration = plan_start.elapsed().as_millis() as u64;
        self.memory
            .add_conversation("assistant", &result.text, None);

        // === DevTrace: 阶段规划 (借鉴方向 4) ===
        self.trace_dev(
            TraceAction::Planning,
            None,
            None,
            None,
            &prompt,
            &result.text,
            plan_duration,
            !result.timed_out,
            if result.timed_out {
                Some("AI 回复超时")
            } else {
                None
            },
        );

        // === 上下文衔接检查 (借鉴方向 1) ===
        self.maybe_context_handoff().await?;

        // === 自主追问 (planning 阶段) ===
        let plan_text = self
            .check_and_clarify_planning(&result.text, result.timed_out)
            .await?;

        // 解析 JSON 阶段计划
        let phases = self.parse_plan(&plan_text)?;

        if phases.is_empty() {
            // fallback: 创建一个简单的单阶段计划
            warn!("AI 未返回有效的阶段计划,使用默认计划");
            let default_phases = vec![Phase {
                id: 0,
                name: "项目初始化".to_string(),
                description: "创建项目结构和基础代码".to_string(),
                status: PhaseStatus::Pending,
                tasks: vec![Task {
                    id: "0-0".to_string(),
                    phase_id: 0,
                    name: "初始化项目".to_string(),
                    prompt: format!(
                        "为以下目标创建初始 Rust 项目。输出 Cargo.toml 和 src/main.rs。\n目标: {}",
                        self.memory.goal
                    ),
                    status: TaskStatus::Pending,
                    result: None,
                    attempts: 0,
                    files_written: vec![],
                    test_result: None,
                    last_good_snapshot: None,
                    clarifications: vec![],
                    depends_on: vec![],
                }],
            }];
            self.memory.set_phases(default_phases);
        } else {
            self.memory.set_phases(phases);
        }

        // 打印计划
        println!("\n开发计划 ({} 个阶段):", self.memory.phases.len());
        for (i, phase) in self.memory.phases.iter().enumerate() {
            println!(
                "  阶段 {}: {} ({} 个任务)",
                i + 1,
                phase.name,
                phase.tasks.len()
            );
            for task in &phase.tasks {
                println!("    - {}", task.name);
            }
        }
        println!();

        self.memory
            .add_decision(0, None, "完成目标拆解", "AI 返回了开发计划");
        Ok(())
    }

    /// 解析 AI 返回的阶段计划 JSON
    ///
    /// 支持截断 JSON 恢复: 当 AI 回复超长导致 JSON 不完整时,
    /// 自动修复截断的 JSON 并提取已完成的部分。
    fn parse_plan(&self, text: &str) -> Result<Vec<Phase>> {
        // 剥离前导 "json" 文本 (markdown 代码块标记残留)
        // AI 回复可能以 "json" 开头 (来自 ```json 代码块的 language 标记被剥离)
        let text = text.trim_start();
        let text = text
            .strip_prefix("json")
            .map(|s| s.trim_start())
            .unwrap_or(text);

        // 找 JSON 代码块
        let json_str = if let Some(start) = text.find("```json") {
            let after = &text[start + 7..];
            if let Some(end) = after.find("```") {
                after[..end].to_string()
            } else {
                // JSON 代码块未闭合 — 可能 AI 回复被截断
                // 尝试从 ```json 之后到文本末尾提取
                warn!("JSON 代码块未闭合, 尝试截断恢复");
                after.to_string()
            }
        } else if text.trim_start().starts_with('[') {
            text.trim().to_string()
        } else {
            // 尝试找第一个 [ 到最后一个 ]
            if let Some(start) = text.find('[') {
                if let Some(end) = text.rfind(']') {
                    text[start..=end].to_string()
                } else {
                    // 没有找到闭合 ] — JSON 可能被截断
                    warn!("JSON 未闭合 (无 ]), 尝试截断恢复");
                    text[start..].to_string()
                }
            } else {
                // 检测 AI 是否返回了代码文件而非 JSON
                if text.contains("file:") || text.contains("```rust") {
                    warn!(
                        "AI 返回了代码文件而非 JSON 阶段计划, 使用默认计划 (提示: 规划阶段应只输出 JSON)"
                    );
                } else {
                    warn!("AI 未返回 JSON 格式的阶段计划, 使用默认计划");
                }
                return Ok(vec![]);
            }
        };

        // 尝试直接解析
        let parsed: Vec<serde_json::Value> = match serde_json::from_str(&json_str) {
            Ok(v) => v,
            Err(e) => {
                warn!("JSON 解析失败: {}, 尝试截断恢复...", e);
                // 尝试修复截断的 JSON
                let repaired = repair_truncated_json(&json_str);
                match serde_json::from_str::<Vec<serde_json::Value>>(&repaired) {
                    Ok(v) => {
                        info!("✅ 截断 JSON 恢复成功 ({} 个阶段)", v.len());
                        v
                    }
                    Err(e2) => {
                        warn!("JSON 截断恢复失败: {}", e2);
                        return Ok(vec![]);
                    }
                }
            }
        };

        let mut phases = Vec::new();
        for (phase_idx, p) in parsed.iter().enumerate() {
            let name = p
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("未命名阶段")
                .to_string();
            let description = p
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let mut tasks = Vec::new();
            if let Some(tasks_arr) = p.get("tasks").and_then(|v| v.as_array()) {
                for (task_idx, t) in tasks_arr.iter().enumerate() {
                    let task_name = t
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("未命名任务")
                        .to_string();
                    let task_prompt = t
                        .get("prompt")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&task_name)
                        .to_string();
                    // 解析依赖任务列表 (用于并行任务执行)
                    let depends_on: Vec<String> = t
                        .get("depends_on")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();

                    tasks.push(Task {
                        id: format!("{}-{}", phase_idx, task_idx),
                        phase_id: phase_idx,
                        name: task_name,
                        prompt: task_prompt,
                        status: TaskStatus::Pending,
                        result: None,
                        attempts: 0,
                        files_written: vec![],
                        test_result: None,
                        last_good_snapshot: None,
                        clarifications: vec![],
                        depends_on,
                    });
                }
            }

            phases.push(Phase {
                id: phase_idx,
                name,
                description,
                status: PhaseStatus::Pending,
                tasks,
            });
        }

        Ok(phases)
    }

    /// 执行一个阶段的所有任务
    async fn execute_phase(&mut self, phase_idx: usize) -> Result<()> {
        self.memory.phases[phase_idx].status = PhaseStatus::InProgress;

        if self.parallel {
            self.execute_phase_parallel(phase_idx).await
        } else {
            self.execute_phase_sequential(phase_idx).await
        }
    }

    /// 顺序执行阶段任务 (原有逻辑, 向后兼容)
    async fn execute_phase_sequential(&mut self, phase_idx: usize) -> Result<()> {
        let tasks = self.memory.phases[phase_idx].tasks.clone();
        let total_tasks = tasks.len();

        for (task_idx, task) in tasks.iter().enumerate() {
            // 断点恢复: 跳过已完成的任务
            if task.status == TaskStatus::Completed {
                println!(
                    "\n  ⏭ 跳过已完成任务 {}/{}: {}",
                    task_idx + 1,
                    total_tasks,
                    task.name
                );
                continue;
            }

            println!("\n  ▶ 任务 {}/{}: {}", task_idx + 1, total_tasks, task.name);

            // === 人工干预: 确认任务执行 (方向 A) ===
            let task_info = self.build_task_info(phase_idx, task_idx);
            let action = self.interaction.confirm_task(&task_info).await?;
            match action {
                TaskAction::Execute => {}
                TaskAction::Skip => {
                    println!("  ⏭ 任务被跳过: {}", task.name);
                    self.memory.add_decision(
                        phase_idx,
                        Some(&task.id),
                        "任务被人类跳过",
                        "人类选择跳过此任务",
                    );
                    self.save_memory();
                    continue;
                }
                TaskAction::Abort => {
                    println!("\n  🛑 开发被中止");
                    self.memory.add_decision(
                        phase_idx,
                        Some(&task.id),
                        "开发被人类中止",
                        "人类选择终止整个开发流程",
                    );
                    self.save_memory();
                    return Err(anyhow::anyhow!("开发被人类中止"));
                }
            }

            // 构建包含上下文的 prompt (SRP: 委托给 ContextBuilder)
            // 注入 SystemPrompt 系统级约束 (前沿技术 + SOLID + Spec-Driven + TDD)
            let system_prompt = SystemPrompt::build_for_task();
            let context = self.memory.build_context(3);
            let full_prompt = format!(
                "{system_prompt}\n{context}\n\n\
                 当前项目上下文:\n{}\n\n\
                 请执行以下任务:\n{}\n\n\
                 用 ```file:路径``` 格式输出所有文件。输出完整文件内容,不要省略。",
                ContextBuilder::get_current_code_summary(&self.workspace),
                task.prompt
            );

            let result = self.execute_task(phase_idx, task_idx, &full_prompt).await;
            self.save_memory(); // 保存时机: 每个 task 完成后

            match result {
                Ok(true) => {
                    println!("  ✅ 任务完成: {}", task.name);
                }
                Ok(false) => {
                    println!("  ⚠ 任务未完全通过,但已尽力");
                }
                Err(e) => {
                    error!("  ❌ 任务失败: {}", e);
                    println!("  ❌ 任务失败: {}", e);
                }
            }
        }

        self.memory.phases[phase_idx].status = PhaseStatus::Completed;
        Ok(())
    }

    /// 并行执行阶段任务 (方向 C)
    ///
    /// 使用 TaskGraph 分析任务依赖关系, 按并行分组执行:
    /// 1. 构建任务依赖图 (DAG)
    /// 2. 检测循环依赖 (有环时回退到顺序执行)
    /// 3. 获取并行分组 (每组内任务互不依赖)
    /// 4. 逐组执行 (组内任务按顺序执行, 因为共享一个 ChatClient)
    /// 5. 如果组内任务失败, 跳过其所有依赖者
    async fn execute_phase_parallel(&mut self, phase_idx: usize) -> Result<()> {
        let tasks = &self.memory.phases[phase_idx].tasks.clone();
        let total_tasks = tasks.len();

        // 构建 TaskGraph
        let graph = match TaskGraph::build_from_tasks(tasks) {
            Ok(g) => g,
            Err(e) => {
                warn!("TaskGraph 构建失败 ({}), 回退到顺序执行", e);
                return self.execute_phase_sequential(phase_idx).await;
            }
        };

        // 环检测
        if graph.has_cycle() {
            warn!("检测到任务依赖环, 回退到顺序执行");
            self.memory.add_decision(
                phase_idx,
                None,
                "依赖环检测",
                "任务间存在循环依赖, 回退到顺序执行",
            );
            return self.execute_phase_sequential(phase_idx).await;
        }

        // 获取并行分组
        let groups = match graph.parallel_groups() {
            Ok(g) => g,
            Err(e) => {
                warn!("并行分组失败 ({}), 回退到顺序执行", e);
                return self.execute_phase_sequential(phase_idx).await;
            }
        };

        // 打印并行执行计划
        let max_parallel = graph.max_parallelism().unwrap_or(1);
        println!(
            "\n  🔄 并行模式: {} 个任务, {} 个并行组, 最大并行度 {}",
            total_tasks,
            groups.len(),
            max_parallel
        );
        for (gi, group) in groups.iter().enumerate() {
            let task_names: Vec<String> = group
                .iter()
                .map(|&idx| self.memory.phases[phase_idx].tasks[idx].name.clone())
                .collect();
            println!("    组 {}: {}", gi + 1, task_names.join(", "));
        }
        println!();

        // 记录决策
        self.memory.add_decision(
            phase_idx,
            None,
            "并行任务执行",
            &format!(
                "{} 个任务分为 {} 组, 最大并行度 {}",
                total_tasks,
                groups.len(),
                max_parallel
            ),
        );

        // 跟踪已失败的任务索引 (用于跳过依赖者)
        let mut failed_tasks: HashSet<usize> = HashSet::new();

        // 逐组执行
        for (group_idx, group) in groups.iter().enumerate() {
            println!("\n  ── 并行组 {}/{} ──", group_idx + 1, groups.len());

            for &task_idx in group {
                // 克隆任务数据避免借用冲突
                let task = self.memory.phases[phase_idx].tasks[task_idx].clone();

                // 断点恢复: 跳过已完成的任务
                if task.status == TaskStatus::Completed {
                    println!(
                        "\n  ⏭ 跳过已完成任务 {}/{}: {}",
                        task_idx + 1,
                        total_tasks,
                        task.name
                    );
                    continue;
                }

                // 检查是否有已失败的依赖
                let has_failed_dep = graph
                    .dependencies_of(task_idx)
                    .iter()
                    .any(|&dep_idx| failed_tasks.contains(&dep_idx));

                if has_failed_dep {
                    println!(
                        "\n  ⏭ 跳过任务 {}/{}: {} (依赖的任务已失败)",
                        task_idx + 1,
                        total_tasks,
                        task.name
                    );
                    self.memory.phases[phase_idx].tasks[task_idx].status = TaskStatus::Failed;
                    self.memory.add_decision(
                        phase_idx,
                        Some(&task.id),
                        "任务跳过 (依赖失败)",
                        "依赖的任务已失败, 跳过此任务",
                    );
                    failed_tasks.insert(task_idx);
                    continue;
                }

                println!("\n  ▶ 任务 {}/{}: {}", task_idx + 1, total_tasks, task.name);

                // === 人工干预: 确认任务执行 (方向 A) ===
                let task_info = self.build_task_info(phase_idx, task_idx);
                let action = self.interaction.confirm_task(&task_info).await?;
                match action {
                    TaskAction::Execute => {}
                    TaskAction::Skip => {
                        println!("  ⏭ 任务被跳过: {}", task.name);
                        self.memory.add_decision(
                            phase_idx,
                            Some(&task.id),
                            "任务被人类跳过",
                            "人类选择跳过此任务",
                        );
                        self.save_memory();
                        continue;
                    }
                    TaskAction::Abort => {
                        println!("\n  🛑 开发被中止");
                        self.memory.add_decision(
                            phase_idx,
                            Some(&task.id),
                            "开发被人类中止",
                            "人类选择终止整个开发流程",
                        );
                        self.save_memory();
                        return Err(anyhow::anyhow!("开发被人类中止"));
                    }
                }

                // 构建包含上下文的 prompt
                // 注入 SystemPrompt 系统级约束 (前沿技术 + SOLID + Spec-Driven + TDD)
                let system_prompt = SystemPrompt::build_for_task();
                let context = self.memory.build_context(3);
                let full_prompt = format!(
                    "{system_prompt}\n{context}\n\n\
                     当前项目上下文:\n{}\n\n\
                     请执行以下任务:\n{}\n\n\
                     用 ```file:路径``` 格式输出所有文件。输出完整文件内容,不要省略。",
                    ContextBuilder::get_current_code_summary(&self.workspace),
                    task.prompt
                );

                let result = self.execute_task(phase_idx, task_idx, &full_prompt).await;
                self.save_memory();

                match result {
                    Ok(true) => {
                        println!("  ✅ 任务完成: {}", task.name);
                    }
                    Ok(false) => {
                        println!("  ⚠ 任务未完全通过,但已尽力");
                        failed_tasks.insert(task_idx);
                    }
                    Err(e) => {
                        error!("  ❌ 任务失败: {}", e);
                        println!("  ❌ 任务失败: {}", e);
                        failed_tasks.insert(task_idx);
                    }
                }
            }
        }

        // 汇总
        let completed = self.memory.phases[phase_idx]
            .tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Completed)
            .count();
        let failed = self.memory.phases[phase_idx]
            .tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Failed)
            .count();
        if failed > 0 {
            println!(
                "\n  ⚠ 并行执行完成: {}/{} 成功, {} 失败/跳过",
                completed, total_tasks, failed
            );
        }

        self.memory.phases[phase_idx].status = PhaseStatus::Completed;
        Ok(())
    }
    async fn execute_task(
        &mut self,
        phase_idx: usize,
        task_idx: usize,
        prompt: &str,
    ) -> Result<bool> {
        let task_id = format!("{}-{}", phase_idx, task_idx);
        self.memory.current_task = Some(task_id.clone());
        self.memory.phases[phase_idx].tasks[task_idx].status = TaskStatus::InProgress;

        // 重置循环终止检测器 (避免跨任务误检) (借鉴方向 3)
        if let Some(ref mut detector) = self.loop_detector {
            detector.reset();
        }

        // 跟踪上一次失败的错误信息 (用于增量修复)
        let mut last_errors: Vec<CompileError> = Vec::new();
        let mut last_feedback: String = String::new();
        // 智能错误诊断结果 (方向 F) — 用于增强修复 prompt
        let mut last_diagnosis: Option<DiagnosisResult> = None;
        // === web_tool 深度集成: 编译错误自动搜索结果 ===
        // 修复轮次中, 将搜索结果追加到修复 prompt, 为 AI 提供额外上下文
        let mut last_search_results: Option<String> = None;

        // === 网络错误处理常量 (orchestrator 层面) ===
        // run_cargo 已重试 3 次 (5s 间隔), orchestrator 层面再重试 3 次 (30s 间隔)
        // 如果仍然失败, 跳过 AI 修复 (不消耗修复轮次), 最多跳过 5 次
        const MAX_ORCH_NETWORK_RETRIES: u32 = 3;
        const ORCH_NETWORK_RETRY_INTERVAL: u64 = 30;
        const MAX_NETWORK_ERROR_SKIPS: u32 = 5;
        let mut network_error_skips: u32 = 0;

        // === 增量发送: 存储首次尝试的 prompt (Task 3) ===
        // 修复轮次中, first_attempt_prompt 已发送过会被增量跟踪跳过,
        // 只发送修复 prompt, 减少 token 消耗。
        let mut first_attempt_prompt: Option<String> = None;

        let mut attempt = 1u32;
        while attempt <= self.max_rounds_per_task {
            self.memory.phases[phase_idx].tasks[task_idx].attempts = attempt;

            let mut attempt_prompt = if attempt == 1 {
                prompt.to_string()
            } else {
                // === 人工干预: 确认修复重试 (方向 A) ===
                let fix_context = FixContext {
                    phase_idx,
                    task_idx,
                    attempt,
                    max_attempts: self.max_rounds_per_task,
                    feedback: last_feedback.clone(),
                };
                let should_fix = self.interaction.confirm_fix(&fix_context).await?;
                if !should_fix {
                    println!("    ⏭ 修复被跳过");
                    self.do_rollback(phase_idx, &task_id);
                    self.memory.phases[phase_idx].tasks[task_idx].status = TaskStatus::Failed;
                    self.memory.add_decision(
                        phase_idx,
                        Some(&task_id),
                        "修复被人类跳过",
                        "人类选择不继续修复",
                    );
                    return Ok(false);
                }

                // 修复轮: 增量修复 — 只发送有错误的文件 + 错误信息
                // SRP: 委托给 FixPromptBuilder
                let base_prompt = FixPromptBuilder::build_fix_prompt(
                    &self.workspace,
                    &self.memory,
                    &last_errors,
                    &last_feedback,
                    phase_idx,
                    task_idx,
                );

                // === 智能错误诊断增强 (方向 F) ===
                // 将诊断结果追加到修复 prompt 前面, 为 AI 提供更精准的修复指导
                let with_diagnosis = if let Some(ref diagnosis) = last_diagnosis {
                    if diagnosis.has_guidance() {
                        format!(
                            "🔍 错误诊断 ({})\n\
                             分析: {}\n\
                             修复建议: {}\n\
                             ────────────────────────────────\n\n{}",
                            diagnosis.category,
                            diagnosis.analysis,
                            diagnosis.fix_guidance,
                            base_prompt,
                        )
                    } else {
                        base_prompt
                    }
                } else {
                    base_prompt
                };

                // === web_tool 深度集成: 追加搜索结果 ===
                // 将编译错误自动搜索的结果追加到修复 prompt 后面,
                // 为 AI 提供额外的解决方案上下文
                if let Some(ref search_section) = last_search_results {
                    format!("{}{}", with_diagnosis, search_section)
                } else {
                    with_diagnosis
                }
            };

            // === 循环终止检测: 策略改变 (借鉴方向 3) ===
            // 检测到死循环时, 在修复 prompt 前追加策略改变提示
            if attempt > 1 {
                let strategy_prompt = if let Some(ref mut detector) = self.loop_detector {
                    if detector.is_looping() {
                        Some(detector.loop_strategy_prompt())
                    } else {
                        None
                    }
                } else {
                    None
                };

                if let Some(strategy) = strategy_prompt {
                    info!("    🔄 循环终止检测: 检测到死循环, 改变策略");
                    println!("    🔄 循环终止检测: 检测到修复死循环, 改变修复策略");

                    // === DevTrace: 循环终止检测 (借鉴方向 4) ===
                    let ld_task_name = self.memory.phases[phase_idx].tasks[task_idx].name.clone();
                    self.trace_dev(
                        TraceAction::LoopDetection,
                        Some(phase_idx),
                        Some(task_idx),
                        Some(&ld_task_name),
                        &attempt_prompt,
                        &strategy,
                        0,
                        false,
                        Some("检测到修复死循环, 改变策略"),
                    );

                    attempt_prompt = format!("{}\n\n{}", strategy, attempt_prompt);
                }
            }

            println!("    尝试 {}/{}...", attempt, self.max_rounds_per_task);

            // 发送给 AI (DIP: 通过 ChatClient trait) (注入转向提醒)
            self.memory
                .add_conversation("user", &attempt_prompt, Some(&task_id));
            let steered_prompt = self.maybe_steer_reminder(&attempt_prompt);

            // === 增量发送集成 (Task 3) ===
            // 首次尝试后存储 steered_prompt, 修复轮次中用于增量跳过
            if attempt == 1 {
                first_attempt_prompt = Some(steered_prompt.clone());
            }

            let msg_start = Instant::now();
            let result = self
                .send_attempt_prompt(
                    &steered_prompt,
                    &first_attempt_prompt,
                    attempt > 1,
                    self.timeout_secs,
                )
                .await?;
            let msg_duration = msg_start.elapsed().as_millis() as u64;

            // === DevTrace: 任务执行/修复尝试 (借鉴方向 4) ===
            let et_task_name = self.memory.phases[phase_idx].tasks[task_idx].name.clone();
            self.trace_dev(
                if attempt == 1 {
                    TraceAction::TaskExecution
                } else {
                    TraceAction::FixAttempt
                },
                Some(phase_idx),
                Some(task_idx),
                Some(&et_task_name),
                &attempt_prompt,
                &result.text,
                msg_duration,
                !result.timed_out,
                if result.timed_out {
                    Some("AI 回复超时")
                } else {
                    None
                },
            );

            self.memory
                .add_conversation("assistant", &result.text, Some(&task_id));
            self.save_memory(); // 保存时机: 每次 AI 对话后

            // === 上下文衔接检查 (借鉴方向 1) ===
            // 对话过长时新开对话并交接上下文
            self.maybe_context_handoff().await?;

            // === 自主追问 (核心中的核心) ===
            // 检查 AI 回复是否需要追问, 如需则发送追问消息
            let final_text = self
                .check_and_clarify(phase_idx, task_idx, &result.text, result.timed_out)
                .await?;

            // === AI 自主指令 (Slash Commands) (借鉴方向 5) ===
            // 检测 AI 回复中的 slash commands 并执行对应操作
            let slash_action = self
                .process_slash_commands(phase_idx, task_idx, &final_text)
                .await?;
            if slash_action.should_skip() {
                // /skip 指令: 跳过当前任务
                println!("    ⏭ AI 发出 /skip 指令, 跳过当前任务");
                self.do_rollback(phase_idx, &task_id);
                self.memory.phases[phase_idx].tasks[task_idx].status = TaskStatus::Failed;
                return Ok(false);
            }

            // === 回调处理器链 — 借鉴 MediaCrawler callback 模式 (Session 69) ===
            // 如果启用了 handler_chain, 先通过 handler 链处理 AI 回复:
            // - CodeExtractorHandler: 提取代码文件 (统计)
            // - TraceWriterHandler: 记录 trace (轻量计数)
            // - MemoryUpdaterHandler: 更新记忆 (计数)
            //
            // handler_chain 只做预处理和统计, 实际的代码提取仍由 FileExtractor 完成
            // (保持 DIP 架构: handler_chain 是可选增强, 不替代核心提取逻辑)
            let clean_text = slash_command::strip_commands(&final_text);
            if let Some(ref chain) = self.handler_chain {
                let task_name = &self.memory.phases[phase_idx].tasks[task_idx].name;
                let ctx = TaskContext::new(
                    if attempt == 1 { "develop" } else { "fix" },
                    task_name,
                    &self.workspace.root.to_string_lossy(),
                )
                .with_turn(self.memory.conversations.len());
                if let Err(e) = chain.execute(&clean_text, &ctx).await {
                    debug!("HandlerChain 执行失败 (非致命): {}", e);
                }
            }

            // 提取代码文件 (DIP: 通过 FileExtractor trait)
            let files = self.extractor.extract(&clean_text);
            if files.is_empty() {
                warn!("    AI 回复中没有代码文件");
                if attempt == self.max_rounds_per_task {
                    // 版本管理: 最终失败,尝试回滚到 known good
                    self.do_rollback(phase_idx, &task_id);
                    self.memory.phases[phase_idx].tasks[task_idx].status = TaskStatus::Failed;
                    return Ok(false);
                }
                attempt += 1;
                continue;
            }

            println!("    提取 {} 个文件", files.len());
            for f in &files {
                println!("      {} ({}字符)", f.path, f.content.len());
            }

            // === 自动修复: 对 Rust 文件应用 apply_fixes (Session 118) ===
            let files = if self.auto_fix_enabled {
                self.apply_auto_fixes_to_files(files)
            } else {
                files
            };

            // 写入工作区 (带版本快照: 先保存将被覆盖的文件)
            let (_snap_id, written) = self
                .workspace
                .write_files_with_snapshot(&files, &format!("pre_write_attempt{}", attempt))?;
            let written_paths: Vec<String> = written
                .iter()
                .map(|p| {
                    p.strip_prefix(&self.workspace.root)
                        .map(|s| s.display().to_string())
                        .unwrap_or_else(|_| p.display().to_string())
                })
                .collect();
            self.memory.phases[phase_idx].tasks[task_idx].files_written = written_paths.clone();

            // 更新工作区文件列表
            self.memory.workspace_files = self
                .workspace
                .list_files()
                .unwrap_or_default()
                .into_iter()
                .filter(|f| !f.starts_with("target/"))
                .collect();

            // === clippy 检查: 代码写入后自动运行 cargo clippy (Session 120) ===
            if self.clippy_check_enabled {
                self.run_clippy_on_project();
            }

            // 编译检查 (DIP: 通过 TestRunner trait)
            println!("    运行 cargo check...");
            let check_start = Instant::now();
            let mut check_result = self.test_runner.check(&self.workspace.root)?;

            // === 网络错误重试 (orchestrator 层面) ===
            // run_cargo 已重试 3 次 (5s 间隔), 如果仍然失败, 在 orchestrator 层面再重试
            // 使用更长的间隔 (30s) 等待网络恢复
            let mut check_net_retries = 0u32;
            while check_result.is_network_error() && check_net_retries < MAX_ORCH_NETWORK_RETRIES {
                check_net_retries += 1;
                warn!(
                    "    ⚠️ 编译检查遇到网络错误, {}s 后重试 ({}/{})",
                    ORCH_NETWORK_RETRY_INTERVAL, check_net_retries, MAX_ORCH_NETWORK_RETRIES
                );
                println!(
                    "    ⚠️ 网络错误, {}s 后重试编译检查 ({}/{})",
                    ORCH_NETWORK_RETRY_INTERVAL, check_net_retries, MAX_ORCH_NETWORK_RETRIES
                );
                tokio::time::sleep(Duration::from_secs(ORCH_NETWORK_RETRY_INTERVAL)).await;
                check_result = self.test_runner.check(&self.workspace.root)?;
            }

            let check_duration = check_start.elapsed().as_millis() as u64;

            // === DevTrace: 编译检查 (借鉴方向 4) ===
            let cc_task_name = self.memory.phases[phase_idx].tasks[task_idx].name.clone();
            let cc_feedback = check_result.to_feedback();
            self.trace_dev(
                TraceAction::CompileCheck,
                Some(phase_idx),
                Some(task_idx),
                Some(&cc_task_name),
                "cargo check",
                &cc_feedback,
                check_duration,
                check_result.success,
                if check_result.success {
                    None
                } else {
                    Some(&cc_feedback)
                },
            );

            // === 缓存策略自动调优 (Session 82) ===
            // 在每次编译检查后评估缓存命中与未命中的修复成功率,
            // 自动调整 TTL 或禁用缓存
            self.evaluate_cache_tuning(phase_idx, task_idx);

            // === 搜索质量评估 (Session 85) ===
            // 在每次编译检查后评估搜索质量, 当搜索有害时自动禁用
            self.evaluate_search_quality(phase_idx, task_idx);

            // === Memory 评估 (Session 90) ===
            // 在每次编译检查后评估 Memory 注入效果, 当注入有害时自动禁用
            self.evaluate_memory_context(phase_idx, task_idx);

            // === 联合决策评估 (Session 99) ===
            // 在三个评估器各自评估后, 综合状态做出联合决策
            self.evaluate_joint_decision(phase_idx, task_idx);

            if check_result.success {
                println!("    ✅ 编译成功");

                // 版本管理: 编译通过,保存 known good 快照 (SRP: 委托给 VersionManager)
                match VersionManager::save_known_good(&self.workspace) {
                    Ok(Some(good_id)) => {
                        self.memory.phases[phase_idx].tasks[task_idx].last_good_snapshot =
                            Some(good_id);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        warn!("    保存 known good 失败: {}", e);
                    }
                }

                // 运行测试 (DIP: 通过 TestRunner trait)
                println!("    运行 cargo test...");
                let test_start = Instant::now();
                let mut test_result = self.test_runner.test(&self.workspace.root)?;

                // === 网络错误重试 (orchestrator 层面) ===
                let mut test_net_retries = 0u32;
                while test_result.is_network_error() && test_net_retries < MAX_ORCH_NETWORK_RETRIES
                {
                    test_net_retries += 1;
                    warn!(
                        "    ⚠️ 测试运行遇到网络错误, {}s 后重试 ({}/{})",
                        ORCH_NETWORK_RETRY_INTERVAL, test_net_retries, MAX_ORCH_NETWORK_RETRIES
                    );
                    println!(
                        "    ⚠️ 网络错误, {}s 后重试测试 ({}/{})",
                        ORCH_NETWORK_RETRY_INTERVAL, test_net_retries, MAX_ORCH_NETWORK_RETRIES
                    );
                    tokio::time::sleep(Duration::from_secs(ORCH_NETWORK_RETRY_INTERVAL)).await;
                    test_result = self.test_runner.test(&self.workspace.root)?;
                }

                let test_duration = test_start.elapsed().as_millis() as u64;
                let test_feedback = test_result.to_feedback();
                println!("    {}", test_feedback.replace('\n', "\n    "));

                // === DevTrace: 测试运行 (借鉴方向 4) ===
                let tr_task_name = self.memory.phases[phase_idx].tasks[task_idx].name.clone();
                self.trace_dev(
                    TraceAction::TestRun,
                    Some(phase_idx),
                    Some(task_idx),
                    Some(&tr_task_name),
                    "cargo test",
                    &test_feedback,
                    test_duration,
                    test_result.success,
                    if test_result.success {
                        None
                    } else {
                        Some(&test_feedback)
                    },
                );

                self.memory.phases[phase_idx].tasks[task_idx].test_result =
                    Some(test_feedback.clone());

                if test_result.success {
                    // === E2E 测试 (方向 A) ===
                    // cargo test 通过后, 检查是否有 E2E 测试用例
                    let e2e_cases = load_e2e_tests_from_workspace(&self.workspace.root);
                    if !e2e_cases.is_empty() {
                        println!("    运行 E2E 测试 ({} 个用例)...", e2e_cases.len());
                        let e2e_results = self
                            .test_runner
                            .run_binary(&self.workspace.root, &e2e_cases)?;

                        let e2e_summary = E2ETestSummary {
                            total: e2e_results.len(),
                            passed: e2e_results.iter().filter(|r| r.passed).count(),
                            failed: e2e_results.iter().filter(|r| !r.passed).count(),
                            results: e2e_results,
                        };

                        let e2e_feedback = e2e_summary.to_feedback();
                        println!("    {}", e2e_feedback.replace('\n', "\n    "));

                        // === DevTrace: E2E 测试 (借鉴方向 4) ===
                        let e2e_task_name =
                            self.memory.phases[phase_idx].tasks[task_idx].name.clone();
                        self.trace_dev(
                            TraceAction::E2ETest,
                            Some(phase_idx),
                            Some(task_idx),
                            Some(&e2e_task_name),
                            &format!("E2E 测试 ({} 个用例)", e2e_cases.len()),
                            &e2e_feedback,
                            0,
                            e2e_summary.success(),
                            if e2e_summary.success() {
                                None
                            } else {
                                Some(&e2e_feedback)
                            },
                        );

                        if e2e_summary.success() {
                            self.memory.phases[phase_idx].tasks[task_idx].status =
                                TaskStatus::Completed;
                            self.memory.phases[phase_idx].tasks[task_idx].result =
                                Some("成功 (含 E2E 测试)".to_string());
                            self.memory.add_decision(
                                phase_idx,
                                Some(&task_id),
                                "任务完成",
                                "编译、测试和 E2E 测试都通过",
                            );
                            return Ok(true);
                        } else {
                            // E2E 测试失败, 记录并进入修复轮
                            warn!("    E2E 测试失败,尝试修复...");
                            self.memory.add_decision(
                                phase_idx,
                                Some(&task_id),
                                "E2E 测试失败,进入修复",
                                &e2e_feedback,
                            );

                            // E2E 失败反馈用于下一轮修复
                            last_errors = vec![];
                            last_feedback = e2e_feedback;

                            // === 智能错误诊断 (方向 F) ===
                            if let Some(diagnoser) = self.error_diagnoser.as_ref() {
                                let ctx = DiagnosisContext {
                                    task_prompt: self.memory.phases[phase_idx].tasks[task_idx]
                                        .prompt
                                        .clone(),
                                    attempt,
                                    max_attempts: self.max_rounds_per_task,
                                    files_written: self.memory.phases[phase_idx].tasks[task_idx]
                                        .files_written
                                        .clone(),
                                };
                                let result = diagnoser
                                    .diagnose(&[], &last_feedback, &ctx, &self.error_history)
                                    .await;
                                info!(
                                    "    🔍 错误诊断: {} (来源: {})",
                                    result.category, result.source
                                );
                                println!(
                                    "    🔍 错误诊断: {} ({})",
                                    result.category, result.source
                                );
                                last_diagnosis = Some(result);
                            }

                            if attempt == self.max_rounds_per_task {
                                self.do_rollback(phase_idx, &task_id);
                                self.memory.phases[phase_idx].tasks[task_idx].status =
                                    TaskStatus::Failed;
                                let _ = self.error_history.save_to_workspace();
                                return Ok(false);
                            }
                        }
                    } else {
                        // 无 E2E 测试用例, 直完成任务
                        self.memory.phases[phase_idx].tasks[task_idx].status =
                            TaskStatus::Completed;
                        self.memory.phases[phase_idx].tasks[task_idx].result =
                            Some("成功".to_string());
                        self.memory.add_decision(
                            phase_idx,
                            Some(&task_id),
                            "任务完成",
                            "编译和测试都通过",
                        );
                        return Ok(true);
                    }
                } else {
                    // 测试失败,记录并进入修复轮
                    // === 网络错误跳过: 不消耗 AI 修复轮次 ===
                    if test_result.is_network_error()
                        && network_error_skips < MAX_NETWORK_ERROR_SKIPS
                    {
                        network_error_skips += 1;
                        warn!(
                            "    ⚠️ 测试失败 (网络错误), 跳过 AI 修复, 直接重试 ({}/{})",
                            network_error_skips, MAX_NETWORK_ERROR_SKIPS
                        );
                        println!(
                            "    ⚠️ 网络错误, 跳过 AI 修复, 等待 30s 后重试 ({}/{})",
                            network_error_skips, MAX_NETWORK_ERROR_SKIPS
                        );
                        // 不设置 last_errors/last_feedback, 不消耗修复轮次
                        tokio::time::sleep(Duration::from_secs(ORCH_NETWORK_RETRY_INTERVAL)).await;
                        continue;
                    }

                    warn!("    测试失败,尝试修复...");
                    self.memory.add_decision(
                        phase_idx,
                        Some(&task_id),
                        "测试失败,进入修复",
                        &test_feedback,
                    );

                    // 记录错误信息,用于下一轮增量修复
                    last_errors = test_result.errors.clone();
                    last_feedback = test_feedback;

                    // === 智能错误诊断 (方向 F) ===
                    if let Some(diagnoser) = self.error_diagnoser.as_ref() {
                        let ctx = DiagnosisContext {
                            task_prompt: self.memory.phases[phase_idx].tasks[task_idx]
                                .prompt
                                .clone(),
                            attempt,
                            max_attempts: self.max_rounds_per_task,
                            files_written: self.memory.phases[phase_idx].tasks[task_idx]
                                .files_written
                                .clone(),
                        };
                        let result = diagnoser
                            .diagnose(&last_errors, &last_feedback, &ctx, &self.error_history)
                            .await;
                        info!(
                            "    🔍 错误诊断: {} (来源: {}, 置信度: {:.0}%)",
                            result.category,
                            result.source,
                            result.confidence * 100.0
                        );
                        println!("    🔍 错误诊断: {} ({})", result.category, result.source);
                        for err in &last_errors {
                            self.error_history.record(err, result.category, false);
                        }
                        last_diagnosis = Some(result);
                    }

                    // === web_tool 深度集成: 编译错误自动搜索 ===
                    // 仅在修复轮次 (attempt > 1) 且非网络错误时搜索
                    last_search_results = self
                        .auto_search_error_solutions(
                            &last_errors,
                            attempt,
                            false,
                            phase_idx,
                            task_idx,
                        )
                        .await
                        .ok()
                        .flatten();

                    if attempt == self.max_rounds_per_task {
                        // 版本管理: 最终失败,回滚到 known good
                        self.do_rollback(phase_idx, &task_id);
                        self.memory.phases[phase_idx].tasks[task_idx].status = TaskStatus::Failed;
                        let _ = self.error_history.save_to_workspace();
                        return Ok(false);
                    }
                }
            } else {
                // 编译失败
                // === 网络错误跳过: 不消耗 AI 修复轮次 ===
                if check_result.is_network_error() && network_error_skips < MAX_NETWORK_ERROR_SKIPS
                {
                    network_error_skips += 1;
                    warn!(
                        "    ⚠️ 编译失败 (网络错误), 跳过 AI 修复, 直接重试 ({}/{})",
                        network_error_skips, MAX_NETWORK_ERROR_SKIPS
                    );
                    println!(
                        "    ⚠️ 网络错误, 跳过 AI 修复, 等待 30s 后重试 ({}/{})",
                        network_error_skips, MAX_NETWORK_ERROR_SKIPS
                    );
                    // 不设置 last_errors/last_feedback, 不消耗修复轮次
                    // 不递增 attempt, 重试当前轮次
                    tokio::time::sleep(Duration::from_secs(ORCH_NETWORK_RETRY_INTERVAL)).await;
                    continue;
                }

                let feedback = check_result.to_feedback();
                println!("    ❌ 编译失败 ({} 个错误)", check_result.errors.len());
                self.memory.phases[phase_idx].tasks[task_idx].test_result = Some(feedback.clone());
                self.memory
                    .add_decision(phase_idx, Some(&task_id), "编译失败,进入修复", &feedback);

                // 记录错误信息,用于下一轮增量修复
                last_errors = check_result.errors.clone();
                last_feedback = feedback;

                // === 智能错误诊断 (方向 F) ===
                if let Some(diagnoser) = self.error_diagnoser.as_ref() {
                    let ctx = DiagnosisContext {
                        task_prompt: self.memory.phases[phase_idx].tasks[task_idx].prompt.clone(),
                        attempt,
                        max_attempts: self.max_rounds_per_task,
                        files_written: self.memory.phases[phase_idx].tasks[task_idx]
                            .files_written
                            .clone(),
                    };
                    let result = diagnoser
                        .diagnose(&last_errors, &last_feedback, &ctx, &self.error_history)
                        .await;
                    info!(
                        "    🔍 错误诊断: {} (来源: {}, 置信度: {:.0}%)",
                        result.category,
                        result.source,
                        result.confidence * 100.0
                    );
                    println!("    🔍 错误诊断: {} ({})", result.category, result.source);
                    // 记录错误历史
                    for err in &last_errors {
                        self.error_history.record(err, result.category, false);
                    }
                    last_diagnosis = Some(result);
                }

                // === web_tool 深度集成: 编译错误自动搜索 ===
                // 仅在修复轮次 (attempt > 1) 且非网络错误时搜索
                last_search_results = self
                    .auto_search_error_solutions(&last_errors, attempt, false, phase_idx, task_idx)
                    .await
                    .ok()
                    .flatten();

                if attempt == self.max_rounds_per_task {
                    // 版本管理: 最终失败,回滚到 known good
                    self.do_rollback(phase_idx, &task_id);
                    self.memory.phases[phase_idx].tasks[task_idx].status = TaskStatus::Failed;
                    // 保存错误历史
                    let _ = self.error_history.save_to_workspace();
                    return Ok(false);
                }
            }

            // === 循环终止检测: 记录错误 + 跳过检查 (借鉴方向 3) ===
            // 记录本轮失败的错误, 检测是否在原地打转
            if let Some(ref mut detector) = self.loop_detector {
                detector.record_errors(&last_errors);
                if detector.should_skip() {
                    warn!("    🛑 循环终止: 策略改变后仍然死循环, 跳过任务");
                    println!("    🛑 循环终止: 策略改变后仍然死循环, 跳过任务");
                    self.do_rollback(phase_idx, &task_id);
                    self.memory.phases[phase_idx].tasks[task_idx].status = TaskStatus::Failed;
                    self.memory.add_decision(
                        phase_idx,
                        Some(&task_id),
                        "循环终止: 跳过任务",
                        "策略改变后仍然死循环, 跳过以避免浪费修复轮次",
                    );
                    let _ = self.error_history.save_to_workspace();
                    return Ok(false);
                }
            }

            // 等一下再重试
            tokio::time::sleep(Duration::from_secs(2)).await;
            attempt += 1;
        }

        Ok(false)
    }

    /// 版本管理: 回滚到 known good 并记录决策 (SRP: 委托给 VersionManager)
    fn do_rollback(&mut self, phase_idx: usize, task_id: &str) {
        match VersionManager::rollback_to_known_good(&self.workspace) {
            Ok(Some(id)) => {
                println!("    🔄 已回滚到 known good 快照 #{}", id);
                self.memory.add_decision(
                    phase_idx,
                    Some(task_id),
                    "版本回滚",
                    &format!("回滚到快照 #{} (最后一次通过 cargo check 的版本)", id),
                );
            }
            Ok(None) => {
                warn!("    无 known good 快照可回滚 (可能从未通过编译)");
            }
            Err(e) => {
                error!("    版本回滚失败: {}", e);
            }
        }
    }

    /// 需求变更处理 — 检查待处理的变更并重新规划 (方向 B)
    ///
    /// 当检测到有待处理的需求变更时:
    /// 1. 收集所有变更描述
    /// 2. 发送给 AI, 要求基于变更重新规划新的开发阶段
    /// 3. 解析 AI 返回的新阶段 JSON
    /// 4. 追加到现有计划末尾
    /// 5. 标记变更已处理
    async fn handle_requirement_changes(&mut self) -> Result<()> {
        let changes_summary = self.memory.pending_changes_summary();
        if changes_summary.is_empty() {
            return Ok(());
        }

        println!("\n{}", "═".repeat(60));
        println!("  🔄 检测到需求变更, 重新规划...");
        println!("{}", "═".repeat(60));
        println!("  {}", changes_summary);

        // === 人工干预: 确认需求变更 (方向 A) ===
        let should_process = self
            .interaction
            .confirm_requirement_change(&changes_summary)
            .await?;
        if !should_process {
            println!("  ⏭ 需求变更被跳过");
            self.memory.mark_changes_processed();
            self.save_memory();
            return Ok(());
        }

        // 构建重新规划 prompt
        let current_plan = self
            .memory
            .phases
            .iter()
            .map(|p| format!("  - {} ({})", p.name, p.description))
            .collect::<Vec<_>>()
            .join("\n");

        let replan_prompt = format!(
            "原始目标: {}\n\n\
             当前已有计划:\n{}\n\n\
             新的需求变更:\n{}\n\n\
             请基于需求变更, 规划新的开发阶段 (只需输出新增的阶段)。\n\
             输出格式 (严格遵循):\n\
             ```json\n\
             [\n\
               {{\n\
                 \"name\": \"阶段名称\",\n\
                 \"description\": \"阶段描述\",\n\
                 \"tasks\": [\n\
                   {{\n\
                     \"name\": \"任务名称\",\n\
                     \"prompt\": \"给AI的具体指令\"\n\
                   }}\n\
                 ]\n\
               }}\n\
             ]\n\
             ```\n\
             注意:\n\
             - 新阶段应基于需求变更内容\n\
             - 每个任务都要产出可编译的代码\n\
             - prompt 要足够详细, 让 AI 能直接生成代码",
            self.memory.goal, current_plan, changes_summary
        );

        self.memory.add_conversation("user", &replan_prompt, None);
        let steered_replan = self.maybe_steer_reminder(&replan_prompt);
        let replan_start = Instant::now();
        let result = self
            .send_message_safe(&steered_replan, self.timeout_secs)
            .await?;
        let replan_duration = replan_start.elapsed().as_millis() as u64;

        // === DevTrace: 需求变更 (借鉴方向 4) ===
        self.trace_dev(
            TraceAction::RequirementChange,
            Some(self.memory.current_phase),
            None,
            None,
            &replan_prompt,
            &result.text,
            replan_duration,
            true,
            None,
        );

        self.memory
            .add_conversation("assistant", &result.text, None);

        // === 上下文衔接检查 (借鉴方向 1) ===
        self.maybe_context_handoff().await?;

        // 解析 AI 返回的新阶段
        let new_phases = self.parse_plan(&result.text)?;

        if new_phases.is_empty() {
            warn!("AI 未返回有效的新阶段计划, 跳过需求变更");
        } else {
            let new_count = new_phases.len();
            self.memory.append_phases(new_phases);
            println!("\n  ✅ 已追加 {} 个新阶段", new_count);
            self.memory.add_decision(
                self.memory.current_phase,
                None,
                "需求变更重新规划",
                &format!("追加 {} 个新阶段", new_count),
            );
        }

        // 标记变更已处理
        self.memory.mark_changes_processed();
        self.save_memory();
        Ok(())
    }
}

// ============================================================================
//  单元测试: SRP 组件 (FixPromptBuilder, ContextBuilder, VersionManager)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testrunner::CompileError;
    use proptest::prelude::*;
    use tempfile::tempdir;

    /// 创建临时工作区并初始化
    fn make_ws() -> (tempfile::TempDir, Workspace) {
        let dir = tempdir().unwrap();
        let ws = Workspace::new(dir.path());
        ws.init().unwrap();
        (dir, ws)
    }

    /// 创建带文件的临时工作区
    fn make_ws_with_files() -> (tempfile::TempDir, Workspace) {
        let (dir, ws) = make_ws();
        ws.write_file("src/main.rs", "fn main() {\n    println!(\"hello\");\n}")
            .unwrap();
        ws.write_file("Cargo.toml", "[package]\nname = \"test\"")
            .unwrap();
        ws.write_file("src/lib.rs", "pub fn hello() {}").unwrap();
        (dir, ws)
    }

    // ===== FixPromptBuilder::normalize_error_path =====

    #[test]
    fn test_normalize_relative_path() {
        let (_dir, ws) = make_ws();
        let result = FixPromptBuilder::normalize_error_path(&ws, "src/main.rs");
        assert_eq!(result, Some("src/main.rs".to_string()));
    }

    #[test]
    fn test_normalize_absolute_path_in_workspace() {
        let (_dir, ws) = make_ws_with_files();
        let abs_path = format!("{}/src/main.rs", ws.root.display());
        let result = FixPromptBuilder::normalize_error_path(&ws, &abs_path);
        assert_eq!(result, Some("src/main.rs".to_string()));
    }

    #[test]
    fn test_normalize_external_absolute_path() {
        let (_dir, ws) = make_ws();
        let result = FixPromptBuilder::normalize_error_path(
            &ws,
            "/Users/someone/.cargo/registry/src/something.rs",
        );
        assert_eq!(result, None);
    }

    #[test]
    fn test_normalize_empty_path() {
        let (_dir, ws) = make_ws();
        let result = FixPromptBuilder::normalize_error_path(&ws, "");
        assert_eq!(result, None);
    }

    #[test]
    fn test_normalize_whitespace_path() {
        let (_dir, ws) = make_ws();
        let result = FixPromptBuilder::normalize_error_path(&ws, "  src/main.rs  ");
        assert_eq!(result, Some("src/main.rs".to_string()));
    }

    // ===== FixPromptBuilder::get_files_full_content =====

    #[test]
    fn test_get_files_full_content_existing() {
        let (_dir, ws) = make_ws_with_files();
        let content = FixPromptBuilder::get_files_full_content(
            &ws,
            &["src/main.rs".to_string(), "Cargo.toml".to_string()],
        );
        assert!(content.contains("--- src/main.rs"));
        assert!(content.contains("fn main()"));
        assert!(content.contains("--- Cargo.toml"));
        assert!(content.contains("[package]"));
    }

    #[test]
    fn test_get_files_full_content_nonexistent() {
        let (_dir, ws) = make_ws();
        let content = FixPromptBuilder::get_files_full_content(&ws, &["nope.rs".to_string()]);
        assert!(content.contains("文件不存在"));
    }

    #[test]
    fn test_get_files_full_content_empty() {
        let (_dir, ws) = make_ws();
        let content = FixPromptBuilder::get_files_full_content(&ws, &[]);
        assert_eq!(content, "(无文件)");
    }

    #[test]
    fn test_get_files_full_content_shows_line_count() {
        let (_dir, ws) = make_ws();
        ws.write_file("test.rs", "line1\nline2\nline3\n").unwrap();
        let content = FixPromptBuilder::get_files_full_content(&ws, &["test.rs".to_string()]);
        assert!(content.contains("3行"));
    }

    // ===== FixPromptBuilder::build_fix_prompt =====

    #[test]
    fn test_build_fix_prompt_empty_feedback_fallback() {
        let (_dir, ws) = make_ws_with_files();
        let memory = Memory::new("test");
        let prompt = FixPromptBuilder::build_fix_prompt(&ws, &memory, &[], "", 0, 0);
        assert!(prompt.contains("之前的尝试未成功"));
        assert!(prompt.contains("当前代码"));
    }

    #[test]
    fn test_build_fix_prompt_with_compile_errors() {
        let (_dir, ws) = make_ws_with_files();
        let memory = Memory::new("test");
        let errors = vec![CompileError {
            file: "src/main.rs".to_string(),
            line: Some(10),
            column: Some(5),
            message: "mismatched types".to_string(),
            error_code: Some("E0308".to_string()),
        }];
        let prompt = FixPromptBuilder::build_fix_prompt(&ws, &memory, &errors, "编译错误", 0, 0);
        assert!(prompt.contains("编译/测试错误"));
        assert!(prompt.contains("src/main.rs"));
        assert!(prompt.contains("fn main()"));
        assert!(prompt.contains("请根据错误信息修复代码"));
    }

    #[test]
    fn test_build_fix_prompt_with_external_error_files_fallback() {
        let (_dir, ws) = make_ws_with_files();
        let mut memory = Memory::new("test");
        // 设置 phases 以测试无特定错误文件时的回退
        memory.set_phases(vec![Phase {
            id: 0,
            name: "test".to_string(),
            description: "".to_string(),
            status: PhaseStatus::InProgress,
            tasks: vec![Task {
                id: "0-0".to_string(),
                phase_id: 0,
                name: "test".to_string(),
                prompt: "".to_string(),
                status: TaskStatus::InProgress,
                result: None,
                attempts: 1,
                files_written: vec!["src/main.rs".to_string()],
                test_result: None,
                last_good_snapshot: None,
                clarifications: vec![],
                depends_on: vec![],
            }],
        }]);
        // 外部依赖路径,normalize 后为 None → 无特定错误文件
        let errors = vec![CompileError {
            file: "/Users/x/.cargo/registry/lib.rs".to_string(),
            line: Some(1),
            column: Some(1),
            message: "error in dependency".to_string(),
            error_code: None,
        }];
        let prompt = FixPromptBuilder::build_fix_prompt(&ws, &memory, &errors, "测试错误", 0, 0);
        // 应回退到发送本任务文件
        assert!(prompt.contains("测试错误"));
        assert!(prompt.contains("src/main.rs"));
    }

    // ===== ContextBuilder =====

    #[test]
    fn test_context_summary_with_files() {
        let (_dir, ws) = make_ws_with_files();
        let summary = ContextBuilder::get_current_code_summary(&ws);
        assert!(summary.contains("src/main.rs"));
        assert!(summary.contains("Cargo.toml"));
        assert!(summary.contains("行"));
    }

    #[test]
    fn test_context_summary_empty_workspace() {
        let (_dir, ws) = make_ws();
        let summary = ContextBuilder::get_current_code_summary(&ws);
        assert_eq!(summary, "(空项目)");
    }

    #[test]
    fn test_context_summary_excludes_target() {
        let (_dir, ws) = make_ws_with_files();
        ws.write_file("target/debug/output", "binary").unwrap();
        let summary = ContextBuilder::get_current_code_summary(&ws);
        assert!(!summary.contains("target/"));
    }

    #[test]
    fn test_project_file_list_empty() {
        let memory = Memory::new("test");
        let list = ContextBuilder::get_project_file_list(&memory);
        assert_eq!(list, "(空项目)");
    }

    #[test]
    fn test_project_file_list_with_files() {
        let mut memory = Memory::new("test");
        memory.workspace_files = vec!["src/main.rs".to_string(), "Cargo.toml".to_string()];
        let list = ContextBuilder::get_project_file_list(&memory);
        assert!(list.contains("src/main.rs"));
        assert!(list.contains("Cargo.toml"));
    }

    // ===== VersionManager =====

    #[test]
    fn test_version_manager_save_known_good() {
        let (_dir, ws) = make_ws_with_files();
        let result = VersionManager::save_known_good(&ws).unwrap();
        assert!(result.is_some());
        assert_eq!(ws.get_known_good_id(), result);
    }

    #[test]
    fn test_version_manager_rollback_to_known_good() {
        let (_dir, ws) = make_ws_with_files();
        // 先保存 known good
        VersionManager::save_known_good(&ws).unwrap();
        // 破坏文件
        ws.write_file("src/main.rs", "broken {{{").unwrap();
        // 回滚
        let result = VersionManager::rollback_to_known_good(&ws).unwrap();
        assert!(result.is_some());
        assert_eq!(
            ws.read_file("src/main.rs").unwrap(),
            "fn main() {\n    println!(\"hello\");\n}"
        );
    }

    #[test]
    fn test_version_manager_rollback_no_known_good() {
        let (_dir, ws) = make_ws_with_files();
        let result = VersionManager::rollback_to_known_good(&ws).unwrap();
        assert_eq!(result, None);
    }

    // ===== 截断 JSON 恢复测试 =====

    #[test]
    fn test_json_prefix_stripped() {
        // AI 回复可能以 "json" 开头 (markdown 代码块标记残留)
        // parse_plan 应剥离 "json" 前缀后正确解析 JSON
        let text_with_prefix = r#"json     [{"name":"阶段1","description":"desc","tasks":[{"name":"task1","prompt":"p1"}]}]"#;
        // 验证剥离 "json" 前缀后可以正确解析
        let stripped = text_with_prefix
            .trim_start()
            .strip_prefix("json")
            .map(|s| s.trim_start())
            .unwrap_or(text_with_prefix);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(stripped).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["name"], "阶段1");
    }

    #[test]
    fn test_json_prefix_not_stripped_for_valid_json() {
        // 不以 "json" 开头的有效 JSON 不应受影响
        let text = r#"[{"name":"阶段1"}]"#;
        let stripped = text
            .trim_start()
            .strip_prefix("json")
            .map(|s| s.trim_start())
            .unwrap_or(text);
        assert_eq!(stripped, text, "有效 JSON 不应被修改");
    }

    #[test]
    fn test_repair_truncated_json_valid_json() {
        // 完整的 JSON 不需要修复
        let json = r#"[{"name":"阶段1"},{"name":"阶段2"}]"#;
        let repaired = repair_truncated_json(json);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&repaired).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn test_repair_truncated_json_missing_closing_bracket() {
        // 缺少闭合 ] — 只有完整对象
        let json = r#"[{"name":"阶段1"},{"name":"阶段2"}"#;
        let repaired = repair_truncated_json(json);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&repaired).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn test_repair_truncated_json_mid_object() {
        // 在对象中间截断 — 只保留完整的对象
        let json = r#"[{"name":"阶段1","tasks":[{"name":"任务1"}]},{"name":"阶段2"#;
        let repaired = repair_truncated_json(json);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&repaired).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["name"], "阶段1");
    }

    #[test]
    fn test_repair_truncated_json_with_trailing_comma() {
        // 末尾有逗号 — 应自动修复
        let json = r#"[{"name":"阶段1"},]"#;
        let repaired = repair_truncated_json(json);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&repaired).unwrap();
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn test_repair_truncated_json_nested_objects() {
        // 嵌套对象的截断
        let json = r#"[{"name":"阶段1","tasks":[{"name":"任务1","prompt":"test"}]},{"name":"阶段2","tasks":[{"name":"任务2","#;
        let repaired = repair_truncated_json(json);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&repaired).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["name"], "阶段1");
        let tasks = parsed[0]["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["name"], "任务1");
    }

    #[test]
    fn test_repair_truncated_json_string_with_braces() {
        // 字符串中包含花括号 — 不应误判
        let json = r#"[{"name":"阶段{1}","prompt":"test{"}]"#;
        let repaired = repair_truncated_json(json);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&repaired).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["name"], "阶段{1}");
    }

    #[test]
    fn test_repair_truncated_json_empty() {
        let repaired = repair_truncated_json("");
        assert_eq!(repaired, "[]");
    }

    #[test]
    fn test_repair_truncated_json_no_complete_object() {
        // 没有完整对象 — 返回空数组
        let json = r#"[{"name":"阶段1"#;
        let repaired = repair_truncated_json(json);
        assert_eq!(repaired, "[]");
    }

    #[test]
    fn test_repair_truncated_json_escaped_quotes() {
        // 包含转义引号
        let json = r#"[{"name":"阶段\"1\"","prompt":"test"},{"name":"阶段2"#;
        let repaired = repair_truncated_json(json);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&repaired).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["name"], r#"阶段"1""#);
    }

    #[test]
    fn test_repair_truncated_json_multiple_phases() {
        // 模拟实际 AI 回复被截断的场景 (类似第 28 次任务遇到的问题)
        let json = r#"[
  {
    "name": "阶段1: 项目初始化",
    "description": "创建 Rust CLI 项目基础结构",
    "tasks": [
      {"name": "创建 Cargo.toml", "prompt": "创建项目的 Cargo.toml 文件, 包含基本依赖"},
      {"name": "创建 main.rs", "prompt": "创建 src/main.rs 入口文件"}
    ]
  },
  {
    "name": "阶段2: 核心功能",
    "description": "实现核心功能模块",
    "tasks": [
      {"name": "解析器", "prompt": "实现命令解析器, 支持 --help --version 等参数"},
      {"name": "执行器", "prompt": "实现命令执行器, 调用对应处理函数"}
    ]
  },
  {
    "name": "阶段3: 测试和文档",
    "description": "完善测试和文档",
    "tasks": [
      {"name": "单元测试", "prompt": "编写全面的单元测试"#;
        // 在阶段3的 task prompt 中间截断
        let repaired = repair_truncated_json(json);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&repaired).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0]["name"], "阶段1: 项目初始化");
        assert_eq!(parsed[1]["name"], "阶段2: 核心功能");
    }

    #[test]
    fn test_repair_truncated_json_simulate_12000_chars() {
        // 模拟 AI 回复 12000+ 字符在中间截断的场景
        let mut json = String::from("[");
        for i in 0..5 {
            json.push_str(&format!(
                r#"{{"name":"阶段{}","description":"{}","tasks":[{{"name":"任务{}","prompt":"{}"}}]}},"#,
                i, "描述".repeat(200), i, "指令".repeat(200)
            ));
        }
        // 截断最后一个对象 (去掉末尾的不完整部分)
        json.push_str(r#"{"name":"阶段5","description":"desc"#);
        let repaired = repair_truncated_json(&json);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&repaired).unwrap();
        assert_eq!(parsed.len(), 5);
    }

    // ===== 额外 edge case 测试 =====

    #[test]
    fn test_normalize_path_with_backslash() {
        let (_dir, ws) = make_ws();
        // 反斜杠开头的路径 → 视为绝对路径 → 不匹配 workspace → None
        let result = FixPromptBuilder::normalize_error_path(&ws, "\\src\\main.rs");
        assert_eq!(result, None);
    }

    #[test]
    fn test_normalize_path_just_slash() {
        let (_dir, ws) = make_ws();
        let result = FixPromptBuilder::normalize_error_path(&ws, "/");
        // "/" starts_with "/" → 尝试匹配 workspace root
        // 不匹配 → None
        assert_eq!(result, None);
    }

    #[test]
    fn test_normalize_path_relative_with_dot() {
        let (_dir, ws) = make_ws();
        let result = FixPromptBuilder::normalize_error_path(&ws, "./src/main.rs");
        // "./src/main.rs" 不以 / 开头 → 相对路径
        assert_eq!(result, Some("./src/main.rs".to_string()));
    }

    #[test]
    fn test_get_files_full_content_multiple_files() {
        let (_dir, ws) = make_ws_with_files();
        let content = FixPromptBuilder::get_files_full_content(
            &ws,
            &["src/main.rs".to_string(), "src/lib.rs".to_string()],
        );
        assert!(content.contains("--- src/main.rs"));
        assert!(content.contains("--- src/lib.rs"));
        assert!(content.contains("fn main()"));
        assert!(content.contains("pub fn hello()"));
    }

    #[test]
    fn test_get_files_full_content_mixed_existing_and_nonexistent() {
        let (_dir, ws) = make_ws_with_files();
        let content = FixPromptBuilder::get_files_full_content(
            &ws,
            &["src/main.rs".to_string(), "nope.rs".to_string()],
        );
        assert!(content.contains("--- src/main.rs"));
        assert!(content.contains("--- nope.rs"));
        assert!(content.contains("文件不存在"));
    }

    #[test]
    fn test_context_summary_limits_to_10_files() {
        let (_dir, ws) = make_ws();
        // 写 15 个文件
        for i in 0..15 {
            ws.write_file(&format!("file{}.rs", i), "fn main() {}")
                .unwrap();
        }
        let summary = ContextBuilder::get_current_code_summary(&ws);
        // get_current_code_summary 取前 10 个文件
        let file_count = summary.lines().filter(|l| l.contains("行")).count();
        assert!(
            file_count <= 10,
            "只显示前 10 个文件, 实际 {} 个",
            file_count
        );
    }

    #[test]
    fn test_context_summary_excludes_cargo_lock() {
        let (_dir, ws) = make_ws_with_files();
        ws.write_file("Cargo.lock", "# lock file").unwrap();
        let summary = ContextBuilder::get_current_code_summary(&ws);
        assert!(!summary.contains("Cargo.lock"));
    }

    #[test]
    fn test_project_file_list_multiple_files() {
        let mut memory = Memory::new("test");
        memory.workspace_files = vec![
            "src/main.rs".to_string(),
            "Cargo.toml".to_string(),
            "README.md".to_string(),
        ];
        let list = ContextBuilder::get_project_file_list(&memory);
        assert!(list.contains("src/main.rs"));
        assert!(list.contains("Cargo.toml"));
        assert!(list.contains("README.md"));
    }

    #[test]
    fn test_version_manager_save_and_rollback_multiple() {
        let (_dir, ws) = make_ws_with_files();
        // 保存 v1
        let v1 = VersionManager::save_known_good(&ws).unwrap();
        // 修改文件
        ws.write_file("src/main.rs", "fn v2() {}").unwrap();
        // 保存 v2
        let v2 = VersionManager::save_known_good(&ws).unwrap();
        assert_ne!(v1, v2);
        // 回滚到 v2
        VersionManager::rollback_to_known_good(&ws).unwrap();
        assert_eq!(ws.read_file("src/main.rs").unwrap(), "fn v2() {}");
    }

    #[test]
    fn test_repair_truncated_json_single_object() {
        // 单个完整对象
        let json = r#"{"name":"阶段1"}"#;
        let repaired = repair_truncated_json(json);
        let parsed: serde_json::Value = serde_json::from_str(&repaired).unwrap();
        assert_eq!(parsed["name"], "阶段1");
    }

    #[test]
    fn test_repair_truncated_json_only_opening() {
        // 只有 [
        let repaired = repair_truncated_json("[");
        assert_eq!(repaired, "[]");
    }

    #[test]
    fn test_repair_truncated_json_nested_arrays() {
        // 嵌套数组
        let json = r#"[{"name":"阶段1","tasks":["a","b"]},{"name":"阶段2","tasks":["c"#;
        let repaired = repair_truncated_json(json);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&repaired).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["name"], "阶段1");
        let tasks = parsed[0]["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 2);
    }

    #[test]
    fn test_repair_truncated_json_with_newlines() {
        // 多行 JSON (实际 AI 回复常见格式)
        let json = r#"[
  {
    "name": "阶段1",
    "tasks": [
      {"name": "任务1"}
    ]
  },
  {
    "name": "阶段2",
    "tasks": [
      {"name": "任务2"
    ]
  }
]"#;
        let repaired = repair_truncated_json(json);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&repaired).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["name"], "阶段1");
    }

    #[test]
    fn test_build_fix_prompt_with_multiple_error_files() {
        let (_dir, ws) = make_ws_with_files();
        ws.write_file("src/lib.rs", "pub fn hello() {}").unwrap();
        let memory = Memory::new("test");
        let errors = vec![
            CompileError {
                file: "src/main.rs".to_string(),
                line: Some(10),
                column: Some(5),
                message: "error 1".to_string(),
                error_code: Some("E0308".to_string()),
            },
            CompileError {
                file: "src/lib.rs".to_string(),
                line: Some(1),
                column: None,
                message: "error 2".to_string(),
                error_code: None,
            },
        ];
        let prompt = FixPromptBuilder::build_fix_prompt(&ws, &memory, &errors, "编译错误", 0, 0);
        assert!(prompt.contains("src/main.rs"));
        assert!(prompt.contains("src/lib.rs"));
        assert!(prompt.contains("fn main()"));
        assert!(prompt.contains("pub fn hello()"));
    }

    #[test]
    fn test_build_fix_prompt_deduplicates_error_files() {
        let (_dir, ws) = make_ws_with_files();
        let memory = Memory::new("test");
        let errors = vec![
            CompileError {
                file: "src/main.rs".to_string(),
                line: Some(10),
                column: Some(5),
                message: "error 1".to_string(),
                error_code: Some("E0308".to_string()),
            },
            CompileError {
                file: "src/main.rs".to_string(),
                line: Some(20),
                column: Some(1),
                message: "error 2".to_string(),
                error_code: Some("E0277".to_string()),
            },
        ];
        let prompt = FixPromptBuilder::build_fix_prompt(&ws, &memory, &errors, "编译错误", 0, 0);
        // 只出现一次
        let count = prompt.matches("--- src/main.rs").count();
        assert_eq!(count, 1);
    }

    // ========================================================================
    //  proptest 属性测试 (Session 68)
    // ========================================================================

    /// 构建 JSON 对象字符串的策略
    fn json_obj_strategy() -> impl Strategy<Value = String> {
        (r"[a-z]{1,8}", r"[a-z]{1,8}").prop_map(|(k, v)| format!(r#"{{"{}":"{}"}}"#, k, v))
    }

    #[test]
    fn prop_valid_json_unchanged() {
        proptest!(|(ref input in json_obj_strategy())| {
            let repaired = repair_truncated_json(input);
            let original_parsed: serde_json::Value = serde_json::from_str(input).unwrap();
            let repaired_parsed: serde_json::Value = serde_json::from_str(&repaired).unwrap();
            prop_assert_eq!(original_parsed, repaired_parsed);
        });
    }

    #[test]
    fn prop_valid_json_array_unchanged() {
        proptest!(|(objs in prop::collection::vec(json_obj_strategy(), 0..10))| {
            let input = format!("[{}]", objs.join(","));
            let repaired = repair_truncated_json(&input);
            let original: serde_json::Value = serde_json::from_str(&input).unwrap();
            let repaired_parsed: serde_json::Value = serde_json::from_str(&repaired).unwrap();
            prop_assert_eq!(original, repaired_parsed);
        });
    }

    #[test]
    fn prop_output_always_valid_json() {
        proptest!(|(ref s in r".{0,200}")| {
            let repaired = repair_truncated_json(s);
            prop_assert!(
                serde_json::from_str::<serde_json::Value>(&repaired).is_ok(),
                "output not valid JSON: input={:?}, output={:?}",
                s,
                repaired
            );
        });
    }

    #[test]
    fn prop_empty_input_returns_empty_array() {
        proptest!(|(ref s in r"\s*")| {
            let repaired = repair_truncated_json(s);
            if s.trim().is_empty() {
                prop_assert_eq!(repaired, "[]");
            }
        });
    }

    #[test]
    fn prop_repaired_never_more_elements() {
        proptest!(|(
            objs in prop::collection::vec(json_obj_strategy(), 1..10),
            trailing in r"[a-z:, ]{0,50}"
        )| {
            let input = format!("[{},{}", objs.join(","), trailing);
            let repaired = repair_truncated_json(&input);
            let repaired_arr: Vec<serde_json::Value> =
                serde_json::from_str(&repaired).unwrap_or_default();
            prop_assert!(
                repaired_arr.len() <= objs.len(),
                "repaired has {} elements but input has {} complete objects: input={:?}",
                repaired_arr.len(),
                objs.len(),
                input
            );
        });
    }

    #[test]
    fn prop_normalize_empty_returns_none() {
        proptest!(|(ref s in r"\s*")| {
            let (_dir, ws) = make_ws();
            let result = FixPromptBuilder::normalize_error_path(&ws, s);
            if s.trim().is_empty() {
                prop_assert!(result.is_none(), "empty path should return None: input={:?}", s);
            }
        });
    }

    #[test]
    fn prop_normalize_relative_returns_some() {
        proptest!(|(path in r"[a-zA-Z0-9_.][a-zA-Z0-9_./-]{0,49}")| {
            let (_dir, ws) = make_ws();
            let path = if path.starts_with('/') || path.starts_with('\\') {
                &path[1..]
            } else {
                &path
            };
            if !path.trim().is_empty() {
                let result = FixPromptBuilder::normalize_error_path(&ws, path);
                prop_assert!(result.is_some(), "relative path should return Some: input={:?}", path);
                prop_assert_eq!(result.unwrap(), path);
            }
        });
    }

    #[test]
    fn prop_get_files_nonexistent_includes_marker() {
        proptest!(|(paths in prop::collection::vec(r"[a-zA-Z0-9_./-]{1,30}\.rs", 1..5))| {
            let (_dir, ws) = make_ws();
            let content = FixPromptBuilder::get_files_full_content(&ws, &paths);
            prop_assert!(
                content.contains("not exist") || content.contains("不存在"),
                "nonexistent files should include marker: paths={:?}",
                paths
            );
        });
    }

    #[test]
    fn prop_get_files_empty_returns_placeholder() {
        let (_dir, ws) = make_ws();
        let content = FixPromptBuilder::get_files_full_content(&ws, &[]);
        assert_eq!(content, "(无文件)");
    }

    // ===== Session 75: send_with_continuation + IncrementalStats 测试 =====

    /// Mock ChatClient for testing send_with_continuation
    use std::sync::{Arc, Mutex};

    struct MockChatClient {
        sent_messages: Arc<Mutex<Vec<String>>>,
        responses: Arc<Mutex<Vec<String>>>,
        turn_count: Arc<std::sync::atomic::AtomicUsize>,
        new_conversation_called: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl MockChatClient {
        fn new(responses: Vec<&str>) -> Self {
            Self {
                sent_messages: Arc::new(Mutex::new(vec![])),
                responses: Arc::new(Mutex::new(
                    responses.into_iter().map(String::from).collect(),
                )),
                turn_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                new_conversation_called: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            }
        }

        fn sent_messages(&self) -> Vec<String> {
            self.sent_messages.lock().unwrap().clone()
        }

        fn new_conversation_count(&self) -> usize {
            self.new_conversation_called
                .load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    #[async_trait::async_trait]
    impl crate::traits::ChatClient for MockChatClient {
        async fn send_message(
            &self,
            msg: &str,
            _timeout: u64,
        ) -> Result<crate::traits::ChatResult> {
            self.sent_messages.lock().unwrap().push(msg.to_string());
            self.turn_count
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let text = {
                let mut queue = self.responses.lock().unwrap();
                if queue.is_empty() {
                    "(empty)".to_string()
                } else {
                    queue.remove(0)
                }
            };
            Ok(crate::traits::ChatResult {
                text,
                timed_out: false,
            })
        }

        async fn start_new_conversation(&self) -> Result<()> {
            self.new_conversation_called
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.turn_count
                .store(0, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        }

        fn conversation_turn_count(&self) -> usize {
            self.turn_count.load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    /// Mock TestRunner for testing
    struct MockTestRunner;
    impl crate::traits::TestRunner for MockTestRunner {
        fn check(&self, _dir: &std::path::Path) -> Result<crate::testrunner::TestResult> {
            Ok(crate::testrunner::TestResult {
                success: true,
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 0,
                errors: vec![],
                test_summary: None,
            })
        }
        fn test(&self, _dir: &std::path::Path) -> Result<crate::testrunner::TestResult> {
            Ok(crate::testrunner::TestResult {
                success: true,
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 0,
                errors: vec![],
                test_summary: None,
            })
        }
    }

    /// Mock FileExtractor for testing
    struct MockExtractor;
    impl crate::traits::FileExtractor for MockExtractor {
        fn extract(&self, _text: &str) -> Vec<crate::extract::ExtractedFile> {
            vec![]
        }
    }

    /// 创建测试用 Orchestrator
    fn make_orchestrator<'a>(
        chat: &'a MockChatClient,
        workspace_dir: &'a str,
    ) -> Orchestrator<'a, MockChatClient, MockTestRunner, MockExtractor> {
        Orchestrator::new(
            chat,
            MockTestRunner,
            MockExtractor,
            workspace_dir,
            "test goal",
            3,
            60,
        )
    }

    #[tokio::test]
    async fn test_send_with_continuation_no_live_continuation() {
        // 未启用 live_continuation 时, 退化为全量发送
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec!["AI response"]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap());

        let messages = vec!["msg1".to_string(), "msg2".to_string()];
        let result = orch.send_with_continuation(&messages, 30).await.unwrap();

        assert_eq!(result.text, "AI response");
        // 应该发送拼接后的消息
        let sent = chat.sent_messages();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("msg1"));
        assert!(sent[0].contains("msg2"));
        // 统计: total=2, sent=2 (全量, 无增量优化)
        assert_eq!(orch.incremental_stats().total_messages, 2);
        assert_eq!(orch.incremental_stats().sent_messages, 2);
        assert_eq!(orch.incremental_stats().skipped_messages, 0);
    }

    #[tokio::test]
    async fn test_send_with_continuation_with_live_continuation_first_send() {
        // 启用 live_continuation, 第一次发送应该是全量
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec!["AI response 1"]);
        let mut orch =
            make_orchestrator(&chat, dir.path().to_str().unwrap()).with_live_continuation();

        let messages = vec!["system_prompt".to_string(), "task_description".to_string()];
        let result = orch.send_with_continuation(&messages, 30).await.unwrap();

        assert_eq!(result.text, "AI response 1");
        let sent = chat.sent_messages();
        assert_eq!(sent.len(), 1);
        // 全量发送: total=2, sent=2
        assert_eq!(orch.incremental_stats().total_messages, 2);
        assert_eq!(orch.incremental_stats().sent_messages, 2);
        assert_eq!(orch.incremental_stats().skipped_messages, 0);
        assert!(orch.incremental_stats().saved_ratio() < 0.001);
    }

    #[tokio::test]
    async fn test_send_with_continuation_with_live_continuation_incremental() {
        // 启用 live_continuation, 第二次发送应该是增量
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec!["AI response 1", "AI response 2"]);
        let mut orch =
            make_orchestrator(&chat, dir.path().to_str().unwrap()).with_live_continuation();

        // 第一次发送: 全量
        let msgs1 = vec![
            "system".to_string(),
            "task1".to_string(),
            "result1".to_string(),
        ];
        orch.send_with_continuation(&msgs1, 30).await.unwrap();

        // 第二次发送: 有增量 (system/task1/result1 已发送, 只有 task2 是新的)
        let msgs2 = vec![
            "system".to_string(),
            "task1".to_string(),
            "result1".to_string(),
            "task2".to_string(),
        ];
        let result = orch.send_with_continuation(&msgs2, 30).await.unwrap();

        assert_eq!(result.text, "AI response 2");
        let sent = chat.sent_messages();
        // 第二次应该只发送增量 (task2)
        assert_eq!(sent.len(), 2); // 两次 send_message 调用
        assert_eq!(sent[1], "task2"); // 第二次只发送了 task2

        // 累计统计: total=7, sent=4 (3+1), skipped=3
        assert_eq!(orch.incremental_stats().total_messages, 7);
        assert_eq!(orch.incremental_stats().sent_messages, 4);
        assert_eq!(orch.incremental_stats().skipped_messages, 3);
        assert!((orch.incremental_stats().saved_ratio() - 3.0 / 7.0).abs() < 0.001);
    }

    #[tokio::test]
    async fn test_send_with_continuation_all_skipped() {
        // 所有消息都已发送过 → 全部跳过
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec!["AI response 1"]);
        let mut orch =
            make_orchestrator(&chat, dir.path().to_str().unwrap()).with_live_continuation();

        let msgs = vec!["msg1".to_string(), "msg2".to_string()];
        // 第一次发送
        orch.send_with_continuation(&msgs, 30).await.unwrap();

        // 第二次发送相同消息 → 全部跳过
        let result = orch.send_with_continuation(&msgs, 30).await.unwrap();

        // 应该返回空结果
        assert!(result.text.is_empty());
        assert!(!result.timed_out);

        // 只调用了一次 send_message (第一次)
        assert_eq!(chat.sent_messages().len(), 1);

        // 统计: total=4, sent=2, skipped=2
        assert_eq!(orch.incremental_stats().total_messages, 4);
        assert_eq!(orch.incremental_stats().sent_messages, 2);
        assert_eq!(orch.incremental_stats().skipped_messages, 2);
    }

    #[tokio::test]
    async fn test_send_with_continuation_empty_messages() {
        // 空消息列表应返回错误
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap());

        let result = orch.send_with_continuation(&[], 30).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_send_with_continuation_single_message() {
        // 单条消息应直接发送
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec!["AI response"]);
        let mut orch =
            make_orchestrator(&chat, dir.path().to_str().unwrap()).with_live_continuation();

        let msgs = vec!["hello world".to_string()];
        let result = orch.send_with_continuation(&msgs, 30).await.unwrap();

        assert_eq!(result.text, "AI response");
        let sent = chat.sent_messages();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0], "hello world");
    }

    #[tokio::test]
    async fn test_reset_continuation_after_context_handoff() {
        // 验证上下文衔接后 reset_continuation 被调用
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec!["AI response 1", "AI response 2", "AI response 3"]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_live_continuation()
            .with_context_handoff(1); // 1轮就触发上下文衔接

        // 第一次发送
        let msgs1 = vec!["msg1".to_string()];
        orch.send_with_continuation(&msgs1, 30).await.unwrap();

        // 验证 live_continuation 已追踪
        assert!(orch.live_continuation.as_ref().unwrap().sent_count() > 0);

        // 触发上下文衔接 (turn_count >= 1)
        // maybe_context_handoff 会在 start_new_conversation 后调用 reset_continuation
        orch.maybe_context_handoff().await.unwrap();

        // 验证 new_conversation 被调用
        assert!(chat.new_conversation_count() > 0);

        // 验证 live_continuation 被重置
        assert!(orch.live_continuation.as_ref().unwrap().is_reset());
        assert_eq!(orch.live_continuation.as_ref().unwrap().sent_count(), 0);
    }

    #[tokio::test]
    async fn test_incremental_stats_accessor() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec!["AI response"]);
        let orch = make_orchestrator(&chat, dir.path().to_str().unwrap());

        // 初始统计应为空
        assert_eq!(orch.incremental_stats().send_count, 0);
        assert_eq!(orch.incremental_stats().total_messages, 0);
    }

    #[tokio::test]
    async fn test_send_with_continuation_stats_accumulate() {
        // 多次发送后统计应正确累积
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec!["resp1", "resp2", "resp3"]);
        let mut orch =
            make_orchestrator(&chat, dir.path().to_str().unwrap()).with_live_continuation();

        // 第一次: 全量 (3条)
        orch.send_with_continuation(&["a".to_string(), "b".to_string(), "c".to_string()], 30)
            .await
            .unwrap();

        // 第二次: 增量 (只有 d 是新的)
        orch.send_with_continuation(
            &[
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
                "d".to_string(),
            ],
            30,
        )
        .await
        .unwrap();

        // 第三次: 全量 (新对话, 有 e/f)
        orch.send_with_continuation(&["e".to_string(), "f".to_string()], 30)
            .await
            .unwrap();

        // 累计统计
        let stats = orch.incremental_stats();
        assert_eq!(stats.send_count, 3);
        assert_eq!(stats.total_messages, 9); // 3 + 4 + 2
        assert_eq!(stats.sent_messages, 6); // 3 + 1 + 2
        assert_eq!(stats.skipped_messages, 3); // 0 + 3 + 0
    }

    #[tokio::test]
    async fn test_send_with_continuation_with_conversation_tracker() {
        // 启用 conversation_tracker (Radix Tree) + live_continuation
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec!["resp1", "resp2"]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_live_continuation()
            .with_conversation_tracker();

        let msgs1 = vec![
            "system".to_string(),
            "task1".to_string(),
            "result1".to_string(),
        ];
        orch.send_with_continuation(&msgs1, 30).await.unwrap();

        let msgs2 = vec![
            "system".to_string(),
            "task1".to_string(),
            "result1".to_string(),
            "task2".to_string(),
        ];
        let result = orch.send_with_continuation(&msgs2, 30).await.unwrap();

        assert_eq!(result.text, "resp2");
        // 增量应该只包含 task2
        let sent = chat.sent_messages();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1], "task2");

        // 统计
        assert_eq!(orch.incremental_stats().skipped_messages, 3);
    }

    #[tokio::test]
    async fn test_reset_continuation_manual() {
        // 手动调用 reset_continuation
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec!["resp1", "resp2"]);
        let mut orch =
            make_orchestrator(&chat, dir.path().to_str().unwrap()).with_live_continuation();

        let msgs = vec!["msg1".to_string(), "msg2".to_string()];
        orch.send_with_continuation(&msgs, 30).await.unwrap();
        assert!(orch.live_continuation.as_ref().unwrap().sent_count() > 0);

        // 手动重置
        orch.reset_continuation();
        assert!(orch.live_continuation.as_ref().unwrap().is_reset());
        assert_eq!(orch.live_continuation.as_ref().unwrap().sent_count(), 0);

        // 重置后再次发送应该是全量
        let result = orch.send_with_continuation(&msgs, 30).await.unwrap();
        assert_eq!(result.text, "resp2");
        // 两次 send_message 调用 (重置后全量发送)
        assert_eq!(chat.sent_messages().len(), 2);
    }

    // ===== Session 76: send_attempt_prompt + build_messages_from_memory 测试 =====

    #[tokio::test]
    async fn test_send_attempt_prompt_no_incremental() {
        // 未启用增量跟踪时, send_attempt_prompt 退化为 send_message_safe
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec!["AI response"]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap());

        let result = orch
            .send_attempt_prompt("test prompt", &None, false, 30)
            .await
            .unwrap();

        assert_eq!(result.text, "AI response");
        let sent = chat.sent_messages();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0], "test prompt");
    }

    #[tokio::test]
    async fn test_send_attempt_prompt_first_attempt() {
        // 首次尝试: 全量发送
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec!["AI response 1", "AI response 2"]);
        let mut orch =
            make_orchestrator(&chat, dir.path().to_str().unwrap()).with_live_continuation();

        // 首次尝试
        let result = orch
            .send_attempt_prompt("first prompt", &None, false, 30)
            .await
            .unwrap();

        assert_eq!(result.text, "AI response 1");
        let sent = chat.sent_messages();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0], "first prompt");
        // 统计: total=1, sent=1 (全量, 无增量优化)
        assert_eq!(orch.incremental_stats().total_messages, 1);
        assert_eq!(orch.incremental_stats().sent_messages, 1);
    }

    #[tokio::test]
    async fn test_send_attempt_prompt_fix_attempt() {
        // 修复轮次: first_prompt 被跳过, 只发送 fix_prompt
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec!["AI response 1", "AI response 2"]);
        let mut orch =
            make_orchestrator(&chat, dir.path().to_str().unwrap()).with_live_continuation();

        // 首次尝试
        orch.send_attempt_prompt("first prompt", &None, false, 30)
            .await
            .unwrap();

        // 修复轮次: first_prompt 已发送过 → 跳过, 只发送 fix_prompt
        let first_prompt = Some("first prompt".to_string());
        let result = orch
            .send_attempt_prompt("fix prompt", &first_prompt, true, 30)
            .await
            .unwrap();

        assert_eq!(result.text, "AI response 2");
        let sent = chat.sent_messages();
        assert_eq!(sent.len(), 2); // 两次 send_message
        assert_eq!(sent[1], "fix prompt"); // 第二次只发送了 fix_prompt

        // 统计: total=3 (1+2), sent=2 (1+1), skipped=1
        assert_eq!(orch.incremental_stats().total_messages, 3);
        assert_eq!(orch.incremental_stats().sent_messages, 2);
        assert_eq!(orch.incremental_stats().skipped_messages, 1);
    }

    #[tokio::test]
    async fn test_send_attempt_prompt_fix_without_first() {
        // 修复轮次但 first_prompt 为 None: 全量发送 (如上下文衔接后)
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec!["AI response"]);
        let mut orch =
            make_orchestrator(&chat, dir.path().to_str().unwrap()).with_live_continuation();

        let result = orch
            .send_attempt_prompt("fix prompt", &None, true, 30)
            .await
            .unwrap();

        assert_eq!(result.text, "AI response");
        let sent = chat.sent_messages();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0], "fix prompt");
    }

    #[tokio::test]
    async fn test_send_attempt_prompt_with_conversation_tracker() {
        // 启用 conversation_tracker (Radix Tree) + live_continuation
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec!["resp1", "resp2"]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_live_continuation()
            .with_conversation_tracker();

        // 首次
        orch.send_attempt_prompt("full prompt", &None, false, 30)
            .await
            .unwrap();

        // 修复: full prompt 已发送 → 跳过
        let first = Some("full prompt".to_string());
        let result = orch
            .send_attempt_prompt("fix prompt", &first, true, 30)
            .await
            .unwrap();

        assert_eq!(result.text, "resp2");
        let sent = chat.sent_messages();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1], "fix prompt");
        assert_eq!(orch.incremental_stats().skipped_messages, 1);
    }

    #[tokio::test]
    async fn test_send_attempt_prompt_multiple_fix_rounds() {
        // 多轮修复: 第一次修复跳过 first_prompt, 后续修复也跳过
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec!["resp1", "resp2", "resp3"]);
        let mut orch =
            make_orchestrator(&chat, dir.path().to_str().unwrap()).with_live_continuation();

        // 首次
        let first = "first prompt".to_string();
        orch.send_attempt_prompt(&first, &None, false, 30)
            .await
            .unwrap();

        // 修复1
        let first_opt = Some(first.clone());
        orch.send_attempt_prompt("fix1", &first_opt, true, 30)
            .await
            .unwrap();

        // 修复2: first_prompt 仍然会被跳过
        let result = orch
            .send_attempt_prompt("fix2", &first_opt, true, 30)
            .await
            .unwrap();

        assert_eq!(result.text, "resp3");
        // 3次 send_message 调用
        assert_eq!(chat.sent_messages().len(), 3);
        assert_eq!(chat.sent_messages()[1], "fix1");
        assert_eq!(chat.sent_messages()[2], "fix2");
    }

    #[test]
    fn test_build_messages_from_memory_empty() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let orch = make_orchestrator(&chat, dir.path().to_str().unwrap());

        // Memory 无对话
        let messages = orch.build_messages_from_memory(3);
        assert!(messages.is_empty());
    }

    #[test]
    fn test_build_messages_from_memory_with_conversations() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap());

        // 添加对话
        orch.memory.add_conversation("user", "hello", Some("0-0"));
        orch.memory
            .add_conversation("assistant", "hi there", Some("0-0"));
        orch.memory.add_conversation("user", "do task", Some("0-0"));

        // 提取最近 2 条
        let messages = orch.build_messages_from_memory(2);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0], "hi there");
        assert_eq!(messages[1], "do task");
    }

    #[test]
    fn test_build_messages_from_memory_count_zero() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap());

        orch.memory.add_conversation("user", "hello", Some("0-0"));

        let messages = orch.build_messages_from_memory(0);
        assert!(messages.is_empty());
    }

    #[test]
    fn test_build_messages_from_memory_more_than_available() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap());

        orch.memory.add_conversation("user", "hello", Some("0-0"));

        // 请求 10 条但只有 1 条
        let messages = orch.build_messages_from_memory(10);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0], "hello");
    }

    #[test]
    fn test_build_messages_from_memory_skips_empty_content() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap());

        orch.memory.add_conversation("user", "", Some("0-0"));
        orch.memory
            .add_conversation("assistant", "response", Some("0-0"));

        let messages = orch.build_messages_from_memory(2);
        // 空内容应被过滤
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0], "response");
    }

    // ===== Session 89: build_messages_from_memory 深度集成测试 =====

    // --- 纯函数 build_fix_messages_with_memory 测试 ---

    #[test]
    fn test_build_fix_messages_with_first_and_memory() {
        let msgs = build_fix_messages_with_memory(
            &Some("first".to_string()),
            "fix",
            &["hist1".to_string(), "hist2".to_string()],
        );
        assert_eq!(msgs, vec!["first", "hist1", "hist2", "fix"]);
    }

    #[test]
    fn test_build_fix_messages_no_first_with_memory() {
        let msgs = build_fix_messages_with_memory(
            &None,
            "fix",
            &["hist1".to_string(), "hist2".to_string()],
        );
        assert_eq!(msgs, vec!["hist1", "hist2", "fix"]);
    }

    #[test]
    fn test_build_fix_messages_with_first_no_memory() {
        let msgs = build_fix_messages_with_memory(&Some("first".to_string()), "fix", &[]);
        assert_eq!(msgs, vec!["first", "fix"]);
    }

    #[test]
    fn test_build_fix_messages_no_first_no_memory() {
        let msgs = build_fix_messages_with_memory(&None, "fix", &[]);
        assert_eq!(msgs, vec!["fix"]);
    }

    #[test]
    fn test_build_fix_messages_empty_fix_prompt() {
        let msgs =
            build_fix_messages_with_memory(&Some("first".to_string()), "", &["hist".to_string()]);
        assert_eq!(msgs, vec!["first", "hist", ""]);
    }

    #[test]
    fn test_build_fix_messages_order_preserved() {
        let memory = vec!["msg1".to_string(), "msg2".to_string(), "msg3".to_string()];
        let msgs = build_fix_messages_with_memory(&Some("first".to_string()), "fix", &memory);
        // 顺序: first, msg1, msg2, msg3, fix
        assert_eq!(msgs.len(), 5);
        assert_eq!(msgs[0], "first");
        assert_eq!(msgs[1], "msg1");
        assert_eq!(msgs[2], "msg2");
        assert_eq!(msgs[3], "msg3");
        assert_eq!(msgs[4], "fix");
    }

    #[test]
    fn test_build_fix_messages_large_memory() {
        let memory: Vec<String> = (0..20).map(|i| format!("msg{}", i)).collect();
        let msgs = build_fix_messages_with_memory(&Some("first".to_string()), "fix", &memory);
        assert_eq!(msgs.len(), 22); // 1 (first) + 20 (memory) + 1 (fix)
        assert_eq!(msgs[0], "first");
        assert_eq!(msgs[21], "fix");
    }

    // --- MemoryContextStats 测试 ---

    #[test]
    fn test_memory_context_stats_new() {
        let stats = MemoryContextStats::new();
        assert_eq!(stats.injection_count, 0);
        assert_eq!(stats.total_messages_injected, 0);
        assert_eq!(stats.total_messages_skipped, 0);
        assert!(!stats.has_data());
        assert!((stats.avg_messages_per_injection() - 0.0).abs() < 0.001);
        assert!((stats.skip_rate() - 0.0).abs() < 0.001);
        assert!(stats.to_summary().is_empty());
    }

    #[test]
    fn test_memory_context_stats_record_single_injection() {
        let mut stats = MemoryContextStats::new();
        stats.record_injection(3, 2);
        assert_eq!(stats.injection_count, 1);
        assert_eq!(stats.total_messages_injected, 3);
        assert_eq!(stats.total_messages_skipped, 2);
        assert!(stats.has_data());
    }

    #[test]
    fn test_memory_context_stats_multiple_injections() {
        let mut stats = MemoryContextStats::new();
        stats.record_injection(3, 2);
        stats.record_injection(5, 4);
        stats.record_injection(2, 1);
        assert_eq!(stats.injection_count, 3);
        assert_eq!(stats.total_messages_injected, 10);
        assert_eq!(stats.total_messages_skipped, 7);
    }

    #[test]
    fn test_memory_context_stats_avg_messages() {
        let mut stats = MemoryContextStats::new();
        stats.record_injection(3, 0);
        stats.record_injection(5, 0);
        // avg = (3 + 5) / 2 = 4.0
        assert!((stats.avg_messages_per_injection() - 4.0).abs() < 0.001);
    }

    #[test]
    fn test_memory_context_stats_skip_rate() {
        let mut stats = MemoryContextStats::new();
        stats.record_injection(4, 3);
        // skip_rate = 3/4 = 0.75
        assert!((stats.skip_rate() - 0.75).abs() < 0.001);
    }

    #[test]
    fn test_memory_context_stats_skip_rate_zero_injected() {
        let stats = MemoryContextStats::new();
        assert!((stats.skip_rate() - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_memory_context_stats_to_summary() {
        let mut stats = MemoryContextStats::new();
        stats.record_injection(4, 3);
        let summary = stats.to_summary();
        assert!(summary.contains("注入次数: 1"));
        assert!(summary.contains("总消息: 4"));
        assert!(summary.contains("跳过: 3"));
        assert!(summary.contains("跳过率: 75.0%"));
    }

    #[test]
    fn test_memory_context_stats_to_summary_empty() {
        let stats = MemoryContextStats::new();
        assert!(stats.to_summary().is_empty());
    }

    #[test]
    fn test_memory_context_stats_serde_roundtrip() {
        let mut stats = MemoryContextStats::new();
        stats.record_injection(5, 3);
        stats.record_injection(2, 1);
        let json = serde_json::to_string(&stats).unwrap();
        let loaded: MemoryContextStats = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.injection_count, 2);
        assert_eq!(loaded.total_messages_injected, 7);
        assert_eq!(loaded.total_messages_skipped, 4);
    }

    #[test]
    fn test_memory_context_stats_default() {
        let stats = MemoryContextStats::default();
        assert_eq!(stats.injection_count, 0);
        assert_eq!(stats.total_messages_injected, 0);
        assert_eq!(stats.total_messages_skipped, 0);
    }

    // --- with_memory_context builder 测试 ---

    #[test]
    fn test_with_memory_context_sets_field() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let orch = make_orchestrator(&chat, dir.path().to_str().unwrap());

        // 默认 memory_context_count 为 0
        assert_eq!(orch.memory_context_count, 0);

        // 启用后应为指定值
        let orch = orch.with_memory_context(3);
        assert_eq!(orch.memory_context_count, 3);
    }

    #[test]
    fn test_with_memory_context_zero_disables() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_memory_context(5)
            .with_memory_context(0);
        assert_eq!(orch.memory_context_count, 0);
    }

    // --- send_attempt_prompt 集成测试 (with memory context) ---

    #[tokio::test]
    async fn test_send_attempt_prompt_with_memory_context_fix() {
        // 修复轮次启用 memory context: [first, memory..., fix]
        // 注意: memory 对话是直接添加到 memory.conversations 的,
        // 未通过 send_with_continuation 发送, 因此 LiveContinuation 不会跳过它们。
        // first_prompt 在首次尝试时已发送 → 被 LiveContinuation 跳过。
        // memory 消息 + fix_prompt 作为增量发送。
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec!["resp1", "resp2"]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_live_continuation()
            .with_memory_context(2);

        // 添加对话历史 (直接添加到 memory, 未通过 chat 发送)
        orch.memory.add_conversation("user", "hello", Some("0-0"));
        orch.memory
            .add_conversation("assistant", "hi there", Some("0-0"));
        orch.memory.add_conversation("user", "do task", Some("0-0"));

        // 首次尝试
        orch.send_attempt_prompt("first prompt", &None, false, 30)
            .await
            .unwrap();

        // 修复轮次: 注入 memory context
        // 消息列表: [first_prompt, "hi there", "do task", "fix prompt"]
        // LiveContinuation 跳过 first_prompt → 增量: ["hi there", "do task", "fix prompt"]
        let first = Some("first prompt".to_string());
        let result = orch
            .send_attempt_prompt("fix prompt", &first, true, 30)
            .await
            .unwrap();

        assert_eq!(result.text, "resp2");
        let sent = chat.sent_messages();
        // 第一次: "first prompt" (全量)
        // 第二次: 增量发送, first_prompt 被跳过, memory + fix 被发送
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0], "first prompt");
        // memory 消息 + fix prompt 拼接 (用 \n\n 分隔)
        assert!(sent[1].contains("hi there"));
        assert!(sent[1].contains("do task"));
        assert!(sent[1].contains("fix prompt"));
    }

    #[tokio::test]
    async fn test_send_attempt_prompt_memory_context_stats_tracked() {
        // 验证 memory_context_stats 被正确更新
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec!["resp1", "resp2"]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_live_continuation()
            .with_memory_context(3);

        // 添加对话历史
        orch.memory.add_conversation("user", "hello", Some("0-0"));
        orch.memory.add_conversation("assistant", "hi", Some("0-0"));

        // 首次尝试 (不注入)
        orch.send_attempt_prompt("first", &None, false, 30)
            .await
            .unwrap();
        assert!(!orch.memory_context_stats.has_data());

        // 修复轮次 (注入)
        let first = Some("first".to_string());
        orch.send_attempt_prompt("fix", &first, true, 30)
            .await
            .unwrap();

        // 验证统计
        assert!(orch.memory_context_stats.has_data());
        assert_eq!(orch.memory_context_stats.injection_count, 1);
        assert_eq!(orch.memory_context_stats.total_messages_injected, 2);
    }

    #[tokio::test]
    async fn test_send_attempt_prompt_memory_context_disabled() {
        // memory_context_count=0 时不注入, 行为与之前一致
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec!["resp1", "resp2"]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_live_continuation()
            .with_memory_context(0); // 禁用

        // 添加对话历史 (不应被注入)
        orch.memory.add_conversation("user", "hello", Some("0-0"));

        // 首次尝试
        orch.send_attempt_prompt("first", &None, false, 30)
            .await
            .unwrap();

        // 修复轮次
        let first = Some("first".to_string());
        orch.send_attempt_prompt("fix", &first, true, 30)
            .await
            .unwrap();

        // 统计不应有数据
        assert!(!orch.memory_context_stats.has_data());
    }

    #[tokio::test]
    async fn test_send_attempt_prompt_memory_context_empty_conversations() {
        // Memory 无对话时不注入
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec!["resp1", "resp2"]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_live_continuation()
            .with_memory_context(3);

        // 不添加任何对话历史

        // 首次尝试
        orch.send_attempt_prompt("first", &None, false, 30)
            .await
            .unwrap();

        // 修复轮次 (无对话可注入)
        let first = Some("first".to_string());
        orch.send_attempt_prompt("fix", &first, true, 30)
            .await
            .unwrap();

        // 统计不应有数据 (无对话被注入)
        assert!(!orch.memory_context_stats.has_data());
    }

    #[tokio::test]
    async fn test_send_attempt_prompt_memory_context_first_attempt_no_inject() {
        // 首次尝试不注入 memory context
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec!["resp"]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_live_continuation()
            .with_memory_context(3);

        // 添加对话历史
        orch.memory.add_conversation("user", "hello", Some("0-0"));

        // 首次尝试 (不应注入)
        orch.send_attempt_prompt("first", &None, false, 30)
            .await
            .unwrap();

        // 统计不应有数据
        assert!(!orch.memory_context_stats.has_data());
    }

    #[tokio::test]
    async fn test_send_attempt_prompt_memory_context_no_first_prompt() {
        // 修复轮次但 first_prompt=None (如上下文衔接后): [memory..., fix]
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec!["resp"]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_live_continuation()
            .with_memory_context(2);

        // 添加对话历史
        orch.memory.add_conversation("user", "hello", Some("0-0"));
        orch.memory.add_conversation("assistant", "hi", Some("0-0"));

        // 修复轮次, first_prompt=None
        let result = orch
            .send_attempt_prompt("fix", &None, true, 30)
            .await
            .unwrap();

        assert_eq!(result.text, "resp");
        // 统计应记录注入
        assert!(orch.memory_context_stats.has_data());
        assert_eq!(orch.memory_context_stats.total_messages_injected, 2);
    }

    #[tokio::test]
    async fn test_send_attempt_prompt_memory_context_multiple_fix_rounds() {
        // 多轮修复: 每次修复都注入 memory context
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec!["r1", "r2", "r3"]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_live_continuation()
            .with_memory_context(2);

        // 添加对话历史
        orch.memory.add_conversation("user", "msg1", Some("0-0"));
        orch.memory
            .add_conversation("assistant", "resp1", Some("0-0"));

        // 首次尝试
        let first = "first".to_string();
        orch.send_attempt_prompt(&first, &None, false, 30)
            .await
            .unwrap();

        // 修复1
        let first_opt = Some(first.clone());
        orch.send_attempt_prompt("fix1", &first_opt, true, 30)
            .await
            .unwrap();

        // 修复2
        orch.send_attempt_prompt("fix2", &first_opt, true, 30)
            .await
            .unwrap();

        // 两次注入
        assert_eq!(orch.memory_context_stats.injection_count, 2);
        assert_eq!(orch.memory_context_stats.total_messages_injected, 4); // 2 * 2
    }

    #[tokio::test]
    async fn test_send_attempt_prompt_memory_context_with_conversation_tracker() {
        // 同时启用 conversation_tracker + memory context
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec!["r1", "r2"]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_live_continuation()
            .with_conversation_tracker()
            .with_memory_context(2);

        // 添加对话历史
        orch.memory.add_conversation("user", "msg1", Some("0-0"));
        orch.memory
            .add_conversation("assistant", "resp1", Some("0-0"));

        // 首次
        orch.send_attempt_prompt("first", &None, false, 30)
            .await
            .unwrap();

        // 修复: memory context + Radix Tree 增量
        let first = Some("first".to_string());
        let result = orch
            .send_attempt_prompt("fix", &first, true, 30)
            .await
            .unwrap();

        assert_eq!(result.text, "r2");
        assert!(orch.memory_context_stats.has_data());
    }

    #[tokio::test]
    async fn test_send_attempt_prompt_memory_context_no_incremental() {
        // 未启用增量跟踪时, memory context 不生效
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec!["resp"]);
        let mut orch =
            make_orchestrator(&chat, dir.path().to_str().unwrap()).with_memory_context(3); // 无 live_continuation/conversation_tracker

        // 添加对话历史
        orch.memory.add_conversation("user", "hello", Some("0-0"));

        // 修复轮次
        let result = orch
            .send_attempt_prompt("fix", &None, true, 30)
            .await
            .unwrap();

        assert_eq!(result.text, "resp");
        // memory context 在非增量模式下不注入
        assert!(!orch.memory_context_stats.has_data());
    }

    #[test]
    fn test_final_report_memory_context_stats() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);

        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_live_continuation()
            .with_memory_context(3)
            .with_dev_trace(true);
        setup_test_phase(&mut orch);

        // 手动注入统计
        orch.memory_context_stats.record_injection(3, 2);
        orch.memory_context_stats.record_injection(2, 1);

        orch.final_report().unwrap();

        // 报告应包含 memory context 统计
        // (验证不崩溃即可, 因为输出到 stdout)
    }

    #[test]
    fn test_final_report_no_memory_context_stats_when_empty() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap()).with_dev_trace(true);
        setup_test_phase(&mut orch);

        // 无 memory context 统计
        assert!(!orch.memory_context_stats.has_data());

        // 不应崩溃
        orch.final_report().unwrap();
    }

    // ===== Session 82: CacheTuner 集成测试 =====

    use crate::dev_trace::DevTraceEntry;
    use crate::memory::{Phase, PhaseStatus, Task, TaskStatus};

    /// 创建带 DevTrace + CacheTuner 的 Orchestrator
    fn make_orchestrator_with_tuning<'a>(
        chat: &'a MockChatClient,
        workspace_dir: &'a str,
    ) -> Orchestrator<'a, MockChatClient, MockTestRunner, MockExtractor> {
        std::fs::create_dir_all(format!("{}/.forge", workspace_dir)).unwrap();
        make_orchestrator(chat, workspace_dir)
            .with_dev_trace(true)
            .with_cache_tuner(CacheTuner::with_default_config(1800))
    }

    /// 在 memory 中添加测试 phase + task
    fn setup_test_phase(
        orch: &mut Orchestrator<'_, MockChatClient, MockTestRunner, MockExtractor>,
    ) {
        orch.memory.phases.push(Phase {
            id: 0,
            name: "test phase".to_string(),
            description: "test".to_string(),
            status: PhaseStatus::InProgress,
            tasks: vec![Task {
                id: "0-0".to_string(),
                phase_id: 0,
                name: "test task".to_string(),
                prompt: "test".to_string(),
                status: TaskStatus::InProgress,
                result: None,
                attempts: 1,
                files_written: vec![],
                test_result: None,
                last_good_snapshot: None,
                clarifications: vec![],
                depends_on: vec![],
            }],
        });
    }

    /// 写入 DevTrace 条目
    fn write_trace_entries(
        orch: &Orchestrator<'_, MockChatClient, MockTestRunner, MockExtractor>,
        entries: &[DevTraceEntry],
    ) {
        if let Some(ref writer) = orch.dev_trace {
            for entry in entries {
                writer.write_entry(entry).unwrap();
            }
        }
    }

    #[test]
    fn test_with_cache_tuner_sets_field() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let orch = make_orchestrator(&chat, dir.path().to_str().unwrap());

        // 默认 cache_tuner 为 None
        assert!(orch.cache_tuner.is_none());

        // 启用后应为 Some
        let orch = orch.with_cache_tuner(CacheTuner::with_default_config(1800));
        assert!(orch.cache_tuner.is_some());
        assert!(orch.cache_tuner.as_ref().unwrap().is_enabled());
        assert_eq!(orch.cache_tuner.as_ref().unwrap().current_ttl(), 1800);
    }

    #[test]
    fn test_evaluate_cache_tuning_noop_without_tuner() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap()).with_dev_trace(true);
        setup_test_phase(&mut orch);

        // 没有 cache_tuner, 应该是无操作
        let ttl_before = orch.search_cache.ttl_secs();
        orch.evaluate_cache_tuning(0, 0);
        assert_eq!(orch.search_cache.ttl_secs(), ttl_before);
    }

    #[test]
    fn test_evaluate_cache_tuning_noop_without_devtrace() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_cache_tuner(CacheTuner::with_default_config(1800));
        setup_test_phase(&mut orch);

        // 没有 dev_trace, 应该是无操作
        let ttl_before = orch.search_cache.ttl_secs();
        orch.evaluate_cache_tuning(0, 0);
        assert_eq!(orch.search_cache.ttl_secs(), ttl_before);
    }

    #[test]
    fn test_evaluate_cache_tuning_noop_with_empty_entries() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator_with_tuning(&chat, dir.path().to_str().unwrap());
        setup_test_phase(&mut orch);

        // DevTrace 为空, 应该无操作
        let ttl_before = orch.search_cache.ttl_secs();
        orch.evaluate_cache_tuning(0, 0);
        assert_eq!(orch.search_cache.ttl_secs(), ttl_before);
    }

    #[test]
    fn test_evaluate_cache_tuning_keep_current_insufficient_data() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator_with_tuning(&chat, dir.path().to_str().unwrap());
        setup_test_phase(&mut orch);

        // 写入少量数据 (不足 min_samples=3)
        write_trace_entries(
            &orch,
            &[
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
            ],
        );

        let ttl_before = orch.search_cache.ttl_secs();
        orch.evaluate_cache_tuning(0, 0);

        // 数据不足, 应保持当前
        assert_eq!(orch.search_cache.ttl_secs(), ttl_before);
        assert!(orch.cache_tuner.as_ref().unwrap().is_enabled());
    }

    #[test]
    fn test_evaluate_cache_tuning_disables_harmful_cache() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator_with_tuning(&chat, dir.path().to_str().unwrap());
        setup_test_phase(&mut orch);

        // 构建数据: 缓存命中后修复率低 (0/3), 未命中后修复率高 (3/3)
        // diff = 0% - 100% = -100% < -15% → 禁用缓存
        let entries: Vec<DevTraceEntry> = vec![
            // 3次缓存命中 → 全部编译失败
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(0),
                Some("task"),
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
                Some("task"),
                "check",
                "failed",
                50,
                false,
                Some("error"),
            ),
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(1),
                Some("task2"),
                "q2",
                "r2",
                0,
                true,
                Some("缓存命中 (key=E0277, 原始耗时=500ms, 命中次数=1)"),
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
                Some("error"),
            ),
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(2),
                Some("task3"),
                "q3",
                "r3",
                0,
                true,
                Some("缓存命中 (key=E0507, 原始耗时=500ms, 命中次数=1)"),
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
                Some("error"),
            ),
            // 3次缓存未命中 → 全部编译通过
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(3),
                Some("task4"),
                "q4",
                "r4",
                500,
                true,
                Some("编译错误自动搜索"),
            ),
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
                TraceAction::WebSearch,
                Some(0),
                Some(4),
                Some("task5"),
                "q5",
                "r5",
                600,
                true,
                Some("编译错误自动搜索"),
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(4),
                Some("task5"),
                "check",
                "passed",
                50,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(5),
                Some("task6"),
                "q6",
                "r6",
                700,
                true,
                Some("编译错误自动搜索"),
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(5),
                Some("task6"),
                "check",
                "passed",
                50,
                true,
                None,
            ),
        ];
        write_trace_entries(&orch, &entries);

        orch.evaluate_cache_tuning(0, 0);

        // 缓存应被禁用
        assert!(!orch.cache_tuner.as_ref().unwrap().is_enabled());
    }

    #[test]
    fn test_evaluate_cache_tuning_increases_ttl_for_effective_cache() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator_with_tuning(&chat, dir.path().to_str().unwrap());
        setup_test_phase(&mut orch);

        // 构建数据: 缓存命中后修复率高 (3/3), 未命中后修复率低 (1/3)
        // diff = 100% - 33% = +67% > 5% → 延长 TTL
        let entries: Vec<DevTraceEntry> = vec![
            // 3次缓存命中 → 全部编译通过
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(0),
                Some("task"),
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
                "q2",
                "r2",
                0,
                true,
                Some("缓存命中 (key=E0277, 原始耗时=500ms, 命中次数=1)"),
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
                0,
                true,
                Some("缓存命中 (key=E0507, 原始耗时=500ms, 命中次数=1)"),
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(2),
                Some("task3"),
                "check",
                "passed",
                50,
                true,
                None,
            ),
            // 3次缓存未命中 → 1次通过 2次失败
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(3),
                Some("task4"),
                "q4",
                "r4",
                500,
                true,
                Some("编译错误自动搜索"),
            ),
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
                TraceAction::WebSearch,
                Some(0),
                Some(4),
                Some("task5"),
                "q5",
                "r5",
                600,
                true,
                Some("编译错误自动搜索"),
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
                Some("error"),
            ),
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(5),
                Some("task6"),
                "q6",
                "r6",
                700,
                true,
                Some("编译错误自动搜索"),
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
                Some("error"),
            ),
        ];
        write_trace_entries(&orch, &entries);

        let ttl_before = orch.search_cache.ttl_secs();
        orch.evaluate_cache_tuning(0, 0);

        // TTL 应延长 (1800 * 1.5 = 2700)
        assert!(orch.search_cache.ttl_secs() > ttl_before);
        assert_eq!(orch.search_cache.ttl_secs(), 2700);
        assert!(orch.cache_tuner.as_ref().unwrap().is_enabled());
    }

    #[test]
    fn test_evaluate_cache_tuning_reduces_ttl_for_slightly_harmful_cache() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator_with_tuning(&chat, dir.path().to_str().unwrap());
        setup_test_phase(&mut orch);

        // 构建数据: 缓存命中后修复率低 (1/3=33%), 未命中后修复率高 (2/3=67%)
        // diff = 33% - 67% = -33% < -5% → 缩短 TTL
        // 但 -33% < -15% → 实际应该禁用, 让我调整为略差
        // diff = -10% → 缩短 TTL (在 -15% 和 -5% 之间)
        // hit: 2/3=67%, miss: 3/3=100%, diff = -33% → 这会禁用
        // 调整: hit: 2/3=67%, miss: 2/3=67% → diff=0 → 保持
        // 调整: hit: 2/4=50%, miss: 3/4=75% → diff=-25% → 禁用
        // 需要 diff 在 [-15%, -5%) 之间
        // hit: 2/3=67%, miss: 3/4=75% → diff=-8% → 缩短 TTL
        let entries: Vec<DevTraceEntry> = vec![
            // 3次缓存命中 → 2次通过 1次失败 (67%)
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(0),
                Some("task"),
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
                "q2",
                "r2",
                0,
                true,
                Some("缓存命中 (key=E0277, 原始耗时=500ms, 命中次数=1)"),
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
                0,
                true,
                Some("缓存命中 (key=E0507, 原始耗时=500ms, 命中次数=1)"),
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
                Some("error"),
            ),
            // 4次缓存未命中 → 3次通过 1次失败 (75%)
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(3),
                Some("task4"),
                "q4",
                "r4",
                500,
                true,
                Some("编译错误自动搜索"),
            ),
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
                TraceAction::WebSearch,
                Some(0),
                Some(4),
                Some("task5"),
                "q5",
                "r5",
                600,
                true,
                Some("编译错误自动搜索"),
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(4),
                Some("task5"),
                "check",
                "passed",
                50,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(5),
                Some("task6"),
                "q6",
                "r6",
                700,
                true,
                Some("编译错误自动搜索"),
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(5),
                Some("task6"),
                "check",
                "passed",
                50,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(6),
                Some("task7"),
                "q7",
                "r7",
                800,
                true,
                Some("编译错误自动搜索"),
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(6),
                Some("task7"),
                "check",
                "failed",
                50,
                false,
                Some("error"),
            ),
        ];
        write_trace_entries(&orch, &entries);

        let ttl_before = orch.search_cache.ttl_secs();
        orch.evaluate_cache_tuning(0, 0);

        // diff = 67% - 75% = -8% → 在 [-15%, -5%) 之间 → 缩短 TTL
        // 1800 * 0.5 = 900
        assert!(orch.search_cache.ttl_secs() < ttl_before);
        assert_eq!(orch.search_cache.ttl_secs(), 900);
        assert!(orch.cache_tuner.as_ref().unwrap().is_enabled());
    }

    #[test]
    fn test_evaluate_cache_tuning_writes_devtrace_entry() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator_with_tuning(&chat, dir.path().to_str().unwrap());
        setup_test_phase(&mut orch);

        // 写入一些数据
        write_trace_entries(
            &orch,
            &[
                DevTraceEntry::new(
                    TraceAction::WebSearch,
                    Some(0),
                    Some(0),
                    Some("task"),
                    "q",
                    "r",
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
            ],
        );

        // 调用 evaluate_cache_tuning
        orch.evaluate_cache_tuning(0, 0);

        // 验证 DevTrace 中新增了 CacheTuning 条目
        let entries = orch.dev_trace.as_ref().unwrap().read_all().unwrap();
        let tuning_entries: Vec<_> = entries
            .iter()
            .filter(|e| e.action == TraceAction::CacheTuning)
            .collect();
        assert_eq!(tuning_entries.len(), 1);
        assert!(tuning_entries[0].success);
    }

    #[test]
    fn test_cache_disabled_after_tuning_skips_cache_in_search() {
        // 验证当 cache_tuner 禁用缓存后, auto_search_error_solutions 不使用缓存
        // 这里间接测试: 禁用后 search_cache 中不应有新条目插入
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator_with_tuning(&chat, dir.path().to_str().unwrap());
        setup_test_phase(&mut orch);

        // 手动禁用缓存
        if let Some(ref mut tuner) = orch.cache_tuner {
            let mut corr = crate::dev_trace::CacheFixCorrelation::new();
            // 填入足够的负面数据来禁用
            corr.record_hit_check(false);
            corr.record_hit_check(false);
            corr.record_hit_check(false);
            corr.record_miss_check(true);
            corr.record_miss_check(true);
            corr.record_miss_check(true);
            let stats = orch.search_cache.stats().clone();
            tuner.evaluate_and_apply(&corr, &stats);
        }

        // 验证缓存已被禁用
        assert!(!orch.cache_tuner.as_ref().unwrap().is_enabled());

        // 验证 cache_enabled 逻辑: 当 tuner 禁用时, cache_key 应为 None
        // (这里间接验证: search_cache 不应被使用)
        let cache_enabled = orch.cache_tuner.as_ref().is_none_or(|t| t.is_enabled());
        assert!(!cache_enabled);
    }

    #[test]
    fn test_with_clarification_preserves_cache_tuner() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_cache_tuner(CacheTuner::with_default_config(1800));

        // with_clarification 应保留 cache_tuner
        let orch2 = orch.with_clarification(HeuristicClarificationChecker::new());
        assert!(orch2.cache_tuner.is_some());
        assert_eq!(orch2.cache_tuner.as_ref().unwrap().current_ttl(), 1800);
    }

    // ===== Session 84: CacheTuner 持久化集成测试 =====

    #[test]
    fn test_final_report_saves_cache_tuning_history() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator_with_tuning(&chat, dir.path().to_str().unwrap());
        setup_test_phase(&mut orch);

        // 调用 final_report
        orch.final_report().unwrap();

        // 验证历史文件已创建
        let history_path = dir.path().join(".forge").join("cache_tuning_history.json");
        assert!(history_path.exists(), "缓存调优历史文件应存在");

        // 验证可以加载
        let loaded = crate::cache_tuning::CacheTuningHistory::load(&history_path).unwrap();
        assert_eq!(loaded.initial_ttl, 1800);
        assert_eq!(loaded.current_ttl, 1800);
        assert!(loaded.saved_at.is_some()); // final_report 保存时添加时间戳
    }

    #[test]
    fn test_final_report_saves_adjusted_ttl() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator_with_tuning(&chat, dir.path().to_str().unwrap());
        setup_test_phase(&mut orch);

        // 手动调整 TTL
        if let Some(ref mut tuner) = orch.cache_tuner {
            let decision =
                crate::cache_tuning::CacheTuningDecision::adjust_ttl(1800, 2700, 0.33, "测试延长");
            tuner.apply_decision(&decision);
        }

        orch.final_report().unwrap();

        // 验证保存的 TTL
        let history_path = dir.path().join(".forge").join("cache_tuning_history.json");
        let loaded = crate::cache_tuning::CacheTuningHistory::load(&history_path).unwrap();
        assert_eq!(loaded.initial_ttl, 1800);
        assert_eq!(loaded.current_ttl, 2700);
        assert_eq!(loaded.adjustment_count, 1);
        assert_eq!(loaded.decisions.len(), 1);
    }

    #[test]
    fn test_final_report_saves_disabled_state() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator_with_tuning(&chat, dir.path().to_str().unwrap());
        setup_test_phase(&mut orch);

        // 手动禁用缓存
        if let Some(ref mut tuner) = orch.cache_tuner {
            let decision =
                crate::cache_tuning::CacheTuningDecision::disable_cache(1800, -0.2, "有害");
            tuner.apply_decision(&decision);
        }

        orch.final_report().unwrap();

        // 验证保存的禁用状态
        let history_path = dir.path().join(".forge").join("cache_tuning_history.json");
        let loaded = crate::cache_tuning::CacheTuningHistory::load(&history_path).unwrap();
        assert!(!loaded.enabled);
        assert_eq!(loaded.disable_count, 1);
    }

    #[test]
    fn test_final_report_no_save_without_tuner() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap()).with_dev_trace(true);
        setup_test_phase(&mut orch);

        // 没有 cache_tuner
        assert!(orch.cache_tuner.is_none());

        orch.final_report().unwrap();

        // 不应创建历史文件
        let history_path = dir.path().join(".forge").join("cache_tuning_history.json");
        assert!(!history_path.exists());
    }

    #[test]
    fn test_final_report_empty_tuner_no_decisions() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator_with_tuning(&chat, dir.path().to_str().unwrap());
        setup_test_phase(&mut orch);

        // tuner 存在但没有决策
        assert!(orch.cache_tuner.as_ref().unwrap().decisions().is_empty());

        orch.final_report().unwrap();

        // 文件应存在 (即使没有决策, 也保存状态)
        let history_path = dir.path().join(".forge").join("cache_tuning_history.json");
        assert!(history_path.exists());

        let loaded = crate::cache_tuning::CacheTuningHistory::load(&history_path).unwrap();
        assert!(loaded.is_empty()); // 无决策
        assert_eq!(loaded.current_ttl, 1800); // TTL 保持默认
    }

    #[test]
    fn test_load_history_restores_ttl_in_orchestrator() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();

        // 先保存一个历史 (TTL=2700)
        let history = crate::cache_tuning::CacheTuningHistory {
            initial_ttl: 1800,
            current_ttl: 2700,
            enabled: true,
            adjustment_count: 1,
            disable_count: 0,
            decisions: vec![crate::cache_tuning::CacheTuningDecision::adjust_ttl(
                1800, 2700, 0.33, "延长",
            )],
            saved_at: None,
        };
        history.save_to_workspace(dir.path()).unwrap();

        // 创建 orchestrator (默认 TTL=1800)
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator_with_tuning(&chat, dir.path().to_str().unwrap());

        // 模拟 run() 中的加载逻辑
        if let Some(ref tuner) = orch.cache_tuner {
            let config = tuner.config().clone();
            let default_ttl = tuner.current_ttl();
            if let Some(loaded_tuner) =
                CacheTuner::load_from_workspace(&orch.workspace.root, config, default_ttl)
            {
                let new_ttl = loaded_tuner.current_ttl();
                let enabled = loaded_tuner.is_enabled();
                orch.cache_tuner = Some(loaded_tuner);
                orch.search_cache.set_ttl(new_ttl);
                if !enabled {
                    orch.search_cache.clear();
                }
            }
        }

        // 验证从历史恢复
        assert_eq!(orch.cache_tuner.as_ref().unwrap().current_ttl(), 2700);
        assert_eq!(orch.cache_tuner.as_ref().unwrap().initial_ttl(), 2700);
        assert!(orch.cache_tuner.as_ref().unwrap().is_enabled());
        // search_cache 的 TTL 也应同步
        assert_eq!(orch.search_cache.ttl_secs(), 2700);
    }

    #[test]
    fn test_load_history_restores_disabled_in_orchestrator() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();

        // 保存一个禁用状态的历史
        let history = crate::cache_tuning::CacheTuningHistory {
            initial_ttl: 1800,
            current_ttl: 1800,
            enabled: false,
            adjustment_count: 0,
            disable_count: 1,
            decisions: vec![crate::cache_tuning::CacheTuningDecision::disable_cache(
                1800, -0.2, "有害",
            )],
            saved_at: None,
        };
        history.save_to_workspace(dir.path()).unwrap();

        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator_with_tuning(&chat, dir.path().to_str().unwrap());

        // 模拟 run() 中的加载逻辑
        if let Some(ref tuner) = orch.cache_tuner {
            let config = tuner.config().clone();
            let default_ttl = tuner.current_ttl();
            if let Some(loaded_tuner) =
                CacheTuner::load_from_workspace(&orch.workspace.root, config, default_ttl)
            {
                let new_ttl = loaded_tuner.current_ttl();
                let enabled = loaded_tuner.is_enabled();
                orch.cache_tuner = Some(loaded_tuner);
                orch.search_cache.set_ttl(new_ttl);
                if !enabled {
                    orch.search_cache.clear();
                }
            }
        }

        // 验证禁用状态被恢复
        assert!(!orch.cache_tuner.as_ref().unwrap().is_enabled());
        assert_eq!(orch.cache_tuner.as_ref().unwrap().current_ttl(), 1800);
    }

    #[test]
    fn test_load_history_preserves_config_in_orchestrator() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();

        // 保存一个空历史
        let history = crate::cache_tuning::CacheTuningHistory::new();
        history.save_to_workspace(dir.path()).unwrap();

        // 用 aggressive 配置创建 orchestrator
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap()).with_cache_tuner(
            CacheTuner::new(crate::cache_tuning::CacheTuningConfig::aggressive(), 1800),
        );

        // 模拟加载
        if let Some(ref tuner) = orch.cache_tuner {
            let config = tuner.config().clone();
            let default_ttl = tuner.current_ttl();
            if let Some(loaded_tuner) =
                CacheTuner::load_from_workspace(&orch.workspace.root, config, default_ttl)
            {
                orch.cache_tuner = Some(loaded_tuner);
            }
        }

        // 验证配置被保留 (aggressive 的 min_samples=2)
        let tuner = orch.cache_tuner.as_ref().unwrap();
        assert_eq!(tuner.config().min_samples, 2);
        assert_eq!(tuner.config().disable_threshold, -0.05);
    }

    #[test]
    fn test_save_load_roundtrip_via_orchestrator() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator_with_tuning(&chat, dir.path().to_str().unwrap());
        setup_test_phase(&mut orch);

        // 调整 TTL (模拟调优过程)
        if let Some(ref mut tuner) = orch.cache_tuner {
            let decision =
                crate::cache_tuning::CacheTuningDecision::adjust_ttl(1800, 2700, 0.33, "延长");
            tuner.apply_decision(&decision);
        }

        // 保存 (final_report)
        orch.final_report().unwrap();

        // 创建新 orchestrator (模拟新 session)
        let mut orch2 = make_orchestrator_with_tuning(&chat, dir.path().to_str().unwrap());

        // 加载历史
        if let Some(ref tuner) = orch2.cache_tuner {
            let config = tuner.config().clone();
            let default_ttl = tuner.current_ttl();
            if let Some(loaded_tuner) =
                CacheTuner::load_from_workspace(&orch2.workspace.root, config, default_ttl)
            {
                let new_ttl = loaded_tuner.current_ttl();
                let enabled = loaded_tuner.is_enabled();
                orch2.cache_tuner = Some(loaded_tuner);
                orch2.search_cache.set_ttl(new_ttl);
                if !enabled {
                    orch2.search_cache.clear();
                }
            }
        }

        // 验证状态恢复
        assert_eq!(orch2.cache_tuner.as_ref().unwrap().current_ttl(), 2700);
        assert_eq!(orch2.cache_tuner.as_ref().unwrap().initial_ttl(), 2700);
        assert!(orch2.cache_tuner.as_ref().unwrap().is_enabled());
        assert_eq!(orch2.search_cache.ttl_secs(), 2700);

        // 新 session 继续调优
        if let Some(ref mut tuner) = orch2.cache_tuner {
            let decision =
                crate::cache_tuning::CacheTuningDecision::adjust_ttl(2700, 4050, 0.33, "再次延长");
            tuner.apply_decision(&decision);
        }

        assert_eq!(orch2.cache_tuner.as_ref().unwrap().current_ttl(), 4050);
        assert_eq!(orch2.cache_tuner.as_ref().unwrap().adjustment_count(), 1);
    }

    // ===== Session 85: 搜索质量评估集成测试 =====

    /// 创建带搜索质量评估器的测试 Orchestrator
    fn make_orchestrator_with_quality<'a>(
        chat: &'a MockChatClient,
        workspace_dir: &'a str,
    ) -> Orchestrator<'a, MockChatClient, MockTestRunner, MockExtractor> {
        std::fs::create_dir_all(format!("{}/.forge", workspace_dir)).unwrap();
        make_orchestrator(chat, workspace_dir)
            .with_dev_trace(true)
            .with_search_quality_evaluator(
                crate::search_quality::SearchQualityEvaluator::with_default_config(),
            )
    }

    #[test]
    fn test_with_search_quality_evaluator_sets_field() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let orch = make_orchestrator(&chat, dir.path().to_str().unwrap());

        // 默认 search_quality_evaluator 为 None
        assert!(orch.search_quality_evaluator.is_none());

        // 启用后应为 Some
        let orch = orch.with_search_quality_evaluator(
            crate::search_quality::SearchQualityEvaluator::with_default_config(),
        );
        assert!(orch.search_quality_evaluator.is_some());
        assert!(orch.search_quality_evaluator.as_ref().unwrap().is_enabled());
    }

    #[test]
    fn test_evaluate_search_quality_noop_without_evaluator() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap()).with_dev_trace(true);
        setup_test_phase(&mut orch);

        // 没有 search_quality_evaluator, 应该是无操作
        orch.evaluate_search_quality(0, 0);
        // 不 panic 即可
    }

    #[test]
    fn test_evaluate_search_quality_noop_without_devtrace() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_search_quality_evaluator(
                crate::search_quality::SearchQualityEvaluator::with_default_config(),
            );
        setup_test_phase(&mut orch);

        // 没有 dev_trace, 应该是无操作
        orch.evaluate_search_quality(0, 0);
        // 不 panic 即可
    }

    #[test]
    fn test_evaluate_search_quality_noop_with_empty_entries() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator_with_quality(&chat, dir.path().to_str().unwrap());
        setup_test_phase(&mut orch);

        // DevTrace 为空, 应该无操作
        orch.evaluate_search_quality(0, 0);
        assert!(orch.search_quality_evaluator.as_ref().unwrap().is_enabled());
    }

    #[test]
    fn test_evaluate_search_quality_insufficient_data() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator_with_quality(&chat, dir.path().to_str().unwrap());
        setup_test_phase(&mut orch);

        // 写入少量数据 (不足 min_samples=5)
        write_trace_entries(
            &orch,
            &[
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
            ],
        );

        orch.evaluate_search_quality(0, 0);

        // 数据不足, 应保持启用
        assert!(orch.search_quality_evaluator.as_ref().unwrap().is_enabled());
    }

    #[test]
    fn test_evaluate_search_quality_disables_harmful_search() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator_with_quality(&chat, dir.path().to_str().unwrap());
        setup_test_phase(&mut orch);

        // 构建数据: 搜索后修复率低 (0/3), 不搜索修复率高 (3/3)
        // diff = 0% - 100% = -100% < -10% → 禁用搜索
        let entries: Vec<DevTraceEntry> = vec![
            // 3次有搜索 → 全部编译失败
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
                "failed",
                50,
                false,
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
                "failed",
                50,
                false,
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
            // 3次无搜索 → 全部编译通过
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
                "passed",
                50,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(5),
                Some("task6"),
                "check",
                "passed",
                50,
                true,
                None,
            ),
        ];
        write_trace_entries(&orch, &entries);

        orch.evaluate_search_quality(0, 0);

        // 搜索应被禁用
        assert!(!orch.search_quality_evaluator.as_ref().unwrap().is_enabled());
    }

    #[test]
    fn test_evaluate_search_quality_keeps_beneficial_search() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator_with_quality(&chat, dir.path().to_str().unwrap());
        setup_test_phase(&mut orch);

        // 构建数据: 搜索后修复率高 (3/3), 不搜索修复率低 (0/3)
        // diff = 100% - 0% = +100% >= 5% → 保持搜索
        let entries: Vec<DevTraceEntry> = vec![
            // 3次有搜索 → 全部编译通过
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
                "passed",
                50,
                true,
                None,
            ),
            // 3次无搜索 → 全部编译失败
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(3),
                Some("task4"),
                "check",
                "failed",
                50,
                false,
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
        write_trace_entries(&orch, &entries);

        orch.evaluate_search_quality(0, 0);

        // 搜索应保持启用
        assert!(orch.search_quality_evaluator.as_ref().unwrap().is_enabled());
    }

    #[test]
    fn test_evaluate_search_quality_writes_devtrace_entry() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator_with_quality(&chat, dir.path().to_str().unwrap());
        setup_test_phase(&mut orch);

        // 写入一些数据
        write_trace_entries(
            &orch,
            &[
                DevTraceEntry::new(
                    TraceAction::WebSearch,
                    Some(0),
                    Some(0),
                    Some("task"),
                    "q",
                    "r",
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
            ],
        );

        // 调用 evaluate_search_quality
        orch.evaluate_search_quality(0, 0);

        // 验证 DevTrace 中新增了 SearchQuality 条目
        let entries = orch.dev_trace.as_ref().unwrap().read_all().unwrap();
        let sq_entries: Vec<_> = entries
            .iter()
            .filter(|e| e.action == TraceAction::SearchQuality)
            .collect();
        assert_eq!(sq_entries.len(), 1);
        assert!(sq_entries[0].success);
    }

    #[test]
    fn test_search_disabled_after_quality_eval_skips_search() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator_with_quality(&chat, dir.path().to_str().unwrap());
        setup_test_phase(&mut orch);

        // 手动禁用搜索
        if let Some(ref mut evaluator) = orch.search_quality_evaluator {
            let mut stats = crate::dev_trace::SearchQualityStats::new();
            // 搜索有害
            for _ in 0..3 {
                stats.record_with_search(false);
                stats.record_without_search(true);
            }
            evaluator.evaluate_and_apply(&stats);
        }

        // 验证搜索已被禁用
        assert!(!orch.search_quality_evaluator.as_ref().unwrap().is_enabled());

        // 验证 auto_search 逻辑: 当 evaluator 禁用时, 应跳过搜索
        let search_enabled = orch
            .search_quality_evaluator
            .as_ref()
            .is_none_or(|e| e.is_enabled());
        assert!(!search_enabled);
    }

    #[test]
    fn test_with_clarification_preserves_search_quality_evaluator() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_search_quality_evaluator(
                crate::search_quality::SearchQualityEvaluator::with_default_config(),
            );

        // with_clarification 应保留 search_quality_evaluator
        let orch2 = orch.with_clarification(HeuristicClarificationChecker::new());
        assert!(orch2.search_quality_evaluator.is_some());
        assert!(orch2
            .search_quality_evaluator
            .as_ref()
            .unwrap()
            .is_enabled());
    }

    #[test]
    fn test_evaluate_search_quality_neutral_keeps_enabled() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator_with_quality(&chat, dir.path().to_str().unwrap());
        setup_test_phase(&mut orch);

        // diff = 0% → 中性 → 保持
        // 有搜索: 3/3 通过, 无搜索: 3/3 通过
        let entries: Vec<DevTraceEntry> = vec![
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
                "passed",
                50,
                true,
                None,
            ),
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
                "passed",
                50,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(5),
                Some("task6"),
                "check",
                "passed",
                50,
                true,
                None,
            ),
        ];
        write_trace_entries(&orch, &entries);

        orch.evaluate_search_quality(0, 0);

        // 中性 → 保持启用
        assert!(orch.search_quality_evaluator.as_ref().unwrap().is_enabled());
    }

    // ===== Session 86: SearchQualityEvaluator 持久化集成测试 =====

    #[test]
    fn test_final_report_saves_search_quality_history() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator_with_quality(&chat, dir.path().to_str().unwrap());
        setup_test_phase(&mut orch);

        // 调用 final_report
        orch.final_report().unwrap();

        // 验证历史文件已创建
        let history_path = dir
            .path()
            .join(".forge")
            .join("search_quality_history.json");
        assert!(history_path.exists(), "搜索质量历史文件应存在");

        // 验证可以加载
        let loaded = crate::search_quality::SearchQualityHistory::load(&history_path).unwrap();
        assert!(loaded.initial_enabled); // 初始启用
        assert!(loaded.current_enabled); // 当前仍启用
        assert!(loaded.saved_at.is_some()); // final_report 保存时添加时间戳
    }

    #[test]
    fn test_final_report_no_history_without_evaluator() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap());
        setup_test_phase(&mut orch);
        // 不启用 search_quality_evaluator

        orch.final_report().unwrap();

        // 验证历史文件未创建
        let history_path = dir
            .path()
            .join(".forge")
            .join("search_quality_history.json");
        assert!(!history_path.exists(), "无 evaluator 时不应创建历史文件");
    }

    #[test]
    fn test_search_quality_history_preserves_disabled_state() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator_with_quality(&chat, dir.path().to_str().unwrap());
        setup_test_phase(&mut orch);

        // 手动禁用搜索
        if let Some(ref mut evaluator) = orch.search_quality_evaluator {
            let mut stats = crate::dev_trace::SearchQualityStats::new();
            for _ in 0..3 {
                stats.record_with_search(false);
                stats.record_without_search(true);
            }
            evaluator.evaluate_and_apply(&stats);
        }
        assert!(!orch.search_quality_evaluator.as_ref().unwrap().is_enabled());

        // 保存
        orch.final_report().unwrap();

        // 加载并验证
        let history_path = dir
            .path()
            .join(".forge")
            .join("search_quality_history.json");
        let loaded = crate::search_quality::SearchQualityHistory::load(&history_path).unwrap();
        assert!(loaded.initial_enabled); // session 开始时启用
        assert!(!loaded.current_enabled); // 最终禁用
        assert!(loaded.enabled_changed());
        assert_eq!(loaded.disable_count, 1);
        assert_eq!(loaded.evaluation_count, 1);
    }

    #[test]
    fn test_search_quality_history_preserves_evaluation_count() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator_with_quality(&chat, dir.path().to_str().unwrap());
        setup_test_phase(&mut orch);

        // 多次评估
        if let Some(ref mut evaluator) = orch.search_quality_evaluator {
            let stats = crate::dev_trace::SearchQualityStats::new();
            evaluator.evaluate_and_apply(&stats);
            evaluator.evaluate_and_apply(&stats);
            evaluator.evaluate_and_apply(&stats);
        }
        assert_eq!(
            orch.search_quality_evaluator
                .as_ref()
                .unwrap()
                .evaluation_count(),
            3
        );

        // 保存
        orch.final_report().unwrap();

        // 加载并验证
        let history_path = dir
            .path()
            .join(".forge")
            .join("search_quality_history.json");
        let loaded = crate::search_quality::SearchQualityHistory::load(&history_path).unwrap();
        assert_eq!(loaded.evaluation_count, 3);
    }

    #[test]
    fn test_search_quality_history_cross_session_persistence() {
        let dir = tempdir().unwrap();

        // === Session 1: 评估并禁用 ===
        {
            let chat = MockChatClient::new(vec![]);
            let mut orch = make_orchestrator_with_quality(&chat, dir.path().to_str().unwrap());
            setup_test_phase(&mut orch);

            if let Some(ref mut evaluator) = orch.search_quality_evaluator {
                let mut stats = crate::dev_trace::SearchQualityStats::new();
                for _ in 0..3 {
                    stats.record_with_search(false);
                    stats.record_without_search(true);
                }
                evaluator.evaluate_and_apply(&stats);
            }
            assert!(!orch.search_quality_evaluator.as_ref().unwrap().is_enabled());

            orch.final_report().unwrap();
        }

        // === Session 2: 从历史恢复 ===
        {
            let chat = MockChatClient::new(vec![]);
            let mut orch = make_orchestrator_with_quality(&chat, dir.path().to_str().unwrap());
            setup_test_phase(&mut orch);

            // 模拟 run() 中的加载逻辑
            if let Some(ref evaluator) = orch.search_quality_evaluator {
                let config = evaluator.config().clone();
                if let Some(loaded) =
                    crate::search_quality::SearchQualityEvaluator::load_from_workspace(
                        &orch.workspace.root,
                        config,
                    )
                {
                    let enabled = loaded.is_enabled();
                    let eval_count = loaded.evaluation_count();
                    orch.search_quality_evaluator = Some(loaded);
                    assert!(!enabled, "搜索应从历史恢复为禁用");
                    assert_eq!(eval_count, 1, "评估次数应保留");
                }
            }

            assert!(!orch.search_quality_evaluator.as_ref().unwrap().is_enabled());
            assert_eq!(
                orch.search_quality_evaluator
                    .as_ref()
                    .unwrap()
                    .evaluation_count(),
                1
            );
        }
    }

    // ===== Session 87: DevTrace 历史摘要面板集成测试 =====

    #[test]
    fn test_final_report_devtrace_includes_cache_tuning_history() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator_with_tuning(&chat, dir.path().to_str().unwrap());
        setup_test_phase(&mut orch);

        // 手动调整 TTL
        if let Some(ref mut tuner) = orch.cache_tuner {
            let decision =
                crate::cache_tuning::CacheTuningDecision::adjust_ttl(1800, 2700, 0.33, "延长");
            tuner.apply_decision(&decision);
        }

        // 捕获 stdout 验证报告中包含缓存调优历史面板
        // final_report 内部会构建 DevTraceSummary 并附加历史摘要
        orch.final_report().unwrap();

        // 验证历史文件已创建 (含调优数据)
        let history_path = dir.path().join(".forge").join("cache_tuning_history.json");
        assert!(history_path.exists());
        let loaded = crate::cache_tuning::CacheTuningHistory::load(&history_path).unwrap();
        assert_eq!(loaded.initial_ttl, 1800);
        assert_eq!(loaded.current_ttl, 2700);
        assert_eq!(loaded.adjustment_count, 1);
        assert_eq!(loaded.decisions.len(), 1);
    }

    #[test]
    fn test_final_report_devtrace_includes_search_quality_history() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator_with_quality(&chat, dir.path().to_str().unwrap());
        setup_test_phase(&mut orch);

        // 执行评估
        if let Some(ref mut evaluator) = orch.search_quality_evaluator {
            let mut stats = crate::dev_trace::SearchQualityStats::new();
            for _ in 0..3 {
                stats.record_with_search(false);
                stats.record_without_search(true);
            }
            evaluator.evaluate_and_apply(&stats);
        }
        assert!(!orch.search_quality_evaluator.as_ref().unwrap().is_enabled());

        orch.final_report().unwrap();

        // 验证历史文件已创建 (含搜索质量数据)
        let history_path = dir
            .path()
            .join(".forge")
            .join("search_quality_history.json");
        assert!(history_path.exists());
        let loaded = crate::search_quality::SearchQualityHistory::load(&history_path).unwrap();
        assert!(!loaded.current_enabled); // 最终禁用
        assert!(loaded.initial_enabled); // 初始启用
        assert!(loaded.enabled_changed());
        assert_eq!(loaded.evaluation_count, 1);
        assert_eq!(loaded.disable_count, 1);
    }

    #[test]
    fn test_final_report_devtrace_includes_both_histories() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);

        // 同时启用 cache_tuner 和 search_quality_evaluator
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_cache_tuner(CacheTuner::with_default_config(1800))
            .with_search_quality_evaluator(SearchQualityEvaluator::with_default_config())
            .with_dev_trace(true);
        setup_test_phase(&mut orch);

        // 调整 TTL
        if let Some(ref mut tuner) = orch.cache_tuner {
            let decision =
                crate::cache_tuning::CacheTuningDecision::adjust_ttl(1800, 2700, 0.33, "延长");
            tuner.apply_decision(&decision);
        }

        // 评估搜索质量
        if let Some(ref mut evaluator) = orch.search_quality_evaluator {
            let mut stats = crate::dev_trace::SearchQualityStats::new();
            for _ in 0..3 {
                stats.record_with_search(false);
                stats.record_without_search(true);
            }
            evaluator.evaluate_and_apply(&stats);
        }

        orch.final_report().unwrap();

        // 两个历史文件都应存在
        let ct_path = dir.path().join(".forge").join("cache_tuning_history.json");
        let sq_path = dir
            .path()
            .join(".forge")
            .join("search_quality_history.json");
        assert!(ct_path.exists(), "缓存调优历史文件应存在");
        assert!(sq_path.exists(), "搜索质量历史文件应存在");

        // 验证缓存调优历史
        let ct = crate::cache_tuning::CacheTuningHistory::load(&ct_path).unwrap();
        assert_eq!(ct.current_ttl, 2700);
        assert_eq!(ct.adjustment_count, 1);

        // 验证搜索质量历史
        let sq = crate::search_quality::SearchQualityHistory::load(&sq_path).unwrap();
        assert!(!sq.current_enabled);
        assert_eq!(sq.evaluation_count, 1);
    }

    #[test]
    fn test_final_report_no_history_panels_without_evaluators() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap()).with_dev_trace(true);
        setup_test_phase(&mut orch);

        // 不启用 cache_tuner 和 search_quality_evaluator
        assert!(orch.cache_tuner.is_none());
        assert!(orch.search_quality_evaluator.is_none());

        // final_report 应正常执行, 不生成历史文件
        orch.final_report().unwrap();

        let ct_path = dir.path().join(".forge").join("cache_tuning_history.json");
        let sq_path = dir
            .path()
            .join(".forge")
            .join("search_quality_history.json");
        assert!(!ct_path.exists());
        assert!(!sq_path.exists());
    }

    // ===== Session 88: DevTraceSummary JSON 导出集成测试 =====

    #[test]
    fn test_final_report_creates_json_export() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap()).with_dev_trace(true);
        setup_test_phase(&mut orch);

        // 写入一些 trace 条目
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
                Some("check"),
                "in",
                "out",
                500,
                false,
                Some("编译错误"),
            ),
        ];
        write_trace_entries(&orch, &entries);

        orch.final_report().unwrap();

        // JSON 文件应存在
        let json_path = dir.path().join(".forge").join("devtrace_summary.json");
        assert!(json_path.exists(), "DevTrace JSON 文件应存在");

        // 验证内容可反序列化为 DevTraceJsonExport
        let content = std::fs::read_to_string(&json_path).unwrap();
        let export: crate::dev_trace::DevTraceJsonExport =
            serde_json::from_str(&content).expect("应能反序列化为 DevTraceJsonExport");

        // 验证元数据
        assert!(!export.meta.exported_at.is_empty());
        assert_eq!(export.meta.format_version, "1.0");
        assert!(!export.meta.forge_version.is_empty());

        // 验证摘要内容
        assert_eq!(export.summary.total_entries, 2);
    }

    #[test]
    fn test_final_report_json_includes_history_data() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);

        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_cache_tuner(CacheTuner::with_default_config(1800))
            .with_search_quality_evaluator(SearchQualityEvaluator::with_default_config())
            .with_dev_trace(true);
        setup_test_phase(&mut orch);

        // 调整 TTL
        if let Some(ref mut tuner) = orch.cache_tuner {
            let decision =
                crate::cache_tuning::CacheTuningDecision::adjust_ttl(1800, 2700, 0.33, "延长");
            tuner.apply_decision(&decision);
        }

        // 评估搜索质量
        if let Some(ref mut evaluator) = orch.search_quality_evaluator {
            let mut stats = crate::dev_trace::SearchQualityStats::new();
            for _ in 0..3 {
                stats.record_with_search(false);
                stats.record_without_search(true);
            }
            evaluator.evaluate_and_apply(&stats);
        }

        orch.final_report().unwrap();

        // JSON 文件应存在且包含历史数据
        let json_path = dir.path().join(".forge").join("devtrace_summary.json");
        assert!(json_path.exists());

        let content = std::fs::read_to_string(&json_path).unwrap();
        let export: crate::dev_trace::DevTraceJsonExport = serde_json::from_str(&content).unwrap();

        // 缓存调优历史应存在
        let cth = export
            .summary
            .cache_tuning_history_summary
            .expect("JSON 应包含缓存调优历史");
        assert_eq!(cth.initial_ttl, 1800);
        assert_eq!(cth.current_ttl, 2700);
        assert_eq!(cth.ttl_delta, 900);

        // 搜索质量历史应存在
        let sqh = export
            .summary
            .search_quality_history_summary
            .expect("JSON 应包含搜索质量历史");
        assert!(sqh.initial_enabled);
        assert!(!sqh.current_enabled);
        assert!(sqh.enabled_changed);
    }

    #[test]
    fn test_final_report_json_without_evaluators() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap()).with_dev_trace(true);
        setup_test_phase(&mut orch);

        // 写入 trace 条目
        let entries = vec![DevTraceEntry::new(
            TraceAction::TaskExecution,
            Some(0),
            Some(0),
            Some("task"),
            "in",
            "out",
            500,
            true,
            None,
        )];
        write_trace_entries(&orch, &entries);

        orch.final_report().unwrap();

        // JSON 文件应存在 (即使没有 evaluators)
        let json_path = dir.path().join(".forge").join("devtrace_summary.json");
        assert!(json_path.exists());

        let content = std::fs::read_to_string(&json_path).unwrap();
        let export: crate::dev_trace::DevTraceJsonExport = serde_json::from_str(&content).unwrap();

        // 摘要应存在
        assert_eq!(export.summary.total_entries, 1);
        // 历史摘要应为 None (没有 evaluators)
        assert!(export.summary.cache_tuning_history_summary.is_none());
        assert!(export.summary.search_quality_history_summary.is_none());
    }

    #[test]
    fn test_final_report_json_overwrites_previous() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap()).with_dev_trace(true);
        setup_test_phase(&mut orch);

        // 第一次 final_report (空 trace)
        orch.final_report().unwrap();

        let json_path = dir.path().join(".forge").join("devtrace_summary.json");
        assert!(json_path.exists());
        let content1 = std::fs::read_to_string(&json_path).unwrap();
        let export1: crate::dev_trace::DevTraceJsonExport =
            serde_json::from_str(&content1).unwrap();
        assert_eq!(export1.summary.total_entries, 0);

        // 写入 trace 条目后再次 final_report
        let entries = vec![
            DevTraceEntry::new(
                TraceAction::TaskExecution,
                Some(0),
                Some(0),
                Some("t1"),
                "in",
                "out",
                500,
                true,
                None,
            ),
            DevTraceEntry::new(
                TraceAction::CompileCheck,
                Some(0),
                Some(0),
                Some("t2"),
                "in",
                "out",
                300,
                true,
                None,
            ),
        ];
        write_trace_entries(&orch, &entries);

        orch.final_report().unwrap();

        // 文件应被覆盖
        let content2 = std::fs::read_to_string(&json_path).unwrap();
        let export2: crate::dev_trace::DevTraceJsonExport =
            serde_json::from_str(&content2).unwrap();
        assert_eq!(export2.summary.total_entries, 2);
    }

    #[test]
    fn test_final_report_json_meta_valid() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap()).with_dev_trace(true);
        setup_test_phase(&mut orch);

        orch.final_report().unwrap();

        let json_path = dir.path().join(".forge").join("devtrace_summary.json");
        let content = std::fs::read_to_string(&json_path).unwrap();
        let export: crate::dev_trace::DevTraceJsonExport = serde_json::from_str(&content).unwrap();

        // exported_at 应为有效的 ISO 8601 时间戳
        assert!(export.meta.exported_at.contains('T'));
        // forge_version 应与 env!("CARGO_PKG_VERSION") 一致
        assert_eq!(export.meta.forge_version, env!("CARGO_PKG_VERSION"));
        // format_version 应为 "1.0"
        assert_eq!(export.meta.format_version, "1.0");
    }

    // ===== Session 92: 协同分析持久化集成测试 =====

    #[test]
    fn test_final_report_saves_synergy_history() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);

        // 同时启用三个评估器
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_cache_tuner(CacheTuner::with_default_config(1800))
            .with_search_quality_evaluator(SearchQualityEvaluator::with_default_config())
            .with_dev_trace(true);
        setup_test_phase(&mut orch);

        orch.final_report().unwrap();

        // 验证协同分析历史文件已创建
        let history_path = dir
            .path()
            .join(".forge")
            .join("evaluator_synergy_history.json");
        assert!(history_path.exists(), "协同分析历史文件应存在");

        // 验证可以加载
        let loaded =
            crate::evaluator_synergy::EvaluatorSynergyHistory::load(&history_path).unwrap();
        assert_eq!(loaded.session_count(), 1);
        assert!(loaded.saved_at.is_some());
    }

    #[test]
    fn test_final_report_no_synergy_history_without_evaluators() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap()).with_dev_trace(true);
        setup_test_phase(&mut orch);

        // 不启用任何评估器
        orch.final_report().unwrap();

        // 不应创建协同分析历史文件
        let history_path = dir
            .path()
            .join(".forge")
            .join("evaluator_synergy_history.json");
        assert!(!history_path.exists(), "无评估器时不应创建协同分析历史文件");
    }

    #[test]
    fn test_synergy_history_cross_session_accumulation() {
        let dir = tempdir().unwrap();

        // Session 1: 保存协同分析历史
        {
            let chat = MockChatClient::new(vec![]);
            let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
                .with_cache_tuner(CacheTuner::with_default_config(1800))
                .with_dev_trace(true);
            setup_test_phase(&mut orch);

            orch.final_report().unwrap();
        }

        // 验证 Session 1 的历史
        let history_path = dir
            .path()
            .join(".forge")
            .join("evaluator_synergy_history.json");
        let history1 =
            crate::evaluator_synergy::EvaluatorSynergyHistory::load(&history_path).unwrap();
        assert_eq!(history1.session_count(), 1);

        // Session 2: 加载历史并追加
        {
            let chat = MockChatClient::new(vec![]);
            let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
                .with_cache_tuner(CacheTuner::with_default_config(1800))
                .with_dev_trace(true);
            setup_test_phase(&mut orch);

            orch.final_report().unwrap();
        }

        // 验证 Session 2 的历史 (应累积为 2 个 session)
        let history2 =
            crate::evaluator_synergy::EvaluatorSynergyHistory::load(&history_path).unwrap();
        assert_eq!(history2.session_count(), 2);
        assert_eq!(history2.sessions[0].session_index, 1);
        assert_eq!(history2.sessions[1].session_index, 2);
    }

    #[test]
    fn test_synergy_history_in_devtrace_json() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);

        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_cache_tuner(CacheTuner::with_default_config(1800))
            .with_search_quality_evaluator(SearchQualityEvaluator::with_default_config())
            .with_dev_trace(true);
        setup_test_phase(&mut orch);

        orch.final_report().unwrap();

        // DevTrace JSON 应包含协同分析历史
        let json_path = dir.path().join(".forge").join("devtrace_summary.json");
        assert!(json_path.exists());

        let content = std::fs::read_to_string(&json_path).unwrap();
        let export: crate::dev_trace::DevTraceJsonExport = serde_json::from_str(&content).unwrap();

        // 协同分析摘要应存在
        assert!(
            export.summary.evaluator_synergy_summary.is_some(),
            "JSON 应包含协同分析摘要"
        );

        // 协同分析历史摘要应存在
        let syh = export
            .summary
            .evaluator_synergy_history_summary
            .expect("JSON 应包含协同分析历史摘要");
        assert_eq!(syh.session_count, 1);
    }

    #[test]
    fn test_synergy_history_trend_after_multiple_sessions() {
        let dir = tempdir().unwrap();

        // 手动创建一个已有 2 个 session 的历史
        use crate::evaluator_synergy::{EvaluatorSynergyHistory, EvaluatorSynergyHistoryEntry};
        use chrono::Utc;

        {
            let mut history = EvaluatorSynergyHistory::new();
            history.add_entry(EvaluatorSynergyHistoryEntry::new(
                1,
                Utc::now(),
                1,
                0.50,
                0.60,
                3,
                0,
                false,
                true,
            ));
            history.add_entry(EvaluatorSynergyHistoryEntry::new(
                2,
                Utc::now(),
                1,
                0.65,
                0.70,
                3,
                0,
                false,
                true,
            ));
            history.save_to_workspace(dir.path()).unwrap();
        }

        // Session 3: final_report 应加载历史并追加
        {
            let chat = MockChatClient::new(vec![]);
            let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
                .with_cache_tuner(CacheTuner::with_default_config(1800))
                .with_dev_trace(true);
            setup_test_phase(&mut orch);

            orch.final_report().unwrap();
        }

        // 验证历史累积为 3 个 session
        let history_path = dir
            .path()
            .join(".forge")
            .join("evaluator_synergy_history.json");
        let history =
            crate::evaluator_synergy::EvaluatorSynergyHistory::load(&history_path).unwrap();
        assert_eq!(history.session_count(), 3);
        assert_eq!(history.sessions[2].session_index, 3);
        // 前两个 session 的评分保留
        assert!((history.sessions[0].synergy_score - 0.50).abs() < 0.001);
        assert!((history.sessions[1].synergy_score - 0.65).abs() < 0.001);
        // 第三个 session 的评分由 final_report 构建 (取决于 mock 数据)
        // 趋势无法预测 (第三 session 评分取决于 mock 环境), 只验证累积正确
    }

    // ===== Session 93: Sparkline + HTML 报告集成测试 =====

    #[test]
    fn test_devtrace_html_report_generated() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_cache_tuner(CacheTuner::with_default_config(1800))
            .with_dev_trace(true);
        setup_test_phase(&mut orch);

        orch.final_report().unwrap();

        // HTML 报告应存在
        let html_path = dir.path().join(".forge").join("devtrace_report.html");
        assert!(html_path.exists(), "HTML 报告文件应存在");

        let content = std::fs::read_to_string(&html_path).unwrap();
        assert!(content.contains("<!DOCTYPE html>"));
        assert!(content.contains("Forge DevTrace"));
        assert!(content.contains("chart.js"));
    }

    #[test]
    fn test_devtrace_analysis_report_generated() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_cache_tuner(CacheTuner::with_default_config(1800))
            .with_dev_trace(true);
        setup_test_phase(&mut orch);

        orch.final_report().unwrap();

        // 分析报告应存在
        let analysis_path = dir.path().join(".forge").join("devtrace_analysis.md");
        assert!(analysis_path.exists(), "DevTrace 分析报告文件应存在");

        let content = std::fs::read_to_string(&analysis_path).unwrap();
        assert!(content.contains("# DevTrace 智能分析报告"));
        assert!(content.contains("健康度评分"));
        assert!(content.contains("可操作建议"));
    }

    #[test]
    fn test_devtrace_analysis_report_with_cache_data() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_cache_tuner(CacheTuner::with_default_config(1800))
            .with_dev_trace(true);
        setup_test_phase(&mut orch);

        // 手动写入 DevTrace 条目以提供缓存数据
        if let Some(ref trace) = orch.dev_trace {
            use crate::dev_trace::{DevTraceEntry, TraceAction};
            let entry = DevTraceEntry::new(
                TraceAction::WebSearch,
                Some(0),
                Some(0),
                Some("test"),
                "query=test",
                "搜索成功, 耗时=2000ms",
                2000,
                true,
                None,
            );
            let _ = trace.write_entry(&entry);
        }

        orch.final_report().unwrap();

        let analysis_path = dir.path().join(".forge").join("devtrace_analysis.md");
        assert!(analysis_path.exists());
    }

    #[test]
    fn test_devtrace_analysis_report_no_devtrace() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap());
        setup_test_phase(&mut orch);

        orch.final_report().unwrap();

        // 没有 DevTrace 时, 不应生成分析报告
        let analysis_path = dir.path().join(".forge").join("devtrace_analysis.md");
        assert!(!analysis_path.exists());
    }

    #[test]
    fn test_devtrace_analysis_report_contains_recommendations() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap()).with_dev_trace(true);
        setup_test_phase(&mut orch);

        orch.final_report().unwrap();

        let analysis_path = dir.path().join(".forge").join("devtrace_analysis.md");
        assert!(analysis_path.exists());
        let content = std::fs::read_to_string(&analysis_path).unwrap();
        // 空摘要: 成功率=0 → 应有 Critical 建议
        assert!(content.contains("可操作建议"));
        assert!(content.contains("成功率"));
    }

    // ===== Session 100: 健康度评分历史 + HTML 分析报告集成测试 =====

    #[test]
    fn test_final_report_saves_health_score_history() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap()).with_dev_trace(true);
        setup_test_phase(&mut orch);

        orch.final_report().unwrap();

        // 健康度评分历史文件应存在
        let history_path = dir.path().join(".forge").join("health_score_history.json");
        assert!(history_path.exists(), "健康度评分历史文件应存在");

        // 验证内容可反序列化
        let loaded = crate::dev_trace_analyzer::HealthScoreHistory::load(&history_path).unwrap();
        assert_eq!(loaded.session_count(), 1);
        assert!(loaded.latest_score() >= 0.0);
    }

    #[test]
    fn test_health_score_history_cross_session_accumulation() {
        let dir = tempdir().unwrap();

        // Session 1: 运行并保存历史
        {
            let chat = MockChatClient::new(vec![]);
            let mut orch =
                make_orchestrator(&chat, dir.path().to_str().unwrap()).with_dev_trace(true);
            setup_test_phase(&mut orch);
            orch.final_report().unwrap();
        }

        // Session 2: 再次运行, 应加载历史并追加
        {
            let chat = MockChatClient::new(vec![]);
            let mut orch =
                make_orchestrator(&chat, dir.path().to_str().unwrap()).with_dev_trace(true);
            setup_test_phase(&mut orch);
            orch.final_report().unwrap();
        }

        // 验证历史累积为 2 个 session
        let history_path = dir.path().join(".forge").join("health_score_history.json");
        let history = crate::dev_trace_analyzer::HealthScoreHistory::load(&history_path).unwrap();
        assert_eq!(history.session_count(), 2);
        assert_eq!(history.sessions[0].session_index, 1);
        assert_eq!(history.sessions[1].session_index, 2);
    }

    #[test]
    fn test_final_report_generates_analysis_html() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap()).with_dev_trace(true);
        setup_test_phase(&mut orch);

        orch.final_report().unwrap();

        // HTML 分析报告应存在
        let html_path = dir.path().join(".forge").join("devtrace_analysis.html");
        assert!(html_path.exists(), "DevTrace 分析 HTML 报告应存在");

        let content = std::fs::read_to_string(&html_path).unwrap();
        assert!(content.contains("<!DOCTYPE html>"));
        assert!(content.contains("chart.js"));
        assert!(content.contains("健康度评分"));
    }

    #[test]
    fn test_health_score_history_in_devtrace_json() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap()).with_dev_trace(true);
        setup_test_phase(&mut orch);

        orch.final_report().unwrap();

        // DevTrace JSON 应包含健康度评分历史摘要
        let json_path = dir.path().join(".forge").join("devtrace_summary.json");
        assert!(json_path.exists());

        let content = std::fs::read_to_string(&json_path).unwrap();
        assert!(
            content.contains("health_score_history_summary"),
            "JSON 应包含 health_score_history_summary 字段"
        );
    }

    #[test]
    fn test_final_report_health_score_history_trend_after_multiple_sessions() {
        let dir = tempdir().unwrap();

        // 手动创建一个已有 2 个 session 的历史
        use chrono::Utc;
        {
            let mut history = crate::dev_trace_analyzer::HealthScoreHistory::new();
            history.add_entry(crate::dev_trace_analyzer::HealthScoreHistoryEntry::new(
                1,
                Utc::now(),
                50.0,
                "一般".to_string(),
                0.3,
                1,
                2,
                0,
                50,
                1000,
            ));
            history.add_entry(crate::dev_trace_analyzer::HealthScoreHistoryEntry::new(
                2,
                Utc::now(),
                75.0,
                "良好".to_string(),
                0.8,
                0,
                1,
                2,
                100,
                3600000,
            ));
            history.save_to_workspace(dir.path()).unwrap();
        }

        // Session 3: final_report 应加载历史并追加
        {
            let chat = MockChatClient::new(vec![]);
            let mut orch =
                make_orchestrator(&chat, dir.path().to_str().unwrap()).with_dev_trace(true);
            setup_test_phase(&mut orch);
            orch.final_report().unwrap();
        }

        // 验证历史累积为 3 个 session
        let history_path = dir.path().join(".forge").join("health_score_history.json");
        let history = crate::dev_trace_analyzer::HealthScoreHistory::load(&history_path).unwrap();
        assert_eq!(history.session_count(), 3);
        assert_eq!(history.sessions[2].session_index, 3);
        // 前两个 session 的评分保留
        assert!((history.sessions[0].score - 50.0).abs() < 0.001);
        assert!((history.sessions[1].score - 75.0).abs() < 0.001);
    }

    #[test]
    fn test_final_report_analysis_config_loaded() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();

        // 先保存一个自定义配置文件
        let config = crate::dev_trace_analyzer::AnalysisConfig::default();
        config.save_to_workspace(dir.path()).unwrap();

        let config_path = dir.path().join(".forge").join("analysis_config.json");
        assert!(config_path.exists());

        // 运行 final_report, 应自动加载配置 (不崩溃)
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap()).with_dev_trace(true);
        setup_test_phase(&mut orch);

        orch.final_report().unwrap();

        // 分析报告应正常生成
        let analysis_path = dir.path().join(".forge").join("devtrace_analysis.md");
        assert!(analysis_path.exists());
    }

    #[test]
    fn test_health_score_history_report_panel() {
        let dir = tempdir().unwrap();

        // 手动创建 2 个 session 的历史
        use chrono::Utc;
        {
            let mut history = crate::dev_trace_analyzer::HealthScoreHistory::new();
            history.add_entry(crate::dev_trace_analyzer::HealthScoreHistoryEntry::new(
                1,
                Utc::now(),
                50.0,
                "一般".to_string(),
                0.3,
                1,
                2,
                0,
                50,
                1000,
            ));
            history.add_entry(crate::dev_trace_analyzer::HealthScoreHistoryEntry::new(
                2,
                Utc::now(),
                75.0,
                "良好".to_string(),
                0.8,
                0,
                1,
                2,
                100,
                3600000,
            ));
            history.save_to_workspace(dir.path()).unwrap();
        }

        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap()).with_dev_trace(true);
        setup_test_phase(&mut orch);

        orch.final_report().unwrap();

        // DevTrace JSON 应包含健康度评分历史摘要
        let json_path = dir.path().join(".forge").join("devtrace_summary.json");
        let content = std::fs::read_to_string(&json_path).unwrap();
        let export: crate::dev_trace::DevTraceJsonExport = serde_json::from_str(&content).unwrap();

        // 健康度评分历史摘要应存在
        let hsh = export
            .summary
            .health_score_history_summary
            .expect("JSON 应包含健康度评分历史摘要");
        assert_eq!(hsh.session_count, 3); // 2 个预设 + 1 个当前
        assert!(hsh.latest_score >= 0.0);
    }

    #[test]
    fn test_sparkline_data_in_devtrace_json() {
        let dir = tempdir().unwrap();

        // 手动创建 2 个 session 的历史
        use crate::evaluator_synergy::{EvaluatorSynergyHistory, EvaluatorSynergyHistoryEntry};
        use chrono::Utc;

        {
            let mut history = EvaluatorSynergyHistory::new();
            history.add_entry(EvaluatorSynergyHistoryEntry::new(
                1,
                Utc::now(),
                1,
                0.45,
                0.60,
                3,
                0,
                false,
                true,
            ));
            history.add_entry(EvaluatorSynergyHistoryEntry::new(
                2,
                Utc::now(),
                1,
                0.70,
                0.75,
                4,
                0,
                false,
                true,
            ));
            history.save_to_workspace(dir.path()).unwrap();
        }

        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_cache_tuner(CacheTuner::with_default_config(1800))
            .with_dev_trace(true);
        setup_test_phase(&mut orch);

        orch.final_report().unwrap();

        // JSON 应包含 sparkline 数据
        let json_path = dir.path().join(".forge").join("devtrace_summary.json");
        assert!(json_path.exists());
        let content = std::fs::read_to_string(&json_path).unwrap();
        let export: crate::dev_trace::DevTraceJsonExport = serde_json::from_str(&content).unwrap();

        // sparkline 数据应存在 (至少 3 个 session: 2 个预存 + 1 个当前)
        assert!(
            export.summary.synergy_score_history.is_some(),
            "JSON 应包含协同评分历史列表"
        );
        let scores = export.summary.synergy_score_history.unwrap();
        assert!(scores.len() >= 2, "至少应有 2 个评分数据点");

        assert!(
            export.summary.fix_rate_history.is_some(),
            "JSON 应包含修复率历史列表"
        );
    }

    #[test]
    fn test_sparkline_rendered_in_report() {
        use crate::dev_trace::DevTraceSummary;
        use crate::evaluator_synergy::EvaluatorSynergyHistorySummary;

        // 构建带 sparkline 数据的摘要
        let summary = DevTraceSummary::empty()
            .with_evaluator_synergy_history(EvaluatorSynergyHistorySummary {
                session_count: 3,
                latest_score: 0.75,
                avg_score: 0.60,
                score_trend: crate::evaluator_synergy::ScoreTrend::Improving,
                score_delta: 0.15,
                latest_fix_rate: 0.80,
                avg_fix_rate: 0.70,
                fix_rate_trend: crate::evaluator_synergy::ScoreTrend::Improving,
                total_decisions: 9,
                total_disables: 0,
                saved_at: None,
            })
            .with_synergy_sparkline(vec![0.45, 0.60, 0.75], vec![0.65, 0.70, 0.80]);

        let report = summary.to_report();

        // 报告应包含 sparkline 图表
        assert!(report.contains("评分趋势图"));
        assert!(report.contains("修复率趋势图"));
        // 应包含 sparkline 字符
        assert!(report.contains('▁') || report.contains('▃') || report.contains('▅'));
        assert!(report.contains('▇') || report.contains('█'));
    }

    #[test]
    fn test_html_report_with_sparkline_data() {
        let dir = tempdir().unwrap();

        // 手动创建 3 个 session 的历史
        use crate::evaluator_synergy::{EvaluatorSynergyHistory, EvaluatorSynergyHistoryEntry};
        use chrono::Utc;

        {
            let mut history = EvaluatorSynergyHistory::new();
            history.add_entry(EvaluatorSynergyHistoryEntry::new(
                1,
                Utc::now(),
                3,
                0.40,
                0.55,
                5,
                0,
                false,
                true,
            ));
            history.add_entry(EvaluatorSynergyHistoryEntry::new(
                2,
                Utc::now(),
                3,
                0.55,
                0.65,
                6,
                0,
                false,
                true,
            ));
            history.add_entry(EvaluatorSynergyHistoryEntry::new(
                3,
                Utc::now(),
                3,
                0.70,
                0.75,
                7,
                0,
                false,
                true,
            ));
            history.save_to_workspace(dir.path()).unwrap();
        }

        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_cache_tuner(CacheTuner::with_default_config(1800))
            .with_search_quality_evaluator(SearchQualityEvaluator::with_default_config())
            .with_dev_trace(true);
        setup_test_phase(&mut orch);

        orch.final_report().unwrap();

        // HTML 报告应包含协同分析趋势图表
        let html_path = dir.path().join(".forge").join("devtrace_report.html");
        assert!(html_path.exists());
        let content = std::fs::read_to_string(&html_path).unwrap();
        assert!(content.contains("协同评分趋势"));
        assert!(content.contains("修复率趋势"));
        assert!(content.contains("synergyScoreChart"));
        assert!(content.contains("fixRateChart"));
    }

    // ===== Session 94: 缓存调优 sparkline 集成测试 =====

    #[test]
    fn test_final_report_extracts_cache_tuning_sparkline_data() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator_with_tuning(&chat, dir.path().to_str().unwrap());
        setup_test_phase(&mut orch);

        // 模拟调优决策
        if let Some(ref mut tuner) = orch.cache_tuner {
            let d1 = crate::cache_tuning::CacheTuningDecision::adjust_ttl(1800, 2700, 0.33, "延长");
            tuner.apply_decision(&d1);
            let d2 =
                crate::cache_tuning::CacheTuningDecision::adjust_ttl(2700, 3600, 0.10, "再次延长");
            tuner.apply_decision(&d2);
        }

        orch.final_report().unwrap();

        // 验证 DevTraceSummary JSON 包含 sparkline 数据
        let json_path = dir.path().join(".forge").join("devtrace_summary.json");
        assert!(json_path.exists());
        let json_content = std::fs::read_to_string(&json_path).unwrap();
        assert!(
            json_content.contains("ttl_history_values"),
            "JSON 应包含 ttl_history_values 字段"
        );
        assert!(
            json_content.contains("correlation_diff_history"),
            "JSON 应包含 correlation_diff_history 字段"
        );
        assert!(json_content.contains("2700"), "JSON 应包含 TTL 值 2700");
    }

    #[test]
    fn test_final_report_no_sparkline_without_decisions() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator_with_tuning(&chat, dir.path().to_str().unwrap());
        setup_test_phase(&mut orch);

        // 不添加任何调优决策
        orch.final_report().unwrap();

        // JSON 应存在但不包含 sparkline 数据 (或包含空数组)
        let json_path = dir.path().join(".forge").join("devtrace_summary.json");
        assert!(json_path.exists());
        // 没有决策时, ttl_history_values 可能为 null 或不存在
        // 关键是不应崩溃
    }

    #[test]
    fn test_final_report_html_contains_ttl_trend_chart() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator_with_tuning(&chat, dir.path().to_str().unwrap());
        setup_test_phase(&mut orch);

        // 模拟调优决策
        if let Some(ref mut tuner) = orch.cache_tuner {
            let d1 = crate::cache_tuning::CacheTuningDecision::adjust_ttl(1800, 2700, 0.33, "延长");
            tuner.apply_decision(&d1);
            let d2 =
                crate::cache_tuning::CacheTuningDecision::adjust_ttl(2700, 3600, 0.10, "再次延长");
            tuner.apply_decision(&d2);
        }

        orch.final_report().unwrap();

        // HTML 报告应包含 TTL 趋势图
        let html_path = dir.path().join(".forge").join("devtrace_report.html");
        assert!(html_path.exists());
        let content = std::fs::read_to_string(&html_path).unwrap();
        assert!(
            content.contains("ttlTrendChart"),
            "HTML 应包含 TTL 趋势图 canvas"
        );
        assert!(
            content.contains("TTL 变化趋势"),
            "HTML 应包含 TTL 变化趋势标题"
        );
    }

    #[test]
    fn test_final_report_html_contains_diff_trend_chart() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator_with_tuning(&chat, dir.path().to_str().unwrap());
        setup_test_phase(&mut orch);

        // 模拟调优决策
        if let Some(ref mut tuner) = orch.cache_tuner {
            let d1 = crate::cache_tuning::CacheTuningDecision::adjust_ttl(1800, 2700, 0.33, "延长");
            tuner.apply_decision(&d1);
            let d2 =
                crate::cache_tuning::CacheTuningDecision::adjust_ttl(2700, 3600, -0.10, "缩短");
            tuner.apply_decision(&d2);
        }

        orch.final_report().unwrap();

        // HTML 报告应包含关联差值趋势图
        let html_path = dir.path().join(".forge").join("devtrace_report.html");
        assert!(html_path.exists());
        let content = std::fs::read_to_string(&html_path).unwrap();
        assert!(
            content.contains("diffTrendChart"),
            "HTML 应包含关联差值趋势图 canvas"
        );
        assert!(
            content.contains("关联差值趋势"),
            "HTML 应包含关联差值趋势标题"
        );
    }

    #[test]
    fn test_final_report_sparkline_with_disable_decision() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator_with_tuning(&chat, dir.path().to_str().unwrap());
        setup_test_phase(&mut orch);

        // 模拟禁用决策
        if let Some(ref mut tuner) = orch.cache_tuner {
            let d1 = crate::cache_tuning::CacheTuningDecision::adjust_ttl(1800, 900, -0.10, "缩短");
            tuner.apply_decision(&d1);
            let d2 =
                crate::cache_tuning::CacheTuningDecision::disable_cache(900, -0.20, "缓存有害");
            tuner.apply_decision(&d2);
        }

        orch.final_report().unwrap();

        // 验证 JSON 包含 sparkline 数据
        let json_path = dir.path().join(".forge").join("devtrace_summary.json");
        assert!(json_path.exists());
        let json_content = std::fs::read_to_string(&json_path).unwrap();
        assert!(
            json_content.contains("ttl_history_values"),
            "JSON 应包含 ttl_history_values 字段"
        );
    }

    // ===== Session 95: 搜索质量/Memory 评估 sparkline 集成测试 =====

    #[test]
    fn test_final_report_extracts_search_quality_sparkline_data() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator_with_quality(&chat, dir.path().to_str().unwrap());
        setup_test_phase(&mut orch);

        // 写入 SearchQuality trace 条目 (含差值信息)
        write_trace_entries(
            &orch,
            &[
                DevTraceEntry::new(
                    TraceAction::SearchQuality,
                    Some(0),
                    Some(0),
                    Some("t1"),
                    "with=2/3",
                    "搜索质量: 保持搜索 (差值 +10.0%, 原因: 有效)",
                    0,
                    true,
                    None,
                ),
                DevTraceEntry::new(
                    TraceAction::SearchQuality,
                    Some(0),
                    Some(1),
                    Some("t2"),
                    "with=1/3",
                    "搜索质量: 禁用搜索 (差值 -15.0%, 原因: 有害)",
                    0,
                    true,
                    None,
                ),
            ],
        );

        orch.final_report().unwrap();

        // 验证 JSON 包含搜索质量 sparkline 数据
        let json_path = dir.path().join(".forge").join("devtrace_summary.json");
        assert!(json_path.exists());
        let json_content = std::fs::read_to_string(&json_path).unwrap();
        assert!(
            json_content.contains("search_diff_history"),
            "JSON 应包含 search_diff_history 字段"
        );
    }

    #[test]
    fn test_final_report_extracts_memory_evaluation_sparkline_data() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_dev_trace(true)
            .with_memory_evaluator(
                crate::memory_evaluation::MemoryContextEvaluator::with_default_config(),
            );
        setup_test_phase(&mut orch);

        // 写入 MemoryEvaluation trace 条目 (含差值信息)
        write_trace_entries(
            &orch,
            &[
                DevTraceEntry::new(
                    TraceAction::MemoryEvaluation,
                    Some(0),
                    Some(0),
                    Some("t1"),
                    "with=2/3",
                    "Memory 评估: KeepInjecting (差值 +10.0%, 有效)",
                    0,
                    true,
                    None,
                ),
                DevTraceEntry::new(
                    TraceAction::MemoryEvaluation,
                    Some(0),
                    Some(1),
                    Some("t2"),
                    "with=1/3",
                    "Memory 评估: DisableInjection (差值 -20.0%, 有害)",
                    0,
                    true,
                    None,
                ),
            ],
        );

        orch.final_report().unwrap();

        // 验证 JSON 包含 Memory 评估 sparkline 数据
        let json_path = dir.path().join(".forge").join("devtrace_summary.json");
        assert!(json_path.exists());
        let json_content = std::fs::read_to_string(&json_path).unwrap();
        assert!(
            json_content.contains("memory_diff_history"),
            "JSON 应包含 memory_diff_history 字段"
        );
    }

    #[test]
    fn test_final_report_html_contains_search_diff_trend_chart() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator_with_quality(&chat, dir.path().to_str().unwrap());
        setup_test_phase(&mut orch);

        // 写入 SearchQuality trace 条目
        write_trace_entries(
            &orch,
            &[
                DevTraceEntry::new(
                    TraceAction::SearchQuality,
                    Some(0),
                    Some(0),
                    Some("t1"),
                    "with=2/3",
                    "搜索质量: 保持搜索 (差值 +10.0%, 原因: 有效)",
                    0,
                    true,
                    None,
                ),
                DevTraceEntry::new(
                    TraceAction::SearchQuality,
                    Some(0),
                    Some(1),
                    Some("t2"),
                    "with=1/3",
                    "搜索质量: 保持搜索 (差值 +5.0%, 原因: 中性)",
                    0,
                    true,
                    None,
                ),
            ],
        );

        orch.final_report().unwrap();

        // 验证 HTML 包含搜索质量差值趋势图
        let html_path = dir.path().join(".forge").join("devtrace_report.html");
        assert!(html_path.exists());
        let html_content = std::fs::read_to_string(&html_path).unwrap();
        assert!(
            html_content.contains("searchDiffTrendChart"),
            "HTML 应包含 searchDiffTrendChart"
        );
        assert!(
            html_content.contains("搜索质量差值趋势"),
            "HTML 应包含搜索质量差值趋势标题"
        );
    }

    #[test]
    fn test_final_report_html_contains_memory_diff_trend_chart() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_dev_trace(true)
            .with_memory_evaluator(
                crate::memory_evaluation::MemoryContextEvaluator::with_default_config(),
            );
        setup_test_phase(&mut orch);

        // 写入 MemoryEvaluation trace 条目
        write_trace_entries(
            &orch,
            &[
                DevTraceEntry::new(
                    TraceAction::MemoryEvaluation,
                    Some(0),
                    Some(0),
                    Some("t1"),
                    "with=2/3",
                    "Memory 评估: KeepInjecting (差值 +10.0%, 有效)",
                    0,
                    true,
                    None,
                ),
                DevTraceEntry::new(
                    TraceAction::MemoryEvaluation,
                    Some(0),
                    Some(1),
                    Some("t2"),
                    "with=2/3",
                    "Memory 评估: KeepInjecting (差值 +5.0%, 中性)",
                    0,
                    true,
                    None,
                ),
            ],
        );

        orch.final_report().unwrap();

        // 验证 HTML 包含 Memory 评估差值趋势图
        let html_path = dir.path().join(".forge").join("devtrace_report.html");
        assert!(html_path.exists());
        let html_content = std::fs::read_to_string(&html_path).unwrap();
        assert!(
            html_content.contains("memoryDiffTrendChart"),
            "HTML 应包含 memoryDiffTrendChart"
        );
        assert!(
            html_content.contains("Memory 评估差值趋势"),
            "HTML 应包含 Memory 评估差值趋势标题"
        );
    }

    // ===== Session 99: 联合决策引擎集成测试 =====

    #[test]
    fn test_final_report_saves_joint_decision_history() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);

        // 启用联合决策引擎
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_cache_tuner(CacheTuner::with_default_config(1800))
            .with_search_quality_evaluator(SearchQualityEvaluator::with_default_config())
            .with_dev_trace(true)
            .with_joint_decision_engine(crate::joint_decision::JointDecisionEngine::default());
        setup_test_phase(&mut orch);

        orch.final_report().unwrap();

        // 验证联合决策历史文件已创建
        let history_path = dir
            .path()
            .join(".forge")
            .join("joint_decision_history.json");
        assert!(history_path.exists(), "联合决策历史文件应存在");

        // 验证可以加载
        let loaded = crate::joint_decision::JointDecisionHistory::load(&history_path).unwrap();
        assert_eq!(loaded.session_count(), 1);
        assert!(loaded.saved_at.is_some());
    }

    #[test]
    fn test_final_report_no_joint_decision_history_without_engine() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
        let chat = MockChatClient::new(vec![]);
        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap()).with_dev_trace(true);
        setup_test_phase(&mut orch);

        // 不启用联合决策引擎
        orch.final_report().unwrap();

        // 不应创建联合决策历史文件
        let history_path = dir
            .path()
            .join(".forge")
            .join("joint_decision_history.json");
        assert!(!history_path.exists(), "无引擎时不应创建联合决策历史文件");
    }

    #[test]
    fn test_joint_decision_history_cross_session_accumulation() {
        let dir = tempdir().unwrap();

        // Session 1: 保存联合决策历史
        {
            let chat = MockChatClient::new(vec![]);
            let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
                .with_cache_tuner(CacheTuner::with_default_config(1800))
                .with_dev_trace(true)
                .with_joint_decision_engine(crate::joint_decision::JointDecisionEngine::default());
            setup_test_phase(&mut orch);

            orch.final_report().unwrap();
        }

        // 验证 Session 1 的历史
        let history_path = dir
            .path()
            .join(".forge")
            .join("joint_decision_history.json");
        let history1 = crate::joint_decision::JointDecisionHistory::load(&history_path).unwrap();
        assert_eq!(history1.session_count(), 1);

        // Session 2: 加载历史并追加
        {
            let chat = MockChatClient::new(vec![]);
            let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
                .with_cache_tuner(CacheTuner::with_default_config(1800))
                .with_dev_trace(true)
                .with_joint_decision_engine(crate::joint_decision::JointDecisionEngine::default());
            setup_test_phase(&mut orch);

            orch.final_report().unwrap();
        }

        // 验证历史累积为 2 个 session
        let history2 = crate::joint_decision::JointDecisionHistory::load(&history_path).unwrap();
        assert_eq!(history2.session_count(), 2);
    }

    #[test]
    fn test_joint_decision_history_in_devtrace_json() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);

        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_cache_tuner(CacheTuner::with_default_config(1800))
            .with_dev_trace(true)
            .with_joint_decision_engine(crate::joint_decision::JointDecisionEngine::default());
        setup_test_phase(&mut orch);

        orch.final_report().unwrap();

        // 验证 DevTrace JSON 包含联合决策历史
        let json_path = dir.path().join(".forge").join("devtrace_summary.json");
        assert!(json_path.exists());
        let json_content = std::fs::read_to_string(&json_path).unwrap();
        assert!(
            json_content.contains("joint_decision_history_summary"),
            "DevTrace JSON 应包含 joint_decision_history_summary 字段"
        );
    }

    #[test]
    fn test_joint_decision_html_contains_panel() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);

        // 手动预存联合决策历史
        use crate::evaluator_synergy::EvaluatorType;
        use crate::joint_decision::{
            JointDecisionAction, JointDecisionHistory, JointDecisionHistoryEntry,
        };
        use chrono::Utc;

        {
            let mut history = JointDecisionHistory::new();
            history.add_entry(JointDecisionHistoryEntry::new(
                1,
                Utc::now(),
                JointDecisionAction::EscalateWarning,
                3,
                2,
                0,
                false,
                vec![EvaluatorType::CacheTuner, EvaluatorType::SearchQuality],
            ));
            history.save_to_workspace(dir.path()).unwrap();
        }

        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_cache_tuner(CacheTuner::with_default_config(1800))
            .with_dev_trace(true)
            .with_joint_decision_engine(crate::joint_decision::JointDecisionEngine::default());
        setup_test_phase(&mut orch);

        orch.final_report().unwrap();

        // 验证 HTML 包含联合决策面板
        let html_path = dir.path().join(".forge").join("devtrace_report.html");
        assert!(html_path.exists());
        let html_content = std::fs::read_to_string(&html_path).unwrap();
        assert!(
            html_content.contains("联合决策历史"),
            "HTML 应包含联合决策历史面板"
        );
    }

    #[test]
    fn test_evaluate_joint_decision_with_engine() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
        let chat = MockChatClient::new(vec![]);

        let mut orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_cache_tuner(CacheTuner::with_default_config(1800))
            .with_search_quality_evaluator(SearchQualityEvaluator::with_default_config())
            .with_memory_evaluator(
                crate::memory_evaluation::MemoryContextEvaluator::with_default_config(),
            )
            .with_dev_trace(true)
            .with_joint_decision_engine(crate::joint_decision::JointDecisionEngine::default());
        setup_test_phase(&mut orch);

        // 调用 evaluate_joint_decision (模拟编译检查后调用)
        orch.evaluate_joint_decision(0, 0);

        // 验证 DevTrace 记录了联合决策
        if let Some(ref trace) = orch.dev_trace {
            let entries = trace.read_all().unwrap_or_default();
            let jd_entries: Vec<_> = entries
                .iter()
                .filter(|e| e.action == TraceAction::JointDecision)
                .collect();
            assert!(!jd_entries.is_empty(), "DevTrace 应记录联合决策条目");
        }
    }

    // ===== Session 118: 自动修复 (apply_fixes) 集成测试 =====

    #[test]
    fn test_auto_fix_disabled_by_default() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let orch = make_orchestrator(&chat, dir.path().to_str().unwrap());
        assert!(!orch.auto_fix_enabled, "auto_fix_enabled 应默认为 false");
    }

    #[test]
    fn test_with_auto_fix_enables() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let orch = make_orchestrator(&chat, dir.path().to_str().unwrap()).with_auto_fix(true);
        assert!(
            orch.auto_fix_enabled,
            "with_auto_fix(true) 应设置 auto_fix_enabled 为 true"
        );
    }

    #[test]
    fn test_with_auto_fix_disabled() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let orch = make_orchestrator(&chat, dir.path().to_str().unwrap()).with_auto_fix(false);
        assert!(
            !orch.auto_fix_enabled,
            "with_auto_fix(false) 应设置 auto_fix_enabled 为 false"
        );
    }

    #[test]
    fn test_apply_auto_fixes_to_files_fixes_rust() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let orch = make_orchestrator(&chat, dir.path().to_str().unwrap());

        let files = vec![crate::extract::ExtractedFile {
            path: "src/main.rs".to_string(),
            content: "fn foo() { let x = bar().unwrap(); }".to_string(),
            language: "rust".to_string(),
        }];

        let fixed = orch.apply_auto_fixes_to_files(files);
        assert_eq!(fixed.len(), 1);
        assert!(
            !fixed[0].content.contains(".unwrap()"),
            "Rust 文件中的 unwrap() 应被修复"
        );
        assert!(fixed[0].content.contains('?'), "应包含 ? 操作符");
    }

    #[test]
    fn test_apply_auto_fixes_to_files_skips_non_rust() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let orch = make_orchestrator(&chat, dir.path().to_str().unwrap());

        let files = vec![crate::extract::ExtractedFile {
            path: "Cargo.toml".to_string(),
            content: "[package]\nname = \"test\"\nversion = \"0.1.0\"".to_string(),
            language: "toml".to_string(),
        }];

        let fixed = orch.apply_auto_fixes_to_files(files);
        assert_eq!(fixed.len(), 1);
        // 非 Rust 文件不应被修改
        assert!(fixed[0].content.contains("[package]"));
    }

    #[test]
    fn test_apply_auto_fixes_to_files_mixed() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let orch = make_orchestrator(&chat, dir.path().to_str().unwrap());

        let files = vec![
            crate::extract::ExtractedFile {
                path: "src/main.rs".to_string(),
                content: "fn foo() { let x = bar().unwrap(); }".to_string(),
                language: "rust".to_string(),
            },
            crate::extract::ExtractedFile {
                path: "Cargo.toml".to_string(),
                content: "[package]".to_string(),
                language: "toml".to_string(),
            },
            crate::extract::ExtractedFile {
                path: "src/lib.rs".to_string(),
                content: "pub fn bar() -> bool { true }".to_string(),
                language: "rust".to_string(),
            },
        ];

        let fixed = orch.apply_auto_fixes_to_files(files);
        assert_eq!(fixed.len(), 3);
        // 第一个 Rust 文件应被修复
        assert!(!fixed[0].content.contains(".unwrap()"));
        // TOML 文件不应被修改
        assert!(fixed[1].content.contains("[package]"));
        // 第三个 Rust 文件应被修复 (添加 #[must_use] 和文档注释)
        assert!(
            fixed[2].content.contains("#[must_use]") || fixed[2].content.contains("/// TODO:"),
            "第三个 Rust 文件应被修复: {}",
            fixed[2].content
        );
    }

    #[test]
    fn test_apply_auto_fixes_to_files_no_issues() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let orch = make_orchestrator(&chat, dir.path().to_str().unwrap());

        let files = vec![crate::extract::ExtractedFile {
            path: "src/main.rs".to_string(),
            content: "fn foo() -> i32 { 42 }".to_string(),
            language: "rust".to_string(),
        }];

        let fixed = orch.apply_auto_fixes_to_files(files);
        assert_eq!(fixed.len(), 1);
        assert_eq!(
            fixed[0].content, "fn foo() -> i32 { 42 }",
            "无问题的代码不应被修改"
        );
    }

    #[test]
    fn test_auto_fix_preserves_file_path_and_language() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let orch = make_orchestrator(&chat, dir.path().to_str().unwrap());

        let files = vec![crate::extract::ExtractedFile {
            path: "src/utils/helper.rs".to_string(),
            content: "fn foo() { let x = bar().unwrap(); }".to_string(),
            language: "rust".to_string(),
        }];

        let fixed = orch.apply_auto_fixes_to_files(files);
        assert_eq!(fixed[0].path, "src/utils/helper.rs", "路径应保留");
        assert_eq!(fixed[0].language, "rust", "语言应保留");
    }

    // ===== Session 120: clippy 检查集成测试 =====

    #[test]
    fn test_clippy_check_disabled_by_default() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let orch = make_orchestrator(&chat, dir.path().to_str().unwrap());
        assert!(
            !orch.clippy_check_enabled,
            "clippy_check_enabled 应默认为 false"
        );
    }

    #[test]
    fn test_with_clippy_check_enables() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let orch = make_orchestrator(&chat, dir.path().to_str().unwrap()).with_clippy_check(true);
        assert!(
            orch.clippy_check_enabled,
            "with_clippy_check(true) 应设置 clippy_check_enabled 为 true"
        );
    }

    #[test]
    fn test_with_clippy_check_disabled() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let orch = make_orchestrator(&chat, dir.path().to_str().unwrap()).with_clippy_check(false);
        assert!(
            !orch.clippy_check_enabled,
            "with_clippy_check(false) 应设置 clippy_check_enabled 为 false"
        );
    }

    #[test]
    fn test_with_clippy_check_and_auto_fix_combined() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_auto_fix(true)
            .with_clippy_check(true);
        assert!(orch.auto_fix_enabled, "auto_fix 应启用");
        assert!(orch.clippy_check_enabled, "clippy_check 应启用");
    }

    // ===== Session 121: 分阶段修复测试 =====

    #[test]
    fn test_staged_fix_disabled_by_default() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let orch = make_orchestrator(&chat, dir.path().to_str().unwrap());
        assert!(
            !orch.staged_fix_enabled,
            "staged_fix_enabled 应默认为 false"
        );
    }

    #[test]
    fn test_with_staged_fix_enables() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let orch = make_orchestrator(&chat, dir.path().to_str().unwrap()).with_staged_fix(true);
        assert!(
            orch.staged_fix_enabled,
            "with_staged_fix(true) 应设置 staged_fix_enabled 为 true"
        );
    }

    #[test]
    fn test_with_staged_fix_disabled() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let orch = make_orchestrator(&chat, dir.path().to_str().unwrap()).with_staged_fix(false);
        assert!(
            !orch.staged_fix_enabled,
            "with_staged_fix(false) 应设置 staged_fix_enabled 为 false"
        );
    }

    #[test]
    fn test_staged_fix_with_auto_fix_combined() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_auto_fix(true)
            .with_staged_fix(true)
            .with_clippy_check(true);
        assert!(orch.auto_fix_enabled, "auto_fix 应启用");
        assert!(orch.staged_fix_enabled, "staged_fix 应启用");
        assert!(orch.clippy_check_enabled, "clippy_check 应启用");
    }

    #[test]
    fn test_apply_auto_fixes_staged_mode() {
        use crate::extract::ExtractedFile;
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_auto_fix(true)
            .with_staged_fix(true);

        let files = vec![ExtractedFile {
            path: "src/main.rs".to_string(),
            content: "pub fn foo() { let x = bar().unwrap(); }".to_string(),
            language: "rust".to_string(),
        }];

        let fixed = orch.apply_auto_fixes_to_files(files);
        assert!(
            !fixed[0].content.contains(".unwrap()"),
            "分阶段模式应修复 unwrap"
        );
        assert!(
            fixed[0].content.contains("#[must_use]"),
            "分阶段模式应添加 #[must_use]"
        );
    }

    // ===== Session 123: 修复预览测试 =====

    #[test]
    fn test_fix_preview_disabled_by_default() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let orch = make_orchestrator(&chat, dir.path().to_str().unwrap());
        assert!(
            !orch.fix_preview_enabled,
            "fix_preview_enabled 应默认为 false"
        );
    }

    #[test]
    fn test_with_fix_preview_enables() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let orch = make_orchestrator(&chat, dir.path().to_str().unwrap()).with_fix_preview(true);
        assert!(
            orch.fix_preview_enabled,
            "with_fix_preview(true) 应设置 fix_preview_enabled 为 true"
        );
    }

    #[test]
    fn test_with_fix_preview_disabled() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let orch = make_orchestrator(&chat, dir.path().to_str().unwrap()).with_fix_preview(false);
        assert!(
            !orch.fix_preview_enabled,
            "with_fix_preview(false) 应设置 fix_preview_enabled 为 false"
        );
    }

    #[test]
    fn test_fix_preview_mode_does_not_modify_files() {
        use crate::extract::ExtractedFile;
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_auto_fix(true)
            .with_staged_fix(true)
            .with_fix_preview(true);

        let original_content = "pub fn foo() { let x = bar().unwrap(); }";
        let files = vec![ExtractedFile {
            path: "src/main.rs".to_string(),
            content: original_content.to_string(),
            language: "rust".to_string(),
        }];

        let fixed = orch.apply_auto_fixes_to_files(files);
        // 预览模式不应修改文件内容
        assert_eq!(
            fixed[0].content, original_content,
            "预览模式不应修改文件内容"
        );
    }

    #[test]
    fn test_fix_preview_with_auto_fix_and_staged_fix() {
        let dir = tempdir().unwrap();
        let chat = MockChatClient::new(vec![]);
        let orch = make_orchestrator(&chat, dir.path().to_str().unwrap())
            .with_auto_fix(true)
            .with_staged_fix(true)
            .with_fix_preview(true)
            .with_clippy_check(true);
        assert!(orch.auto_fix_enabled, "auto_fix 应启用");
        assert!(orch.staged_fix_enabled, "staged_fix 应启用");
        assert!(orch.fix_preview_enabled, "fix_preview 应启用");
        assert!(orch.clippy_check_enabled, "clippy_check 应启用");
    }
}

// ============================================================================
//  ds4 风格上下文压缩集成 — Session 73
// ============================================================================

#[allow(clippy::items_after_test_module)]
impl<'a, C, T, E, Q> Orchestrator<'a, C, T, E, Q>
where
    C: ChatClient,
    T: TestRunner,
    E: FileExtractor,
    Q: ClarificationChecker,
{
    /// 检查是否需要触发上下文压缩 — 借鉴 ds4 COMPACT.md 触发逻辑
    ///
    /// 返回 Some(trigger) 如果应该触发压缩，None 表示不需要压缩。
    fn check_compaction_trigger(
        &self,
        config: &crate::context_handoff::CompactionConfig,
    ) -> Result<Option<crate::context_handoff::CompactionTrigger>> {
        // 估算上下文窗口大小（如果没有提供）
        let context_window = self.estimate_context_window();
        let current_tokens = self.chat.conversation_token_count();

        let trigger = crate::context_handoff::check_compaction_trigger(
            current_tokens,
            context_window,
            config,
        );

        if trigger != crate::context_handoff::CompactionTrigger::None {
            info!(
                "🔄 上下文压缩触发: {:?} (当前 {} tokens, 窗口 {} tokens)",
                trigger, current_tokens, context_window
            );
        }

        Ok(
            if trigger == crate::context_handoff::CompactionTrigger::None {
                None
            } else {
                Some(trigger)
            },
        )
    }

    /// 执行上下文压缩 — 发送摘要 prompt → 新开对话 → 发送压缩上下文
    ///
    /// 借鉴 ds4 的压缩流程:
    /// 1. 构建压缩摘要 prompt (要求 AI 总结关键状态)
    /// 2. 发送摘要 prompt 并等待 AI 回复
    /// 3. 新开对话
    /// 4. 发送 AI 生成的摘要作为新对话的上下文
    /// 5. 继续执行
    async fn execute_context_compaction(
        &mut self,
        trigger: crate::context_handoff::CompactionTrigger,
        _config: &crate::context_handoff::CompactionConfig,
    ) -> Result<()> {
        info!("🔄 执行上下文压缩: {:?} (ds4 风格)", trigger);
        println!("\n  🔄 上下文压缩: {:?} 触发, 生成会话摘要...", trigger);

        // 1. 构建压缩摘要 prompt
        let summary_prompt = crate::context_handoff::build_compaction_summary_prompt();

        // 2. 发送摘要 prompt 并等待 AI 回复
        let current_phase = self.memory.current_phase;
        let current_task = self.memory.current_task.clone();
        let summary_start = Instant::now();
        let summary_result = self
            .send_message_safe(summary_prompt, self.timeout_secs)
            .await?;

        if summary_result.timed_out {
            warn!("⚠️ 上下文压缩摘要生成超时, 继续执行");
        }

        let summary_duration = summary_start.elapsed().as_millis() as u64;

        // 记录压缩对话
        self.memory
            .add_conversation("user", "[上下文压缩摘要请求]", current_task.as_deref());
        self.memory
            .add_conversation("assistant", &summary_result.text, current_task.as_deref());

        // === DevTrace: 上下文压缩 (ds4 风格) ===
        self.trace_dev(
            TraceAction::ContextHandoff,
            Some(current_phase),
            None,
            None,
            "[上下文压缩摘要请求]",
            &summary_result.text,
            summary_duration,
            !summary_result.timed_out,
            None,
        );

        // 3. 新开对话
        self.chat.start_new_conversation().await?;

        // 3.5 重置增量跟踪状态 — 压缩后新开对话, 之前发送的消息不再有效
        if self.live_continuation.is_some() || self.conversation_tracker.is_some() {
            info!("🔄 重置增量跟踪状态 (上下文压缩)");
            self.reset_continuation();
        }

        // 4. 发送 AI 生成的摘要作为新对话的上下文
        let handoff_prompt = format!("# 开发会话摘要\n\n{}", summary_result.text);

        let handoff_start = Instant::now();
        let handoff_result = self
            .send_message_safe(&handoff_prompt, self.timeout_secs)
            .await?;
        let handoff_duration = handoff_start.elapsed().as_millis() as u64;

        // === DevTrace: 压缩上下文传递 ===
        self.trace_dev(
            TraceAction::ContextHandoff,
            Some(current_phase),
            None,
            None,
            &format!("[压缩上下文 - {}字符]", handoff_prompt.len()),
            &handoff_result.text,
            handoff_duration,
            !handoff_result.timed_out,
            None,
        );

        // 5. 记录压缩交接决策
        self.memory.add_decision(
            current_phase,
            current_task.as_deref(),
            "上下文压缩",
            &format!(
                "触发 {:?} 压缩 (当前 {} tokens), 新开对话并传递摘要",
                trigger,
                self.chat.conversation_token_count()
            ),
        );

        // 记录交接对话
        self.memory.add_conversation(
            "user",
            &format!("[压缩上下文 - {}字符]", handoff_prompt.len()),
            current_task.as_deref(),
        );
        self.memory
            .add_conversation("assistant", &handoff_result.text, current_task.as_deref());

        self.save_memory();

        println!("  ✅ 上下文压缩完成, token 计数已重置");
        Ok(())
    }

    /// 估算当前模型的上下文窗口大小
    ///
    /// 根据不同网站类型估算上下文窗口:
    /// - Z.ai: 128K tokens (DeepSeek V3)
    /// - DeepSeek: 64K tokens (官方文档)
    /// - Kimi: 128K tokens (官方文档)
    /// - 通义千问: 128K tokens (官方文档)
    /// - Claude.ai: 200K tokens (Claude 3.5 Sonnet)
    /// - 其他: 32K tokens (保守估计)
    fn estimate_context_window(&self) -> usize {
        // 尝试从 chat 客户端获取网站类型信息
        // 注意: 这里需要从 ChatClient trait 获取，但 trait 没有 site_type 方法
        // 所以我们使用一个合理的默认值

        // TODO: 在 ChatClient trait 中添加 site_type() 方法以支持更精确的估算
        128000 // 默认 128K tokens，适用于大多数现代模型
    }
}
