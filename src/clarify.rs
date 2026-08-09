//! 自主提问能力 — HeuristicClarificationChecker
//!
//! **核心中的核心** — Agent 自主判断 AI 回复是否需要追问澄清。
//!
//! ## 检测策略 (启发式规则)
//!
//! 1. **直接提问检测** — AI 回复中包含疑问句 (中英文)
//! 2. **不确定标记检测** — AI 表达了不确定/模糊 (如"可能"、"perhaps")
//! 3. **超时检测** — AI 回复超时 (可能未完成)
//! 4. **过短回复检测** — 回复内容过短 (可能不完整)
//! 5. **防循环机制** — 追问次数上限 + 重复问题检测
//!
//! ## 追问策略
//!
//! 当检测到需要追问时, 生成一条追问消息, 要求 AI 做出决策并继续编码,
//! 而非等待人类回答。这是"自主"的核心 — Agent 自主决策, 不依赖人类。

use crate::traits::{ClarificationChecker, ClarificationContext, ClarificationResult};
use async_trait::async_trait;

/// 启发式澄清检查器 — 默认实现
///
/// 使用规则匹配检测 AI 回复中的疑问、不确定、超时等情况,
/// 并生成自主追问消息 (要求 AI 自行决策而非等待人类回答)。
pub struct HeuristicClarificationChecker {
    /// 回复最小长度阈值 (低于此值视为过短)
    min_response_len: usize,
}

impl Default for HeuristicClarificationChecker {
    fn default() -> Self {
        Self::new()
    }
}

impl HeuristicClarificationChecker {
    pub fn new() -> Self {
        Self {
            min_response_len: 20,
        }
    }

    /// 设置最小回复长度阈值
    pub fn with_min_response_len(mut self, len: usize) -> Self {
        self.min_response_len = len;
        self
    }

    /// 检测 AI 回复中是否包含直接提问
    ///
    /// 匹配中英文疑问模式:
    /// - 中文: "请告诉我", "你希望", "需要什么", "选哪个", 以"？"结尾
    /// - 英文: "please clarify", "would you like", "which do you", 以"?"结尾
    fn detect_question(&self, text: &str) -> Option<String> {
        let lower = text.to_lowercase();

        // 中文提问模式
        let zh_patterns = [
            "请告诉我",
            "请明确",
            "请选择",
            "你希望",
            "你需要",
            "你想要",
            "请问",
            "需要什么",
            "用什么",
            "选哪个",
            "如何处理",
            "不确定",
            "无法确定",
            "不清楚",
        ];

        for pattern in &zh_patterns {
            if text.contains(pattern) {
                return Some(format!("AI 回复中包含提问标记: \"{}\"", pattern));
            }
        }

        // 英文提问模式
        let en_patterns = [
            "please clarify",
            "please specify",
            "what would you",
            "which do you",
            "could you tell",
            "would you like",
            "do you prefer",
            "should i use",
            "i'm not sure",
            "i cannot determine",
            "it depends",
        ];

        for pattern in &en_patterns {
            if lower.contains(pattern) {
                return Some(format!(
                    "AI reply contains question marker: \"{}\"",
                    pattern
                ));
            }
        }

        // 检测以问号结尾的句子
        if text.contains('？') || text.contains('?') {
            // 只有当问号出现在非代码块中时才算
            // 简单启发式: 如果文本中有代码块, 且问号在代码块外
            if let Some(reason) = self.detect_question_mark_outside_code(text) {
                return Some(reason);
            }
        }

        None
    }

    /// 检测代码块外的问号
    fn detect_question_mark_outside_code(&self, text: &str) -> Option<String> {
        let mut in_code_block = false;
        for line in text.lines() {
            if line.trim_start().starts_with("```") {
                in_code_block = !in_code_block;
                continue;
            }
            if !in_code_block && (line.contains('？') || line.contains('?')) {
                return Some("AI 回复中包含问号 (代码块外)".to_string());
            }
        }
        None
    }

