//! 循环终止检测 — 借鉴方向 3
//!
//! 在修复循环中检测 AI 是否在原地打转 (同样的编译错误反复出现),
//! 如果检测到死循环则主动改变策略 (换角度提问、建议跳过),
//! 避免浪费 24 小时运行中的宝贵时间和 token。
//!
//! ## 核心思路
//!
//! 如果 AI 连续 N 轮都在修复同一个错误 (同样的编译错误反复出现),
//! 说明当前策略无效。需要检测"是否在原地打转"并主动改变策略。
//!
//! ## 检测维度
//!
//! 1. **错误码重复**: 同一 `error_code` 连续出现 ≥ `max_repeats` 次
//! 2. **消息签名重复**: 同一 `message_signature` 连续出现 ≥ `max_repeats` 次
//! 3. **错误文件重复**: 同一文件连续出现 ≥ `max_repeats` 次错误
//!
//! ## 策略改变
//!
//! - 首次检测到死循环: 在修复 prompt 中追加"你之前已经尝试过修复这个问题但失败了, 请换一种完全不同的方法"
//! - 策略改变后仍然死循环: 建议跳过当前任务 (而非继续浪费修复轮次)
//!
//! ## 与智能错误诊断的关系
//!
//! - **智能错误诊断 (方向 F)**: 分析单个错误的根因 + 分类 + 修复建议
//! - **循环终止检测 (本模块)**: 跨多轮检测"是否在原地打转" + 改变策略
//! - 两者互补: 错误诊断关注"这个错误是什么", 循环终止关注"是否在重复犯同样的错误"

use crate::testrunner::CompileError;
use std::collections::VecDeque;

// ============================================================================
//  ErrorRound — 一轮修复失败的错误记录
// ============================================================================

/// 一轮修复失败的错误记录
///
/// 记录单次编译/测试失败时的错误信息摘要,
/// 用于跨多轮比较是否出现重复。
#[derive(Debug, Clone)]
pub struct ErrorRound {
    /// 本轮所有错误的 error_code 列表 (None 表示无 error code)
    pub codes: Vec<Option<String>>,
    /// 本轮所有错误的消息签名 (error_code + 消息前 100 字符)
    pub signatures: Vec<String>,
    /// 本轮所有错误的文件路径列表
    pub files: Vec<String>,
}

impl ErrorRound {
    /// 从编译错误列表构建一轮记录
    pub fn from_errors(errors: &[CompileError]) -> Self {
        let codes = errors.iter().map(|e| e.error_code.clone()).collect();
        let signatures = errors.iter().map(Self::make_signature).collect();
        let files = errors.iter().map(|e| e.file.clone()).collect();
        Self {
            codes,
            signatures,
            files,
        }
    }

    /// 生成错误签名 (error_code + 消息前 100 字符)
    fn make_signature(error: &CompileError) -> String {
        let msg_part: String = error.message.chars().take(100).collect();
        match &error.error_code {
            Some(code) => format!("[{}] {}", code, msg_part),
            None => msg_part,
        }
    }
}

// ============================================================================
//  LoopDetector — 循环终止检测器
// ============================================================================

/// 循环终止检测器 — 跨多轮检测修复死循环
///
/// 在 `execute_task` 的修复循环中使用:
/// 1. 每次编译/测试失败后调用 `record_errors()` 记录本轮错误
/// 2. 修复前检查 `is_looping()` 是否检测到死循环
/// 3. 如果检测到死循环: 调用 `loop_strategy_prompt()` 获取策略改变 prompt
/// 4. 如果策略改变后仍死循环 (`should_skip()`): 建议跳过当前任务
/// 5. 任务完成后调用 `reset()` 重置, 避免跨任务误检
///
/// `max_repeats == 0` 时禁用 (不检测)。
#[derive(Debug, Clone)]
pub struct LoopDetector {
    /// 最大重复次数 (同一错误出现此次数后判定为死循环)
    pub max_repeats: usize,
    /// 最近的错误轮次记录 (VecDeque, 保留最近 max_repeats 轮)
    pub rounds: VecDeque<ErrorRound>,
    /// 策略是否已改变 (首次检测到死循环后设为 true, 用于升级到建议跳过)
    pub strategy_changed: bool,
}

