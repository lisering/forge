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
use std::collections::{HashMap, VecDeque};

// ============================================================================
//  纯逻辑函数 — 可独立测试, 不依赖 LoopDetector 状态
// ============================================================================

/// 截断文本到指定字符数 (按 Unicode 字符, 非字节)
///
/// 用于错误签名截断、摘要显示等场景。
///
/// # 示例
///
/// ```
/// # use forge::loop_detector::truncate_text;
/// assert_eq!(truncate_text("hello world", 5), "hello");
/// assert_eq!(truncate_text("hi", 10), "hi"); // 短于 max_chars 不变
/// assert_eq!(truncate_text("", 10), "");
/// assert_eq!(truncate_text("你好世界", 2), "你好"); // UTF-8 安全
/// ```
pub fn truncate_text(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

/// 从 `CompileError` 生成错误签名 (error_code + 消息前 100 字符)
///
/// 签名用于跨轮次比较同一错误是否重复出现。
/// 有 `error_code` 时格式为 `[CODE] message`, 无则直接使用消息。
///
/// # 示例
///
/// ```
/// # use forge::loop_detector::make_error_signature;
/// # use forge::testrunner::CompileError;
/// let err = CompileError {
///     file: "src/main.rs".to_string(),
///     line: Some(10),
///     column: Some(5),
///     message: "mismatched types".to_string(),
///     error_code: Some("E0308".to_string()),
/// };
/// assert_eq!(make_error_signature(&err), "[E0308] mismatched types");
/// ```
pub fn make_error_signature(error: &CompileError) -> String {
    let msg_part = truncate_text(&error.message, 100);
    match &error.error_code {
        Some(code) => format!("[{}] {}", code, msg_part),
        None => msg_part,
    }
}

/// 判断是否应进行循环检测 (guard check)
///
/// `max_repeats == 0` 时禁用检测; 轮次数少于 `max_repeats` 时数据不足。
/// 只有两者均满足时才返回 `true`。
///
/// # 示例
///
/// ```
/// # use forge::loop_detector::should_detect_loop;
/// assert!(!should_detect_loop(0, 10));   // 禁用
/// assert!(!should_detect_loop(3, 2));    // 轮次不足
/// assert!(should_detect_loop(3, 3));     // 刚好
/// assert!(should_detect_loop(3, 5));     // 超过
/// ```
pub fn should_detect_loop(max_repeats: usize, round_count: usize) -> bool {
    max_repeats > 0 && round_count >= max_repeats
}

/// 检查是否有重复的错误码 (同一 `error_code` 出现 ≥ `max_repeats` 次)
///
/// 遍历所有轮次中的所有错误码 (排除 `None`),
/// 统计每个码的出现次数, 任一码 ≥ `max_repeats` 即返回 `true`。
///
/// # 示例
///
/// ```
/// # use forge::loop_detector::{has_any_repeated_codes, ErrorRound};
/// let rounds = vec![
///     ErrorRound { codes: vec![Some("E0308".into())], signatures: vec![], files: vec![] },
///     ErrorRound { codes: vec![Some("E0308".into())], signatures: vec![], files: vec![] },
///     ErrorRound { codes: vec![Some("E0308".into())], signatures: vec![], files: vec![] },
/// ];
/// assert!(has_any_repeated_codes(&rounds, 3));
/// assert!(!has_any_repeated_codes(&rounds, 4));
/// ```
pub fn has_any_repeated_codes<'a>(
    rounds: impl IntoIterator<Item = &'a ErrorRound>,
    max_repeats: usize,
) -> bool {
    let counts = count_code_occurrences(rounds);
    counts.values().any(|&c| c >= max_repeats)
}

/// 检查是否有重复的错误签名 (同一 `signature` 出现 ≥ `max_repeats` 次)
///
/// # 示例
///
/// ```
/// # use forge::loop_detector::{has_any_repeated_signatures, ErrorRound};
/// let sig = "[E0308] error".to_string();
/// let rounds = vec![
///     ErrorRound { codes: vec![], signatures: vec![sig.clone()], files: vec![] },
///     ErrorRound { codes: vec![], signatures: vec![sig.clone()], files: vec![] },
///     ErrorRound { codes: vec![], signatures: vec![sig.clone()], files: vec![] },
/// ];
/// assert!(has_any_repeated_signatures(&rounds, 3));
/// ```
pub fn has_any_repeated_signatures<'a>(
    rounds: impl IntoIterator<Item = &'a ErrorRound>,
    max_repeats: usize,
) -> bool {
    let counts = count_signature_occurrences(rounds);
    counts.values().any(|&c| c >= max_repeats)
}

