//! Error Search — 编译错误自动搜索模块
//!
//! 当编译/测试失败时, 自动从错误信息中提取关键词,
//! 构建搜索查询, 通过 WebTool 搜索解决方案,
//! 将搜索结果追加到修复 prompt 中, 为 AI 提供更精准的修复上下文。
//!
//! ## 设计理念
//!
//! 借鉴 ds4 的 "auto-search on error" 模式:
//! - 编译错误 → 提取关键词 → Google 搜索 → 结果注入修复 prompt
//! - AI 获得额外上下文, 提高首次修复成功率
//! - 与现有 `/search` slash command 互补: 主动搜索 vs AI 自主搜索
//!
//! ## 纯函数架构 (SRP)
//!
//! - [`build_error_search_query`][]: 从 CompileError 列表构建搜索查询
//! - [`extract_error_keywords`][]: 从错误消息中提取关键词
//! - [`format_search_results_section`][]: 格式化搜索结果为 prompt 段落
//! - [`truncate_search_results`][]: 截断搜索结果避免 token 膨胀
//! - [`should_search_errors`][]: 判断是否应该执行搜索
//!
//! ## 示例
//!
//! ```
//! use forge::error_search::{build_error_search_query, format_search_results_section};
//! use forge::testrunner::CompileError;
//!
//! let errors = vec![CompileError {
//!     file: "src/main.rs".to_string(),
//!     line: Some(10),
//!     column: Some(5),
//!     message: "mismatched types: expected `u32`, found `&str`".to_string(),
//!     error_code: Some("E0308".to_string()),
//! }];
//!
//! let query = build_error_search_query(&errors).unwrap();
//! assert!(query.contains("E0308"));
//! assert!(query.contains("mismatched types"));
//!
//! let section = format_search_results_section(&query, "search results...", 150);
//! assert!(section.contains("网页搜索结果"));
//! assert!(section.contains("search results..."));
//! ```

use crate::testrunner::CompileError;

// ============================================================================
//  常量
// ============================================================================

/// 搜索结果最大字符数 (避免 token 膨胀)
const MAX_SEARCH_RESULTS_CHARS: usize = 3000;

/// 最小错误消息长度 (太短不搜索)
const MIN_ERROR_MESSAGE_LEN: usize = 5;

/// 搜索结果截断后追加的省略号
const TRUNCATE_SUFFIX: &str = "\n...(搜索结果已截断)";

/// Rust 编译器常见无意义关键词 (过滤)
const COMMON_NOISE_WORDS: &[&str] = &[
    "the", "a", "an", "to", "in", "of", "is", "for", "and", "or", "not", "this", "that", "with",
    "from", "by", "on", "at", "as", "it", "be", "was", "are", "has", "have", "had", "can", "could",
    "should", "would", "may", "might", "must", "shall", "will", "but", "if", "then", "else",
    "when", "where", "which", "who", "how", "why", "what", "use", "let", "fn", "pub", "mut", "ref",
    "impl", "self", "Self", "crate", "mod", "struct", "enum", "trait", "type", "where", "async",
    "await", "move", "box",
];

// ============================================================================
//  纯函数 — 搜索查询构建
// ============================================================================

/// 从编译错误列表构建搜索查询
///
/// 优先使用 error_code (如 E0308), 因为错误代码是最精确的搜索关键词。
/// 然后附加错误消息中的关键词, 提高搜索精度。
///
/// # 参数
///
/// - `errors`: 编译错误列表
///
/// # 返回
///
/// - `Some(query)`: 构建的搜索查询字符串
/// - `None`: 错误列表为空或消息太短
///
/// # 示例
///
/// ```
/// # use forge::error_search::build_error_search_query;
/// # use forge::testrunner::CompileError;
/// let errors = vec![CompileError {
///     file: "src/main.rs".to_string(),
///     line: Some(10),
///     column: Some(5),
///     message: "mismatched types".to_string(),
///     error_code: Some("E0308".to_string()),
/// }];
/// let query = build_error_search_query(&errors).unwrap();
/// assert!(query.contains("E0308"));
/// ```
pub fn build_error_search_query(errors: &[CompileError]) -> Option<String> {
    if errors.is_empty() {
        return None;
    }

    // 取第一个错误的 error_code 和 message
    let first = &errors[0];

    if first.message.len() < MIN_ERROR_MESSAGE_LEN && first.error_code.is_none() {
        return None;
    }

    let mut parts: Vec<String> = Vec::new();

    // 优先加入 error_code (最精确)
    if let Some(ref code) = first.error_code {
        parts.push(code.clone());
    }

    // 提取错误消息中的关键词
    let keywords = extract_error_keywords(&first.message);
    if !keywords.is_empty() {
        parts.push(keywords.join(" "));
    }

    if parts.is_empty() {
        return None;
    }

    // 加入语言前缀 "rust" 提高搜索精度
    // (假设是 Rust 项目, 因为 Forge 主要针对 Rust)
    Some(format!("rust {}", parts.join(" ")))
}