impl LoopDetector {
    /// 创建循环检测器
    ///
    /// `max_repeats` 为 0 时禁用 (is_looping 始终返回 false)。
    /// 推荐值: 3 (同一错误连续出现 3 次后判定为死循环)。
    pub fn new(max_repeats: usize) -> Self {
        Self {
            max_repeats,
            rounds: VecDeque::new(),
            strategy_changed: false,
        }
    }

    /// 记录本轮失败的错误
    ///
    /// 在每次编译/测试失败后调用。将本轮错误信息存入历史记录。
    /// 如果 `errors` 为空 (如纯测试运行时失败), 仍然记录一轮空记录
    /// (空记录不会触发循环检测, 除非有反馈文本重复)。
    pub fn record_errors(&mut self, errors: &[CompileError]) {
        let round = ErrorRound::from_errors(errors);
        self.rounds.push_back(round);

        // 只保留最近 max_repeats 轮 (避免无限增长)
        while self.rounds.len() > self.max_repeats {
            self.rounds.pop_front();
        }
    }

    /// 检测是否在原地打转 (死循环)
    ///
    /// 检测维度:
    /// 1. 同一 error_code 在最近轮次中出现 ≥ max_repeats 次
    /// 2. 同一 message_signature 出现 ≥ max_repeats 次
    /// 3. 同一文件路径出现 ≥ max_repeats 次
    ///
    /// 任一维度满足即判定为死循环。
    /// `max_repeats == 0` 或轮次数 < max_repeats 时返回 false。
    pub fn is_looping(&self) -> bool {
        if self.max_repeats == 0 || self.rounds.len() < self.max_repeats {
            return false;
        }

        // 维度 1: 错误码重复
        if self.has_repeated_codes() {
            return true;
        }

        // 维度 2: 消息签名重复
        if self.has_repeated_signatures() {
            return true;
        }

        // 维度 3: 错误文件重复
        if self.has_repeated_files() {
            return true;
        }

        false
    }

    /// 检查是否有重复的 error_code
    fn has_repeated_codes(&self) -> bool {
        // 收集所有 error_code (排除 None)
        let mut code_counts: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();

        for round in &self.rounds {
            for c in round.codes.iter().flatten() {
                *code_counts.entry(c.as_str()).or_insert(0) += 1;
            }
        }

        // 检查是否有任何 error_code 出现 >= max_repeats 次
        code_counts.values().any(|&count| count >= self.max_repeats)
    }

    /// 检查是否有重复的消息签名
    fn has_repeated_signatures(&self) -> bool {
        let mut sig_counts: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();

        for round in &self.rounds {
            for sig in &round.signatures {
                *sig_counts.entry(sig.as_str()).or_insert(0) += 1;
            }
        }

        sig_counts.values().any(|&count| count >= self.max_repeats)
    }

    /// 检查是否有重复的文件路径
    fn has_repeated_files(&self) -> bool {
        let mut file_counts: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();

        for round in &self.rounds {
            for file in &round.files {
                *file_counts.entry(file.as_str()).or_insert(0) += 1;
            }
        }

        file_counts.values().any(|&count| count >= self.max_repeats)
    }

    /// 生成策略改变 prompt
    ///
    /// 首次调用时生成"换方法"提示, 同时将 `strategy_changed` 设为 true。
    /// 后续调用时 (strategy_changed == true) 生成"建议跳过"提示。
    pub fn loop_strategy_prompt(&mut self) -> String {
        if !self.strategy_changed {
            // 首次: 建议换方法
            self.strategy_changed = true;
            self.build_strategy_change_prompt()
        } else {
            // 二次: 建议跳过
            self.build_skip_prompt()
        }
    }

    /// 是否应该跳过当前任务
    ///
    /// 策略已改变且仍然在死循环时返回 true。
    pub fn should_skip(&self) -> bool {
        self.strategy_changed && self.is_looping()
    }

    /// 构建"换方法"策略 prompt
    fn build_strategy_change_prompt(&self) -> String {
        // 收集重复的错误摘要
        let repeated = self.get_repeated_errors_summary();

        format!(
            "⚠️ 循环终止检测: 检测到修复死循环\n\
             \n\
             以下错误已连续出现 {n} 次, 说明当前修复策略无效:\n\
             {repeated}\n\
             \n\
             🔧 策略改变要求:\n\
             - 你之前的修复方法没有解决问题, 请换一种完全不同的方法\n\
             - 重新审视问题的根因, 而非在原有代码上微调\n\
             - 考虑: 重构相关代码结构、使用不同的数据类型/算法、检查依赖关系\n\
             - 如果是类型错误, 考虑重新设计接口签名\n\
             - 如果是借用错误, 考虑改变所有权结构或使用 clone\n\
             - 如果是导入错误, 检查模块组织是否合理\n\
             \n\
             请用全新的方法修复这些错误, 用 ```file:路径``` 格式输出完整文件。",
            n = self.max_repeats,
            repeated = repeated,
        )
    }

