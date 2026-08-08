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

use crate::memory::Memory;

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
        // 当前阶段名称
        let current_phase = memory
            .current_phase()
            .map(|p| p.name.clone())
            .unwrap_or_default();

        // 当前任务名称
        let current_task = memory
            .current_task
            .as_ref()
            .and_then(|task_id| {
                memory
                    .phases
                    .iter()
                    .flat_map(|p| &p.tasks)
                    .find(|t| &t.id == task_id)
                    .map(|t| t.name.clone())
            })
            .unwrap_or_default();

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
        self.interval > 0 && turn_count > 0 && turn_count.is_multiple_of(self.interval)
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
        prompt.push_str(&format!("📌 项目目标: {}\n", self.goal));

        if !self.current_phase.is_empty() {
            prompt.push_str(&format!("📋 当前阶段: {}", self.current_phase));
            if !self.current_task.is_empty() {
                prompt.push_str(&format!(" | 任务: {}", self.current_task));
            }
            prompt.push('\n');
        }

        if !self.constraints.is_empty() {
            prompt.push_str("🔧 约束:\n");
            for c in &self.constraints {
                prompt.push_str(&format!("  - {}\n", c));
            }
        }

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

    // ===== build_from_memory 测试 =====

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

    // ===== should_remind 测试 =====

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

    // ===== to_prompt 测试 =====

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

    // ===== inject 测试 =====

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

    // ===== 与上下文衔接的关系测试 =====

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

    // ===== 多次调用稳定性 =====

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
}
