//! 转向提醒机制 — 借鉴方向 2
//!
//! 在对话达到一定轮数后, 自动在发送给 AI 的消息中注入"提醒"片段,
//! 重新提醒 AI 当前在做什么、应该遵循什么约束,
//! 防止 AI 在长时间运行后逐渐偏离原始目标和架构规范。
//!
//! ## 核心思路
//!
//! 24 小时运行中, AI 会在第 20、50、100 轮对话后逐渐"忘记"原始目标和约束,
//! 开始做无关的事、偏离架构规范、重复已完成的任务。
//! 每隔 N 轮对话 (如每 10 轮), 在发送的消息前注入一个"转向提醒" (steer reminder),
//! 重新锚定 AI 的注意力。
//!
//! ## 与上下文衔接的关系
//!
//! - **转向提醒 (本模块)**: 对话未到新开阈值但已较长 → 在消息中注入提醒 → 不新开对话, 仅锚定注意力
//! - **上下文衔接 (context_handoff.rs)**: 对话过长 → 新开对话 → 交接全部上下文 → 重置轮数
//!
//! 两者互补: 转向提醒是轻量级干预 (不新开对话), 上下文衔接是重量级干预 (新开对话 + 交接)。
//! 先做转向提醒, 如果继续变长则触发上下文衔接。
//! 推荐配置: `steer_interval < max_context_turns` (如 10 < 30)。
//! 交接后对话轮数清零, 转向提醒也随之重新计数。

use crate::memory::{Memory, Phase};

// ============================================================================
//  纯逻辑函数 — 无副作用, 可独立测试
// ============================================================================

/// 从阶段列表中提取当前阶段的名称
///
/// 如果索引越界或阶段列表为空, 返回空字符串。
///
/// # 示例
///
/// ```
/// # use forge::steer_reminder::extract_phase_name;
/// # use forge::memory::{Phase, PhaseStatus};
/// let phases = vec![
///     Phase { id: 0, name: "阶段A".into(), description: "".into(),
///            status: PhaseStatus::Completed, tasks: vec![] },
/// ];
/// assert_eq!(extract_phase_name(&phases, 0), "阶段A");
/// assert_eq!(extract_phase_name(&phases, 99), "");  // 越界
/// assert_eq!(extract_phase_name(&[], 0), "");        // 空列表
/// ```
pub fn extract_phase_name(phases: &[Phase], current_phase_idx: usize) -> String {
    phases
        .get(current_phase_idx)
        .map(|p| p.name.clone())
        .unwrap_or_default()
}

/// 从阶段列表中提取当前任务的名称
///
/// 遍历所有阶段的所有任务, 查找 ID 匹配的任务。
/// 如果 `current_task_id` 为 None 或未找到匹配任务, 返回空字符串。
///
/// # 示例
///
/// ```
/// # use forge::steer_reminder::extract_task_name;
/// # use forge::memory::{Phase, PhaseStatus, Task, TaskStatus};
/// let phases = vec![
///     Phase { id: 0, name: "阶段A".into(), description: "".into(),
///            status: PhaseStatus::InProgress, tasks: vec![
///         Task { id: "0-0".into(), phase_id: 0, name: "任务X".into(),
///               prompt: "".into(), status: TaskStatus::Pending,
///               result: None, attempts: 0, files_written: vec![],
///               test_result: None, last_good_snapshot: None,
///               clarifications: vec![], depends_on: vec![] },
///     ] },
/// ];
/// assert_eq!(extract_task_name(&phases, &Some("0-0".into())), "任务X");
/// assert_eq!(extract_task_name(&phases, &None), "");
/// assert_eq!(extract_task_name(&phases, &Some("不存在".into())), "");
/// ```
pub fn extract_task_name(phases: &[Phase], current_task_id: &Option<String>) -> String {
    current_task_id
        .as_ref()
        .and_then(|task_id| {
            phases
                .iter()
                .flat_map(|p| &p.tasks)
                .find(|t| &t.id == task_id)
                .map(|t| t.name.clone())
        })
        .unwrap_or_default()
}

/// 格式化目标行
///
/// 生成 `📌 项目目标: {goal}\n` 格式的字符串。
/// 即使 goal 为空也会生成该行 (空目标也是一种状态)。
///
/// # 示例
///
/// ```
/// # use forge::steer_reminder::format_goal_line;
/// assert_eq!(format_goal_line("构建计算器"), "📌 项目目标: 构建计算器\n");
/// assert_eq!(format_goal_line(""), "📌 项目目标: \n");
/// ```
pub fn format_goal_line(goal: &str) -> String {
    format!("📌 项目目标: {}\n", goal)
}

/// 格式化阶段/任务行
///
/// - 如果阶段名为空, 返回 `None` (不输出此行)
/// - 如果阶段名非空但任务名为空, 只输出阶段
/// - 如果两者都非空, 输出 `📋 当前阶段: {phase} | 任务: {task}\n`
///
/// # 示例
///
/// ```
/// # use forge::steer_reminder::format_phase_task_line;
/// assert_eq!(format_phase_task_line("阶段A", "任务B"),
///     Some("📋 当前阶段: 阶段A | 任务: 任务B\n".to_string()));
/// assert_eq!(format_phase_task_line("阶段A", ""),
///     Some("📋 当前阶段: 阶段A\n".to_string()));
/// assert_eq!(format_phase_task_line("", "任务B"), None);
/// assert_eq!(format_phase_task_line("", ""), None);
/// ```
pub fn format_phase_task_line(phase: &str, task: &str) -> Option<String> {
    if phase.is_empty() {
        return None;
    }
    if task.is_empty() {
        return Some(format!("📋 当前阶段: {}\n", phase));
    }
    Some(format!("📋 当前阶段: {} | 任务: {}\n", phase, task))
}