    /// 构建"建议跳过"策略 prompt
    fn build_skip_prompt(&self) -> String {
        format!(
            "🛑 循环终止检测: 策略改变后仍然死循环\n\
             \n\
             已经尝试了换方法但同样的错误仍然反复出现 ({n} 次)。\n\
             当前任务可能需要人工介入或更多上下文才能解决。\n\
             建议跳过当前任务, 避免继续浪费修复轮次。\n\
             \n\
             请输出当前最佳尝试的代码, 我们将标记此任务为未完全通过。",
            n = self.max_repeats,
        )
    }

    /// 获取重复错误的摘要文本
    fn get_repeated_errors_summary(&self) -> String {
        let mut summaries = Vec::new();

        // 检查重复的签名
        let mut sig_counts: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        for round in &self.rounds {
            for sig in &round.signatures {
                *sig_counts.entry(sig.as_str()).or_insert(0) += 1;
            }
        }

        for (sig, count) in sig_counts.iter() {
            if *count >= self.max_repeats {
                let display: String = sig.chars().take(150).collect();
                summaries.push(format!("  - {} (出现 {} 次)", display, count));
            }
        }

        if summaries.is_empty() {
            // 检查重复的文件
            let mut file_counts: std::collections::HashMap<&str, usize> =
                std::collections::HashMap::new();
            for round in &self.rounds {
                for file in &round.files {
                    *file_counts.entry(file.as_str()).or_insert(0) += 1;
                }
            }
            for (file, count) in file_counts.iter() {
                if *count >= self.max_repeats {
                    summaries.push(format!("  - 文件 {} 出现错误 {} 次", file, count));
                }
            }
        }

        if summaries.is_empty() {
            "(无法提取具体错误摘要)".to_string()
        } else {
            summaries.join("\n")
        }
    }

    /// 重置检测器 (任务完成后调用)
    ///
    /// 清空所有轮次记录和策略状态, 避免跨任务误检。
    pub fn reset(&mut self) {
        self.rounds.clear();
        self.strategy_changed = false;
    }

    /// 获取已记录的轮次数
    pub fn round_count(&self) -> usize {
        self.rounds.len()
    }
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ===== 辅助函数 =====

    fn make_error(code: Option<&str>, msg: &str, file: &str) -> CompileError {
        CompileError {
            file: file.to_string(),
            line: Some(10),
            column: Some(5),
            message: msg.to_string(),
            error_code: code.map(String::from),
        }
    }

    // ===== ErrorRound::from_errors =====

    #[test]
    fn test_error_round_from_errors() {
        let errors = vec![
            make_error(Some("E0308"), "mismatched types", "src/main.rs"),
            make_error(Some("E0382"), "use of moved value", "src/lib.rs"),
        ];
        let round = ErrorRound::from_errors(&errors);

        assert_eq!(round.codes.len(), 2);
        assert_eq!(round.codes[0], Some("E0308".to_string()));
        assert_eq!(round.codes[1], Some("E0382".to_string()));
        assert_eq!(round.signatures.len(), 2);
        assert!(round.signatures[0].contains("E0308"));
        assert!(round.signatures[0].contains("mismatched types"));
        assert_eq!(round.files, vec!["src/main.rs", "src/lib.rs"]);
    }

    #[test]
    fn test_error_round_empty_errors() {
        let round = ErrorRound::from_errors(&[]);
        assert!(round.codes.is_empty());
        assert!(round.signatures.is_empty());
        assert!(round.files.is_empty());
    }

    #[test]
    fn test_error_round_no_error_code() {
        let errors = vec![make_error(
            None,
            "cannot borrow `x` as mutable",
            "src/main.rs",
        )];
        let round = ErrorRound::from_errors(&errors);

        assert_eq!(round.codes[0], None);
        assert!(round.signatures[0].contains("cannot borrow"));
        assert!(!round.signatures[0].starts_with("["));
    }

