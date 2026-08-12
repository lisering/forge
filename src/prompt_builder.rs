//! Prompt 构建器 — 统一管理系统级开发约束和规范
//!
//! 将所有发送给 AI 的 prompt 中需要包含的架构约束、开发规范、
//! 技术要求集中管理，确保每次与 AI 交互都携带完整的开发指令。
//!
//! ## 核心约束
//!
//! 1. **前沿技术** — 使用最新最前沿的技术和研究成果
//! 2. **SOLID 原则** — SRP/OCP/LSP/ISP/DIP
//! 3. **Spec-Driven Development** — Mission → Tech Stack → Roadmap → Feature Phase
//! 4. **TDD** — 先写测试再写实现
//! 5. **代码质量** — 可编译、可测试、可维护

// ============================================================================
//  SystemPrompt — 系统级开发约束
// ============================================================================

/// 系统级开发约束 — 注入到所有发送给 AI 的 prompt 中
///
/// 包含:
/// - 前沿技术要求
/// - SOLID 架构原则
/// - Spec-Driven Development 流程
/// - TDD 开发模式
/// - 代码质量标准
/// - 文件输出格式
#[derive(Debug, Clone)]
pub struct SystemPrompt;

impl SystemPrompt {
    /// 构建完整的系统级约束 prompt
    ///
    /// 约束详情见项目根目录 constraints/SYSTEM_CONSTRAINTS.md
    /// 此方法生成简化的约束引用，完整约束请查看附件或约束文件
    pub fn build() -> String {
        Self::build_attachment_reference()
    }

    /// 构建规划阶段专用约束 — 在拆解目标时注入
    pub fn build_for_planning() -> String {
        Self::build()
    }

    /// 构建任务执行专用约束 — 在执行任务时注入
    pub fn build_for_task() -> String {
        Self::build()
    }

    /// 构建简短约束摘要 — 用于上下文衔接等 token 受限场景
    pub fn build_brief() -> String {
        let mut prompt = String::new();

        prompt.push_str("─── 🔧 开发约束 ───\n");
        prompt.push_str("  • 详见项目根目录 .cursorrules 或 constraints/SYSTEM_CONSTRAINTS.md\n");
        prompt.push_str("  • 前沿技术/SOLID/Spec-Driven/TDD/代码质量/安全/性能/API/文档\n");
        prompt.push_str("─── 约束结束 ───\n\n");

        prompt
    }

