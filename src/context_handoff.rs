//! 上下文衔接机制 — 借鉴方向 1
//!
//! 当网页 AI 对话过长时, 自动新开对话并将上下文衔接上去,
//! 使 Forge 能够 24 小时不间断运行而不被上下文窗口限制。
//!
//! ## 核心思路
//!
//! 网页 AI 的上下文窗口有限 (32K-128K token), 24 小时不间断运行时对话会无限增长。
//! 解决方案不是压缩上下文, 而是在对话过长时新开一个对话 (网页上新建聊天),
//! 将上下文衔接上去 — 这正是人类在对话太长时的做法。
//!
//! ## 流程
//!
//! ```text
//! 检测对话轮数 > 阈值
//!   → 构建 ContextHandoff (从 memory.json + workspace + error_history 提取)
//!   → ChatClient::start_new_conversation() (CDP 导航到 chat.z.ai/)
//!   → 发送交接 prompt 作为新对话的第一条消息
//!   → AI 回复后, 对话轮数清零, 继续执行当前任务
//! ```

use crate::error_diagnosis::ErrorHistory;
use crate::memory::{Memory, Phase, Task, TaskStatus};
use crate::workspace::Workspace;

// ============================================================================
//  摘要数据结构
// ============================================================================

/// 阶段摘要 — 用于交接 prompt
#[derive(Debug, Clone)]
pub struct PhaseSummary {
    /// 阶段名称
    pub name: String,
    /// 阶段描述
    pub description: String,
    /// 阶段状态
    pub status: String,
    /// 阶段内任务数
    pub task_count: usize,
    /// 已完成任务数
    pub completed_count: usize,
}

/// 任务摘要 — 用于交接 prompt
#[derive(Debug, Clone)]
pub struct TaskSummary {
    /// 任务 ID
    pub id: String,
    /// 任务名称
    pub name: String,
    /// 任务状态
    pub status: String,
    /// 任务结果摘要 (如有)
    pub result: Option<String>,
    /// 已写入文件
    pub files_written: Vec<String>,
    /// 尝试次数
    pub attempts: u32,
}

/// 文件摘要 — 工作区文件列表
#[derive(Debug, Clone)]
pub struct FileSummary {
    /// 文件路径 (相对工作区根目录)
    pub path: String,
    /// 文件大小 (字节数)
    pub size: usize,
}

/// 错误摘要 — 最近一轮编译/测试错误
#[derive(Debug, Clone)]
pub struct ErrorSummary {
    /// 错误文件
    pub file: String,
    /// 错误消息
    pub message: String,
    /// 错误码 (如有)
    pub error_code: Option<String>,
}

// ============================================================================
//  纯逻辑函数 — 可独立测试, 不依赖外部状态
// ============================================================================

/// 判断工作区文件路径是否应包含在交接上下文中 (纯逻辑)
///
/// 排除 `target/` 和 `.forge/` 目录下的文件。
///
/// # 示例
///
/// ```
/// # use forge::context_handoff::is_workspace_file_included;
/// assert!(is_workspace_file_included("src/main.rs"));
/// assert!(!is_workspace_file_included("target/debug/output"));
/// ```
pub fn is_workspace_file_included(path: &str) -> bool {
    !path.starts_with("target/") && !path.starts_with(".forge/")
}

/// 截断文本到指定字符数 (纯逻辑)
///
/// 按 Unicode 字符 (而非字节) 截断, 确保不截断多字节字符。
///
/// # 示例
///
/// ```
/// # use forge::context_handoff::truncate_text;
/// assert_eq!(truncate_text("hello", 3), "hel");
/// assert_eq!(truncate_text("ab", 10), "ab");
/// assert_eq!(truncate_text("", 5), "");
/// ```
pub fn truncate_text(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

/// 判断是否应触发上下文衔接 (纯逻辑)
///
/// 当对话轮数达到或超过阈值时返回 `true`。
///
/// # 示例
///
/// ```
/// # use forge::context_handoff::should_trigger_handoff;
/// assert!(!should_trigger_handoff(5, 10));
/// assert!(should_trigger_handoff(10, 10));
/// assert!(should_trigger_handoff(15, 10));
/// ```
pub fn should_trigger_handoff(conversation_count: usize, threshold: usize) -> bool {
    conversation_count >= threshold
}

/// 格式化错误码徽章 (纯逻辑)
///
/// `Some("E0308")` → `"[E0308]"`, `None` → `""`
///
/// # 示例
///
/// ```
/// # use forge::context_handoff::format_error_code_badge;
/// assert_eq!(format_error_code_badge(Some("E0308")), "[E0308]");
/// assert_eq!(format_error_code_badge(None), "");
/// ```
pub fn format_error_code_badge(code: Option<&str>) -> String {
    code.map(|c| format!("[{}]", c)).unwrap_or_default()
}

/// 从 `Phase` 构建 `PhaseSummary` (纯逻辑)
///
/// 提取阶段名称、描述、状态字符串, 并统计任务总数和已完成数。
pub fn build_phase_summary(phase: &Phase) -> PhaseSummary {
    PhaseSummary {
        name: phase.name.clone(),
        description: phase.description.clone(),
        status: format!("{:?}", phase.status),
        task_count: phase.tasks.len(),
        completed_count: phase
            .tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Completed)
            .count(),
    }
}

/// 从 `Task` 构建 `TaskSummary` (纯逻辑)
///
/// 提取任务 ID、名称、状态字符串、结果、已写入文件和尝试次数。
pub fn build_task_summary(task: &Task) -> TaskSummary {
    TaskSummary {
        id: task.id.clone(),
        name: task.name.clone(),
        status: format!("{:?}", task.status),
        result: task.result.clone(),
        files_written: task.files_written.clone(),
        attempts: task.attempts,
    }
}

/// 收集所有阶段中已完成任务 (纯逻辑)
///
/// 遍历所有阶段的所有任务, 筛选 `TaskStatus::Completed`, 转换为 `TaskSummary`。
pub fn collect_completed_tasks(phases: &[Phase]) -> Vec<TaskSummary> {
    phases
        .iter()
        .flat_map(|p| &p.tasks)
        .filter(|t| t.status == TaskStatus::Completed)
        .map(build_task_summary)
        .collect()
}

/// 格式化当前阶段为 prompt 片段 (纯逻辑)
///
/// 返回形如 `【当前阶段】\n名称 — 描述\n状态: ... | 进度: 1/2 任务完成\n\n` 的文本。
pub fn format_phase_section(phase: &PhaseSummary) -> String {
    format!(
        "【当前阶段】\n{} — {}\n状态: {} | 进度: {}/{} 任务完成\n\n",
        phase.name, phase.description, phase.status, phase.completed_count, phase.task_count
    )
}

/// 格式化当前任务为 prompt 片段 (纯逻辑)
///
/// 包含 ID、名称、状态、尝试次数, 可选的已写入文件和结果。
pub fn format_task_section(task: &TaskSummary) -> String {
    let mut section = format!(
        "【当前任务】\nID: {}\n名称: {}\n状态: {}\n尝试: {} 次\n",
        task.id, task.name, task.status, task.attempts
    );
    if !task.files_written.is_empty() {
        section.push_str(&format!("已写入文件: {}\n", task.files_written.join(", ")));
    }
    if let Some(result) = &task.result {
        section.push_str(&format!("结果: {}\n", result));
    }
    section.push('\n');
    section
}

/// 格式化已完成任务列表为 prompt 片段, 最多显示 10 个 (纯逻辑)
///
/// 超过 10 个时追加 `... 还有 N 个` 提示。空列表返回空字符串。
pub fn format_completed_tasks_section(tasks: &[TaskSummary]) -> String {
    if tasks.is_empty() {
        return String::new();
    }
    let mut section = format!("【已完成任务】({})\n", tasks.len());
    for t in tasks.iter().take(10) {
        section.push_str(&format!(
            "  ✅ [{}] {} → {}\n",
            t.id,
            t.name,
            t.files_written.join(", ")
        ));
    }
    if tasks.len() > 10 {
        section.push_str(&format!("  ... 还有 {} 个\n", tasks.len() - 10));
    }
    section.push('\n');
    section
}