    #[test]
    fn test_error_round_signature_truncation() {
        let long_msg = "x".repeat(200);
        let errors = vec![make_error(Some("E0308"), &long_msg, "src/main.rs")];
        let round = ErrorRound::from_errors(&errors);

        // 签名应截断消息到 100 字符
        let expected_msg: String = "x".repeat(100);
        assert_eq!(round.signatures[0], format!("[E0308] {}", expected_msg));
    }

    // ===== LoopDetector::new =====

    #[test]
    fn test_new_default() {
        let detector = LoopDetector::new(3);
        assert_eq!(detector.max_repeats, 3);
        assert!(detector.rounds.is_empty());
        assert!(!detector.strategy_changed);
    }

    #[test]
    fn test_new_disabled() {
        let detector = LoopDetector::new(0);
        assert_eq!(detector.max_repeats, 0);
        assert!(!detector.is_looping());
    }

    // ===== LoopDetector::record_errors =====

    #[test]
    fn test_record_errors_stores_round() {
        let mut detector = LoopDetector::new(3);
        let errors = vec![make_error(Some("E0308"), "mismatched types", "src/main.rs")];
        detector.record_errors(&errors);

        assert_eq!(detector.round_count(), 1);
        assert_eq!(detector.rounds[0].codes.len(), 1);
    }

    #[test]
    fn test_record_errors_multiple_rounds() {
        let mut detector = LoopDetector::new(3);
        let errors = vec![make_error(Some("E0308"), "mismatched types", "src/main.rs")];

        detector.record_errors(&errors);
        detector.record_errors(&errors);
        detector.record_errors(&errors);

        assert_eq!(detector.round_count(), 3);
    }

    #[test]
    fn test_record_errors_trims_old_rounds() {
        let mut detector = LoopDetector::new(3);

        // 记录 5 轮, 只保留最近 3 轮
        for i in 0..5 {
            let errors = vec![make_error(
                Some("E0308"),
                &format!("error {}", i),
                "src/main.rs",
            )];
            detector.record_errors(&errors);
        }

        assert_eq!(detector.round_count(), 3);
        // 最后一轮应该是 error 4
        assert!(detector.rounds[2].signatures[0].contains("error 4"));
    }

    #[test]
    fn test_record_errors_empty() {
        let mut detector = LoopDetector::new(3);
        detector.record_errors(&[]);

        assert_eq!(detector.round_count(), 1);
        assert!(detector.rounds[0].codes.is_empty());
    }

    // ===== LoopDetector::is_looping — 错误码重复 =====

    #[test]
    fn test_is_looping_by_error_code() {
        let mut detector = LoopDetector::new(3);
        let errors = vec![make_error(Some("E0308"), "mismatched types", "src/main.rs")];

        detector.record_errors(&errors);
        assert!(!detector.is_looping()); // 1 次

        detector.record_errors(&errors);
        assert!(!detector.is_looping()); // 2 次

        detector.record_errors(&errors);
        assert!(detector.is_looping()); // 3 次 → 死循环
    }

    #[test]
    fn test_is_looping_different_error_codes() {
        let mut detector = LoopDetector::new(3);

        // 不同 error_code, 不同消息, 不同文件 → 不死循环
        detector.record_errors(&[make_error(Some("E0308"), "error 1", "src/main.rs")]);
        detector.record_errors(&[make_error(Some("E0382"), "error 2", "src/lib.rs")]);
        detector.record_errors(&[make_error(Some("E0425"), "error 3", "src/utils.rs")]);

        assert!(!detector.is_looping()); // 完全不同的错误
    }

    // ===== LoopDetector::is_looping — 消息签名重复 =====

    #[test]
    fn test_is_looping_by_signature_no_code() {
        let mut detector = LoopDetector::new(3);
        let errors = vec![make_error(
            None,
            "cannot borrow `x` as mutable",
            "src/main.rs",
        )];

        detector.record_errors(&errors);
        detector.record_errors(&errors);
        assert!(!detector.is_looping()); // 2 次

        detector.record_errors(&errors);
        assert!(detector.is_looping()); // 3 次
    }

