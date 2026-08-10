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
use crate::clarify::HeuristicClarificationChecker;
use crate::connection_monitor::ConnectionMonitor;
use crate::context_handoff::ContextHandoff;
use crate::dev_trace::{DevTraceWriter, TraceAction};
use crate::error_diagnosis::{DiagnosisContext, DiagnosisResult, ErrorDiagnoser, ErrorHistory};
use crate::interaction::AutoApprove;
use crate::loop_detector::LoopDetector;
use crate::memory::{Memory, Phase, PhaseStatus, Task, TaskStatus};
use crate::prompt_builder::SystemPrompt;
use crate::response_handler::{HandlerChain, TaskContext};
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
            live_continuation: None,
            conversation_tracker: None,
            incremental_stats: crate::dev_trace::IncrementalStats::new(),
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
    fn final_report(&self) -> Result<()> {
        println!("\n{}", "═".repeat(60));
        println!("{}", self.memory.execution_report());
        println!("{}", "═".repeat(60));

        // 保存报告到工作区
        let report_path = self.workspace.root.join("FORGE_REPORT.md");
        std::fs::write(&report_path, self.memory.execution_report())?;
        println!("报告已保存: {}", report_path.display());

        // === DevTrace: 打印追踪摘要 (借鉴方向 4) ===
        if let Some(ref trace) = self.dev_trace {
            let summary = trace.summary();
            println!("\n{}", summary.to_report());
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

        // === 网络错误处理常量 (orchestrator 层面) ===
        // run_cargo 已重试 3 次 (5s 间隔), orchestrator 层面再重试 3 次 (30s 间隔)
        // 如果仍然失败, 跳过 AI 修复 (不消耗修复轮次), 最多跳过 5 次
        const MAX_ORCH_NETWORK_RETRIES: u32 = 3;
        const ORCH_NETWORK_RETRY_INTERVAL: u64 = 30;
        const MAX_NETWORK_ERROR_SKIPS: u32 = 5;
        let mut network_error_skips: u32 = 0;

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
                if let Some(ref diagnosis) = last_diagnosis {
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
            let msg_start = Instant::now();
            let result = self
                .send_message_safe(&steered_prompt, self.timeout_secs)
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