/// 格式化工作区文件列表为 prompt 片段, 最多显示 20 个 (纯逻辑)
///
/// 超过 20 个时追加 `... 还有 N 个文件` 提示。空列表返回空字符串。
pub fn format_workspace_files_section(files: &[FileSummary]) -> String {
    if files.is_empty() {
        return String::new();
    }
    let mut section = format!("【当前项目文件】({})\n", files.len());
    for f in files.iter().take(20) {
        section.push_str(&format!("  {} ({}字节)\n", f.path, f.size));
    }
    if files.len() > 20 {
        section.push_str(&format!("  ... 还有 {} 个文件\n", files.len() - 20));
    }
    section.push('\n');
    section
}

/// 格式化最近错误列表为 prompt 片段 (纯逻辑)
///
/// 每条错误显示文件、错误码徽章和消息。空列表返回空字符串。
pub fn format_recent_errors_section(errors: &[ErrorSummary]) -> String {
    if errors.is_empty() {
        return String::new();
    }
    let mut section = String::from("【最近编译/测试错误】\n");
    for err in errors {
        let badge = format_error_code_badge(err.error_code.as_deref());
        section.push_str(&format!("  {} {}: {}\n", err.file, badge, err.message));
    }
    section.push('\n');
    section
}

/// 格式化错误历史摘要为 prompt 片段 (纯逻辑)
///
/// 空摘要返回空字符串, 非空则包装为 `【错误历史摘要】(Top 5 模式)\n...`。
pub fn format_error_history_section(summary: &str) -> String {
    if summary.is_empty() {
        return String::new();
    }
    format!("【错误历史摘要】(Top 5 模式)\n{}\n", summary)
}

/// 格式化已知问题为 prompt 片段 (纯逻辑)
///
/// 空字符串返回空字符串, 非空则包装为 `【已知问题】\n...`。
pub fn format_known_issues_section(issues: &str) -> String {
    if issues.is_empty() {
        return String::new();
    }
    format!("【已知问题】\n{}\n", issues)
}

// ============================================================================
//  ContextHandoff — 交接上下文构建器
// ============================================================================

/// 上下文衔接 — 包含新对话所需的所有状态信息
///
/// 从 `Memory`、`Workspace`、`ErrorHistory` 构建,
/// 生成自包含的交接 prompt, 使新对话中的 AI 能无缝继续开发。
#[derive(Debug, Clone)]
pub struct ContextHandoff {
    /// 项目终极目标
    pub goal: String,

    /// 当前阶段摘要
    pub current_phase: Option<PhaseSummary>,

    /// 当前任务摘要 (正在执行的任务)
    pub current_task: Option<TaskSummary>,

    /// 已完成的所有任务列表 (跨所有阶段)
    pub completed_tasks: Vec<TaskSummary>,

    /// 当前工作区文件列表
    pub workspace_files: Vec<FileSummary>,

    /// 最近一轮的编译/测试错误 (如有)
    pub recent_errors: Vec<ErrorSummary>,

    /// 错误历史摘要 (从 error_history.json 提取 top 5 模式)
    pub error_history_summary: String,

    /// 架构约束和编码规范
    pub architecture_constraints: String,

    /// 已知问题和待解决项
    pub known_issues: String,
}

impl ContextHandoff {
    /// 从 Memory、Workspace 和 ErrorHistory 构建交接上下文
    pub fn build_from_memory(
        memory: &Memory,
        workspace: &Workspace,
        error_history: &ErrorHistory,
    ) -> Self {
        // 当前阶段
        let current_phase = memory.current_phase().map(build_phase_summary);

        // 当前任务
        let current_task = memory.current_task.as_ref().and_then(|task_id| {
            memory
                .phases
                .iter()
                .flat_map(|p| &p.tasks)
                .find(|t| &t.id == task_id)
                .map(build_task_summary)
        });

        // 已完成的所有任务 (跨所有阶段)
        let completed_tasks = collect_completed_tasks(&memory.phases);

        // 工作区文件列表
        let workspace_files = workspace
            .list_files()
            .unwrap_or_default()
            .into_iter()
            .filter(|f| is_workspace_file_included(f))
            .map(|path| {
                let full = workspace.root.join(&path);
                let size = std::fs::metadata(&full)
                    .map(|m| m.len() as usize)
                    .unwrap_or(0);
                FileSummary { path, size }
            })
            .collect::<Vec<_>>();

        // 最近一轮的编译/测试错误
        let recent_errors = Self::extract_recent_errors(memory);

        // 错误历史摘要
        let error_history_summary = Self::build_error_history_summary(error_history);

        // 架构约束
        let architecture_constraints = Self::default_architecture_constraints();

        // 已知问题
        let known_issues = Self::extract_known_issues(memory);

        Self {
            goal: memory.goal.clone(),
            current_phase,
            current_task,
            completed_tasks,
            workspace_files,
            recent_errors,
            error_history_summary,
            architecture_constraints,
            known_issues,
        }
    }

    /// 从 memory 中提取最近一轮的编译/测试错误
    fn extract_recent_errors(memory: &Memory) -> Vec<ErrorSummary> {
        // 从当前任务的 test_result 中提取错误信息
        if let Some(task_id) = &memory.current_task {
            if let Some(task) = memory
                .phases
                .iter()
                .flat_map(|p| &p.tasks)
                .find(|t| &t.id == task_id)
            {
                if let Some(test_result) = &task.test_result {
                    // 如果 test_result 包含 "编译失败" 或 "测试失败"
                    if test_result.contains("失败") || test_result.contains("error") {
                        // 提取最近一条错误 (简化版: 从 test_result 文本中提取)
                        return vec![ErrorSummary {
                            file: "(见 test_result)".to_string(),
                            message: truncate_text(test_result, 300),
                            error_code: None,
                        }];
                    }
                }
            }
        }
        vec![]
    }

    /// 从 error_history 中构建摘要 (top 5 模式)
    fn build_error_history_summary(history: &ErrorHistory) -> String {
        if history.patterns.is_empty() {
            return String::new();
        }

        // 按出现次数排序, 取 top 5
        let mut sorted: Vec<_> = history.patterns.iter().collect();
        sorted.sort_by_key(|p| std::cmp::Reverse(p.occurrences));

        let mut summary = String::new();
        for (i, p) in sorted.iter().take(5).enumerate() {
            let code = p.error_code.as_deref().unwrap_or("N/A");
            let status = if p.last_fix_succeeded {
                "已修复"
            } else {
                "未修复"
            };
            summary.push_str(&format!(
                "  {}. [{}] {} (出现{}次, {})\n",
                i + 1,
                p.category,
                code,
                p.occurrences,
                status,
            ));
        }
        summary
    }

    /// 默认架构约束
    fn default_architecture_constraints() -> String {
        "- 遵循 SOLID 原则, 特别是 DIP (依赖倒置)\n\
         - 核心逻辑依赖 trait 抽象, 不依赖具体类型\n\
         - 每个新功能必须有配套的单元测试和集成测试 (TDD)\n\
         - 代码必须可编译、可测试\n\
         - 用 ```file:路径``` 格式输出所有文件"
            .to_string()
    }

    /// 从 memory 中提取已知问题
    fn extract_known_issues(memory: &Memory) -> String {
        let mut issues = String::new();

        // 未完成的任务
        let pending: Vec<&Task> = memory
            .phases
            .iter()
            .flat_map(|p| &p.tasks)
            .filter(|t| t.status != TaskStatus::Completed)
            .collect();

        if !pending.is_empty() {
            issues.push_str(&format!("待完成任务 ({}):\n", pending.len()));
            for t in &pending {
                issues.push_str(&format!(
                    "  - [{}] {} (状态: {:?}, 尝试: {}次)\n",
                    t.id, t.name, t.status, t.attempts
                ));
            }
        }

        // 待处理需求变更
        if memory.has_pending_changes() {
            issues.push_str(&format!(
                "\n待处理需求变更: {} 项\n",
                memory.pending_changes().len()
            ));
        }

        issues
    }