    #[test]
    fn test_is_looping_by_signature_different_messages() {
        let mut detector = LoopDetector::new(3);

        detector.record_errors(&[make_error(
            Some("E0308"),
            "mismatched types: usize vs i32",
            "src/main.rs",
        )]);
        detector.record_errors(&[make_error(
            Some("E0308"),
            "mismatched types: String vs &str",
            "src/main.rs",
        )]);
        detector.record_errors(&[make_error(
            Some("E0308"),
            "mismatched types: f64 vs i32",
            "src/main.rs",
        )]);

        // 虽然相同 error_code, 但不同消息签名 → 不死循环
        // 等等, error_code 相同也会被 has_repeated_codes 检测到
        // 所以这里应该检测到 error_code 重复
        assert!(detector.is_looping()); // error_code E0308 出现 3 次
    }

    // ===== LoopDetector::is_looping — 文件重复 =====

    #[test]
    fn test_is_looping_by_file() {
        let mut detector = LoopDetector::new(3);

        // 不同 error_code, 不同消息, 但相同文件
        detector.record_errors(&[make_error(Some("E0308"), "error 1", "src/main.rs")]);
        detector.record_errors(&[make_error(Some("E0382"), "error 2", "src/main.rs")]);
        detector.record_errors(&[make_error(Some("E0425"), "error 3", "src/main.rs")]);

        // 不同 error_code 和签名, 但相同文件 → 文件维度触发
        assert!(detector.is_looping());
    }

    #[test]
    fn test_is_looping_different_files() {
        let mut detector = LoopDetector::new(3);

        detector.record_errors(&[make_error(Some("E0308"), "error 1", "src/main.rs")]);
        detector.record_errors(&[make_error(Some("E0308"), "error 2", "src/lib.rs")]);
        detector.record_errors(&[make_error(Some("E0308"), "error 3", "src/utils.rs")]);

        // 不同文件, 但相同 error_code → error_code 维度触发
        assert!(detector.is_looping());
    }

    #[test]
    fn test_is_looping_completely_different() {
        let mut detector = LoopDetector::new(3);

        detector.record_errors(&[make_error(Some("E0308"), "error 1", "src/main.rs")]);
        detector.record_errors(&[make_error(Some("E0382"), "error 2", "src/lib.rs")]);
        detector.record_errors(&[make_error(Some("E0425"), "error 3", "src/utils.rs")]);

        // 不同 error_code, 不同消息, 但相同文件 src/main.rs 出现在第 1 轮
        // 不对, 文件分别是 main.rs, lib.rs, utils.rs — 全不同
        // error_code 分别 E0308, E0382, E0425 — 全不同
        // 签名也全不同
        // 但是 has_repeated_files 检查的是: 文件出现 >= max_repeats 次
        // main.rs 1次, lib.rs 1次, utils.rs 1次 → 不触发
        assert!(!detector.is_looping());
    }

    // ===== LoopDetector::is_looping — 边界条件 =====

    #[test]
    fn test_is_looping_disabled() {
        let mut detector = LoopDetector::new(0);
        let errors = vec![make_error(Some("E0308"), "error", "src/main.rs")];

        detector.record_errors(&errors);
        detector.record_errors(&errors);
        detector.record_errors(&errors);

        assert!(!detector.is_looping()); // max_repeats=0 禁用
    }

    #[test]
    fn test_is_looping_not_enough_rounds() {
        let mut detector = LoopDetector::new(3);
        let errors = vec![make_error(Some("E0308"), "error", "src/main.rs")];

        detector.record_errors(&errors);
        detector.record_errors(&errors);

        assert!(!detector.is_looping()); // 只有 2 轮, 需要 3 轮
    }

    #[test]
    fn test_is_looping_max_repeats_1() {
        let mut detector = LoopDetector::new(1);
        let errors = vec![make_error(Some("E0308"), "error", "src/main.rs")];

        detector.record_errors(&errors);
        assert!(detector.is_looping()); // 1 次就触发
    }

    #[test]
    fn test_is_looping_max_repeats_2() {
        let mut detector = LoopDetector::new(2);
        let errors = vec![make_error(Some("E0308"), "error", "src/main.rs")];

        detector.record_errors(&errors);
        assert!(!detector.is_looping()); // 1 次, 需要 2 次

        detector.record_errors(&errors);
        assert!(detector.is_looping()); // 2 次 → 触发
    }

    // ===== LoopDetector::loop_strategy_prompt =====

    #[test]
    fn test_loop_strategy_prompt_first_time() {
        let mut detector = LoopDetector::new(3);
        let errors = vec![make_error(Some("E0308"), "mismatched types", "src/main.rs")];

        detector.record_errors(&errors);
        detector.record_errors(&errors);
        detector.record_errors(&errors);

        assert!(!detector.strategy_changed); // 调用前为 false

        let prompt = detector.loop_strategy_prompt();

        assert!(prompt.contains("循环终止检测"));
        assert!(prompt.contains("换一种完全不同的方法"));
        assert!(prompt.contains("mismatched types"));
        assert!(detector.strategy_changed); // 调用后为 true
    }