    /// 检测不确定/模糊标记
    fn detect_uncertainty(&self, text: &str) -> Option<String> {
        let lower = text.to_lowercase();

        let zh_uncertainty = ["两种方案", "多个选择", "或者你可以", "要么"];
        for pattern in &zh_uncertainty {
            if text.contains(pattern) {
                return Some(format!("AI 回复中包含不确定标记: \"{}\"", pattern));
            }
        }

        let en_uncertainty = [
            "either option",
            "option a",
            "option b",
            "multiple approaches",
        ];
        for pattern in &en_uncertainty {
            if lower.contains(pattern) {
                return Some(format!("AI reply contains uncertainty: \"{}\"", pattern));
            }
        }

        None
    }

    /// 检测回复是否过短 (可能不完整)
    fn detect_too_short(&self, text: &str) -> Option<String> {
        let stripped = text.trim();
        if stripped.len() < self.min_response_len {
            return Some(format!(
                "AI 回复过短 ({}字符 < {}阈值)",
                stripped.len(),
                self.min_response_len
            ));
        }
        None
    }

    /// 生成追问消息 — 要求 AI 自主决策并继续编码
    ///
    /// 核心策略: 不等待人类回答, 而是要求 AI 根据项目需求自行选择最佳方案。
    fn build_follow_up(&self, reason: &str) -> String {
        format!(
            "请根据项目需求自行选择最合适的方案，并直接用 ```file:路径``` 格式输出所有代码文件。\n\
             不要再提问，直接做出最佳决策并开始编码。\n\
             （检测到需要澄清的原因: {}）",
            reason
        )
    }

    /// 检测追问是否与之前的问题重复 (简单相似度)
    ///
    /// 如果新追问的前 30 个字符与某个已问过的问题相同, 视为重复。
    fn is_duplicate(&self, question: &str, previous: &[String]) -> bool {
        let prefix: String = question.chars().take(30).collect();
        for prev in previous {
            let prev_prefix: String = prev.chars().take(30).collect();
            if prefix == prev_prefix {
                return true;
            }
        }
        false
    }
}

#[async_trait]
impl ClarificationChecker for HeuristicClarificationChecker {
    async fn check(&self, response: &str, context: &ClarificationContext) -> ClarificationResult {
        // 防循环: 超过最大追问次数, 不再追问
        if !context.can_ask_more() {
            return ClarificationResult::no();
        }

        // 优先级 1: 超时检测 — 超时意味着回复可能被截断
        if context.timed_out {
            let reason = "AI 回复超时,可能未完成".to_string();
            let question = "你之前的回复似乎未完成。请继续完成回复，直接用 ```file:路径``` 格式输出所有代码文件。\n\
                 不要重复已有内容，只输出剩余部分。"
                .to_string();
            if self.is_duplicate(&question, &context.previous_questions) {
                return ClarificationResult::no();
            }
            return ClarificationResult::yes(question, reason);
        }

        // 优先级 2: 直接提问检测
        if let Some(reason) = self.detect_question(response) {
            let question = self.build_follow_up(&reason);
            if self.is_duplicate(&question, &context.previous_questions) {
                return ClarificationResult::no();
            }
            return ClarificationResult::yes(question, reason);
        }

        // 优先级 3: 不确定标记检测
        if let Some(reason) = self.detect_uncertainty(response) {
            let question = self.build_follow_up(&reason);
            if self.is_duplicate(&question, &context.previous_questions) {
                return ClarificationResult::no();
            }
            return ClarificationResult::yes(question, reason);
        }

        // 优先级 4: 过短回复检测
        if let Some(reason) = self.detect_too_short(response) {
            let question = self.build_follow_up(&reason);
            if self.is_duplicate(&question, &context.previous_questions) {
                return ClarificationResult::no();
            }
            return ClarificationResult::yes(question, reason);
        }

        ClarificationResult::no()
    }
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 创建默认上下文 (0 次追问, 最大 2 次)
    fn ctx() -> ClarificationContext {
        ClarificationContext {
            task_prompt: "创建一个 Rust CLI 工具".to_string(),
            timed_out: false,
            questions_asked: 0,
            max_questions: 2,
            previous_questions: vec![],
        }
    }

    /// 创建已追问 2 次的上下文 (达到上限)
    fn ctx_maxed() -> ClarificationContext {
        ClarificationContext {
            task_prompt: "test".to_string(),
            timed_out: false,
            questions_asked: 2,
            max_questions: 2,
            previous_questions: vec!["q1".to_string(), "q2".to_string()],
        }
    }

    // ===== 基本行为测试 =====