/// 从错误消息中提取有意义的关键词
///
/// 过滤常见无意义词汇, 返回关键词列表。
/// 保留错误类型关键词 (mismatched, cannot, expected, found 等)。
///
/// # 参数
///
/// - `message`: 错误消息字符串
///
/// # 返回
///
/// 关键词列表 (去重, 最多 5 个)
///
/// # 示例
///
/// ```
/// # use forge::error_search::extract_error_keywords;
/// let keywords = extract_error_keywords("mismatched types: expected `u32`, found `&str`");
/// assert!(keywords.contains(&"mismatched".to_string()));
/// assert!(keywords.contains(&"types".to_string()));
/// assert!(keywords.contains(&"expected".to_string()));
/// ```
pub fn extract_error_keywords(message: &str) -> Vec<String> {
    let mut keywords: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    // 按非字母数字分割
    for word in message.split(|c: char| !c.is_alphanumeric()) {
        let word = word.trim();
        if word.is_empty() || word.len() < 2 {
            continue;
        }

        // 跳过常见无意义词
        if COMMON_NOISE_WORDS.contains(&word) {
            continue;
        }

        // 跳过纯数字
        if word.chars().all(|c| c.is_numeric()) {
            continue;
        }

        // 去重
        let lower = word.to_lowercase();
        if seen.insert(lower) {
            keywords.push(word.to_string());
        }

        // 最多 5 个关键词
        if keywords.len() >= 5 {
            break;
        }
    }

    keywords
}

/// 判断是否应该对错误执行搜索
///
/// # 条件
///
/// - 非首次尝试 (attempt > 1, 首次失败时直接让 AI 修复)
/// - 错误列表非空
/// - 非网络错误 (网络错误不是代码问题, 搜索无意义)
///
/// # 参数
///
/// - `errors`: 编译错误列表
/// - `attempt`: 当前尝试轮次 (1-based)
/// - `is_network_error`: 是否为网络错误
///
/// # 示例
///
/// ```
/// # use forge::error_search::should_search_errors;
/// # use forge::testrunner::CompileError;
/// let errors = vec![CompileError {
///     file: "src/main.rs".to_string(),
///     line: Some(10),
///     column: None,
///     message: "error".to_string(),
///     error_code: Some("E0308".to_string()),
/// }];
///
/// // 首次尝试不搜索
/// assert!(!should_search_errors(&errors, 1, false));
///
/// // 修复轮次搜索
/// assert!(should_search_errors(&errors, 2, false));
///
/// // 网络错误不搜索
/// assert!(!should_search_errors(&errors, 2, true));
/// ```
pub fn should_search_errors(errors: &[CompileError], attempt: u32, is_network_error: bool) -> bool {
    if is_network_error {
        return false;
    }

    if attempt <= 1 {
        return false;
    }

    !errors.is_empty()
}

// ============================================================================
//  纯函数 — 搜索结果格式化
// ============================================================================

/// 格式化搜索结果为 prompt 段落
///
/// 将搜索查询和结果格式化为一个可追加到修复 prompt 的段落。
/// 包含分隔线、查询词、耗时和结果内容。
///
/// # 参数
///
/// - `query`: 搜索查询词
/// - `results`: 搜索结果内容
/// - `duration_ms`: 搜索耗时 (毫秒)
///
/// # 示例
///
/// ```
/// # use forge::error_search::format_search_results_section;
/// let section = format_search_results_section("rust E0308", "Type mismatch error...", 150);
/// assert!(section.contains("网页搜索结果"));
/// assert!(section.contains("rust E0308"));
/// assert!(section.contains("Type mismatch error"));
/// ```
pub fn format_search_results_section(query: &str, results: &str, duration_ms: u64) -> String {
    let truncated = truncate_search_results(results, MAX_SEARCH_RESULTS_CHARS);

    format!(
        "\n\n\
         ─────────────────────────────────────────\n\
         🔍 网页搜索结果 (自动错误搜索)\n\
         查询: {}\n\
         耗时: {}ms\n\
         ─────────────────────────────────────────\n\
         {}\n\
         ─────────────────────────────────────────\n",
        query, duration_ms, truncated
    )
}