    #[test]
    fn test_loop_strategy_prompt_first_time_sets_flag() {
        let mut detector = LoopDetector::new(3);
        let errors = vec![make_error(Some("E0308"), "error", "src/main.rs")];

        detector.record_errors(&errors);
        detector.record_errors(&errors);
        detector.record_errors(&errors);

        assert!(!detector.strategy_changed);

        let _prompt = detector.loop_strategy_prompt();

        assert!(detector.strategy_changed);
    }

    #[test]
    fn test_loop_strategy_prompt_second_time() {
        let mut detector = LoopDetector::new(3);
        let errors = vec![make_error(Some("E0308"), "error", "src/main.rs")];

        detector.record_errors(&errors);
        detector.record_errors(&errors);
        detector.record_errors(&errors);

        // 第一次: 换方法
        let prompt1 = detector.loop_strategy_prompt();
        assert!(prompt1.contains("换一种完全不同的方法"));

        // 第二次: 建议跳过
        let prompt2 = detector.loop_strategy_prompt();
        assert!(prompt2.contains("建议跳过") || prompt2.contains("跳过"));
    }

    #[test]
    fn test_loop_strategy_prompt_contains_error_summary() {
        let mut detector = LoopDetector::new(3);
        let errors = vec![make_error(
            Some("E0308"),
            "mismatched types: expected usize",
            "src/main.rs",
        )];

        detector.record_errors(&errors);
        detector.record_errors(&errors);
        detector.record_errors(&errors);

        let prompt = detector.loop_strategy_prompt();

        assert!(prompt.contains("mismatched types"));
        assert!(prompt.contains("3 次"));
    }

    // ===== LoopDetector::should_skip =====

    #[test]
    fn test_should_skip_false_before_strategy_change() {
        let mut detector = LoopDetector::new(3);
        let errors = vec![make_error(Some("E0308"), "error", "src/main.rs")];

        detector.record_errors(&errors);
        detector.record_errors(&errors);
        detector.record_errors(&errors);

        // 策略未改变, 即使死循环也不跳过
        assert!(detector.is_looping());
        assert!(!detector.should_skip());
    }

    #[test]
    fn test_should_skip_true_after_strategy_change() {
        let mut detector = LoopDetector::new(3);
        let errors = vec![make_error(Some("E0308"), "error", "src/main.rs")];

        detector.record_errors(&errors);
        detector.record_errors(&errors);
        detector.record_errors(&errors);

        // 改变策略
        detector.loop_strategy_prompt();
        assert!(detector.strategy_changed);

        // 策略改变后仍然死循环 → 应跳过
        assert!(detector.should_skip());
    }

    #[test]
    fn test_should_skip_false_when_not_looping() {
        let mut detector = LoopDetector::new(3);
        let errors = vec![make_error(Some("E0308"), "error", "src/main.rs")];

        detector.record_errors(&errors);
        detector.loop_strategy_prompt(); // 改变策略

        // 只有 1 轮, 不算死循环
        assert!(!detector.is_looping());
        assert!(!detector.should_skip());
    }

    // ===== LoopDetector::reset =====

    #[test]
    fn test_reset_clears_rounds() {
        let mut detector = LoopDetector::new(3);
        let errors = vec![make_error(Some("E0308"), "error", "src/main.rs")];

        detector.record_errors(&errors);
        detector.record_errors(&errors);
        detector.record_errors(&errors);

        assert_eq!(detector.round_count(), 3);
        assert!(detector.is_looping());

        detector.reset();

        assert_eq!(detector.round_count(), 0);
        assert!(!detector.is_looping());
    }

    #[test]
    fn test_reset_clears_strategy_changed() {
        let mut detector = LoopDetector::new(3);
        let errors = vec![make_error(Some("E0308"), "error", "src/main.rs")];

        detector.record_errors(&errors);
        detector.record_errors(&errors);
        detector.record_errors(&errors);
        detector.loop_strategy_prompt();

        assert!(detector.strategy_changed);

        detector.reset();

        assert!(!detector.strategy_changed);
    }