    /// 构建完整的交接 prompt
    ///
    /// 生成的 prompt 自包含所有必要信息, 使新对话中的 AI 能无缝继续开发。
    /// 控制在 2000-3000 token 以内。
    pub fn to_prompt(&self) -> String {
        let mut prompt = String::new();

        // 标题
        prompt.push_str("═══════════════════════════════════════════════════\n");
        prompt.push_str("  📋 上下文衔接 — 我们之前在做什么\n");
        prompt.push_str("═══════════════════════════════════════════════════\n\n");

        // 1. 终极目标
        prompt.push_str("【项目终极目标】\n");
        prompt.push_str(&self.goal);
        prompt.push_str("\n\n");

        // 2. 当前阶段
        if let Some(phase) = &self.current_phase {
            prompt.push_str(&format_phase_section(phase));
        }

        // 3. 当前任务
        if let Some(task) = &self.current_task {
            prompt.push_str(&format_task_section(task));
        }

        // 4. 已完成任务
        prompt.push_str(&format_completed_tasks_section(&self.completed_tasks));

        // 5. 工作区文件
        prompt.push_str(&format_workspace_files_section(&self.workspace_files));

        // 6. 最近错误
        prompt.push_str(&format_recent_errors_section(&self.recent_errors));

        // 7. 错误历史
        prompt.push_str(&format_error_history_section(&self.error_history_summary));

        // 8. 架构约束
        prompt.push_str("【架构约束】\n");
        prompt.push_str(&self.architecture_constraints);
        prompt.push('\n');

        // 9. 已知问题
        prompt.push_str(&format_known_issues_section(&self.known_issues));

        // 结束语
        prompt.push_str("═══════════════════════════════════════════════════\n");
        prompt.push_str("请基于以上上下文, 继续执行当前任务。\n");
        prompt.push_str("用 ```file:路径``` 格式输出所有文件, 输出完整文件内容。\n");

        prompt
    }
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error_diagnosis::{ErrorCategory, ErrorHistory, ErrorPattern};
    use crate::memory::{Memory, Phase, PhaseStatus, Task, TaskStatus};
    use crate::workspace::Workspace;
    use chrono::Utc;
    use tempfile::tempdir;

    /// 构建一个包含完整状态的测试 Memory
    fn make_full_memory() -> Memory {
        let mut mem = Memory::new("构建一个 CLI 计算器");
        mem.set_phases(vec![
            Phase {
                id: 0,
                name: "初始化".to_string(),
                description: "创建项目结构".to_string(),
                status: PhaseStatus::Completed,
                tasks: vec![Task {
                    id: "0-0".to_string(),
                    phase_id: 0,
                    name: "初始化项目".to_string(),
                    prompt: "创建 Cargo.toml 和 main.rs".to_string(),
                    status: TaskStatus::Completed,
                    result: Some("成功".to_string()),
                    attempts: 1,
                    files_written: vec!["Cargo.toml".to_string(), "src/main.rs".to_string()],
                    test_result: Some("✅ 编译成功".to_string()),
                    last_good_snapshot: Some(1),
                    clarifications: vec![],
                    depends_on: vec![],
                }],
            },
            Phase {
                id: 1,
                name: "功能实现".to_string(),
                description: "实现核心功能".to_string(),
                status: PhaseStatus::InProgress,
                tasks: vec![
                    Task {
                        id: "1-0".to_string(),
                        phase_id: 1,
                        name: "解析输入".to_string(),
                        prompt: "实现参数解析".to_string(),
                        status: TaskStatus::Completed,
                        result: Some("成功".to_string()),
                        attempts: 2,
                        files_written: vec!["src/parser.rs".to_string()],
                        test_result: Some("✅ 测试通过".to_string()),
                        last_good_snapshot: Some(3),
                        clarifications: vec![],
                        depends_on: vec![],
                    },
                    Task {
                        id: "1-1".to_string(),
                        phase_id: 1,
                        name: "计算逻辑".to_string(),
                        prompt: "实现计算功能".to_string(),
                        status: TaskStatus::InProgress,
                        result: None,
                        attempts: 1,
                        files_written: vec!["src/calc.rs".to_string()],
                        test_result: Some("❌ 编译失败: mismatched types".to_string()),
                        last_good_snapshot: None,
                        clarifications: vec![],
                        depends_on: vec![],
                    },
                ],
            },
        ]);

        mem.current_phase = 1;
        mem.current_task = Some("1-1".to_string());
        mem.workspace_files = vec![
            "Cargo.toml".to_string(),
            "src/main.rs".to_string(),
            "src/parser.rs".to_string(),
            "src/calc.rs".to_string(),
        ];
        mem
    }

    /// 构建带文件的测试工作区
    fn make_workspace() -> (tempfile::TempDir, Workspace) {
        let dir = tempdir().unwrap();
        let ws = Workspace::new(dir.path());
        ws.init().unwrap();
        ws.write_file("Cargo.toml", "[package]\nname = \"test\"\n")
            .unwrap();
        ws.write_file("src/main.rs", "fn main() {}\n").unwrap();
        (dir, ws)
    }

    // ===== 构建测试 =====

    #[test]
    fn test_build_from_memory_full() {
        let mem = make_full_memory();
        let (_dir, ws) = make_workspace();
        let history = ErrorHistory::new();

        let handoff = ContextHandoff::build_from_memory(&mem, &ws, &history);

        assert_eq!(handoff.goal, "构建一个 CLI 计算器");

        // 当前阶段
        assert!(handoff.current_phase.is_some());
        let phase = handoff.current_phase.unwrap();
        assert_eq!(phase.name, "功能实现");
        assert_eq!(phase.task_count, 2);
        assert_eq!(phase.completed_count, 1);

        // 当前任务
        assert!(handoff.current_task.is_some());
        let task = handoff.current_task.unwrap();
        assert_eq!(task.id, "1-1");
        assert_eq!(task.name, "计算逻辑");
        assert_eq!(task.attempts, 1);

        // 已完成任务 (0-0 和 1-0)
        assert_eq!(handoff.completed_tasks.len(), 2);
        assert!(handoff.completed_tasks.iter().any(|t| t.id == "0-0"));
        assert!(handoff.completed_tasks.iter().any(|t| t.id == "1-0"));

        // 工作区文件
        assert!(!handoff.workspace_files.is_empty());
        assert!(handoff
            .workspace_files
            .iter()
            .any(|f| f.path == "Cargo.toml"));

        // 最近错误 (当前任务有 "编译失败")
        assert!(!handoff.recent_errors.is_empty());

        // 架构约束
        assert!(handoff.architecture_constraints.contains("SOLID"));

        // 已知问题 (有未完成任务)
        assert!(handoff.known_issues.contains("待完成任务"));
    }

    #[test]
    fn test_build_from_memory_empty() {
        let mem = Memory::new("空目标");
        let (_dir, ws) = make_workspace();
        let history = ErrorHistory::new();

        let handoff = ContextHandoff::build_from_memory(&mem, &ws, &history);

        assert_eq!(handoff.goal, "空目标");
        assert!(handoff.current_phase.is_none());
        assert!(handoff.current_task.is_none());
        assert!(handoff.completed_tasks.is_empty());
        assert!(handoff.recent_errors.is_empty());
        assert!(handoff.error_history_summary.is_empty());
        // 即使没有任务, 架构约束也不为空
        assert!(!handoff.architecture_constraints.is_empty());
        // 没有待完成任务, known_issues 可能为空
        assert!(handoff.known_issues.is_empty());
    }

    #[test]
    fn test_build_from_memory_no_current_task() {
        let mut mem = make_full_memory();
        mem.current_task = None;
        let (_dir, ws) = make_workspace();
        let history = ErrorHistory::new();

        let handoff = ContextHandoff::build_from_memory(&mem, &ws, &history);

        assert!(handoff.current_task.is_none());
        // 已完成任务仍应存在
        assert!(!handoff.completed_tasks.is_empty());
    }

    // ===== to_prompt 测试 =====

    #[test]
    fn test_to_prompt_contains_goal() {
        let mem = make_full_memory();
        let (_dir, ws) = make_workspace();
        let history = ErrorHistory::new();

        let handoff = ContextHandoff::build_from_memory(&mem, &ws, &history);
        let prompt = handoff.to_prompt();

        assert!(prompt.contains("构建一个 CLI 计算器"));
        assert!(prompt.contains("项目终极目标"));
    }

    #[test]
    fn test_to_prompt_contains_current_phase() {
        let mem = make_full_memory();
        let (_dir, ws) = make_workspace();
        let history = ErrorHistory::new();

        let handoff = ContextHandoff::build_from_memory(&mem, &ws, &history);
        let prompt = handoff.to_prompt();

        assert!(prompt.contains("功能实现"));
        assert!(prompt.contains("当前阶段"));
    }

    #[test]
    fn test_to_prompt_contains_current_task() {
        let mem = make_full_memory();
        let (_dir, ws) = make_workspace();
        let history = ErrorHistory::new();

        let handoff = ContextHandoff::build_from_memory(&mem, &ws, &history);
        let prompt = handoff.to_prompt();

        assert!(prompt.contains("1-1"));
        assert!(prompt.contains("计算逻辑"));
        assert!(prompt.contains("当前任务"));
    }