    #[tokio::test]
    async fn test_normal_code_response_no_clarification() {
        let checker = HeuristicClarificationChecker::new();
        let response = "好的，我来创建项目。\n```file:src/main.rs\nfn main() {\n    println!(\"hello\");\n}\n```\n```file:Cargo.toml\n[package]\nname = \"test\"\nversion = \"0.1.0\"\n```";
        let result = checker.check(response, &ctx()).await;
        assert!(!result.needs_clarification, "正常代码回复不应需要追问");
    }

    #[tokio::test]
    async fn test_empty_response_no_clarification_when_short_threshold_low() {
        // 空回复但未超时应触发过短检测
        let checker = HeuristicClarificationChecker::new();
        let result = checker.check("", &ctx()).await;
        assert!(result.needs_clarification, "空回复应触发过短检测");
        assert!(result.reason.contains("过短"));
    }

    // ===== 直接提问检测 =====

    #[tokio::test]
    async fn test_detect_chinese_question_marker() {
        let checker = HeuristicClarificationChecker::new();
        let response = "你希望使用哪种框架？是 Actix 还是 Axum？";
        let result = checker.check(response, &ctx()).await;
        assert!(result.needs_clarification, "包含中文提问标记应触发追问");
        assert!(!result.question.is_empty());
    }

    #[tokio::test]
    async fn test_detect_chinese_question_please_tell() {
        let checker = HeuristicClarificationChecker::new();
        let response = "请告诉我你想要实现什么功能？";
        let result = checker.check(response, &ctx()).await;
        assert!(result.needs_clarification);
    }

    #[tokio::test]
    async fn test_detect_english_question_marker() {
        let checker = HeuristicClarificationChecker::new();
        let response = "Would you like me to use Tokio or async-std for this project?";
        let result = checker.check(response, &ctx()).await;
        assert!(result.needs_clarification, "包含英文提问标记应触发追问");
    }

    #[tokio::test]
    async fn test_detect_english_please_clarify() {
        let checker = HeuristicClarificationChecker::new();
        let response = "Please clarify what kind of CLI commands you need.";
        let result = checker.check(response, &ctx()).await;
        assert!(result.needs_clarification);
    }

    #[tokio::test]
    async fn test_question_mark_outside_code_triggers() {
        let checker = HeuristicClarificationChecker::new();
        let response = "I will create the project. What name should the package have?\n```file:src/main.rs\nfn main() {}\n```";
        let result = checker.check(response, &ctx()).await;
        assert!(result.needs_clarification, "代码块外的问号应触发追问");
    }

    #[tokio::test]
    async fn test_question_mark_inside_code_does_not_trigger() {
        let checker = HeuristicClarificationChecker::new();
        let response = "Here is the code:\n```file:src/main.rs\nfn main() {\n    let x: Option<i32> = Some(42)?;\n    println!(\"{}\");\n}\n```";
        let result = checker.check(response, &ctx()).await;
        // 代码块内的 ? 不应触发
        assert!(!result.needs_clarification, "代码块内的问号不应触发追问");
    }

    // ===== 不确定标记检测 =====

    #[tokio::test]
    async fn test_detect_uncertainty_chinese() {
        let checker = HeuristicClarificationChecker::new();
        let response = "对于这个项目，有两种方案可以选择。方案A使用 clap，方案B使用 structopt。";
        let result = checker.check(response, &ctx()).await;
        assert!(result.needs_clarification, "不确定标记应触发追问");
    }

    #[tokio::test]
    async fn test_detect_uncertainty_english() {
        let checker = HeuristicClarificationChecker::new();
        let response = "There are multiple approaches we could take. Option A is simpler, Option B is more robust.";
        let result = checker.check(response, &ctx()).await;
        assert!(result.needs_clarification);
    }

    // ===== 超时检测 =====

    #[tokio::test]
    async fn test_timed_out_triggers_clarification() {
        let checker = HeuristicClarificationChecker::new();
        let mut context = ctx();
        context.timed_out = true;
        // 即使回复看起来正常, 超时也应触发
        let response = "Here is some code:\n```file:src/main.rs\nfn main() {}\n```";
        let result = checker.check(response, &context).await;
        assert!(result.needs_clarification, "超时应触发追问");
        assert!(result.reason.contains("超时"));
        assert!(result.question.contains("未完成"));
    }