/// 格式化约束区块
///
/// 如果约束列表为空, 返回空字符串。
/// 否则生成 `🔧 约束:\n  - {constraint}\n` 格式的多行字符串。
///
/// # 示例
///
/// ```
/// # use forge::steer_reminder::format_constraints_section;
/// assert_eq!(format_constraints_section(&[]), "");
/// let s = format_constraints_section(&["约束A".into(), "约束B".into()]);
/// assert!(s.contains("🔧 约束:"));
/// assert!(s.contains("  - 约束A"));
/// assert!(s.contains("  - 约束B"));
/// ```
pub fn format_constraints_section(constraints: &[String]) -> String {
    if constraints.is_empty() {
        return String::new();
    }
    let mut section = String::from("🔧 约束:\n");
    for c in constraints {
        section.push_str(&format!("  - {}\n", c));
    }
    section
}

/// 判断当前对话轮数是否需要注入提醒 (纯逻辑)
///
/// 触发条件 (全部满足):
/// - `interval > 0` (已启用)
/// - `turn_count > 0` (不是第 0 轮)
/// - `turn_count % interval == 0` (恰好是 interval 的倍数)
///
/// # 示例
///
/// ```
/// # use forge::steer_reminder::check_remind_needed;
/// assert!(!check_remind_needed(0, 10));   // 禁用
/// assert!(!check_remind_needed(10, 0));    // 第 0 轮
/// assert!(check_remind_needed(10, 10));    // 第 10 轮
/// assert!(!check_remind_needed(10, 15));   // 非倍数
/// assert!(check_remind_needed(1, 1));      // 每轮触发
/// ```
pub fn check_remind_needed(interval: usize, turn_count: usize) -> bool {
    interval > 0 && turn_count > 0 && turn_count.is_multiple_of(interval)
}

// ============================================================================
//  SteerReminder — 转向提醒
// ============================================================================

/// 转向提醒 — 在对话过长时注入提醒, 防止 AI 跑偏
///
/// 从 `Memory` 构建当前项目状态的简短摘要, 在对话达到 `interval` 的倍数时,
/// 将提醒 prompt 前置到用户消息中发送给 AI。
///
/// 提醒内容简短 (约 200-500 token), 包含:
/// 1. 当前项目终极目标 (1 句话)
/// 2. 当前阶段名称和任务名称
/// 3. 关键架构约束 (SOLID/DIP/TDD/file:格式)
/// 4. "请继续专注于当前任务"的提醒
#[derive(Debug, Clone)]
pub struct SteerReminder {
    /// 项目终极目标
    pub goal: String,
    /// 当前阶段名称
    pub current_phase: String,
    /// 当前任务名称
    pub current_task: String,
    /// 关键架构约束列表
    pub constraints: Vec<String>,
    /// 每隔多少轮触发一次提醒 (0 = 禁用)
    pub interval: usize,
}

impl SteerReminder {
    /// 从 Memory 构建转向提醒
    ///
    /// 提取当前阶段/任务名称和项目目标, 搭配默认架构约束。
    /// 如果 Memory 中没有阶段 (planning 阶段), 阶段/任务名称为空字符串。
    pub fn build_from_memory(memory: &Memory) -> Self {
        let current_phase = extract_phase_name(&memory.phases, memory.current_phase);
        let current_task = extract_task_name(&memory.phases, &memory.current_task);

        Self {
            goal: memory.goal.clone(),
            current_phase,
            current_task,
            constraints: Self::default_constraints(),
            interval: 0, // 默认禁用, 由 Orchestrator 设置
        }
    }

    /// 判断当前对话轮数是否需要注入提醒
    ///
    /// 触发条件:
    /// - `interval > 0` (已启用)
    /// - `turn_count > 0` (不是第 0 轮)
    /// - `turn_count % interval == 0` (恰好是 interval 的倍数)
    ///
    /// 例如 interval=10: 第 10、20、30... 轮触发。
    /// 第 0 轮不触发 (对话刚开始不需要提醒)。
    pub fn should_remind(&self, turn_count: usize) -> bool {
        check_remind_needed(self.interval, turn_count)
    }

    /// 构建简短的提醒 prompt
    ///
    /// 生成的 prompt 约 200-500 token (800-2000 字符), 包含:
    /// - 分隔线 + 🧭 图标
    /// - 项目终极目标
    /// - 当前阶段和任务
    /// - 关键架构约束
    /// - "请继续专注于当前任务"的提醒
    /// - 分隔线结束
    pub fn to_prompt(&self) -> String {
        let mut prompt = String::new();

        prompt.push_str("─── 🧭 转向提醒 ───\n");
        prompt.push_str(&format_goal_line(&self.goal));

        if let Some(phase_line) = format_phase_task_line(&self.current_phase, &self.current_task) {
            prompt.push_str(&phase_line);
        }

        prompt.push_str(&format_constraints_section(&self.constraints));

        prompt.push_str("⚠ 请继续专注于当前任务, 不要偏离目标。\n");
        prompt.push_str("─── 提醒结束 ───\n");

        prompt
    }

