//! 从 AI 回复中提取代码文件
//!
//! 性能优化: 使用 `OnceLock` 预编译正则表达式, 避免每次调用重新编译。

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::OnceLock;
use tracing::{debug, info, warn};

/// 预编译正则: 规范化 file: 标记 (file:path 后跟多个空格 → 插入换行)
///
/// 使用 `OnceLock` 只编译一次, 后续调用直接复用。
fn normalize_file_markers_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?m)^(file:\S+)\s{2,}").expect("预编译 normalize_file_markers 正则失败")
    })
}

/// 预编译正则: 模式1 — ```file:path\n...``` 或 ```lang:path\n...```
fn tagged_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?s)```(?:file|rust|python|toml|yaml|json|markdown|md|shell|bash|sh|javascript|js|typescript|ts|html|css):([^\n]+?)\n(.*?)```"
        ).expect("预编译 tagged 正则失败")
    })
}

/// 预编译正则: 模式2 — 普通 ```lang\n...``` 代码块 (无路径)
fn plain_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)```(\w+)\n(.*?)```").expect("预编译 plain 正则失败"))
}

/// 预编译正则: 模式3 — file:path 行标记 (无代码块)
fn file_marker_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)^file:(.+)$").expect("预编译 file_marker 正则失败"))
}

/// 一个提取出的文件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedFile {
    pub path: String,
    pub content: String,
    pub language: String,
}

/// 默认文件提取器 — 使用正则从 AI 回复提取代码文件
///
/// 实现 `FileExtractor` trait (DIP)。
pub struct DefaultExtractor;

impl crate::traits::FileExtractor for DefaultExtractor {
    fn extract(&self, text: &str) -> Vec<ExtractedFile> {
        extract_files(text)
    }
}

/// 清理 AI 回复中的 UI 文本污染
///
/// DeepSeek 等网站的代码块有 "复制下载" 按钮文本,
/// 这些文本会泄漏到提取的回复中, 导致文件路径错误。
/// 本函数移除这些已知的 UI 文本。
fn clean_ui_text(text: &str) -> String {
    // DeepSeek 的 "复制下载" 按钮文本 (可能出现在文件路径和代码块之间)
    // 例如: "file:Cargo.toml复制下载[package]" → "file:Cargo.toml\n[package]"
    let mut result = text.to_string();

    // 将 "复制下载" 及其变体替换为换行符 (恢复被 UI 文本覆盖的换行)
    for pattern in &[
        "复制下载",
        "复制\n下载",
        "下载复制",
        "复制 下载",
        "下载 复制",
    ] {
        result = result.replace(pattern, "\n");
    }

    // 移除 file: 行中残留的 "复制" 或 "下载" (单独出现的情况)
    // 注意: 不移除代码内容中的 "复制" 和 "下载" 字样
    let lines: Vec<&str> = result.lines().collect();
    let mut cleaned_lines = Vec::new();
    for line in lines {
        // 如果行以 "file:" 开头, 清理路径中的 UI 文本
        if line.starts_with("file:") {
            let cleaned_line = line.replace("复制", "").replace("下载", "");
            cleaned_lines.push(cleaned_line);
        } else {
            cleaned_lines.push(line.to_string());
        }
    }
    cleaned_lines.join("\n")
}

/// 规范化 file: 标记 — 在 file:path 后插入换行符
///
/// AI 回复可能因 DOM 提取问题导致 file:path 和代码内容在同一行
/// (如 "file:Cargo.toml     [package]..." → "file:Cargo.toml\n[package]...")
/// 此函数检测 file:path 后跟多个空格的情况, 插入换行符分隔路径和内容
fn normalize_file_markers(text: &str) -> String {
    let re = normalize_file_markers_regex();
    re.replace_all(text, "$1\n").to_string()
}

/// 验证 TOML 格式基本语法 (第 39 项改进)
///
/// AI 生成的 Cargo.toml 可能存在格式错误 (如 "unclosed table"),
/// 本函数检查基本的 TOML 语法:
/// - 方括号配对 ([...])
/// - 双引号配对 ("...")
/// - 不完整的表头 (如 [package 后缺 ])
///
/// 返回 Some(warning_message) 如果检测到问题, None 如果格式正常。
fn validate_toml(content: &str) -> Option<String> {
    let mut issues = Vec::new();

    // 1. 检查方括号配对 (行级检查, 避免跨行误判)
    for (line_num, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        // 跳过注释行
        if trimmed.starts_with('#') {
            continue;
        }
        // 检查表头 [section] 或 [[array-of-tables]]
        if trimmed.starts_with('[') {
            let bracket_count = trimmed.chars().filter(|&c| c == '[').count();
            let close_count = trimmed.chars().filter(|&c| c == ']').count();
            if bracket_count != close_count {
                // 允许多行表头? TOML 不支持多行表头
                issues.push(format!(
                    "行 {}: 表头方括号不配对 ({} 个 [ vs {} 个 ]): {}",
                    line_num + 1,
                    bracket_count,
                    close_count,
                    trimmed
                ));
            }
        }
    }

    // 2. 检查全局方括号配对
    let open_brackets = content.chars().filter(|&c| c == '[').count();
    let close_brackets = content.chars().filter(|&c| c == ']').count();
    if open_brackets != close_brackets {
        issues.push(format!(
            "全局方括号不配对 ({} 个 [ vs {} 个 ])",
            open_brackets, close_brackets
        ));
    }

    // 3. 检查双引号配对 (全局)
    let quote_count = content.chars().filter(|&c| c == '"').count();
    if quote_count % 2 != 0 {
        issues.push(format!("双引号不配对 ({} 个, 应为偶数)", quote_count));
    }

    // 4. 检查 [package] 表是否存在 (Cargo.toml 必须有)
    let has_package = content
        .lines()
        .any(|l| l.trim() == "[package]" || l.trim().starts_with("[package"));
    if !has_package {
        issues.push("缺少 [package] 表".to_string());
    }

    if issues.is_empty() {
        None
    } else {
        Some(issues.join("; "))
    }
}

/// 验证 Rust 代码的括号配对 (Session 113, Session 114 增强)
///
/// AI 生成的 Rust 代码 (特别是 GLM 模型) 可能存在括号不配对的问题:
/// - `{` 和 `}` 不配对 (最常见的大括号匹配问题)
/// - `(` 和 `)` 不配对
/// - `[` 和 `]` 不配对
///
/// 本函数在跳过字符串、字符字面量和注释中的括号后, 检查括号配对。
///
/// # 支持的 Rust 语法 (Session 114 增强)
///
/// - 嵌套块注释: `/* /* */ */` (Rust 支持嵌套, C 不支持)
/// - 字节字符串: `b"..."` — 字符串内容不影响计数
/// - 字节字符: `b'x'` — 字符内容不影响计数
/// - 原始字节字符串: `br"..."`, `br#"..."#` — 内容不影响计数
/// - 未终止字符串/注释检测: 字符串或注释未闭合时报告问题
///
/// 返回 `Some(warning_message)` 如果检测到问题, `None` 如果格式正常。
///
/// # 示例
///
/// ```
/// use forge::extract::validate_rust_braces;
///
/// // 配对的代码 — 无问题
/// assert!(validate_rust_braces("fn main() { let x = vec![1, 2, 3]; }").is_none());
///
/// // 缺少闭合大括号 — 应报告问题
/// let result = validate_rust_braces("fn main() { let x = 42;");
/// assert!(result.is_some());
/// assert!(result.unwrap().contains("大括号"));
///
/// // 嵌套块注释 — 内容不影响计数 (Session 114)
/// assert!(validate_rust_braces("fn main() { /* /* { } */ */ let x = 42; }").is_none());
/// ```
pub fn validate_rust_braces(content: &str) -> Option<String> {
    let mut issues = Vec::new();

    // 状态机: 跟踪字符串/注释状态, 只在代码状态中计数括号
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Code,
        LineComment,         // // ...
        BlockComment(usize), // /* ... */ (支持嵌套, depth >= 1)
        String,              // "..."
        RawString(usize),    // r#"..."# (hash_depth)
    }

    let mut state = State::Code;
    let mut brace_open = 0i32;
    let mut brace_close = 0i32;
    let mut paren_open = 0i32;
    let mut paren_close = 0i32;
    let mut bracket_open = 0i32;
    let mut bracket_close = 0i32;

    let chars: Vec<char> = content.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();

        state = match (state, c, next) {
            // === 从代码状态转换 ===
            (State::Code, '/', Some('/')) => {
                i += 1; // 跳过第二个 /
                State::LineComment
            }
            (State::Code, '/', Some('*')) => {
                i += 1; // 跳过 *
                State::BlockComment(1)
            }
            (State::Code, '"', _) => State::String,
            (State::Code, 'r', Some('"')) => {
                // r"..." 或 r#"..."#
                i += 1; // 跳过 "
                let mut hash_depth = 0;
                while i + 1 < chars.len() && chars[i + 1] == '#' {
                    hash_depth += 1;
                    i += 1;
                }
                State::RawString(hash_depth)
            }
            (State::Code, 'r', Some('#')) => {
                // r#"..."#
                let mut hash_depth = 0;
                while i + 1 < chars.len() && chars[i + 1] == '#' {
                    hash_depth += 1;
                    i += 1;
                }
                if i + 1 < chars.len() && chars[i + 1] == '"' {
                    i += 1; // 跳过 "
                    State::RawString(hash_depth)
                } else {
                    // 不是 raw string, 当作普通字符
                    State::Code
                }
            }
            (State::Code, '\'', _) => {
                // 可能是字符字面量 'x' 或生命周期 'a
                // 简单判断: 如果后面是单字符+引号, 则是字符字面量
                if chars.len() > i + 2 && chars[i + 2] == '\'' {
                    i += 2; // 跳过字符和闭合引号
                    State::Code
                } else if chars.len() > i + 3 && chars[i + 1] == '\\' && chars[i + 3] == '\'' {
                    // 转义字符 '\n' '\t' 等
                    i += 3;
                    State::Code
                } else {
                    // 可能是生命周期标注 'a — 不进入字符状态
                    State::Code
                }
            }
            (State::Code, '{', _) => {
                brace_open += 1;
                State::Code
            }
            (State::Code, '}', _) => {
                brace_close += 1;
                State::Code
            }
            (State::Code, '(', _) => {
                paren_open += 1;
                State::Code
            }
            (State::Code, ')', _) => {
                paren_close += 1;
                State::Code
            }
            (State::Code, '[', _) => {
                bracket_open += 1;
                State::Code
            }
            (State::Code, ']', _) => {
                bracket_close += 1;
                State::Code
            }

            // === Code 状态: 其他字符保持不变 ===
            (State::Code, _, _) => State::Code,

            // === 行注释: 遇到换行结束 ===
            (State::LineComment, '\n', _) => State::Code,
            (State::LineComment, _, _) => State::LineComment,

            // === 嵌套块注释: /* 增加深度, */ 减少深度 (Session 114) ===
            (State::BlockComment(depth), '/', Some('*')) => {
                i += 1; // 跳过 *
                State::BlockComment(depth + 1)
            }
            (State::BlockComment(depth), '*', Some('/')) => {
                i += 1; // 跳过 /
                if depth > 1 {
                    State::BlockComment(depth - 1)
                } else {
                    State::Code
                }
            }
            (State::BlockComment(d), _, _) => State::BlockComment(d),

            // === 字符串: 遇到 " 结束 (处理转义) ===
            (State::String, '\\', Some(_)) => {
                i += 1; // 跳过转义字符
                State::String
            }
            (State::String, '"', _) => State::Code,
            (State::String, _, _) => State::String,

            // === Raw 字符串: 遇到 "#...#" 结束 ===
            (State::RawString(depth), '"', _) => {
                // 检查后面是否有对应数量的 #
                let mut found = 0;
                let mut j = i + 1;
                while j < chars.len() && chars[j] == '#' && found < depth {
                    found += 1;
                    j += 1;
                }
                if found == depth {
                    i = j - 1; // 跳过所有 #
                    State::Code
                } else {
                    // " 后面没有足够的 #, 不是结束
                    State::RawString(depth)
                }
            }
            (State::RawString(d), _, _) => State::RawString(d),
        };

        i += 1;
    }

    // Session 114: 检查未终止的字符串/注释
    match state {
        State::String => {
            issues.push("字符串未终止 (缺少闭合的 \")".to_string());
        }
        State::BlockComment(depth) => {
            issues.push(format!("块注释未终止 (嵌套深度 {depth}, 缺少闭合的 */)"));
        }
        State::RawString(depth) => {
            let hash_str = "#".repeat(depth);
            issues.push(format!(
                "Raw 字符串未终止 (hash 深度 {depth}, 缺少闭合的 \"{hash_str})"
            ));
        }
        State::LineComment | State::Code => {}
    }

    // 检查括号配对
    if brace_open != brace_close {
        issues.push(format!(
            "大括号不配对 ({} 个 {{ vs {} 个 }})",
            brace_open, brace_close
        ));
    }
    if paren_open != paren_close {
        issues.push(format!(
            "圆括号不配对 ({} 个 ( vs {} 个 ))",
            paren_open, paren_close
        ));
    }
    if bracket_open != bracket_close {
        issues.push(format!(
            "方括号不配对 ({} 个 [ vs {} 个 ])",
            bracket_open, bracket_close
        ));
    }

    if issues.is_empty() {
        None
    } else {
        Some(issues.join("; "))
    }
}