    #[test]
    fn test_reset_allows_fresh_detection() {
        let mut detector = LoopDetector::new(3);
        let errors = vec![make_error(Some("E0308"), "error", "src/main.rs")];

        // 第一轮: 3 次重复 → 死循环
        detector.record_errors(&errors);
        detector.record_errors(&errors);
        detector.record_errors(&errors);
        assert!(detector.is_looping());

        // 重置
        detector.reset();

        // 第二轮: 2 次重复 → 不死循环
        detector.record_errors(&errors);
        detector.record_errors(&errors);
        assert!(!detector.is_looping());

        // 第 3 次 → 死循环
        detector.record_errors(&errors);
        assert!(detector.is_looping());
    }

    // ===== LoopDetector::round_count =====

    #[test]
    fn test_round_count() {
        let mut detector = LoopDetector::new(5);
        assert_eq!(detector.round_count(), 0);

        detector.record_errors(&[make_error(Some("E0308"), "e1", "f.rs")]);
        assert_eq!(detector.round_count(), 1);

        detector.record_errors(&[make_error(Some("E0308"), "e2", "f.rs")]);
        assert_eq!(detector.round_count(), 2);
    }

    // ===== 集成场景测试 =====

    #[test]
    fn test_scenario_progressive_errors_then_loop() {
        let mut detector = LoopDetector::new(3);

        // 第 1 轮: 错误 A
        detector.record_errors(&[make_error(Some("E0308"), "type error A", "src/main.rs")]);
        assert!(!detector.is_looping());

        // 第 2 轮: 错误 A + 新错误 B
        detector.record_errors(&[
            make_error(Some("E0308"), "type error A", "src/main.rs"),
            make_error(Some("E0425"), "cannot find value", "src/lib.rs"),
        ]);
        assert!(!detector.is_looping());

        // 第 3 轮: 又是错误 A → 死循环 (error_code E0308 出现 3 次)
        detector.record_errors(&[make_error(Some("E0308"), "type error A", "src/main.rs")]);
        assert!(detector.is_looping());
    }

    #[test]
    fn test_scenario_different_errors_no_loop() {
        let mut detector = LoopDetector::new(3);

        detector.record_errors(&[make_error(Some("E0308"), "error A", "src/main.rs")]);
        detector.record_errors(&[make_error(Some("E0382"), "error B", "src/lib.rs")]);
        detector.record_errors(&[make_error(Some("E0425"), "error C", "src/utils.rs")]);

        assert!(!detector.is_looping());
    }

    #[test]
    fn test_scenario_error_resolved_then_new_error() {
        let mut detector = LoopDetector::new(3);

        // 前 2 轮: 错误 A
        detector.record_errors(&[make_error(Some("E0308"), "error A", "src/main.rs")]);
        detector.record_errors(&[make_error(Some("E0308"), "error A", "src/main.rs")]);

        // 第 3 轮: 错误 A 解决了, 出现新错误 B
        detector.record_errors(&[make_error(Some("E0382"), "error B", "src/lib.rs")]);

        // E0308 只出现 2 次, E0382 只出现 1 次 → 不死循环
        // 但 src/main.rs 出现 2 次, src/lib.rs 出现 1 次 → 不死循环
        assert!(!detector.is_looping());
    }

    #[test]
    fn test_scenario_multiple_errors_same_round() {
        let mut detector = LoopDetector::new(3);

        let errors = vec![
            make_error(Some("E0308"), "error 1", "src/main.rs"),
            make_error(Some("E0382"), "error 2", "src/lib.rs"),
        ];

        detector.record_errors(&errors);
        detector.record_errors(&errors);
        detector.record_errors(&errors);

        // 同一组错误出现 3 次 → 死循环
        assert!(detector.is_looping());
    }

    #[test]
    fn test_strategy_prompt_contains_repeated_file_info() {
        let mut detector = LoopDetector::new(3);

        // 不同 error_code 和消息, 但相同文件
        detector.record_errors(&[make_error(Some("E0308"), "msg1", "src/main.rs")]);
        detector.record_errors(&[make_error(Some("E0382"), "msg2", "src/main.rs")]);
        detector.record_errors(&[make_error(Some("E0425"), "msg3", "src/main.rs")]);

        let prompt = detector.loop_strategy_prompt();

        // 签名不同, 但文件相同 → 应包含文件信息
        assert!(prompt.contains("src/main.rs") || prompt.contains("文件"));
    }
}