/// 检查是否有重复的文件路径 (同一文件出现 ≥ `max_repeats` 次)
///
/// # 示例
///
/// ```
/// # use forge::loop_detector::{has_any_repeated_files, ErrorRound};
/// let rounds = vec![
///     ErrorRound { codes: vec![], signatures: vec![], files: vec!["src/main.rs".into()] },
///     ErrorRound { codes: vec![], signatures: vec![], files: vec!["src/main.rs".into()] },
///     ErrorRound { codes: vec![], signatures: vec![], files: vec!["src/main.rs".into()] },
/// ];
/// assert!(has_any_repeated_files(&rounds, 3));
/// ```
pub fn has_any_repeated_files<'a>(
    rounds: impl IntoIterator<Item = &'a ErrorRound>,
    max_repeats: usize,
) -> bool {
    let counts = count_file_occurrences(rounds);
    counts.values().any(|&c| c >= max_repeats)
}

/// 收集出现次数 ≥ `max_repeats` 的错误签名及其计数
///
/// 返回 `(签名, 出现次数)` 列表, 按出现次数降序排列。
///
/// # 示例
///
/// ```
/// # use forge::loop_detector::{collect_repeated_signatures, ErrorRound};
/// let sig = "[E0308] error".to_string();
/// let rounds = vec![
///     ErrorRound { codes: vec![], signatures: vec![sig.clone()], files: vec![] },
///     ErrorRound { codes: vec![], signatures: vec![sig.clone()], files: vec![] },
///     ErrorRound { codes: vec![], signatures: vec![sig.clone()], files: vec![] },
/// ];
/// let repeated = collect_repeated_signatures(&rounds, 3);
/// assert_eq!(repeated.len(), 1);
/// assert_eq!(repeated[0].1, 3);
/// ```
pub fn collect_repeated_signatures<'a>(
    rounds: impl IntoIterator<Item = &'a ErrorRound>,
    max_repeats: usize,
) -> Vec<(String, usize)> {
    let counts = count_signature_occurrences(rounds);
    let mut result: Vec<(String, usize)> = counts
        .into_iter()
        .filter(|(_, c)| *c >= max_repeats)
        .collect();
    result.sort_by_key(|b| std::cmp::Reverse(b.1));
    result
}

/// 收集出现次数 ≥ `max_repeats` 的文件路径及其计数
///
/// 返回 `(文件路径, 出现次数)` 列表, 按出现次数降序排列。
///
/// # 示例
///
/// ```
/// # use forge::loop_detector::{collect_repeated_files, ErrorRound};
/// let rounds = vec![
///     ErrorRound { codes: vec![], signatures: vec![], files: vec!["src/main.rs".into()] },
///     ErrorRound { codes: vec![], signatures: vec![], files: vec!["src/main.rs".into()] },
///     ErrorRound { codes: vec![], signatures: vec![], files: vec!["src/main.rs".into()] },
/// ];
/// let repeated = collect_repeated_files(&rounds, 3);
/// assert_eq!(repeated.len(), 1);
/// assert_eq!(repeated[0].0, "src/main.rs");
/// assert_eq!(repeated[0].1, 3);
/// ```
pub fn collect_repeated_files<'a>(
    rounds: impl IntoIterator<Item = &'a ErrorRound>,
    max_repeats: usize,
) -> Vec<(String, usize)> {
    let counts = count_file_occurrences(rounds);
    let mut result: Vec<(String, usize)> = counts
        .into_iter()
        .filter(|(_, c)| *c >= max_repeats)
        .collect();
    result.sort_by_key(|b| std::cmp::Reverse(b.1));
    result
}