    /// 构建附件引用模式 — 用于支持附件上传的 AI (DeepSeek/Z.ai)
    ///
    /// 当 AI 支持文件上传时，使用此模式：
    /// 1. 上传 SYSTEM_CONSTRAINTS.md 附件
    /// 2. 使用此 prompt 引用附件
    ///
    /// 优势：
    /// - 减少主 prompt 的 token 消耗
    /// - 约束可以更长更详细
    /// - 便于版本管理和复用
    pub fn build_attachment_reference() -> String {
        let mut prompt = String::new();

        prompt.push_str("╔══════════════════════════════════════════════════════════════════╗\n");
        prompt.push_str("║  🔥 FORGE 系统级开发约束 — 必须严格执行的铁律 🔥                  ║\n");
        prompt.push_str("╚══════════════════════════════════════════════════════════════════╝\n\n");

        prompt.push_str("⚠️  铁律声明 (违反将导致代码被拒绝):\n");
        prompt.push_str("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
        prompt.push_str("❌ 禁止: 不遵循附件《Forge 系统级开发约束》的任何行为\n");
        prompt.push_str("❌ 禁止: 跳过测试直接写实现代码\n");
        prompt.push_str("❌ 禁止: 使用 unwrap()/expect() 而不处理错误\n");
        prompt.push_str("❌ 禁止: 输出不完整的文件内容或省略代码\n");
        prompt.push_str("❌ 禁止: 生成无效格式的 Cargo.toml\n");
        prompt.push_str("❌ 禁止: 违反 SOLID 原则 (特别是 DIP 依赖倒置)\n");
        prompt.push_str("❌ 禁止: 在单元测试中访问真实的外部依赖\n");
        prompt.push_str("❌ 禁止: 大括号/圆括号/方括号不配对 (最常见的 AI 代码生成错误)\n");
        prompt.push_str("❌ 禁止: 使用 todo!()/unimplemented!()/panic!() (非测试代码)\n");
        prompt
            .push_str("❌ 禁止: 使用 unsafe 块/函数/实现 (非必要不使用, 必须时添加 SAFETY 注释)\n");
        prompt.push_str("❌ 禁止: 使用 unreachable!() 宏 (非测试代码)\n");
        prompt
            .push_str("❌ 禁止: 滥用 unwrap_or()/unwrap_or_default() 掩盖错误 (确认是否应传播)\n");
        prompt.push_str("❌ 禁止: 使用 ? 操作符的函数不返回 Result/Option 类型 (Session 119)\n");
        prompt.push_str(
            "❌ 禁止: 修改函数返回类型为 Result 后遗漏 use anyhow::Result; 导入 (Session 120)\n",
        );
        prompt.push_str(
            "❌ 禁止: 修改函数签名为 Result<T, E> 后遗漏函数体 Ok(...) 包装 (Session 121)\n",
        );
        prompt.push_str(
            "❌ 禁止: 使用 bail!()/ensure!() 宏但未导入 use anyhow::{bail, ensure}; (Session 122)\n\n",
        );

        prompt.push_str("✅ 必须: 严格遵循附件《Forge 系统级开发约束》中的全部 10 大约束\n");
        prompt.push_str("✅ 必须: TDD 模式 — 先写测试，再写实现，最后重构\n");
        prompt.push_str("✅ 必须: 每个公共函数都有对应的单元测试\n");
        prompt.push_str("✅ 必须: 使用 ```file:路径``` 格式输出完整文件内容\n");
        prompt.push_str("✅ 必须: 代码零警告、零 clippy 警告\n");
        prompt.push_str("✅ 必须: 使用 trait 抽象外部依赖，支持无 Chrome 环境测试\n");
        prompt.push_str("✅ 必须: 确保所有 { } ( ) [ ] 配对 — 输出前逐个检查\n");
        prompt.push_str("✅ 必须: 公共 API (pub fn/struct/enum/trait) 有 /// 文档注释\n");
        prompt.push_str(
            "✅ 必须: 返回 Result/Option/bool/Vec/String/&str/Box/Rc/Arc/Cow/PathBuf 的公共函数添加 #[must_use] 属性\n\n",
        );

        prompt.push_str("📎 附件内容 (必须逐条执行):\n");
        prompt.push_str("  1. 前沿技术要求 — 使用最新最前沿的技术\n");
        prompt.push_str("  2. SOLID 架构原则 — SRP/OCP/LSP/ISP/DIP\n");
        prompt.push_str("  3. Spec-Driven Development — Mission→Tech Stack→Roadmap→Feature\n");
        prompt.push_str("  4. TDD 开发模式 — 测试金字塔 70:20:10、Mock 规范\n");
        prompt.push_str("  5. 代码质量标准 — 零警告、anyhow 错误处理\n");
        prompt.push_str("  6. 安全与可靠性 — 输入验证/防御式编程/RAII\n");
        prompt.push_str("  7. 性能与可观测性 — async/await、tracing 追踪\n");
        prompt.push_str("  8. API 设计规范 — RESTful、幂等性、统一错误格式\n");
        prompt.push_str("  9. 文档规范 — README、代码注释、ADR\n");
        prompt.push_str("  10. 文件输出格式 — ```file:路径```、完整 TOML\n\n");

        prompt.push_str("🔴 重要: 如有冲突，以附件《Forge 系统级开发约束》为准。\n");
        prompt.push_str("🔴 重要: 每次回复前，请自检是否违反了上述任何铁律。\n\n");

        prompt.push_str("╔══════════════════════════════════════════════════════════════════╗\n");
        prompt.push_str("║  开始执行 — 请严格遵循上述铁律生成代码                             ║\n");
        prompt.push_str("╚══════════════════════════════════════════════════════════════════╝\n\n");

        prompt
    }
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ===== SystemPrompt::build =====

    #[test]
    fn test_build_contains_attachment_reference() {
        let prompt = SystemPrompt::build();
        assert!(prompt.contains("FORGE 系统级开发约束"), "必须引用约束文件");
        assert!(prompt.contains("铁律"), "必须提及铁律");
        assert!(prompt.contains("禁止:"), "必须列出禁止事项");
        assert!(prompt.contains("必须:"), "必须列出必须事项");
    }

    #[test]
    fn test_build_is_deterministic() {
        let p1 = SystemPrompt::build();
        let p2 = SystemPrompt::build();
        assert_eq!(p1, p2, "SystemPrompt::build() 应是确定性的");
    }

    // ===== SystemPrompt::build_for_planning =====

    #[test]
    fn test_build_for_planning_contains_attachment_ref() {
        let prompt = SystemPrompt::build_for_planning();
        assert!(
            prompt.contains("FORGE 系统级开发约束"),
            "规划 prompt 必须引用约束文件"
        );
        assert!(prompt.contains("铁律"), "规划 prompt 必须提及铁律");
    }

    // ===== SystemPrompt::build_for_task =====

    #[test]
    fn test_build_for_task_contains_attachment_ref() {
        let prompt = SystemPrompt::build_for_task();
        assert!(
            prompt.contains("FORGE 系统级开发约束"),
            "任务 prompt 必须引用约束文件"
        );
        assert!(prompt.contains("铁律"), "任务 prompt 必须提及铁律");
    }

    // ===== build_brief =====

    #[test]
    fn test_build_brief_is_shorter_than_full() {
        let full = SystemPrompt::build();
        let brief = SystemPrompt::build_brief();
        assert!(
            brief.len() < full.len(),
            "简短约束应比完整约束短 ({} < {})",
            brief.len(),
            full.len()
        );
    }

    #[test]
    fn test_build_brief_contains_constraints_ref() {
        let brief = SystemPrompt::build_brief();
        assert!(
            brief.contains(".cursorrules") || brief.contains("SYSTEM_CONSTRAINTS.md"),
            "简短约束必须引用约束文件"
        );
    }

    // ===== 不可变性测试 =====

    #[test]
    fn test_build_for_planning_is_deterministic() {
        let p1 = SystemPrompt::build_for_planning();
        let p2 = SystemPrompt::build_for_planning();
        assert_eq!(p1, p2, "SystemPrompt::build_for_planning() 应是确定性的");
    }

    #[test]
    fn test_build_for_task_is_deterministic() {
        let p1 = SystemPrompt::build_for_task();
        let p2 = SystemPrompt::build_for_task();
        assert_eq!(p1, p2, "SystemPrompt::build_for_task() 应是确定性的");
    }

    // ===== Session 113: 大括号匹配提醒测试 =====

    #[test]
    fn test_build_contains_brace_matching_warning() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("大括号"),
            "系统 prompt 应包含大括号匹配警告"
        );
        assert!(prompt.contains("配对"), "系统 prompt 应包含括号配对提醒");
    }

    #[test]
    fn test_build_contains_brace_check_instruction() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("逐个检查"),
            "系统 prompt 应包含输出前逐个检查括号的指令"
        );
    }

    // ===== Session 114: 代码质量禁止项测试 =====

    #[test]
    fn test_build_contains_todo_unimplemented_panic_warning() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("todo!()"),
            "系统 prompt 应包含 todo!() 禁止项"
        );
        assert!(
            prompt.contains("unimplemented!()"),
            "系统 prompt 应包含 unimplemented!() 禁止项"
        );
        assert!(
            prompt.contains("panic!()"),
            "系统 prompt 应包含 panic!() 禁止项"
        );
    }

    // ===== Session 115: unsafe + 公共 API 文档要求测试 =====

    #[test]
    fn test_build_contains_unsafe_warning() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("unsafe"),
            "系统 prompt 应包含 unsafe 禁止项"
        );
        assert!(
            prompt.contains("SAFETY"),
            "系统 prompt 应提及 SAFETY 注释要求"
        );
    }

    #[test]
    fn test_build_contains_doc_comment_requirement() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("文档注释"),
            "系统 prompt 应包含公共 API 文档注释要求"
        );
        assert!(
            prompt.contains("pub fn/struct/enum/trait"),
            "系统 prompt 应明确列出需要文档注释的公共 API 类型"
        );
    }

    // ===== Session 116: unreachable!() + #[must_use] 要求测试 =====

    #[test]
    fn test_build_contains_unreachable_warning() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("unreachable!()"),
            "系统 prompt 应包含 unreachable!() 禁止项"
        );
    }

    #[test]
    fn test_build_contains_must_use_requirement() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("#[must_use]"),
            "系统 prompt 应包含 #[must_use] 属性要求"
        );
        assert!(
            prompt.contains("Result/Option/bool"),
            "系统 prompt 应明确列出需要 #[must_use] 的返回类型"
        );
    }

    // ===== Session 117: unwrap_or + 扩展 must_use 类型测试 =====

    #[test]
    fn test_build_contains_unwrap_or_warning() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("unwrap_or"),
            "系统 prompt 应包含 unwrap_or() 滥用警告"
        );
        assert!(
            prompt.contains("unwrap_or_default"),
            "系统 prompt 应包含 unwrap_or_default() 滥用警告"
        );
    }

    #[test]
    fn test_build_contains_expanded_must_use_types() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("Vec"),
            "系统 prompt 应在 #[must_use] 要求中包含 Vec 类型"
        );
        assert!(
            prompt.contains("String"),
            "系统 prompt 应在 #[must_use] 要求中包含 String 类型"
        );
        assert!(
            prompt.contains("&str"),
            "系统 prompt 应在 #[must_use] 要求中包含 &str 类型"
        );
    }

    // ===== Session 118: 扩展 must_use 类型测试 =====

    #[test]
    fn test_build_contains_extended_must_use_types() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("Box"),
            "系统 prompt 应在 #[must_use] 要求中包含 Box 类型"
        );
        assert!(
            prompt.contains("Arc"),
            "系统 prompt 应在 #[must_use] 要求中包含 Arc 类型"
        );
        assert!(
            prompt.contains("PathBuf"),
            "系统 prompt 应在 #[must_use] 要求中包含 PathBuf 类型"
        );
    }

    // ===== Session 119: ? 操作符返回类型约束测试 =====

    #[test]
    fn test_build_contains_question_mark_result_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("? 操作符的函数不返回 Result/Option"),
            "系统 prompt 应包含 ? 操作符函数必须返回 Result/Option 的约束"
        );
    }

    #[test]
    fn test_build_contains_anyhow_import_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("use anyhow::Result;"),
            "系统 prompt 应包含 use anyhow::Result 导入约束 (Session 120)"
        );
    }

    // ===== Session 121: Ok 包装约束测试 =====

    #[test]
    fn test_build_contains_ok_wrapping_constraint() {
        let prompt = SystemPrompt::build();
        assert!(
            prompt.contains("Ok(...)"),
            "系统 prompt 应包含 Ok(...) 包装约束 (Session 121)"
        );
    }
}