    #[test]
    fn test_to_prompt_contains_completed_tasks() {
        let mem = make_full_memory();
        let (_dir, ws) = make_workspace();
        let history = ErrorHistory::new();

        let handoff = ContextHandoff::build_from_memory(&mem, &ws, &history);
        let prompt = handoff.to_prompt();

        assert!(prompt.contains("已完成任务"));
        assert!(prompt.contains("0-0"));
        assert!(prompt.contains("1-0"));
    }

    #[test]
    fn test_to_prompt_contains_workspace_files() {
        let mem = make_full_memory();
        let (_dir, ws) = make_workspace();
        let history = ErrorHistory::new();

        let handoff = ContextHandoff::build_from_memory(&mem, &ws, &history);
        let prompt = handoff.to_prompt();

        assert!(prompt.contains("当前项目文件"));
        assert!(prompt.contains("Cargo.toml"));
        assert!(prompt.contains("src/main.rs"));
    }

    #[test]
    fn test_to_prompt_contains_recent_errors() {
        let mem = make_full_memory();
        let (_dir, ws) = make_workspace();
        let history = ErrorHistory::new();

        let handoff = ContextHandoff::build_from_memory(&mem, &ws, &history);
        let prompt = handoff.to_prompt();

        assert!(prompt.contains("编译失败"));
    }

    #[test]
    fn test_to_prompt_contains_architecture_constraints() {
        let mem = make_full_memory();
        let (_dir, ws) = make_workspace();
        let history = ErrorHistory::new();

        let handoff = ContextHandoff::build_from_memory(&mem, &ws, &history);
        let prompt = handoff.to_prompt();

        assert!(prompt.contains("架构约束"));
        assert!(prompt.contains("SOLID"));
        assert!(prompt.contains("file:路径"));
    }

    #[test]
    fn test_to_prompt_contains_known_issues() {
        let mem = make_full_memory();
        let (_dir, ws) = make_workspace();
        let history = ErrorHistory::new();

        let handoff = ContextHandoff::build_from_memory(&mem, &ws, &history);
        let prompt = handoff.to_prompt();

        assert!(prompt.contains("已知问题"));
        assert!(prompt.contains("待完成任务"));
    }

    #[test]
    fn test_to_prompt_empty_state() {
        let mem = Memory::new("空目标");
        let (_dir, ws) = make_workspace();
        let history = ErrorHistory::new();

        let handoff = ContextHandoff::build_from_memory(&mem, &ws, &history);
        let prompt = handoff.to_prompt();

        // 即使空状态也应包含目标
        assert!(prompt.contains("空目标"));
        assert!(prompt.contains("项目终极目标"));
        assert!(prompt.contains("架构约束"));
        // 不应包含当前阶段/任务的信息块 (用【】区分)
        assert!(!prompt.contains("【当前阶段】"));
        assert!(!prompt.contains("【当前任务】"));
    }

    // ===== 错误历史摘要测试 =====

    #[test]
    fn test_error_history_summary_empty() {
        let history = ErrorHistory::new();
        let summary = ContextHandoff::build_error_history_summary(&history);
        assert!(summary.is_empty());
    }

    #[test]
    fn test_error_history_summary_with_patterns() {
        let mut history = ErrorHistory::new();
        let now = Utc::now();

        // 添加 3 个模式, 出现次数不同
        history.patterns.push(ErrorPattern {
            error_code: Some("E0308".to_string()),
            message_signature: "[E0308] mismatched types".to_string(),
            category: ErrorCategory::TypeError,
            occurrences: 5,
            first_seen: now,
            last_seen: now,
            last_fix_succeeded: true,
            suggested_approach: None,
        });
        history.patterns.push(ErrorPattern {
            error_code: Some("E0382".to_string()),
            message_signature: "[E0382] use of moved value".to_string(),
            category: ErrorCategory::BorrowError,
            occurrences: 3,
            first_seen: now,
            last_seen: now,
            last_fix_succeeded: false,
            suggested_approach: None,
        });
        history.patterns.push(ErrorPattern {
            error_code: None,
            message_signature: "test failure".to_string(),
            category: ErrorCategory::TestFailure,
            occurrences: 1,
            first_seen: now,
            last_seen: now,
            last_fix_succeeded: true,
            suggested_approach: None,
        });

        let summary = ContextHandoff::build_error_history_summary(&history);

        // 应按出现次数降序排列
        assert!(summary.contains("E0308"));
        assert!(summary.contains("5次"));
        assert!(summary.contains("已修复"));
        assert!(summary.contains("E0382"));
        assert!(summary.contains("3次"));
        assert!(summary.contains("未修复"));
    }

    #[test]
    fn test_error_history_summary_truncates_to_5() {
        let mut history = ErrorHistory::new();
        let now = Utc::now();

        // 添加 10 个模式
        for i in 0..10 {
            history.patterns.push(ErrorPattern {
                error_code: Some(format!("E{:04}", i)),
                message_signature: format!("error {}", i),
                category: ErrorCategory::Unknown,
                occurrences: i + 1,
                first_seen: now,
                last_seen: now,
                last_fix_succeeded: false,
                suggested_approach: None,
            });
        }

        let summary = ContextHandoff::build_error_history_summary(&history);

        // 应只显示前 5 个 (出现次数最多的)
        let count = summary.lines().filter(|l| !l.is_empty()).count();
        assert_eq!(count, 5);
    }

    // ===== to_prompt 包含错误历史 =====

    #[test]
    fn test_to_prompt_contains_error_history() {
        let mem = make_full_memory();
        let (_dir, ws) = make_workspace();
        let mut history = ErrorHistory::new();
        let now = Utc::now();

        history.patterns.push(ErrorPattern {
            error_code: Some("E0308".to_string()),
            message_signature: "[E0308] mismatched types".to_string(),
            category: ErrorCategory::TypeError,
            occurrences: 3,
            first_seen: now,
            last_seen: now,
            last_fix_succeeded: true,
            suggested_approach: None,
        });

        let handoff = ContextHandoff::build_from_memory(&mem, &ws, &history);
        let prompt = handoff.to_prompt();

        assert!(prompt.contains("错误历史摘要"));
        assert!(prompt.contains("E0308"));
    }

    // ===== 工作区文件过滤测试 =====

    #[test]
    fn test_workspace_files_excludes_target_and_forge() {
        let dir = tempdir().unwrap();
        let ws = Workspace::new(dir.path());
        ws.init().unwrap();
        ws.write_file("src/main.rs", "fn main() {}").unwrap();
        ws.write_file("target/debug/output", "binary").unwrap();
        ws.write_file("Cargo.toml", "[package]").unwrap();

        let mem = Memory::new("test");
        let history = ErrorHistory::new();
        let handoff = ContextHandoff::build_from_memory(&mem, &ws, &history);

        // 不应包含 target/ 和 .forge/
        assert!(handoff
            .workspace_files
            .iter()
            .any(|f| f.path == "src/main.rs"));
        assert!(handoff
            .workspace_files
            .iter()
            .any(|f| f.path == "Cargo.toml"));
        assert!(!handoff
            .workspace_files
            .iter()
            .any(|f| f.path.starts_with("target/")));
        assert!(!handoff
            .workspace_files
            .iter()
            .any(|f| f.path.starts_with(".forge/")));
    }

    // ===== 已知问题提取测试 =====

    #[test]
    fn test_known_issues_with_pending_tasks() {
        let mem = make_full_memory();
        let issues = ContextHandoff::extract_known_issues(&mem);
        assert!(issues.contains("待完成任务"));
        // 当前任务 1-1 未完成
        assert!(issues.contains("1-1"));
    }

    #[test]
    fn test_known_issues_with_requirement_changes() {
        let mut mem = Memory::new("test");
        mem.add_requirement_change("添加用户认证", "cli");
        let issues = ContextHandoff::extract_known_issues(&mem);
        assert!(issues.contains("待处理需求变更"));
    }

    #[test]
    fn test_known_issues_empty() {
        let mut mem = Memory::new("test");
        mem.set_phases(vec![Phase {
            id: 0,
            name: "test".to_string(),
            description: "".to_string(),
            status: PhaseStatus::Completed,
            tasks: vec![Task {
                id: "0-0".to_string(),
                phase_id: 0,
                name: "done".to_string(),
                prompt: "".to_string(),
                status: TaskStatus::Completed,
                result: None,
                attempts: 1,
                files_written: vec![],
                test_result: None,
                last_good_snapshot: None,
                clarifications: vec![],
                depends_on: vec![],
            }],
        }]);

        let issues = ContextHandoff::extract_known_issues(&mem);
        assert!(issues.is_empty());
    }

