//! 人工干预接口实现 — 方向 A
//!
//! 提供三种 `HumanInteraction` 实现:
//! - `AutoApprove`: 自动批准一切 (默认, 全自主模式)
//! - `CliInteraction`: CLI 交互式确认 (人类通过终端 y/n 确认)
//! - `MockInteraction`: 测试版, 预编程响应

use crate::traits::{FixContext, HumanInteraction, PlanInfo, TaskAction, TaskInfo};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Mutex;

// ============================================================================
//  AutoApprove — 自动批准一切 (默认实现)
// ============================================================================

/// 自动批准 — 全自主模式, 不暂停, 自动确认一切
///
/// 这是默认实现, 保持与原有行为的向后兼容。
/// 所有决策点都自动通过:
/// - 计划确认 → true
/// - 任务确认 → Execute
/// - 修复确认 → true
/// - 需求变更确认 → true
pub struct AutoApprove;

#[async_trait]
impl HumanInteraction for AutoApprove {
    async fn confirm_planning(&self, _plan: &PlanInfo) -> Result<bool> {
        Ok(true)
    }

    async fn confirm_task(&self, _task: &TaskInfo) -> Result<TaskAction> {
        Ok(TaskAction::Execute)
    }

    async fn confirm_fix(&self, _context: &FixContext) -> Result<bool> {
        Ok(true)
    }

    async fn confirm_requirement_change(&self, _changes_summary: &str) -> Result<bool> {
        Ok(true)
    }
}

// ============================================================================
//  CliInteraction — CLI 交互式确认
// ============================================================================

/// CLI 交互式确认 — 通过终端让人类确认每个决策点
///
/// 在关键决策点暂停, 在终端显示信息并等待人类输入:
/// - 计划确认: 显示计划, 等待 y/n
/// - 任务确认: 显示任务, 等待 y(执行)/s(跳过)/a(中止)
/// - 修复确认: 显示失败信息, 等待 y/n
/// - 需求变更确认: 显示变更, 等待 y/n
///
/// 启用方式: `forge run --interactive "目标"`
pub struct CliInteraction;

impl CliInteraction {
    pub fn new() -> Self {
        Self
    }

    /// 从 stdin 读取一行输入, 去除首尾空白
    fn read_line_trimmed() -> String {
        let mut input = String::new();
        let _ = std::io::stdin().read_line(&mut input);
        input.trim().to_lowercase()
    }

    /// 显示计划摘要
    fn display_plan(plan: &PlanInfo) {
        println!("\n{}", "═".repeat(60));
        println!("  📋 AI 开发计划 — 请确认");
        println!("{}", "═".repeat(60));
        println!("  终极目标: {}", plan.goal);
        println!("  阶段数: {}", plan.phases.len());
        for (i, phase) in plan.phases.iter().enumerate() {
            println!(
                "\n  阶段 {}/{}: {} — {}",
                i + 1,
                plan.phases.len(),
                phase.name,
                phase.description
            );
            for (j, task) in phase.tasks.iter().enumerate() {
                println!("    任务 {}.{}: {}", i + 1, j + 1, task.name);
            }
        }
        println!("\n{}", "═".repeat(60));
    }

    /// 显示任务信息
    fn display_task(task: &TaskInfo) {
        println!("\n{}", "─".repeat(60));
        println!("  ▶ 任务: {} (ID: {})", task.name, task.id);
        let preview: String = task.prompt.chars().take(200).collect();
        println!("  指令预览: {}...", preview);
        println!("{}", "─".repeat(60));
    }

    /// 显示修复上下文
    fn display_fix(context: &FixContext) {
        println!("\n{}", "─".repeat(60));
        println!(
            "  🔄 修复重试 {}/{} (阶段 {}, 任务 {})",
            context.attempt,
            context.max_attempts,
            context.phase_idx + 1,
            context.task_idx + 1
        );
        let preview: String = context.feedback.chars().take(500).collect();
        println!("  上次失败反馈:\n{}", preview);
        println!("{}", "─".repeat(60));
    }