    /// 将提醒 prompt 前置到原始消息
    ///
    /// 如果 `should_remind` 返回 false, 返回原始消息不变。
    /// 如果返回 true, 返回 `format!("{}\n\n{}", steer_prompt, original_prompt)`。
    pub fn inject(&self, turn_count: usize, original_prompt: &str) -> String {
        if !self.should_remind(turn_count) {
            return original_prompt.to_string();
        }
        format!("{}\n\n{}", self.to_prompt(), original_prompt)
    }

    /// 默认架构约束 — 从 SYSTEM_CONSTRAINTS.md 获取统一约束
    fn default_constraints() -> Vec<String> {
        vec![
            "详见 .cursorrules 或 constraints/SYSTEM_CONSTRAINTS.md".to_string(),
            "前沿技术/SOLID/Spec-Driven/TDD/代码质量/安全/性能/API/文档".to_string(),
        ]
    }
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{Memory, Phase, PhaseStatus, Task, TaskStatus};

    /// 构建包含完整状态的测试 Memory
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
                    files_written: vec!["Cargo.toml".to_string()],
                    test_result: None,
                    last_good_snapshot: None,
                    clarifications: vec![],
                    depends_on: vec![],
                }],
            },
            Phase {
                id: 1,
                name: "功能实现".to_string(),
                description: "实现核心功能".to_string(),
                status: PhaseStatus::InProgress,
                tasks: vec![Task {
                    id: "1-0".to_string(),
                    phase_id: 1,
                    name: "计算逻辑".to_string(),
                    prompt: "实现计算功能".to_string(),
                    status: TaskStatus::InProgress,
                    result: None,
                    attempts: 1,
                    files_written: vec!["src/calc.rs".to_string()],
                    test_result: None,
                    last_good_snapshot: None,
                    clarifications: vec![],
                    depends_on: vec![],
                }],
            },
        ]);
        mem.current_phase = 1;
        mem.current_task = Some("1-0".to_string());
        mem
    }

    // ========================================================================
    //  纯函数: extract_phase_name 测试
    // ========================================================================

    #[test]
    fn test_extract_phase_name_normal() {
        let phases = vec![
            Phase {
                id: 0,
                name: "阶段A".to_string(),
                description: "".to_string(),
                status: PhaseStatus::Pending,
                tasks: vec![],
            },
            Phase {
                id: 1,
                name: "阶段B".to_string(),
                description: "".to_string(),
                status: PhaseStatus::InProgress,
                tasks: vec![],
            },
        ];
        assert_eq!(extract_phase_name(&phases, 0), "阶段A");
        assert_eq!(extract_phase_name(&phases, 1), "阶段B");
    }

    #[test]
    fn test_extract_phase_name_empty_phases() {
        assert_eq!(extract_phase_name(&[], 0), "");
        assert_eq!(extract_phase_name(&[], 100), "");
    }

    #[test]
    fn test_extract_phase_name_index_out_of_bounds() {
        let phases = vec![Phase {
            id: 0,
            name: "唯一阶段".to_string(),
            description: "".to_string(),
            status: PhaseStatus::Pending,
            tasks: vec![],
        }];
        assert_eq!(extract_phase_name(&phases, 1), "");
        assert_eq!(extract_phase_name(&phases, usize::MAX), "");
    }

    #[test]
    fn test_extract_phase_name_empty_name() {
        let phases = vec![Phase {
            id: 0,
            name: "".to_string(),
            description: "".to_string(),
            status: PhaseStatus::Pending,
            tasks: vec![],
        }];
        assert_eq!(extract_phase_name(&phases, 0), "");
    }

    #[test]
    fn test_extract_phase_name_unicode() {
        let phases = vec![Phase {
            id: 0,
            name: "🚀 初次迭代 — 架构设计".to_string(),
            description: "".to_string(),
            status: PhaseStatus::Pending,
            tasks: vec![],
        }];
        assert_eq!(extract_phase_name(&phases, 0), "🚀 初次迭代 — 架构设计");
    }

    // ========================================================================
    //  纯函数: extract_task_name 测试
    // ========================================================================

    #[test]
    fn test_extract_task_name_found() {
        let phases = vec![Phase {
            id: 0,
            name: "阶段A".to_string(),
            description: "".to_string(),
            status: PhaseStatus::InProgress,
            tasks: vec![Task {
                id: "0-0".to_string(),
                phase_id: 0,
                name: "任务X".to_string(),
                prompt: "".to_string(),
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
        assert_eq!(
            extract_task_name(&phases, &Some("0-0".to_string())),
            "任务X"
        );
    }

    #[test]
    fn test_extract_task_name_none() {
        let phases = vec![Phase {
            id: 0,
            name: "阶段A".to_string(),
            description: "".to_string(),
            status: PhaseStatus::Pending,
            tasks: vec![],
        }];
        assert_eq!(extract_task_name(&phases, &None), "");
    }

    #[test]
    fn test_extract_task_name_not_found() {
        let phases = vec![Phase {
            id: 0,
            name: "阶段A".to_string(),
            description: "".to_string(),
            status: PhaseStatus::Pending,
            tasks: vec![Task {
                id: "0-0".to_string(),
                phase_id: 0,
                name: "任务X".to_string(),
                prompt: "".to_string(),
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
        assert_eq!(extract_task_name(&phases, &Some("不存在".to_string())), "");
    }

    #[test]
    fn test_extract_task_name_empty_phases() {
        assert_eq!(extract_task_name(&[], &Some("0-0".to_string())), "");
        assert_eq!(extract_task_name(&[], &None), "");
    }

    #[test]
    fn test_extract_task_name_cross_phase_search() {
        // 任务在不同阶段中, 应跨阶段搜索
        let phases = vec![
            Phase {
                id: 0,
                name: "阶段A".to_string(),
                description: "".to_string(),
                status: PhaseStatus::Completed,
                tasks: vec![Task {
                    id: "0-0".to_string(),
                    phase_id: 0,
                    name: "任务A0".to_string(),
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
            },
            Phase {
                id: 1,
                name: "阶段B".to_string(),
                description: "".to_string(),
                status: PhaseStatus::InProgress,
                tasks: vec![Task {
                    id: "1-2".to_string(),
                    phase_id: 1,
                    name: "任务B2".to_string(),
                    prompt: "".to_string(),
                    status: TaskStatus::InProgress,
                    result: None,
                    attempts: 0,
                    files_written: vec![],
                    test_result: None,
                    last_good_snapshot: None,
                    clarifications: vec![],
                    depends_on: vec![],
                }],
            },
        ];
        // 应在阶段B中找到 1-2
        assert_eq!(
            extract_task_name(&phases, &Some("1-2".to_string())),
            "任务B2"
        );
        // 应在阶段A中找到 0-0
        assert_eq!(
            extract_task_name(&phases, &Some("0-0".to_string())),
            "任务A0"
        );
    }

    #[test]
    fn test_extract_task_name_empty_task_name() {
        let phases = vec![Phase {
            id: 0,
            name: "阶段A".to_string(),
            description: "".to_string(),
            status: PhaseStatus::Pending,
            tasks: vec![Task {
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
            }],
        }];
        // 空任务名 → 返回空字符串
        assert_eq!(extract_task_name(&phases, &Some("0-0".to_string())), "");
    }

    #[test]
    fn test_extract_task_name_unicode_id() {
        let phases = vec![Phase {
            id: 0,
            name: "阶段".to_string(),
            description: "".to_string(),
            status: PhaseStatus::Pending,
            tasks: vec![Task {
                id: "任务-α".to_string(),
                phase_id: 0,
                name: "Unicode 任务".to_string(),
                prompt: "".to_string(),
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
        assert_eq!(
            extract_task_name(&phases, &Some("任务-α".to_string())),
            "Unicode 任务"
        );
    }

    // ========================================================================
    //  纯函数: format_goal_line 测试
    // ========================================================================

    #[test]
    fn test_format_goal_line_normal() {
        assert_eq!(format_goal_line("构建计算器"), "📌 项目目标: 构建计算器\n");
    }

    #[test]
    fn test_format_goal_line_empty() {
        assert_eq!(format_goal_line(""), "📌 项目目标: \n");
    }

    #[test]
    fn test_format_goal_line_unicode() {
        let result = format_goal_line("🚀 目标 α & β");
        assert!(result.contains("🚀 目标 α & β"));
        assert!(result.starts_with("📌"));
        assert!(result.ends_with('\n'));
    }

    #[test]
    fn test_format_goal_line_with_newlines() {
        // goal 包含换行符, 应原样保留 (调用方负责清理)
        let result = format_goal_line("第一行\n第二行");
        assert!(result.contains("第一行\n第二行"));
    }

    // ========================================================================
    //  纯函数: format_phase_task_line 测试
    // ========================================================================

    #[test]
    fn test_format_phase_task_line_both_present() {
        let result = format_phase_task_line("阶段A", "任务B").unwrap();
        assert_eq!(result, "📋 当前阶段: 阶段A | 任务: 任务B\n");
    }

    #[test]
    fn test_format_phase_task_line_phase_only() {
        let result = format_phase_task_line("阶段A", "").unwrap();
        assert_eq!(result, "📋 当前阶段: 阶段A\n");
    }

    #[test]
    fn test_format_phase_task_line_phase_empty() {
        // 阶段为空时返回 None (即使任务非空)
        assert_eq!(format_phase_task_line("", "任务B"), None);
    }

    #[test]
    fn test_format_phase_task_line_both_empty() {
        assert_eq!(format_phase_task_line("", ""), None);
    }

    #[test]
    fn test_format_phase_task_line_unicode() {
        let result = format_phase_task_line("阶段🚀", "任务🌟").unwrap();
        assert!(result.contains("阶段🚀"));
        assert!(result.contains("任务🌟"));
    }

    // ========================================================================
    //  纯函数: format_constraints_section 测试
    // ========================================================================

    #[test]
    fn test_format_constraints_section_empty() {
        assert_eq!(format_constraints_section(&[]), "");
    }

    #[test]
    fn test_format_constraints_section_single() {
        let result = format_constraints_section(&["约束A".to_string()]);
        assert!(result.contains("🔧 约束:"));
        assert!(result.contains("  - 约束A"));
        assert!(result.ends_with('\n'));
    }

    #[test]
    fn test_format_constraints_section_multiple() {
        let result = format_constraints_section(&[
            "约束A".to_string(),
            "约束B".to_string(),
            "约束C".to_string(),
        ]);
        assert!(result.contains("  - 约束A"));
        assert!(result.contains("  - 约束B"));
        assert!(result.contains("  - 约束C"));
        // 应只有一行标题
        assert_eq!(result.matches("🔧 约束:").count(), 1);
    }

    #[test]
    fn test_format_constraints_section_empty_string_item() {
        // 空字符串约束也应被格式化 (边缘情况)
        let result = format_constraints_section(&["".to_string()]);
        assert!(result.contains("  - \n"));
    }

    #[test]
    fn test_format_constraints_section_unicode() {
        let result = format_constraints_section(&["🔒 安全: 不硬编码密钥".to_string()]);
        assert!(result.contains("🔒 安全: 不硬编码密钥"));
    }

    #[test]
    fn test_format_constraints_section_many_items() {
        // 大量约束项 (边界测试)
        let constraints: Vec<String> = (0..100).map(|i| format!("约束{}", i)).collect();
        let result = format_constraints_section(&constraints);
        // 每个约束都应出现
        for i in 0..100 {
            assert!(result.contains(&format!("约束{}", i)));
        }
    }

    // ========================================================================
    //  纯函数: check_remind_needed 测试
    // ========================================================================

    #[test]
    fn test_check_remind_needed_disabled() {
        assert!(!check_remind_needed(0, 0));
        assert!(!check_remind_needed(0, 10));
        assert!(!check_remind_needed(0, 100));
    }

    #[test]
    fn test_check_remind_needed_zero_turn() {
        assert!(!check_remind_needed(10, 0));
        assert!(!check_remind_needed(1, 0));
    }

    #[test]
    fn test_check_remind_needed_at_multiples() {
        assert!(check_remind_needed(10, 10));
        assert!(check_remind_needed(10, 20));
        assert!(check_remind_needed(10, 100));
    }

    #[test]
    fn test_check_remind_needed_not_at_multiples() {
        assert!(!check_remind_needed(10, 1));
        assert!(!check_remind_needed(10, 9));
        assert!(!check_remind_needed(10, 11));
        assert!(!check_remind_needed(10, 15));
    }

    #[test]
    fn test_check_remind_needed_interval_1() {
        // 每轮都触发 (除第 0 轮)
        assert!(!check_remind_needed(1, 0));
        assert!(check_remind_needed(1, 1));
        assert!(check_remind_needed(1, 2));
        assert!(check_remind_needed(1, 100));
    }

    #[test]
    fn test_check_remind_needed_usize_max() {
        // 极端值: interval = usize::MAX
        assert!(!check_remind_needed(usize::MAX, 0));
        assert!(!check_remind_needed(usize::MAX, 1)); // 1 不是 usize::MAX 的倍数
                                                      // usize::MAX 是 usize::MAX 的倍数 (1 倍)
        assert!(check_remind_needed(usize::MAX, usize::MAX));

        // 极端值: turn_count = usize::MAX, interval = 10
        // usize::MAX % 10 不一定为 0, 只测试不 panic
        let _ = check_remind_needed(10, usize::MAX);
    }

    #[test]
    fn test_check_remind_needed_large_interval() {
        // interval = 1000
        assert!(!check_remind_needed(1000, 999));
        assert!(check_remind_needed(1000, 1000));
        assert!(check_remind_needed(1000, 2000));
    }

    // ========================================================================
    //  build_from_memory 测试
    // ========================================================================

    #[test]
    fn test_build_from_memory_full() {
        let mem = make_full_memory();
        let reminder = SteerReminder::build_from_memory(&mem);

        assert_eq!(reminder.goal, "构建一个 CLI 计算器");
        assert_eq!(reminder.current_phase, "功能实现");
        assert_eq!(reminder.current_task, "计算逻辑");
        assert!(!reminder.constraints.is_empty());
        assert_eq!(reminder.interval, 0); // 默认禁用
    }

    #[test]
    fn test_build_from_memory_empty() {
        let mem = Memory::new("空目标");
        let reminder = SteerReminder::build_from_memory(&mem);

        assert_eq!(reminder.goal, "空目标");
        assert_eq!(reminder.current_phase, "");
        assert_eq!(reminder.current_task, "");
        assert!(!reminder.constraints.is_empty()); // 约束始终有默认值
    }

    #[test]
    fn test_build_from_memory_no_current_task() {
        let mut mem = make_full_memory();
        mem.current_task = None;
        let reminder = SteerReminder::build_from_memory(&mem);

        assert_eq!(reminder.current_phase, "功能实现");
        assert_eq!(reminder.current_task, ""); // 没有当前任务时为空
    }

    #[test]
    fn test_build_from_memory_no_phases() {
        let mem = Memory::new("test goal");
        let reminder = SteerReminder::build_from_memory(&mem);

        assert_eq!(reminder.current_phase, "");
        assert_eq!(reminder.current_task, "");
        assert_eq!(reminder.goal, "test goal");
    }

    #[test]
    fn test_build_from_memory_phase_index_out_of_bounds() {
        // current_phase 超出 phases 范围 → 阶段名为空
        let mut mem = make_full_memory();
        mem.current_phase = 999; // 只有 2 个阶段 (0, 1)
        let reminder = SteerReminder::build_from_memory(&mem);
        assert_eq!(reminder.current_phase, "");
        // current_task 仍可跨阶段搜索到
        assert_eq!(reminder.current_task, "计算逻辑");
    }

    #[test]
    fn test_build_from_memory_task_id_not_found() {
        let mut mem = make_full_memory();
        mem.current_task = Some("nonexistent-id".to_string());
        let reminder = SteerReminder::build_from_memory(&mem);
        assert_eq!(reminder.current_phase, "功能实现");
        assert_eq!(reminder.current_task, "");
    }

    #[test]
    fn test_constraints_not_empty() {
        let mem = Memory::new("test");
        let reminder = SteerReminder::build_from_memory(&mem);
        assert!(!reminder.constraints.is_empty());
        // 约束应引用 .cursorrules 或 SYSTEM_CONSTRAINTS.md
        assert!(reminder
            .constraints
            .iter()
            .any(|c| c.contains(".cursorrules") || c.contains("SYSTEM_CONSTRAINTS.md")));
    }

    #[test]
    fn test_build_from_memory_empty_goal() {
        let mem = Memory::new("");
        let reminder = SteerReminder::build_from_memory(&mem);
        assert_eq!(reminder.goal, "");
        assert_eq!(reminder.current_phase, "");
        assert_eq!(reminder.current_task, "");
    }

    #[test]
    fn test_build_from_memory_unicode_goal() {
        let mem = Memory::new("🚀 构建 α & β 系统 — 测试用");
        let reminder = SteerReminder::build_from_memory(&mem);
        assert_eq!(reminder.goal, "🚀 构建 α & β 系统 — 测试用");
    }

    // ========================================================================
    //  should_remind 测试 (通过 SteerReminder 方法, 内部调用纯函数)
    // ========================================================================

    #[test]
    fn test_should_remind_disabled() {
        let mem = Memory::new("test");
        let mut reminder = SteerReminder::build_from_memory(&mem);
        reminder.interval = 0; // 禁用

        assert!(!reminder.should_remind(0));
        assert!(!reminder.should_remind(10));
        assert!(!reminder.should_remind(100));
    }

    #[test]
    fn test_should_remind_at_interval_multiples() {
        let mem = Memory::new("test");
        let mut reminder = SteerReminder::build_from_memory(&mem);
        reminder.interval = 10;

        // 第 10, 20, 30 轮应触发
        assert!(reminder.should_remind(10));
        assert!(reminder.should_remind(20));
        assert!(reminder.should_remind(30));
    }

    #[test]
    fn test_should_remind_not_at_multiples() {
        let mem = Memory::new("test");
        let mut reminder = SteerReminder::build_from_memory(&mem);
        reminder.interval = 10;

        // 第 1-9, 11-19 轮不触发
        assert!(!reminder.should_remind(0));
        assert!(!reminder.should_remind(1));
        assert!(!reminder.should_remind(9));
        assert!(!reminder.should_remind(11));
        assert!(!reminder.should_remind(15));
    }

    #[test]
    fn test_should_remind_zero_turn_not_triggered() {
        let mem = Memory::new("test");
        let mut reminder = SteerReminder::build_from_memory(&mem);
        reminder.interval = 10;

        // 第 0 轮不触发 (对话刚开始不需要提醒)
        assert!(!reminder.should_remind(0));
    }

    #[test]
    fn test_should_remind_interval_1() {
        let mem = Memory::new("test");
        let mut reminder = SteerReminder::build_from_memory(&mem);
        reminder.interval = 1;

        // 每轮都触发
        assert!(!reminder.should_remind(0));
        assert!(reminder.should_remind(1));
        assert!(reminder.should_remind(2));
        assert!(reminder.should_remind(3));
    }

    #[test]
    fn test_should_remind_interval_5() {
        let mem = Memory::new("test");
        let mut reminder = SteerReminder::build_from_memory(&mem);
        reminder.interval = 5;

        assert!(!reminder.should_remind(0));
        assert!(!reminder.should_remind(1));
        assert!(!reminder.should_remind(4));
        assert!(reminder.should_remind(5));
        assert!(!reminder.should_remind(6));
        assert!(reminder.should_remind(10));
        assert!(reminder.should_remind(15));
    }

    #[test]
    fn test_should_remind_large_turn_count() {
        let mem = Memory::new("test");
        let mut reminder = SteerReminder::build_from_memory(&mem);
        reminder.interval = 100;

        assert!(reminder.should_remind(100));
        assert!(reminder.should_remind(200));
        assert!(reminder.should_remind(1000));
        assert!(!reminder.should_remind(99));
        assert!(!reminder.should_remind(101));
    }

    // ========================================================================
    //  to_prompt 测试
    // ========================================================================

    #[test]
    fn test_to_prompt_contains_goal() {
        let mem = make_full_memory();
        let reminder = SteerReminder::build_from_memory(&mem);
        let prompt = reminder.to_prompt();

        assert!(prompt.contains("构建一个 CLI 计算器"));
        assert!(prompt.contains("项目目标"));
    }

    #[test]
    fn test_to_prompt_contains_phase_and_task() {
        let mem = make_full_memory();
        let reminder = SteerReminder::build_from_memory(&mem);
        let prompt = reminder.to_prompt();

        assert!(prompt.contains("功能实现"));
        assert!(prompt.contains("计算逻辑"));
        assert!(prompt.contains("当前阶段"));
    }

    #[test]
    fn test_to_prompt_contains_constraints() {
        let mem = Memory::new("test");
        let reminder = SteerReminder::build_from_memory(&mem);
        let prompt = reminder.to_prompt();

        assert!(prompt.contains(".cursorrules") || prompt.contains("SYSTEM_CONSTRAINTS.md"));
        assert!(prompt.contains("约束"));
    }

    #[test]
    fn test_to_prompt_contains_reminder_text() {
        let mem = Memory::new("test");
        let reminder = SteerReminder::build_from_memory(&mem);
        let prompt = reminder.to_prompt();

        assert!(prompt.contains("转向提醒"));
        assert!(prompt.contains("专注于当前任务"));
        assert!(prompt.contains("提醒结束"));
    }

    #[test]
    fn test_to_prompt_empty_state() {
        let mem = Memory::new("空目标");
        let reminder = SteerReminder::build_from_memory(&mem);
        let prompt = reminder.to_prompt();

        // 即使空状态也应包含目标
        assert!(prompt.contains("空目标"));
        assert!(prompt.contains("转向提醒"));
        // 不应包含阶段/任务行 (因为阶段名为空)
        assert!(!prompt.contains("当前阶段"));
    }

    #[test]
    fn test_to_prompt_phase_only_no_task() {
        let mut mem = make_full_memory();
        mem.current_task = None;
        let reminder = SteerReminder::build_from_memory(&mem);
        let prompt = reminder.to_prompt();

        // 有阶段但没任务
        assert!(prompt.contains("功能实现"));
        assert!(!prompt.contains("计算逻辑")); // 没有任务名
    }

    // ===== prompt 长度控制 =====

    #[test]
    fn test_to_prompt_reasonable_length() {
        let mem = make_full_memory();
        let reminder = SteerReminder::build_from_memory(&mem);
        let prompt = reminder.to_prompt();

        // 提醒应简短: 200-2000 字符 (约 50-500 token)
        let len = prompt.chars().count();
        assert!(len >= 100, "提醒 prompt 过短: {} 字符", len);
        assert!(len <= 2000, "提醒 prompt 过长: {} 字符", len);
    }

    #[test]
    fn test_to_prompt_with_custom_constraints() {
        let mut reminder = SteerReminder::build_from_memory(&Memory::new("test"));
        reminder.constraints = vec!["自定义约束A".to_string(), "自定义约束B".to_string()];
        let prompt = reminder.to_prompt();
        assert!(prompt.contains("自定义约束A"));
        assert!(prompt.contains("自定义约束B"));
    }

    #[test]
    fn test_to_prompt_with_empty_constraints() {
        let mut reminder = SteerReminder::build_from_memory(&Memory::new("test"));
        reminder.constraints = vec![];
        let prompt = reminder.to_prompt();
        // 空约束不应出现约束区块
        assert!(!prompt.contains("🔧 约束:"));
        // 但其他部分应正常
        assert!(prompt.contains("转向提醒"));
        assert!(prompt.contains("test"));
    }

    #[test]
    fn test_to_prompt_structure_order() {
        // 验证 prompt 中各部分的顺序
        let mem = make_full_memory();
        let reminder = SteerReminder::build_from_memory(&mem);
        let prompt = reminder.to_prompt();

        let header_pos = prompt.find("转向提醒").unwrap();
        let goal_pos = prompt.find("项目目标").unwrap();
        let phase_pos = prompt.find("当前阶段").unwrap();
        let constraint_pos = prompt.find("约束").unwrap();
        let focus_pos = prompt.find("专注于当前任务").unwrap();
        let end_pos = prompt.find("提醒结束").unwrap();

        assert!(header_pos < goal_pos, "标题应在目标前");
        assert!(goal_pos < phase_pos, "目标应在阶段前");
        assert!(phase_pos < constraint_pos, "阶段应在约束前");
        assert!(constraint_pos < focus_pos, "约束应在提醒前");
        assert!(focus_pos < end_pos, "提醒应在结束前");
    }

    // ========================================================================
    //  inject 测试
    // ========================================================================

    #[test]
    fn test_inject_when_should_remind() {
        let mem = make_full_memory();
        let mut reminder = SteerReminder::build_from_memory(&mem);
        reminder.interval = 10;

        let original = "请实现 XXX 功能";
        let result = reminder.inject(10, original); // 第 10 轮, 应触发

        // 应包含提醒 + 原始消息
        assert!(result.contains("转向提醒"));
        assert!(result.contains("请实现 XXX 功能"));
        // 提醒应在前面
        let remind_pos = result.find("转向提醒").unwrap();
        let original_pos = result.find("请实现 XXX 功能").unwrap();
        assert!(remind_pos < original_pos);
    }

    #[test]
    fn test_inject_when_should_not_remind() {
        let mem = make_full_memory();
        let mut reminder = SteerReminder::build_from_memory(&mem);
        reminder.interval = 10;

        let original = "请实现 XXX 功能";
        let result = reminder.inject(5, original); // 第 5 轮, 不应触发

        // 应只返回原始消息
        assert_eq!(result, original);
    }

    #[test]
    fn test_inject_disabled() {
        let mem = make_full_memory();
        let mut reminder = SteerReminder::build_from_memory(&mem);
        reminder.interval = 0; // 禁用

        let original = "请实现 XXX 功能";
        let result = reminder.inject(100, original);

        assert_eq!(result, original);
    }

    #[test]
    fn test_inject_zero_turn() {
        let mem = make_full_memory();
        let mut reminder = SteerReminder::build_from_memory(&mem);
        reminder.interval = 10;

        let original = "请实现 XXX 功能";
        let result = reminder.inject(0, original); // 第 0 轮, 不触发

        assert_eq!(result, original);
    }

    #[test]
    fn test_inject_preserves_original_content() {
        let mem = make_full_memory();
        let mut reminder = SteerReminder::build_from_memory(&mem);
        reminder.interval = 1;

        let original = "line1\nline2\nline3\n```file:src/main.rs\nfn main() {}\n```";
        let result = reminder.inject(1, original);

        // 原始内容应完整保留
        assert!(result.contains("line1"));
        assert!(result.contains("line2"));
        assert!(result.contains("line3"));
        assert!(result.contains("```file:src/main.rs"));
        assert!(result.contains("fn main() {}"));
    }

    #[test]
    fn test_inject_empty_original() {
        let mem = make_full_memory();
        let mut reminder = SteerReminder::build_from_memory(&mem);
        reminder.interval = 1;

        let result = reminder.inject(1, "");
        // 空原始消息注入后应只有提醒部分 + 两个换行
        assert!(result.contains("转向提醒"));
        assert!(result.ends_with("\n\n"));
    }

    #[test]
    fn test_inject_very_long_original() {
        let mem = make_full_memory();
        let mut reminder = SteerReminder::build_from_memory(&mem);
        reminder.interval = 1;

        let original = "x".repeat(10000);
        let result = reminder.inject(1, &original);
        assert!(result.contains("转向提醒"));
        assert!(result.contains(&"x".repeat(100)));
    }

    #[test]
    fn test_inject_unicode_original() {
        let mem = make_full_memory();
        let mut reminder = SteerReminder::build_from_memory(&mem);
        reminder.interval = 1;

        let original = "实现 🚀 功能 — 中文测试";
        let result = reminder.inject(1, original);
        assert!(result.contains("🚀"));
        assert!(result.contains("中文测试"));
    }

    #[test]
    fn test_inject_multiple_turns_only_fires_at_multiples() {
        let mem = make_full_memory();
        let mut reminder = SteerReminder::build_from_memory(&mem);
        reminder.interval = 5;

        // 第 1-4 轮: 不注入
        for turn in 1..=4 {
            let result = reminder.inject(turn, "msg");
            assert_eq!(result, "msg", "第 {} 轮不应注入", turn);
        }
        // 第 5 轮: 注入
        let result5 = reminder.inject(5, "msg");
        assert!(result5.contains("转向提醒"));
        // 第 6-9 轮: 不注入
        for turn in 6..=9 {
            let result = reminder.inject(turn, "msg");
            assert_eq!(result, "msg", "第 {} 轮不应注入", turn);
        }
        // 第 10 轮: 注入
        let result10 = reminder.inject(10, "msg");
        assert!(result10.contains("转向提醒"));
    }

    // ========================================================================
    //  与上下文衔接的关系测试
    // ========================================================================

    #[test]
    fn test_steer_resets_after_handoff() {
        // 交接后 turn_count = 0, 转向提醒也应重新计数
        let mem = Memory::new("test");
        let mut reminder = SteerReminder::build_from_memory(&mem);
        reminder.interval = 10;

        // 交接前: 第 10 轮触发
        assert!(reminder.should_remind(10));

        // 交接后: 轮数清零, 重新计数
        // (should_remind 只看 turn_count, 交接后 turn_count=0)
        assert!(!reminder.should_remind(0)); // 交接后立即不触发
        assert!(reminder.should_remind(10)); // 再过 10 轮才触发
    }

    #[test]
    fn test_steer_interval_less_than_handoff() {
        // 推荐配置: steer_interval < max_context_turns
        // 如 steer_interval=10, max_context_turns=30
        // → 第 10, 20 轮注入提醒, 第 30 轮触发交接
        let mem = Memory::new("test");
        let mut reminder = SteerReminder::build_from_memory(&mem);
        reminder.interval = 10;
        let max_context_turns = 30;

        // 第 10 轮: 注入提醒
        assert!(reminder.should_remind(10));
        // 第 20 轮: 再次注入提醒
        assert!(reminder.should_remind(20));
        // 第 30 轮: 应触发交接 (steer 也触发, 但交接优先)
        assert!(reminder.should_remind(30));
        assert_eq!(max_context_turns, 30);
    }

    // ========================================================================
    //  多次调用稳定性
    // ========================================================================

    #[test]
    fn test_to_prompt_deterministic() {
        let mem = make_full_memory();
        let reminder = SteerReminder::build_from_memory(&mem);

        let prompt1 = reminder.to_prompt();
        let prompt2 = reminder.to_prompt();

        assert_eq!(prompt1, prompt2, "相同状态应生成相同的提醒 prompt");
    }

    #[test]
    fn test_build_from_memory_multiple_calls() {
        let mem = make_full_memory();

        let r1 = SteerReminder::build_from_memory(&mem);
        let r2 = SteerReminder::build_from_memory(&mem);

        assert_eq!(r1.goal, r2.goal);
        assert_eq!(r1.current_phase, r2.current_phase);
        assert_eq!(r1.current_task, r2.current_task);
        assert_eq!(r1.constraints, r2.constraints);
    }

    // ========================================================================
    //  纯函数与方法的集成一致性测试
    // ========================================================================

    #[test]
    fn test_should_remind_matches_pure_function() {
        let mem = Memory::new("test");
        let mut reminder = SteerReminder::build_from_memory(&mem);

        for interval in [0usize, 1, 5, 10, 100] {
            reminder.interval = interval;
            for turn in [0usize, 1, 4, 5, 9, 10, 15, 20, 99, 100] {
                assert_eq!(
                    reminder.should_remind(turn),
                    check_remind_needed(interval, turn),
                    "不一致: interval={}, turn={}",
                    interval,
                    turn
                );
            }
        }
    }

    #[test]
    fn test_build_from_memory_uses_pure_extractors() {
        let mem = make_full_memory();
        let reminder = SteerReminder::build_from_memory(&mem);

        // build_from_memory 内部调用的纯函数结果应一致
        assert_eq!(
            reminder.current_phase,
            extract_phase_name(&mem.phases, mem.current_phase)
        );
        assert_eq!(
            reminder.current_task,
            extract_task_name(&mem.phases, &mem.current_task)
        );
    }
}