    // ===== prompt 长度控制 =====

    #[test]
    fn test_to_prompt_reasonable_length() {
        let mem = make_full_memory();
        let (_dir, ws) = make_workspace();
        let mut history = ErrorHistory::new();
        let now = Utc::now();

        // 添加一些错误历史
        for i in 0..10 {
            history.patterns.push(ErrorPattern {
                error_code: Some(format!("E{:04}", i)),
                message_signature: format!("error {}", i),
                category: ErrorCategory::Unknown,
                occurrences: i + 1,
                first_seen: now,
                last_seen: now,
                last_fix_succeeded: false,
                suggested_approach: None,
            });
        }

        let handoff = ContextHandoff::build_from_memory(&mem, &ws, &history);
        let prompt = handoff.to_prompt();

        // prompt 应在合理范围内 (不超过 20000 字符 ≈ 5000 token)
        let len = prompt.chars().count();
        assert!(len < 20000, "交接 prompt 过长: {} 字符", len);
        // 但也不应太短 (至少包含目标)
        assert!(len > 200, "交接 prompt 过短: {} 字符", len);
    }

    // ===== 多阶段已完成任务 =====

    #[test]
    fn test_completed_tasks_across_phases() {
        let mut mem = Memory::new("test");
        mem.set_phases(vec![
            Phase {
                id: 0,
                name: "Phase 0".to_string(),
                description: "".to_string(),
                status: PhaseStatus::Completed,
                tasks: vec![
                    Task {
                        id: "0-0".to_string(),
                        phase_id: 0,
                        name: "Task A".to_string(),
                        prompt: "".to_string(),
                        status: TaskStatus::Completed,
                        result: Some("ok".to_string()),
                        attempts: 1,
                        files_written: vec!["a.rs".to_string()],
                        test_result: None,
                        last_good_snapshot: None,
                        clarifications: vec![],
                        depends_on: vec![],
                    },
                    Task {
                        id: "0-1".to_string(),
                        phase_id: 0,
                        name: "Task B".to_string(),
                        prompt: "".to_string(),
                        status: TaskStatus::Completed,
                        result: Some("ok".to_string()),
                        attempts: 1,
                        files_written: vec!["b.rs".to_string()],
                        test_result: None,
                        last_good_snapshot: None,
                        clarifications: vec![],
                        depends_on: vec![],
                    },
                ],
            },
            Phase {
                id: 1,
                name: "Phase 1".to_string(),
                description: "".to_string(),
                status: PhaseStatus::InProgress,
                tasks: vec![Task {
                    id: "1-0".to_string(),
                    phase_id: 1,
                    name: "Task C".to_string(),
                    prompt: "".to_string(),
                    status: TaskStatus::Completed,
                    result: Some("ok".to_string()),
                    attempts: 2,
                    files_written: vec!["c.rs".to_string()],
                    test_result: None,
                    last_good_snapshot: None,
                    clarifications: vec![],
                    depends_on: vec![],
                }],
            },
        ]);
        mem.current_phase = 1;
        mem.current_task = None;

        let (_dir, ws) = make_workspace();
        let history = ErrorHistory::new();

        let handoff = ContextHandoff::build_from_memory(&mem, &ws, &history);

        // 应有 3 个已完成任务 (跨阶段)
        assert_eq!(handoff.completed_tasks.len(), 3);
    }

    // ===== 文件大小测试 =====

    #[test]
    fn test_file_summary_contains_size() {
        let dir = tempdir().unwrap();
        let ws = Workspace::new(dir.path());
        ws.init().unwrap();
        ws.write_file("small.rs", "fn main() {}\n").unwrap();
        ws.write_file("large.rs", &"x".repeat(1000)).unwrap();

        let mem = Memory::new("test");
        let history = ErrorHistory::new();
        let handoff = ContextHandoff::build_from_memory(&mem, &ws, &history);

        let small = handoff
            .workspace_files
            .iter()
            .find(|f| f.path == "small.rs")
            .unwrap();
        let large = handoff
            .workspace_files
            .iter()
            .find(|f| f.path == "large.rs")
            .unwrap();
        assert!(small.size < large.size);
        assert_eq!(large.size, 1000);
    }

    // ==================================================================
    //  纯逻辑函数测试 — 14 个函数
    // ==================================================================

    // ===== is_workspace_file_included =====

    #[test]
    fn test_is_workspace_file_included_src() {
        assert!(is_workspace_file_included("src/main.rs"));
        assert!(is_workspace_file_included("src/lib.rs"));
        assert!(is_workspace_file_included("src/core/mod.rs"));
    }

    #[test]
    fn test_is_workspace_file_included_cargo() {
        assert!(is_workspace_file_included("Cargo.toml"));
        assert!(is_workspace_file_included("Cargo.lock"));
    }

    #[test]
    fn test_is_workspace_file_included_target() {
        assert!(!is_workspace_file_included("target/debug/output"));
        assert!(!is_workspace_file_included("target/release/test"));
        assert!(!is_workspace_file_included("target/"));
    }

    #[test]
    fn test_is_workspace_file_included_forge() {
        assert!(!is_workspace_file_included(".forge/logs/trace.jsonl"));
        assert!(!is_workspace_file_included(".forge/memory.json"));
        assert!(!is_workspace_file_included(".forge/"));
    }

    #[test]
    fn test_is_workspace_file_included_root_files() {
        assert!(is_workspace_file_included("README.md"));
        assert!(is_workspace_file_included(".gitignore"));
        assert!(is_workspace_file_included("build.rs"));
    }

    #[test]
    fn test_is_workspace_file_included_empty() {
        assert!(is_workspace_file_included(""));
    }

    // ===== truncate_text =====