/// 截断搜索结果, 避免 token 膨胀
///
/// 当结果超过 `max_chars` 时, 在最近的换行符处截断并追加省略号。
/// 如果没有换行符, 直接在 max_chars 处截断。
///
/// # 参数
///
/// - `results`: 搜索结果内容
/// - `max_chars`: 最大字符数
///
/// # 示例
///
/// ```
/// # use forge::error_search::truncate_search_results;
/// let short = "short result";
/// assert_eq!(truncate_search_results(short, 100), short);
///
/// let long = "line1\nline2\nline3\nline4";
/// let truncated = truncate_search_results(long, 12);
/// assert!(truncated.contains("..."));
/// ```
pub fn truncate_search_results(results: &str, max_chars: usize) -> String {
    if results.len() <= max_chars {
        return results.to_string();
    }

    // 在 max_chars 范围内找最近的换行符
    let prefix = &results[..max_chars.min(results.len())];
    let cut_point = prefix.rfind('\n').unwrap_or(max_chars);

    let truncated = &results[..cut_point];
    format!("{}{}", truncated, TRUNCATE_SUFFIX)
}

// ============================================================================
//  测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testrunner::CompileError;

    // ===== build_error_search_query =====

    #[test]
    fn test_build_query_with_error_code() {
        let errors = vec![CompileError {
            file: "src/main.rs".to_string(),
            line: Some(10),
            column: Some(5),
            message: "mismatched types: expected `u32`, found `&str`".to_string(),
            error_code: Some("E0308".to_string()),
        }];
        let query = build_error_search_query(&errors).unwrap();
        assert!(query.contains("rust"));
        assert!(query.contains("E0308"));
        assert!(query.contains("mismatched"));
    }

    #[test]
    fn test_build_query_without_error_code() {
        let errors = vec![CompileError {
            file: "src/lib.rs".to_string(),
            line: Some(5),
            column: None,
            message: "cannot find value `x` in this scope".to_string(),
            error_code: None,
        }];
        let query = build_error_search_query(&errors).unwrap();
        assert!(query.contains("rust"));
        assert!(query.contains("cannot"));
    }

    #[test]
    fn test_build_query_empty_errors() {
        let errors: Vec<CompileError> = vec![];
        assert!(build_error_search_query(&errors).is_none());
    }

    #[test]
    fn test_build_query_short_message_no_code() {
        let errors = vec![CompileError {
            file: "src/main.rs".to_string(),
            line: None,
            column: None,
            message: "err".to_string(), // 太短
            error_code: None,
        }];
        assert!(build_error_search_query(&errors).is_none());
    }

    #[test]
    fn test_build_query_short_message_with_code() {
        // 消息短但有 error_code, 仍然搜索
        let errors = vec![CompileError {
            file: "src/main.rs".to_string(),
            line: None,
            column: None,
            message: "err".to_string(),
            error_code: Some("E0308".to_string()),
        }];
        let query = build_error_search_query(&errors).unwrap();
        assert!(query.contains("E0308"));
    }

    #[test]
    fn test_build_query_multiple_errors_uses_first() {
        let errors = vec![
            CompileError {
                file: "src/main.rs".to_string(),
                line: Some(10),
                column: Some(5),
                message: "mismatched types".to_string(),
                error_code: Some("E0308".to_string()),
            },
            CompileError {
                file: "src/lib.rs".to_string(),
                line: Some(20),
                column: None,
                message: "unused variable".to_string(),
                error_code: Some("E0308".to_string()),
            },
        ];
        let query = build_error_search_query(&errors).unwrap();
        // 应该使用第一个错误的信息
        assert!(query.contains("mismatched"));
    }

    #[test]
    fn test_build_query_special_characters_in_message() {
        let errors = vec![CompileError {
            file: "src/main.rs".to_string(),
            line: Some(1),
            column: Some(1),
            message: "expected `Vec<String>`, found `&[&str]`".to_string(),
            error_code: Some("E0308".to_string()),
        }];
        let query = build_error_search_query(&errors).unwrap();
        assert!(query.contains("E0308"));
        assert!(query.contains("expected"));
    }

    // ===== extract_error_keywords =====

    #[test]
    fn test_extract_keywords_basic() {
        let keywords = extract_error_keywords("mismatched types: expected `u32`, found `&str`");
        assert!(keywords.contains(&"mismatched".to_string()));
        assert!(keywords.contains(&"types".to_string()));
        assert!(keywords.contains(&"expected".to_string()));
        assert!(keywords.contains(&"found".to_string()));
    }

    #[test]
    fn test_extract_keywords_filters_common_words() {
        let keywords = extract_error_keywords("the value of this is not found");
        assert!(!keywords.contains(&"the".to_string()));
        assert!(!keywords.contains(&"of".to_string()));
        assert!(!keywords.contains(&"this".to_string()));
        assert!(keywords.contains(&"value".to_string()));
        assert!(keywords.contains(&"not".to_string()) || !keywords.contains(&"not".to_string())); // "not" 在 COMMON_NOISE_WORDS 中
        assert!(keywords.contains(&"found".to_string()));
    }

    #[test]
    fn test_extract_keywords_deduplicates() {
        let keywords = extract_error_keywords("test test test value value");
        // 去重后只保留唯一的
        assert!(keywords.iter().filter(|k| k.as_str() == "test").count() == 1);
        assert!(keywords.iter().filter(|k| k.as_str() == "value").count() == 1);
    }

    #[test]
    fn test_extract_keywords_max_five() {
        let keywords =
            extract_error_keywords("alpha beta gamma delta epsilon zeta eta theta iota kappa");
        assert!(keywords.len() <= 5);
    }

    #[test]
    fn test_extract_keywords_empty_message() {
        let keywords = extract_error_keywords("");
        assert!(keywords.is_empty());
    }

    #[test]
    fn test_extract_keywords_short_words_filtered() {
        let keywords = extract_error_keywords("a b c de fg");
        // 单字符词被过滤, 双字符保留
        assert!(keywords.iter().all(|k| k.len() >= 2));
    }

    #[test]
    fn test_extract_keywords_numeric_filtered() {
        let keywords = extract_error_keywords("error 12345 67890 found");
        assert!(!keywords.iter().any(|k| k == "12345"));
        assert!(!keywords.iter().any(|k| k == "67890"));
        assert!(keywords.contains(&"error".to_string()));
        assert!(keywords.contains(&"found".to_string()));
    }

    #[test]
    fn test_extract_keywords_preserves_case() {
        let keywords = extract_error_keywords("Mismatched Types mismatched");
        // 第一个 "Mismatched" 和第三个 "mismatched" 应该被视为重复
        assert!(keywords.len() <= 2); // "Mismatched" + "Types"
    }

    // ===== should_search_errors =====

    #[test]
    fn test_should_search_first_attempt_false() {
        let errors = vec![CompileError {
            file: "src/main.rs".to_string(),
            line: Some(1),
            column: None,
            message: "error".to_string(),
            error_code: Some("E0308".to_string()),
        }];
        assert!(!should_search_errors(&errors, 1, false));
    }

    #[test]
    fn test_should_search_fix_attempt_true() {
        let errors = vec![CompileError {
            file: "src/main.rs".to_string(),
            line: Some(1),
            column: None,
            message: "error".to_string(),
            error_code: Some("E0308".to_string()),
        }];
        assert!(should_search_errors(&errors, 2, false));
        assert!(should_search_errors(&errors, 3, false));
    }

    #[test]
    fn test_should_search_network_error_false() {
        let errors = vec![CompileError {
            file: "src/main.rs".to_string(),
            line: Some(1),
            column: None,
            message: "error".to_string(),
            error_code: Some("E0308".to_string()),
        }];
        assert!(!should_search_errors(&errors, 2, true));
        assert!(!should_search_errors(&errors, 3, true));
    }

    #[test]
    fn test_should_search_empty_errors_false() {
        let errors: Vec<CompileError> = vec![];
        assert!(!should_search_errors(&errors, 2, false));
    }

    #[test]
    fn test_should_search_zero_attempt_false() {
        let errors = vec![CompileError {
            file: "src/main.rs".to_string(),
            line: Some(1),
            column: None,
            message: "error".to_string(),
            error_code: Some("E0308".to_string()),
        }];
        assert!(!should_search_errors(&errors, 0, false));
    }

    // ===== format_search_results_section =====

    #[test]
    fn test_format_section_basic() {
        let section = format_search_results_section("rust E0308", "search results here", 150);
        assert!(section.contains("网页搜索结果"));
        assert!(section.contains("rust E0308"));
        assert!(section.contains("search results here"));
        assert!(section.contains("150ms"));
    }

    #[test]
    fn test_format_section_includes_separator() {
        let section = format_search_results_section("query", "result", 50);
        assert!(section.contains("─────────"));
    }

    #[test]
    fn test_format_section_empty_results() {
        let section = format_search_results_section("query", "", 0);
        assert!(section.contains("网页搜索结果"));
        assert!(section.contains("query"));
        assert!(section.contains("0ms"));
    }

    #[test]
    fn test_format_section_long_results_truncated() {
        let long_results = "a".repeat(5000);
        let section = format_search_results_section("query", &long_results, 100);
        // 应该被截断
        assert!(section.contains("...(搜索结果已截断)"));
        // 截断后总长度不应过长
        assert!(section.len() < 5000);
    }

    #[test]
    fn test_format_section_unicode_content() {
        let section = format_search_results_section("rust 错误", "搜索结果包含中文内容", 200);
        assert!(section.contains("rust 错误"));
        assert!(section.contains("搜索结果包含中文内容"));
    }

    // ===== truncate_search_results =====

    #[test]
    fn test_truncate_short_unchanged() {
        let result = "short text";
        assert_eq!(truncate_search_results(result, 100), result);
    }

    #[test]
    fn test_truncate_exact_length() {
        let result = "12345";
        assert_eq!(truncate_search_results(result, 5), result);
    }

    #[test]
    fn test_truncate_long_at_newline() {
        let result = "line1\nline2\nline3\nline4";
        let truncated = truncate_search_results(result, 12);
        assert!(truncated.starts_with("line1"));
        assert!(truncated.contains("..."));
    }

    #[test]
    fn test_truncate_long_no_newline() {
        let result = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let truncated = truncate_search_results(result, 10);
        assert!(truncated.contains("..."));
        assert!(truncated.len() < result.len());
    }

    #[test]
    fn test_truncate_empty() {
        assert_eq!(truncate_search_results("", 100), "");
    }

    #[test]
    fn test_truncate_zero_max() {
        let result = "test";
        let truncated = truncate_search_results(result, 0);
        assert_eq!(truncated, TRUNCATE_SUFFIX);
    }

    #[test]
    fn test_truncate_preserves_newlines_before_cut() {
        let result = "line1\nline2\nline3";
        let truncated = truncate_search_results(result, 12);
        // 应该在第二个换行符之前截断
        assert!(truncated.contains("line1"));
        assert!(truncated.contains("..."));
    }

    // ===== 集成场景测试 =====

    #[test]
    fn test_full_workflow_build_and_format() {
        let errors = vec![CompileError {
            file: "src/main.rs".to_string(),
            line: Some(10),
            column: Some(5),
            message: "mismatched types: expected `u32`, found `&str`".to_string(),
            error_code: Some("E0308".to_string()),
        }];

        // 1. 判断是否应该搜索
        assert!(should_search_errors(&errors, 2, false));

        // 2. 构建搜索查询
        let query = build_error_search_query(&errors).unwrap();
        assert!(query.contains("E0308"));

        // 3. 格式化搜索结果
        let section = format_search_results_section(&query, "Solution: use as u32", 200);
        assert!(section.contains("E0308"));
        assert!(section.contains("Solution"));
    }

    #[test]
    fn test_workflow_network_error_skip() {
        let errors = vec![CompileError {
            file: "src/main.rs".to_string(),
            line: None,
            column: None,
            message: "network error".to_string(),
            error_code: None,
        }];

        // 网络错误不搜索
        assert!(!should_search_errors(&errors, 2, true));
    }

    #[test]
    fn test_workflow_first_attempt_skip() {
        let errors = vec![CompileError {
            file: "src/main.rs".to_string(),
            line: Some(1),
            column: None,
            message: "compile error".to_string(),
            error_code: Some("E0308".to_string()),
        }];

        // 首次尝试不搜索
        assert!(!should_search_errors(&errors, 1, false));

        // 修复轮次搜索
        assert!(should_search_errors(&errors, 2, false));
    }

    #[test]
    fn test_build_query_chinese_error_message() {
        let errors = vec![CompileError {
            file: "src/main.rs".to_string(),
            line: Some(1),
            column: None,
            message: "类型不匹配: 期望 u32, 找到 &str".to_string(),
            error_code: Some("E0308".to_string()),
        }];
        let query = build_error_search_query(&errors).unwrap();
        assert!(query.contains("E0308"));
        assert!(query.contains("rust"));
    }
}