/// 从文本中提取所有代码文件
///
/// 按优先级依次尝试三种提取模式:
/// 1. 标记代码块: ` ```file:path\n...``` ` 或 ` ```lang:path\n...``` `
/// 2. 普通代码块: ` ```lang\n...``` ` (无路径, 仅当模式1无结果时)
/// 3. 文件标记格式: `file:path\n...代码...` (无反引号, 仅当模式1和2均无结果时)
///
/// 提取后自动验证 TOML 格式、Rust 括号配对和 Rust 代码质量。
pub fn extract_files(text: &str) -> Vec<ExtractedFile> {
    // 清理 DeepSeek 等 UI 文本污染 (如 "复制下载" 按钮文本)
    let cleaned = clean_ui_text(text);
    // 规范化 file: 标记 (处理 path 和内容在同一行的情况)
    let normalized = normalize_file_markers(&cleaned);
    let text = normalized.as_str();

    // 按优先级尝试提取模式
    let mut files = extract_tagged_files(text);
    if files.is_empty() {
        files = extract_plain_code_blocks(text);
    }
    if files.is_empty() {
        files = extract_file_marker_format(text);
    }

    // 去重: 同一路径取最后一个
    let result = deduplicate_files(files);

    // 验证提取的文件
    validate_extracted_files(&result);

    result
}

/// 模式1: 提取标记代码块 ` ```file:path\n...``` ` 或 ` ```lang:path\n...``` `
fn extract_tagged_files(text: &str) -> Vec<ExtractedFile> {
    let re_tagged = tagged_regex();
    let mut files = Vec::new();

    for cap in re_tagged.captures_iter(text) {
        let path = cap.get(1).map(|m| m.as_str().trim()).unwrap_or("");
        let content = cap.get(2).map(|m| m.as_str()).unwrap_or("");
        let lang = guess_language_from_path(path);

        if !path.is_empty() && !content.is_empty() {
            files.push(ExtractedFile {
                path: path.to_string(),
                content: content.to_string(),
                language: lang,
            });
        }
    }

    files
}

/// 模式2: 提取普通代码块 ` ```lang\n...``` ` (无路径)
fn extract_plain_code_blocks(text: &str) -> Vec<ExtractedFile> {
    let re_plain = plain_regex();
    let mut files = Vec::new();

    for cap in re_plain.captures_iter(text) {
        let lang = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let content = cap.get(2).map(|m| m.as_str()).unwrap_or("");

        if !content.is_empty() && content.len() > 50 {
            let path = format!("snippet_{}.{}", files.len() + 1, ext_for_lang(lang));
            files.push(ExtractedFile {
                path,
                content: content.to_string(),
                language: lang.to_string(),
            });
        }
    }

    files
}

/// 模式3: 提取文件标记格式 `file:path\n...代码...` (无反引号)
fn extract_file_marker_format(text: &str) -> Vec<ExtractedFile> {
    let re_file_marker = file_marker_regex();
    let markers: Vec<(usize, String)> = re_file_marker
        .captures_iter(text)
        .map(|cap| {
            let path = cap
                .get(1)
                .map(|m| m.as_str().trim())
                .unwrap_or("")
                .to_string();
            let pos = cap.get(0).unwrap().start();
            (pos, path)
        })
        .collect();

    let mut files = Vec::new();

    for (i, (pos_ref, path)) in markers.iter().enumerate() {
        let pos = *pos_ref;
        // 代码从 file:path 行之后开始,到下一个 file: 行或文本结束
        let line_end = text[pos..]
            .find('\n')
            .map(|n| pos + n + 1)
            .unwrap_or(text.len());
        let code_start = line_end;
        let code_end = if i + 1 < markers.len() {
            markers[i + 1].0
        } else {
            text.len()
        };
        let content = if code_start < code_end {
            text[code_start..code_end].trim().to_string()
        } else {
            String::new()
        };

        if !path.is_empty() && !content.is_empty() {
            files.push(ExtractedFile {
                path: path.clone(),
                content,
                language: guess_language_from_path(path),
            });
        }
    }

    files
}

/// 去重: 同一路径取最后一个
fn deduplicate_files(files: Vec<ExtractedFile>) -> Vec<ExtractedFile> {
    let mut seen: HashMap<String, ExtractedFile> = HashMap::new();
    for f in files {
        seen.insert(f.path.clone(), f);
    }
    seen.into_values().collect()
}

/// 验证提取的文件: TOML 格式、Rust 括号配对、Rust 代码质量
fn validate_extracted_files(files: &[ExtractedFile]) {
    if files.is_empty() {
        return;
    }
    info!("提取到 {} 个文件", files.len());
    for f in files {
        debug!("  {} ({} 字符)", f.path, f.content.len());
        // 第 39 项改进: 验证 TOML 文件格式
        if f.path.ends_with(".toml") {
            if let Some(issue) = validate_toml(&f.content) {
                warn!("⚠ TOML 格式验证失败 [{}]: {}", f.path, issue);
            }
        }
        // Session 113: 验证 Rust 代码括号配对
        if f.path.ends_with(".rs") {
            if let Some(issue) = validate_rust_braces(&f.content) {
                warn!("⚠ Rust 代码括号验证失败 [{}]: {}", f.path, issue);
            }
        }
        // Session 114: 验证 Rust 代码质量
        if f.path.ends_with(".rs") {
            let quality_issues = validate_rust_code_quality(&f.content);
            if !quality_issues.is_empty() {
                warn!(
                    "⚠ Rust 代码质量警告 [{}]: {}",
                    f.path,
                    quality_issues.join("; ")
                );
            }
        }
    }
}

/// 验证 Rust 代码质量 — 检查违反约束的常见 AI 代码模式 (Session 114, Session 115 增强)
///
/// AI 生成的 Rust 代码可能包含以下违反开发约束的模式:
/// - `unwrap()` — 应使用 `?` 或 `match` 处理错误
/// - `expect()` — 同上
/// - `todo!()` — 未实现的占位代码
/// - `unimplemented!()` — 同上
/// - `panic!()` — 非测试代码中不应使用
/// - `unsafe { }` / `unsafe fn` / `unsafe impl` — 应避免 unsafe (Session 115)
/// - 公共 API 缺少 `///` 文档注释 (Session 115)
///
/// 测试代码 (`#[cfg(test)]` 模块内) 中的 `unwrap()`/`expect()` 是允许的。
///
/// 返回问题列表 (空列表表示无问题)。每个问题包含行号、描述和修复建议。
///
/// # 示例
///
/// ```
/// use forge::extract::validate_rust_code_quality;
///
/// // 无问题
/// assert!(validate_rust_code_quality("fn main() { let x = 42; }").is_empty());
///
/// // 包含 unwrap() — 应报告
/// let issues = validate_rust_code_quality("fn foo() { let x = bar().unwrap(); }");
/// assert!(!issues.is_empty());
/// assert!(issues[0].contains("unwrap"));
/// ```
pub fn validate_rust_code_quality(content: &str) -> Vec<String> {
    validate_rust_code_quality_detailed(content)
        .iter()
        .map(|issue| {
            let base = format!("行 {}: {}", issue.line, issue.message);
            if let Some(ref suggestion) = issue.suggestion {
                format!("{} | 建议: {}", base, suggestion)
            } else {
                base
            }
        })
        .collect()
}

/// 检查行中是否包含模式 (排除注释部分)
fn contains_pattern_outside_comment(line: &str, pattern: &str) -> bool {
    // 找到 // 注释的位置 (简化处理: 不考虑字符串中的 //)
    let code_part = if let Some(pos) = line.find("//") {
        &line[..pos]
    } else {
        line
    };
    code_part.contains(pattern)
}

// ============================================================================
//  Session 115: 增强代码质量验证 — unsafe 检测 + 缺失文档检测 + 修复建议
// ============================================================================

/// 代码质量问题类型 (Session 115)
///
/// 对应 `validate_rust_code_quality_detailed` 返回的 `QualityIssue` 的类型。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum IssueType {
    /// 使用 `.unwrap()` — 应使用 `?` 或 `match` 处理错误
    Unwrap,
    /// 使用 `.expect()` — 应使用 `?` 或 `match` 处理错误
    Expect,
    /// 使用 `todo!()` 宏 — 未实现的占位代码
    Todo,
    /// 使用 `unimplemented!()` 宏 — 未实现的占位代码
    Unimplemented,
    /// 使用 `panic!()` — 非测试代码不应直接 panic
    Panic,
    /// 使用 `unsafe { }` 块 — 应避免 unsafe, 寻找安全替代方案
    UnsafeBlock,
    /// 使用 `unsafe fn` — 应避免 unsafe 函数
    UnsafeFn,
    /// 使用 `unsafe impl` — 应避免 unsafe 实现
    UnsafeImpl,
    /// 公共 API 缺少 `///` 文档注释
    MissingDoc,
    /// 使用 `unreachable!()` 宏 — 不应假设代码不可达 (Session 116)
    Unreachable,
    /// 公共函数返回 `Result`/`Option`/`bool` 缺少 `#[must_use]` 属性 (Session 116)
    MissingMustUse,
}

/// 代码质量问题 — 包含行号、类型、消息和自动修复建议 (Session 115)
///
/// 由 `validate_rust_code_quality_detailed` 返回, 提供结构化的质量问题信息。
///
/// # 示例
///
/// ```
/// use forge::extract::{validate_rust_code_quality_detailed, IssueType};
///
/// let issues = validate_rust_code_quality_detailed("fn foo() { bar().unwrap(); }");
/// assert!(!issues.is_empty());
/// assert_eq!(issues[0].issue_type, IssueType::Unwrap);
/// assert!(issues[0].suggestion.is_some());
/// ```
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct QualityIssue {
    /// 行号 (1-based)
    pub line: usize,
    /// 问题类型
    pub issue_type: IssueType,
    /// 问题描述
    pub message: String,
    /// 自动修复建议 (如果有)
    pub suggestion: Option<String>,
}

/// 移除字符串字面量内容, 保留引号标记 (Session 116)
///
/// 用于避免字符串中的 `unsafe` 等关键字误报。
/// 仅处理普通双引号字符串 (含转义), 不处理 raw string (r#"..."#)。
///
/// # 示例
///
/// ```
/// use forge::extract::strip_string_content;
///
/// assert_eq!(strip_string_content("let s = \"unsafe { }\";"), "let s = \"\";");
/// assert_eq!(strip_string_content("unsafe { x }"), "unsafe { x }");
/// ```
pub fn strip_string_content(line: &str) -> String {
    let mut result = String::new();
    let mut in_string = false;
    let mut escape = false;

    for c in line.chars() {
        if escape {
            escape = false;
            continue;
        }
        if c == '\\' && in_string {
            escape = true;
            continue;
        }
        if c == '"' {
            in_string = !in_string;
            result.push('"');
            continue;
        }
        if !in_string {
            result.push(c);
        }
    }
    result
}

/// 检测行中是否包含 `unsafe` 关键字 (不是标识符的一部分) (Session 115, Session 116 增强)
///
/// 返回 unsafe 的类型: `"block"` / `"fn"` / `"impl"`, 或 `None`。
///
/// # 精确匹配
///
/// - `unsafe { ... }` → `"block"`
/// - `unsafe fn ...` → `"fn"`
/// - `unsafe impl ...` → `"impl"`
/// - `unsafe_value` → `None` (标识符的一部分)
/// - `is_unsafe = true` → `None` (标识符的一部分)
/// - `// unsafe` → `None` (注释)
/// - `"unsafe { }"` → `None` (字符串内容, Session 116)
fn detect_unsafe_keyword(line: &str) -> Option<&'static str> {
    let code_part = if let Some(pos) = line.find("//") {
        &line[..pos]
    } else {
        line
    };
    // Session 116: 移除字符串内容, 避免字符串中的 unsafe 误报
    let stripped = strip_string_content(code_part);
    let code_part = stripped.as_str();

    let mut search_start = 0;
    while let Some(pos) = code_part[search_start..].find("unsafe") {
        let abs_pos = search_start + pos;

        // 检查前一个字符是否是标识符字符
        let is_start_of_word = if abs_pos == 0 {
            true
        } else {
            let prev = code_part.as_bytes()[abs_pos - 1];
            !(prev.is_ascii_alphanumeric() || prev == b'_')
        };

        if !is_start_of_word {
            search_start = abs_pos + 6; // 6 = len("unsafe")
            continue;
        }

        // 检查 "unsafe" 后面的内容
        let after = &code_part[abs_pos + 6..]; // 6 = len("unsafe")
        let after_trimmed = after.trim_start();

        if after_trimmed.starts_with('{') {
            return Some("block");
        }

        // 检查 "fn" 是否是独立单词
        if after_trimmed.starts_with("fn ") || after_trimmed == "fn" {
            return Some("fn");
        }

        // 检查 "impl" 是否是独立单词
        if after_trimmed.starts_with("impl ") || after_trimmed == "impl" {
            return Some("impl");
        }

        search_start = abs_pos + 6;
    }

    None
}