    #[tokio::test]
    async fn test_timed_out_has_higher_priority_than_question() {
        let checker = HeuristicClarificationChecker::new();
        let mut context = ctx();
        context.timed_out = true;
        let response = "你希望用什么框架？";
        let result = checker.check(response, &context).await;
        // 超时应优先
        assert!(result.reason.contains("超时"));
    }

    // ===== 过短回复检测 =====

    #[tokio::test]
    async fn test_short_response_triggers_clarification() {
        let checker = HeuristicClarificationChecker::new();
        let result = checker.check("ok", &ctx()).await;
        assert!(result.needs_clarification, "过短回复应触发追问");
        assert!(result.reason.contains("过短"));
    }

    #[tokio::test]
    async fn test_response_above_threshold_no_clarification() {
        let checker = HeuristicClarificationChecker::new().with_min_response_len(10);
        let response = "这是一段足够长的回复，超过了阈值。";
        let result = checker.check(response, &ctx()).await;
        assert!(!result.needs_clarification);
    }

    // ===== 防循环机制 =====

    #[tokio::test]
    async fn test_max_questions_reached_no_clarification() {
        let checker = HeuristicClarificationChecker::new();
        let response = "你希望用哪种框架？";
        let result = checker.check(response, &ctx_maxed()).await;
        assert!(!result.needs_clarification, "达到最大追问次数后不应再追问");
    }

    #[tokio::test]
    async fn test_duplicate_question_not_asked_again() {
        let checker = HeuristicClarificationChecker::new();
        let response = "你希望用哪种框架？";
        // 先问一次
        let result1 = checker.check(response, &ctx()).await;
        assert!(result1.needs_clarification);

        // 再问一次 (模拟已问过)
        let context = ClarificationContext {
            task_prompt: "test".to_string(),
            timed_out: false,
            questions_asked: 1,
            max_questions: 2,
            previous_questions: vec![result1.question.clone()],
        };
        let result2 = checker.check(response, &context).await;
        // 不应再追问 (重复)
        assert!(!result2.needs_clarification, "重复的问题不应再追问");
    }

    #[tokio::test]
    async fn test_can_ask_one_more_after_first() {
        let checker = HeuristicClarificationChecker::new();
        let response = "你希望用哪种框架？";
        let context = ClarificationContext {
            task_prompt: "test".to_string(),
            timed_out: false,
            questions_asked: 1,
            max_questions: 2,
            previous_questions: vec!["some other question".to_string()],
        };
        let result = checker.check(response, &context).await;
        assert!(result.needs_clarification, "未达上限且非重复时应可追问");
    }

    // ===== 追问消息质量 =====

    #[tokio::test]
    async fn test_follow_up_contains_code_format_instruction() {
        let checker = HeuristicClarificationChecker::new();
        let response = "你希望用哪种框架？";
        let result = checker.check(response, &ctx()).await;
        assert!(
            result.question.contains("file:路径"),
            "追问消息应包含文件格式要求"
        );
    }

    #[tokio::test]
    async fn test_follow_up_asks_ai_to_decide() {
        let checker = HeuristicClarificationChecker::new();
        let response = "你希望用哪种框架？";
        let result = checker.check(response, &ctx()).await;
        assert!(
            result.question.contains("自行选择") || result.question.contains("最佳决策"),
            "追问消息应要求 AI 自主决策"
        );
    }

    #[tokio::test]
    async fn test_reason_is_descriptive() {
        let checker = HeuristicClarificationChecker::new();
        let response = "请问需要添加哪些依赖？";
        let result = checker.check(response, &ctx()).await;
        assert!(!result.reason.is_empty(), "追问原因不应为空");
        assert!(
            result.reason.contains("提问标记") || result.reason.contains("问号"),
            "追问原因应描述触发原因: {}",
            result.reason
        );
    }

    // ===== ClarificationResult 辅助方法 =====

    #[tokio::test]
    async fn test_clarification_result_no() {
        let result = ClarificationResult::no();
        assert!(!result.needs_clarification);
        assert!(result.question.is_empty());
        assert!(result.reason.is_empty());
    }

    #[tokio::test]
    async fn test_clarification_result_yes() {
        let result = ClarificationResult::yes("追问消息", "原因");
        assert!(result.needs_clarification);
        assert_eq!(result.question, "追问消息");
        assert_eq!(result.reason, "原因");
    }