/// 格式化重复错误摘要文本
///
/// 优先展示重复的签名, 无签名时回退到文件, 两者皆无则返回默认提示。
/// 每条签名截断到 150 字符以避免 prompt 过长。
///
/// # 示例
///
/// ```
/// # use forge::loop_detector::format_repeated_summary;
/// let sigs = vec![("[E0308] mismatched types".into(), 3usize)];
/// let files = vec![];
/// let summary = format_repeated_summary(&sigs, &files);
/// assert!(summary.contains("mismatched types"));
/// assert!(summary.contains("3 次"));
/// ```
pub fn format_repeated_summary(
    repeated_sigs: &[(String, usize)],
    repeated_files: &[(String, usize)],
) -> String {
    if !repeated_sigs.is_empty() {
        let summaries: Vec<String> = repeated_sigs
            .iter()
            .map(|(sig, count)| {
                let display = truncate_text(sig, 150);
                format!("  - {} (出现 {} 次)", display, count)
            })
            .collect();
        summaries.join("\n")
    } else if !repeated_files.is_empty() {
        let summaries: Vec<String> = repeated_files
            .iter()
            .map(|(file, count)| format!("  - 文件 {} 出现错误 {} 次", file, count))
            .collect();
        summaries.join("\n")
    } else {
        "(无法提取具体错误摘要)".to_string()
    }
}

/// 构建"换方法"策略 prompt 文本
///
/// 首次检测到死循环时调用, 提示 AI 换一种完全不同的方法。
///
/// # 示例
///
/// ```
/// # use forge::loop_detector::build_strategy_change_prompt_text;
/// let prompt = build_strategy_change_prompt_text(3, "  - [E0308] error (出现 3 次)");
/// assert!(prompt.contains("循环终止检测"));
/// assert!(prompt.contains("换一种完全不同的方法"));
/// assert!(prompt.contains("3 次"));
/// ```
pub fn build_strategy_change_prompt_text(max_repeats: usize, repeated_summary: &str) -> String {
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
        n = max_repeats,
        repeated = repeated_summary,
    )
}

/// 构建"建议跳过"策略 prompt 文本
///
/// 策略改变后仍然死循环时调用, 建议跳过当前任务。
///
/// # 示例
///
/// ```
/// # use forge::loop_detector::build_skip_prompt_text;
/// let prompt = build_skip_prompt_text(3);
/// assert!(prompt.contains("跳过"));
/// assert!(prompt.contains("3 次"));
/// ```
pub fn build_skip_prompt_text(max_repeats: usize) -> String {
    format!(
        "🛑 循环终止检测: 策略改变后仍然死循环\n\
         \n\
         已经尝试了换方法但同样的错误仍然反复出现 ({n} 次)。\n\
         当前任务可能需要人工介入或更多上下文才能解决。\n\
         建议跳过当前任务, 避免继续浪费修复轮次。\n\
         \n\
         请输出当前最佳尝试的代码, 我们将标记此任务为未完全通过。",
        n = max_repeats,
    )
}

/// 判断是否应该跳过当前任务
///
/// 策略已改变 (`strategy_changed == true`) 且仍然在死循环 (`is_looping == true`) 时返回 `true`。
///
/// # 示例
///
/// ```
/// # use forge::loop_detector::should_skip_task;
/// assert!(should_skip_task(true, true));
/// assert!(!should_skip_task(true, false));
/// assert!(!should_skip_task(false, true));
/// assert!(!should_skip_task(false, false));
/// ```
pub fn should_skip_task(strategy_changed: bool, is_looping: bool) -> bool {
    strategy_changed && is_looping
}

// ============================================================================
//  内部辅助 — 计数函数 (纯逻辑, 不导出)
// ============================================================================

/// 统计所有轮次中各 error_code 的出现次数 (排除 None)
fn count_code_occurrences<'a>(
    rounds: impl IntoIterator<Item = &'a ErrorRound>,
) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for round in rounds {
        for c in round.codes.iter().flatten() {
            *counts.entry(c.clone()).or_insert(0) += 1;
        }
    }
    counts
}

/// 统计所有轮次中各签名的出现次数
fn count_signature_occurrences<'a>(
    rounds: impl IntoIterator<Item = &'a ErrorRound>,
) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for round in rounds {
        for sig in &round.signatures {
            *counts.entry(sig.clone()).or_insert(0) += 1;
        }
    }
    counts
}