    #[test]
    fn test_truncate_text_short() {
        assert_eq!(truncate_text("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_text_exact() {
        assert_eq!(truncate_text("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_text_long() {
        assert_eq!(truncate_text("hello world", 5), "hello");
    }

    #[test]
    fn test_truncate_text_empty() {
        assert_eq!(truncate_text("", 10), "");
    }

    #[test]
    fn test_truncate_text_zero_max() {
        assert_eq!(truncate_text("hello", 0), "");
    }

    #[test]
    fn test_truncate_text_unicode() {
        // 中文字符每个算一个 char
        assert_eq!(truncate_text("你好世界", 2), "你好");
        assert_eq!(truncate_text("你好world", 5), "你好wor");
    }

    // ===== should_trigger_handoff =====

    #[test]
    fn test_should_trigger_handoff_below() {
        assert!(!should_trigger_handoff(5, 10));
    }

    #[test]
    fn test_should_trigger_handoff_at_threshold() {
        assert!(should_trigger_handoff(10, 10));
    }

    #[test]
    fn test_should_trigger_handoff_above() {
        assert!(should_trigger_handoff(15, 10));
    }

    #[test]
    fn test_should_trigger_handoff_zero_threshold() {
        assert!(should_trigger_handoff(0, 0));
    }

    #[test]
    fn test_should_trigger_handoff_zero_count() {
        assert!(!should_trigger_handoff(0, 1));
    }

    // ===== format_error_code_badge =====

    #[test]
    fn test_format_error_code_badge_some() {
        assert_eq!(format_error_code_badge(Some("E0308")), "[E0308]");
    }

    #[test]
    fn test_format_error_code_badge_none() {
        assert_eq!(format_error_code_badge(None), "");
    }

    #[test]
    fn test_format_error_code_badge_empty_string() {
        assert_eq!(format_error_code_badge(Some("")), "[]");
    }

    // ===== build_phase_summary =====

    #[test]
    fn test_build_phase_summary_full() {
        let phase = Phase {
            id: 0,
            name: "功能实现".to_string(),
            description: "核心功能".to_string(),
            status: PhaseStatus::InProgress,
            tasks: vec![
                Task {
                    id: "0-0".to_string(),
                    phase_id: 0,
                    name: "任务A".to_string(),
                    prompt: "".to_string(),
                    status: TaskStatus::Completed,
                    result: None,
                    attempts: 1,
                    files_written: vec![],
                    test_result: None,
                    last_good_snapshot: None,
                    clarifications: vec![],
                    depends_on: vec![],
                },
                Task {
                    id: "0-1".to_string(),
                    phase_id: 0,
                    name: "任务B".to_string(),
                    prompt: "".to_string(),
                    status: TaskStatus::Pending,
                    result: None,
                    attempts: 0,
                    files_written: vec![],
                    test_result: None,
                    last_good_snapshot: None,
                    clarifications: vec![],
                    depends_on: vec![],
                },
            ],
        };

        let summary = build_phase_summary(&phase);
        assert_eq!(summary.name, "功能实现");
        assert_eq!(summary.description, "核心功能");
        assert_eq!(summary.status, "InProgress");
        assert_eq!(summary.task_count, 2);
        assert_eq!(summary.completed_count, 1);
    }

    #[test]
    fn test_build_phase_summary_empty() {
        let phase = Phase {
            id: 0,
            name: "空阶段".to_string(),
            description: "".to_string(),
            status: PhaseStatus::Pending,
            tasks: vec![],
        };

        let summary = build_phase_summary(&phase);
        assert_eq!(summary.task_count, 0);
        assert_eq!(summary.completed_count, 0);
    }

    #[test]
    fn test_build_phase_summary_all_completed() {
        let phase = Phase {
            id: 0,
            name: "已完成阶段".to_string(),
            description: "".to_string(),
            status: PhaseStatus::Completed,
            tasks: vec![
                Task {
                    id: "0-0".to_string(),
                    phase_id: 0,
                    name: "A".to_string(),
                    prompt: "".to_string(),
                    status: TaskStatus::Completed,
                    result: None,
                    attempts: 1,
                    files_written: vec![],
                    test_result: None,
                    last_good_snapshot: None,
                    clarifications: vec![],
                    depends_on: vec![],
                },
                Task {
                    id: "0-1".to_string(),
                    phase_id: 0,
                    name: "B".to_string(),
                    prompt: "".to_string(),
                    status: TaskStatus::Completed,
                    result: None,
                    attempts: 1,
                    files_written: vec![],
                    test_result: None,
                    last_good_snapshot: None,
                    clarifications: vec![],
                    depends_on: vec![],
                },
            ],
        };

        let summary = build_phase_summary(&phase);
        assert_eq!(summary.task_count, 2);
        assert_eq!(summary.completed_count, 2);
    }

    // ===== build_task_summary =====

    #[test]
    fn test_build_task_summary_full() {
        let task = Task {
            id: "1-0".to_string(),
            phase_id: 1,
            name: "实现解析".to_string(),
            prompt: "实现参数解析".to_string(),
            status: TaskStatus::Completed,
            result: Some("成功".to_string()),
            attempts: 3,
            files_written: vec!["src/parser.rs".to_string(), "src/token.rs".to_string()],
            test_result: Some("✅ 通过".to_string()),
            last_good_snapshot: Some(2),
            clarifications: vec![],
            depends_on: vec![],
        };

        let summary = build_task_summary(&task);
        assert_eq!(summary.id, "1-0");
        assert_eq!(summary.name, "实现解析");
        assert_eq!(summary.status, "Completed");
        assert_eq!(summary.result.as_deref(), Some("成功"));
        assert_eq!(summary.files_written, vec!["src/parser.rs", "src/token.rs"]);
        assert_eq!(summary.attempts, 3);
    }

    #[test]
    fn test_build_task_summary_minimal() {
        let task = Task {
            id: "0-0".to_string(),
            phase_id: 0,
            name: "".to_string(),
            prompt: "".to_string(),
            status: TaskStatus::Pending,
            result: None,
            attempts: 0,
            files_written: vec![],
            test_result: None,
            last_good_snapshot: None,
            clarifications: vec![],
            depends_on: vec![],
        };

        let summary = build_task_summary(&task);
        assert_eq!(summary.id, "0-0");
        assert_eq!(summary.status, "Pending");
        assert!(summary.result.is_none());
        assert!(summary.files_written.is_empty());
        assert_eq!(summary.attempts, 0);
    }

    // ===== collect_completed_tasks =====

    #[test]
    fn test_collect_completed_tasks_empty() {
        let phases: Vec<Phase> = vec![];
        let result = collect_completed_tasks(&phases);
        assert!(result.is_empty());
    }

    #[test]
    fn test_collect_completed_tasks_none_completed() {
        let phases = vec![Phase {
            id: 0,
            name: "test".to_string(),
            description: "".to_string(),
            status: PhaseStatus::InProgress,
            tasks: vec![
                Task {
                    id: "0-0".to_string(),
                    phase_id: 0,
                    name: "A".to_string(),
                    prompt: "".to_string(),
                    status: TaskStatus::Pending,
                    result: None,
                    attempts: 0,
                    files_written: vec![],
                    test_result: None,
                    last_good_snapshot: None,
                    clarifications: vec![],
                    depends_on: vec![],
                },
                Task {
                    id: "0-1".to_string(),
                    phase_id: 0,
                    name: "B".to_string(),
                    prompt: "".to_string(),
                    status: TaskStatus::InProgress,
                    result: None,
                    attempts: 1,
                    files_written: vec![],
                    test_result: None,
                    last_good_snapshot: None,
                    clarifications: vec![],
                    depends_on: vec![],
                },
            ],
        }];

        let result = collect_completed_tasks(&phases);
        assert!(result.is_empty());
    }

    #[test]
    fn test_collect_completed_tasks_some_completed() {
        let phases = vec![Phase {
            id: 0,
            name: "test".to_string(),
            description: "".to_string(),
            status: PhaseStatus::InProgress,
            tasks: vec![
                Task {
                    id: "0-0".to_string(),
                    phase_id: 0,
                    name: "完成".to_string(),
                    prompt: "".to_string(),
                    status: TaskStatus::Completed,
                    result: None,
                    attempts: 1,
                    files_written: vec![],
                    test_result: None,
                    last_good_snapshot: None,
                    clarifications: vec![],
                    depends_on: vec![],
                },
                Task {
                    id: "0-1".to_string(),
                    phase_id: 0,
                    name: "未完成".to_string(),
                    prompt: "".to_string(),
                    status: TaskStatus::InProgress,
                    result: None,
                    attempts: 1,
                    files_written: vec![],
                    test_result: None,
                    last_good_snapshot: None,
                    clarifications: vec![],
                    depends_on: vec![],
                },
            ],
        }];

        let result = collect_completed_tasks(&phases);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "0-0");
    }

    #[test]
    fn test_collect_completed_tasks_across_phases() {
        let phases = vec![
            Phase {
                id: 0,
                name: "Phase 0".to_string(),
                description: "".to_string(),
                status: PhaseStatus::Completed,
                tasks: vec![Task {
                    id: "0-0".to_string(),
                    phase_id: 0,
                    name: "A".to_string(),
                    prompt: "".to_string(),
                    status: TaskStatus::Completed,
                    result: None,
                    attempts: 1,
                    files_written: vec!["a.rs".to_string()],
                    test_result: None,
                    last_good_snapshot: None,
                    clarifications: vec![],
                    depends_on: vec![],
                }],
            },
            Phase {
                id: 1,
                name: "Phase 1".to_string(),
                description: "".to_string(),
                status: PhaseStatus::InProgress,
                tasks: vec![
                    Task {
                        id: "1-0".to_string(),
                        phase_id: 1,
                        name: "B".to_string(),
                        prompt: "".to_string(),
                        status: TaskStatus::Completed,
                        result: None,
                        attempts: 1,
                        files_written: vec!["b.rs".to_string()],
                        test_result: None,
                        last_good_snapshot: None,
                        clarifications: vec![],
                        depends_on: vec![],
                    },
                    Task {
                        id: "1-1".to_string(),
                        phase_id: 1,
                        name: "C".to_string(),
                        prompt: "".to_string(),
                        status: TaskStatus::Failed,
                        result: None,
                        attempts: 2,
                        files_written: vec![],
                        test_result: None,
                        last_good_snapshot: None,
                        clarifications: vec![],
                        depends_on: vec![],
                    },
                ],
            },
        ];

        let result = collect_completed_tasks(&phases);
        assert_eq!(result.len(), 2);
        assert!(result.iter().any(|t| t.id == "0-0"));
        assert!(result.iter().any(|t| t.id == "1-0"));
        assert!(!result.iter().any(|t| t.id == "1-1"));
    }

    #[test]
    fn test_collect_completed_tasks_preserves_files() {
        let phases = vec![Phase {
            id: 0,
            name: "test".to_string(),
            description: "".to_string(),
            status: PhaseStatus::Completed,
            tasks: vec![Task {
                id: "0-0".to_string(),
                phase_id: 0,
                name: "A".to_string(),
                prompt: "".to_string(),
                status: TaskStatus::Completed,
                result: None,
                attempts: 1,
                files_written: vec!["src/main.rs".to_string(), "src/lib.rs".to_string()],
                test_result: None,
                last_good_snapshot: None,
                clarifications: vec![],
                depends_on: vec![],
            }],
        }];

        let result = collect_completed_tasks(&phases);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].files_written, vec!["src/main.rs", "src/lib.rs"]);
    }

    // ===== format_phase_section =====

    #[test]
    fn test_format_phase_section_normal() {
        let phase = PhaseSummary {
            name: "功能实现".to_string(),
            description: "核心功能".to_string(),
            status: "InProgress".to_string(),
            task_count: 3,
            completed_count: 1,
        };

        let section = format_phase_section(&phase);
        assert!(section.contains("【当前阶段】"));
        assert!(section.contains("功能实现"));
        assert!(section.contains("核心功能"));
        assert!(section.contains("InProgress"));
        assert!(section.contains("1/3 任务完成"));
    }

    #[test]
    fn test_format_phase_section_zero_tasks() {
        let phase = PhaseSummary {
            name: "空阶段".to_string(),
            description: "".to_string(),
            status: "Pending".to_string(),
            task_count: 0,
            completed_count: 0,
        };

        let section = format_phase_section(&phase);
        assert!(section.contains("0/0 任务完成"));
    }

    // ===== format_task_section =====

    #[test]
    fn test_format_task_section_with_files_and_result() {
        let task = TaskSummary {
            id: "1-0".to_string(),
            name: "实现解析".to_string(),
            status: "InProgress".to_string(),
            result: Some("部分完成".to_string()),
            files_written: vec!["src/parser.rs".to_string()],
            attempts: 2,
        };

        let section = format_task_section(&task);
        assert!(section.contains("【当前任务】"));
        assert!(section.contains("1-0"));
        assert!(section.contains("实现解析"));
        assert!(section.contains("InProgress"));
        assert!(section.contains("2 次"));
        assert!(section.contains("已写入文件: src/parser.rs"));
        assert!(section.contains("结果: 部分完成"));
    }

    #[test]
    fn test_format_task_section_without_files() {
        let task = TaskSummary {
            id: "0-0".to_string(),
            name: "初始化".to_string(),
            status: "Pending".to_string(),
            result: None,
            files_written: vec![],
            attempts: 0,
        };

        let section = format_task_section(&task);
        assert!(section.contains("【当前任务】"));
        assert!(!section.contains("已写入文件"));
        assert!(!section.contains("结果:"));
    }

    #[test]
    fn test_format_task_section_without_result() {
        let task = TaskSummary {
            id: "1-1".to_string(),
            name: "计算逻辑".to_string(),
            status: "InProgress".to_string(),
            result: None,
            files_written: vec!["src/calc.rs".to_string()],
            attempts: 1,
        };

        let section = format_task_section(&task);
        assert!(section.contains("已写入文件: src/calc.rs"));
        assert!(!section.contains("结果:"));
    }

    // ===== format_completed_tasks_section =====

    #[test]
    fn test_format_completed_tasks_section_empty() {
        let section = format_completed_tasks_section(&[]);
        assert!(section.is_empty());
    }

    #[test]
    fn test_format_completed_tasks_section_single() {
        let tasks = vec![TaskSummary {
            id: "0-0".to_string(),
            name: "初始化".to_string(),
            status: "Completed".to_string(),
            result: None,
            files_written: vec!["Cargo.toml".to_string()],
            attempts: 1,
        }];

        let section = format_completed_tasks_section(&tasks);
        assert!(section.contains("【已完成任务】"));
        assert!(section.contains("(1)"));
        assert!(section.contains("0-0"));
        assert!(section.contains("初始化"));
        assert!(section.contains("Cargo.toml"));
        assert!(!section.contains("... 还有"));
    }

    #[test]
    fn test_format_completed_tasks_section_exactly_ten() {
        let tasks: Vec<TaskSummary> = (0..10)
            .map(|i| TaskSummary {
                id: format!("0-{}", i),
                name: format!("Task {}", i),
                status: "Completed".to_string(),
                result: None,
                files_written: vec![format!("file{}.rs", i)],
                attempts: 1,
            })
            .collect();

        let section = format_completed_tasks_section(&tasks);
        assert!(section.contains("(10)"));
        assert!(!section.contains("... 还有"));
    }

    #[test]
    fn test_format_completed_tasks_section_eleven() {
        let tasks: Vec<TaskSummary> = (0..11)
            .map(|i| TaskSummary {
                id: format!("0-{}", i),
                name: format!("Task {}", i),
                status: "Completed".to_string(),
                result: None,
                files_written: vec![format!("file{}.rs", i)],
                attempts: 1,
            })
            .collect();

        let section = format_completed_tasks_section(&tasks);
        assert!(section.contains("(11)"));
        assert!(section.contains("... 还有 1 个"));
    }

    #[test]
    fn test_format_completed_tasks_section_many() {
        let tasks: Vec<TaskSummary> = (0..25)
            .map(|i| TaskSummary {
                id: format!("0-{}", i),
                name: format!("Task {}", i),
                status: "Completed".to_string(),
                result: None,
                files_written: vec![format!("file{}.rs", i)],
                attempts: 1,
            })
            .collect();

        let section = format_completed_tasks_section(&tasks);
        assert!(section.contains("(25)"));
        assert!(section.contains("... 还有 15 个"));
    }

    #[test]
    fn test_format_completed_tasks_section_empty_files() {
        let tasks = vec![TaskSummary {
            id: "0-0".to_string(),
            name: "设计".to_string(),
            status: "Completed".to_string(),
            result: None,
            files_written: vec![],
            attempts: 1,
        }];

        let section = format_completed_tasks_section(&tasks);
        // files_written 为空时 join 产生空字符串
        assert!(section.contains("设计 →"));
    }

    // ===== format_workspace_files_section =====

    #[test]
    fn test_format_workspace_files_section_empty() {
        let section = format_workspace_files_section(&[]);
        assert!(section.is_empty());
    }

    #[test]
    fn test_format_workspace_files_section_single() {
        let files = vec![FileSummary {
            path: "src/main.rs".to_string(),
            size: 500,
        }];

        let section = format_workspace_files_section(&files);
        assert!(section.contains("【当前项目文件】"));
        assert!(section.contains("(1)"));
        assert!(section.contains("src/main.rs"));
        assert!(section.contains("500字节"));
        assert!(!section.contains("... 还有"));
    }

    #[test]
    fn test_format_workspace_files_section_exactly_twenty() {
        let files: Vec<FileSummary> = (0..20)
            .map(|i| FileSummary {
                path: format!("src/file{}.rs", i),
                size: 100,
            })
            .collect();

        let section = format_workspace_files_section(&files);
        assert!(section.contains("(20)"));
        assert!(!section.contains("... 还有"));
    }

    #[test]
    fn test_format_workspace_files_section_twenty_one() {
        let files: Vec<FileSummary> = (0..21)
            .map(|i| FileSummary {
                path: format!("src/file{}.rs", i),
                size: 100,
            })
            .collect();

        let section = format_workspace_files_section(&files);
        assert!(section.contains("(21)"));
        assert!(section.contains("... 还有 1 个文件"));
    }

    #[test]
    fn test_format_workspace_files_section_zero_size() {
        let files = vec![FileSummary {
            path: "empty.txt".to_string(),
            size: 0,
        }];

        let section = format_workspace_files_section(&files);
        assert!(section.contains("0字节"));
    }

    // ===== format_recent_errors_section =====

    #[test]
    fn test_format_recent_errors_section_empty() {
        let section = format_recent_errors_section(&[]);
        assert!(section.is_empty());
    }

    #[test]
    fn test_format_recent_errors_section_with_code() {
        let errors = vec![ErrorSummary {
            file: "src/main.rs".to_string(),
            message: "mismatched types".to_string(),
            error_code: Some("E0308".to_string()),
        }];

        let section = format_recent_errors_section(&errors);
        assert!(section.contains("【最近编译/测试错误】"));
        assert!(section.contains("src/main.rs"));
        assert!(section.contains("[E0308]"));
        assert!(section.contains("mismatched types"));
    }

    #[test]
    fn test_format_recent_errors_section_without_code() {
        let errors = vec![ErrorSummary {
            file: "src/lib.rs".to_string(),
            message: "unexpected token".to_string(),
            error_code: None,
        }];

        let section = format_recent_errors_section(&errors);
        assert!(section.contains("src/lib.rs"));
        assert!(!section.contains("[E"));
        assert!(section.contains("unexpected token"));
    }

    #[test]
    fn test_format_recent_errors_section_multiple() {
        let errors = vec![
            ErrorSummary {
                file: "src/main.rs".to_string(),
                message: "error 1".to_string(),
                error_code: Some("E0308".to_string()),
            },
            ErrorSummary {
                file: "src/lib.rs".to_string(),
                message: "error 2".to_string(),
                error_code: None,
            },
        ];

        let section = format_recent_errors_section(&errors);
        assert!(section.contains("src/main.rs"));
        assert!(section.contains("src/lib.rs"));
        assert!(section.contains("error 1"));
        assert!(section.contains("error 2"));
    }

    // ===== format_error_history_section =====

    #[test]
    fn test_format_error_history_section_empty() {
        let section = format_error_history_section("");
        assert!(section.is_empty());
    }

    #[test]
    fn test_format_error_history_section_non_empty() {
        let summary = "  1. [TypeError] E0308 (出现5次, 已修复)\n";
        let section = format_error_history_section(summary);
        assert!(section.contains("【错误历史摘要】"));
        assert!(section.contains("Top 5 模式"));
        assert!(section.contains("E0308"));
    }

    // ===== format_known_issues_section =====

    #[test]
    fn test_format_known_issues_section_empty() {
        let section = format_known_issues_section("");
        assert!(section.is_empty());
    }

    #[test]
    fn test_format_known_issues_section_non_empty() {
        let issues = "待完成任务 (2):\n  - [1-0] 任务A\n";
        let section = format_known_issues_section(issues);
        assert!(section.contains("【已知问题】"));
        assert!(section.contains("待完成任务"));
        assert!(section.contains("任务A"));
    }

    // ===== 方法委托测试 =====

    #[test]
    fn test_build_from_memory_uses_build_phase_summary() {
        let mem = make_full_memory();
        let (_dir, ws) = make_workspace();
        let history = ErrorHistory::new();

        let handoff = ContextHandoff::build_from_memory(&mem, &ws, &history);

        // 验证 build_phase_summary 正确被调用
        assert!(handoff.current_phase.is_some());
        let phase = handoff.current_phase.unwrap();
        assert_eq!(phase.name, "功能实现");
        assert_eq!(phase.status, "InProgress");
    }

    #[test]
    fn test_build_from_memory_uses_build_task_summary() {
        let mem = make_full_memory();
        let (_dir, ws) = make_workspace();
        let history = ErrorHistory::new();

        let handoff = ContextHandoff::build_from_memory(&mem, &ws, &history);

        assert!(handoff.current_task.is_some());
        let task = handoff.current_task.unwrap();
        assert_eq!(task.id, "1-1");
        assert_eq!(task.name, "计算逻辑");
        assert_eq!(task.attempts, 1);
    }

    #[test]
    fn test_build_from_memory_uses_collect_completed_tasks() {
        let mem = make_full_memory();
        let (_dir, ws) = make_workspace();
        let history = ErrorHistory::new();

        let handoff = ContextHandoff::build_from_memory(&mem, &ws, &history);

        // make_full_memory 有 2 个已完成任务 (0-0 和 1-0)
        assert_eq!(handoff.completed_tasks.len(), 2);
        // 验证使用了 build_task_summary (status 是字符串)
        assert_eq!(handoff.completed_tasks[0].status, "Completed");
    }

    #[test]
    fn test_build_from_memory_uses_is_workspace_file_included() {
        let dir = tempdir().unwrap();
        let ws = Workspace::new(dir.path());
        ws.init().unwrap();
        ws.write_file("src/main.rs", "fn main() {}").unwrap();
        ws.write_file("target/debug/output", "binary").unwrap();
        ws.write_file(".forge/memory.json", "{}").unwrap();
        ws.write_file("Cargo.toml", "[package]").unwrap();

        let mem = Memory::new("test");
        let history = ErrorHistory::new();
        let handoff = ContextHandoff::build_from_memory(&mem, &ws, &history);

        assert!(handoff
            .workspace_files
            .iter()
            .any(|f| f.path == "src/main.rs"));
        assert!(handoff
            .workspace_files
            .iter()
            .any(|f| f.path == "Cargo.toml"));
        assert!(!handoff
            .workspace_files
            .iter()
            .any(|f| f.path.starts_with("target/")));
        assert!(!handoff
            .workspace_files
            .iter()
            .any(|f| f.path.starts_with(".forge/")));
    }

    #[test]
    fn test_extract_recent_errors_uses_truncate_text() {
        let mut mem = Memory::new("test");
        let long_message = "失败: ".to_string() + &"x".repeat(500);
        mem.set_phases(vec![Phase {
            id: 0,
            name: "test".to_string(),
            description: "".to_string(),
            status: PhaseStatus::InProgress,
            tasks: vec![Task {
                id: "0-0".to_string(),
                phase_id: 0,
                name: "A".to_string(),
                prompt: "".to_string(),
                status: TaskStatus::InProgress,
                result: None,
                attempts: 1,
                files_written: vec![],
                test_result: Some(long_message),
                last_good_snapshot: None,
                clarifications: vec![],
                depends_on: vec![],
            }],
        }]);
        mem.current_task = Some("0-0".to_string());

        let errors = ContextHandoff::extract_recent_errors(&mem);
        assert_eq!(errors.len(), 1);
        // 应被截断到 300 字符
        let msg_len = errors[0].message.chars().count();
        assert_eq!(msg_len, 300);
    }

    #[test]
    fn test_to_prompt_uses_format_phase_section() {
        let mem = make_full_memory();
        let (_dir, ws) = make_workspace();
        let history = ErrorHistory::new();

        let handoff = ContextHandoff::build_from_memory(&mem, &ws, &history);
        let prompt = handoff.to_prompt();

        // format_phase_section 的输出格式
        assert!(prompt.contains("【当前阶段】"));
        assert!(prompt.contains("进度: 1/2 任务完成"));
    }

    #[test]
    fn test_to_prompt_uses_format_task_section() {
        let mem = make_full_memory();
        let (_dir, ws) = make_workspace();
        let history = ErrorHistory::new();

        let handoff = ContextHandoff::build_from_memory(&mem, &ws, &history);
        let prompt = handoff.to_prompt();

        // format_task_section 的输出格式
        assert!(prompt.contains("【当前任务】"));
        assert!(prompt.contains("尝试: 1 次"));
    }

    #[test]
    fn test_to_prompt_uses_format_completed_tasks_section() {
        let mem = make_full_memory();
        let (_dir, ws) = make_workspace();
        let history = ErrorHistory::new();

        let handoff = ContextHandoff::build_from_memory(&mem, &ws, &history);
        let prompt = handoff.to_prompt();

        // format_completed_tasks_section 的输出格式 (包含数量)
        assert!(prompt.contains("【已完成任务】(2)"));
    }

    #[test]
    fn test_to_prompt_uses_format_error_code_badge() {
        // 构造一个带 error_code 的 ErrorSummary
        let handoff = ContextHandoff {
            goal: "test".to_string(),
            current_phase: None,
            current_task: None,
            completed_tasks: vec![],
            workspace_files: vec![],
            recent_errors: vec![ErrorSummary {
                file: "src/main.rs".to_string(),
                message: "error".to_string(),
                error_code: Some("E0308".to_string()),
            }],
            error_history_summary: String::new(),
            architecture_constraints: String::new(),
            known_issues: String::new(),
        };

        let prompt = handoff.to_prompt();
        assert!(prompt.contains("[E0308]"));
    }

    #[test]
    fn test_to_prompt_empty_uses_format_functions() {
        let handoff = ContextHandoff {
            goal: "空目标".to_string(),
            current_phase: None,
            current_task: None,
            completed_tasks: vec![],
            workspace_files: vec![],
            recent_errors: vec![],
            error_history_summary: String::new(),
            architecture_constraints: "约束".to_string(),
            known_issues: String::new(),
        };

        let prompt = handoff.to_prompt();

        // 空状态不应包含可选区块
        assert!(!prompt.contains("【当前阶段】"));
        assert!(!prompt.contains("【当前任务】"));
        assert!(!prompt.contains("【已完成任务】"));
        assert!(!prompt.contains("【当前项目文件】"));
        assert!(!prompt.contains("【最近编译/测试错误】"));
        assert!(!prompt.contains("【错误历史摘要】"));
        assert!(!prompt.contains("【已知问题】"));
        // 但应包含目标
        assert!(prompt.contains("空目标"));
        assert!(prompt.contains("【架构约束】"));
    }
}