    // ===== ClarificationContext 辅助方法 =====

    #[tokio::test]
    async fn test_context_can_ask_more() {
        let ctx1 = ClarificationContext {
            task_prompt: "t".to_string(),
            timed_out: false,
            questions_asked: 0,
            max_questions: 2,
            previous_questions: vec![],
        };
        assert!(ctx1.can_ask_more());

        let ctx2 = ClarificationContext {
            task_prompt: "t".to_string(),
            timed_out: false,
            questions_asked: 2,
            max_questions: 2,
            previous_questions: vec![],
        };
        assert!(!ctx2.can_ask_more());
    }

    // ===== 边界情况 =====

    #[tokio::test]
    async fn test_whitespace_only_response_triggers_short() {
        let checker = HeuristicClarificationChecker::new();
        let result = checker.check("   \n   \t  ", &ctx()).await;
        assert!(result.needs_clarification, "纯空白回复应触发过短检测");
    }

    #[tokio::test]
    async fn test_normal_long_response_no_clarification() {
        let checker = HeuristicClarificationChecker::new();
        let response = "我来帮你创建这个项目。首先创建 Cargo.toml 文件，然后创建 src/main.rs 文件。代码结构清晰，功能完整。\n```file:Cargo.toml\n[package]\nname = \"myapp\"\nversion = \"0.1.0\"\nedition = \"2021\"\n```\n```file:src/main.rs\nfn main() {\n    println!(\"Hello, World!\");\n}\n```";
        let result = checker.check(response, &ctx()).await;
        assert!(!result.needs_clarification, "正常完整回复不应需要追问");
    }

    #[tokio::test]
    async fn test_response_with_code_but_also_question_triggers() {
        let checker = HeuristicClarificationChecker::new();
        let response = "```file:src/main.rs\nfn main() {}\n```\n请问你需要添加测试吗？";
        let result = checker.check(response, &ctx()).await;
        assert!(
            result.needs_clarification,
            "即使有代码,包含提问也应触发追问"
        );
    }

    #[tokio::test]
    async fn test_multiple_question_markers_only_triggers_once() {
        let checker = HeuristicClarificationChecker::new();
        let response = "你希望用什么框架？需要什么功能？选哪个版本？";
        let result = checker.check(response, &ctx()).await;
        assert!(result.needs_clarification);
        // 只产生一条追问消息
        assert!(!result.question.is_empty());
    }

    // ===== 额外 edge case 测试 =====

    #[tokio::test]
    async fn test_detect_question_what_to_use() {
        let checker = HeuristicClarificationChecker::new();
        let response = "用什么框架比较好？";
        let result = checker.check(response, &ctx()).await;
        assert!(result.needs_clarification);
    }

    #[tokio::test]
    async fn test_detect_question_how_to_handle() {
        let checker = HeuristicClarificationChecker::new();
        let response = "如何处理并发请求？请告诉我。";
        let result = checker.check(response, &ctx()).await;
        assert!(result.needs_clarification);
    }

    #[tokio::test]
    async fn test_detect_question_cannot_determine() {
        let checker = HeuristicClarificationChecker::new();
        let response = "无法确定使用哪个版本。";
        let result = checker.check(response, &ctx()).await;
        assert!(result.needs_clarification);
    }

    #[tokio::test]
    async fn test_detect_uncertainty_either_option() {
        let checker = HeuristicClarificationChecker::new();
        let response = "We could use either option A or option B for this task.";
        let result = checker.check(response, &ctx()).await;
        assert!(result.needs_clarification);
    }

    #[tokio::test]
    async fn test_detect_uncertainty_multiple_approaches() {
        let checker = HeuristicClarificationChecker::new();
        let response = "There are multiple approaches to solve this. Let me think.";
        let result = checker.check(response, &ctx()).await;
        assert!(result.needs_clarification);
    }

    #[tokio::test]
    async fn test_detect_uncertainty_or_you_can() {
        let checker = HeuristicClarificationChecker::new();
        let response = "或者你可以选择另一种方案。";
        let result = checker.check(response, &ctx()).await;
        assert!(result.needs_clarification);
    }