/// 统计所有轮次中各文件路径的出现次数
fn count_file_occurrences<'a>(
    rounds: impl IntoIterator<Item = &'a ErrorRound>,
) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for round in rounds {
        for file in &round.files {
            *counts.entry(file.clone()).or_insert(0) += 1;
        }
    }
    counts
}

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
        let signatures = errors.iter().map(make_error_signature).collect();
        let files = errors.iter().map(|e| e.file.clone()).collect();
        Self {
            codes,
            signatures,
            files,
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
        if !should_detect_loop(self.max_repeats, self.rounds.len()) {
            return false;
        }

        // 维度 1: 错误码重复
        if has_any_repeated_codes(&self.rounds, self.max_repeats) {
            return true;
        }

        // 维度 2: 消息签名重复
        if has_any_repeated_signatures(&self.rounds, self.max_repeats) {
            return true;
        }

        // 维度 3: 错误文件重复
        if has_any_repeated_files(&self.rounds, self.max_repeats) {
            return true;
        }

        false
    }

    /// 生成策略改变 prompt
    ///
    /// 首次调用时生成"换方法"提示, 同时将 `strategy_changed` 设为 true。
    /// 后续调用时 (strategy_changed == true) 生成"建议跳过"提示。
    pub fn loop_strategy_prompt(&mut self) -> String {
        if !self.strategy_changed {
            // 首次: 建议换方法
            self.strategy_changed = true;
            let repeated = self.get_repeated_errors_summary();
            build_strategy_change_prompt_text(self.max_repeats, &repeated)
        } else {
            // 二次: 建议跳过
            build_skip_prompt_text(self.max_repeats)
        }
    }

    /// 是否应该跳过当前任务
    ///
    /// 策略已改变且仍然在死循环时返回 true。
    pub fn should_skip(&self) -> bool {
        should_skip_task(self.strategy_changed, self.is_looping())
    }

    /// 获取重复错误的摘要文本
    fn get_repeated_errors_summary(&self) -> String {
        let repeated_sigs = collect_repeated_signatures(&self.rounds, self.max_repeats);
        let repeated_files = collect_repeated_files(&self.rounds, self.max_repeats);
        format_repeated_summary(&repeated_sigs, &repeated_files)
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

    // ===== 纯逻辑函数测试 =====

    // --- truncate_text ---

    #[test]
    fn test_truncate_text_normal() {
        assert_eq!(truncate_text("hello world", 5), "hello");
    }

    #[test]
    fn test_truncate_text_shorter_than_max() {
        assert_eq!(truncate_text("hi", 10), "hi");
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
    fn test_truncate_text_utf8_multibyte() {
        assert_eq!(truncate_text("你好世界", 2), "你好");
    }

    #[test]
    fn test_truncate_text_exact_length() {
        assert_eq!(truncate_text("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_text_single_char() {
        assert_eq!(truncate_text("A", 1), "A");
    }

    // --- make_error_signature ---

    #[test]
    fn test_make_error_signature_with_code() {
        let err = make_error(Some("E0308"), "mismatched types", "src/main.rs");
        assert_eq!(make_error_signature(&err), "[E0308] mismatched types");
    }

    #[test]
    fn test_make_error_signature_without_code() {
        let err = make_error(None, "cannot borrow `x`", "src/main.rs");
        assert_eq!(make_error_signature(&err), "cannot borrow `x`");
    }

    #[test]
    fn test_make_error_signature_long_message_truncated() {
        let long_msg = "x".repeat(200);
        let err = make_error(Some("E0308"), &long_msg, "src/main.rs");
        let expected_msg: String = "x".repeat(100);
        assert_eq!(
            make_error_signature(&err),
            format!("[E0308] {}", expected_msg)
        );
    }

    #[test]
    fn test_make_error_signature_empty_message() {
        let err = make_error(Some("E0308"), "", "src/main.rs");
        assert_eq!(make_error_signature(&err), "[E0308] ");
    }

    #[test]
    fn test_make_error_signature_empty_message_no_code() {
        let err = make_error(None, "", "src/main.rs");
        assert_eq!(make_error_signature(&err), "");
    }

    #[test]
    fn test_make_error_signature_utf8_message() {
        let err = make_error(Some("E0308"), "类型不匹配", "src/main.rs");
        assert_eq!(make_error_signature(&err), "[E0308] 类型不匹配");
    }

    // --- should_detect_loop ---

    #[test]
    fn test_should_detect_loop_disabled() {
        assert!(!should_detect_loop(0, 10));
    }

    #[test]
    fn test_should_detect_loop_insufficient_rounds() {
        assert!(!should_detect_loop(3, 2));
    }

    #[test]
    fn test_should_detect_loop_exact_match() {
        assert!(should_detect_loop(3, 3));
    }

    #[test]
    fn test_should_detect_loop_more_than_enough() {
        assert!(should_detect_loop(3, 5));
    }

    #[test]
    fn test_should_detect_loop_both_zero() {
        assert!(!should_detect_loop(0, 0));
    }

    #[test]
    fn test_should_detect_loop_one_round() {
        assert!(should_detect_loop(1, 1));
        assert!(!should_detect_loop(1, 0));
    }

    // --- has_any_repeated_codes ---

    #[test]
    fn test_has_any_repeated_codes_empty_rounds() {
        assert!(!has_any_repeated_codes(&[], 3));
    }

    #[test]
    fn test_has_any_repeated_codes_true() {
        let rounds = vec![
            ErrorRound {
                codes: vec![Some("E0308".into())],
                signatures: vec![],
                files: vec![],
            },
            ErrorRound {
                codes: vec![Some("E0308".into())],
                signatures: vec![],
                files: vec![],
            },
            ErrorRound {
                codes: vec![Some("E0308".into())],
                signatures: vec![],
                files: vec![],
            },
        ];
        assert!(has_any_repeated_codes(&rounds, 3));
    }

    #[test]
    fn test_has_any_repeated_codes_false_different_codes() {
        let rounds = vec![
            ErrorRound {
                codes: vec![Some("E0308".into())],
                signatures: vec![],
                files: vec![],
            },
            ErrorRound {
                codes: vec![Some("E0382".into())],
                signatures: vec![],
                files: vec![],
            },
            ErrorRound {
                codes: vec![Some("E0425".into())],
                signatures: vec![],
                files: vec![],
            },
        ];
        assert!(!has_any_repeated_codes(&rounds, 3));
    }

    #[test]
    fn test_has_any_repeated_codes_none_codes_excluded() {
        let rounds = vec![
            ErrorRound {
                codes: vec![None],
                signatures: vec![],
                files: vec![],
            },
            ErrorRound {
                codes: vec![None],
                signatures: vec![],
                files: vec![],
            },
            ErrorRound {
                codes: vec![None],
                signatures: vec![],
                files: vec![],
            },
        ];
        assert!(!has_any_repeated_codes(&rounds, 3));
    }

    #[test]
    fn test_has_any_repeated_codes_multiple_errors_per_round() {
        let rounds = vec![
            ErrorRound {
                codes: vec![Some("E0308".into()), Some("E0382".into())],
                signatures: vec![],
                files: vec![],
            },
            ErrorRound {
                codes: vec![Some("E0308".into())],
                signatures: vec![],
                files: vec![],
            },
        ];
        // E0308 appears 2 times → threshold 2 → true
        assert!(has_any_repeated_codes(&rounds, 2));
    }

    #[test]
    fn test_has_any_repeated_codes_mixed_none_and_some() {
        let rounds = vec![
            ErrorRound {
                codes: vec![None, Some("E0308".into())],
                signatures: vec![],
                files: vec![],
            },
            ErrorRound {
                codes: vec![Some("E0308".into()), None],
                signatures: vec![],
                files: vec![],
            },
        ];
        assert!(has_any_repeated_codes(&rounds, 2));
    }

    #[test]
    fn test_has_any_repeated_codes_threshold_not_met() {
        let rounds = vec![
            ErrorRound {
                codes: vec![Some("E0308".into())],
                signatures: vec![],
                files: vec![],
            },
            ErrorRound {
                codes: vec![Some("E0308".into())],
                signatures: vec![],
                files: vec![],
            },
        ];
        assert!(!has_any_repeated_codes(&rounds, 3));
    }

    // --- has_any_repeated_signatures ---

    #[test]
    fn test_has_any_repeated_signatures_empty() {
        assert!(!has_any_repeated_signatures(&[], 3));
    }

    #[test]
    fn test_has_any_repeated_signatures_true() {
        let sig = "[E0308] error".to_string();
        let rounds = vec![
            ErrorRound {
                codes: vec![],
                signatures: vec![sig.clone()],
                files: vec![],
            },
            ErrorRound {
                codes: vec![],
                signatures: vec![sig.clone()],
                files: vec![],
            },
            ErrorRound {
                codes: vec![],
                signatures: vec![sig.clone()],
                files: vec![],
            },
        ];
        assert!(has_any_repeated_signatures(&rounds, 3));
    }

    #[test]
    fn test_has_any_repeated_signatures_false_different_sigs() {
        let rounds = vec![
            ErrorRound {
                codes: vec![],
                signatures: vec!["sig A".into()],
                files: vec![],
            },
            ErrorRound {
                codes: vec![],
                signatures: vec!["sig B".into()],
                files: vec![],
            },
            ErrorRound {
                codes: vec![],
                signatures: vec!["sig C".into()],
                files: vec![],
            },
        ];
        assert!(!has_any_repeated_signatures(&rounds, 3));
    }

    #[test]
    fn test_has_any_repeated_signatures_multiple_sigs_per_round() {
        let sig = "common sig".to_string();
        let rounds = vec![
            ErrorRound {
                codes: vec![],
                signatures: vec![sig.clone(), "other".into()],
                files: vec![],
            },
            ErrorRound {
                codes: vec![],
                signatures: vec![sig.clone()],
                files: vec![],
            },
        ];
        assert!(has_any_repeated_signatures(&rounds, 2));
    }

    // --- has_any_repeated_files ---

    #[test]
    fn test_has_any_repeated_files_empty() {
        assert!(!has_any_repeated_files(&[], 3));
    }

    #[test]
    fn test_has_any_repeated_files_true() {
        let rounds = vec![
            ErrorRound {
                codes: vec![],
                signatures: vec![],
                files: vec!["src/main.rs".into()],
            },
            ErrorRound {
                codes: vec![],
                signatures: vec![],
                files: vec!["src/main.rs".into()],
            },
            ErrorRound {
                codes: vec![],
                signatures: vec![],
                files: vec!["src/main.rs".into()],
            },
        ];
        assert!(has_any_repeated_files(&rounds, 3));
    }

    #[test]
    fn test_has_any_repeated_files_false_different_files() {
        let rounds = vec![
            ErrorRound {
                codes: vec![],
                signatures: vec![],
                files: vec!["src/main.rs".into()],
            },
            ErrorRound {
                codes: vec![],
                signatures: vec![],
                files: vec!["src/lib.rs".into()],
            },
            ErrorRound {
                codes: vec![],
                signatures: vec![],
                files: vec!["src/utils.rs".into()],
            },
        ];
        assert!(!has_any_repeated_files(&rounds, 3));
    }

    #[test]
    fn test_has_any_repeated_files_multiple_files_per_round() {
        let rounds = vec![
            ErrorRound {
                codes: vec![],
                signatures: vec![],
                files: vec!["src/main.rs".into(), "src/lib.rs".into()],
            },
            ErrorRound {
                codes: vec![],
                signatures: vec![],
                files: vec!["src/main.rs".into()],
            },
        ];
        assert!(has_any_repeated_files(&rounds, 2));
    }

    // --- collect_repeated_signatures ---

    #[test]
    fn test_collect_repeated_signatures_empty() {
        assert!(collect_repeated_signatures(&[], 3).is_empty());
    }

    #[test]
    fn test_collect_repeated_signatures_no_repeats() {
        let rounds = vec![
            ErrorRound {
                codes: vec![],
                signatures: vec!["sig A".into()],
                files: vec![],
            },
            ErrorRound {
                codes: vec![],
                signatures: vec!["sig B".into()],
                files: vec![],
            },
        ];
        assert!(collect_repeated_signatures(&rounds, 3).is_empty());
    }

    #[test]
    fn test_collect_repeated_signatures_one_repeat() {
        let sig = "[E0308] error".to_string();
        let rounds = vec![
            ErrorRound {
                codes: vec![],
                signatures: vec![sig.clone()],
                files: vec![],
            },
            ErrorRound {
                codes: vec![],
                signatures: vec![sig.clone()],
                files: vec![],
            },
            ErrorRound {
                codes: vec![],
                signatures: vec![sig.clone()],
                files: vec![],
            },
        ];
        let result = collect_repeated_signatures(&rounds, 3);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "[E0308] error");
        assert_eq!(result[0].1, 3);
    }

    #[test]
    fn test_collect_repeated_signatures_multiple_repeats() {
        let sig_a = "[E0308] error A".to_string();
        let sig_b = "[E0382] error B".to_string();
        let rounds = vec![
            ErrorRound {
                codes: vec![],
                signatures: vec![sig_a.clone(), sig_b.clone()],
                files: vec![],
            },
            ErrorRound {
                codes: vec![],
                signatures: vec![sig_a.clone(), sig_b.clone()],
                files: vec![],
            },
        ];
        let result = collect_repeated_signatures(&rounds, 2);
        assert_eq!(result.len(), 2);
        // Both have count 2; order between them is non-deterministic by sort
        // since both have count 2, but both should be present
        let sigs: Vec<&str> = result.iter().map(|(s, _)| s.as_str()).collect();
        assert!(sigs.contains(&"[E0308] error A"));
        assert!(sigs.contains(&"[E0382] error B"));
    }

    #[test]
    fn test_collect_repeated_signatures_sorted_by_count_desc() {
        let sig_a = "frequent".to_string();
        let sig_b = "rare".to_string();
        let rounds = vec![
            ErrorRound {
                codes: vec![],
                signatures: vec![sig_a.clone()],
                files: vec![],
            },
            ErrorRound {
                codes: vec![],
                signatures: vec![sig_a.clone()],
                files: vec![],
            },
            ErrorRound {
                codes: vec![],
                signatures: vec![sig_a.clone()],
                files: vec![],
            },
            ErrorRound {
                codes: vec![],
                signatures: vec![sig_b.clone()],
                files: vec![],
            },
            ErrorRound {
                codes: vec![],
                signatures: vec![sig_b.clone()],
                files: vec![],
            },
        ];
        let result = collect_repeated_signatures(&rounds, 2);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, "frequent");
        assert_eq!(result[0].1, 3);
        assert_eq!(result[1].0, "rare");
        assert_eq!(result[1].1, 2);
    }

    // --- collect_repeated_files ---

    #[test]
    fn test_collect_repeated_files_empty() {
        assert!(collect_repeated_files(&[], 3).is_empty());
    }

    #[test]
    fn test_collect_repeated_files_no_repeats() {
        let rounds = vec![
            ErrorRound {
                codes: vec![],
                signatures: vec![],
                files: vec!["src/main.rs".into()],
            },
            ErrorRound {
                codes: vec![],
                signatures: vec![],
                files: vec!["src/lib.rs".into()],
            },
        ];
        assert!(collect_repeated_files(&rounds, 3).is_empty());
    }

    #[test]
    fn test_collect_repeated_files_one_repeat() {
        let rounds = vec![
            ErrorRound {
                codes: vec![],
                signatures: vec![],
                files: vec!["src/main.rs".into()],
            },
            ErrorRound {
                codes: vec![],
                signatures: vec![],
                files: vec!["src/main.rs".into()],
            },
            ErrorRound {
                codes: vec![],
                signatures: vec![],
                files: vec!["src/main.rs".into()],
            },
        ];
        let result = collect_repeated_files(&rounds, 3);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "src/main.rs");
        assert_eq!(result[0].1, 3);
    }

    #[test]
    fn test_collect_repeated_files_multiple_repeats() {
        let rounds = vec![
            ErrorRound {
                codes: vec![],
                signatures: vec![],
                files: vec!["src/main.rs".into(), "src/lib.rs".into()],
            },
            ErrorRound {
                codes: vec![],
                signatures: vec![],
                files: vec!["src/main.rs".into(), "src/lib.rs".into()],
            },
        ];
        let result = collect_repeated_files(&rounds, 2);
        assert_eq!(result.len(), 2);
        let files: Vec<&str> = result.iter().map(|(f, _)| f.as_str()).collect();
        assert!(files.contains(&"src/main.rs"));
        assert!(files.contains(&"src/lib.rs"));
    }

    // --- format_repeated_summary ---

    #[test]
    fn test_format_repeated_summary_with_signatures() {
        let sigs = vec![("[E0308] mismatched types".into(), 3usize)];
        let files: Vec<(String, usize)> = vec![];
        let summary = format_repeated_summary(&sigs, &files);
        assert!(summary.contains("[E0308] mismatched types"));
        assert!(summary.contains("3 次"));
    }

    #[test]
    fn test_format_repeated_summary_fallback_to_files() {
        let sigs: Vec<(String, usize)> = vec![];
        let files = vec![("src/main.rs".into(), 3usize)];
        let summary = format_repeated_summary(&sigs, &files);
        assert!(summary.contains("src/main.rs"));
        assert!(summary.contains("3 次"));
        assert!(summary.contains("文件"));
    }

    #[test]
    fn test_format_repeated_summary_both_empty() {
        let summary = format_repeated_summary(&[], &[]);
        assert_eq!(summary, "(无法提取具体错误摘要)");
    }

    #[test]
    fn test_format_repeated_summary_signatures_take_priority() {
        let sigs = vec![("[E0308] sig".into(), 3usize)];
        let files = vec![("src/main.rs".into(), 3usize)];
        let summary = format_repeated_summary(&sigs, &files);
        assert!(summary.contains("[E0308] sig"));
        assert!(!summary.contains("文件")); // files should not appear when sigs present
    }

    #[test]
    fn test_format_repeated_summary_multiple_signatures() {
        let sigs = vec![
            ("[E0308] error A".into(), 3usize),
            ("[E0382] error B".into(), 3usize),
        ];
        let summary = format_repeated_summary(&sigs, &[]);
        assert!(summary.contains("error A"));
        assert!(summary.contains("error B"));
        assert!(summary.contains('\n')); // multiple entries joined by newline
    }

    #[test]
    fn test_format_repeated_summary_truncates_long_signature() {
        let long_sig = "x".repeat(200);
        let sigs = vec![(long_sig, 3usize)];
        let summary = format_repeated_summary(&sigs, &[]);
        // Should be truncated to 150 chars in the display
        let display_part: String = summary.chars().filter(|c| *c == 'x').collect();
        assert_eq!(display_part.chars().count(), 150);
    }

    #[test]
    fn test_format_repeated_summary_multiple_files() {
        let files = vec![
            ("src/main.rs".into(), 3usize),
            ("src/lib.rs".into(), 3usize),
        ];
        let summary = format_repeated_summary(&[], &files);
        assert!(summary.contains("src/main.rs"));
        assert!(summary.contains("src/lib.rs"));
        assert!(summary.contains('\n'));
    }

    // --- build_strategy_change_prompt_text ---

    #[test]
    fn test_build_strategy_change_prompt_text_basic() {
        let prompt = build_strategy_change_prompt_text(3, "  - [E0308] error (出现 3 次)");
        assert!(prompt.contains("循环终止检测"));
        assert!(prompt.contains("换一种完全不同的方法"));
        assert!(prompt.contains("[E0308] error"));
        assert!(prompt.contains("3 次"));
    }

    #[test]
    fn test_build_strategy_change_prompt_text_empty_summary() {
        let prompt = build_strategy_change_prompt_text(2, "(无法提取具体错误摘要)");
        assert!(prompt.contains("2 次"));
        assert!(prompt.contains("无法提取"));
    }

    #[test]
    fn test_build_strategy_change_prompt_text_max_repeats_one() {
        let prompt = build_strategy_change_prompt_text(1, "  - error (出现 1 次)");
        assert!(prompt.contains("1 次"));
    }

    #[test]
    fn test_build_strategy_change_prompt_text_contains_strategy_options() {
        let prompt = build_strategy_change_prompt_text(3, "");
        assert!(prompt.contains("重构"));
        assert!(prompt.contains("数据类型"));
        assert!(prompt.contains("借用错误"));
        assert!(prompt.contains("导入错误"));
    }

    #[test]
    fn test_build_strategy_change_prompt_text_contains_file_format_instruction() {
        let prompt = build_strategy_change_prompt_text(3, "");
        assert!(prompt.contains("file:路径"));
    }

    // --- build_skip_prompt_text ---

    #[test]
    fn test_build_skip_prompt_text_basic() {
        let prompt = build_skip_prompt_text(3);
        assert!(prompt.contains("跳过"));
        assert!(prompt.contains("3 次"));
    }

    #[test]
    fn test_build_skip_prompt_text_max_repeats_one() {
        let prompt = build_skip_prompt_text(1);
        assert!(prompt.contains("1 次"));
    }

    #[test]
    fn test_build_skip_prompt_text_max_repeats_large() {
        let prompt = build_skip_prompt_text(100);
        assert!(prompt.contains("100 次"));
    }

    #[test]
    fn test_build_skip_prompt_text_contains_human_intervention() {
        let prompt = build_skip_prompt_text(3);
        assert!(prompt.contains("人工介入") || prompt.contains("上下文"));
    }

    #[test]
    fn test_build_skip_prompt_text_contains_best_attempt_instruction() {
        let prompt = build_skip_prompt_text(3);
        assert!(prompt.contains("最佳尝试"));
    }

    // --- should_skip_task ---

    #[test]
    fn test_should_skip_task_true() {
        assert!(should_skip_task(true, true));
    }

    #[test]
    fn test_should_skip_task_false_strategy_not_changed() {
        assert!(!should_skip_task(false, true));
    }

    #[test]
    fn test_should_skip_task_false_not_looping() {
        assert!(!should_skip_task(true, false));
    }

    #[test]
    fn test_should_skip_task_false_both_false() {
        assert!(!should_skip_task(false, false));
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