    /// 显示需求变更
    fn display_changes(changes_summary: &str) {
        println!("\n{}", "═".repeat(60));
        println!("  🔄 需求变更 — 请确认");
        println!("{}", "═".repeat(60));
        println!("{}", changes_summary);
        println!("{}", "═".repeat(60));
    }
}

impl Default for CliInteraction {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl HumanInteraction for CliInteraction {
    async fn confirm_planning(&self, plan: &PlanInfo) -> Result<bool> {
        Self::display_plan(plan);
        print!("确认执行此计划? [Y/n] ");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let input = Self::read_line_trimmed();
        let approved = input.is_empty() || input == "y" || input == "yes";
        if approved {
            println!("  ✅ 计划已确认");
        } else {
            println!("  ❌ 计划被拒绝, 终止开发");
        }
        Ok(approved)
    }

    async fn confirm_task(&self, task: &TaskInfo) -> Result<TaskAction> {
        Self::display_task(task);
        print!("执行此任务? [Y]es / [S]kip / [A]bort (默认 Y): ");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let input = Self::read_line_trimmed();
        let action = if input.is_empty() || input == "y" || input == "yes" {
            TaskAction::Execute
        } else if input == "s" || input == "skip" {
            println!("  ⏭ 跳过任务: {}", task.name);
            TaskAction::Skip
        } else if input == "a" || input == "abort" {
            println!("  🛑 终止开发");
            TaskAction::Abort
        } else {
            TaskAction::Execute
        };
        Ok(action)
    }

    async fn confirm_fix(&self, context: &FixContext) -> Result<bool> {
        Self::display_fix(context);
        print!("继续修复? [Y/n] ");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let input = Self::read_line_trimmed();
        let approved = input.is_empty() || input == "y" || input == "yes";
        if approved {
            println!("  🔄 继续修复");
        } else {
            println!("  ⏭ 跳过修复, 标记为失败");
        }
        Ok(approved)
    }

    async fn confirm_requirement_change(&self, changes_summary: &str) -> Result<bool> {
        Self::display_changes(changes_summary);
        print!("处理需求变更? [Y/n] ");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let input = Self::read_line_trimmed();
        let approved = input.is_empty() || input == "y" || input == "yes";
        if approved {
            println!("  🔄 处理需求变更");
        } else {
            println!("  ⏭ 跳过需求变更");
        }
        Ok(approved)
    }
}

// ============================================================================
//  MockInteraction — 测试版, 预编程响应
// ============================================================================

/// 测试版人工干预 — 预编程响应
///
/// 用于集成测试: 可以设置每个决策点的返回值。
/// 内部使用 Mutex 保证线程安全。
///
/// # 示例
/// ```no_run
/// use forge::interaction::MockInteraction;
/// use forge::traits::{HumanInteraction, TaskAction, PlanInfo};
///
/// let mock = MockInteraction::new()
///     .with_plan_response(false)
///     .with_task_response(TaskAction::Skip);
/// ```
pub struct MockInteraction {
    /// 计划确认返回值 (默认 true)
    plan_response: Mutex<bool>,
    /// 任务确认返回值队列 (按顺序弹出, 空时使用 default_task_response)
    task_responses: Mutex<Vec<TaskAction>>,
    /// 默认任务确认返回值 (队列空时使用)
    default_task_response: Mutex<TaskAction>,
    /// 修复确认返回值 (默认 true)
    fix_response: Mutex<bool>,
    /// 需求变更确认返回值 (默认 true)
    change_response: Mutex<bool>,
    /// 记录每个方法被调用的次数
    pub call_counts: MockCallCounts,
}

/// Mock 调用计数 — 记录每个方法被调用的次数
#[derive(Debug, Default)]
pub struct MockCallCounts {
    pub confirm_planning: std::sync::atomic::AtomicU32,
    pub confirm_task: std::sync::atomic::AtomicU32,
    pub confirm_fix: std::sync::atomic::AtomicU32,
    pub confirm_requirement_change: std::sync::atomic::AtomicU32,
}

impl MockInteraction {
    pub fn new() -> Self {
        Self {
            plan_response: Mutex::new(true),
            task_responses: Mutex::new(vec![]),
            default_task_response: Mutex::new(TaskAction::Execute),
            fix_response: Mutex::new(true),
            change_response: Mutex::new(true),
            call_counts: MockCallCounts::default(),
        }
    }