/// 检查行是否是公共 API 声明 (需要文档注释) (Session 115)
///
/// 检测 `pub fn` / `pub struct` / `pub enum` / `pub trait` / `pub mod`
/// (包括 `pub async fn`, `pub unsafe fn`, `pub const fn` 等修饰符组合)。
///
/// 排除:
/// - `pub(crate)`, `pub(super)`, `pub(self)`, `pub(in ...)` — 非完全公共
/// - `pub use` — 重导出, 不需要文档
/// - `pub const`, `pub static`, `pub type` — 简化处理, 暂不要求
fn is_public_api_declaration(line: &str) -> bool {
    // 排除 pub(crate), pub(super), pub(self), pub(in ...)
    if line.starts_with("pub(crate)")
        || line.starts_with("pub(super)")
        || line.starts_with("pub(self)")
        || line.starts_with("pub(in ")
    {
        return false;
    }

    let Some(after_pub) = line.strip_prefix("pub ") else {
        return false;
    };
    let after_pub = after_pub.trim_start();

    // 跳过修饰符: unsafe, async, const
    let after_modifiers = skip_modifiers(after_pub);

    // pub use 不需要文档注释 (重导出)
    if after_modifiers.starts_with("use ") {
        return false;
    }

    after_modifiers.starts_with("fn ")
        || after_modifiers.starts_with("struct ")
        || after_modifiers.starts_with("enum ")
        || after_modifiers.starts_with("trait ")
        || after_modifiers.starts_with("mod ")
}

/// 跳过 Rust 修饰符 (unsafe, async, const) 并返回剩余内容
fn skip_modifiers(s: &str) -> &str {
    let mut current = s;
    loop {
        let trimmed = current.trim_start();
        if let Some(stripped) = trimmed.strip_prefix("unsafe ") {
            current = stripped;
        } else if let Some(stripped) = trimmed.strip_prefix("async ") {
            current = stripped;
        } else if let Some(stripped) = trimmed.strip_prefix("const ") {
            current = stripped;
        } else {
            return trimmed;
        }
    }
}

/// 提取公共 API 的类型描述 (Session 115)
///
/// 从 `pub fn foo()` 中提取 `"pub fn foo"`, 从 `pub struct Bar` 中提取 `"pub struct Bar"`。
fn extract_public_item_type(line: &str) -> String {
    let after_pub = line.strip_prefix("pub ").unwrap_or(line);
    let after_modifiers = skip_modifiers(after_pub.trim_start());

    if let Some(rest) = after_modifiers.strip_prefix("fn ") {
        format!("pub fn {}", extract_first_identifier(rest))
    } else if let Some(rest) = after_modifiers.strip_prefix("struct ") {
        format!("pub struct {}", extract_first_identifier(rest))
    } else if let Some(rest) = after_modifiers.strip_prefix("enum ") {
        format!("pub enum {}", extract_first_identifier(rest))
    } else if let Some(rest) = after_modifiers.strip_prefix("trait ") {
        format!("pub trait {}", extract_first_identifier(rest))
    } else if let Some(rest) = after_modifiers.strip_prefix("mod ") {
        format!("pub mod {}", extract_first_identifier(rest))
    } else {
        "pub item".to_string()
    }
}

/// 从字符串中提取第一个标识符 (字母数字+下划线序列)
fn extract_first_identifier(s: &str) -> &str {
    s.split(|c: char| !c.is_alphanumeric() && c != '_')
        .next()
        .unwrap_or("")
}

/// 检查公共 API 声明前是否有文档注释 (Session 115)
///
/// 从当前行向上查找, 跳过空行和 `#[...]` 属性, 检查是否有 `///` 或 `//!` 注释。
fn has_doc_comment(lines: &[&str], line_num: usize) -> bool {
    let mut i = line_num;
    while i > 0 {
        i -= 1;
        let prev = lines[i].trim();

        // 跳过空行
        if prev.is_empty() {
            continue;
        }

        // 跳过属性 #[...] 和 #![...]
        if prev.starts_with("#[") || prev.starts_with("#![") {
            continue;
        }

        // 检查是否是文档注释
        if prev.starts_with("///") || prev.starts_with("//!") {
            return true;
        }

        // 如果是其他代码行, 说明没有文档注释
        return false;
    }

    false
}

/// 检查函数签名是否返回需要 `#[must_use]` 的类型 (Session 116)
///
/// 检测返回 `Result`、`Option`、`bool` 等不应被忽略的类型的公共函数。
/// 仅检查同一行内包含 `->` 的函数签名。
fn returns_must_use_type(line: &str) -> bool {
    // 必须包含 fn 关键字和返回类型箭头
    if !line.contains("fn ") {
        return false;
    }
    let Some(arrow_pos) = line.find("->") else {
        return false;
    };
    let return_part = &line[arrow_pos + 2..];
    let return_type = return_part.trim();
    return_type.starts_with("Result")
        || return_type.starts_with("Option")
        || return_type.starts_with("bool")
        || return_type.starts_with("impl Iterator")
        || return_type.starts_with("impl IntoIterator")
}

/// 检查函数声明前是否有 `#[must_use]` 属性 (Session 116)
///
/// 从当前行向上查找, 跳过空行、文档注释和其他属性, 检查是否有 `#[must_use]`。
fn has_must_use_attribute(lines: &[&str], line_num: usize) -> bool {
    let mut i = line_num;
    while i > 0 {
        i -= 1;
        let prev = lines[i].trim();
        if prev.is_empty() {
            continue;
        }
        if prev.starts_with("#[must_use") {
            return true;
        }
        // 其他属性和文档注释 — 继续向上查找
        if prev.starts_with("#[") || prev.starts_with("///") || prev.starts_with("//!") {
            continue;
        }
        // 非属性、非文档行 — 停止查找
        return false;
    }
    false
}

/// 验证 Rust 代码质量 — 详细版, 返回结构化问题列表 (Session 115, Session 116 增强)
///
/// 在 `validate_rust_code_quality` 的基础上, 新增:
/// - `unsafe` 块/函数/实现检测
/// - 公共 API (`pub fn`/`pub struct`/`pub enum`/`pub trait`/`pub mod`) 缺少文档注释检测
/// - 每个问题的自动修复建议
/// - `unreachable!()` 宏检测 (Session 116)
/// - 公共函数返回 `Result`/`Option`/`bool` 缺少 `#[must_use]` 属性检测 (Session 116)
/// - `unsafe` 在字符串中的误报排除 (Session 116)
///
/// # 示例
///
/// ```
/// use forge::extract::{validate_rust_code_quality_detailed, IssueType};
///
/// // 无问题
/// assert!(validate_rust_code_quality_detailed("fn main() { let x = 42; }").is_empty());
///
/// // unsafe 块检测
/// let issues = validate_rust_code_quality_detailed("fn foo() { unsafe { let x = 42; } }");
/// assert!(issues.iter().any(|i| i.issue_type == IssueType::UnsafeBlock));
///
/// // 公共 API 缺少文档注释
/// let issues = validate_rust_code_quality_detailed("pub fn foo() {}");
/// assert!(issues.iter().any(|i| i.issue_type == IssueType::MissingDoc));
/// ```
pub fn validate_rust_code_quality_detailed(content: &str) -> Vec<QualityIssue> {
    let mut issues = Vec::new();

    let lines: Vec<&str> = content.lines().collect();
    let mut in_test_module = false;
    let mut test_module_brace_depth = 0i32;

    for (line_num, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        // 检测 #[cfg(test)] 标记
        if trimmed.contains("#[cfg(test)]") {
            in_test_module = true;
            test_module_brace_depth = 0;
        }

        // 跟踪测试模块的大括号深度
        if in_test_module {
            for c in line.chars() {
                if c == '{' {
                    test_module_brace_depth += 1;
                } else if c == '}' {
                    test_module_brace_depth -= 1;
                    if test_module_brace_depth <= 0 {
                        in_test_module = false;
                    }
                }
            }
        }

        // 跳过注释行
        if trimmed.starts_with("//") {
            continue;
        }

        // 在非测试代码中检查禁止模式
        if !in_test_module {
            // 检查 .unwrap()
            if contains_pattern_outside_comment(trimmed, ".unwrap()") {
                issues.push(QualityIssue {
                    line: line_num + 1,
                    issue_type: IssueType::Unwrap,
                    message: "使用 .unwrap() — 应使用 ? 或 match 处理错误".to_string(),
                    suggestion: Some(
                        "将 `.unwrap()` 替换为 `?` 操作符, 或使用 `match` 处理 Ok/Err 分支"
                            .to_string(),
                    ),
                });
            }

            // 检查 .expect()
            if contains_pattern_outside_comment(trimmed, ".expect(") {
                issues.push(QualityIssue {
                    line: line_num + 1,
                    issue_type: IssueType::Expect,
                    message: "使用 .expect() — 应使用 ? 或 match 处理错误".to_string(),
                    suggestion: Some(
                        "将 `.expect(\"...\")` 替换为 `?` 操作符, 或使用 `match` 处理 Ok/Err 分支"
                            .to_string(),
                    ),
                });
            }

            // 检查 todo!()
            if contains_pattern_outside_comment(trimmed, "todo!()")
                || contains_pattern_outside_comment(trimmed, "todo!(")
            {
                issues.push(QualityIssue {
                    line: line_num + 1,
                    issue_type: IssueType::Todo,
                    message: "使用 todo!() 宏 — 未实现的占位代码".to_string(),
                    suggestion: Some(
                        "实现此函数的功能, 或返回 `Err(anyhow!(\"未实现\"))`".to_string(),
                    ),
                });
            }

            // 检查 unimplemented!()
            if contains_pattern_outside_comment(trimmed, "unimplemented!()")
                || contains_pattern_outside_comment(trimmed, "unimplemented!(")
            {
                issues.push(QualityIssue {
                    line: line_num + 1,
                    issue_type: IssueType::Unimplemented,
                    message: "使用 unimplemented!() 宏 — 未实现的占位代码".to_string(),
                    suggestion: Some(
                        "实现此函数的功能, 或返回 `Err(anyhow!(\"未实现\"))`".to_string(),
                    ),
                });
            }

            // 检查 panic!()
            if (contains_pattern_outside_comment(trimmed, "panic!()")
                || contains_pattern_outside_comment(trimmed, "panic!("))
                && !trimmed.starts_with("//")
            {
                issues.push(QualityIssue {
                    line: line_num + 1,
                    issue_type: IssueType::Panic,
                    message: "使用 panic!() — 非测试代码不应直接 panic".to_string(),
                    suggestion: Some(
                        "返回 `Result` 类型: `Err(anyhow!(\"...\"))` 而非 panic".to_string(),
                    ),
                });
            }

            // Session 115: 检查 unsafe 块/函数/实现
            if let Some(unsafe_kind) = detect_unsafe_keyword(trimmed) {
                let (issue_type, message, suggestion) = match unsafe_kind {
                    "block" => (
                        IssueType::UnsafeBlock,
                        "使用 unsafe 块 — 应避免 unsafe, 寻找安全替代方案".to_string(),
                        "使用安全 API 替代; 如必须使用 unsafe, 添加 `// SAFETY: ...` 注释说明不变量"
                            .to_string(),
                    ),
                    "fn" => (
                        IssueType::UnsafeFn,
                        "使用 unsafe fn — 应避免 unsafe 函数".to_string(),
                        "使用安全 API 封装 unsafe 操作, 或添加 `// SAFETY: ...` 注释".to_string(),
                    ),
                    "impl" => (
                        IssueType::UnsafeImpl,
                        "使用 unsafe impl — 应避免 unsafe 实现".to_string(),
                        "确保实现的安全性, 添加 `// SAFETY: ...` 注释说明为何实现是安全的"
                            .to_string(),
                    ),
                    _ => unreachable!(),
                };
                issues.push(QualityIssue {
                    line: line_num + 1,
                    issue_type,
                    message,
                    suggestion: Some(suggestion),
                });
            }

            // Session 115: 检查公共 API 缺少文档注释
            if is_public_api_declaration(trimmed) && !has_doc_comment(&lines, line_num) {
                let item_desc = extract_public_item_type(trimmed);
                issues.push(QualityIssue {
                    line: line_num + 1,
                    issue_type: IssueType::MissingDoc,
                    message: format!("公共 API `{}` 缺少 `///` 文档注释", item_desc),
                    suggestion: Some(format!(
                        "在 `{}` 上方添加 `///` 文档注释, 说明其用途、参数和返回值",
                        item_desc
                    )),
                });
            }

            // Session 116: 检查 unreachable!() 宏
            if contains_pattern_outside_comment(trimmed, "unreachable!()")
                || contains_pattern_outside_comment(trimmed, "unreachable!(")
            {
                issues.push(QualityIssue {
                    line: line_num + 1,
                    issue_type: IssueType::Unreachable,
                    message: "使用 unreachable!() 宏 — 不应假设代码不可达".to_string(),
                    suggestion: Some(
                        "返回 `Result` 类型: `Err(anyhow!(\"不应到达此处\"))` 或使用 `match` 处理所有分支"
                            .to_string(),
                    ),
                });
            }

            // Session 116: 检查公共函数返回 Result/Option/bool 缺少 #[must_use]
            if is_public_api_declaration(trimmed)
                && returns_must_use_type(trimmed)
                && !has_must_use_attribute(&lines, line_num)
            {
                let item_desc = extract_public_item_type(trimmed);
                issues.push(QualityIssue {
                    line: line_num + 1,
                    issue_type: IssueType::MissingMustUse,
                    message: format!(
                        "公共函数 `{}` 返回 Result/Option/bool 但缺少 #[must_use] 属性",
                        item_desc
                    ),
                    suggestion: Some(format!(
                        "在 `{}` 上方添加 `#[must_use]` 属性, 确保调用者不会忽略返回值",
                        item_desc
                    )),
                });
            }
        }
    }

    issues
}