    #[tokio::test]
    async fn test_custom_min_response_len() {
        let checker = HeuristicClarificationChecker::new().with_min_response_len(5);
        // 3 字符 < 5 阈值
        let result = checker.check("abc", &ctx()).await;
        assert!(result.needs_clarification);
        assert!(result.reason.contains("过短"));
    }

    #[tokio::test]
    async fn test_custom_min_response_len_high() {
        let checker = HeuristicClarificationChecker::new().with_min_response_len(1000);
        // 20 字符 < 1000 阈值
        let result = checker.check("这是一段普通长度的回复。", &ctx()).await;
        assert!(result.needs_clarification);
        assert!(result.reason.contains("过短"));
    }

    #[tokio::test]
    async fn test_timed_out_with_empty_response() {
        let checker = HeuristicClarificationChecker::new();
        let mut context = ctx();
        context.timed_out = true;
        let result = checker.check("", &context).await;
        // 超时优先于过短检测
        assert!(result.needs_clarification);
        assert!(result.reason.contains("超时"));
    }

    #[tokio::test]
    async fn test_duplicate_timed_out_question() {
        let checker = HeuristicClarificationChecker::new();
        let mut context = ctx();
        context.timed_out = true;
        // 先问一次
        let result1 = checker.check("some response", &context).await;
        assert!(result1.needs_clarification);

        // 设置 previous_questions 包含之前的追问
        let context2 = ClarificationContext {
            task_prompt: "test".to_string(),
            timed_out: true,
            questions_asked: 1,
            max_questions: 3,
            previous_questions: vec![result1.question.clone()],
        };
        let result2 = checker.check("some response", &context2).await;
        // 重复的追问不应再触发
        assert!(!result2.needs_clarification);
    }

    #[tokio::test]
    async fn test_question_mark_in_inline_code() {
        let checker = HeuristicClarificationChecker::new();
        // 行内代码中的 ? 不在 ``` 代码块内
        // 但行内 `code?` 不被代码块检测过滤
        let response = "Here is the code:\n```file:src/main.rs\nfn main() {}\n```\nDo you want `Option<T>?` handling?";
        let result = checker.check(response, &ctx()).await;
        // 代码块外有问号 → 应触发
        assert!(result.needs_clarification);
    }

    #[tokio::test]
    async fn test_english_it_depends() {
        let checker = HeuristicClarificationChecker::new();
        let response = "It depends on your use case. If you need performance, use approach A. If you need simplicity, use approach B.";
        let result = checker.check(response, &ctx()).await;
        assert!(result.needs_clarification);
    }

    #[tokio::test]
    async fn test_follow_up_contains_self_decision_instruction() {
        let checker = HeuristicClarificationChecker::new();
        let response = "你希望用哪种框架？";
        let result = checker.check(response, &ctx()).await;
        assert!(
            result.question.contains("自行选择") || result.question.contains("最佳决策"),
            "追问消息应要求 AI 自主决策"
        );
        assert!(
            result.question.contains("不要再提问"),
            "追问消息应明确要求不要再提问"
        );
    }

    #[tokio::test]
    async fn test_checker_with_default() {
        let checker = HeuristicClarificationChecker::default();
        // Default 和 new() 行为一致
        let result = checker.check("ok", &ctx()).await;
        assert!(result.needs_clarification, "过短回复应触发追问");
    }

    #[tokio::test]
    async fn test_max_questions_boundary() {
        let checker = HeuristicClarificationChecker::new();
        // questions_asked == max_questions → 不能再问
        let context = ClarificationContext {
            task_prompt: "test".to_string(),
            timed_out: false,
            questions_asked: 5,
            max_questions: 5,
            previous_questions: vec![],
        };
        let result = checker.check("你希望用什么？", &context).await;
        assert!(!result.needs_clarification, "达到上限不应再追问");
    }

    #[tokio::test]
    async fn test_one_question_below_max() {
        let checker = HeuristicClarificationChecker::new();
        // questions_asked = max - 1 → 还能问一次
        let context = ClarificationContext {
            task_prompt: "test".to_string(),
            timed_out: false,
            questions_asked: 4,
            max_questions: 5,
            previous_questions: vec!["previous question text".to_string()],
        };
        let result = checker.check("你希望用什么框架？", &context).await;
        assert!(result.needs_clarification, "未达上限应可追问");
    }
}