    /// 设置计划确认的返回值
    pub fn with_plan_response(self, response: bool) -> Self {
        *self.plan_response.lock().unwrap() = response;
        self
    }

    /// 设置任务确认的返回值 (所有后续任务都用此值)
    pub fn with_task_response(self, response: TaskAction) -> Self {
        *self.default_task_response.lock().unwrap() = response;
        self
    }

    /// 设置任务确认的返回值序列 (按顺序弹出, 用完后回退到默认值)
    ///
    /// 用于测试“第一次跳过, 第二次执行”等场景。
    pub fn with_task_responses(self, responses: Vec<TaskAction>) -> Self {
        *self.task_responses.lock().unwrap() = responses;
        self
    }

    /// 设置修复确认的返回值
    pub fn with_fix_response(self, response: bool) -> Self {
        *self.fix_response.lock().unwrap() = response;
        self
    }

    /// 设置需求变更确认的返回值
    pub fn with_change_response(self, response: bool) -> Self {
        *self.change_response.lock().unwrap() = response;
        self
    }
}

impl Default for MockInteraction {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl HumanInteraction for MockInteraction {
    async fn confirm_planning(&self, _plan: &PlanInfo) -> Result<bool> {
        self.call_counts
            .confirm_planning
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(*self.plan_response.lock().unwrap())
    }

    async fn confirm_task(&self, _task: &TaskInfo) -> Result<TaskAction> {
        self.call_counts
            .confirm_task
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // 先尝试从队列弹出
        let mut queue = self.task_responses.lock().unwrap();
        if !queue.is_empty() {
            return Ok(queue.remove(0));
        }
        drop(queue);
        // 回退到默认值
        Ok(self.default_task_response.lock().unwrap().clone())
    }

    async fn confirm_fix(&self, _context: &FixContext) -> Result<bool> {
        self.call_counts
            .confirm_fix
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(*self.fix_response.lock().unwrap())
    }

    async fn confirm_requirement_change(&self, _changes_summary: &str) -> Result<bool> {
        self.call_counts
            .confirm_requirement_change
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(*self.change_response.lock().unwrap())
    }
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::{FixContext, PhaseInfo, PlanInfo, TaskAction, TaskInfo};

    fn make_plan() -> PlanInfo {
        PlanInfo {
            goal: "构建一个 CLI 工具".to_string(),
            phases: vec![PhaseInfo {
                name: "初始化".to_string(),
                description: "创建项目结构".to_string(),
                tasks: vec![TaskInfo {
                    id: "0-0".to_string(),
                    name: "初始化项目".to_string(),
                    prompt: "创建 Cargo.toml 和 main.rs".to_string(),
                }],
            }],
        }
    }

    fn make_task() -> TaskInfo {
        TaskInfo {
            id: "0-0".to_string(),
            name: "测试任务".to_string(),
            prompt: "执行测试".to_string(),
        }
    }

    fn make_fix_context() -> FixContext {
        FixContext {
            phase_idx: 0,
            task_idx: 0,
            attempt: 2,
            max_attempts: 3,
            feedback: "编译错误: mismatched types".to_string(),
        }
    }

    // ===== AutoApprove =====

    #[tokio::test]
    async fn test_auto_approve_planning() {
        let auto = AutoApprove;
        let result = auto.confirm_planning(&make_plan()).await.unwrap();
        assert!(result);
    }

    #[tokio::test]
    async fn test_auto_approve_task() {
        let auto = AutoApprove;
        let result = auto.confirm_task(&make_task()).await.unwrap();
        assert_eq!(result, TaskAction::Execute);
    }