/// 为质量问题生成自动修复代码 (Session 116)
///
/// 接收 `QualityIssue` 和原始行内容, 返回修复后的行内容 (如果有自动修复)。
///
/// # 支持的自动修复
///
/// - `Unwrap`: `.unwrap()` → `?`
/// - `Expect`: `.expect("...")` → `?`
/// - `Todo`: `todo!()` → `Err(anyhow!("未实现"))`
/// - `Unimplemented`: `unimplemented!()` → `Err(anyhow!("未实现"))`
/// - `Panic`: `panic!("msg")` → `return Err(anyhow!("msg"))`
/// - `Unreachable`: `unreachable!()` → `return Err(anyhow!("不应到达此处"))`
/// - `UnsafeBlock`/`UnsafeFn`/`UnsafeImpl`: 添加 `// SAFETY:` 注释
/// - `MissingDoc`: 添加 `/// TODO:` 文档注释占位符
/// - `MissingMustUse`: 添加 `#[must_use]` 属性
///
/// # 示例
///
/// ```
/// use forge::extract::{generate_fix, IssueType, QualityIssue};
///
/// let issue = QualityIssue {
///     line: 1,
///     issue_type: IssueType::Unwrap,
///     message: "使用 .unwrap()".to_string(),
///     suggestion: None,
/// };
/// let original = "let x = foo().unwrap();";
/// let fixed = generate_fix(&issue, original);
/// assert!(fixed.is_some());
/// assert!(fixed.unwrap().contains('?'));
/// ```
pub fn generate_fix(issue: &QualityIssue, original_line: &str) -> Option<String> {
    match issue.issue_type {
        IssueType::Unwrap => {
            if original_line.contains(".unwrap()") {
                Some(original_line.replace(".unwrap()", "?"))
            } else {
                None
            }
        }
        IssueType::Expect => {
            if let Some(start) = original_line.find(".expect(") {
                let rest = &original_line[start..];
                if let Some(close) = rest.find(')') {
                    let before = &original_line[..start];
                    let after = &original_line[start + close + 1..];
                    Some(format!("{}?{}", before, after))
                } else {
                    None
                }
            } else {
                None
            }
        }
        IssueType::Todo => {
            if original_line.contains("todo!()") {
                Some(original_line.replace("todo!()", r#"Err(anyhow!("未实现"))"#))
            } else if original_line.contains("todo!(") {
                Some(original_line.replace("todo!(", "Err(anyhow!("))
            } else {
                None
            }
        }
        IssueType::Unimplemented => {
            if original_line.contains("unimplemented!()") {
                Some(original_line.replace("unimplemented!()", r#"Err(anyhow!("未实现"))"#))
            } else if original_line.contains("unimplemented!(") {
                Some(original_line.replace("unimplemented!(", "Err(anyhow!("))
            } else {
                None
            }
        }
        IssueType::Panic => {
            if original_line.contains("panic!()") {
                Some(original_line.replace("panic!()", r#"return Err(anyhow!("panic"))"#))
            } else if original_line.contains("panic!(") {
                Some(original_line.replace("panic!(", "return Err(anyhow!("))
            } else {
                None
            }
        }
        IssueType::Unreachable => {
            if original_line.contains("unreachable!()") {
                Some(
                    original_line
                        .replace("unreachable!()", r#"return Err(anyhow!("不应到达此处"))"#),
                )
            } else if original_line.contains("unreachable!(") {
                Some(original_line.replace("unreachable!(", "return Err(anyhow!("))
            } else {
                None
            }
        }
        IssueType::UnsafeBlock | IssueType::UnsafeFn | IssueType::UnsafeImpl => {
            let indent_len = original_line.len() - original_line.trim_start().len();
            let indent_str = &original_line[..indent_len];
            Some(format!(
                "{}// SAFETY: 需要说明为何此 unsafe 操作是安全的\n{}",
                indent_str, original_line
            ))
        }
        IssueType::MissingDoc => {
            let indent_len = original_line.len() - original_line.trim_start().len();
            let indent_str = &original_line[..indent_len];
            Some(format!(
                "{}/// TODO: 添加文档注释\n{}",
                indent_str, original_line
            ))
        }
        IssueType::MissingMustUse => {
            let indent_len = original_line.len() - original_line.trim_start().len();
            let indent_str = &original_line[..indent_len];
            Some(format!("{}#[must_use]\n{}", indent_str, original_line))
        }
    }
}

fn guess_language_from_path(path: &str) -> String {
    if path.ends_with(".rs") {
        "rust".to_string()
    } else if path.ends_with(".py") {
        "python".to_string()
    } else if path.ends_with(".toml") {
        "toml".to_string()
    } else if path.ends_with(".yaml") || path.ends_with(".yml") {
        "yaml".to_string()
    } else if path.ends_with(".json") {
        "json".to_string()
    } else if path.ends_with(".md") {
        "markdown".to_string()
    } else if path.ends_with(".sh") {
        "shell".to_string()
    } else if path.ends_with(".js") {
        "javascript".to_string()
    } else if path.ends_with(".ts") {
        "typescript".to_string()
    } else if path.ends_with(".html") {
        "html".to_string()
    } else if path.ends_with(".css") {
        "css".to_string()
    } else {
        "text".to_string()
    }
}

fn ext_for_lang(lang: &str) -> &str {
    match lang {
        "rust" => "rs",
        "python" => "py",
        "toml" => "toml",
        "yaml" => "yaml",
        "json" => "json",
        "markdown" | "md" => "md",
        "shell" | "bash" | "sh" => "sh",
        "javascript" => "js",
        "typescript" => "ts",
        "html" => "html",
        "css" => "css",
        _ => "txt",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_tagged_files() {
        let text = "Here:\n```file:src/main.rs\nfn main() {}\n```\n```file:Cargo.toml\n[package]\nname = \"test\"\n```\n";
        let files = extract_files(text);
        assert_eq!(files.len(), 2);
        let main = files.iter().find(|f| f.path == "src/main.rs").unwrap();
        assert_eq!(main.content.trim(), "fn main() {}");
        let cargo = files.iter().find(|f| f.path == "Cargo.toml").unwrap();
        assert_eq!(cargo.content.trim(), "[package]\nname = \"test\"");
    }

    #[test]
    fn test_extract_rust_tag() {
        let text = "```rust:src/lib.rs\npub fn hello() {}\n```";
        let files = extract_files(text);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/lib.rs");
        assert_eq!(files[0].content.trim(), "pub fn hello() {}");
        assert_eq!(files[0].language, "rust");
    }

    #[test]
    fn test_extract_multiple_tagged() {
        let text = "```file:src/main.rs\nfn main() {}\n```\ntext\n```file:src/lib.rs\npub fn lib() {}\n```\nmore\n```file:Cargo.toml\n[package]\nname = \"x\"\n```\n";
        assert_eq!(extract_files(text).len(), 3);
    }

    #[test]
    fn test_extract_dedup_same_path() {
        let text = "```file:src/main.rs\nversion 1\n```\n```file:src/main.rs\nversion 2\n```";
        let files = extract_files(text);
        assert_eq!(files.len(), 1, "同路径取最后一个");
        assert_eq!(files[0].content.trim(), "version 2");
    }

    #[test]
    fn test_extract_plain_code_block() {
        let text = "Code:\n```rust\nfn hello() {\n    let x = 42;\n    println!(\"hello world, value is {}\", x);\n}\n```\n";
        let files = extract_files(text);
        assert_eq!(files.len(), 1);
        assert!(files[0].path.starts_with("snippet_"));
        assert!(files[0].path.ends_with(".rs"));
    }

    #[test]
    fn test_extract_file_marker_format() {
        let text = "file:src/main.rs\nfn main() {}\nfile:Cargo.toml\n[package]\nname = \"test\"";
        let files = extract_files(text);
        assert_eq!(files.len(), 2);
        let main = files.iter().find(|f| f.path == "src/main.rs").unwrap();
        assert_eq!(main.content, "fn main() {}");
    }

    #[test]
    fn test_extract_empty_text() {
        assert!(extract_files("").is_empty());
    }

    #[test]
    fn test_extract_no_code_blocks() {
        assert!(extract_files("Just plain text.").is_empty());
    }

    #[test]
    fn test_extract_empty_file_content_skipped() {
        let text = "```file:empty.rs\n```";
        assert!(extract_files(text).is_empty(), "空内容文件应被跳过");
    }

    #[test]
    fn test_guess_language_from_path() {
        assert_eq!(guess_language_from_path("main.rs"), "rust");
        assert_eq!(guess_language_from_path("script.py"), "python");
        assert_eq!(guess_language_from_path("config.toml"), "toml");
        assert_eq!(guess_language_from_path("data.json"), "json");
        assert_eq!(guess_language_from_path("readme.md"), "markdown");
        assert_eq!(guess_language_from_path("unknown.xyz"), "text");
    }

    #[test]
    fn test_ext_for_lang() {
        assert_eq!(ext_for_lang("rust"), "rs");
        assert_eq!(ext_for_lang("python"), "py");
        assert_eq!(ext_for_lang("toml"), "toml");
        assert_eq!(ext_for_lang("unknown"), "txt");
    }

    // ===== clean_ui_text 测试 =====

    #[test]
    fn test_clean_ui_text_removes_deepseek_copy_download() {
        let text = "file:Cargo.toml复制下载[package]\nname = \"test\"";
        let cleaned = clean_ui_text(text);
        assert!(!cleaned.contains("复制下载"), "应移除 '复制下载'");
        assert!(cleaned.contains("file:Cargo.toml"), "应保留文件路径");
    }

    #[test]
    fn test_clean_ui_text_preserves_normal_text() {
        let text = "file:src/main.rs\nfn main() {}\nfile:Cargo.toml\n[package]";
        let cleaned = clean_ui_text(text);
        assert_eq!(cleaned, text, "正常文本不应被修改");
    }

    #[test]
    fn test_clean_ui_text_removes_copy_on_file_line() {
        let text = "file:src/main.rs复制\nfn main() {}";
        let cleaned = clean_ui_text(text);
        assert!(!cleaned.contains("复制"), "应移除 file: 行的 '复制'");
        assert!(cleaned.contains("file:src/main.rs"), "应保留文件路径");
    }

    #[test]
    fn test_clean_ui_text_preserves_copy_in_code() {
        // 代码内容中的 "复制" 不应被移除
        let text = "file:src/main.rs\n// 这是复制功能\nfn copy() {}";
        let cleaned = clean_ui_text(text);
        assert!(cleaned.contains("复制"), "代码内容中的 '复制' 不应被移除");
    }

    #[test]
    fn test_extract_files_with_deepseek_ui_text() {
        // 模拟 DeepSeek 回复中的 "复制下载" 污染
        let text = "file:Cargo.toml复制下载[package]\nname = \"test\"\nfile:src/main.rs复制下载fn main() {}\n";
        let files = extract_files(text);
        assert_eq!(files.len(), 2, "应提取 2 个文件");
        let cargo = files.iter().find(|f| f.path == "Cargo.toml").unwrap();
        assert_eq!(cargo.content.trim(), "[package]\nname = \"test\"");
        let main = files.iter().find(|f| f.path == "src/main.rs").unwrap();
        assert_eq!(main.content.trim(), "fn main() {}");
    }

    // ===== normalize_file_markers 测试 =====

    #[test]
    fn test_normalize_file_markers_inserts_newline() {
        // file:path 后跟多个空格和内容 — 应插入换行符
        let text = "file:Cargo.toml     [package]\nname = \"test\"";
        let normalized = normalize_file_markers(text);
        assert!(
            normalized.starts_with("file:Cargo.toml\n"),
            "应在 file:path 后插入换行符"
        );
        assert!(!normalized.contains("     "), "应移除多余空格");
    }

    #[test]
    fn test_normalize_file_markers_preserves_correct_format() {
        // file:path 后跟单个换行符 — 不应修改
        let text = "file:Cargo.toml\n[package]\nname = \"test\"";
        let normalized = normalize_file_markers(text);
        assert_eq!(normalized, text, "正确格式不应被修改");
    }

    #[test]
    fn test_normalize_file_markers_multiple_files() {
        // 多个 file: 标记在同一行 — 都应被规范化
        let text =
            "file:Cargo.toml     [package]\nname = \"test\"\nfile:src/main.rs     fn main() {}";
        let normalized = normalize_file_markers(text);
        assert!(
            normalized.contains("file:Cargo.toml\n["),
            "Cargo.toml 应有换行"
        );
        assert!(
            normalized.contains("file:src/main.rs\nfn"),
            "main.rs 应有换行"
        );
    }

    #[test]
    fn test_normalize_file_markers_single_space_not_affected() {
        // file:path 后跟单个空格 — 不应被修改 (可能是路径的一部分)
        let text = "file:src/main.rs\nfn main() {}";
        let normalized = normalize_file_markers(text);
        assert_eq!(normalized, text, "单个换行不应被修改");
    }

    #[test]
    fn test_extract_files_with_normalized_format() {
        // 模拟 DOM 提取导致 file:path 和内容在同一行
        let text =
            "file:Cargo.toml     [package]\nname = \"test\"\nfile:src/main.rs     fn main() {}";
        let files = extract_files(text);
        assert_eq!(files.len(), 2, "应提取 2 个文件");
        let cargo = files.iter().find(|f| f.path == "Cargo.toml").unwrap();
        assert!(
            cargo.content.contains("[package]"),
            "Cargo.toml 内容应包含 [package]"
        );
        let main = files.iter().find(|f| f.path == "src/main.rs").unwrap();
        assert!(
            main.content.contains("fn main"),
            "main.rs 内容应包含 fn main"
        );
    }

    // ===== validate_toml 测试 (第 39 项改进) =====

    #[test]
    fn test_validate_toml_valid() {
        let toml = r#"[package]
name = "calculator"
version = "0.1.0"
edition = "2021"

[dependencies]
"#;
        assert!(validate_toml(toml).is_none(), "有效 TOML 不应报告问题");
    }

    #[test]
    fn test_validate_toml_unclosed_table() {
        // unclosed table — 缺少 ]
        let toml = r#"[package
name = "calculator""#;
        let result = validate_toml(toml);
        assert!(result.is_some(), "未闭合的表头应报告问题");
        assert!(result.unwrap().contains("方括号"), "应报告方括号不配对");
    }

    #[test]
    fn test_validate_toml_mismatched_brackets() {
        // 方括号不配对 (多了开括号)
        let toml = r#"[package]
name = "test"

[[dependencies]
"#;
        let result = validate_toml(toml);
        assert!(result.is_some(), "方括号不配对应报告问题");
    }

    #[test]
    fn test_validate_toml_unclosed_quote() {
        // 双引号不配对
        let toml = r#"[package]
name = "unclosed
version = "0.1.0"
"#;
        let result = validate_toml(toml);
        assert!(result.is_some(), "双引号不配对应报告问题");
        assert!(result.unwrap().contains("引号"), "应报告引号问题");
    }

    #[test]
    fn test_validate_toml_missing_package() {
        // 缺少 [package] 表
        let toml = r#"[dependencies]
serde = "1.0"
"#;
        let result = validate_toml(toml);
        assert!(result.is_some(), "缺少 [package] 应报告问题");
        assert!(result.unwrap().contains("package"), "应报告缺少 package");
    }

    #[test]
    fn test_validate_toml_array_of_tables() {
        // 数组表 [[bin]] 应正确通过
        let toml = r#"[package]
name = "test"

[[bin]]
name = "main"
path = "src/main.rs"
"#;
        assert!(validate_toml(toml).is_none(), "数组表 [[bin]] 应通过验证");
    }

    #[test]
    fn test_validate_toml_with_comments() {
        // 注释行不应影响验证
        let toml = r#"[package]
# This is a comment with [ brackets ]
name = "test"
"#;
        assert!(validate_toml(toml).is_none(), "注释行不应影响验证");
    }

    #[test]
    fn test_validate_toml_empty_content() {
        assert!(validate_toml("").is_some(), "空内容应报告缺少 [package]");
    }

    // ===== validate_rust_braces 测试 (Session 113) =====

    #[test]
    fn test_validate_rust_braces_balanced() {
        let code = "fn main() { let x = vec![1, 2, 3]; }";
        assert!(
            validate_rust_braces(code).is_none(),
            "配对的代码不应报告问题"
        );
    }

    #[test]
    fn test_validate_rust_braces_missing_close_brace() {
        let code = "fn main() { let x = 42;";
        let result = validate_rust_braces(code);
        assert!(result.is_some(), "缺少闭合大括号应报告问题");
        assert!(result.unwrap().contains("大括号"), "应报告大括号不配对");
    }

    #[test]
    fn test_validate_rust_braces_extra_close_brace() {
        let code = "fn main() { let x = 42; }}";
        let result = validate_rust_braces(code);
        assert!(result.is_some(), "多余闭合大括号应报告问题");
    }

    #[test]
    fn test_validate_rust_braces_missing_paren() {
        let code = "fn main() { let x = (1 + 2; }";
        let result = validate_rust_braces(code);
        assert!(result.is_some(), "缺少闭合圆括号应报告问题");
        assert!(result.unwrap().contains("圆括号"), "应报告圆括号不配对");
    }

    #[test]
    fn test_validate_rust_braces_missing_bracket() {
        let code = "fn main() { let x = vec![1, 2, 3; }";
        let result = validate_rust_braces(code);
        assert!(result.is_some(), "缺少闭合方括号应报告问题");
        assert!(result.unwrap().contains("方括号"), "应报告方括号不配对");
    }

    #[test]
    fn test_validate_rust_braces_ignores_string_content() {
        // 字符串中的括号不应影响计数
        let code = r#"fn main() { let s = "}{)(["; }"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "字符串中的括号不应影响计数"
        );
    }

    #[test]
    fn test_validate_rust_braces_ignores_line_comment() {
        let code = "fn main() { // comment with { } ( ) [ ]\n let x = 42; }";
        assert!(
            validate_rust_braces(code).is_none(),
            "行注释中的括号不应影响计数"
        );
    }

    #[test]
    fn test_validate_rust_braces_ignores_block_comment() {
        let code = "fn main() { /* comment with { } ( ) [ ] */ let x = 42; }";
        assert!(
            validate_rust_braces(code).is_none(),
            "块注释中的括号不应影响计数"
        );
    }

    #[test]
    fn test_validate_rust_braces_ignores_raw_string() {
        let code = r##"fn main() { let s = r#"{ } ( ) [ ]"#; }"##;
        assert!(
            validate_rust_braces(code).is_none(),
            "raw string 中的括号不应影响计数"
        );
    }

    #[test]
    fn test_validate_rust_braces_ignores_char_literal() {
        let code = "fn main() { let c = '}'; let d = '{'; }";
        assert!(
            validate_rust_braces(code).is_none(),
            "字符字面量中的括号不应影响计数"
        );
    }

    #[test]
    fn test_validate_rust_braces_handles_lifetime() {
        let code = "fn foo<'a>(x: &'a str) { let y = x; }";
        assert!(
            validate_rust_braces(code).is_none(),
            "生命周期标注不应影响计数"
        );
    }

    #[test]
    fn test_validate_rust_braces_complex_code() {
        let code = r#"
use std::collections::HashMap;

fn main() {
    let mut map: HashMap<String, Vec<i32>> = HashMap::new();
    map.insert("key".to_string(), vec![1, 2, 3]);
    
    for (k, v) in &map {
        println!("{}: {:?}", k, v);
    }
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "复杂 Rust 代码应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_empty() {
        assert!(validate_rust_braces("").is_none(), "空内容不应报告问题");
    }

    #[test]
    fn test_validate_rust_braces_nested() {
        let code = "fn main() { if true { if false { } else { } } }";
        assert!(validate_rust_braces(code).is_none(), "嵌套大括号应正确配对");
    }

    #[test]
    fn test_validate_rust_braces_with_escape_in_string() {
        let code = r#"fn main() { let s = "Hello \\\"World{"; }"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "字符串中的转义字符和括号不应影响计数"
        );
    }

    #[test]
    fn test_validate_rust_braces_glm_style_truncation() {
        // 模拟 GLM 模型截断代码 (缺少最后的闭合大括号)
        let code = r#"
pub struct Config {
    pub name: String,
    pub value: i32,
}

impl Config {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            value: 0,
        }
    }
    
    pub fn validate(&self) -> bool {
        !self.name.is_empty() && self.value >= 0
    }
"#;
        let result = validate_rust_braces(code);
        assert!(result.is_some(), "GLM 截断代码应报告大括号不配对");
    }

    // ===== validate_rust_braces Session 114 增强: 嵌套块注释 =====

    #[test]
    fn test_validate_rust_braces_nested_block_comment() {
        // 嵌套块注释中的括号不应影响计数
        let code = "fn main() { /* /* { } ( ) [ ] */ */ let x = 42; }";
        assert!(
            validate_rust_braces(code).is_none(),
            "嵌套块注释中的括号不应影响计数"
        );
    }

    #[test]
    fn test_validate_rust_braces_deeply_nested_block_comment() {
        // 三层嵌套块注释
        let code = "fn main() { /* /* /* { } */ ( ) */ [ ] */ let x = 42; }";
        assert!(
            validate_rust_braces(code).is_none(),
            "三层嵌套块注释中的括号不应影响计数"
        );
    }

    #[test]
    fn test_validate_rust_braces_nested_block_comment_unterminated() {
        // 嵌套块注释未终止 (只有内层关闭了, 外层未关闭)
        let code = "fn main() { /* /* { } */ let x = 42; }";
        let result = validate_rust_braces(code);
        assert!(result.is_some(), "未终止的嵌套块注释应报告问题");
        assert!(
            result.unwrap().contains("块注释未终止"),
            "应报告块注释未终止"
        );
    }

    #[test]
    fn test_validate_rust_braces_unterminated_block_comment() {
        // 块注释未终止
        let code = "fn main() { /* comment without end { }";
        let result = validate_rust_braces(code);
        assert!(result.is_some(), "未终止的块注释应报告问题");
        assert!(
            result.unwrap().contains("块注释未终止"),
            "应报告块注释未终止"
        );
    }

    // ===== validate_rust_braces Session 114 增强: 字节字符串/字符 =====

    #[test]
    fn test_validate_rust_braces_byte_string() {
        // 字节字符串 b"..." 中的括号不应影响计数
        let code = r#"fn main() { let b: &[u8] = b"{ } ( ) [ ]"; }"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "字节字符串中的括号不应影响计数"
        );
    }

    #[test]
    fn test_validate_rust_braces_byte_char() {
        // 字节字符 b'x' 中的括号不应影响计数
        let code = "fn main() { let c = b'{'; let d = b'}'; }";
        assert!(
            validate_rust_braces(code).is_none(),
            "字节字符中的括号不应影响计数"
        );
    }

    #[test]
    fn test_validate_rust_braces_raw_byte_string() {
        // 原始字节字符串 br#"..."# 中的括号不应影响计数
        let code = r##"fn main() { let b = br#"{ } ( ) [ ]"#; }"##;
        assert!(
            validate_rust_braces(code).is_none(),
            "原始字节字符串中的括号不应影响计数"
        );
    }

    // ===== validate_rust_braces Session 114 增强: 未终止字符串检测 =====

    #[test]
    fn test_validate_rust_braces_unterminated_string() {
        // 字符串未终止
        let code = r#"fn main() { let s = "hello world; }"#;
        let result = validate_rust_braces(code);
        assert!(result.is_some(), "未终止的字符串应报告问题");
        assert!(
            result.unwrap().contains("字符串未终止"),
            "应报告字符串未终止"
        );
    }

    #[test]
    fn test_validate_rust_braces_unterminated_raw_string() {
        // Raw 字符串未终止
        let code = r##"fn main() { let s = r#"hello world; }"##;
        let result = validate_rust_braces(code);
        assert!(result.is_some(), "未终止的 raw 字符串应报告问题");
        assert!(
            result.unwrap().contains("Raw 字符串未终止"),
            "应报告 Raw 字符串未终止"
        );
    }

    #[test]
    fn test_validate_rust_braces_attributes() {
        // 属性 #[derive(Debug)] 应正确处理
        let code = r#"
#[derive(Debug, Clone)]
pub struct Foo {
    x: i32,
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_foo() {
        let f = Foo { x: 42 };
    }
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "属性和测试模块应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_closure_with_pipes() {
        // 闭包 |x| { ... } 中的管道符不应影响计数
        let code = "fn main() { let f = |x: i32| { x * 2 }; let g = |y| { y + 1 }; }";
        assert!(validate_rust_braces(code).is_none(), "闭包语法应通过验证");
    }

    #[test]
    fn test_validate_rust_braces_macro_rules() {
        // macro_rules! 宏定义
        let code = r#"
macro_rules! say_hello {
    () => {
        println!("hello");
    };
    ($name:expr) => {
        println!("hello, {}", $name);
    };
}

fn main() {
    say_hello!();
    say_hello!("world");
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "macro_rules! 宏定义应通过验证"
        );
    }

    // ===== validate_rust_code_quality 测试 (Session 114) =====

    #[test]
    fn test_validate_rust_code_quality_clean() {
        let code = "fn main() { let x = 42; println!(\"{}\", x); }";
        assert!(
            validate_rust_code_quality(code).is_empty(),
            "无禁止模式的代码不应报告问题"
        );
    }

    #[test]
    fn test_validate_rust_code_quality_unwrap() {
        let code = "fn foo() -> i32 { let x = bar().unwrap(); x }";
        let issues = validate_rust_code_quality(code);
        assert!(!issues.is_empty(), "应检测到 unwrap()");
        assert!(issues[0].contains("unwrap"), "应报告 unwrap");
    }

    #[test]
    fn test_validate_rust_code_quality_expect() {
        let code = "fn foo() -> i32 { let x = bar().expect(\"failed\"); x }";
        let issues = validate_rust_code_quality(code);
        assert!(!issues.is_empty(), "应检测到 expect()");
        assert!(issues[0].contains("expect"), "应报告 expect");
    }

    #[test]
    fn test_validate_rust_code_quality_todo() {
        let code = "fn foo() -> i32 { todo!() }";
        let issues = validate_rust_code_quality(code);
        assert!(!issues.is_empty(), "应检测到 todo!()");
        assert!(issues[0].contains("todo"), "应报告 todo!()");
    }

    #[test]
    fn test_validate_rust_code_quality_unimplemented() {
        let code = "fn foo() -> i32 { unimplemented!() }";
        let issues = validate_rust_code_quality(code);
        assert!(!issues.is_empty(), "应检测到 unimplemented!()");
        assert!(
            issues[0].contains("unimplemented"),
            "应报告 unimplemented!()"
        );
    }

    #[test]
    fn test_validate_rust_code_quality_panic() {
        let code = "fn foo() { panic!(\"something went wrong\"); }";
        let issues = validate_rust_code_quality(code);
        assert!(!issues.is_empty(), "应检测到 panic!()");
        assert!(issues[0].contains("panic"), "应报告 panic!()");
    }

    #[test]
    fn test_validate_rust_code_quality_allows_unwrap_in_test() {
        // 测试模块中的 unwrap() 不应报告
        let code = r#"
#[cfg(test)]
mod tests {
    #[test]
    fn test_foo() {
        let x = Some(42).unwrap();
        assert_eq!(x, 42);
    }
}
"#;
        let issues = validate_rust_code_quality(code);
        assert!(
            issues.is_empty(),
            "测试模块中的 unwrap() 不应报告问题, got: {:?}",
            issues
        );
    }

    #[test]
    fn test_validate_rust_code_quality_ignores_comment() {
        // 注释中的 unwrap() 不应报告
        let code = "fn foo() { // let x = bar().unwrap();\n let y = 42; }";
        let issues = validate_rust_code_quality(code);
        assert!(
            issues.is_empty(),
            "注释中的 unwrap() 不应报告问题, got: {:?}",
            issues
        );
    }

    #[test]
    fn test_validate_rust_code_quality_empty() {
        assert!(
            validate_rust_code_quality("").is_empty(),
            "空内容不应报告问题"
        );
    }

    #[test]
    fn test_validate_rust_code_quality_multiple_issues() {
        let code = r#"
fn foo() -> i32 {
    let x = bar().unwrap();
    let y = baz().expect("oops");
    panic!("done");
}
"#;
        let issues = validate_rust_code_quality(code);
        assert_eq!(issues.len(), 3, "应检测到 3 个问题, got {}", issues.len());
    }

    #[test]
    fn test_validate_rust_code_quality_mixed_test_and_non_test() {
        // 非测试代码有 unwrap, 测试代码也有 unwrap, 只报告非测试的
        let code = r#"
fn foo() -> i32 {
    bar().unwrap()
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_foo() {
        let x = Some(1).unwrap();
    }
}
"#;
        let issues = validate_rust_code_quality(code);
        assert_eq!(
            issues.len(),
            1,
            "只应报告非测试代码中的 1 个 unwrap, got: {:?}",
            issues
        );
        assert!(issues[0].contains("unwrap"));
    }

    #[test]
    fn test_validate_rust_code_quality_with_result_ok() {
        // 使用 ? 操作符的代码不应报告
        let code = r#"
fn foo() -> Result<i32, String> {
    let x = bar()?;
    Ok(x)
}
"#;
        let issues = validate_rust_code_quality(code);
        assert!(
            issues.is_empty(),
            "使用 ? 的代码不应报告问题, got: {:?}",
            issues
        );
    }

    #[test]
    fn test_validate_rust_code_quality_unwrap_with_chain() {
        // 链式调用中的 unwrap
        let code = "fn foo() { let x = vec![1, 2, 3].iter().map(|x| x * 2).collect::<Vec<_>>().first().unwrap(); }";
        let issues = validate_rust_code_quality(code);
        assert!(!issues.is_empty(), "应检测到链式调用中的 unwrap()");
    }

    // ===== contains_pattern_outside_comment 测试 =====

    #[test]
    fn test_contains_pattern_outside_comment_plain() {
        assert!(contains_pattern_outside_comment(
            "let x = foo().unwrap();",
            ".unwrap()"
        ));
    }

    #[test]
    fn test_contains_pattern_outside_comment_in_comment() {
        // 模式只在注释中
        assert!(!contains_pattern_outside_comment(
            "let x = 42; // foo().unwrap()",
            ".unwrap()"
        ));
    }

    #[test]
    fn test_contains_pattern_outside_comment_mixed() {
        // 模式在代码和注释中都有
        assert!(contains_pattern_outside_comment(
            "let x = foo().unwrap(); // bar().unwrap()",
            ".unwrap()"
        ));
    }

    #[test]
    fn test_contains_pattern_outside_comment_no_match() {
        assert!(!contains_pattern_outside_comment(
            "let x = foo()?;",
            ".unwrap()"
        ));
    }

    // ===== 重构后的提取函数测试 (Session 114) =====

    #[test]
    fn test_extract_tagged_files_directly() {
        let text = "```file:src/main.rs\nfn main() {}\n```\n```file:Cargo.toml\n[package]\nname = \"test\"\n```\n";
        let files = extract_tagged_files(text);
        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|f| f.path == "src/main.rs"));
        assert!(files.iter().any(|f| f.path == "Cargo.toml"));
    }

    #[test]
    fn test_extract_tagged_files_empty() {
        assert!(extract_tagged_files("no code blocks here").is_empty());
    }

    #[test]
    fn test_extract_plain_code_blocks_directly() {
        let text = "```rust\nfn hello() {\n    let x = 42;\n    println!(\"hello world, value is {}\", x);\n}\n```\n";
        let files = extract_plain_code_blocks(text);
        assert_eq!(files.len(), 1);
        assert!(files[0].path.starts_with("snippet_"));
        assert!(files[0].path.ends_with(".rs"));
    }

    #[test]
    fn test_extract_plain_code_blocks_short_content_skipped() {
        // 内容长度 <= 50 的代码块应被跳过
        let text = "```rust\nshort\n```";
        let files = extract_plain_code_blocks(text);
        assert!(files.is_empty(), "短内容应被跳过");
    }

    #[test]
    fn test_extract_file_marker_format_directly() {
        let text = "file:src/main.rs\nfn main() {}\nfile:Cargo.toml\n[package]\nname = \"test\"";
        let files = extract_file_marker_format(text);
        assert_eq!(files.len(), 2);
        let main = files.iter().find(|f| f.path == "src/main.rs").unwrap();
        assert_eq!(main.content, "fn main() {}");
    }

    #[test]
    fn test_extract_file_marker_format_no_markers() {
        assert!(extract_file_marker_format("just some text").is_empty());
    }

    #[test]
    fn test_deduplicate_files() {
        let files = vec![
            ExtractedFile {
                path: "src/main.rs".to_string(),
                content: "version 1".to_string(),
                language: "rust".to_string(),
            },
            ExtractedFile {
                path: "src/main.rs".to_string(),
                content: "version 2".to_string(),
                language: "rust".to_string(),
            },
            ExtractedFile {
                path: "Cargo.toml".to_string(),
                content: "[package]".to_string(),
                language: "toml".to_string(),
            },
        ];
        let result = deduplicate_files(files);
        assert_eq!(result.len(), 2, "应去重为 2 个文件");
        let main = result.iter().find(|f| f.path == "src/main.rs").unwrap();
        assert_eq!(main.content, "version 2", "应保留最后一个版本");
    }

    #[test]
    fn test_deduplicate_files_empty() {
        let result = deduplicate_files(vec![]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_deduplicate_files_no_duplicates() {
        let files = vec![
            ExtractedFile {
                path: "a.rs".to_string(),
                content: "a".to_string(),
                language: "rust".to_string(),
            },
            ExtractedFile {
                path: "b.rs".to_string(),
                content: "b".to_string(),
                language: "rust".to_string(),
            },
        ];
        let result = deduplicate_files(files);
        assert_eq!(result.len(), 2, "无重复时不应减少文件数");
    }

    #[test]
    fn test_validate_extracted_files_no_panic() {
        // 验证 validate_extracted_files 不会 panic
        let files = vec![
            ExtractedFile {
                path: "src/main.rs".to_string(),
                content: "fn main() { let x = 42; }".to_string(),
                language: "rust".to_string(),
            },
            ExtractedFile {
                path: "Cargo.toml".to_string(),
                content: "[package]\nname = \"test\"\nversion = \"0.1.0\"\n".to_string(),
                language: "toml".to_string(),
            },
        ];
        // 不应 panic
        validate_extracted_files(&files);
    }

    #[test]
    fn test_validate_extracted_files_empty() {
        // 空列表不应 panic
        validate_extracted_files(&[]);
    }

    #[test]
    fn test_validate_extracted_files_with_quality_issues() {
        // 包含 unwrap() 的 Rust 文件应被质量检查检测到
        let files = vec![ExtractedFile {
            path: "src/main.rs".to_string(),
            content: "fn foo() { let x = bar().unwrap(); }".to_string(),
            language: "rust".to_string(),
        }];
        // 不应 panic — 质量检查仅输出 warn 日志
        validate_extracted_files(&files);
    }

    // ===== Session 115: detect_unsafe_keyword 测试 =====

    #[test]
    fn test_detect_unsafe_keyword_block() {
        assert_eq!(
            detect_unsafe_keyword("unsafe { let x = 42; }"),
            Some("block")
        );
        assert_eq!(detect_unsafe_keyword("    unsafe {"), Some("block"));
    }

    #[test]
    fn test_detect_unsafe_keyword_fn() {
        assert_eq!(
            detect_unsafe_keyword("unsafe fn foo() -> i32 { 42 }"),
            Some("fn")
        );
        assert_eq!(detect_unsafe_keyword("pub unsafe fn bar() {}"), Some("fn"));
    }

    #[test]
    fn test_detect_unsafe_keyword_impl() {
        assert_eq!(
            detect_unsafe_keyword("unsafe impl Foo for Bar { }"),
            Some("impl")
        );
        assert_eq!(
            detect_unsafe_keyword("unsafe impl Send for *const () {}"),
            Some("impl")
        );
    }

    #[test]
    fn test_detect_unsafe_keyword_identifier_not_matched() {
        // unsafe 作为标识符的一部分不应匹配
        assert_eq!(detect_unsafe_keyword("let unsafe_value = 42;"), None);
        assert_eq!(detect_unsafe_keyword("let is_unsafe = true;"), None);
        assert_eq!(detect_unsafe_keyword("fn unsafe_handler() {}"), None);
    }

    #[test]
    fn test_detect_unsafe_keyword_in_comment_not_matched() {
        // 注释中的 unsafe 不应匹配
        assert_eq!(detect_unsafe_keyword("// unsafe { } block"), None);
        assert_eq!(
            detect_unsafe_keyword("let x = 42; // unsafe fn not_real"),
            None
        );
    }

    #[test]
    fn test_detect_unsafe_keyword_none() {
        // 无 unsafe 的代码
        assert_eq!(detect_unsafe_keyword("fn main() { let x = 42; }"), None);
        assert_eq!(detect_unsafe_keyword(""), None);
    }

    // ===== Session 115: is_public_api_declaration 测试 =====

    #[test]
    fn test_is_public_api_declaration_fn() {
        assert!(is_public_api_declaration("pub fn foo() {}"));
        assert!(is_public_api_declaration("pub async fn foo() {}"));
        assert!(is_public_api_declaration("pub unsafe fn foo() {}"));
        assert!(is_public_api_declaration("pub const fn foo() {}"));
    }

    #[test]
    fn test_is_public_api_declaration_struct() {
        assert!(is_public_api_declaration("pub struct Foo {}"));
        assert!(is_public_api_declaration("pub struct Foo;"));
    }

    #[test]
    fn test_is_public_api_declaration_enum() {
        assert!(is_public_api_declaration(
            "pub enum Color { Red, Green, Blue }"
        ));
    }

    #[test]
    fn test_is_public_api_declaration_trait() {
        assert!(is_public_api_declaration(
            "pub trait Foo { fn bar(&self); }"
        ));
    }

    #[test]
    fn test_is_public_api_declaration_mod() {
        assert!(is_public_api_declaration("pub mod utils;"));
    }

    #[test]
    fn test_is_public_api_declaration_not_public() {
        // 非 pub 的不应匹配
        assert!(!is_public_api_declaration("fn foo() {}"));
        assert!(!is_public_api_declaration("struct Foo {}"));
        assert!(!is_public_api_declaration("enum Color {}"));
    }

    #[test]
    fn test_is_public_api_declaration_pub_crate() {
        // pub(crate) 不应匹配
        assert!(!is_public_api_declaration("pub(crate) fn foo() {}"));
        assert!(!is_public_api_declaration("pub(crate) struct Foo {}"));
        assert!(!is_public_api_declaration("pub(super) fn foo() {}"));
    }

    #[test]
    fn test_is_public_api_declaration_pub_use() {
        // pub use 不应匹配 (重导出)
        assert!(!is_public_api_declaration(
            "pub use std::collections::HashMap;"
        ));
        assert!(!is_public_api_declaration("pub use crate::foo;"));
    }

    // ===== Session 115: skip_modifiers 测试 =====

    #[test]
    fn test_skip_modifiers_no_modifiers() {
        assert_eq!(skip_modifiers("fn foo()"), "fn foo()");
    }

    #[test]
    fn test_skip_modifiers_unsafe() {
        assert_eq!(skip_modifiers("unsafe fn foo()"), "fn foo()");
    }

    #[test]
    fn test_skip_modifiers_async() {
        assert_eq!(skip_modifiers("async fn foo()"), "fn foo()");
    }

    #[test]
    fn test_skip_modifiers_const() {
        assert_eq!(skip_modifiers("const fn foo()"), "fn foo()");
    }

    #[test]
    fn test_skip_modifiers_multiple() {
        assert_eq!(skip_modifiers("unsafe async fn foo()"), "fn foo()");
        assert_eq!(skip_modifiers("async unsafe fn foo()"), "fn foo()");
        assert_eq!(skip_modifiers("unsafe const fn foo()"), "fn foo()");
    }

    // ===== Session 115: extract_public_item_type 测试 =====

    #[test]
    fn test_extract_public_item_type_fn() {
        assert_eq!(extract_public_item_type("pub fn foo() {}"), "pub fn foo");
        assert_eq!(
            extract_public_item_type("pub async fn bar() -> i32 {}"),
            "pub fn bar"
        );
    }

    #[test]
    fn test_extract_public_item_type_struct() {
        assert_eq!(
            extract_public_item_type("pub struct Foo {}"),
            "pub struct Foo"
        );
    }

    #[test]
    fn test_extract_public_item_type_enum() {
        assert_eq!(
            extract_public_item_type("pub enum Color { Red }"),
            "pub enum Color"
        );
    }

    #[test]
    fn test_extract_public_item_type_trait() {
        assert_eq!(
            extract_public_item_type("pub trait Foo { fn bar(&self); }"),
            "pub trait Foo"
        );
    }

    #[test]
    fn test_extract_public_item_type_with_modifiers() {
        assert_eq!(
            extract_public_item_type("pub unsafe fn dangerous() {}"),
            "pub fn dangerous"
        );
        assert_eq!(
            extract_public_item_type("pub async fn async_fn() {}"),
            "pub fn async_fn"
        );
    }

    // ===== Session 115: has_doc_comment 测试 =====

    #[test]
    fn test_has_doc_comment_present() {
        let lines = vec!["/// This is a doc comment", "pub fn foo() {}"];
        assert!(has_doc_comment(&lines, 1));
    }

    #[test]
    fn test_has_doc_comment_absent() {
        let lines = vec!["let x = 42;", "pub fn foo() {}"];
        assert!(!has_doc_comment(&lines, 1));
    }

    #[test]
    fn test_has_doc_comment_with_attribute() {
        // 属性 #[...] 应被跳过, 继续向上查找文档注释
        let lines = vec![
            "/// This is a doc comment",
            "#[derive(Debug)]",
            "pub struct Foo {}",
        ];
        assert!(has_doc_comment(&lines, 2));
    }

    #[test]
    fn test_has_doc_comment_with_blank_lines() {
        // 空行应被跳过
        let lines = vec!["/// Doc comment", "", "", "pub fn foo() {}"];
        assert!(has_doc_comment(&lines, 3));
    }

    #[test]
    fn test_has_doc_comment_inner_doc() {
        // //! 内部文档注释也应被识别
        let lines = vec!["//! Module docs", "pub fn foo() {}"];
        assert!(has_doc_comment(&lines, 1));
    }

    #[test]
    fn test_has_doc_comment_first_line() {
        // 第一行就是 pub 声明 — 没有前一行
        let lines = vec!["pub fn foo() {}"];
        assert!(!has_doc_comment(&lines, 0));
    }

    // ===== Session 115: validate_rust_code_quality_detailed 测试 =====

    #[test]
    fn test_validate_detailed_clean() {
        let issues =
            validate_rust_code_quality_detailed("fn main() { let x = 42; println!(\"{}\", x); }");
        assert!(issues.is_empty(), "无禁止模式的代码不应报告问题");
    }

    #[test]
    fn test_validate_detailed_unwrap() {
        let issues = validate_rust_code_quality_detailed("fn foo() { let x = bar().unwrap(); }");
        assert!(!issues.is_empty());
        assert_eq!(issues[0].issue_type, IssueType::Unwrap);
        assert!(issues[0].suggestion.is_some());
        assert!(issues[0].suggestion.as_ref().unwrap().contains("?"));
    }

    #[test]
    fn test_validate_detailed_unsafe_block() {
        let issues = validate_rust_code_quality_detailed("fn foo() { unsafe { let x = 42; } }");
        assert!(!issues.is_empty());
        assert_eq!(issues[0].issue_type, IssueType::UnsafeBlock);
        assert!(issues[0].suggestion.as_ref().unwrap().contains("SAFETY"));
    }

    #[test]
    fn test_validate_detailed_unsafe_fn() {
        let issues = validate_rust_code_quality_detailed("unsafe fn foo() -> i32 { 42 }");
        assert!(!issues.is_empty());
        assert_eq!(issues[0].issue_type, IssueType::UnsafeFn);
    }

    #[test]
    fn test_validate_detailed_unsafe_impl() {
        let issues = validate_rust_code_quality_detailed("unsafe impl Send for MyType {}");
        assert!(!issues.is_empty());
        assert_eq!(issues[0].issue_type, IssueType::UnsafeImpl);
    }

    #[test]
    fn test_validate_detailed_missing_doc_pub_fn() {
        let issues = validate_rust_code_quality_detailed("pub fn foo() {}");
        assert!(!issues.is_empty());
        assert_eq!(issues[0].issue_type, IssueType::MissingDoc);
        assert!(issues[0].message.contains("pub fn foo"));
    }

    #[test]
    fn test_validate_detailed_missing_doc_pub_struct() {
        let issues = validate_rust_code_quality_detailed("pub struct Foo { x: i32 }");
        assert!(!issues.is_empty());
        assert_eq!(issues[0].issue_type, IssueType::MissingDoc);
        assert!(issues[0].message.contains("pub struct Foo"));
    }

    #[test]
    fn test_validate_detailed_has_doc_no_issue() {
        // 有文档注释的公共 API 不应报告 MissingDoc
        let code = "/// This is foo.\npub fn foo() {}";
        let issues = validate_rust_code_quality_detailed(code);
        let missing_doc_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.issue_type == IssueType::MissingDoc)
            .collect();
        assert!(
            missing_doc_issues.is_empty(),
            "有文档注释不应报告 MissingDoc"
        );
    }

    #[test]
    fn test_validate_detailed_doc_with_attribute() {
        // 文档注释和声明之间有属性 — 不应报告 MissingDoc
        let code = "/// This is foo.\n#[derive(Debug)]\npub fn foo() {}";
        let issues = validate_rust_code_quality_detailed(code);
        let missing_doc_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.issue_type == IssueType::MissingDoc)
            .collect();
        assert!(missing_doc_issues.is_empty());
    }

    #[test]
    fn test_validate_detailed_pub_crate_no_missing_doc() {
        // pub(crate) 不应报告 MissingDoc
        let issues = validate_rust_code_quality_detailed("pub(crate) fn foo() {}");
        let missing_doc_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.issue_type == IssueType::MissingDoc)
            .collect();
        assert!(missing_doc_issues.is_empty());
    }

    #[test]
    fn test_validate_detailed_pub_use_no_missing_doc() {
        // pub use 不应报告 MissingDoc
        let issues = validate_rust_code_quality_detailed("pub use std::collections::HashMap;");
        let missing_doc_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.issue_type == IssueType::MissingDoc)
            .collect();
        assert!(missing_doc_issues.is_empty());
    }

    #[test]
    fn test_validate_detailed_allows_unsafe_in_test() {
        // 测试模块中的 unsafe 不应报告
        let code = r#"
#[cfg(test)]
mod tests {
    #[test]
    fn test_unsafe() {
        unsafe { let x = 42; }
    }
}
"#;
        let issues = validate_rust_code_quality_detailed(code);
        let unsafe_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.issue_type == IssueType::UnsafeBlock)
            .collect();
        assert!(unsafe_issues.is_empty(), "测试模块中的 unsafe 不应报告");
    }

    #[test]
    fn test_validate_detailed_allows_missing_doc_in_test() {
        // 测试模块中的 pub fn 不应报告 MissingDoc
        let code = r#"
#[cfg(test)]
mod tests {
    pub fn helper() {}
}
"#;
        let issues = validate_rust_code_quality_detailed(code);
        let missing_doc_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.issue_type == IssueType::MissingDoc)
            .collect();
        assert!(missing_doc_issues.is_empty());
    }

    #[test]
    fn test_validate_detailed_multiple_issue_types() {
        // 同时包含 unwrap + unsafe + missing doc
        let code = r#"
pub fn foo() {
    let x = bar().unwrap();
    unsafe { let y = x; }
}
"#;
        let issues = validate_rust_code_quality_detailed(code);
        // 应有 3 个问题: MissingDoc (pub fn foo) + Unwrap + UnsafeBlock
        assert_eq!(issues.len(), 3, "应有 3 个问题, got: {:?}", issues);
        assert!(issues.iter().any(|i| i.issue_type == IssueType::MissingDoc));
        assert!(issues.iter().any(|i| i.issue_type == IssueType::Unwrap));
        assert!(issues
            .iter()
            .any(|i| i.issue_type == IssueType::UnsafeBlock));
    }

    #[test]
    fn test_validate_detailed_all_have_suggestions() {
        // 所有问题都应有建议
        let code = r#"
pub fn foo() {
    bar().unwrap();
    panic!("oops");
    unsafe { }
}
"#;
        let issues = validate_rust_code_quality_detailed(code);
        assert!(!issues.is_empty());
        for issue in &issues {
            assert!(
                issue.suggestion.is_some(),
                "问题 {:?} 应有修复建议",
                issue.issue_type
            );
        }
    }

    #[test]
    fn test_validate_quality_backward_compat() {
        // 验证 validate_rust_code_quality (字符串版) 与 detailed 版的一致性
        let code = "fn foo() { bar().unwrap(); }";
        let str_issues = validate_rust_code_quality(code);
        let detailed_issues = validate_rust_code_quality_detailed(code);
        assert_eq!(str_issues.len(), detailed_issues.len());
        // 字符串版应包含行号和消息
        assert!(str_issues[0].contains("行 1"));
        assert!(str_issues[0].contains("unwrap"));
        // 字符串版应包含建议
        assert!(str_issues[0].contains("建议"));
    }

    // ===== Session 115: validate_rust_braces const generics 测试 =====

    #[test]
    fn test_validate_rust_braces_const_generic_struct() {
        let code = "pub struct Foo<const N: usize> { data: [i32; N] }";
        assert!(
            validate_rust_braces(code).is_none(),
            "const generic 结构体应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_const_generic_fn() {
        let code = "fn foo<const N: usize>() { let arr = [0; N]; }";
        assert!(
            validate_rust_braces(code).is_none(),
            "const generic 函数应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_const_generic_impl() {
        let code = "impl<const N: usize> Foo<N> for Bar { fn baz(&self) {} }";
        assert!(
            validate_rust_braces(code).is_none(),
            "const generic impl 应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_const_generic_trait() {
        let code = r#"
trait Foo: Sized {
    fn bar<const N: usize>(&self) -> [i32; N];
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "const generic trait 方法应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_const_eval_block() {
        // const 表达式中的 {} 块
        let code = "const X: usize = { let x = 42; x + 1 };";
        assert!(
            validate_rust_braces(code).is_none(),
            "const 表达式中的块应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_attribute_with_string_brackets() {
        // 属性中包含 {} 的字符串
        let code = r#"
#[doc = "this { has } brackets"]
pub fn foo() {}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "属性字符串中的括号不应影响计数"
        );
    }

    #[test]
    fn test_validate_rust_braces_where_clause() {
        let code = r#"
fn foo<T>(x: T) -> i32 where T: Sized {
    42
}
"#;
        assert!(validate_rust_braces(code).is_none(), "where 子句应通过验证");
    }

    #[test]
    fn test_validate_rust_braces_nested_generics() {
        let code = "fn foo() { let x: Vec<HashMap<String, Vec<i32>>> = Vec::new(); }";
        assert!(
            validate_rust_braces(code).is_none(),
            "嵌套泛型应通过验证 (>> 不影响计数)"
        );
    }

    // ===== Session 116: strip_string_content 测试 =====

    #[test]
    fn test_strip_string_content_basic() {
        assert_eq!(
            strip_string_content(r#"let s = "unsafe { }";"#),
            r#"let s = "";"#
        );
        assert_eq!(
            strip_string_content(r#"let s = "hello world";"#),
            r#"let s = "";"#
        );
    }

    #[test]
    fn test_strip_string_content_with_escape() {
        // 转义引号不影响字符串结束判断
        assert_eq!(
            strip_string_content(r#"let s = "he said \"hello\"";"#),
            r#"let s = "";"#
        );
    }

    #[test]
    fn test_strip_string_content_no_strings() {
        assert_eq!(
            strip_string_content("fn foo() { let x = 42; }"),
            "fn foo() { let x = 42; }"
        );
    }

    #[test]
    fn test_strip_string_content_multiple_strings() {
        assert_eq!(
            strip_string_content(r#"let a = "first"; let b = "second";"#),
            r#"let a = ""; let b = "";"#
        );
    }

    #[test]
    fn test_strip_string_content_unterminated() {
        // 未终止字符串 — 全部当作字符串内容
        let result = strip_string_content(r#"let s = "unterminated"#);
        assert!(result.starts_with("let s = \""));
    }

    // ===== Session 116: detect_unsafe_keyword 字符串感知测试 =====

    #[test]
    fn test_detect_unsafe_keyword_in_string_no_false_positive() {
        // 字符串中的 unsafe 不应匹配
        assert_eq!(
            detect_unsafe_keyword(r#"let s = "unsafe { } block";"#),
            None
        );
        assert_eq!(
            detect_unsafe_keyword(r#"let msg = "unsafe fn not_real";"#),
            None
        );
        assert_eq!(
            detect_unsafe_keyword(r#"let desc = "unsafe impl Send";"#),
            None
        );
    }

    #[test]
    fn test_detect_unsafe_keyword_real_unsafe_still_detected() {
        // 真正的 unsafe 仍应被检测到
        assert_eq!(
            detect_unsafe_keyword("unsafe { let x = 42; }"),
            Some("block")
        );
        assert_eq!(
            detect_unsafe_keyword("unsafe fn foo() -> i32 { 42 }"),
            Some("fn")
        );
    }

    #[test]
    fn test_detect_unsafe_keyword_mixed_string_and_code() {
        // 同一行有字符串和真正的 unsafe
        assert_eq!(
            detect_unsafe_keyword(r#"let s = "safe"; unsafe { let x = 42; }"#),
            Some("block")
        );
    }

    // ===== Session 116: returns_must_use_type 测试 =====

    #[test]
    fn test_returns_must_use_type_result() {
        assert!(returns_must_use_type(
            "pub fn foo() -> Result<i32, String> {"
        ));
        assert!(returns_must_use_type(
            "pub async fn bar() -> Result<(), Error> {"
        ));
    }

    #[test]
    fn test_returns_must_use_type_option() {
        assert!(returns_must_use_type("pub fn foo() -> Option<i32> {"));
        assert!(returns_must_use_type("fn bar() -> Option<String> {"));
    }

    #[test]
    fn test_returns_must_use_type_bool() {
        assert!(returns_must_use_type("pub fn is_valid() -> bool {"));
    }

    #[test]
    fn test_returns_must_use_type_not_matching() {
        assert!(!returns_must_use_type("pub fn foo() -> i32 {"));
        assert!(!returns_must_use_type("pub fn foo() -> String {"));
        assert!(!returns_must_use_type("pub fn foo() {}"));
        assert!(!returns_must_use_type("pub struct Foo {"));
    }

    #[test]
    fn test_returns_must_use_type_impl_iterator() {
        assert!(returns_must_use_type(
            "pub fn iter() -> impl Iterator<Item = i32> {"
        ));
    }

    // ===== Session 116: has_must_use_attribute 测试 =====

    #[test]
    fn test_has_must_use_attribute_present() {
        let lines = vec!["#[must_use]", "pub fn foo() -> bool { true }"];
        assert!(has_must_use_attribute(&lines, 1));
    }

    #[test]
    fn test_has_must_use_attribute_absent() {
        let lines = vec!["/// doc comment", "pub fn foo() -> bool { true }"];
        assert!(!has_must_use_attribute(&lines, 1));
    }

    #[test]
    fn test_has_must_use_attribute_with_other_attributes() {
        // #[must_use] 在其他属性之间
        let lines = vec![
            "/// doc comment",
            "#[derive(Debug)]",
            "#[must_use]",
            "pub fn foo() -> bool { true }",
        ];
        assert!(has_must_use_attribute(&lines, 3));
    }

    #[test]
    fn test_has_must_use_attribute_with_blank_lines() {
        let lines = vec!["#[must_use]", "", "pub fn foo() -> bool { true }"];
        assert!(has_must_use_attribute(&lines, 2));
    }

    #[test]
    fn test_has_must_use_attribute_first_line() {
        let lines = vec!["pub fn foo() -> bool { true }"];
        assert!(!has_must_use_attribute(&lines, 0));
    }

    // ===== Session 116: validate_rust_code_quality_detailed 新增检测测试 =====

    #[test]
    fn test_validate_detailed_unreachable() {
        let issues = validate_rust_code_quality_detailed("fn foo() -> i32 { unreachable!() }");
        assert!(!issues.is_empty());
        assert_eq!(issues[0].issue_type, IssueType::Unreachable);
        assert!(issues[0].suggestion.is_some());
    }

    #[test]
    fn test_validate_detailed_unreachable_with_message() {
        let issues =
            validate_rust_code_quality_detailed(r#"fn foo() { unreachable!("not here"); }"#);
        assert!(!issues.is_empty());
        assert_eq!(issues[0].issue_type, IssueType::Unreachable);
    }

    #[test]
    fn test_validate_detailed_missing_must_use_result() {
        let code = "/// Doc.\npub fn foo() -> Result<i32, String> { Ok(42) }";
        let issues = validate_rust_code_quality_detailed(code);
        assert!(
            issues
                .iter()
                .any(|i| i.issue_type == IssueType::MissingMustUse),
            "应检测到缺少 #[must_use], got: {:?}",
            issues
        );
    }

    #[test]
    fn test_validate_detailed_missing_must_use_option() {
        let code = "/// Doc.\npub fn find() -> Option<i32> { Some(42) }";
        let issues = validate_rust_code_quality_detailed(code);
        assert!(
            issues
                .iter()
                .any(|i| i.issue_type == IssueType::MissingMustUse),
            "应检测到缺少 #[must_use]"
        );
    }

    #[test]
    fn test_validate_detailed_missing_must_use_bool() {
        let code = "/// Doc.\npub fn is_valid() -> bool { true }";
        let issues = validate_rust_code_quality_detailed(code);
        assert!(
            issues
                .iter()
                .any(|i| i.issue_type == IssueType::MissingMustUse),
            "应检测到缺少 #[must_use]"
        );
    }

    #[test]
    fn test_validate_detailed_has_must_use_no_issue() {
        let code = "/// Doc.\n#[must_use]\npub fn foo() -> Result<i32, String> { Ok(42) }";
        let issues = validate_rust_code_quality_detailed(code);
        let must_use_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.issue_type == IssueType::MissingMustUse)
            .collect();
        assert!(
            must_use_issues.is_empty(),
            "有 #[must_use] 不应报告 MissingMustUse"
        );
    }

    #[test]
    fn test_validate_detailed_must_use_not_required_for_int() {
        // 返回 i32 的函数不需要 #[must_use]
        let code = "/// Doc.\npub fn foo() -> i32 { 42 }";
        let issues = validate_rust_code_quality_detailed(code);
        let must_use_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.issue_type == IssueType::MissingMustUse)
            .collect();
        assert!(must_use_issues.is_empty());
    }

    #[test]
    fn test_validate_detailed_unsafe_in_string_no_false_positive() {
        // 字符串中的 unsafe 不应误报
        let code = r#"fn foo() { let s = "unsafe { } block"; }"#;
        let issues = validate_rust_code_quality_detailed(code);
        let unsafe_issues: Vec<_> = issues
            .iter()
            .filter(|i| {
                i.issue_type == IssueType::UnsafeBlock
                    || i.issue_type == IssueType::UnsafeFn
                    || i.issue_type == IssueType::UnsafeImpl
            })
            .collect();
        assert!(
            unsafe_issues.is_empty(),
            "字符串中的 unsafe 不应误报, got: {:?}",
            unsafe_issues
        );
    }

    #[test]
    fn test_validate_detailed_allows_unreachable_in_test() {
        // 测试模块中的 unreachable!() 不应报告
        let code = r#"
#[cfg(test)]
mod tests {
    #[test]
    fn test_foo() {
        unreachable!()
    }
}
"#;
        let issues = validate_rust_code_quality_detailed(code);
        let unreachable_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.issue_type == IssueType::Unreachable)
            .collect();
        assert!(unreachable_issues.is_empty());
    }

    // ===== Session 116: generate_fix 测试 =====

    #[test]
    fn test_generate_fix_unwrap() {
        let issue = QualityIssue {
            line: 1,
            issue_type: IssueType::Unwrap,
            message: "使用 .unwrap()".to_string(),
            suggestion: None,
        };
        let original = "let x = foo().unwrap();";
        let fixed = generate_fix(&issue, original);
        assert!(fixed.is_some());
        assert!(fixed.as_ref().unwrap().contains('?'));
        assert!(!fixed.as_ref().unwrap().contains(".unwrap()"));
    }

    #[test]
    fn test_generate_fix_expect() {
        let issue = QualityIssue {
            line: 1,
            issue_type: IssueType::Expect,
            message: "使用 .expect()".to_string(),
            suggestion: None,
        };
        let original = r#"let x = foo().expect("msg");"#;
        let fixed = generate_fix(&issue, original);
        assert!(fixed.is_some());
        assert!(fixed.as_ref().unwrap().contains('?'));
        assert!(!fixed.as_ref().unwrap().contains(".expect("));
    }

    #[test]
    fn test_generate_fix_todo() {
        let issue = QualityIssue {
            line: 1,
            issue_type: IssueType::Todo,
            message: "使用 todo!()".to_string(),
            suggestion: None,
        };
        let original = "fn foo() -> i32 { todo!() }";
        let fixed = generate_fix(&issue, original);
        assert!(fixed.is_some());
        assert!(fixed.as_ref().unwrap().contains("Err(anyhow!"));
        assert!(!fixed.as_ref().unwrap().contains("todo!"));
    }

    #[test]
    fn test_generate_fix_unimplemented() {
        let issue = QualityIssue {
            line: 1,
            issue_type: IssueType::Unimplemented,
            message: "使用 unimplemented!()".to_string(),
            suggestion: None,
        };
        let original = "fn foo() -> i32 { unimplemented!() }";
        let fixed = generate_fix(&issue, original);
        assert!(fixed.is_some());
        assert!(fixed.as_ref().unwrap().contains("Err(anyhow!"));
    }

    #[test]
    fn test_generate_fix_panic() {
        let issue = QualityIssue {
            line: 1,
            issue_type: IssueType::Panic,
            message: "使用 panic!()".to_string(),
            suggestion: None,
        };
        let original = r#"fn foo() { panic!("oops"); }"#;
        let fixed = generate_fix(&issue, original);
        assert!(fixed.is_some());
        assert!(fixed.as_ref().unwrap().contains("return Err(anyhow!"));
        assert!(!fixed.as_ref().unwrap().contains("panic!("));
    }

    #[test]
    fn test_generate_fix_unreachable() {
        let issue = QualityIssue {
            line: 1,
            issue_type: IssueType::Unreachable,
            message: "使用 unreachable!()".to_string(),
            suggestion: None,
        };
        let original = "fn foo() -> i32 { unreachable!() }";
        let fixed = generate_fix(&issue, original);
        assert!(fixed.is_some());
        assert!(fixed.as_ref().unwrap().contains("return Err(anyhow!"));
        assert!(!fixed.as_ref().unwrap().contains("unreachable!"));
    }

    #[test]
    fn test_generate_fix_unsafe_block() {
        let issue = QualityIssue {
            line: 1,
            issue_type: IssueType::UnsafeBlock,
            message: "使用 unsafe 块".to_string(),
            suggestion: None,
        };
        let original = "    unsafe { let x = 42; }";
        let fixed = generate_fix(&issue, original);
        assert!(fixed.is_some());
        assert!(fixed.as_ref().unwrap().contains("SAFETY:"));
        assert!(fixed.as_ref().unwrap().contains("unsafe {"));
    }

    #[test]
    fn test_generate_fix_missing_doc() {
        let issue = QualityIssue {
            line: 1,
            issue_type: IssueType::MissingDoc,
            message: "缺少文档注释".to_string(),
            suggestion: None,
        };
        let original = "pub fn foo() {}";
        let fixed = generate_fix(&issue, original);
        assert!(fixed.is_some());
        assert!(fixed.as_ref().unwrap().contains("/// TODO:"));
        assert!(fixed.as_ref().unwrap().contains("pub fn foo()"));
    }

    #[test]
    fn test_generate_fix_missing_must_use() {
        let issue = QualityIssue {
            line: 1,
            issue_type: IssueType::MissingMustUse,
            message: "缺少 #[must_use]".to_string(),
            suggestion: None,
        };
        let original = "pub fn foo() -> bool { true }";
        let fixed = generate_fix(&issue, original);
        assert!(fixed.is_some());
        assert!(fixed.as_ref().unwrap().contains("#[must_use]"));
        assert!(fixed.as_ref().unwrap().contains("pub fn foo()"));
    }

    #[test]
    fn test_generate_fix_preserves_indentation() {
        // 验证缩进保留
        let issue = QualityIssue {
            line: 1,
            issue_type: IssueType::MissingDoc,
            message: "缺少文档注释".to_string(),
            suggestion: None,
        };
        let original = "    pub fn foo() {}";
        let fixed = generate_fix(&issue, original).unwrap();
        assert!(fixed.starts_with("    /// TODO:"));
    }

    #[test]
    fn test_generate_fix_expect_no_match() {
        // 行中不包含 .expect( 时返回 None
        let issue = QualityIssue {
            line: 1,
            issue_type: IssueType::Expect,
            message: "使用 .expect()".to_string(),
            suggestion: None,
        };
        let original = "let x = 42;";
        let fixed = generate_fix(&issue, original);
        assert!(fixed.is_none());
    }

    // ===== Session 116: validate_rust_braces let else / let chains 测试 =====

    #[test]
    fn test_validate_rust_braces_let_else() {
        // let-else 语法 (Rust 1.65+)
        let code = r#"
fn foo(x: Option<i32>) -> i32 {
    let Some(v) = x else {
        return 0;
    };
    v + 1
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "let-else 语法应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_let_else_complex() {
        // 复杂的 let-else
        let code = r#"
fn foo(s: &str) -> i32 {
    let Ok(n) = s.parse::<i32>() else {
        eprintln!("parse failed");
        return 0;
    };
    n * 2
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "复杂 let-else 语法应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_let_chains() {
        // let chains 语法 (Rust 1.88+, unstable)
        // if let Some(x) = opt && x > 0 { ... }
        let code = r#"
fn foo(x: Option<i32>) {
    if let Some(v) = x && v > 0 {
        println!("positive: {}", v);
    }
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "let chains 语法应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_let_else_missing_brace() {
        // let-else 缺少闭合大括号
        let code = "fn foo(x: Option<i32>) -> i32 { let Some(v) = x else { return 0;";
        let result = validate_rust_braces(code);
        assert!(result.is_some(), "缺少闭合大括号的 let-else 应报告问题");
    }
}