    #[tokio::test]
    async fn test_auto_approve_fix() {
        let auto = AutoApprove;
        let result = auto.confirm_fix(&make_fix_context()).await.unwrap();
        assert!(result);
    }

    #[tokio::test]
    async fn test_auto_approve_change() {
        let auto = AutoApprove;
        let result = auto
            .confirm_requirement_change("变更1\n变更2")
            .await
            .unwrap();
        assert!(result);
    }

    // ===== MockInteraction =====

    #[tokio::test]
    async fn test_mock_default_approves_all() {
        let mock = MockInteraction::new();
        assert!(mock.confirm_planning(&make_plan()).await.unwrap());
        assert_eq!(
            mock.confirm_task(&make_task()).await.unwrap(),
            TaskAction::Execute
        );
        assert!(mock.confirm_fix(&make_fix_context()).await.unwrap());
        assert!(mock.confirm_requirement_change("变更").await.unwrap());
    }

    #[tokio::test]
    async fn test_mock_reject_planning() {
        let mock = MockInteraction::new().with_plan_response(false);
        let result = mock.confirm_planning(&make_plan()).await.unwrap();
        assert!(!result);
    }

    #[tokio::test]
    async fn test_mock_skip_task() {
        let mock = MockInteraction::new().with_task_response(TaskAction::Skip);
        let result = mock.confirm_task(&make_task()).await.unwrap();
        assert_eq!(result, TaskAction::Skip);
    }

    #[tokio::test]
    async fn test_mock_abort_task() {
        let mock = MockInteraction::new().with_task_response(TaskAction::Abort);
        let result = mock.confirm_task(&make_task()).await.unwrap();
        assert_eq!(result, TaskAction::Abort);
    }

    #[tokio::test]
    async fn test_mock_reject_fix() {
        let mock = MockInteraction::new().with_fix_response(false);
        let result = mock.confirm_fix(&make_fix_context()).await.unwrap();
        assert!(!result);
    }

    #[tokio::test]
    async fn test_mock_reject_change() {
        let mock = MockInteraction::new().with_change_response(false);
        let result = mock.confirm_requirement_change("变更").await.unwrap();
        assert!(!result);
    }

    #[tokio::test]
    async fn test_mock_call_counts() {
        let mock = MockInteraction::new();
        let _ = mock.confirm_planning(&make_plan()).await;
        let _ = mock.confirm_task(&make_task()).await;
        let _ = mock.confirm_fix(&make_fix_context()).await;
        let _ = mock.confirm_requirement_change("变更").await;

        assert_eq!(
            mock.call_counts
                .confirm_planning
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        assert_eq!(
            mock.call_counts
                .confirm_task
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        assert_eq!(
            mock.call_counts
                .confirm_fix
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        assert_eq!(
            mock.call_counts
                .confirm_requirement_change
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
    }

    #[tokio::test]
    async fn test_mock_multiple_calls_independent() {
        let mock = MockInteraction::new();
        // 第一次调用默认 Execute
        assert_eq!(
            mock.confirm_task(&make_task()).await.unwrap(),
            TaskAction::Execute
        );
        // 第二次也是 Execute
        assert_eq!(
            mock.confirm_task(&make_task()).await.unwrap(),
            TaskAction::Execute
        );
        assert_eq!(
            mock.call_counts
                .confirm_task
                .load(std::sync::atomic::Ordering::Relaxed),
            2
        );
    }

    #[tokio::test]
    async fn test_mock_builder_chain() {
        let mock = MockInteraction::new()
            .with_plan_response(false)
            .with_task_response(TaskAction::Skip)
            .with_fix_response(false)
            .with_change_response(false);

        assert!(!mock.confirm_planning(&make_plan()).await.unwrap());
        assert_eq!(
            mock.confirm_task(&make_task()).await.unwrap(),
            TaskAction::Skip
        );
        assert!(!mock.confirm_fix(&make_fix_context()).await.unwrap());
        assert!(!mock.confirm_requirement_change("变更").await.unwrap());
    }
}
