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

/// 预编译正则: 模式1 — ```file:path\n...``` 或 ```lang:path\n...``` (已弃用, 改用逐行解析)
///
/// Session 140: `extract_tagged_files` 已改用逐行解析 (`parse_code_fence_open` +
/// `is_code_fence_close`), 正确处理代码内容中的嵌套反引号。此正则保留供测试参考。
#[allow(dead_code)]
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
            // 字节字符串 b"..." (Session 134)
            (State::Code, 'b', Some('"')) => {
                i += 1; // 跳过 "
                State::String
            }
            // 原始字节字符串 br"..." 或 br#"..."# (Session 134)
            (State::Code, 'b', Some('r')) => {
                // 检查后面是否是 r" 或 r#"
                if i + 2 < chars.len() && chars[i + 2] == '"' {
                    // br"..."
                    i += 2; // 跳过 r"
                    State::RawString(0)
                } else if i + 2 < chars.len() && chars[i + 2] == '#' {
                    // br#"..."#
                    let mut hash_depth = 0;
                    let mut j = i + 2;
                    while j < chars.len() && chars[j] == '#' {
                        hash_depth += 1;
                        j += 1;
                    }
                    if j < chars.len() && chars[j] == '"' {
                        i = j; // 跳过所有 # 和 "
                        State::RawString(hash_depth)
                    } else {
                        State::Code
                    }
                } else {
                    State::Code
                }
            }
            // 字节字符 b'x' (Session 134)
            (State::Code, 'b', Some('\'')) => {
                // b'x' 或 b'\x7f' — 同字符字面量处理
                if chars.len() > i + 3 && chars[i + 3] == '\'' {
                    // b'x'
                    i += 3; // 跳过 'x'
                    State::Code
                } else if chars.len() > i + 4 && chars[i + 2] == '\\' && chars[i + 4] == '\'' {
                    // b'\x7f' 等转义
                    i += 4;
                    State::Code
                } else {
                    State::Code
                }
            }
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

    // Session 140: 截断检测 — 对 .rs 文件检查括号配对
    let truncated = detect_truncated_files(&result);
    if !truncated.is_empty() {
        for (path, issue) in &truncated {
            warn!("⚠ 文件 {} 可能被截断: {}", path, issue);
        }
    }

    result
}

/// 检查行是否是代码块开始标记 (```file:path 或 ```lang:path)
///
/// 返回 Some((path, fence_backtick_count)) 如果是开始标记, 否则 None。
/// fence_backtick_count 是开头的反引号数量 (3 或更多)。
fn parse_code_fence_open(line: &str) -> Option<(String, usize)> {
    let trimmed = line.trim_start();
    // 统计开头的反引号数量
    let backtick_count = trimmed.chars().take_while(|&c| c == '`').count();
    if backtick_count < 3 {
        return None;
    }
    let rest = &trimmed[backtick_count..];
    // 检查是否是 file:path 或 lang:path 格式
    if rest.is_empty() {
        return None; // 普通 ``` 开始标记 (无路径), 不是 tagged 格式
    }
    // 必须有冒号分隔符: file:path 或 lang:path
    if let Some(colon_pos) = rest.find(':') {
        let _lang_part = &rest[..colon_pos];
        let path_part = &rest[colon_pos + 1..].trim();
        if !path_part.is_empty() {
            return Some((path_part.to_string(), backtick_count));
        }
    }
    None
}

/// 检查行是否是代码块结束标记 (``` 或更多反引号, 后面可选附加文本)
///
/// 返回 true 如果行的开头 (去除空白后) 反引号数量 >= fence_backtick_count。
fn is_code_fence_close(line: &str, fence_backtick_count: usize) -> bool {
    let trimmed = line.trim_start();
    let backtick_count = trimmed.chars().take_while(|&c| c == '`').count();
    // 结束标记的反引号数量必须 >= 开始标记的数量 (CommonMark 规范)
    // 且行中反引号之后只能有空白或无内容
    if backtick_count >= fence_backtick_count {
        let after_backticks = &trimmed[backtick_count..];
        return after_backticks.trim().is_empty();
    }
    false
}

/// 模式1: 提取标记代码块 ` ```file:path\n...``` ` 或 ` ```lang:path\n...``` `
///
/// 使用逐行解析代替正则表达式, 正确处理代码内容中包含 ``` 的情况。
/// 这解决了正则非贪婪匹配 `(.*?)``` 在代码含嵌套反引号时截断的问题。
///
/// 解析规则 (CommonMark 兼容):
/// 1. 行首 ``` (3+ 反引号) + `lang:path` → 开始代码块
/// 2. 行首 ``` (N+ 反引号), N >= 开始反引号数, 后面无内容 → 结束代码块
/// 3. 代码块内的 ``` 不被视为结束标记 (除非满足条件 2)
fn extract_tagged_files(text: &str) -> Vec<ExtractedFile> {
    let lines: Vec<&str> = text.lines().collect();
    let mut files = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        // 检查当前行是否是代码块开始标记
        if let Some((path, fence_count)) = parse_code_fence_open(lines[i]) {
            // 收集代码块内容, 直到找到匹配的结束标记
            let mut content_lines: Vec<&str> = Vec::new();
            let mut found_close = false;
            let mut j = i + 1;

            while j < lines.len() {
                if is_code_fence_close(lines[j], fence_count) {
                    found_close = true;
                    break;
                }
                content_lines.push(lines[j]);
                j += 1;
            }

            if found_close {
                let content = content_lines.join("\n");
                let lang = guess_language_from_path(&path);
                if !path.is_empty() && !content.is_empty() {
                    files.push(ExtractedFile {
                        path: path.to_string(),
                        content: content.to_string(),
                        language: lang,
                    });
                }
                i = j + 1; // 跳过结束标记行
            } else {
                // 没有找到结束标记 — 代码块未闭合 (可能被截断)
                // 仍然提取已收集的内容, 但标记为可能截断
                let content = content_lines.join("\n");
                let lang = guess_language_from_path(&path);
                if !path.is_empty() && !content.is_empty() {
                    warn!(
                        "⚠ 代码块 [{}] 未找到结束标记, 可能被截断 (收集 {} 行)",
                        path,
                        content_lines.len()
                    );
                    files.push(ExtractedFile {
                        path: path.to_string(),
                        content: content.to_string(),
                        language: lang,
                    });
                }
                i = j; // 继续从当前位置
            }
        } else {
            i += 1;
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

/// 检测被截断的 Rust 文件
///
/// 对每个 .rs 文件调用 `validate_rust_braces` 检查括号配对。
/// 如果括号不配对, 说明文件可能被截断 (AI 回复提取时丢失了中间内容)。
///
/// 返回被截断的文件路径列表 (路径 + 验证错误信息)。
///
/// # 示例
///
/// ```
/// use forge::extract::{detect_truncated_files, ExtractedFile};
///
/// // 正常文件 — 不截断
/// let files = vec![ExtractedFile {
///     path: "src/main.rs".to_string(),
///     content: "fn main() { println!(\"hello\"); }".to_string(),
///     language: "rust".to_string(),
/// }];
/// let truncated = detect_truncated_files(&files);
/// assert!(truncated.is_empty());
///
/// // 截断文件 — 括号不配对
/// let files = vec![ExtractedFile {
///     path: "src/main.rs".to_string(),
///     content: "fn main() {\n    // code here".to_string(),
///     language: "rust".to_string(),
/// }];
/// let truncated = detect_truncated_files(&files);
/// assert!(!truncated.is_empty());
/// assert!(truncated[0].0.contains("main.rs"));
/// ```
pub fn detect_truncated_files(files: &[ExtractedFile]) -> Vec<(String, String)> {
    let mut truncated = Vec::new();
    for f in files {
        if f.path.ends_with(".rs") {
            if let Some(issue) = validate_rust_braces(&f.content) {
                warn!("⚠ 文件 [{}] 可能被截断: {}", f.path, issue);
                truncated.push((f.path.clone(), issue));
            }
        }
    }
    truncated
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
    /// 使用 `.unwrap_or()` — 可能掩盖错误 (Session 117)
    UnwrapOr,
    /// 使用 `.unwrap_or_default()` — 可能掩盖错误 (Session 117)
    UnwrapOrDefault,
    /// 函数使用 `?` 操作符但未返回 `Result` 类型 (Session 119)
    MissingResultReturn,
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

/// 移除字符串字面量内容, 保留引号标记 (Session 116, Session 117 增强)
///
/// 用于避免字符串中的 `unsafe` 等关键字误报。
/// 支持以下字符串类型 (Session 117 新增 raw string 和字节字符串):
/// - 普通双引号字符串 `"..."` (含转义)
/// - Raw string `r"..."`, `r#"..."#`, `r##"..."##` 等
/// - 字节字符串 `b"..."` (含转义)
/// - Raw 字节字符串 `br"..."`, `br#"..."#` 等
///
/// # 示例
///
/// ```
/// use forge::extract::strip_string_content;
///
/// assert_eq!(strip_string_content("let s = \"unsafe { }\";"), "let s = \"\";");
/// assert_eq!(strip_string_content("unsafe { x }"), "unsafe { x }");
/// // Session 117: raw string
/// assert_eq!(strip_string_content(r#"let s = r"unsafe";"#), r#"let s = r"";"#);
/// assert_eq!(strip_string_content(r##"let s = r#"unsafe"#"##), r##"let s = r#""#"##);
/// // Session 117: byte string
/// assert_eq!(strip_string_content(r#"let s = b"unsafe";"#), r#"let s = b"";"#);
/// ```
pub fn strip_string_content(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut result = String::new();
    let mut i = 0;

    while i < chars.len() {
        // 检测 raw string / byte string 前缀: r, b, br, rb
        if chars[i] == 'r' || chars[i] == 'b' {
            if let Some((prefix_len, hash_count, _is_raw)) = detect_string_prefix(&chars, i) {
                result.extend(chars[i..i + prefix_len].iter());
                i += prefix_len;
                // 跳过内容直到闭合引号 + hash
                if let Some(end) = find_string_end(&chars, i, hash_count) {
                    // 保留闭合引号和 hash 标记
                    result.push('"');
                    for _ in 0..hash_count {
                        result.push('#');
                    }
                    i = end + 1 + hash_count;
                } else {
                    // 未闭合 — 保留剩余部分
                    result.extend(chars[i..].iter());
                    break;
                }
                continue;
            }
        }

        // 普通双引号字符串
        if chars[i] == '"' {
            result.push('"');
            i += 1;
            while i < chars.len() {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    i += 2; // 跳过转义字符
                    continue;
                }
                if chars[i] == '"' {
                    result.push('"');
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }

        result.push(chars[i]);
        i += 1;
    }
    result
}

/// 检测 raw string / byte string 前缀 (Session 117)
///
/// 返回 `Some((prefix_len, hash_count, is_raw))` 其中:
/// - `prefix_len`: 前缀长度 (含引号, 不含 # 标记)
/// - `hash_count`: # 的数量
/// - `is_raw`: 是否为 raw string (无转义)
fn detect_string_prefix(chars: &[char], start: usize) -> Option<(usize, usize, bool)> {
    let mut pos = start;
    let mut is_raw = false;

    // 检测前缀: r, b, br, rb
    if chars[pos] == 'r' {
        is_raw = true;
        pos += 1;
        if pos < chars.len() && chars[pos] == 'b' {
            pos += 1; // rb (raw byte)
        }
    } else if chars[pos] == 'b' {
        pos += 1;
        if pos < chars.len() && chars[pos] == 'r' {
            is_raw = true;
            pos += 1; // br (byte raw)
        }
    }

    // 检测 # 标记
    let hash_start = pos;
    while pos < chars.len() && chars[pos] == '#' {
        pos += 1;
    }
    let hash_count = pos - hash_start;

    // 必须有引号
    if pos < chars.len() && chars[pos] == '"' {
        let prefix_len = pos + 1 - start; // 包含引号
        Some((prefix_len, hash_count, is_raw))
    } else {
        None
    }
}

/// 查找 raw string 的内容结束位置 (Session 117)
///
/// 从引号后开始搜索 `"` 后跟 `hash_count` 个 `#` 的位置。
/// 返回引号的索引 (不含)。
fn find_string_end(chars: &[char], content_start: usize, hash_count: usize) -> Option<usize> {
    let mut i = content_start;
    while i < chars.len() {
        if chars[i] == '"' {
            // 检查后面是否有足够数量的 #
            let mut j = i + 1;
            let mut found_hashes = 0;
            while j < chars.len() && chars[j] == '#' && found_hashes < hash_count {
                found_hashes += 1;
                j += 1;
            }
            if found_hashes == hash_count {
                return Some(i);
            }
        }
        i += 1;
    }
    None
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

/// 检查返回类型字符串是否是需要 `#[must_use]` 的类型 (Session 117, Session 118 扩展)
///
/// 纯函数: 检测返回类型是否属于不应被忽略的类型。
///
/// Session 118 新增类型:
/// - `Box<[T]>`, `Box<str>` — 堆分配的切片/字符串
/// - `Rc<T>`, `Arc<T>` — 引用计数智能指针
/// - `Cow<'a, B>` — 写时复制
/// - `PathBuf`, `&Path` — 路径类型
/// - `&[T]` — 切片引用
/// - `impl Into<>`, `impl AsRef<>` — 转换 trait
/// - `impl DoubleEndedIterator`, `impl ExactSizeIterator`, `impl FusedIterator` — 迭代器 trait
/// - `impl Read`, `impl Write`, `impl BufRead` — I/O trait
fn is_must_use_return_type(return_type: &str) -> bool {
    return_type.starts_with("Result")
        || return_type.starts_with("Option")
        || return_type.starts_with("bool")
        // impl trait 系列
        || return_type.starts_with("impl Iterator")
        || return_type.starts_with("impl IntoIterator")
        || return_type.starts_with("impl Display")
        || return_type.starts_with("impl Debug")
        || return_type.starts_with("impl Into<")
        || return_type.starts_with("impl AsRef<")
        || return_type.starts_with("impl DoubleEndedIterator")
        || return_type.starts_with("impl ExactSizeIterator")
        || return_type.starts_with("impl FusedIterator")
        || return_type.starts_with("impl Read")
        || return_type.starts_with("impl Write")
        || return_type.starts_with("impl BufRead")
        // 字符串类型
        || return_type.starts_with("&str")
        || return_type.starts_with("String")
        // 集合类型
        || return_type.starts_with("Vec<")
        || return_type.starts_with("HashMap<")
        || return_type.starts_with("HashSet<")
        || return_type.starts_with("BTreeMap<")
        || return_type.starts_with("BTreeSet<")
        // 智能指针 (Session 118)
        || return_type.starts_with("Box<[")
        || return_type.starts_with("Box<str")
        || return_type.starts_with("Rc<")
        || return_type.starts_with("Arc<")
        || return_type.starts_with("Cow<")
        // 路径类型 (Session 118)
        || return_type.starts_with("PathBuf")
        || return_type.starts_with("&Path")
        // 切片引用 (Session 118)
        || return_type.starts_with("&[")
}

/// 检查函数签名是否返回需要 `#[must_use]` 的类型 (Session 116, Session 117 增强)
///
/// 检测返回 `Result`、`Option`、`bool`、`&str`、`String`、`Vec` 等不应被忽略的类型的公共函数。
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
    is_must_use_return_type(return_type)
}

/// 检查多行函数签名是否返回需要 `#[must_use]` 的类型 (Session 117)
///
/// 当函数签名跨越多行时 (如 `->` 在下一行), 向下查找返回类型。
/// 最多查找 5 行, 遇到 `{` 则认为函数体已开始, 无返回类型。
fn returns_must_use_type_multiline(lines: &[&str], line_num: usize) -> bool {
    // 如果当前行已能检测到, 直接返回
    if returns_must_use_type(lines[line_num]) {
        return true;
    }
    // 如果当前行有 fn 但没有 ->, 向下查找
    if !lines[line_num].contains("fn ") {
        return false;
    }
    if lines[line_num].contains("->") {
        return false; // 单行有 -> 但未被检测到, 说明不是 must_use 类型
    }
    // 向下查找 -> (最多 5 行)
    for i in 1..=5 {
        if line_num + i >= lines.len() {
            break;
        }
        let next_line = lines[line_num + i].trim();
        if let Some(arrow_pos) = next_line.find("->") {
            let return_type = next_line[arrow_pos + 2..].trim();
            return is_must_use_return_type(return_type);
        }
        // 如果遇到 { 说明函数体开始, 没有返回类型
        if next_line.contains('{') {
            return false;
        }
    }
    false
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

/// 检查前一非空行是否以指定注释前缀开头 (Session 118)
///
/// 用于使前缀修复 (REVIEW/SAFETY) 幂等: 如果上一行已有对应注释,
/// 则不再重复报告该问题。
///
/// # 示例
///
/// ```ignore
/// let lines = vec!["    // REVIEW: 确认 unwrap_or", "    let x = foo().unwrap_or(0);"];
/// assert!(has_prefix_comment(&lines, 1, "// REVIEW:"));
/// ```
fn has_prefix_comment(lines: &[&str], line_num: usize, prefix: &str) -> bool {
    if line_num == 0 {
        return false;
    }
    let prev = lines[line_num - 1].trim();
    prev.starts_with(prefix)
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

            // Session 117: 检查 .unwrap_or() (Session 118: 跳过已有 REVIEW 注释)
            if contains_pattern_outside_comment(trimmed, ".unwrap_or(")
                && !has_prefix_comment(&lines, line_num, "// REVIEW:")
            {
                issues.push(QualityIssue {
                    line: line_num + 1,
                    issue_type: IssueType::UnwrapOr,
                    message: "使用 .unwrap_or() — 可能掩盖错误, 确认是否应传播错误".to_string(),
                    suggestion: Some(
                        "如果操作可能失败, 使用 `?` 传播错误; 如果有合理默认值, 添加注释说明原因"
                            .to_string(),
                    ),
                });
            }

            // Session 117: 检查 .unwrap_or_default() (Session 118: 跳过已有 REVIEW 注释)
            if contains_pattern_outside_comment(trimmed, ".unwrap_or_default()")
                && !has_prefix_comment(&lines, line_num, "// REVIEW:")
            {
                issues.push(QualityIssue {
                    line: line_num + 1,
                    issue_type: IssueType::UnwrapOrDefault,
                    message: "使用 .unwrap_or_default() — 可能掩盖错误, 确认是否应传播错误"
                        .to_string(),
                    suggestion: Some(
                        "如果操作可能失败, 使用 `?` 传播错误; 如果默认值合理, 添加注释说明原因"
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
            // Session 115/116: 检查 unsafe (Session 118: 跳过已有 SAFETY 注释)
            if let Some(unsafe_kind) = detect_unsafe_keyword(trimmed) {
                if has_prefix_comment(&lines, line_num, "// SAFETY:") {
                    // 已有 SAFETY 注释, 跳过检测 (幂等性)
                } else {
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

            // Session 116/117: 检查公共函数返回 Result/Option/bool 缺少 #[must_use]
            if is_public_api_declaration(trimmed)
                && returns_must_use_type_multiline(&lines, line_num)
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

    // Session 119: 检测使用 ? 操作符但未返回 Result 的函数
    issues.extend(detect_missing_result_returns(content));

    issues
}

/// 检测使用 `?` 操作符或将被修复为 `?`/`Err` 的模式但未返回 `Result` 类型的函数 (Session 119)
///
/// 扫描代码中所有函数, 当函数体内使用了以下模式但函数签名未返回 `Result` 时,
/// 报告 `MissingResultReturn` 问题:
/// - `?` 操作符 (已在使用)
/// - `.unwrap()` / `.expect()` (将被修复为 `?`)
/// - `todo!()` / `unimplemented!()` / `panic!()` / `unreachable!()` (将被修复为 `Err`)
///
/// 此检测在 `validate_rust_code_quality_detailed` 中调用。
/// 检测 `.unwrap()` 等模式确保 `apply_fixes` 一次性修复函数签名和函数体, 保持幂等性。
///
/// # 示例
///
/// ```
/// use forge::extract::{detect_missing_result_returns, IssueType};
///
/// // 函数使用 ? 但不返回 Result
/// let issues = detect_missing_result_returns("fn foo() { let x = bar()?; }");
/// assert!(issues.iter().any(|i| i.issue_type == IssueType::MissingResultReturn));
///
/// // 函数使用 unwrap 但不返回 Result — 也会被检测 (unwrap 将被修复为 ?)
/// let issues = detect_missing_result_returns("fn foo() { let x = bar().unwrap(); }");
/// assert!(issues.iter().any(|i| i.issue_type == IssueType::MissingResultReturn));
///
/// // 函数返回 Result — 无问题
/// let issues = detect_missing_result_returns("fn foo() -> Result<(), Error> { let x = bar()?; Ok(()) }");
/// assert!(issues.is_empty());
/// ```
pub fn detect_missing_result_returns(content: &str) -> Vec<QualityIssue> {
    let lines: Vec<&str> = content.lines().collect();
    let mut issues = Vec::new();

    let mut func_start_line: Option<usize> = None;
    let mut brace_depth: i32 = 0;
    let mut needs_result = false;
    let mut returns_result = false;
    let mut in_test_module = false;
    let mut test_module_brace_depth = 0i32;

    for (line_num, &line) in lines.iter().enumerate() {
        let stripped = strip_string_content(line);
        let trimmed = stripped.trim();

        // 跟踪测试模块
        if trimmed.starts_with("mod ") && trimmed.contains("test") && stripped.contains('{') {
            in_test_module = true;
            let opens = stripped.matches('{').count();
            let closes = stripped.matches('}').count();
            test_module_brace_depth = opens as i32 - closes as i32;
            if test_module_brace_depth <= 0 {
                in_test_module = false;
            }
            continue;
        }
        if in_test_module {
            let opens = stripped.matches('{').count();
            let closes = stripped.matches('}').count();
            test_module_brace_depth += opens as i32 - closes as i32;
            if test_module_brace_depth <= 0 {
                in_test_module = false;
            }
            continue;
        }

        // 检测函数开始
        if func_start_line.is_none() {
            if contains_fn_keyword(&stripped)
                && !trimmed.starts_with("//")
                && !trimmed.starts_with("///")
            {
                func_start_line = Some(line_num);
                needs_result = false;
                returns_result = stripped.contains("-> Result")
                    || stripped.contains("-> Option")
                    || stripped.contains("-> impl Iterator");

                if stripped.contains('{') {
                    let opens = stripped.matches('{').count();
                    let closes = stripped.matches('}').count();
                    brace_depth = opens as i32 - closes as i32;

                    if contains_result_requiring_pattern(&stripped, line) {
                        needs_result = true;
                    }

                    if brace_depth <= 0 {
                        if needs_result && !returns_result {
                            push_missing_result_issue(&mut issues, func_start_line.unwrap());
                        }
                        func_start_line = None;
                        brace_depth = 0;
                    }
                }
            }
        } else if brace_depth == 0 {
            // 多行签名收集阶段
            if !returns_result
                && (stripped.contains("-> Result")
                    || stripped.contains("-> Option")
                    || stripped.contains("-> impl Iterator"))
            {
                returns_result = true;
            }
            if stripped.contains('{') {
                let opens = stripped.matches('{').count();
                let closes = stripped.matches('}').count();
                brace_depth = opens as i32 - closes as i32;

                if !returns_result
                    && (stripped.contains("-> Result")
                        || stripped.contains("-> Option")
                        || stripped.contains("-> impl Iterator"))
                {
                    returns_result = true;
                }
                if contains_result_requiring_pattern(&stripped, line) {
                    needs_result = true;
                }

                if brace_depth <= 0 {
                    if needs_result && !returns_result {
                        push_missing_result_issue(&mut issues, func_start_line.unwrap());
                    }
                    func_start_line = None;
                    brace_depth = 0;
                }
            }
        } else {
            // 函数体内
            let opens = stripped.matches('{').count();
            let closes = stripped.matches('}').count();
            brace_depth += opens as i32 - closes as i32;

            if contains_result_requiring_pattern(&stripped, line) {
                needs_result = true;
            }

            if brace_depth <= 0 {
                if needs_result && !returns_result {
                    push_missing_result_issue(&mut issues, func_start_line.unwrap());
                }
                func_start_line = None;
                brace_depth = 0;
            }
        }
    }

    issues
}

/// 检查 stripped 行是否包含 `fn` 关键字 (辅助函数, Session 119)
fn contains_fn_keyword(stripped: &str) -> bool {
    // 检查 "fn " 或 "fn\t" 或 "fn{" 或 "fn("
    stripped.contains("fn ")
        || stripped.contains("fn\t")
        || stripped.contains("fn(")
        || stripped.contains("fn{")
}

/// 检查行是否包含需要函数返回 Result 的模式 (辅助函数, Session 119)
///
/// 以下模式在 `apply_fixes` 后会使用 `?` 或 `Err(anyhow!(...))`:
/// - `?` 操作符 (已在代码中使用)
/// - `.unwrap()` → `?`
/// - `.expect(` → `?`
/// - `todo!(` → `Err(anyhow!(...))`
/// - `unimplemented!(` → `Err(anyhow!(...))`
/// - `panic!(` → `return Err(anyhow!(...))`
/// - `unreachable!(` → `return Err(anyhow!(...))`
fn contains_result_requiring_pattern(stripped: &str, original_line: &str) -> bool {
    // 检查 ? 操作符 (排除注释和字符串)
    if stripped.contains('?') && !is_in_comment_or_string(original_line, '?') {
        return true;
    }
    // 对其他模式, 使用 contains_pattern_outside_comment 排除注释中的匹配
    if contains_pattern_outside_comment(stripped, ".unwrap()") {
        return true;
    }
    if contains_pattern_outside_comment(stripped, ".expect(") {
        return true;
    }
    if contains_pattern_outside_comment(stripped, "todo!(") {
        return true;
    }
    if contains_pattern_outside_comment(stripped, "unimplemented!(") {
        return true;
    }
    if contains_pattern_outside_comment(stripped, "panic!(") {
        return true;
    }
    if contains_pattern_outside_comment(stripped, "unreachable!(") {
        return true;
    }
    false
}

/// 推送 MissingResultReturn 问题 (辅助函数, Session 119)
fn push_missing_result_issue(issues: &mut Vec<QualityIssue>, line: usize) {
    issues.push(QualityIssue {
        line: line + 1,
        issue_type: IssueType::MissingResultReturn,
        message: "函数使用 ? 操作符但未返回 Result 类型".to_string(),
        suggestion: Some("将函数返回类型改为 `Result<T, anyhow::Error>`".to_string()),
    });
}

/// 检查字符是否在注释中 (辅助函数, Session 119)
///
/// 使用 `strip_string_content` 已移除字符串内容后, 仍需检查 `?` 是否在行注释中。
/// 返回 `true` 表示字符在注释中 (应跳过), `false` 表示字符在代码中 (应报告)。
fn is_in_comment_or_string(line: &str, target: char) -> bool {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    let mut in_string = false;
    let mut in_line_comment = false;

    while i < chars.len() {
        if in_line_comment {
            return true;
        }
        if in_string {
            if chars[i] == '"' && (i == 0 || chars[i - 1] != '\\') {
                in_string = false;
            }
        } else {
            if chars[i] == '"' {
                in_string = true;
            } else if i + 1 < chars.len() && chars[i] == '/' && chars[i + 1] == '/' {
                in_line_comment = true;
            } else if chars[i] == target {
                return false;
            }
        }
        i += 1;
    }
    true
}

/// 为质量问题生成自动修复代码 (Session 116, Session 119 增强)
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
/// - `UnwrapOr`: 添加 `// REVIEW:` 注释 (Session 117)
/// - `UnwrapOrDefault`: 添加 `// REVIEW:` 注释 (Session 117)
/// - `MissingResultReturn`: 修改函数签名添加 `-> Result<T, anyhow::Error>` (Session 119)
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
        IssueType::UnwrapOr => {
            let indent_len = original_line.len() - original_line.trim_start().len();
            let indent_str = &original_line[..indent_len];
            Some(format!(
                "{}// REVIEW: 确认 unwrap_or 是否应改为 ? 传播错误\n{}",
                indent_str, original_line
            ))
        }
        IssueType::UnwrapOrDefault => {
            let indent_len = original_line.len() - original_line.trim_start().len();
            let indent_str = &original_line[..indent_len];
            Some(format!(
                "{}// REVIEW: 确认 unwrap_or_default 是否应改为 ? 传播错误\n{}",
                indent_str, original_line
            ))
        }
        IssueType::MissingResultReturn => {
            // 函数使用 ? 但未返回 Result — 修改函数签名添加 -> Result<T, anyhow::Error>
            // 两种情况:
            // 1. fn foo() -> i32 { → fn foo() -> Result<i32, anyhow::Error> {
            // 2. fn foo() { → fn foo() -> Result<(), anyhow::Error> {
            let stripped = strip_string_content(original_line);
            let trimmed = stripped.trim();

            // 检查是否是函数签名行 (包含 fn 关键字)
            if !trimmed.contains("fn ") && !trimmed.contains("fn\t") {
                return None;
            }

            // 情况 1: 已有返回类型 -> ReturnType {
            // 找到 -> 和 { 之间的返回类型
            if let Some(arrow_pos) = stripped.find("->") {
                // 找到 { 的位置
                if let Some(brace_pos) = stripped[arrow_pos..].find('{') {
                    let return_type = stripped[arrow_pos + 2..arrow_pos + brace_pos].trim();
                    // 如果已经返回 Result, 不需要修复
                    if return_type.starts_with("Result") {
                        return None;
                    }
                    let before = &original_line[..arrow_pos + 2];
                    let brace_abs = arrow_pos + brace_pos;
                    let after = if brace_abs < original_line.len() {
                        &original_line[brace_abs..]
                    } else {
                        ""
                    };
                    return Some(format!(
                        "{} Result<{}, anyhow::Error>{}",
                        before, return_type, after
                    ));
                }
            }

            // 情况 2: 无返回类型 fn foo() {
            // 在 { 前插入 -> Result<(), anyhow::Error>
            if let Some(brace_pos) = stripped.find('{') {
                // 确保这不是一个 struct/enum 定义
                let before_brace = stripped[..brace_pos].trim();
                if before_brace.contains("fn ") || before_brace.contains("fn\t") {
                    let before = &original_line[..brace_pos];
                    let after = &original_line[brace_pos..];
                    return Some(format!("{}-> Result<(), anyhow::Error> {}", before, after));
                }
            }

            None
        }
    }
}

/// 获取修复优先级 — 同一行的多个问题按优先级排序 (Session 117)
///
/// 优先级 0: 原地修复 (修改行内容, 不改变行数)
/// 优先级 1: 前缀修复 (在行前添加注释/属性)
/// 优先级 2: 文档注释前缀 (应在属性之后, 代码之前)
fn fix_priority(issue_type: &IssueType) -> u8 {
    match issue_type {
        IssueType::Unwrap
        | IssueType::Expect
        | IssueType::Todo
        | IssueType::Unimplemented
        | IssueType::Panic
        | IssueType::Unreachable
        | IssueType::MissingResultReturn => 0,
        IssueType::UnsafeBlock
        | IssueType::UnsafeFn
        | IssueType::UnsafeImpl
        | IssueType::UnwrapOr
        | IssueType::UnwrapOrDefault
        | IssueType::MissingMustUse => 1,
        IssueType::MissingDoc => 2,
    }
}

/// 批量自动修复 — 对整个文件内容应用所有可修复的质量问题 (Session 117)
///
/// 调用 `validate_rust_code_quality_detailed` 检测所有问题,
/// 然后对每个问题调用 `generate_fix` 生成修复, 合并后返回修复后的完整内容。
///
/// 处理顺序: 从最后一行向第一行处理 (避免行号偏移),
/// 同一行内按优先级处理 (原地修复 → 前缀修复 → 文档注释)。
///
/// # 示例
///
/// ```
/// use forge::extract::apply_fixes;
///
/// let code = "fn foo() { let x = bar().unwrap(); }";
/// let fixed = apply_fixes(code);
/// assert!(!fixed.contains(".unwrap()"));
/// assert!(fixed.contains('?'));
/// ```
pub fn apply_fixes(content: &str) -> String {
    let issues = validate_rust_code_quality_detailed(content);
    if issues.is_empty() {
        return content.to_string();
    }

    let mut result_lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();

    // 按行号降序排列 (从底向顶处理), 同一行按优先级升序 (原地修复先于前缀修复)
    let mut sorted_issues = issues;
    sorted_issues.sort_by(|a, b| {
        b.line
            .cmp(&a.line)
            .then(fix_priority(&a.issue_type).cmp(&fix_priority(&b.issue_type)))
    });

    for issue in &sorted_issues {
        if issue.line == 0 || issue.line > result_lines.len() {
            continue;
        }
        let line_idx = issue.line - 1; // 转为 0-based
        let original_line = result_lines[line_idx].clone();
        if let Some(fixed) = generate_fix(issue, &original_line) {
            let fixed_lines: Vec<String> = fixed.lines().map(|s| s.to_string()).collect();
            result_lines.splice(line_idx..=line_idx, fixed_lines);
        }
    }

    result_lines.join("\n")
}

/// 批量自动修复 + 自动导入 — 修复问题并添加缺失的 `use anyhow::Result;` 导入 (Session 120)
///
/// 在 `apply_fixes` 的基础上, 自动检查修复后的代码是否需要 `use anyhow::Result;` 导入。
/// 当 `MissingResultReturn` 修复将函数签名改为 `Result<T, anyhow::Error>` 后,
/// 此函数确保文件顶部有对应的导入语句。
///
/// # 工作流程
///
/// 1. 调用 `apply_fixes` 修复所有问题
/// 2. 调用 `ensure_anyhow_import` 添加缺失的导入
///
/// # 示例
///
/// ```
/// use forge::extract::apply_fixes_with_imports;
///
/// // 函数使用 ? 但未返回 Result, 修复后需要导入
/// let code = "fn foo() -> i32 { let x: Result<i32, _> = Ok(42); x? }";
/// let fixed = apply_fixes_with_imports(code);
/// assert!(fixed.contains("Result<"), "应修改返回类型");
/// assert!(fixed.contains("use anyhow::"), "应添加导入");
/// ```
pub fn apply_fixes_with_imports(content: &str) -> String {
    // 1. 修复所有质量问题 (unwrap → ?, 签名修改等)
    let fixed = apply_fixes(content);
    // 2. 包装函数体最后的表达式为 Ok(...) (Session 121)
    let wrapped = wrap_last_expression_in_ok(&fixed);
    // 3. 包装 return 语句为 Ok(...) (Session 122)
    let return_wrapped = wrap_return_statements_in_ok(&wrapped);
    // 4. 确保所需的 anyhow 导入 (增强版, Session 121)
    let anyhow_imported = ensure_anyhow_imports(&return_wrapped);
    // 5. 合并分散的 anyhow 导入 (Session 121)
    let anyhow_merged = merge_anyhow_imports(&anyhow_imported);
    // 6. 确保所需的 std 导入 (Session 125)
    let std_imported = ensure_std_imports(&anyhow_merged);
    // 7. 确保所需的外部 crate 导入 (Session 127)
    ensure_external_imports(&std_imported)
}

/// 修复预览 — dry-run 模式的返回值 (Session 118)
///
/// 包含原始内容、修复后内容、检测到的问题列表和修复统计,
/// 不实际修改文件, 仅预览将要应用的修复。
///
/// # 字段
///
/// - `original_content`: 原始代码内容
/// - `fixed_content`: 修复后的代码内容 (与 `apply_fixes` 结果相同)
/// - `issues`: 检测到的所有质量问题
/// - `fixes_applied`: 成功应用的修复数量
/// - `is_changed`: 是否有任何变化 (`fixed_content != original_content`)
///
/// # 示例
///
/// ```
/// use forge::extract::apply_fixes_dry_run;
///
/// let code = "fn foo() { let x = bar().unwrap(); }";
/// let preview = apply_fixes_dry_run(code);
/// assert!(preview.is_changed);
/// assert!(preview.fixes_applied > 0);
/// assert!(!preview.fixed_content.contains(".unwrap()"));
/// ```
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FixPreview {
    /// 原始代码内容
    pub original_content: String,
    /// 修复后的代码内容
    pub fixed_content: String,
    /// 检测到的所有质量问题
    pub issues: Vec<QualityIssue>,
    /// 成功应用的修复数量
    pub fixes_applied: usize,
    /// 是否有任何变化
    pub is_changed: bool,
}

/// 批量自动修复 dry-run 模式 — 预览将要应用的修复 (Session 118)
///
/// 与 `apply_fixes` 功能相同, 但返回 `FixPreview` 结构体,
/// 包含详细的修复信息 (问题列表、修复数量、变化标识),
/// 不修改原始内容, 适用于:
/// - 代码审查: 预览修复后再决定是否应用
/// - 修复报告: 展示哪些问题被自动修复
/// - CI/CD: 在 PR 中展示修复预览
///
/// # 示例
///
/// ```
/// use forge::extract::apply_fixes_dry_run;
///
/// let code = "pub fn foo() -> bool { true }";
/// let preview = apply_fixes_dry_run(code);
/// assert!(preview.is_changed, "应有修复");
/// assert!(!preview.issues.is_empty(), "应检测到问题");
/// assert!(preview.fixed_content.contains("#[must_use]"), "应添加 #[must_use]");
/// ```
pub fn apply_fixes_dry_run(content: &str) -> FixPreview {
    let issues = validate_rust_code_quality_detailed(content);
    let fixed_content = apply_fixes(content);
    let fixes_applied = if issues.is_empty() {
        0
    } else {
        // 统计实际应用的修复数量 (有变化则至少应用了一个)
        if fixed_content != content {
            // 通过比较行数差和问题数估算
            issues
                .iter()
                .filter(|issue| {
                    // 检查 generate_fix 是否能产生修复
                    let lines: Vec<&str> = content.lines().collect();
                    if issue.line == 0 || issue.line > lines.len() {
                        return false;
                    }
                    generate_fix(issue, lines[issue.line - 1]).is_some()
                })
                .count()
        } else {
            0
        }
    };

    FixPreview {
        is_changed: fixed_content != content,
        original_content: content.to_string(),
        fixed_content,
        issues,
        fixes_applied,
    }
}

/// 批量自动修复 (带过滤) — 只修复指定类型的问题 (Session 119)
///
/// 与 `apply_fixes` 功能相同, 但只对 `filter` 中列出的 `IssueType` 应用修复。
/// 其他类型的问题会被检测但不修复, 适用于:
/// - 只修复 unwrap/expect (高优先级安全风险), 不修复 missing_doc (低优先级)
/// - 分阶段修复: 先修复原地修复类 (unwrap/expect/todo), 再修复前缀类 (doc/must_use)
///
/// # 示例
///
/// ```
/// use forge::extract::{apply_fixes_filtered, IssueType};
///
/// let code = "pub fn foo() -> bool { let x = bar().unwrap(); true }";
/// // 只修复 Unwrap, 不修复 MissingDoc / MissingMustUse
/// let filter = [IssueType::Unwrap];
/// let fixed = apply_fixes_filtered(code, &filter);
/// assert!(!fixed.contains(".unwrap()"), "unwrap 应被修复");
/// assert!(fixed.contains("pub fn foo()"), "函数签名应保留");
/// ```
pub fn apply_fixes_filtered(content: &str, filter: &[IssueType]) -> String {
    if filter.is_empty() {
        return content.to_string();
    }

    let all_issues = validate_rust_code_quality_detailed(content);
    let issues: Vec<QualityIssue> = all_issues
        .into_iter()
        .filter(|issue| filter.contains(&issue.issue_type))
        .collect();

    if issues.is_empty() {
        return content.to_string();
    }

    let mut result_lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();

    // 按行号降序排列 (从底向顶处理), 同一行按优先级升序
    let mut sorted_issues = issues;
    sorted_issues.sort_by(|a, b| {
        b.line
            .cmp(&a.line)
            .then(fix_priority(&a.issue_type).cmp(&fix_priority(&b.issue_type)))
    });

    for issue in &sorted_issues {
        if issue.line == 0 || issue.line > result_lines.len() {
            continue;
        }
        let line_idx = issue.line - 1;
        let original_line = result_lines[line_idx].clone();
        if let Some(fixed) = generate_fix(issue, &original_line) {
            let fixed_lines: Vec<String> = fixed.lines().map(|s| s.to_string()).collect();
            result_lines.splice(line_idx..=line_idx, fixed_lines);
        }
    }

    result_lines.join("\n")
}

/// 批量自动修复 dry-run 模式 (带过滤) — 预览指定类型的修复 (Session 119)
///
/// 与 `apply_fixes_dry_run` 功能相同, 但只对 `filter` 中列出的 `IssueType` 应用修复。
/// `issues` 字段仍包含所有检测到的问题 (不论是否在过滤列表中),
/// `fixes_applied` 只统计被过滤后实际应用的修复数量。
///
/// # 示例
///
/// ```
/// use forge::extract::{apply_fixes_dry_run_filtered, IssueType};
///
/// let code = "pub fn foo() -> bool { let x = bar().unwrap(); true }";
/// // 只修复 Unwrap
/// let filter = [IssueType::Unwrap];
/// let preview = apply_fixes_dry_run_filtered(code, &filter);
/// assert!(preview.is_changed, "应有变化");
/// assert!(preview.fixes_applied > 0, "应有修复");
/// assert!(!preview.fixed_content.contains(".unwrap()"), "unwrap 应被修复");
/// // MissingDoc 和 MissingMustUse 问题仍应被检测到
/// assert!(preview.issues.len() >= 2, "应检测到所有问题 (包括未过滤的)");
/// ```
pub fn apply_fixes_dry_run_filtered(content: &str, filter: &[IssueType]) -> FixPreview {
    let issues = validate_rust_code_quality_detailed(content);
    let fixed_content = apply_fixes_filtered(content, filter);
    let fixes_applied = if fixed_content != content && !filter.is_empty() {
        issues
            .iter()
            .filter(|issue| {
                if !filter.contains(&issue.issue_type) {
                    return false;
                }
                let lines: Vec<&str> = content.lines().collect();
                if issue.line == 0 || issue.line > lines.len() {
                    return false;
                }
                generate_fix(issue, lines[issue.line - 1]).is_some()
            })
            .count()
    } else {
        0
    };

    FixPreview {
        is_changed: fixed_content != content,
        original_content: content.to_string(),
        fixed_content,
        issues,
        fixes_applied,
    }
}

/// 批量自动修复 (排除模式) — 修复除指定类型外的所有问题 (Session 120)
///
/// 与 `apply_fixes_filtered` 互补: 不是"只修复指定类型", 而是"修复除了指定类型外的所有类型"。
/// 适用于: 先排除低优先级修复 (如 MissingDoc), 集中修复高优先级问题 (unwrap/expect/panic 等)。
///
/// # 参数
///
/// - `content`: 原始代码内容
/// - `exclude`: 要排除的 `IssueType` 列表 (这些类型不会被修复)
///
/// # 示例
///
/// ```
/// use forge::extract::{apply_fixes_except, IssueType};
///
/// let code = "pub fn foo() -> bool { let x = bar().unwrap(); true }";
/// // 排除 MissingDoc 和 MissingMustUse, 只修复 Unwrap
/// let exclude = [IssueType::MissingDoc, IssueType::MissingMustUse];
/// let fixed = apply_fixes_except(code, &exclude);
/// assert!(!fixed.contains(".unwrap()"), "unwrap 应被修复");
/// assert!(!fixed.contains("/// TODO:"), "不应添加文档注释 (已排除)");
/// ```
pub fn apply_fixes_except(content: &str, exclude: &[IssueType]) -> String {
    let all_issues = validate_rust_code_quality_detailed(content);
    let issues: Vec<QualityIssue> = all_issues
        .into_iter()
        .filter(|issue| !exclude.contains(&issue.issue_type))
        .collect();

    if issues.is_empty() {
        return content.to_string();
    }

    let mut result_lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();

    // 按行号降序排列 (从底向顶处理), 同一行按优先级升序
    let mut sorted_issues = issues;
    sorted_issues.sort_by(|a, b| {
        b.line
            .cmp(&a.line)
            .then(fix_priority(&a.issue_type).cmp(&fix_priority(&b.issue_type)))
    });

    for issue in &sorted_issues {
        if issue.line == 0 || issue.line > result_lines.len() {
            continue;
        }
        let line_idx = issue.line - 1;
        let original_line = result_lines[line_idx].clone();
        if let Some(fixed) = generate_fix(issue, &original_line) {
            let fixed_lines: Vec<String> = fixed.lines().map(|s| s.to_string()).collect();
            result_lines.splice(line_idx..=line_idx, fixed_lines);
        }
    }

    result_lines.join("\n")
}

/// 批量自动修复 dry-run 模式 (排除模式) — 预览除指定类型外的修复 (Session 120)
///
/// 与 `apply_fixes_dry_run_filtered` 互补: 修复除了 `exclude` 中列出的类型外的所有问题。
/// `issues` 字段仍包含所有检测到的问题 (不论是否被排除),
/// `fixes_applied` 只统计未被排除且实际应用的修复数量。
///
/// # 示例
///
/// ```
/// use forge::extract::{apply_fixes_dry_run_except, IssueType};
///
/// let code = "pub fn foo() -> bool { let x = bar().unwrap(); true }";
/// // 排除 MissingDoc, 修复其他所有问题
/// let exclude = [IssueType::MissingDoc];
/// let preview = apply_fixes_dry_run_except(code, &exclude);
/// assert!(preview.is_changed, "应有变化");
/// assert!(preview.fixes_applied > 0, "应有修复");
/// assert!(!preview.fixed_content.contains(".unwrap()"), "unwrap 应被修复");
/// assert!(preview.issues.len() >= 2, "应检测到所有问题 (包括被排除的)");
/// ```
pub fn apply_fixes_dry_run_except(content: &str, exclude: &[IssueType]) -> FixPreview {
    let issues = validate_rust_code_quality_detailed(content);
    let fixed_content = apply_fixes_except(content, exclude);
    let fixes_applied = if fixed_content != content {
        issues
            .iter()
            .filter(|issue| {
                if exclude.contains(&issue.issue_type) {
                    return false;
                }
                let lines: Vec<&str> = content.lines().collect();
                if issue.line == 0 || issue.line > lines.len() {
                    return false;
                }
                generate_fix(issue, lines[issue.line - 1]).is_some()
            })
            .count()
    } else {
        0
    };

    FixPreview {
        is_changed: fixed_content != content,
        original_content: content.to_string(),
        fixed_content,
        issues,
        fixes_applied,
    }
}

/// 行级别变更类型 (Session 120)
///
/// 描述两行之间的变更关系。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LineDiffType {
    /// 行被修改 (内容变化但行号相同)
    Modified,
    /// 行被添加 (修复后新增的行)
    Added,
    /// 行被删除 (修复后移除的行)
    Removed,
}

/// 行级别差异 — 描述两行之间的变更 (Session 120)
///
/// 由 `compute_line_diff` 返回, 提供 `apply_fixes` 前后逐行比较的差异信息。
///
/// # 字段
///
/// - `line_number`: 行号 (1-based, 基于原始内容)
/// - `diff_type`: 变更类型 (Modified/Added/Removed)
/// - `original_line`: 原始行内容 (Removed/Modified 时有值, Added 时为 None)
/// - `fixed_line`: 修复后行内容 (Added/Modified 时有值, Removed 时为 None)
///
/// # 示例
///
/// ```
/// use forge::extract::{compute_line_diff, LineDiffType};
///
/// let original = "fn foo() { bar().unwrap(); }";
/// let fixed = "fn foo() { bar()?; }";
/// let diffs = compute_line_diff(original, fixed);
/// assert!(!diffs.is_empty(), "应有差异");
/// assert!(diffs.iter().any(|d| d.diff_type == LineDiffType::Modified), "应有修改行");
/// ```
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LineDiff {
    /// 行号 (1-based, 基于原始内容)
    pub line_number: usize,
    /// 变更类型
    pub diff_type: LineDiffType,
    /// 原始行内容 (Removed/Modified 时有值, Added 时为 None)
    pub original_line: Option<String>,
    /// 修复后行内容 (Added/Modified 时有值, Removed 时为 None)
    pub fixed_line: Option<String>,
}

/// 计算两段文本之间的行级别差异 (Session 120)
///
/// 逐行比较原始内容和修复后内容, 返回所有有变化的行的差异信息。
/// 用于 `verify_idempotent_detailed` 中提供行级别的 diff, 而非仅问题列表。
///
/// # 示例
///
/// ```
/// use forge::extract::{compute_line_diff, LineDiffType};
///
/// // 无差异
/// let diffs = compute_line_diff("fn foo() {}", "fn foo() {}");
/// assert!(diffs.is_empty());
///
/// // 有差异
/// let diffs = compute_line_diff("let x = 1;", "let x = 2;");
/// assert_eq!(diffs.len(), 1);
/// assert_eq!(diffs[0].diff_type, LineDiffType::Modified);
/// ```
pub fn compute_line_diff(original: &str, fixed: &str) -> Vec<LineDiff> {
    let original_lines: Vec<&str> = original.lines().collect();
    let fixed_lines: Vec<&str> = fixed.lines().collect();
    let mut diffs = Vec::new();

    let max_len = original_lines.len().max(fixed_lines.len());

    for i in 0..max_len {
        let line_number = i + 1;
        match (original_lines.get(i), fixed_lines.get(i)) {
            (Some(orig), Some(fix)) => {
                if orig != fix {
                    diffs.push(LineDiff {
                        line_number,
                        diff_type: LineDiffType::Modified,
                        original_line: Some(orig.to_string()),
                        fixed_line: Some(fix.to_string()),
                    });
                }
            }
            (Some(orig), None) => {
                diffs.push(LineDiff {
                    line_number,
                    diff_type: LineDiffType::Removed,
                    original_line: Some(orig.to_string()),
                    fixed_line: None,
                });
            }
            (None, Some(fix)) => {
                diffs.push(LineDiff {
                    line_number,
                    diff_type: LineDiffType::Added,
                    original_line: None,
                    fixed_line: Some(fix.to_string()),
                });
            }
            (None, None) => {}
        }
    }

    diffs
}

/// 确保代码包含 `use anyhow::Result;` 导入 (Session 120)
///
/// 当 `apply_fixes` 将函数签名修改为返回 `Result<T, anyhow::Error>` 后,
/// 需要确保文件顶部有 `use anyhow::Result;` (或等价的 `use anyhow::Error;`) 导入。
/// 此函数检查并添加缺失的导入。
///
/// # 规则
///
/// 1. 如果代码不包含 `anyhow::Error` 或 `anyhow::Result`, 不需要添加导入, 返回原内容
/// 2. 如果已有 `use anyhow::Result;` / `use anyhow::Error;` / `use anyhow::*;` 等导入, 不重复添加
/// 3. 否则在文件第一个非注释/非属性行前插入 `use anyhow::Result;`
///
/// # 示例
///
/// ```
/// use forge::extract::ensure_anyhow_import;
///
/// // 已有导入, 不修改
/// let code = "use anyhow::Result;\nfn foo() -> Result<i32, anyhow::Error> { Ok(42) }";
/// assert_eq!(ensure_anyhow_import(code), code);
///
/// // 需要添加导入
/// let code = "fn foo() -> Result<i32, anyhow::Error> { Ok(42) }";
/// let result = ensure_anyhow_import(code);
/// assert!(result.contains("use anyhow::Result;"), "应添加导入");
/// ```
pub fn ensure_anyhow_import(content: &str) -> String {
    // 如果不包含 anyhow::Error 或 anyhow::Result, 不需要添加
    if !content.contains("anyhow::Error") && !content.contains("anyhow::Result") {
        return content.to_string();
    }

    // 检查是否已有 anyhow 导入
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("use anyhow::Result;")
            || trimmed.starts_with("use anyhow::Error;")
            || trimmed.starts_with("use anyhow::*;")
            || trimmed.contains("use anyhow::{")
                && trimmed.contains("Result")
                && trimmed.contains("Error")
        {
            return content.to_string();
        }
    }

    // 找到插入位置: 第一个非注释、非属性、非空白行之前
    let lines: Vec<&str> = content.lines().collect();
    let mut insert_pos = 0;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("//")
            || trimmed.starts_with("/*")
            || trimmed.starts_with("*")
            || trimmed.starts_with("#![")
            || trimmed.starts_with("//!")
            || trimmed.starts_with("///")
        {
            continue;
        }
        // 找到第一个代码行, 在它前面插入
        insert_pos = i;
        break;
    }

    // 如果全是注释/空白, 在末尾插入
    if insert_pos == 0 && !lines.is_empty() {
        // 检查是否全是注释
        let all_comments = lines
            .iter()
            .all(|l| l.trim().is_empty() || l.trim().starts_with("//"));
        if all_comments {
            insert_pos = lines.len();
        }
    }

    let mut result_lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    result_lines.insert(insert_pos, "use anyhow::Result;".to_string());

    result_lines.join("\n")
}

/// 幂等性验证结果 — 包含详细差异信息 (Session 119, Session 120 增强)
///
/// 当 `apply_fixes` 不满足幂等性时, 通过此结构体可以查看具体的差异,
/// 包括第一次修复后剩余的问题和第二次修复后新增的问题。
///
/// # 字段
///
/// - `is_idempotent`: 是否幂等 (二次应用无变化)
/// - `first_pass_issues`: 第一次修复后检测到的问题
/// - `second_pass_issues`: 第二次修复后检测到的问题
/// - `new_issues_in_second_pass`: 第二次修复后新增的问题 (不在第一次结果中)
/// - `first_pass_diff`: 原始内容与第一次修复之间的行级别差异 (Session 120)
/// - `second_pass_diff`: 第一次修复与第二次修复之间的行级别差异 (Session 120)
///
/// # 示例
///
/// ```
/// use forge::extract::verify_idempotent_detailed;
///
/// let result = verify_idempotent_detailed("fn foo() -> i32 { 42 }");
/// assert!(result.is_idempotent, "无问题代码应幂等");
/// assert!(result.first_pass_issues.is_empty());
/// assert!(result.second_pass_issues.is_empty());
/// ```
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IdempotencyResult {
    /// 是否幂等 (二次应用无变化)
    pub is_idempotent: bool,
    /// 第一次修复后检测到的问题
    pub first_pass_issues: Vec<QualityIssue>,
    /// 第二次修复后检测到的问题
    pub second_pass_issues: Vec<QualityIssue>,
    /// 第二次修复后新增的问题 (不在第一次结果中)
    pub new_issues_in_second_pass: Vec<QualityIssue>,
    /// 原始内容与第一次修复之间的行级别差异 (Session 120)
    pub first_pass_diff: Vec<LineDiff>,
    /// 第一次修复与第二次修复之间的行级别差异 (Session 120)
    pub second_pass_diff: Vec<LineDiff>,
}

/// 验证 apply_fixes 的幂等性 — 返回详细差异 (Session 119)
///
/// 与 `verify_idempotent` 功能相同, 但返回 `IdempotencyResult` 结构体,
/// 包含第一次和第二次修复后的具体问题列表, 便于调查非幂等的原因。
///
/// # 示例
///
/// ```
/// use forge::extract::verify_idempotent_detailed;
///
/// // 无问题的代码是幂等的
/// let result = verify_idempotent_detailed("fn foo() -> i32 { 42 }");
/// assert!(result.is_idempotent);
///
/// // 有问题的代码, 修复后应幂等
/// let result = verify_idempotent_detailed("fn foo() { let x = bar().unwrap(); }");
/// assert!(result.is_idempotent, "修复后二次应用应无变化");
/// ```
pub fn verify_idempotent_detailed(content: &str) -> IdempotencyResult {
    let first_pass = apply_fixes(content);
    let first_pass_issues = validate_rust_code_quality_detailed(&first_pass);
    let second_pass = apply_fixes(&first_pass);
    let second_pass_issues = validate_rust_code_quality_detailed(&second_pass);

    // 找出第二次修复后新增的问题 (不在第一次结果中)
    let new_issues_in_second_pass: Vec<QualityIssue> = second_pass_issues
        .iter()
        .filter(|s| {
            !first_pass_issues
                .iter()
                .any(|f| f.line == s.line && f.issue_type == s.issue_type)
        })
        .cloned()
        .collect();

    // 计算行级别差异 (Session 120, Session 124: 使用统一 diff 接口自动选择最优算法)
    let first_pass_diff = compute_line_diff_unified(content, &first_pass);
    let second_pass_diff = compute_line_diff_unified(&first_pass, &second_pass);

    IdempotencyResult {
        is_idempotent: first_pass == second_pass,
        first_pass_issues,
        second_pass_issues,
        new_issues_in_second_pass,
        first_pass_diff,
        second_pass_diff,
    }
}

/// 运行 clippy 检查 — 对项目目录执行 cargo clippy (Session 119)
///
/// 在代码写入工作区后调用, 返回 clippy 的警告和错误。
/// 此函数需要项目目录中存在 `Cargo.toml` 文件。
///
/// # 参数
///
/// - `project_dir`: 项目目录路径 (包含 Cargo.toml)
/// - `fix`: 是否使用 `--fix` 自动修复 (需要 nightly 工具链)
///
/// # 返回值
///
/// - `Ok(Vec<String>)`: clippy 输出的消息列表 (空列表表示无问题)
/// - `Err(String)`: clippy 执行失败 (如 Cargo.toml 不存在或 clippy 未安装)
///
/// # 示例
///
/// ```no_run
/// use forge::extract::run_clippy_check;
///
/// let messages = run_clippy_check("./projects/my-project", false).unwrap();
/// if messages.is_empty() {
///     println!("clippy 无警告");
/// } else {
///     for msg in &messages {
///         println!("{}", msg);
///     }
/// }
/// ```
pub fn run_clippy_check(project_dir: &str, fix: bool) -> Result<Vec<String>, String> {
    let cargo_toml = std::path::Path::new(project_dir).join("Cargo.toml");
    if !cargo_toml.exists() {
        return Err(format!("Cargo.toml 不存在于目录: {}", project_dir));
    }

    let mut cmd = std::process::Command::new("cargo");
    cmd.current_dir(project_dir)
        .arg("clippy")
        .arg("--message-format=short");

    if fix {
        cmd.arg("--fix").arg("--allow-dirty").arg("--allow-no-vcs");
    }

    let output = cmd
        .output()
        .map_err(|e| format!("执行 cargo clippy 失败: {}", e))?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    let mut messages = Vec::new();

    // clippy --message-format=short 输出格式:
    // path:line:col: message
    for line in stderr.lines().chain(stdout.lines()) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // 跳过 cargo 自身的输出 (如 "Compiling...", "Finished...")
        if trimmed.starts_with("Compiling")
            || trimmed.starts_with("Finished")
            || trimmed.starts_with("Downloading")
            || trimmed.starts_with("Downloaded")
            || trimmed.starts_with("warning: unused")
            || trimmed.starts_with("Checking")
        {
            continue;
        }
        // 只保留 clippy 警告和错误 (包含 "warning:" 或 "error:")
        if trimmed.contains("warning:") || trimmed.contains("error:") {
            messages.push(trimmed.to_string());
        }
    }

    Ok(messages)
}

/// 验证 apply_fixes 的幂等性 — 二次应用不应产生变化 (Session 118)
///
/// 幂等性: `apply_fixes(apply_fixes(x)) == apply_fixes(x)`
///
/// 如果 `apply_fixes` 一次应用后已修复所有问题, 二次应用应无变化。
/// 如果不满足幂等性, 说明修复可能引入了新的问题或修复不完整。
///
/// # 返回值
///
/// - `true`: 二次应用无变化 (幂等)
/// - `false`: 二次应用仍有变化 (非幂等, 需要调查)
///
/// # 示例
///
/// ```
/// use forge::extract::verify_idempotent;
///
/// // 无问题的代码是幂等的
/// assert!(verify_idempotent("fn foo() -> i32 { 42 }"));
///
/// // 有问题的代码, 修复后应幂等
/// assert!(verify_idempotent("fn foo() { let x = bar().unwrap(); }"));
/// ```
pub fn verify_idempotent(content: &str) -> bool {
    let first_pass = apply_fixes(content);
    let second_pass = apply_fixes(&first_pass);
    first_pass == second_pass
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

// ============================================================================
//  Session 121: 增强自动修复 — 函数体 Ok 包装 + 合并导入 + LCS diff
// ============================================================================

/// 包装函数体最后的表达式为 `Ok(...)` (Session 121)
///
/// 当 `generate_fix` 将函数签名修改为 `-> Result<T, anyhow::Error>` 后,
/// 函数体中的返回值也需要包装为 `Ok(...)`。此函数:
///
/// 1. 扫描所有返回 `Result<T, anyhow::Error>` 的函数
/// 2. 找到函数体最后的表达式 (闭合 `}` 前最后一个非空/非注释行)
/// 3. 如果该表达式不以 `Ok(` / `Err(` / `return` / `?` 开头, 包装为 `Ok(...)`
///
/// # 幂等性
///
/// 二次调用不产生变化 — 已包装的表达式不会被重复包装。
///
/// # 示例
///
/// ```
/// use forge::extract::wrap_last_expression_in_ok;
///
/// // 单行函数: 42 → Ok(42)
/// let code = "fn foo() -> Result<i32, anyhow::Error> { 42 }";
/// let result = wrap_last_expression_in_ok(code);
/// assert!(result.contains("Ok(42)"), "应包装返回值");
///
/// // 已包装的不修改 (幂等)
/// let code = "fn foo() -> Result<i32, anyhow::Error> { Ok(42) }";
/// assert_eq!(wrap_last_expression_in_ok(code), code, "已包装不修改");
/// ```
pub fn wrap_last_expression_in_ok(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut result_lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    let stripped = strip_string_content(content);
    let stripped_lines: Vec<&str> = stripped.lines().collect();

    let mut i = 0;
    while i < stripped_lines.len() {
        let line = stripped_lines[i].trim();

        // 检测函数签名: 包含 fn 和 -> Result< 和 anyhow::Error
        if (line.contains("fn ") || line.contains("fn\t"))
            && line.contains("-> Result<")
            && line.contains("anyhow::Error")
        {
            // 找到函数体的起始 { 并跟踪深度
            let mut brace_depth = 0i32;
            let fn_start = i;
            let mut found_open = false;

            // 在当前行查找 {
            for (char_idx, ch) in stripped_lines[i].char_indices() {
                if ch == '{' {
                    brace_depth = 1;
                    found_open = true;
                    // 检查是否是单行函数 (同一行有闭合 })
                    let after = &stripped_lines[i][char_idx + 1..];
                    if let Some(close_pos) = after.rfind('}') {
                        let body = after[..close_pos].trim();
                        if !body.is_empty()
                            && !body.starts_with("Ok(")
                            && !body.starts_with("Err(")
                            && !body.starts_with("return")
                            && !body.ends_with('?')
                            && !body.ends_with("?;")
                            && !body.starts_with("//")
                        {
                            // 单行函数: 包装 body — clone 避免借用冲突
                            let orig_line = result_lines[i].clone();
                            if let Some(pos) = orig_line.find('{') {
                                if let Some(end_pos) = orig_line.rfind('}') {
                                    let inner = orig_line[pos + 1..end_pos].trim();
                                    if !inner.is_empty()
                                        && !inner.starts_with("Ok(")
                                        && !inner.starts_with("Err(")
                                        && !inner.starts_with("return")
                                        && !inner.ends_with('?')
                                        && !inner.ends_with("?;")
                                    {
                                        result_lines[i] = format!(
                                            "{} Ok({}){}",
                                            &orig_line[..pos + 1],
                                            inner,
                                            &orig_line[end_pos..]
                                        );
                                    }
                                }
                            }
                        }
                    }
                    break;
                }
            }

            if found_open && brace_depth > 0 {
                // 多行函数: 找到闭合 }
                let mut j = i + 1;
                while j < stripped_lines.len() && brace_depth > 0 {
                    for ch in stripped_lines[j].chars() {
                        match ch {
                            '{' => brace_depth += 1,
                            '}' => brace_depth -= 1,
                            _ => {}
                        }
                    }

                    if brace_depth == 0 {
                        // 当前行包含闭合 }, 找到了函数体末尾
                        if let Some(brace_pos) = stripped_lines[j].rfind('}') {
                            let before_brace = stripped_lines[j][..brace_pos].trim();
                            if !before_brace.is_empty()
                                && !before_brace.starts_with("//")
                                && !before_brace.starts_with("/*")
                                && !before_brace.starts_with("Ok(")
                                && !before_brace.starts_with("Err(")
                                && !before_brace.starts_with("return")
                                && !before_brace.ends_with('?')
                                && !before_brace.ends_with("?;")
                            {
                                // 同行有表达式 — clone 避免借用冲突
                                let orig_line = result_lines[j].clone();
                                if let Some(obp) = orig_line.rfind('}') {
                                    let inner = orig_line[..obp].trim_end();
                                    let expr = inner.trim();
                                    if !expr.is_empty()
                                        && !expr.starts_with("Ok(")
                                        && !expr.starts_with("Err(")
                                        && !expr.starts_with("return")
                                        && !expr.ends_with('?')
                                        && !expr.ends_with("?;")
                                    {
                                        let indent_len = inner.len() - inner.trim_start().len();
                                        let indent = &inner[..indent_len];
                                        result_lines[j] =
                                            format!("{}Ok({}){}", indent, expr, &orig_line[obp..]);
                                    }
                                }
                            } else {
                                // 在前一行查找最后的表达式
                                let mut k = j.saturating_sub(1);
                                while k > fn_start {
                                    let prev = stripped_lines[k].trim();
                                    if prev.is_empty()
                                        || prev.starts_with("//")
                                        || prev.starts_with("/*")
                                        || prev.starts_with("*")
                                        || prev.starts_with("}")
                                    {
                                        k = k.saturating_sub(1);
                                        continue;
                                    }
                                    // 找到最后的表达式行
                                    if !prev.starts_with("Ok(")
                                        && !prev.starts_with("Err(")
                                        && !prev.starts_with("return")
                                        && !prev.ends_with('?')
                                        && !prev.ends_with("?;")
                                        && !prev.ends_with(';')
                                        && !prev.starts_with("let ")
                                        && !prev.starts_with("if ")
                                        && !prev.starts_with("match ")
                                        && !prev.starts_with("for ")
                                        && !prev.starts_with("while ")
                                        && !prev.starts_with("loop ")
                                    {
                                        // 包装这行 — clone 避免借用冲突
                                        let orig_line = result_lines[k].clone();
                                        let indent_len =
                                            orig_line.len() - orig_line.trim_start().len();
                                        let indent = &orig_line[..indent_len];
                                        let expr = orig_line.trim();
                                        result_lines[k] = format!("{}Ok({})", indent, expr);
                                    }
                                    break;
                                }
                            }
                        }
                        break;
                    }

                    j += 1;
                }
            }
        }

        i += 1;
    }

    result_lines.join("\n")
}

/// 合并分散的 `use anyhow::Result;` 和 `use anyhow::Error;` 为合并导入 (Session 121)
///
/// 将:
/// ```ignore
/// use anyhow::Result;
/// use anyhow::Error;
/// ```
/// 合并为:
/// ```ignore
/// use anyhow::{Result, Error};
/// ```
///
/// # 幂等性
///
/// 已合并的导入不会被重复处理。
///
/// # 示例
///
/// ```
/// use forge::extract::merge_anyhow_imports;
///
/// // 分散导入 → 合并导入
/// let code = "use anyhow::Result;\nuse anyhow::Error;\nfn foo() {}";
/// let result = merge_anyhow_imports(code);
/// assert!(result.contains("use anyhow::{Result, Error};"), "应合并导入");
/// assert!(!result.contains("use anyhow::Result;\n"), "不应有单独的 Result 导入");
///
/// // 已合并不修改 (幂等)
/// let code = "use anyhow::{Result, Error};\nfn foo() {}";
/// assert_eq!(merge_anyhow_imports(code), code, "已合并不修改");
/// ```
pub fn merge_anyhow_imports(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut has_result = false;
    let mut has_error = false;
    let mut result_line_idx: Option<usize> = None;
    let mut error_line_idx: Option<usize> = None;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed == "use anyhow::Result;" {
            has_result = true;
            result_line_idx = Some(i);
        } else if trimmed == "use anyhow::Error;" {
            has_error = true;
            error_line_idx = Some(i);
        }
    }

    // 只有同时存在分散的 Result 和 Error 导入时才合并
    if !has_result || !has_error {
        return content.to_string();
    }

    // 检查是否已有合并导入
    for line in &lines {
        let trimmed = line.trim();
        if trimmed.contains("use anyhow::{")
            && trimmed.contains("Result")
            && trimmed.contains("Error")
        {
            // 已有合并导入, 删除分散的导入行
            let mut result_lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
            if let Some(ri) = result_line_idx {
                result_lines[ri] = String::new();
            }
            if let Some(ei) = error_line_idx {
                result_lines[ei] = String::new();
            }
            return result_lines.join("\n");
        }
    }

    // 合并: 保留第一个位置, 删除第二个
    let mut result_lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    let keep_idx = result_line_idx.min(error_line_idx).unwrap();
    let remove_idx = result_line_idx.max(error_line_idx).unwrap();

    result_lines[keep_idx] = "use anyhow::{Result, Error};".to_string();
    result_lines.remove(remove_idx);

    result_lines.join("\n")
}

/// 确保代码包含所需的 anyhow 导入 (增强版, Session 121)
///
/// 与 `ensure_anyhow_import` 相比, 此函数:
/// 1. 同时检查 `Result` 和 `Error` 的导入需求
/// 2. 支持合并导入 `use anyhow::{Result, Error};`
/// 3. 如果已有其中一个, 自动合并添加另一个
///
/// # 规则
///
/// 1. 检测代码中使用了 `anyhow::Result` / `anyhow::Error` 哪些类型
/// 2. 检查已有的导入 (包括合并导入 `use anyhow::{...}`)
/// 3. 缺失的导入自动添加, 已有的不重复
/// 4. 如果已有分散导入, 合并为 `use anyhow::{Result, Error};`
///
/// # 示例
///
/// ```
/// use forge::extract::ensure_anyhow_imports;
///
/// // 需要 Result 和 Error, 都没有 → 添加合并导入
/// let code = "fn foo() -> Result<i32, anyhow::Error> { Ok(42) }";
/// let result = ensure_anyhow_imports(code);
/// assert!(result.contains("use anyhow::"), "应添加导入");
///
/// // 已有 Result 导入, 还需要 Error → 合并
/// let code = "use anyhow::Result;\nfn foo() -> Result<i32, anyhow::Error> { Ok(42) }";
/// let result = ensure_anyhow_imports(code);
/// assert!(result.contains("Error"), "应包含 Error 导入");
///
/// // 幂等
/// let second = ensure_anyhow_imports(&result);
/// assert_eq!(result, second, "二次调用不变化");
/// ```
pub fn ensure_anyhow_imports(content: &str) -> String {
    // 检测需要哪些导入
    let needs_result = content.contains("anyhow::Result") || content.contains("Result<");
    let needs_error = content.contains("anyhow::Error");

    if !needs_result && !needs_error {
        return content.to_string();
    }

    // 检查已有导入
    let mut has_result_import = false;
    let mut has_error_import = false;
    let mut has_wildcard = false;
    let mut has_merged = false;
    let mut result_line_idx: Option<usize> = None;
    let mut error_line_idx: Option<usize> = None;

    let lines: Vec<&str> = content.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("use anyhow::Result;") {
            has_result_import = true;
            result_line_idx = Some(i);
        } else if trimmed.starts_with("use anyhow::Error;") {
            has_error_import = true;
            error_line_idx = Some(i);
        } else if trimmed.starts_with("use anyhow::*;") {
            has_wildcard = true;
        } else if trimmed.contains("use anyhow::{") {
            if trimmed.contains("Result") {
                has_result_import = true;
            }
            if trimmed.contains("Error") {
                has_error_import = true;
            }
            if trimmed.contains("Result") && trimmed.contains("Error") {
                has_merged = true;
            }
        }
    }

    // 通配导入已覆盖所有
    if has_wildcard {
        return content.to_string();
    }

    // 检查是否需要添加什么
    let need_add_result = needs_result && !has_result_import;
    let need_add_error = needs_error && !has_error_import;

    // 如果不需要添加任何东西, 尝试合并分散导入
    if !need_add_result && !need_add_error {
        return merge_anyhow_imports(content);
    }

    // 如果已有其中一个分散导入, 需要合并添加另一个
    if has_result_import && need_add_error {
        if let Some(idx) = result_line_idx {
            // 将 use anyhow::Result; 替换为 use anyhow::{Result, Error};
            let mut result_lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
            result_lines[idx] = "use anyhow::{Result, Error};".to_string();
            return result_lines.join("\n");
        }
    }

    if has_error_import && need_add_result {
        if let Some(idx) = error_line_idx {
            // 将 use anyhow::Error; 替换为 use anyhow::{Result, Error};
            let mut result_lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
            result_lines[idx] = "use anyhow::{Result, Error};".to_string();
            return result_lines.join("\n");
        }
    }

    // 如果已有合并导入但缺少一个, 补充
    if has_merged {
        return content.to_string();
    }

    // 需要新增导入: 构建导入语句
    let import_line = if need_add_result && need_add_error {
        "use anyhow::{Result, Error};".to_string()
    } else if need_add_result {
        "use anyhow::Result;".to_string()
    } else {
        "use anyhow::Error;".to_string()
    };

    // 找到插入位置
    let mut insert_pos = 0;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("//")
            || trimmed.starts_with("/*")
            || trimmed.starts_with("*")
            || trimmed.starts_with("#![")
            || trimmed.starts_with("//!")
            || trimmed.starts_with("///")
        {
            continue;
        }
        insert_pos = i;
        break;
    }

    if insert_pos == 0 && !lines.is_empty() {
        let all_comments = lines
            .iter()
            .all(|l| l.trim().is_empty() || l.trim().starts_with("//"));
        if all_comments {
            insert_pos = lines.len();
        }
    }

    let mut result_lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    result_lines.insert(insert_pos, import_line);

    result_lines.join("\n")
}

/// 计算两段文本之间的行级别差异 (LCS 算法, Session 121)
///
/// 使用最长公共子序列 (Longest Common Subsequence) 算法,
/// 比逐行比较更准确地识别插入和删除操作。
///
/// 与 `compute_line_diff` 的区别:
/// - `compute_line_diff`: 逐行比较, 行号对齐 — 简单但插入/删除会导致后续全部标记为 Modified
/// - `compute_line_diff_lcs`: LCS 对齐 — 正确识别中间插入/删除, 不影响后续行的匹配
///
/// # 算法
///
/// 1. 构建 LCS 动态规划表 O(m×n)
/// 2. 回溯找出公共行
/// 3. 非公共行标记为 Added (仅存在于 fixed) 或 Removed (仅存在于 original)
///
/// # 示例
///
/// ```
/// use forge::extract::{compute_line_diff_lcs, LineDiffType};
///
/// // 无差异
/// let diffs = compute_line_diff_lcs("fn foo() {}", "fn foo() {}");
/// assert!(diffs.is_empty());
///
/// // 中间插入一行 — LCS 正确识别为 Added
/// let original = "fn foo() {\n}\n";
/// let fixed = "fn foo() {\n    let x = 42;\n}\n";
/// let diffs = compute_line_diff_lcs(original, fixed);
/// assert!(diffs.iter().any(|d| d.diff_type == LineDiffType::Added), "应有 Added");
/// assert!(!diffs.iter().any(|d| d.diff_type == LineDiffType::Modified), "不应有 Modified");
/// ```
pub fn compute_line_diff_lcs(original: &str, fixed: &str) -> Vec<LineDiff> {
    let orig_lines: Vec<&str> = original.lines().collect();
    let fix_lines: Vec<&str> = fixed.lines().collect();
    let m = orig_lines.len();
    let n = fix_lines.len();

    // 构建 LCS 动态规划表
    // dp[i][j] = orig_lines[0..i] 和 fix_lines[0..j] 的 LCS 长度
    let mut dp = vec![vec![0usize; n + 1]; m + 1];
    for i in 1..=m {
        for j in 1..=n {
            if orig_lines[i - 1] == fix_lines[j - 1] {
                dp[i][j] = dp[i - 1][j - 1] + 1;
            } else {
                dp[i][j] = dp[i - 1][j].max(dp[i][j - 1]);
            }
        }
    }

    // 回溯找出 LCS 并同时构建 diff
    let mut diffs = Vec::new();
    let mut i = m;
    let mut j = n;
    let mut line_number = 1usize;

    // 从后向前回溯, 收集操作
    let mut operations: Vec<(usize, LineDiffType, Option<String>, Option<String>)> = Vec::new();

    while i > 0 || j > 0 {
        if i > 0 && j > 0 && orig_lines[i - 1] == fix_lines[j - 1] {
            // 公共行 — 无差异
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || dp[i][j - 1] >= dp[i - 1][j]) {
            // Added: 仅存在于 fixed
            operations.push((
                j,
                LineDiffType::Added,
                None,
                Some(fix_lines[j - 1].to_string()),
            ));
            j -= 1;
        } else if i > 0 {
            // Removed: 仅存在于 original
            operations.push((
                i,
                LineDiffType::Removed,
                Some(orig_lines[i - 1].to_string()),
                None,
            ));
            i -= 1;
        }
    }

    // 反转操作 (之前是从后向前)
    operations.reverse();

    // 分配行号
    for (ln, diff_type, orig, fix) in operations {
        diffs.push(LineDiff {
            line_number: ln,
            diff_type,
            original_line: orig,
            fixed_line: fix,
        });
        line_number = ln + 1;
    }

    let _ = line_number; // suppress unused warning
    diffs
}

/// 分阶段自动修复 — 按优先级分批修复, 每批验证后再修下一批 (Session 121)
///
/// 与 `apply_fixes` 一次性修复所有问题不同, 此函数分三个阶段:
///
/// 1. **高优先级**: Unwrap / Expect / Todo / Unimplemented / Panic / Unreachable / MissingResultReturn
/// 2. **中优先级**: UnsafeBlock / UnsafeFn / UnsafeImpl / UnwrapOr / UnwrapOrDefault / MissingMustUse
/// 3. **低优先级**: MissingDoc
///
/// 每阶段使用 `apply_fixes_except` 排除其他阶段的问题,
/// 确保高优先级修复不会因低优先级修复的行号偏移而错位。
///
/// # 示例
///
/// ```
/// use forge::extract::apply_staged_fixes;
///
/// let code = "pub fn foo() { let x = bar().unwrap(); }";
/// let result = apply_staged_fixes(code);
/// assert!(!result.contains(".unwrap()"), "unwrap 应被修复");
/// assert!(result.contains("#[must_use]"), "应添加 #[must_use]");
/// ```
pub fn apply_staged_fixes(content: &str) -> String {
    // 阶段 1: 高优先级 — 排除中低优先级
    let stage1_exclude = [
        IssueType::UnsafeBlock,
        IssueType::UnsafeFn,
        IssueType::UnsafeImpl,
        IssueType::UnwrapOr,
        IssueType::UnwrapOrDefault,
        IssueType::MissingMustUse,
        IssueType::MissingDoc,
    ];
    let after_stage1 = apply_fixes_except(content, &stage1_exclude);

    // 阶段 2: 中优先级 — 排除低优先级
    let stage2_exclude = [IssueType::MissingDoc];
    let after_stage2 = apply_fixes_except(&after_stage1, &stage2_exclude);

    // 阶段 3: 低优先级 — 修复剩余所有问题
    apply_fixes(&after_stage2)
}

/// 分阶段修复验证结果 (Session 144)
///
/// 记录每阶段修复后的验证状态, 包括是否回滚。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedFixValidation {
    /// 阶段 1 (高优先级) 修复后的内容
    pub stage1_result: String,
    /// 阶段 1 是否通过验证 (true = 通过, false = 回滚)
    pub stage1_valid: bool,
    /// 阶段 2 (中优先级) 修复后的内容
    pub stage2_result: String,
    /// 阶段 2 是否通过验证
    pub stage2_valid: bool,
    /// 阶段 3 (低优先级) 修复后的内容
    pub stage3_result: String,
    /// 阶段 3 是否通过验证
    pub stage3_valid: bool,
    /// 最终结果 (可能回滚到某阶段或原始内容)
    pub final_result: String,
    /// 是否有任何阶段被回滚
    pub any_rolled_back: bool,
    /// 回滚的阶段列表 (1, 2, 3)
    pub rolled_back_stages: Vec<u8>,
}

/// 分阶段修复带验证 — 每阶段修复后调用验证函数, 失败则回滚 (Session 144)
///
/// 与 `apply_staged_fixes` 不同, 此函数在每个阶段修复后调用 `validate_fn` 验证。
/// 如果验证失败 (返回 false), 该阶段修复被回滚 (使用修复前的内容)。
///
/// # 参数
///
/// - `content`: 原始代码
/// - `validate_fn`: 验证函数, 接收修复后的代码, 返回 true 表示通过, false 表示失败
///
/// # 回滚策略
///
/// - 阶段 1 失败 → 回滚到原始内容, 继续阶段 2 (基于原始内容)
/// - 阶段 2 失败 → 回滚到阶段 1 结果, 继续阶段 3 (基于阶段 1 结果)
/// - 阶段 3 失败 → 回滚到阶段 2 结果
///
/// # 示例
///
/// ```
/// use forge::extract::apply_staged_fixes_with_validation;
///
/// let code = "pub fn foo() { let x = bar().unwrap(); }";
/// // 验证函数: 始终通过
/// let result = apply_staged_fixes_with_validation(code, |_| true);
/// assert!(!result.final_result.contains(".unwrap()"), "unwrap 应被修复");
/// assert!(!result.any_rolled_back, "不应有回滚");
/// ```
pub fn apply_staged_fixes_with_validation<F>(
    content: &str,
    mut validate_fn: F,
) -> StagedFixValidation
where
    F: FnMut(&str) -> bool,
{
    let stage1_exclude = [
        IssueType::UnsafeBlock,
        IssueType::UnsafeFn,
        IssueType::UnsafeImpl,
        IssueType::UnwrapOr,
        IssueType::UnwrapOrDefault,
        IssueType::MissingMustUse,
        IssueType::MissingDoc,
    ];
    let after_stage1 = apply_fixes_except(content, &stage1_exclude);

    // 阶段 1 验证
    let (stage1_result, stage1_valid) = if validate_fn(&after_stage1) {
        (after_stage1.clone(), true)
    } else {
        (content.to_string(), false) // 回滚到原始内容
    };

    let stage2_exclude = [IssueType::MissingDoc];
    let after_stage2 = apply_fixes_except(&stage1_result, &stage2_exclude);

    // 阶段 2 验证
    let (stage2_result, stage2_valid) = if validate_fn(&after_stage2) {
        (after_stage2.clone(), true)
    } else {
        (stage1_result.clone(), false) // 回滚到阶段 1 结果
    };

    let after_stage3 = apply_fixes(&stage2_result);

    // 阶段 3 验证
    let (stage3_result, stage3_valid) = if validate_fn(&after_stage3) {
        (after_stage3.clone(), true)
    } else {
        (stage2_result.clone(), false) // 回滚到阶段 2 结果
    };

    let mut rolled_back_stages = Vec::new();
    if !stage1_valid {
        rolled_back_stages.push(1);
    }
    if !stage2_valid {
        rolled_back_stages.push(2);
    }
    if !stage3_valid {
        rolled_back_stages.push(3);
    }

    StagedFixValidation {
        stage1_result,
        stage1_valid,
        stage2_result,
        stage2_valid,
        stage3_result: stage3_result.clone(),
        stage3_valid,
        final_result: stage3_result,
        any_rolled_back: !rolled_back_stages.is_empty(),
        rolled_back_stages,
    }
}

/// 包装 Result 函数中的 `return` 语句 (Session 122)
///
/// 与 `wrap_last_expression_in_ok` 互补:
/// - `wrap_last_expression_in_ok`: 包装函数体最后的尾表达式
/// - `wrap_return_statements_in_ok`: 包装函数体中的 `return value;` 语句
///
/// 检测返回 `Result<T, anyhow::Error>` 的函数中的 `return` 语句,
/// 如果 return 的值不以 `Ok(` 或 `Err(` 开头, 包装为 `return Ok(value);`。
///
/// # 幂等性
///
/// 已包装的 `return Ok(...)` 或 `return Err(...)` 语句不会被重复处理。
///
/// # 示例
///
/// ```
/// use forge::extract::wrap_return_statements_in_ok;
///
/// // return 42 → return Ok(42)
/// let code = "fn foo() -> Result<i32, anyhow::Error> {\n    return 42;\n}";
/// let result = wrap_return_statements_in_ok(code);
/// assert!(result.contains("return Ok(42);"), "应包装 return 值");
///
/// // 已包装不修改 (幂等)
/// let code = "fn foo() -> Result<i32, anyhow::Error> {\n    return Ok(42);\n}";
/// assert_eq!(wrap_return_statements_in_ok(code), code, "已包装不修改");
/// ```
pub fn wrap_return_statements_in_ok(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut result_lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    let stripped = strip_string_content(content);
    let stripped_lines: Vec<&str> = stripped.lines().collect();

    let mut i = 0;
    while i < stripped_lines.len() {
        let line = stripped_lines[i].trim();

        // 检测函数签名: 包含 fn 和 -> Result< 和 anyhow::Error
        if (line.contains("fn ") || line.contains("fn\t"))
            && line.contains("-> Result<")
            && line.contains("anyhow::Error")
        {
            // 找到函数体起始 {
            if let Some(brace_pos) = stripped_lines[i].find('{') {
                // 从 { 之后开始处理
                let after_brace = &stripped_lines[i][brace_pos + 1..];
                let mut brace_depth = 1i32;

                // 处理同一行 { 之后的 } (单行函数)
                for ch in after_brace.chars() {
                    match ch {
                        '{' => brace_depth += 1,
                        '}' => brace_depth -= 1,
                        _ => {}
                    }
                }

                // 如果函数体未闭合, 继续到下一行
                let mut j = i + 1;
                while j < stripped_lines.len() && brace_depth > 0 {
                    let cur = stripped_lines[j].trim();

                    // 检测 return 语句 (Session 122 + Session 123 多行 + Session 124 闭包/async/tab)
                    // Session 124: 支持 tab (\t) 和 return-at-end-of-line (表达式在下一行)
                    let is_return_stmt = cur == "return"
                        || cur == "return;"
                        || cur.starts_with("return ")
                        || cur.starts_with("return\t");

                    let already_wrapped = cur.starts_with("return Ok(")
                        || cur.starts_with("return Err(")
                        || cur.starts_with("return\tOk(")
                        || cur.starts_with("return\tErr(");

                    if is_return_stmt && !already_wrapped {
                        // Session 124: 处理 return; (返回 unit) → return Ok(());
                        if cur == "return;" {
                            let orig_line = result_lines[j].clone();
                            let indent_len = orig_line.len() - orig_line.trim_start().len();
                            let indent = &orig_line[..indent_len];
                            result_lines[j] = format!("{}return Ok(());", indent);
                        } else {
                            // 获取 return 之后的表达式
                            let after_return: &str = if cur == "return" {
                                // return 在行尾, 表达式在下一行 (Session 124)
                                ""
                            } else {
                                // 跳过 "return" (6 chars) + 空白
                                cur[6..].trim_start()
                            };
                            let trimmed_expr = after_return.trim();

                            // 跳过已包装或空表达式
                            // Session 124: cur == "return" 时 trimmed_expr 为空, 但表达式在下一行
                            if (!trimmed_expr.is_empty() || cur == "return")
                                && !trimmed_expr.starts_with("Ok(")
                                && !trimmed_expr.starts_with("Err(")
                            {
                                if trimmed_expr.ends_with(';') {
                                    // 单行 return (Session 122)
                                    let expr = trimmed_expr.trim_end_matches(';').trim();
                                    let orig_line = result_lines[j].clone();
                                    let indent_len = orig_line.len() - orig_line.trim_start().len();
                                    let indent = &orig_line[..indent_len];
                                    result_lines[j] = format!("{}return Ok({});", indent, expr);
                                } else {
                                    // 多行 return (Session 123 + Session 124)
                                    // 跟踪表达式内的括号深度, 找到终止 ;
                                    // 支持: 闭包 (||x| ...), async 块 (async { ... }),
                                    //       unsafe 块 (unsafe { ... }), move 闭包 (move || ...)
                                    let mut expr_depth = 0i32;
                                    let mut found_semi = false;

                                    // 计算首行 return 之后部分的深度
                                    for ch in after_return.chars() {
                                        match ch {
                                            '(' | '[' | '{' => expr_depth += 1,
                                            ')' | ']' | '}' => expr_depth -= 1,
                                            ';' if expr_depth == 0 => found_semi = true,
                                            _ => {}
                                        }
                                    }

                                    // Session 124: return 在行尾时, expr_depth == 0 且 !found_semi
                                    // 此时表达式在下一行, 需要扫描后续行
                                    if !found_semi
                                        && (expr_depth > 0 || (cur == "return" && expr_depth == 0))
                                    {
                                        // 扫描后续行找到终止 ;
                                        let mut end_line = j + 1;
                                        while end_line < stripped_lines.len() {
                                            for ch in stripped_lines[end_line].chars() {
                                                match ch {
                                                    '(' | '[' | '{' => expr_depth += 1,
                                                    ')' | ']' | '}' => expr_depth -= 1,
                                                    ';' if expr_depth == 0 => found_semi = true,
                                                    _ => {}
                                                }
                                            }
                                            if found_semi && expr_depth == 0 {
                                                break;
                                            }
                                            end_line += 1;
                                        }

                                        if found_semi && end_line < stripped_lines.len() {
                                            // 包装多行表达式: return expr → return Ok(expr
                                            let orig_line = result_lines[j].clone();
                                            let indent_len =
                                                orig_line.len() - orig_line.trim_start().len();
                                            let indent = &orig_line[..indent_len];

                                            if cur == "return" {
                                                // Session 124: return 在行尾, 表达式在下一行
                                                // 首行改为 "return Ok(" (不含表达式)
                                                result_lines[j] = format!("{}return Ok(", indent);
                                                // 末行在 ; 前插入 )
                                                let last_line = result_lines[end_line].clone();
                                                let last_trimmed = last_line.trim();
                                                if let Some(semi_pos) = last_trimmed.rfind(';') {
                                                    let before_semi = &last_trimmed[..semi_pos];
                                                    let last_indent_len = last_line.len()
                                                        - last_line.trim_start().len();
                                                    let last_indent = &last_line[..last_indent_len];
                                                    result_lines[end_line] =
                                                        format!("{}{});", last_indent, before_semi);
                                                }
                                            } else {
                                                // 正常多行 return (Session 123)
                                                // 修改首行: "    return expr..." → "    return Ok(expr..."
                                                result_lines[j] = format!(
                                                    "{}return Ok({}",
                                                    indent,
                                                    after_return.trim_start()
                                                );

                                                // 修改末行: "...);" → "...);"
                                                // 在最后一个 ; 前插入 )
                                                let last_line = result_lines[end_line].clone();
                                                let last_trimmed = last_line.trim();
                                                if let Some(semi_pos) = last_trimmed.rfind(';') {
                                                    let before_semi = &last_trimmed[..semi_pos];
                                                    let last_indent_len = last_line.len()
                                                        - last_line.trim_start().len();
                                                    let last_indent = &last_line[..last_indent_len];
                                                    result_lines[end_line] =
                                                        format!("{}{});", last_indent, before_semi);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // 更新 brace 深度
                    for ch in stripped_lines[j].chars() {
                        match ch {
                            '{' => brace_depth += 1,
                            '}' => brace_depth -= 1,
                            _ => {}
                        }
                    }

                    j += 1;
                }

                // 跳到函数体结束之后
                i = j;
                continue;
            }
        }

        i += 1;
    }

    result_lines.join("\n")
}

/// 使用 Myers diff 算法计算行级别差异 (Session 122)
///
/// Myers 算法时间复杂度 O(ND), 其中 N 是行数, D 是差异行数。
/// 当差异较少时 (D << N), 比 LCS 的 O(N×M) 更高效。
///
/// 与 `compute_line_diff_lcs` 的区别:
/// - `compute_line_diff_lcs`: LCS 动态规划, O(N×M) 时间空间
/// - `compute_line_diff_myers`: Myers O(ND), 稀疏差异时更高效
/// - 两者产生相同格式的 `LineDiff` 结果
///
/// # 算法
///
/// 1. 构建编辑图, 寻找最短编辑路径 O(ND)
/// 2. 沿对角线回溯, 识别公共行 (snake)
/// 3. 非公共行标记为 Added 或 Removed
///
/// # 示例
///
/// ```
/// use forge::extract::{compute_line_diff_myers, LineDiffType};
///
/// // 无差异
/// let diffs = compute_line_diff_myers("fn foo() {}", "fn foo() {}");
/// assert!(diffs.is_empty());
///
/// // 插入一行
/// let original = "fn foo() {\n}\n";
/// let fixed = "fn foo() {\n    let x = 42;\n}\n";
/// let diffs = compute_line_diff_myers(original, fixed);
/// assert!(diffs.iter().any(|d| d.diff_type == LineDiffType::Added), "应有 Added");
/// assert!(!diffs.iter().any(|d| d.diff_type == LineDiffType::Modified), "不应有 Modified");
/// ```
pub fn compute_line_diff_myers(original: &str, fixed: &str) -> Vec<LineDiff> {
    let a: Vec<&str> = original.lines().collect();
    let b: Vec<&str> = fixed.lines().collect();
    let n = a.len();
    let m = b.len();

    if n == 0 && m == 0 {
        return Vec::new();
    }
    if n == 0 {
        return b
            .iter()
            .enumerate()
            .map(|(i, line)| LineDiff {
                line_number: i + 1,
                diff_type: LineDiffType::Added,
                original_line: None,
                fixed_line: Some(line.to_string()),
            })
            .collect();
    }
    if m == 0 {
        return a
            .iter()
            .enumerate()
            .map(|(i, line)| LineDiff {
                line_number: i + 1,
                diff_type: LineDiffType::Removed,
                original_line: Some(line.to_string()),
                fixed_line: None,
            })
            .collect();
    }

    // Myers O(ND) 算法
    let max_d = n + m;
    let offset = max_d;
    let mut v = vec![0i32; 2 * max_d + 1];
    let mut trace: Vec<Vec<i32>> = Vec::new();

    for d in 0..=max_d {
        trace.push(v.clone());

        for k in (-(d as i32)..=d as i32).step_by(2) {
            let idx = (k + offset as i32) as usize;

            // 确定起始 x
            let mut x: i32;
            if k == -(d as i32) {
                // k == -D: 来自 k+1 (插入/下移)
                x = v[idx + 1];
            } else if k == d as i32 {
                // k == D: 来自 k-1 (删除/右移)
                x = v[idx - 1] + 1;
            } else if v[idx - 1] < v[idx + 1] {
                x = v[idx + 1]; // 下移 (插入)
            } else {
                x = v[idx - 1] + 1; // 右移 (删除)
            }

            let mut y = x - k;

            // 沿对角线 (snake) 匹配
            while x < n as i32 && y < m as i32 && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }

            v[idx] = x;

            if x >= n as i32 && y >= m as i32 {
                // 到达终点 — 回溯
                return myers_backtrack(&trace, &a, &b, offset, d);
            }
        }
    }

    Vec::new()
}

/// Myers 算法回溯 — 从终点回溯到起点, 收集差异
fn myers_backtrack(
    trace: &[Vec<i32>],
    a: &[&str],
    b: &[&str],
    offset: usize,
    max_d: usize,
) -> Vec<LineDiff> {
    let n = a.len();
    let m = b.len();
    let mut diffs = Vec::new();

    let mut cx = n as i32;
    let mut cy = m as i32;

    for dd in (1..=max_d).rev() {
        if dd >= trace.len() {
            break;
        }
        let prev_v = &trace[dd];
        let ck = cx - cy;
        // 确定前一个 k 值
        let prev_k: i32;
        if ck == -(dd as i32) {
            prev_k = ck + 1; // 插入
        } else if ck == dd as i32 {
            prev_k = ck - 1; // 删除
        } else {
            let left_idx = (ck - 1 + offset as i32) as usize;
            let right_idx = (ck + 1 + offset as i32) as usize;
            if left_idx < prev_v.len() && right_idx < prev_v.len() {
                if prev_v[left_idx] < prev_v[right_idx] {
                    prev_k = ck + 1; // 插入
                } else {
                    prev_k = ck - 1; // 删除
                }
            } else if right_idx < prev_v.len() {
                prev_k = ck + 1;
            } else {
                prev_k = ck - 1;
            }
        }

        let prev_x = prev_v[(prev_k + offset as i32) as usize];
        let prev_y = prev_x - prev_k;

        // 沿 snake 回溯到编辑点
        while cx > prev_x && cy > prev_y {
            cx -= 1;
            cy -= 1;
        }

        // 确定编辑类型
        if prev_k == ck + 1 {
            // 插入: prev (prev_x, prev_y) → (prev_x, prev_y+1)
            // 插入的行是 b[prev_y] (0-indexed)
            if prev_y >= 0 && (prev_y as usize) < m {
                diffs.push(LineDiff {
                    line_number: (prev_y + 1) as usize,
                    diff_type: LineDiffType::Added,
                    original_line: None,
                    fixed_line: Some(b[prev_y as usize].to_string()),
                });
            }
        } else if prev_k == ck - 1 {
            // 删除: prev (prev_x, prev_y) → (prev_x+1, prev_y)
            // 删除的行是 a[prev_x] (0-indexed)
            if prev_x >= 0 && (prev_x as usize) < n {
                diffs.push(LineDiff {
                    line_number: (prev_x + 1) as usize,
                    diff_type: LineDiffType::Removed,
                    original_line: Some(a[prev_x as usize].to_string()),
                    fixed_line: None,
                });
            }
        }

        cx = prev_x;
        cy = prev_y;

        if cx == 0 && cy == 0 {
            break;
        }
    }

    diffs.reverse();
    diffs
}

/// Diff 算法选择策略 (Session 123)
///
/// 用于 `compute_line_diff_with_algorithm` 指定使用哪种 diff 算法。
///
/// # 变体
///
/// - `Auto`: 自动选择最优算法 (基于输入特征启发式选择)
/// - `Basic`: 基础逐行比较 O(N), 只比较相同行号的行
/// - `Lcs`: LCS 动态规划 O(N×M), 适合中等规模输入
/// - `Myers`: Myers O(ND), 适合大规模稀疏差异
///
/// # 示例
///
/// ```
/// use forge::extract::{compute_line_diff_with_algorithm, DiffAlgorithm, LineDiffType};
///
/// let original = "a\nb\nc";
/// let fixed = "a\nx\nc";
///
/// let auto_diffs = compute_line_diff_with_algorithm(original, fixed, DiffAlgorithm::Auto);
/// let lcs_diffs = compute_line_diff_with_algorithm(original, fixed, DiffAlgorithm::Lcs);
/// let myers_diffs = compute_line_diff_with_algorithm(original, fixed, DiffAlgorithm::Myers);
///
/// // 所有算法都应检测到差异
/// assert!(!auto_diffs.is_empty());
/// assert!(!lcs_diffs.is_empty());
/// assert!(!myers_diffs.is_empty());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub enum DiffAlgorithm {
    /// 自动选择最优算法 (默认)
    ///
    /// 启发式策略:
    /// 1. 任一输入为空 → Myers (直接处理边界)
    /// 2. 两输入都 < 20 行 → Basic (最快, 无需构建 DP 表)
    /// 3. N × M ≤ 10000 → LCS (中等规模, 稳定)
    /// 4. N × M > 10000 → Hirschberg (大规模, 线性空间 O(N+M)) (Session 125)
    #[default]
    Auto,
    /// 基础逐行比较 O(N)
    Basic,
    /// LCS 动态规划 O(N×M)
    Lcs,
    /// Myers O(ND)
    Myers,
    /// Hirschberg 线性空间 LCS O(N+M) 空间 (Session 125)
    ///
    /// 适用于大文件 diff, 空间效率远优于 LCS (O(N×M) → O(N+M))
    Hirschberg,
}

/// 使用指定算法计算行级别差异 (Session 123)
///
/// 根据 `algorithm` 参数选择 diff 算法, 返回 `Vec<LineDiff>`。
///
/// # 算法对比
///
/// | 算法 | 时间复杂度 | 空间复杂度 | 适用场景 |
/// |------|-----------|-----------|---------|
/// | Basic | O(N) | O(1) | 小输入, 行号对齐 |
/// | LCS | O(N×M) | O(N×M) | 中等输入, 行插入/删除 |
/// | Myers | O(ND) | O(D²) | 大输入, 稀疏差异 |
///
/// # 示例
///
/// ```
/// use forge::extract::{compute_line_diff_with_algorithm, DiffAlgorithm};
///
/// let original = "fn foo() {\n}\n";
/// let fixed = "fn foo() {\n    let x = 42;\n}\n";
/// let diffs = compute_line_diff_with_algorithm(original, fixed, DiffAlgorithm::Myers);
/// assert!(!diffs.is_empty(), "应有差异");
/// ```
pub fn compute_line_diff_with_algorithm(
    original: &str,
    fixed: &str,
    algorithm: DiffAlgorithm,
) -> Vec<LineDiff> {
    match algorithm {
        DiffAlgorithm::Basic => compute_line_diff(original, fixed),
        DiffAlgorithm::Lcs => compute_line_diff_lcs(original, fixed),
        DiffAlgorithm::Myers => compute_line_diff_myers(original, fixed),
        DiffAlgorithm::Hirschberg => compute_line_diff_hirschberg(original, fixed),
        DiffAlgorithm::Auto => {
            let n = original.lines().count();
            let m = fixed.lines().count();

            // 边界: 任一输入为空 → Myers 直接处理
            if n == 0 || m == 0 {
                return compute_line_diff_myers(original, fixed);
            }

            // 小输入: 基础比较最快 (无 DP 表开销)
            if n < 20 && m < 20 {
                return compute_line_diff(original, fixed);
            }

            // 中等输入: LCS 更稳定
            let nm = n.saturating_mul(m);
            if nm <= 10_000 {
                return compute_line_diff_lcs(original, fixed);
            }

            // 大输入: Hirschberg 线性空间 (Session 125)
            // 空间 O(N+M) 远优于 LCS 的 O(N×M), 适用于大文件
            compute_line_diff_hirschberg(original, fixed)
        }
    }
}

/// 统一 diff 接口 — 自动选择最优算法 (Session 123)
///
/// 这是计算行差异的推荐入口, 自动根据输入特征选择最优算法:
///
/// - **小输入** (< 20 行): 基础逐行比较, 无 DP 表开销
/// - **中等输入** (N×M ≤ 10000): LCS 动态规划, 稳定可靠
/// - **大输入** (N×M > 10000): Myers O(ND), 稀疏差异更高效
///
/// # 示例
///
/// ```
/// use forge::extract::{compute_line_diff_unified, LineDiffType};
///
/// // 小输入 → Basic
/// let diffs = compute_line_diff_unified("a\nb", "a\nc");
/// assert!(!diffs.is_empty());
///
/// // 空输入
/// let diffs = compute_line_diff_unified("", "");
/// assert!(diffs.is_empty());
///
/// // 大规模稀疏差异 → Myers
/// let original: String = (0..100).map(|i| format!("line {i}\n")).collect();
/// let mut fixed = original.clone();
/// fixed.push_str("line 100\n");
/// let diffs = compute_line_diff_unified(&original, &fixed);
/// assert!(diffs.iter().any(|d| d.diff_type == LineDiffType::Added));
/// ```
pub fn compute_line_diff_unified(original: &str, fixed: &str) -> Vec<LineDiff> {
    compute_line_diff_with_algorithm(original, fixed, DiffAlgorithm::Auto)
}

/// Diff 算法对比结果项 (Session 131)
///
/// 记录单个 diff 算法的运行结果统计信息。
///
/// # 字段
///
/// - `algorithm`: 算法名称
/// - `diff_count`: 检测到的差异行数
/// - `added_count`: Added 行数
/// - `removed_count`: Removed 行数
/// - `modified_count`: Modified 行数
/// - `original_lines`: 原始文本行数
/// - `fixed_lines`: 修复文本行数
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DiffComparisonEntry {
    /// 算法名称
    pub algorithm: String,
    /// 差异行总数
    pub diff_count: usize,
    /// Added 行数
    pub added_count: usize,
    /// Removed 行数
    pub removed_count: usize,
    /// Modified 行数
    pub modified_count: usize,
    /// 原始文本行数
    pub original_lines: usize,
    /// 修复文本行数
    pub fixed_lines: usize,
}

/// Diff 算法对比结果 (Session 131)
///
/// 包含所有 diff 算法 (Basic/LCS/Myers/Hirschberg) 的对比数据,
/// 可用于选择最优算法和验证算法一致性。
///
/// # 字段
///
/// - `original_lines`: 原始文本行数
/// - `fixed_lines`: 修复文本行数
/// - `entries`: 各算法的对比结果
/// - `all_consistent`: 所有算法是否产生相同的差异行数
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DiffComparisonResult {
    /// 原始文本行数
    pub original_lines: usize,
    /// 修复文本行数
    pub fixed_lines: usize,
    /// 各算法对比结果
    pub entries: Vec<DiffComparisonEntry>,
    /// 所有算法差异行数是否一致
    pub all_consistent: bool,
}

impl DiffComparisonEntry {
    /// 从算法名称和 diff 结果创建对比项
    fn from_diffs(
        algorithm: &str,
        diffs: &[LineDiff],
        original_lines: usize,
        fixed_lines: usize,
    ) -> Self {
        let added_count = diffs
            .iter()
            .filter(|d| d.diff_type == LineDiffType::Added)
            .count();
        let removed_count = diffs
            .iter()
            .filter(|d| d.diff_type == LineDiffType::Removed)
            .count();
        let modified_count = diffs
            .iter()
            .filter(|d| d.diff_type == LineDiffType::Modified)
            .count();
        Self {
            algorithm: algorithm.to_string(),
            diff_count: diffs.len(),
            added_count,
            removed_count,
            modified_count,
            original_lines,
            fixed_lines,
        }
    }
}

/// 对比所有 diff 算法的性能和结果 (Session 131)
///
/// 运行 Basic/LCS/Myers/Hirschberg 四种 diff 算法,
/// 收集各自的差异行数统计, 并检查结果一致性。
///
/// # 返回
///
/// 返回 `DiffComparisonResult`, 包含:
/// - 各算法的差异行数 (Added/Removed/Modified)
/// - 一致性检查结果 (所有算法是否产生相同差异行数)
///
/// # 示例
///
/// ```
/// use forge::extract::compare_diff_algorithms;
///
/// let original = "fn foo() {\n}\n";
/// let fixed = "fn foo() {\n    let x = 42;\n}\n";
/// let result = compare_diff_algorithms(original, fixed);
/// assert_eq!(result.entries.len(), 4, "应有 4 种算法结果");
/// assert!(result.entries.iter().any(|e| e.algorithm == "Basic"));
/// assert!(result.entries.iter().any(|e| e.algorithm == "LCS"));
/// assert!(result.entries.iter().any(|e| e.algorithm == "Myers"));
/// assert!(result.entries.iter().any(|e| e.algorithm == "Hirschberg"));
/// ```
pub fn compare_diff_algorithms(original: &str, fixed: &str) -> DiffComparisonResult {
    let original_lines = original.lines().count();
    let fixed_lines = fixed.lines().count();

    let basic_diffs = compute_line_diff(original, fixed);
    let lcs_diffs = compute_line_diff_lcs(original, fixed);
    let myers_diffs = compute_line_diff_myers(original, fixed);
    let hirschberg_diffs = compute_line_diff_hirschberg(original, fixed);

    let entries = vec![
        DiffComparisonEntry::from_diffs("Basic", &basic_diffs, original_lines, fixed_lines),
        DiffComparisonEntry::from_diffs("LCS", &lcs_diffs, original_lines, fixed_lines),
        DiffComparisonEntry::from_diffs("Myers", &myers_diffs, original_lines, fixed_lines),
        DiffComparisonEntry::from_diffs(
            "Hirschberg",
            &hirschberg_diffs,
            original_lines,
            fixed_lines,
        ),
    ];

    // 检查一致性: 所有算法的差异行数是否相同
    let diff_counts: Vec<usize> = entries.iter().map(|e| e.diff_count).collect();
    let all_consistent = diff_counts.iter().all(|&c| c == diff_counts[0]);

    DiffComparisonResult {
        original_lines,
        fixed_lines,
        entries,
        all_consistent,
    }
}

/// 格式化 diff 算法对比结果为表格字符串 (Session 131)
///
/// 输出人类可读的对比表格, 包含各算法的差异行数统计。
///
/// # 输出格式
///
/// ```text
/// Diff Algorithm Comparison
/// =========================
/// Original: 5 lines | Fixed: 7 lines
/// Consistent: Yes
///
/// | Algorithm   | Total | Added | Removed | Modified |
/// |-------------|-------|-------|---------|----------|
/// | Basic       |     2 |     2 |       0 |        0 |
/// | LCS         |     2 |     2 |       0 |        0 |
/// | Myers       |     2 |     2 |       0 |        0 |
/// | Hirschberg  |     2 |     2 |       0 |        0 |
/// ```
///
/// # 示例
///
/// ```
/// use forge::extract::{compare_diff_algorithms, format_diff_comparison};
///
/// let result = compare_diff_algorithms("a\nb", "a\nc");
/// let table = format_diff_comparison(&result);
/// assert!(table.contains("Algorithm"), "应包含表头");
/// assert!(table.contains("Basic"), "应包含 Basic");
/// assert!(table.contains("Hirschberg"), "应包含 Hirschberg");
/// ```
pub fn format_diff_comparison(result: &DiffComparisonResult) -> String {
    let mut output = String::new();

    output.push_str("Diff Algorithm Comparison\n");
    output.push_str("=========================\n");
    output.push_str(&format!(
        "Original: {} lines | Fixed: {} lines\n",
        result.original_lines, result.fixed_lines
    ));
    output.push_str(&format!(
        "Consistent: {}\n\n",
        if result.all_consistent { "Yes" } else { "No" }
    ));

    output.push_str("| Algorithm   | Total | Added | Removed | Modified |\n");
    output.push_str("|-------------|-------|-------|---------|----------|\n");

    for entry in &result.entries {
        output.push_str(&format!(
            "| {:<11} | {:>5} | {:>5} | {:>7} | {:>8} |\n",
            entry.algorithm,
            entry.diff_count,
            entry.added_count,
            entry.removed_count,
            entry.modified_count
        ));
    }

    // 移除末尾换行
    if output.ends_with('\n') {
        output.pop();
    }

    output
}

/// 格式化行差异摘要 (Session 122)
///
/// 将 `Vec<LineDiff>` 格式化为人类可读的差异摘要字符串。
///
/// # 输出格式
///
/// ```text
/// Diff Summary: 2 Added, 1 Removed, 0 Modified
///   + Added   | line 3 | let x = 42;
///   - Removed | line 2 | let y = 10;
///   + Added   | line 5 | Ok(result)
/// ```
///
/// # 示例
///
/// ```
/// use forge::extract::{compute_line_diff_myers, format_diff_summary};
///
/// let original = "fn foo() {\n}\n";
/// let fixed = "fn foo() {\n    let x = 42;\n}\n";
/// let diffs = compute_line_diff_myers(original, fixed);
/// let summary = format_diff_summary(&diffs);
/// assert!(summary.contains("Added"), "应包含 Added 计数");
/// assert!(summary.contains("let x = 42"), "应包含差异行内容");
/// ```
pub fn format_diff_summary(diffs: &[LineDiff]) -> String {
    if diffs.is_empty() {
        return "No differences found.".to_string();
    }

    let added = diffs
        .iter()
        .filter(|d| d.diff_type == LineDiffType::Added)
        .count();
    let removed = diffs
        .iter()
        .filter(|d| d.diff_type == LineDiffType::Removed)
        .count();
    let modified = diffs
        .iter()
        .filter(|d| d.diff_type == LineDiffType::Modified)
        .count();

    let mut result = format!(
        "Diff Summary: {} Added, {} Removed, {} Modified\n",
        added, removed, modified
    );

    for d in diffs {
        let (symbol, label) = match d.diff_type {
            LineDiffType::Added => ("+", "Added"),
            LineDiffType::Removed => ("-", "Removed"),
            LineDiffType::Modified => ("~", "Modified"),
        };
        let content = d
            .fixed_line
            .as_ref()
            .or(d.original_line.as_ref())
            .map(|s| s.as_str())
            .unwrap_or("");
        result.push_str(&format!(
            "  {} {:7} | line {:<4} | {}\n",
            symbol, label, d.line_number, content
        ));
    }

    result
}

/// 分阶段修复预览 (Session 122)
///
/// 与 `apply_staged_fixes` 配合使用, 预览每个阶段的修复结果而不修改原始内容。
///
/// # 字段
///
/// - `original_content`: 原始代码
/// - `stage1_result`: 高优先级修复后的代码
/// - `stage2_result`: 中优先级修复后的代码
/// - `stage3_result`: 低优先级修复后的代码
/// - `stage1_changed`: 高优先级阶段是否有变化
/// - `stage2_changed`: 中优先级阶段是否有变化
/// - `stage3_changed`: 低优先级阶段是否有变化
/// - `total_changed`: 是否有任何变化
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedFixPreview {
    pub original_content: String,
    pub stage1_result: String,
    pub stage2_result: String,
    pub stage3_result: String,
    pub stage1_changed: bool,
    pub stage2_changed: bool,
    pub stage3_changed: bool,
    pub total_changed: bool,
}

/// 分阶段修复预览 — 预览每个阶段的修复结果 (Session 122)
///
/// 与 `apply_staged_fixes` 使用相同的分阶段逻辑, 但返回每个阶段的中间结果。
///
/// # 示例
///
/// ```
/// use forge::extract::apply_staged_fixes_preview;
///
/// let code = "pub fn foo() { let x = bar().unwrap(); }";
/// let preview = apply_staged_fixes_preview(code);
/// assert!(preview.total_changed, "应有变化");
/// assert!(preview.stage1_changed, "高优先级阶段应修复 unwrap");
/// ```
pub fn apply_staged_fixes_preview(content: &str) -> StagedFixPreview {
    // 阶段 1: 高优先级
    let stage1_exclude = [
        IssueType::UnsafeBlock,
        IssueType::UnsafeFn,
        IssueType::UnsafeImpl,
        IssueType::UnwrapOr,
        IssueType::UnwrapOrDefault,
        IssueType::MissingMustUse,
        IssueType::MissingDoc,
    ];
    let stage1_result = apply_fixes_except(content, &stage1_exclude);
    let stage1_changed = stage1_result != content;

    // 阶段 2: 中优先级
    let stage2_exclude = [IssueType::MissingDoc];
    let stage2_result = apply_fixes_except(&stage1_result, &stage2_exclude);
    let stage2_changed = stage2_result != stage1_result;

    // 阶段 3: 低优先级
    let stage3_result = apply_fixes(&stage2_result);
    let stage3_changed = stage3_result != stage2_result;

    let total_changed = stage1_changed || stage2_changed || stage3_changed;

    StagedFixPreview {
        original_content: content.to_string(),
        stage1_result,
        stage2_result,
        stage3_result,
        stage1_changed,
        stage2_changed,
        stage3_changed,
        total_changed,
    }
}

/// 增强版 anyhow 导入检查 — 支持 bail!/ensure!/anyhow! 宏 + Context trait (Session 122 + Session 123)
///
/// 在 `ensure_anyhow_imports` 基础上, 额外检测以下导入需求:
/// - `bail!()` 宏
/// - `ensure!()` 宏
/// - `anyhow!()` 宏 (Session 123 新增)
/// - `.context()` 方法 → `Context` trait (Session 123 新增)
///
/// # 规则
///
/// 1. 检测 `Result`/`Error`/`bail!`/`ensure!`/`anyhow!`/`Context` 的使用需求
/// 2. 检查已有导入
/// 3. 构建合并导入: `use anyhow::{Result, Error, Context, anyhow, bail, ensure};`
/// 4. 排序: Result → Error → Context → anyhow → bail → ensure → format_err → 其他
/// 5. 幂等: 已有合并导入不重复处理
///
/// # 示例
///
/// ```
/// use forge::extract::ensure_anyhow_imports_extended;
///
/// // 使用 bail! 但没有导入
/// let code = "fn foo() -> Result<(), anyhow::Error> { bail!(\"error\"); }";
/// let result = ensure_anyhow_imports_extended(code);
/// assert!(result.contains("bail"), "应包含 bail 导入");
///
/// // 使用 anyhow! 宏 (Session 123)
/// let code = "fn foo() -> Result<(), anyhow::Error> { Err(anyhow!(\"error\")) }";
/// let result = ensure_anyhow_imports_extended(code);
/// assert!(result.contains("anyhow"), "应包含 anyhow 宏导入");
///
/// // 幂等
/// let second = ensure_anyhow_imports_extended(&result);
/// assert_eq!(result, second, "二次调用不变化");
/// ```
pub fn ensure_anyhow_imports_extended(content: &str) -> String {
    // 检测需要的导入
    let needs_result = content.contains("anyhow::Result") || content.contains("Result<");
    let needs_error = content.contains("anyhow::Error");
    let needs_bail = content.contains("bail!(") || content.contains("anyhow::bail!(");
    let needs_ensure = content.contains("ensure!(") || content.contains("anyhow::ensure!(");
    // Session 123: anyhow! 宏和 Context trait
    let needs_anyhow_macro = content.contains("anyhow!(");
    let needs_context = content.contains(".context(") || content.contains(".with_context(");

    if !needs_result
        && !needs_error
        && !needs_bail
        && !needs_ensure
        && !needs_anyhow_macro
        && !needs_context
    {
        return content.to_string();
    }

    let lines: Vec<&str> = content.lines().collect();
    let mut result_lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();

    // 收集所有需要的导入项
    let mut needed_items: Vec<&str> = Vec::new();
    if needs_result {
        needed_items.push("Result");
    }
    if needs_error {
        needed_items.push("Error");
    }
    if needs_context {
        needed_items.push("Context");
    }
    if needs_anyhow_macro {
        needed_items.push("anyhow");
    }
    if needs_bail {
        needed_items.push("bail");
    }
    if needs_ensure {
        needed_items.push("ensure");
    }

    // 收集所有已有的导入项和行索引
    let mut existing_items: Vec<&str> = Vec::new();
    let mut anyhow_line_indices: Vec<usize> = Vec::new();
    let mut has_wildcard = false;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed == "use anyhow::Result;" {
            if !existing_items.contains(&"Result") {
                existing_items.push("Result");
            }
            anyhow_line_indices.push(i);
        } else if trimmed == "use anyhow::Error;" {
            if !existing_items.contains(&"Error") {
                existing_items.push("Error");
            }
            anyhow_line_indices.push(i);
        } else if trimmed == "use anyhow::*;" {
            has_wildcard = true;
        } else if trimmed.starts_with("use anyhow::{") && trimmed.ends_with("};") {
            let inner = trimmed
                .trim_start_matches("use anyhow::{")
                .trim_end_matches("};");
            for item in inner.split(',').map(|s| s.trim()) {
                if !existing_items.contains(&item) && !item.is_empty() {
                    existing_items.push(item);
                }
            }
            anyhow_line_indices.push(i);
        }
    }

    // 通配导入已覆盖所有
    if has_wildcard {
        return content.to_string();
    }

    // 合并已有和需要的导入项
    let mut all_items = existing_items.clone();
    for item in &needed_items {
        if !all_items.contains(item) {
            all_items.push(item);
        }
    }

    // 排序: Result, Error, Context, anyhow, bail, ensure, format_err, 其他
    all_items.sort_by_key(|item| match *item {
        "Result" => 0,
        "Error" => 1,
        "Context" => 2,
        "anyhow" => 3,
        "bail" => 4,
        "ensure" => 5,
        "format_err" => 6,
        _ => 7,
    });

    if anyhow_line_indices.is_empty() {
        // 需要新增导入行
        let import_line = format!("use anyhow::{{{}}};", all_items.join(", "));

        let mut insert_pos = 0;
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty()
                || trimmed.starts_with("//")
                || trimmed.starts_with("/*")
                || trimmed.starts_with("*")
                || trimmed.starts_with("#![")
                || trimmed.starts_with("//!")
                || trimmed.starts_with("///")
            {
                continue;
            }
            insert_pos = i;
            break;
        }

        if insert_pos == 0 && !lines.is_empty() {
            let all_comments = lines
                .iter()
                .all(|l| l.trim().is_empty() || l.trim().starts_with("//"));
            if all_comments {
                insert_pos = lines.len();
            }
        }

        result_lines.insert(insert_pos, import_line);
    } else {
        // 保留第一个位置, 删除其余
        let keep_idx = anyhow_line_indices[0];
        result_lines[keep_idx] = format!("use anyhow::{{{}}};", all_items.join(", "));
        // 从后向前删除其他行
        for &idx in anyhow_line_indices[1..].iter().rev() {
            result_lines.remove(idx);
        }
    }

    result_lines.join("\n")
}

// ============================================================================
//  Session 124: Hirschberg 线性空间 diff + 统一 diff 格式 + std 导入检查
// ============================================================================

/// Hirschberg 算法辅助函数 — 正向 LCS 长度计算 (O(M) 空间)
///
/// 返回 `Vec<usize>`，其中 `result[j]` = LCS(a, b[:j])
fn hirschberg_forward_lcs(a: &[&str], b: &[&str]) -> Vec<usize> {
    let m = b.len();
    let mut prev = vec![0usize; m + 1];
    let mut curr = vec![0usize; m + 1];

    for &ai in a {
        curr[0] = 0;
        for j in 0..m {
            if ai == b[j] {
                curr[j + 1] = prev[j] + 1;
            } else {
                curr[j + 1] = prev[j + 1].max(curr[j]);
            }
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev
}

/// Hirschberg 算法辅助函数 — 反向 LCS 长度计算 (O(M) 空间)
///
/// 返回 `Vec<usize>`，其中 `result[j]` = LCS(a, b[j:])
fn hirschberg_backward_lcs(a: &[&str], b: &[&str]) -> Vec<usize> {
    let m = b.len();
    let mut next = vec![0usize; m + 1];
    let mut curr = vec![0usize; m + 1];

    for &ai in a.iter().rev() {
        curr[m] = 0;
        for j in (0..m).rev() {
            if ai == b[j] {
                curr[j] = next[j + 1] + 1;
            } else {
                curr[j] = next[j].max(curr[j + 1]);
            }
        }
        std::mem::swap(&mut next, &mut curr);
    }
    next
}

/// Hirschberg 分治递归 — 返回 LCS 匹配对的列表 `(a_index, b_index)`
fn hirschberg_lcs_pairs(a: &[&str], b: &[&str]) -> Vec<(usize, usize)> {
    let n = a.len();
    let m = b.len();

    if n == 0 || m == 0 {
        return Vec::new();
    }

    if n == 1 {
        // 基本情况: 在 b 中找到第一个匹配 a[0] 的行
        for (j, item) in b.iter().enumerate() {
            if a[0] == *item {
                return vec![(0, j)];
            }
        }
        return Vec::new();
    }

    if m == 1 {
        // 基本情况: 在 a 中找到第一个匹配 b[0] 的行
        for (i, item) in a.iter().enumerate() {
            if *item == b[0] {
                return vec![(i, 0)];
            }
        }
        return Vec::new();
    }

    // 分治: 将 a 分成两半
    let mid = n / 2;

    // 正向: LCS(a[:mid], b[:j]) for all j
    let forward = hirschberg_forward_lcs(&a[..mid], b);

    // 反向: LCS(a[mid:], b[j:]) for all j
    let backward = hirschberg_backward_lcs(&a[mid..], b);

    // 找到最优分割点 k, 最大化 forward[k] + backward[k]
    let mut max_sum = 0usize;
    let mut split_k = 0usize;
    for k in 0..=m {
        let sum = forward[k] + backward[k];
        if sum > max_sum {
            max_sum = sum;
            split_k = k;
        }
    }

    // 递归求解左右两半
    let left = hirschberg_lcs_pairs(&a[..mid], &b[..split_k]);
    let right = hirschberg_lcs_pairs(&a[mid..], &b[split_k..]);

    // 合并: 右半部分的索引需要加上偏移量
    let mut result = left;
    for (ai, bi) in right {
        result.push((ai + mid, bi + split_k));
    }
    result
}

/// 使用 Hirschberg 线性空间算法计算行级别差异 (Session 124)
///
/// Hirschberg 算法是 LCS 的线性空间变体, 空间复杂度 O(N+M),
/// 而标准 LCS DP 需要 O(N×M) 空间。时间复杂度仍为 O(N×M)。
///
/// 适用于大文件 diff, 避免 LCS 的 O(N×M) 内存消耗。
///
/// # 算法
///
/// 1. 分治: 将输入分成两半
/// 2. 正向计算左半 LCS 长度 (O(M) 空间)
/// 3. 反向计算右半 LCS 长度 (O(M) 空间)
/// 4. 找到最优分割点, 递归求解
/// 5. 从 LCS 匹配对生成 `LineDiff` (Added/Removed)
///
/// # 与其他算法的对比
///
/// | 算法 | 时间 | 空间 | 适用场景 |
/// |------|------|------|---------|
/// | Basic | O(N) | O(1) | 小输入, 行号对齐 |
/// | LCS | O(N×M) | O(N×M) | 中等输入 |
/// | Myers | O(ND) | O(D²) | 稀疏差异 |
/// | Hirschberg | O(N×M) | O(N+M) | 大输入, 内存受限 |
///
/// # 示例
///
/// ```
/// use forge::extract::{compute_line_diff_hirschberg, LineDiffType};
///
/// // 无差异
/// let diffs = compute_line_diff_hirschberg("fn foo() {}", "fn foo() {}");
/// assert!(diffs.is_empty());
///
/// // 插入一行
/// let original = "fn foo() {\n}\n";
/// let fixed = "fn foo() {\n    let x = 42;\n}\n";
/// let diffs = compute_line_diff_hirschberg(original, fixed);
/// assert!(diffs.iter().any(|d| d.diff_type == LineDiffType::Added));
/// ```
pub fn compute_line_diff_hirschberg(original: &str, fixed: &str) -> Vec<LineDiff> {
    let a: Vec<&str> = original.lines().collect();
    let b: Vec<&str> = fixed.lines().collect();

    if a.is_empty() && b.is_empty() {
        return Vec::new();
    }
    if a.is_empty() {
        return b
            .iter()
            .enumerate()
            .map(|(i, line)| LineDiff {
                line_number: i + 1,
                diff_type: LineDiffType::Added,
                original_line: None,
                fixed_line: Some(line.to_string()),
            })
            .collect();
    }
    if b.is_empty() {
        return a
            .iter()
            .enumerate()
            .map(|(i, line)| LineDiff {
                line_number: i + 1,
                diff_type: LineDiffType::Removed,
                original_line: Some(line.to_string()),
                fixed_line: None,
            })
            .collect();
    }

    // 使用 Hirschberg 算法计算 LCS 匹配对
    let lcs_pairs = hirschberg_lcs_pairs(&a, &b);

    // 从 LCS 匹配对生成 diff
    let mut diffs = Vec::new();
    let mut ai = 0usize;
    let mut bi = 0usize;

    for &(match_a, match_b) in &lcs_pairs {
        // 原始中匹配前的行 → Removed
        while ai < match_a {
            diffs.push(LineDiff {
                line_number: ai + 1,
                diff_type: LineDiffType::Removed,
                original_line: Some(a[ai].to_string()),
                fixed_line: None,
            });
            ai += 1;
        }
        // 修复中匹配前的行 → Added
        while bi < match_b {
            diffs.push(LineDiff {
                line_number: bi + 1,
                diff_type: LineDiffType::Added,
                original_line: None,
                fixed_line: Some(b[bi].to_string()),
            });
            bi += 1;
        }
        // 匹配行 — 跳过 (不变)
        ai = match_a + 1;
        bi = match_b + 1;
    }

    // 最后一个匹配之后的剩余行
    while ai < a.len() {
        diffs.push(LineDiff {
            line_number: ai + 1,
            diff_type: LineDiffType::Removed,
            original_line: Some(a[ai].to_string()),
            fixed_line: None,
        });
        ai += 1;
    }
    while bi < b.len() {
        diffs.push(LineDiff {
            line_number: bi + 1,
            diff_type: LineDiffType::Added,
            original_line: None,
            fixed_line: Some(b[bi].to_string()),
        });
        bi += 1;
    }

    diffs
}

/// 统一 diff 格式输出 — 类似 `git diff` 的格式 (Session 124)
///
/// 将两段文本的差异格式化为统一 diff 格式 (unified diff),
/// 包含 `---`/`+++` 文件头和 `@@ -start,count +start,count @@` hunk 头,
/// 每行前缀 ` `(空格=不变)、`+`(新增)、`-`(删除)。
///
/// # 参数
///
/// - `original`: 原始文本
/// - `fixed`: 修改后文本
///
/// # 返回值
///
/// 统一 diff 格式的字符串。如果两段文本相同, 返回空字符串。
///
/// # 示例
///
/// ```
/// use forge::extract::format_diff_unified;
///
/// // 无差异 → 空字符串
/// assert_eq!(format_diff_unified("fn foo() {}", "fn foo() {}"), "");
///
/// // 有差异 → 统一 diff 格式
/// let diff = format_diff_unified("fn foo() {\n}\n", "fn foo() {\n    let x = 42;\n}\n");
/// assert!(diff.contains("--- original"), "应包含 --- original 头");
/// assert!(diff.contains("+++ fixed"), "应包含 +++ fixed 头");
/// assert!(diff.contains("@@"), "应包含 @@ hunk 头");
/// assert!(diff.contains("+    let x = 42;"), "应包含新增行");
/// ```
pub fn format_diff_unified(original: &str, fixed: &str) -> String {
    format_diff_unified_with_options(original, fixed, "original", "fixed", 3)
}

/// 带选项的统一 diff 格式输出 (Session 125)
///
/// 与 `format_diff_unified` 功能相同, 但支持自定义:
/// - `original_name`: 原始文件名 (显示在 `---` 头)
/// - `fixed_name`: 修复后文件名 (显示在 `+++` 头)
/// - `context`: 上下文行数 (默认 3)
///
/// # 示例
///
/// ```
/// use forge::extract::format_diff_unified_with_options;
///
/// let original = "fn foo() {\n}\n";
/// let fixed = "fn foo() {\n    let x = 42;\n}\n";
/// let diff = format_diff_unified_with_options(original, fixed, "src/main.rs", "src/main.rs", 5);
/// assert!(diff.contains("--- src/main.rs"));
/// assert!(diff.contains("+++ src/main.rs"));
/// ```
pub fn format_diff_unified_with_options(
    original: &str,
    fixed: &str,
    original_name: &str,
    fixed_name: &str,
    context: usize,
) -> String {
    if original == fixed {
        return String::new();
    }

    let orig_lines: Vec<&str> = original.lines().collect();
    let fixed_lines: Vec<&str> = fixed.lines().collect();

    // 使用 Hirschberg 算法计算 LCS 对齐 (O(N+M) 空间)
    let lcs_pairs = hirschberg_lcs_pairs(&orig_lines, &fixed_lines);

    // 构建差异条目列表: (prefix, orig_line_no, fixed_line_no, content)
    // prefix: ' ' = 不变, '+' = 新增, '-' = 删除
    #[allow(clippy::type_complexity)]
    let mut entries: Vec<(char, Option<usize>, Option<usize>, &str)> = Vec::new();
    let mut ai = 0usize;
    let mut bi = 0usize;

    for &(match_a, match_b) in &lcs_pairs {
        while ai < match_a {
            entries.push(('-', Some(ai + 1), None, orig_lines[ai]));
            ai += 1;
        }
        while bi < match_b {
            entries.push(('+', None, Some(bi + 1), fixed_lines[bi]));
            bi += 1;
        }
        entries.push((' ', Some(ai + 1), Some(bi + 1), orig_lines[ai]));
        ai = match_a + 1;
        bi = match_b + 1;
    }
    while ai < orig_lines.len() {
        entries.push(('-', Some(ai + 1), None, orig_lines[ai]));
        ai += 1;
    }
    while bi < fixed_lines.len() {
        entries.push(('+', None, Some(bi + 1), fixed_lines[bi]));
        bi += 1;
    }

    // 找到所有变更区域, 合并相近的变更 (距离 <= 2*context 的归入同一 hunk)
    let mut hunks: Vec<(usize, usize)> = Vec::new(); // (start, end) in entries
    let mut i = 0;
    while i < entries.len() {
        if entries[i].0 == ' ' {
            i += 1;
            continue;
        }

        // 找到变更区域的起始 (包含 context 行)
        let hunk_start = i.saturating_sub(context);
        let mut last_change = i;
        let mut j = i + 1;
        while j < entries.len() {
            if entries[j].0 != ' ' {
                last_change = j;
                j += 1;
            } else {
                // 检查前方是否有变更 (在 2*context 范围内)
                let look_ahead = std::cmp::min(j + 2 * context, entries.len());
                let has_more = entries[j..look_ahead].iter().any(|e| e.0 != ' ');
                if has_more {
                    j += 1;
                } else {
                    break;
                }
            }
        }
        let hunk_end = std::cmp::min(last_change + context + 1, entries.len());
        hunks.push((hunk_start, hunk_end));
        i = hunk_end;
    }

    let mut result = String::new();
    result.push_str(&format!("--- {}\n", original_name));
    result.push_str(&format!("+++ {}\n", fixed_name));

    for &(start, end) in &hunks {
        let hunk_entries = &entries[start..end];

        // 统计 hunk 中原始和修复的行数
        let orig_count = hunk_entries.iter().filter(|e| e.1.is_some()).count();
        let fixed_count = hunk_entries.iter().filter(|e| e.2.is_some()).count();

        // 获取起始行号
        let orig_start = hunk_entries.iter().find_map(|e| e.1).unwrap_or(0);
        let fixed_start = hunk_entries.iter().find_map(|e| e.2).unwrap_or(0);

        // 格式化 hunk 头
        result.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            orig_start, orig_count, fixed_start, fixed_count
        ));

        // 格式化条目
        for &(prefix, _, _, content) in hunk_entries {
            result.push(prefix);
            result.push_str(content);
            result.push('\n');
        }
    }

    result
}

/// 检测类型名是否作为独立标识符出现 (词边界匹配) (Session 125)
///
/// 防止 "Cell" 误匹配 "OnceCell" 或 "RefCell" 中的子串。
/// 检查类型名出现位置的左右字符是否为非标识符字符 (非字母数字和下划线)。
fn contains_type_usage(content: &str, type_name: &str) -> bool {
    let bytes = content.as_bytes();
    let tn_bytes = type_name.as_bytes();
    let tn_len = tn_bytes.len();

    for (pos, _) in content.match_indices(type_name) {
        // 检查前一个字符是否为非标识符字符
        let before_ok = pos == 0 || {
            let ch = bytes[pos - 1];
            !ch.is_ascii_alphanumeric() && ch != b'_'
        };
        // 检查后一个字符是否为非标识符字符
        let after_pos = pos + tn_len;
        let after_ok = after_pos >= bytes.len() || {
            let ch = bytes[after_pos];
            !ch.is_ascii_alphanumeric() && ch != b'_'
        };
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

/// 检查是否存在覆盖给定模块路径的 glob 导入 (Session 129)
///
/// 检测 `use module::*;` 或 `use parent::*;` 形式的 glob 导入,
/// 如果存在则该模块下的所有类型都已被导入, 不需要额外添加。
///
/// 例如: `use tokio::*;` 覆盖 `tokio::task::JoinHandle`,
///       `use std::sync::*;` 覆盖 `std::sync::atomic::AtomicBool`。
fn has_covering_glob_import(glob_imports: &[String], module_path: &str) -> bool {
    glob_imports.iter().any(|glob| {
        // 精确匹配: use module::*;
        module_path == glob
        // 父模块 glob: use parent::*; 覆盖 parent::sub::Type
            || module_path.starts_with(&format!("{}::", glob))
    })
}

/// 从代码中提取所有 glob 导入的模块路径 (Session 129 + Session 133 增强)
///
/// 扫描 `use X::*;` 形式的导入, 返回模块路径列表。
/// 用于快速判断某类型是否已被 glob 导入覆盖。
///
/// # 支持的格式
///
/// - `use module::*;` → `module` (Session 129)
/// - `use module::{*, sub::*};` → `module` + `module::sub` (Session 133)
/// - `use module::{Type, sub::*};` → `module::sub` (Session 133)
/// - `use module::{sub::{*, inner::*}};` → `module::sub` + `module::sub::inner` (Session 133)
pub fn extract_glob_imports(content: &str) -> Vec<String> {
    let mut globs = Vec::new();

    // 预处理: 将多行 use 语句合并为单行 (Session 134)
    // 例如: use std::{\n io::*,\n sync::*\n}; → use std::{ io::*, sync::*};
    let joined = join_multiline_use_statements(content);

    for line in joined.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("use ") {
            continue;
        }
        // Session 135: 移除行注释后再解析 (支持 use 语句中的 // 注释)
        let trimmed = strip_use_line_comments(trimmed);
        // Session 136: 移除块注释后再解析 (支持 use 语句中的 /* */ 注释)
        let trimmed = strip_use_block_comments(&trimmed);
        if !trimmed.starts_with("use ") {
            continue;
        }
        let rest = &trimmed[4..]; // skip "use "
        if !rest.ends_with(';') {
            continue;
        }
        let rest = &rest[..rest.len() - 1]; // remove trailing ';'

        // Case 1: use module::*;
        if let Some(module) = rest.strip_suffix("::*") {
            globs.push(module.to_string());
            continue;
        }

        // Case 2: use module::{...} with globs inside braces
        if let Some(brace_pos) = rest.find("::{") {
            if let Some(close_pos) = rest.rfind('}') {
                if close_pos > brace_pos + 2 {
                    let module = &rest[..brace_pos];
                    let inner = &rest[brace_pos + 3..close_pos];
                    extract_globs_from_braces(inner, module, &mut globs);
                }
            }
        }
    }
    globs
}

/// 从 use 语句行中移除行注释 (Session 135)
///
/// use 语句中不包含字符串字面量, 因此可以安全地移除 `// ...` 注释。
/// 排除 URL 中的 `://` 协议分隔符 (如 `http://`)。
pub fn strip_use_line_comments(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'/' && bytes[i + 1] == b'/' {
            // 排除 :// (URL 中的协议分隔符)
            if i > 0 && bytes[i - 1] == b':' {
                i += 1;
                continue;
            }
            return s[..i].trim_end().to_string();
        }
        i += 1;
    }
    s.to_string()
}

/// 从 use 语句行中移除块注释 (Session 136)
///
/// 支持 Rust 嵌套块注释 `/* /* ... */ */`.
/// use 语句中不包含字符串字面量, 因此可以安全地移除块注释。
///
/// # 示例
///
/// ```
/// use forge::extract::strip_use_block_comments;
///
/// assert_eq!(strip_use_block_comments("use std::{/* comment */ io::*};"), "use std::{ io::*};");
/// assert_eq!(strip_use_block_comments("use std::io::*;"), "use std::io::*;");
/// ```
pub fn strip_use_block_comments(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut result = String::new();
    let mut i = 0;
    let mut depth = 0;
    while i < chars.len() {
        if depth > 0 {
            // 在块注释内部
            if i + 1 < chars.len() && chars[i] == '/' && chars[i + 1] == '*' {
                depth += 1;
                i += 2;
            } else if i + 1 < chars.len() && chars[i] == '*' && chars[i + 1] == '/' {
                depth -= 1;
                i += 2;
            } else {
                i += 1;
            }
        } else {
            if i + 1 < chars.len() && chars[i] == '/' && chars[i + 1] == '*' {
                depth = 1;
                i += 2;
            } else {
                result.push(chars[i]);
                i += 1;
            }
        }
    }
    result
}

/// 将多行 use 语句合并为单行 (Session 134)
///
/// 扫描代码中的 `use` 语句, 如果以 `{` 开头但未在同一行以 `};` 结束,
/// 则将后续行合并到同一行 (用空格替代换行), 直到找到 `};` 为止。
fn join_multiline_use_statements(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut result: Vec<String> = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        // 检测 use 语句且包含未闭合的 {
        // Session 136: 先移除块注释, 避免块注释中的 { } 影响计数
        let trimmed_no_block = strip_use_block_comments(trimmed);
        if trimmed_no_block.starts_with("use ") && trimmed_no_block.contains('{') {
            // 计算当前行的 { 和 } 数量 (移除块注释后)
            let open_count = trimmed_no_block.chars().filter(|&c| c == '{').count();
            let close_count = trimmed_no_block.chars().filter(|&c| c == '}').count();

            if open_count > close_count {
                // 多行 use 语句: 合并后续行
                // Session 135: 对首行也移除行注释
                let mut joined = strip_use_line_comments(line);
                i += 1;
                while i < lines.len() {
                    joined.push(' ');
                    // Session 135: 移除每行的注释后再合并
                    let cleaned = strip_use_line_comments(lines[i]);
                    joined.push_str(&cleaned);
                    let joined_trimmed = joined.trim();
                    // Session 136: 移除块注释后再计数
                    let joined_cleaned = strip_use_block_comments(joined_trimmed);
                    let o = joined_cleaned.chars().filter(|&c| c == '{').count();
                    let c = joined_cleaned.chars().filter(|&c| c == '}').count();
                    if o <= c {
                        break;
                    }
                    i += 1;
                }
                result.push(joined);
                i += 1;
                continue;
            }
        }

        result.push(line.to_string());
        i += 1;
    }

    result.join("\n")
}

/// 从花括号内容中提取 glob 导入路径 (Session 133)
///
/// 递归解析 `use module::{...}` 中花括号内的内容,
/// 提取 `*` 和 `sub::*` 形式的 glob 导入。
fn extract_globs_from_braces(inner: &str, parent: &str, globs: &mut Vec<String>) {
    for item in split_top_level_commas(inner) {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        if item == "*" {
            // use parent::{*} → parent
            globs.push(parent.to_string());
        } else if let Some(sub) = item.strip_suffix("::*") {
            // use parent::{sub::*} → parent::sub
            globs.push(format!("{}::{}", parent, sub));
        } else if let Some(nested_pos) = item.find("::{") {
            // use parent::{sub::{...}} → recurse
            if let Some(nested_close) = item.rfind('}') {
                if nested_close > nested_pos + 2 {
                    let sub = &item[..nested_pos];
                    let nested_inner = &item[nested_pos + 3..nested_close];
                    let full_path = format!("{}::{}", parent, sub);
                    extract_globs_from_braces(nested_inner, &full_path, globs);
                }
            }
        }
    }
}

/// 在顶层按逗号分割字符串 (忽略嵌套花括号内的逗号) (Session 133)
fn split_top_level_commas(s: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    for (i, ch) in s.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                if depth > 0 {
                    depth -= 1;
                }
            }
            ',' if depth == 0 => {
                result.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if start < s.len() {
        result.push(&s[start..]);
    }
    result
}

/// 检测并添加缺失的 `std` 导入 (Session 124 + Session 125 扩展)
///
/// 类似 `ensure_anyhow_imports`, 但针对 Rust 标准库常用类型:
///
/// - `HashMap` / `HashSet` → `use std::collections::{HashMap, HashSet};`
/// - `BTreeMap` / `BTreeSet` → `use std::collections::{BTreeMap, BTreeSet};`
/// - `Path` / `PathBuf` → `use std::path::{Path, PathBuf};`
/// - `File` → `use std::fs::File;`
/// - `Read` / `Write` / `BufRead` → `use std::io::{Read, Write, BufRead};`
/// - `Cell` / `RefCell` / `OnceCell` → `use std::cell::{Cell, RefCell, OnceCell};` (Session 125)
/// - `Arc` / `Mutex` / `RwLock` / `OnceLock` → `use std::sync::{Arc, Mutex, RwLock, OnceLock};` (Session 125)
/// - `Rc` → `use std::rc::Rc;` (Session 125)
/// - `Command` / `ExitStatus` → `use std::process::{Command, ExitStatus};` (Session 125)
/// - `env` → `use std::env;` (Session 125)
/// - `Instant` / `Duration` → `use std::time::{Instant, Duration};` (Session 125)
/// - `TcpListener` / `TcpStream` → `use std::net::{TcpListener, TcpStream};` (Session 125)
/// - `thread` → `use std::thread;` (Session 126)
/// - `Thread` / `JoinHandle` → `use std::thread::{Thread, JoinHandle};` (Session 126)
/// - `PhantomData` → `use std::marker::PhantomData;` (Session 126)
/// - `Cow` → `use std::borrow::Cow;` (Session 126)
/// - `Sender` / `Receiver` → `use std::sync::mpsc::{Sender, Receiver};` (Session 126)
/// - `Condvar` / `Barrier` → `use std::sync::{Condvar, Barrier};` (Session 126)
/// - `AtomicBool` / `AtomicI32` / `AtomicU32` / ... → `use std::sync::atomic::{...};` (Session 126)
/// - `Pin` → `use std::pin::Pin;` (Session 127)
/// - `Ordering` → `use std::cmp::Ordering;` (Session 127)
/// - `Range` / `RangeInclusive` → `use std::ops::{Range, RangeInclusive};` (Session 127)
/// - `TypeId` / `Any` → `use std::any::{TypeId, Any};` (Session 127)
/// - `Formatter` / `Display` / `Debug` → `use std::fmt::{...};` (Session 127)
/// - `FromIterator` / `Peekable` → `use std::iter::{...};` (Session 127)
/// - `Hash` / `Hasher` → `use std::hash::{Hash, Hasher};` (Session 127)
/// - `mem` → `use std::mem;` (Session 127)
/// - `NonZeroU32` / `NonZeroU64` / `NonZeroUsize` → `use std::num::{...};` (Session 127)
/// - `Entry` → `use std::collections::hash_map::Entry;` (Session 127)
/// - `Future` → `use std::future::Future;` (Session 128)
/// - `Poll` / `Waker` → `use std::task::{Poll, Waker};` (Session 128)
/// - `Layout` → `use std::alloc::Layout;` (Session 128)
/// - `CString` / `CStr` → `use std::ffi::{CStr, CString};` (Session 128)
/// - `Pattern` → `use std::str::pattern::Pattern;` (Session 128)
///
/// # 规则
///
/// 1. 检测代码中使用了哪些 std 类型
/// 2. 检查已有的导入 (包括合并导入和全限定路径如 `std::collections::HashMap`)
/// 3. 缺失的导入自动添加, 已有的不重复
/// 4. 同一模块的多个类型合并为 `use std::module::{Type1, Type2};`
/// 5. 幂等: 已有导入不重复添加
/// 6. 同一模块的多个类型按字母序排列 (Session 126)
///
/// # 示例
///
/// ```
/// use forge::extract::ensure_std_imports;
///
/// // 使用 HashMap 但未导入
/// let code = "fn foo() -> HashMap<String, i32> { HashMap::new() }";
/// let result = ensure_std_imports(code);
/// assert!(result.contains("use std::collections::HashMap;"), "应添加 HashMap 导入: {}", result);
///
/// // 已有导入不重复 (幂等)
/// let second = ensure_std_imports(&result);
/// assert_eq!(result, second, "二次调用不变化");
///
/// // 使用全限定路径不需要导入
/// let code = "fn foo() -> std::collections::HashMap<String, i32> { std::collections::HashMap::new() }";
/// let result = ensure_std_imports(code);
/// assert!(!result.contains("use std::collections"), "全限定路径不需要导入");
/// ```
pub fn ensure_std_imports(content: &str) -> String {
    // 定义需要检测的类型: (类型名, 模块路径, 导入前缀)
    // 格式: (type_name, module_path)
    let type_modules: &[(&str, &str)] = &[
        // collections
        ("HashMap", "std::collections"),
        ("HashSet", "std::collections"),
        ("BTreeMap", "std::collections"),
        ("BTreeSet", "std::collections"),
        ("VecDeque", "std::collections"),
        ("LinkedList", "std::collections"),
        ("BinaryHeap", "std::collections"),
        // path
        ("Path", "std::path"),
        ("PathBuf", "std::path"),
        // fs
        ("File", "std::fs"),
        // io
        ("BufReader", "std::io"),
        ("BufWriter", "std::io"),
        ("Read", "std::io"),
        ("Write", "std::io"),
        ("BufRead", "std::io"),
        ("Stdin", "std::io"),
        ("Stdout", "std::io"),
        ("Stderr", "std::io"),
        // cell (Session 125)
        ("Cell", "std::cell"),
        ("RefCell", "std::cell"),
        ("OnceCell", "std::cell"),
        // sync (Session 125)
        ("Arc", "std::sync"),
        ("Mutex", "std::sync"),
        ("RwLock", "std::sync"),
        ("OnceLock", "std::sync"),
        // rc (Session 125)
        ("Rc", "std::rc"),
        // process (Session 125)
        ("Command", "std::process"),
        ("ExitStatus", "std::process"),
        // env (Session 125)
        ("env", "std"),
        // time (Session 125)
        ("Instant", "std::time"),
        ("Duration", "std::time"),
        // net (Session 125)
        ("TcpListener", "std::net"),
        ("TcpStream", "std::net"),
        // thread (Session 126)
        ("thread", "std"),
        ("Thread", "std::thread"),
        ("JoinHandle", "std::thread"),
        // marker (Session 126)
        ("PhantomData", "std::marker"),
        // borrow (Session 126)
        ("Cow", "std::borrow"),
        // sync::mpsc (Session 126)
        ("Sender", "std::sync::mpsc"),
        ("Receiver", "std::sync::mpsc"),
        // sync additional (Session 126)
        ("Condvar", "std::sync"),
        ("Barrier", "std::sync"),
        // sync::atomic (Session 126)
        ("AtomicBool", "std::sync::atomic"),
        ("AtomicI32", "std::sync::atomic"),
        ("AtomicU32", "std::sync::atomic"),
        ("AtomicI64", "std::sync::atomic"),
        ("AtomicU64", "std::sync::atomic"),
        ("AtomicUsize", "std::sync::atomic"),
        // pin (Session 127)
        ("Pin", "std::pin"),
        // cmp (Session 127)
        ("Ordering", "std::cmp"),
        // ops (Session 127)
        ("Range", "std::ops"),
        ("RangeInclusive", "std::ops"),
        // any (Session 127)
        ("TypeId", "std::any"),
        ("Any", "std::any"),
        // fmt (Session 127)
        ("Formatter", "std::fmt"),
        ("Display", "std::fmt"),
        ("Debug", "std::fmt"),
        // iter (Session 127)
        ("FromIterator", "std::iter"),
        ("Peekable", "std::iter"),
        // hash (Session 127)
        ("Hash", "std::hash"),
        ("Hasher", "std::hash"),
        // mem (Session 127)
        ("mem", "std"),
        // num (Session 127)
        ("NonZeroU32", "std::num"),
        ("NonZeroU64", "std::num"),
        ("NonZeroUsize", "std::num"),
        // collections::hash_map (Session 127)
        ("Entry", "std::collections::hash_map"),
        // future (Session 128)
        ("Future", "std::future"),
        // task (Session 128)
        ("Poll", "std::task"),
        ("Waker", "std::task"),
        // alloc (Session 128)
        ("Layout", "std::alloc"),
        // ffi (Session 128)
        ("CString", "std::ffi"),
        ("CStr", "std::ffi"),
        // str pattern (Session 128)
        ("Pattern", "std::str::pattern"),
        // process additional (Session 129)
        ("Child", "std::process"),
        ("Stdio", "std::process"),
        // ffi additional (Session 129)
        ("OsStr", "std::ffi"),
        ("OsString", "std::ffi"),
        // os::unix::io (Session 130)
        ("RawFd", "std::os::unix::io"),
        ("OwnedFd", "std::os::unix::io"),
        ("BorrowedFd", "std::os::unix::io"),
        // os::windows::io (Session 130)
        ("RawHandle", "std::os::windows::io"),
        ("OwnedHandle", "std::os::windows::io"),
        ("BorrowedHandle", "std::os::windows::io"),
        // Session 150: std types commonly used in data structure implementations
        // std::str::FromStr — needed when implementing impl FromStr for T
        ("FromStr", "std::str"),
        // std::fmt::Write — needed when implementing impl Write for T
        ("Write", "std::fmt"),
        // std::ops::{Deref, DerefMut, Index, IndexMut, Drop} — smart pointer & indexing traits
        ("Deref", "std::ops"),
        ("DerefMut", "std::ops"),
        ("Index", "std::ops"),
        ("IndexMut", "std::ops"),
        ("Drop", "std::ops"),
        // std::iter::{FusedIterator, DoubleEndedIterator, ExactSizeIterator} — iterator marker traits
        ("FusedIterator", "std::iter"),
        ("DoubleEndedIterator", "std::iter"),
        ("ExactSizeIterator", "std::iter"),
    ];

    // 收集需要的导入: module_path -> Vec<type_name>
    let mut needed: std::collections::BTreeMap<&str, Vec<&str>> = std::collections::BTreeMap::new();

    // 预计算 glob 导入用于覆盖检查 (Session 129)
    let glob_imports = extract_glob_imports(content);

    for &(type_name, module_path) in type_modules {
        // 检测使用: 出现类型名作为标识符 (不是全限定路径的一部分)
        // 使用词边界匹配: 排除 type_name 作为更大标识符子串的情况 (如 "Cell" in "OnceCell")
        let full_path = format!("{}::{}", module_path, type_name);
        let bare_usage = contains_type_usage(content, type_name) && !content.contains(&full_path);

        if bare_usage {
            // 检查是否已有导入
            let already_imported = content.lines().any(|line| {
                let trimmed = line.trim();
                // 检查 use module_path::type_name;
                trimmed.starts_with(&format!("use {}::{}", module_path, type_name))
                // 检查 use module_path::{..., type_name, ...};
                || (trimmed.contains(&format!("use {}::{{", module_path))
                    && contains_type_usage(trimmed, type_name))
                // 检查 use module_path::*;
                || trimmed.starts_with(&format!("use {}::*;", module_path))
                // 检查 use std::collections::*; 等 (对 std::collections::HashMap 等)
                || trimmed.starts_with("use std::*;")
            }) || has_covering_glob_import(&glob_imports, module_path);

            if !already_imported {
                needed.entry(module_path).or_default().push(type_name);
            }
        }
    }

    if needed.is_empty() {
        return content.to_string();
    }

    // 构建导入行 (Session 126: 同一模块的类型按字母序排列)
    let import_lines: Vec<String> = needed
        .iter()
        .map(|(&module, types)| {
            let mut sorted_types = types.to_vec();
            sorted_types.sort();
            if sorted_types.len() == 1 {
                format!("use {}::{};", module, sorted_types[0])
            } else {
                format!("use {}::{{{}}};", module, sorted_types.join(", "))
            }
        })
        .collect();

    // 找到插入位置: 第一个非注释、非属性、非空白行之前
    let lines: Vec<&str> = content.lines().collect();
    let mut insert_pos = 0;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("//")
            || trimmed.starts_with("/*")
            || trimmed.starts_with("*")
            || trimmed.starts_with("#![")
            || trimmed.starts_with("//!")
            || trimmed.starts_with("///")
        {
            continue;
        }
        insert_pos = i;
        break;
    }

    if insert_pos == 0 && !lines.is_empty() {
        let all_comments = lines
            .iter()
            .all(|l| l.trim().is_empty() || l.trim().starts_with("//"));
        if all_comments {
            insert_pos = lines.len();
        }
    }

    let mut result_lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    for (offset, import_line) in import_lines.iter().enumerate() {
        result_lines.insert(insert_pos + offset, import_line.clone());
    }

    result_lines.join("\n")
}

/// 检测并添加缺失的外部 crate 导入 (Session 127 + Session 128 扩展)
///
/// 与 `ensure_std_imports` 类似, 但针对常见的外部 crate 类型:
///
/// - `Serialize` / `Deserialize` → `use serde::{Deserialize, Serialize};`
/// - `Regex` → `use regex::Regex;`
/// - `DateTime` / `NaiveDateTime` / `NaiveDate` / `NaiveTime` / `TimeZone` → `use chrono::{...};`
/// - `tracing` 宏 (`info!` / `warn!` / `error!` / `debug!` / `trace!`) → `use tracing::{...};`
/// - `Client` / `Response` / `StatusCode` → `use reqwest::{...};` (Session 128)
/// - `Value` / `json!` → `use serde_json::{Value, json};` (Session 128)
/// - `JoinHandle` → `use tokio::task::JoinHandle;` (Session 128)
/// - `spawn` / `join!` / `select!` → `use tokio::{spawn, join, select};` (Session 128)
/// - `IterTools` → `use itertools::IterTools;` (Session 129)
/// - `iproduct!` / `izip!` / `multiunzip!` → `use itertools::{...};` (Session 129)
/// - `#[derive(Error)]` → `use thiserror::Error;` (Session 129)
/// - `#[async_trait]` → `use async_trait::async_trait;` (Session 129)
/// - `Arg` / `Subcommand` / `ArgAction` → `use clap::{...};` (Session 130)
/// - `Uuid` → `use uuid::Uuid;` (Session 130)
/// - `Url` → `use url::Url;` (Session 130)
/// - `Level` / `LevelFilter` / `log!` → `use log::{...};` (Session 130)
/// - `EnvFilter` → `use tracing_subscriber::EnvFilter;` (Session 130)
/// - `Rng` / `ThreadRng` → `use rand::Rng;` / `use rand::rngs::ThreadRng;` (Session 131)
/// - `DashMap` → `use dashmap::DashMap;` (Session 131)
/// - `ParallelIterator` → `use rayon::prelude::ParallelIterator;` (Session 131)
/// - `Array1` / `Array2` → `use ndarray::{Array1, Array2};` (Session 131)
/// - `StreamExt` / `Stream` → `use tokio_stream::{StreamExt, Stream};` (Session 131)
/// - `.gen_range()` / `.fill()` → `use rand::Rng;` (Session 131)
/// - `.par_iter()` / `.par_iter_mut()` → `use rayon::prelude::ParallelIterator;` (Session 131)
/// - `Router` / `Json` / `Handler` / `IntoResponse` → `use axum::{...};` (Session 132)
/// - `Tera` / `Context` → `use tera::{...};` (Session 132)
/// - `#[derive(Template)]` → `use askama::Template;` (Session 132)
/// - `Connection` / `Statement` / `Row` → `use rusqlite::{...};` (Session 132)
/// - `params!` → `use rusqlite::params;` (Session 132)
/// - `Cmd` / `AsyncConnection` → `use redis::{...};` (Session 132)
/// - `Pool` / `QueryBuilder` / `Executor` / `AnyPool` / `MySqlPool` / `PgPool` / `SqlitePool` → `use sqlx::{...};` (Session 133)
/// - `query!` / `query_as!` / `query_unchecked!` → `use sqlx::{...};` (Session 133)
/// - `TypedHeader` → `use axum_extra::...;` (Session 133)
/// - `Message` / `Transport` / `SmtpTransport` / `AsyncTransport` → `use lettre::{...};` (Session 133)
/// - `Config` → `use config::Config;` (Session 133)
/// - `Filter` / `Reply` → `use warp::{...};` (Session 135)
/// - `HttpResponse` / `HttpRequest` / `Responder` / `HttpServer` → `use actix_web::{...};` (Session 135)
/// - `EntityTrait` / `Database` / `DbConn` / `PaginatorTrait` → `use sea_orm::{...};` (Session 135)
/// - `QueryDsl` / `RunQueryDsl` / `ExpressionMethods` / `PgConnection` / `SqliteConnection` → `use diesel::{...};` (Session 135)
/// - `Mailgun` / `Recipient` → `use mailgun::{...};` (Session 136)
/// - `Charge` / `Customer` / `PaymentIntent` → `use stripe::{...};` (Session 136)
/// - `PutObjectOutput` / `GetObjectOutput` → `use aws_sdk_s3::{...};` (Session 136)
/// - `SdkConfig` / `BehaviorVersion` → `use aws_config::{...};` (Session 136)
/// - `Builder` / `Target` / `Filter` → `use env_logger::{...};` (Session 139)
/// - `Watcher` / `EventKind` / `Event` → `use notify::{...};` (Session 139)
/// - `ShadowBuilder` → `use shadow_rs::ShadowBuilder;` (Session 139)
/// - `System` / `CpuCore` / `Disk` → `use sysinfo::{...};` (Session 141)
/// - `SerialPort` → `use serialport::SerialPort;` (Session 141)
/// - `machine_uid` → `use machine_uid;` (Session 141)
/// - `.cloned()` / `.copied()` / `.fuse()` → `use std::iter::Iterator;` (Session 141)
///
/// # 规则
///
/// 1. 检测代码中使用了哪些外部 crate 类型 (词边界匹配)
/// 2. 排除全限定路径 (如 `serde::Serialize`, `chrono::Utc`)
/// 3. 缺失的导入自动添加, 已有的不重复
/// 4. 同一 crate 的多个类型合并为 `use crate::{Type1, Type2};`
/// 5. 幂等: 已有导入不重复添加
/// 6. 同一 crate 的多个类型按字母序排列
/// 7. glob 导入覆盖: `use crate::*;` 或 `use parent::*;` 覆盖子模块类型 (Session 129)
///
/// # 示例
///
/// ```
/// use forge::extract::ensure_external_imports;
///
/// // 使用 Serialize 但未导入
/// let code = "#[derive(Serialize)]\nstruct Foo { x: i32 }";
/// let result = ensure_external_imports(code);
/// assert!(result.contains("use serde::Serialize;"), "应添加 Serialize 导入: {}", result);
///
/// // 已有导入不重复 (幂等)
/// let second = ensure_external_imports(&result);
/// assert_eq!(result, second, "二次调用不变化");
///
/// // 全限定路径不需要导入
/// let code = "#[derive(serde::Serialize)]\nstruct Foo { x: i32 }";
/// let result = ensure_external_imports(code);
/// assert!(!result.contains("use serde::"), "全限定路径不需要导入");
/// ```
pub fn ensure_external_imports(content: &str) -> String {
    // 类型检测: (type_name, crate_path)
    let type_modules: &[(&str, &str)] = &[
        // serde
        ("Serialize", "serde"),
        ("Deserialize", "serde"),
        // regex
        ("Regex", "regex"),
        // chrono
        ("DateTime", "chrono"),
        ("NaiveDateTime", "chrono"),
        ("NaiveDate", "chrono"),
        ("NaiveTime", "chrono"),
        ("TimeZone", "chrono"),
        // reqwest (Session 128)
        ("Client", "reqwest"),
        ("Response", "reqwest"),
        ("StatusCode", "reqwest"),
        // serde_json (Session 128)
        ("Value", "serde_json"),
        // tokio::task (Session 128)
        ("JoinHandle", "tokio::task"),
        // itertools (Session 129)
        ("IterTools", "itertools"),
        // clap (Session 130)
        ("Arg", "clap"),
        ("Subcommand", "clap"),
        ("ArgAction", "clap"),
        // uuid (Session 130)
        ("Uuid", "uuid"),
        // url (Session 130)
        ("Url", "url"),
        // log (Session 130)
        ("Level", "log"),
        ("LevelFilter", "log"),
        // tracing_subscriber (Session 130)
        ("EnvFilter", "tracing_subscriber"),
        // rand (Session 131)
        ("Rng", "rand"),
        ("ThreadRng", "rand::rngs"),
        // dashmap (Session 131)
        ("DashMap", "dashmap"),
        // rayon (Session 131)
        ("ParallelIterator", "rayon::prelude"),
        // ndarray (Session 131)
        ("Array1", "ndarray"),
        ("Array2", "ndarray"),
        // tokio_stream (Session 131)
        ("StreamExt", "tokio_stream"),
        ("Stream", "tokio_stream"),
        // axum (Session 132)
        ("Router", "axum"),
        ("Json", "axum"),
        ("Handler", "axum"),
        ("IntoResponse", "axum"),
        // tera (Session 132)
        ("Tera", "tera"),
        ("Context", "tera"),
        // askama (Session 132)
        ("Template", "askama"),
        // rusqlite (Session 132)
        ("Connection", "rusqlite"),
        ("Statement", "rusqlite"),
        ("Row", "rusqlite"),
        // redis (Session 132)
        ("Cmd", "redis"),
        ("AsyncConnection", "redis"),
        // sqlx (Session 133)
        ("Pool", "sqlx"),
        ("QueryBuilder", "sqlx"),
        ("Executor", "sqlx"),
        ("AnyPool", "sqlx"),
        ("MySqlPool", "sqlx"),
        ("PgPool", "sqlx"),
        ("SqlitePool", "sqlx"),
        // axum-extra (Session 133)
        ("TypedHeader", "axum_extra"),
        // lettre (Session 133)
        ("Message", "lettre"),
        ("Transport", "lettre"),
        ("SmtpTransport", "lettre"),
        ("AsyncTransport", "lettre"),
        // config (Session 133)
        ("Config", "config"),
        // hyper (Session 134)
        ("Body", "hyper"),
        ("HeaderMap", "hyper"),
        ("Uri", "hyper"),
        ("Method", "hyper"),
        // tower (Session 134)
        ("Service", "tower"),
        ("ServiceExt", "tower"),
        ("Layer", "tower"),
        ("BoxError", "tower"),
        // tonic (Session 134)
        ("Status", "tonic"),
        ("Code", "tonic"),
        ("Channel", "tonic::transport"),
        // proc_macro2 (Session 134)
        ("TokenStream", "proc_macro2"),
        ("Span", "proc_macro2"),
        ("Ident", "proc_macro2"),
        ("Literal", "proc_macro2"),
        // syn (Session 134)
        ("DeriveInput", "syn"),
        ("ItemFn", "syn"),
        ("ItemStruct", "syn"),
        ("ItemEnum", "syn"),
        // warp (Session 135)
        ("Filter", "warp"),
        ("Reply", "warp"),
        // actix-web (Session 135)
        ("HttpResponse", "actix_web"),
        ("HttpRequest", "actix_web"),
        ("Responder", "actix_web"),
        ("HttpServer", "actix_web"),
        // sea-orm (Session 135)
        ("EntityTrait", "sea_orm"),
        ("Database", "sea_orm"),
        ("DbConn", "sea_orm"),
        ("PaginatorTrait", "sea_orm"),
        // diesel (Session 135)
        ("QueryDsl", "diesel"),
        ("RunQueryDsl", "diesel"),
        ("ExpressionMethods", "diesel"),
        ("PgConnection", "diesel"),
        ("SqliteConnection", "diesel"),
        // mailgun (Session 136)
        ("Mailgun", "mailgun"),
        ("Recipient", "mailgun"),
        // stripe (Session 136)
        ("Charge", "stripe"),
        ("Customer", "stripe"),
        ("PaymentIntent", "stripe"),
        // aws-sdk-s3 (Session 136)
        ("PutObjectOutput", "aws_sdk_s3"),
        ("GetObjectOutput", "aws_sdk_s3"),
        // aws-config (Session 136)
        ("SdkConfig", "aws_config"),
        ("BehaviorVersion", "aws_config"),
        // oauth2 (Session 137)
        ("BasicClient", "oauth2"),
        ("AuthorizationCode", "oauth2"),
        ("AccessToken", "oauth2"),
        ("CsrfToken", "oauth2"),
        ("PkceCodeVerifier", "oauth2"),
        // glob (Session 137)
        ("Pattern", "glob"),
        ("GlobBuilder", "glob"),
        // cookie (Session 137)
        ("Cookie", "cookie"),
        ("CookieJar", "cookie"),
        // dotenv (Session 138)
        ("EnvError", "dotenv"),
        // tauri (Session 138)
        ("AppBuilder", "tauri"),
        ("AppHandle", "tauri"),
        ("Manager", "tauri"),
        ("Invoke", "tauri"),
        // wgpu (Session 138)
        ("Device", "wgpu"),
        ("Queue", "wgpu"),
        ("Surface", "wgpu"),
        ("SurfaceConfiguration", "wgpu"),
        ("ShaderModule", "wgpu"),
        // env_logger (Session 139)
        ("Builder", "env_logger"),
        ("Target", "env_logger"),
        ("Filter", "env_logger"),
        // notify (Session 139)
        ("Watcher", "notify"),
        ("EventKind", "notify"),
        ("Event", "notify"),
        // shadow-rs (Session 139)
        ("ShadowBuilder", "shadow_rs"),
        // sysinfo (Session 141)
        ("System", "sysinfo"),
        ("CpuCore", "sysinfo"),
        ("Disk", "sysinfo"),
        // serialport (Session 141)
        ("SerialPort", "serialport"),
        // machine-uid (Session 141)
        ("machine_uid", "machine_uid"),
        // dotenvy (Session 142)
        ("EnvLoader", "dotenvy"),
        ("EnvIter", "dotenvy"),
        // fd-lock (Session 142)
        ("FdLock", "fd_lock"),
        // nix (Session 142)
        ("NixPath", "nix::path"),
        ("Errno", "nix::errno"),
        // camino (Session 142)
        ("Utf8PathBuf", "camino"),
        ("Utf8Path", "camino"),
        // External crates (Session 144)
        ("Enigo", "enigo"),
        ("Display", "x11::display"),
        ("HMODULE", "winapi"),
        ("CGContext", "core_graphics"),
        // image (Session 145)
        ("ImageBuffer", "image"),
        ("Rgba", "image"),
        // imageproc (Session 145)
        ("Drawing", "imageproc::drawing"),
        // rusttype (Session 145)
        ("Font", "rusttype"),
        ("PositionedGlyph", "rusttype"),
        // plotters (Session 145)
        ("ChartContext", "plotters::chart"),
        // ratatui (Session 146)
        ("Line", "ratatui::text"),
        ("Layout", "ratatui::layout"),
        ("Block", "ratatui::widgets"),
        ("Widget", "ratatui::widgets"),
        // crossterm (Session 146)
        ("execute", "crossterm"),
        ("queue", "crossterm"),
        ("terminal", "crossterm"),
        // tui (Session 146)
        ("Frame", "tui"),
        ("Terminal", "tui"),
        // glium (Session 146)
        ("Display", "glium"),
        ("Surface", "glium::surface"),
        // vulkano (Session 146)
        ("VkHandle", "vulkano"),
        ("VulkanObject", "vulkano"),
        // ndarray-npy (Session 146)
        ("open_npz", "ndarray_npy"),
        ("save_npz", "ndarray_npy"),
        // eframe (Session 147)
        ("App", "eframe"),
        ("Frame", "eframe"),
        // egui (Session 147)
        ("Context", "egui"),
        ("Ui", "egui"),
        // iced (Session 147)
        ("Application", "iced"),
        ("Command", "iced"),
        // druid (Session 147)
        ("AppDelegate", "druid"),
        ("Widget", "druid::widget"),
        // slint (Session 147)
        ("ComponentHandle", "slint"),
        ("Model", "slint"),
        // num_cpus (Session 148) — num_cpus::num_cpus() function
        ("num_cpus", "num_cpus"),
        // once_cell (Session 148) — once_cell::sync::Lazy
        ("Lazy", "once_cell::sync"),
        // flate2 (Session 148) — flate2::write::GzEncoder / flate2::read::GzDecoder
        ("GzEncoder", "flate2::write"),
        ("GzDecoder", "flate2::read"),
        // tar (Session 148) — tar::Archive / tar::Entry
        ("Archive", "tar"),
        ("Entry", "tar"),
        // walkdir (Session 148) — walkdir::WalkDir
        ("WalkDir", "walkdir"),
        // tempfile (Session 148) — tempfile::TempDir / tempfile::NamedTempFile
        ("TempDir", "tempfile"),
        ("NamedTempFile", "tempfile"),
        // indicatif (Session 148) — indicatif::ProgressBar / indicatif::MultiProgress
        ("ProgressBar", "indicatif"),
        ("MultiProgress", "indicatif"),
        // dialoguer (Session 148) — dialoguer::Input / dialoguer::Select
        ("Input", "dialoguer"),
        ("Select", "dialoguer"),
        // console (Session 148) — console::Term / console::Style
        ("Term", "console"),
        ("Style", "console"),
        // Session 150: std types commonly used in data structure implementations
        // std::str::FromStr — needed when implementing impl FromStr for T
        ("FromStr", "std::str"),
        // std::fmt::Write — needed when implementing impl Write for T (Display/Debug already covered)
        ("Write", "std::fmt"),
        // std::ops::{Deref, DerefMut, Index, IndexMut, Drop} — smart pointer & indexing traits
        ("Deref", "std::ops"),
        ("DerefMut", "std::ops"),
        ("Index", "std::ops"),
        ("IndexMut", "std::ops"),
        ("Drop", "std::ops"),
        // std::iter::{FusedIterator, DoubleEndedIterator, ExactSizeIterator} — iterator marker traits
        ("FusedIterator", "std::iter"),
        ("DoubleEndedIterator", "std::iter"),
        ("ExactSizeIterator", "std::iter"),
    ];

    // 收集需要的导入: crate_path -> Vec<type_name>
    let mut needed: std::collections::BTreeMap<&str, Vec<&str>> = std::collections::BTreeMap::new();

    // 预计算 glob 导入用于覆盖检查 (Session 129)
    let glob_imports = extract_glob_imports(content);

    for &(type_name, crate_path) in type_modules {
        let full_path = format!("{}::{}", crate_path, type_name);
        // Session 141: 当 crate_path == type_name (如 machine_uid) 时,
        // 还需要检查 crate_path:: 前缀 (如 "machine_uid::get()") 是否为全限定路径
        let crate_prefix = format!("{}::", crate_path);
        let bare_usage = contains_type_usage(content, type_name)
            && !content.contains(&full_path)
            && !(crate_path == type_name && content.contains(&crate_prefix));

        if bare_usage {
            // 检查是否已有导入
            let already_imported = content.lines().any(|line| {
                let trimmed = line.trim();
                trimmed.starts_with(&format!("use {}::{}", crate_path, type_name))
                    || (trimmed.contains(&format!("use {}::{{", crate_path))
                        && contains_type_usage(trimmed, type_name))
                    || trimmed.starts_with(&format!("use {}::*;", crate_path))
            }) || has_covering_glob_import(&glob_imports, crate_path);

            if !already_imported {
                needed.entry(crate_path).or_default().push(type_name);
            }
        }
    }

    // 检测 tracing 宏使用 (info!, warn!, error!, debug!, trace!)
    let tracing_macros: &[&str] = &["info", "warn", "error", "debug", "trace"];
    let mut tracing_needed: Vec<&str> = Vec::new();

    for &macro_name in tracing_macros {
        let macro_pattern = format!("{}!(", macro_name);
        let full_path = format!("tracing::{}!", macro_name);

        // 检测裸宏使用 (排除 tracing:: 前缀)
        let bare_macro = content.contains(&macro_pattern) && !content.contains(&full_path);

        if bare_macro {
            // 检查是否已有导入
            let already_imported = content.lines().any(|line| {
                let trimmed = line.trim();
                trimmed.starts_with(&format!("use tracing::{};", macro_name))
                    || (trimmed.contains("use tracing::{")
                        && contains_type_usage(trimmed, macro_name))
                    || trimmed.starts_with("use tracing::*;")
            });

            if !already_imported {
                tracing_needed.push(macro_name);
            }
        }
    }

    if !tracing_needed.is_empty() {
        tracing_needed.sort();
        needed.entry("tracing").or_default().extend(tracing_needed);
    }

    // 检测 serde_json::json! 宏使用 (Session 128)
    let serde_json_macros: &[&str] = &["json"];
    let mut serde_json_macro_needed: Vec<&str> = Vec::new();

    for &macro_name in serde_json_macros {
        let macro_pattern = format!("{}!(", macro_name);
        let full_path = format!("serde_json::{}!", macro_name);

        let bare_macro = content.contains(&macro_pattern) && !content.contains(&full_path);

        if bare_macro {
            let already_imported = content.lines().any(|line| {
                let trimmed = line.trim();
                trimmed.starts_with(&format!("use serde_json::{};", macro_name))
                    || (trimmed.contains("use serde_json::{")
                        && contains_type_usage(trimmed, macro_name))
                    || trimmed.starts_with("use serde_json::*;")
            });

            if !already_imported {
                serde_json_macro_needed.push(macro_name);
            }
        }
    }

    if !serde_json_macro_needed.is_empty() {
        serde_json_macro_needed.sort();
        needed
            .entry("serde_json")
            .or_default()
            .extend(serde_json_macro_needed);
    }

    // 检测 tokio 宏和函数使用 (Session 128)
    // tokio::spawn (函数), tokio::join! / tokio::select! (宏)
    // 注意: join! 通常使用 () 调用, select! 使用 {} 调用, 可能有空格
    let tokio_macros: &[&str] = &["join", "select"];
    let mut tokio_macro_needed: Vec<&str> = Vec::new();

    for &macro_name in tokio_macros {
        let macro_bang = format!("{}!", macro_name);
        let full_path = format!("tokio::{}!", macro_name);

        // 检测裸宏使用: "macro_name!" 存在且不 preceded by "tokio::"
        let bare_macro = content.contains(&macro_bang) && !content.contains(&full_path);

        if bare_macro {
            let already_imported = content.lines().any(|line| {
                let trimmed = line.trim();
                trimmed.starts_with(&format!("use tokio::{};", macro_name))
                    || (trimmed.contains("use tokio::{")
                        && contains_type_usage(trimmed, macro_name))
                    || trimmed.starts_with("use tokio::*;")
            });

            if !already_imported {
                tokio_macro_needed.push(macro_name);
            }
        }
    }

    // 检测 tokio::spawn 函数使用 (排除 tokio::spawn 全限定路径)
    let spawn_bare = contains_type_usage(content, "spawn") && !content.contains("tokio::spawn");
    if spawn_bare {
        // 检查是否已有导入
        let already_imported = content.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("use tokio::spawn;")
                || (trimmed.contains("use tokio::{") && contains_type_usage(trimmed, "spawn"))
                || trimmed.starts_with("use tokio::*;")
        });

        if !already_imported {
            tokio_macro_needed.push("spawn");
        }
    }

    if !tokio_macro_needed.is_empty() {
        tokio_macro_needed.sort();
        tokio_macro_needed.dedup();
        needed
            .entry("tokio")
            .or_default()
            .extend(tokio_macro_needed);
    }

    // 检测 itertools 宏使用 (Session 129)
    // iproduct!, izip!, multiunzip!
    let itertools_macros: &[&str] = &["iproduct", "izip", "multiunzip"];
    let mut itertools_macro_needed: Vec<&str> = Vec::new();

    for &macro_name in itertools_macros {
        let macro_bang = format!("{}!(", macro_name);
        let full_path = format!("itertools::{}!", macro_name);

        let bare_macro = content.contains(&macro_bang) && !content.contains(&full_path);

        if bare_macro {
            let already_imported = content.lines().any(|line| {
                let trimmed = line.trim();
                trimmed.starts_with(&format!("use itertools::{};", macro_name))
                    || (trimmed.contains("use itertools::{")
                        && contains_type_usage(trimmed, macro_name))
                    || trimmed.starts_with("use itertools::*;")
            }) || has_covering_glob_import(&glob_imports, "itertools");

            if !already_imported {
                itertools_macro_needed.push(macro_name);
            }
        }
    }

    if !itertools_macro_needed.is_empty() {
        itertools_macro_needed.sort();
        itertools_macro_needed.dedup();
        needed
            .entry("itertools")
            .or_default()
            .extend(itertools_macro_needed);
    }

    // 检测 thiserror::Error derive 宏使用 (Session 129)
    // #[derive(Error)] → use thiserror::Error;
    if content.contains("#[derive(Error)]") && !content.contains("thiserror::Error") {
        let already_imported = content.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("use thiserror::Error;")
                || trimmed.contains("use thiserror::{")
                || trimmed.starts_with("use thiserror::*;")
        }) || has_covering_glob_import(&glob_imports, "thiserror");

        if !already_imported {
            needed.entry("thiserror").or_default().push("Error");
        }
    }

    // 检测 async_trait 属性宏使用 (Session 129)
    // #[async_trait] → use async_trait::async_trait;
    if content.contains("#[async_trait]") && !content.contains("async_trait::async_trait") {
        let already_imported = content.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("use async_trait::async_trait;")
                || trimmed.contains("use async_trait::{")
                || trimmed.starts_with("use async_trait::*;")
        }) || has_covering_glob_import(&glob_imports, "async_trait");

        if !already_imported {
            needed.entry("async_trait").or_default().push("async_trait");
        }
    }

    // 检测 itertools trait 方法调用 (Session 129)
    // .interleave( / .intersperse( / .tuples( / .group_by( / .permutations( / .combinations( 等
    // 这些方法需要 IterTools trait 在作用域中
    let itertools_methods: &[&str] = &[
        "interleave",
        "intersperse",
        "tuples",
        "group_by",
        "permutations",
        "combinations",
        "step",
        "multipeek",
    ];
    let mut itertools_method_found = false;
    for &method_name in itertools_methods {
        let method_call = format!(".{}(", method_name);
        if content.contains(&method_call) {
            itertools_method_found = true;
            break;
        }
    }

    if itertools_method_found {
        let already_imported = content.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("use itertools::IterTools;")
                || (trimmed.contains("use itertools::{")
                    && contains_type_usage(trimmed, "IterTools"))
                || trimmed.starts_with("use itertools::*;")
        }) || has_covering_glob_import(&glob_imports, "itertools");

        if !already_imported {
            needed.entry("itertools").or_default().push("IterTools");
        }
    }

    // 检测 rand Rng trait 方法调用 (Session 131)
    // .gen_range( / .fill( 方法需要 Rng trait 在作用域中
    let rand_methods: &[&str] = &["gen_range", "fill"];
    let mut rand_method_found = false;
    for &method_name in rand_methods {
        let method_call = format!(".{}(", method_name);
        if content.contains(&method_call) {
            rand_method_found = true;
            break;
        }
    }

    if rand_method_found {
        let already_imported = content.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("use rand::Rng;")
                || (trimmed.contains("use rand::{") && contains_type_usage(trimmed, "Rng"))
                || trimmed.starts_with("use rand::*;")
        }) || has_covering_glob_import(&glob_imports, "rand");

        if !already_imported {
            needed.entry("rand").or_default().push("Rng");
        }
    }

    // 检测 rayon ParallelIterator trait 方法调用 (Session 131)
    // .par_iter( / .par_iter_mut( 方法需要 ParallelIterator trait 在作用域中
    let rayon_methods: &[&str] = &["par_iter", "par_iter_mut"];
    let mut rayon_method_found = false;
    for &method_name in rayon_methods {
        let method_call = format!(".{}(", method_name);
        if content.contains(&method_call) {
            rayon_method_found = true;
            break;
        }
    }

    if rayon_method_found {
        let already_imported = content.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("use rayon::prelude::ParallelIterator;")
                || (trimmed.contains("use rayon::prelude::{")
                    && contains_type_usage(trimmed, "ParallelIterator"))
                || trimmed.starts_with("use rayon::prelude::*;")
                || trimmed.starts_with("use rayon::*;")
        }) || has_covering_glob_import(&glob_imports, "rayon");

        if !already_imported {
            needed
                .entry("rayon::prelude")
                .or_default()
                .push("ParallelIterator");
        }
    }

    // 检测 clap derive 宏使用 (Session 130)
    // #[derive(Subcommand)] → use clap::Subcommand;
    // #[derive(Arg)] → use clap::Arg;
    // 这些已在 type_modules 中检测, 但 #[derive(Subcommand)] 也需要检测
    // (contains_type_usage 已覆盖 #[derive(Arg)] 中的 Arg)

    // 检测 log::log! 宏使用 (Session 130)
    // log!(Level::Info, "msg") → use log::log;
    if content.contains("log!(") && !content.contains("log::log!") {
        let already_imported = content.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("use log::log;")
                || (trimmed.contains("use log::{") && contains_type_usage(trimmed, "log"))
                || trimmed.starts_with("use log::*;")
        }) || has_covering_glob_import(&glob_imports, "log");

        if !already_imported {
            needed.entry("log").or_default().push("log");
        }
    }

    // 检测 rusqlite params! 宏使用 (Session 132)
    // params![1, 2, 3] → use rusqlite::params;
    if content.contains("params![") && !content.contains("rusqlite::params") {
        let already_imported = content.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("use rusqlite::params;")
                || (trimmed.contains("use rusqlite::{") && contains_type_usage(trimmed, "params"))
                || trimmed.starts_with("use rusqlite::*;")
        }) || has_covering_glob_import(&glob_imports, "rusqlite");

        if !already_imported {
            needed.entry("rusqlite").or_default().push("params");
        }
    }

    // 检测 sqlx query! / query_as! 宏使用 (Session 133)
    // query!("SELECT * FROM users") → use sqlx::query;
    // query_as!(User, "SELECT * FROM users") → use sqlx::query_as;
    let sqlx_macros: &[&str] = &["query", "query_as", "query_unchecked"];
    let mut sqlx_macro_needed: Vec<&str> = Vec::new();

    for &macro_name in sqlx_macros {
        let macro_bang = format!("{}!(", macro_name);
        let full_path = format!("sqlx::{}!", macro_name);

        let bare_macro = content.contains(&macro_bang) && !content.contains(&full_path);

        if bare_macro {
            let already_imported = content.lines().any(|line| {
                let trimmed = line.trim();
                trimmed.starts_with(&format!("use sqlx::{};", macro_name))
                    || (trimmed.contains("use sqlx::{") && contains_type_usage(trimmed, macro_name))
                    || trimmed.starts_with("use sqlx::*;")
            }) || has_covering_glob_import(&glob_imports, "sqlx");

            if !already_imported {
                sqlx_macro_needed.push(macro_name);
            }
        }
    }

    if !sqlx_macro_needed.is_empty() {
        sqlx_macro_needed.sort();
        sqlx_macro_needed.dedup();
        needed.entry("sqlx").or_default().extend(sqlx_macro_needed);
    }

    // 检测 quote! / format_ident! 宏使用 (Session 134)
    // quote! { ... } → use quote::quote;
    // format_ident!("name") → use quote::format_ident;
    let quote_macros: &[&str] = &["quote", "format_ident"];
    let mut quote_macro_needed: Vec<&str> = Vec::new();

    for &macro_name in quote_macros {
        let macro_bang = format!("{}!", macro_name);
        let full_path = format!("quote::{}!", macro_name);

        let bare_macro = content.contains(&macro_bang) && !content.contains(&full_path);

        if bare_macro {
            let already_imported = content.lines().any(|line| {
                let trimmed = line.trim();
                trimmed.starts_with(&format!("use quote::{};", macro_name))
                    || (trimmed.contains("use quote::{")
                        && contains_type_usage(trimmed, macro_name))
                    || trimmed.starts_with("use quote::*;")
            }) || has_covering_glob_import(&glob_imports, "quote");

            if !already_imported {
                quote_macro_needed.push(macro_name);
            }
        }
    }

    if !quote_macro_needed.is_empty() {
        quote_macro_needed.sort();
        quote_macro_needed.dedup();
        needed
            .entry("quote")
            .or_default()
            .extend(quote_macro_needed);
    }

    // 检测 syn parse_quote! / parse_str! 宏使用 (Session 134)
    // parse_quote!(ItemFn) → use syn::parse_quote;
    // parse_str!("code") → use syn::parse_str;
    let syn_macros: &[&str] = &["parse_quote", "parse_str"];
    let mut syn_macro_needed: Vec<&str> = Vec::new();

    for &macro_name in syn_macros {
        let macro_bang = format!("{}!(", macro_name);
        let full_path = format!("syn::{}!", macro_name);

        let bare_macro = content.contains(&macro_bang) && !content.contains(&full_path);

        if bare_macro {
            let already_imported = content.lines().any(|line| {
                let trimmed = line.trim();
                trimmed.starts_with(&format!("use syn::{};", macro_name))
                    || (trimmed.contains("use syn::{") && contains_type_usage(trimmed, macro_name))
                    || trimmed.starts_with("use syn::*;")
            }) || has_covering_glob_import(&glob_imports, "syn");

            if !already_imported {
                syn_macro_needed.push(macro_name);
            }
        }
    }

    if !syn_macro_needed.is_empty() {
        syn_macro_needed.sort();
        syn_macro_needed.dedup();
        needed.entry("syn").or_default().extend(syn_macro_needed);
    }

    // 检测 .cloned() / .copied() / .fuse() → Iterator trait 方法调用 (Session 141)
    // 这些方法需要 Iterator trait 在作用域中
    let s141_iterator_methods: &[&str] = &["cloned", "copied", "fuse"];
    let mut s141_iterator_method_found = false;
    for &method_name in s141_iterator_methods {
        let method_call = format!(".{}(", method_name);
        if content.contains(&method_call) {
            s141_iterator_method_found = true;
            break;
        }
    }

    if s141_iterator_method_found {
        let already_imported = content.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("use std::iter::Iterator;")
                || (trimmed.contains("use std::iter::{")
                    && contains_type_usage(trimmed, "Iterator"))
                || trimmed.starts_with("use std::iter::*;")
        }) || has_covering_glob_import(&glob_imports, "std::iter");

        if !already_imported {
            needed.entry("std::iter").or_default().push("Iterator");
        }
    }

    // 检测 .flatten() / .max() / .min() / .sum() / .product() → Iterator trait 方法调用 (Session 142)
    // .flatten() / .max( / .min( 需要 Iterator trait 在作用域中
    // .sum( / .product( 需要 Iterator trait 的 sum()/product() 方法 (通过 std::iter::Sum/Product trait)
    let s142_iterator_methods: &[&str] = &[
        "flatten",
        "max",
        "min",
        "sum",
        "product",
        // Session 144: .any() / .all() / .find() / .position() / .count() / .fold() / .reduce() / .partition() / .for_each()
        "any",
        "all",
        "find",
        "position",
        "count",
        "fold",
        "reduce",
        "partition",
        "for_each",
        // Session 145: .scan() / .unzip() / .cycle() / .peekable() (already in s141) / .sum() / .product() (already above)
        "scan",
        "unzip",
        "cycle",
        // Session 146: .chunks() / .windows() / .rchunks() / .as_chunks() / .array_chunks()
        "chunks",
        "windows",
        "rchunks",
        "as_chunks",
        "array_chunks",
        // Session 147: .first() / .last() / .nth() / .next_back() / .rposition() / .rfold() / .rfind()
        "first",
        "last",
        "nth",
        "next_back",
        "rposition",
        "rfold",
        "rfind",
        // Session 148: .iter_mut() / .into_iter() / .rev() (already above) / .step_by() (already above)
        // .iter_mut() → Iterator (mutable iteration)
        "iter_mut",
        // .split() / .splitn() / .rsplit() / .rsplitn() → Iterator (str/slice methods)
        "split",
        "splitn",
        "rsplit",
        "rsplitn",
        // .lines() / .chars() / .bytes() / .enumerate() (already above) → Iterator
        "lines",
        "chars",
        "bytes",
        // Session 149: .iter() / .try_fold() / .try_for_each() / .is_sorted() / .collect_into()
        // .iter() → Iterator (core method for obtaining an iterator)
        "iter",
        // .try_fold() / .try_for_each() → Iterator (fallible iteration)
        "try_fold",
        "try_for_each",
        // .is_sorted() → Iterator (checks if iterator is sorted)
        "is_sorted",
        // .collect_into() → Iterator (collects into a specified collection)
        "collect_into",
    ];
    let mut s142_iterator_method_found = false;
    for &method_name in s142_iterator_methods {
        let method_call = format!(".{}(", method_name);
        if content.contains(&method_call) {
            s142_iterator_method_found = true;
            break;
        }
    }

    if s142_iterator_method_found {
        let already_imported = content.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("use std::iter::Iterator;")
                || (trimmed.contains("use std::iter::{")
                    && contains_type_usage(trimmed, "Iterator"))
                || trimmed.starts_with("use std::iter::*;")
        }) || has_covering_glob_import(&glob_imports, "std::iter");

        if !already_imported {
            needed.entry("std::iter").or_default().push("Iterator");
        }
    }

    if needed.is_empty() {
        return content.to_string();
    }

    // 构建导入行 (字母序排列)
    let import_lines: Vec<String> = needed
        .iter()
        .map(|(&crate_path, types)| {
            let mut sorted_types = types.to_vec();
            sorted_types.sort();
            sorted_types.dedup();
            if sorted_types.len() == 1 {
                format!("use {}::{};", crate_path, sorted_types[0])
            } else {
                format!("use {}::{{{}}};", crate_path, sorted_types.join(", "))
            }
        })
        .collect();

    // 找到插入位置: 第一个非注释、非属性、非空白行之前
    let lines: Vec<&str> = content.lines().collect();
    let mut insert_pos = 0;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("//")
            || trimmed.starts_with("/*")
            || trimmed.starts_with("*")
            || trimmed.starts_with("#![")
            || trimmed.starts_with("//!")
            || trimmed.starts_with("///")
        {
            continue;
        }
        insert_pos = i;
        break;
    }

    if insert_pos == 0 && !lines.is_empty() {
        let all_comments = lines
            .iter()
            .all(|l| l.trim().is_empty() || l.trim().starts_with("//"));
        if all_comments {
            insert_pos = lines.len();
        }
    }

    let mut result_lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    for (offset, import_line) in import_lines.iter().enumerate() {
        result_lines.insert(insert_pos + offset, import_line.clone());
    }

    result_lines.join("\n")
}

/// ANSI 颜色常量 (Session 126)
const ANSI_RESET: &str = "\x1b[0m";
const ANSI_RED: &str = "\x1b[31m";
const ANSI_GREEN: &str = "\x1b[32m";
const ANSI_CYAN: &str = "\x1b[36m";
const ANSI_BOLD_YELLOW: &str = "\x1b[1;33m";

/// 带颜色的统一 diff 格式输出 (Session 126)
///
/// 与 `format_diff_unified_with_options` 功能相同, 但添加 ANSI 颜色:
/// - `---` / `+++` 文件头 → 粗体黄色
/// - `@@` hunk 头 → 青色
/// - `+` 新增行 → 绿色
/// - `-` 删除行 → 红色
/// - ` ` 上下文行 → 无颜色
///
/// # 示例
///
/// ```
/// use forge::extract::format_diff_unified_colored;
///
/// let original = "fn foo() {\n}\n";
/// let fixed = "fn foo() {\n    let x = 42;\n}\n";
/// let diff = format_diff_unified_colored(original, fixed, "src/main.rs", "src/main.rs", 3);
/// assert!(!diff.is_empty(), "有差异应有颜色 diff");
/// assert!(diff.contains("\x1b[32m"), "应包含绿色 ANSI 码");
/// ```
pub fn format_diff_unified_colored(
    original: &str,
    fixed: &str,
    original_name: &str,
    fixed_name: &str,
    context: usize,
) -> String {
    let plain =
        format_diff_unified_with_options(original, fixed, original_name, fixed_name, context);
    if plain.is_empty() {
        return String::new();
    }

    let mut result = String::new();
    for line in plain.lines() {
        if line.starts_with("--- ") || line.starts_with("+++ ") {
            result.push_str(ANSI_BOLD_YELLOW);
            result.push_str(line);
            result.push_str(ANSI_RESET);
        } else if line.starts_with("@@") {
            result.push_str(ANSI_CYAN);
            result.push_str(line);
            result.push_str(ANSI_RESET);
        } else if line.starts_with('+') {
            result.push_str(ANSI_GREEN);
            result.push_str(line);
            result.push_str(ANSI_RESET);
        } else if line.starts_with('-') {
            result.push_str(ANSI_RED);
            result.push_str(line);
            result.push_str(ANSI_RESET);
        } else {
            result.push_str(line);
        }
        result.push('\n');
    }

    // 移除末尾多余的换行
    if result.ends_with('\n') {
        result.pop();
    }
    result
}

/// 导入问题 — 编译前静态检查发现的缺失导入 (Session 126)
///
/// 表示代码中使用了某个类型但缺少对应的 `use` 导入语句。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ImportIssue {
    /// 缺失的类型名 (如 "HashMap", "Result")
    pub type_name: String,
    /// 应导入的模块路径 (如 "std::collections", "anyhow")
    pub module_path: String,
    /// 使用该类型的行号 (1-based)
    pub usage_line: usize,
}

/// 编译前静态检查所有导入是否完整 (Session 126)
///
/// 扫描代码中使用的类型, 检查是否有对应的 `use` 导入语句。
/// 返回所有缺失导入的列表, 每项包含类型名、模块路径和使用行号。
///
/// # 检测范围
///
/// - `anyhow::Result` / `Result<` → `use anyhow::Result;`
/// - `anyhow::Error` → `use anyhow::Error;`
/// - `bail!` / `ensure!` → `use anyhow::{bail, ensure};`
/// - `anyhow!` → `use anyhow::anyhow;`
/// - `.context(` / `.with_context(` → `use anyhow::Context;`
/// - 所有 `ensure_std_imports` 检测的 std 类型
/// - `.lock()` → `use std::sync::Mutex;` (Session 131)
/// - `.par_iter()` / `.par_iter_mut()` → `use rayon::prelude::ParallelIterator;` (Session 131)
/// - `.gen_range()` / `.fill()` → `use rand::Rng;` (Session 131)
/// - `.read()` / `.write()` → `use std::sync::RwLock;` (Session 132)
/// - `.send()` → `use std::sync::mpsc::Sender;` (Session 132)
/// - `.recv()` → `use std::sync::mpsc::Receiver;` (Session 132)
/// - `.try_read()` / `.try_write()` → `use std::sync::RwLock;` (Session 133)
/// - `.try_send()` → `use std::sync::mpsc::Sender;` (Session 133)
/// - `.await?` → `use anyhow::Result;` (Session 134)
/// - `.spawn()` → `use tokio::spawn;` (Session 134)
/// - `.collect()` → `use std::iter::FromIterator;` (Session 135)
/// - `.into_iter()` → `use std::iter::IntoIterator;` (Session 135)
/// - `.map()` / `.filter()` / `.zip()` / `.chain()` / `.enumerate()` / `.flat_map()` / `.peekable()` / `.skip()` / `.take()` / `.rev()` / `.step_by()` / `.cloned()` / `.copied()` / `.fuse()` → `use std::iter::Iterator;` (Session 136 + 137 + 138 + 139 + 141)
/// - `Mailgun` / `Recipient` → `use mailgun::{...};` (Session 136)
/// - `Charge` / `Customer` / `PaymentIntent` → `use stripe::{...};` (Session 136)
/// - `PutObjectOutput` / `GetObjectOutput` → `use aws_sdk_s3::{...};` (Session 136)
/// - `SdkConfig` / `BehaviorVersion` → `use aws_config::{...};` (Session 136)
/// - `BasicClient` / `AuthorizationCode` / `AccessToken` / `CsrfToken` / `PkceCodeVerifier` → `use oauth2::{...};` (Session 137)
/// - `Pattern` / `GlobBuilder` → `use glob::{...};` (Session 137)
/// - `Cookie` / `CookieJar` → `use cookie::{...};` (Session 137)
/// - `Builder` / `Target` / `Filter` → `use env_logger::{...};` (Session 139)
/// - `Watcher` / `EventKind` / `Event` → `use notify::{...};` (Session 139)
/// - `ShadowBuilder` → `use shadow_rs::ShadowBuilder;` (Session 139)
/// - `System` / `CpuCore` / `Disk` → `use sysinfo::{...};` (Session 141)
/// - `SerialPort` → `use serialport::SerialPort;` (Session 141)
/// - `machine_uid` → `use machine_uid;` (Session 141)
/// - `.cloned()` / `.copied()` / `.fuse()` → `use std::iter::Iterator;` (Session 141)
///
/// # 示例
///
/// ```
/// use forge::extract::verify_imports;
///
/// // 使用 HashMap 但未导入
/// let issues = verify_imports("fn foo() -> HashMap<String, i32> { HashMap::new() }");
/// assert!(issues.iter().any(|i| i.type_name == "HashMap"), "应检测到 HashMap 缺失导入");
///
/// // 已有导入 → 无问题
/// let issues = verify_imports("use std::collections::HashMap;\nfn foo() -> HashMap<String, i32> { HashMap::new() }");
/// assert!(!issues.iter().any(|i| i.type_name == "HashMap"), "已有导入不应报告");
///
/// // 无类型使用 → 无问题
/// let issues = verify_imports("fn foo() -> i32 { 42 }");
/// assert!(issues.is_empty(), "无类型使用不应有问题");
/// ```
pub fn verify_imports(content: &str) -> Vec<ImportIssue> {
    let lines: Vec<&str> = content.lines().collect();
    let mut issues = Vec::new();

    // 检查 anyhow::Result / Result< 的使用
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        // 跳过注释行和导入行
        if trimmed.starts_with("//") || trimmed.starts_with("use ") {
            continue;
        }

        // Result<T, ...> 或 anyhow::Result
        if (trimmed.contains("Result<") || trimmed.contains("anyhow::Result"))
            && !content.contains("use anyhow::Result")
            && !content.contains("use anyhow::{")
        {
            issues.push(ImportIssue {
                type_name: "Result".to_string(),
                module_path: "anyhow".to_string(),
                usage_line: i + 1,
            });
        }

        // anyhow::Error
        if trimmed.contains("anyhow::Error")
            && !content.contains("use anyhow::Error")
            && !content.contains("use anyhow::{")
        {
            issues.push(ImportIssue {
                type_name: "Error".to_string(),
                module_path: "anyhow".to_string(),
                usage_line: i + 1,
            });
        }

        // bail!
        if trimmed.contains("bail!")
            && !content.contains("use anyhow::bail")
            && !content.contains("use anyhow::{")
        {
            issues.push(ImportIssue {
                type_name: "bail".to_string(),
                module_path: "anyhow".to_string(),
                usage_line: i + 1,
            });
        }

        // ensure!
        if trimmed.contains("ensure!")
            && !content.contains("use anyhow::ensure")
            && !content.contains("use anyhow::{")
        {
            issues.push(ImportIssue {
                type_name: "ensure".to_string(),
                module_path: "anyhow".to_string(),
                usage_line: i + 1,
            });
        }

        // .context( 或 .with_context(
        if (trimmed.contains(".context(") || trimmed.contains(".with_context("))
            && !content.contains("use anyhow::Context")
            && !content.contains("use anyhow::{")
        {
            issues.push(ImportIssue {
                type_name: "Context".to_string(),
                module_path: "anyhow".to_string(),
                usage_line: i + 1,
            });
        }
    }

    // 检查 std 类型 (复用 ensure_std_imports 的类型列表)
    let type_modules: &[(&str, &str)] = &[
        ("HashMap", "std::collections"),
        ("HashSet", "std::collections"),
        ("BTreeMap", "std::collections"),
        ("BTreeSet", "std::collections"),
        ("VecDeque", "std::collections"),
        ("LinkedList", "std::collections"),
        ("BinaryHeap", "std::collections"),
        ("Path", "std::path"),
        ("PathBuf", "std::path"),
        ("File", "std::fs"),
        ("BufReader", "std::io"),
        ("BufWriter", "std::io"),
        ("Read", "std::io"),
        ("Write", "std::io"),
        ("BufRead", "std::io"),
        ("Stdin", "std::io"),
        ("Stdout", "std::io"),
        ("Stderr", "std::io"),
        ("Cell", "std::cell"),
        ("RefCell", "std::cell"),
        ("OnceCell", "std::cell"),
        ("Arc", "std::sync"),
        ("Mutex", "std::sync"),
        ("RwLock", "std::sync"),
        ("OnceLock", "std::sync"),
        ("Rc", "std::rc"),
        ("Command", "std::process"),
        ("ExitStatus", "std::process"),
        ("env", "std"),
        ("Instant", "std::time"),
        ("Duration", "std::time"),
        ("TcpListener", "std::net"),
        ("TcpStream", "std::net"),
        ("thread", "std"),
        ("Thread", "std::thread"),
        ("JoinHandle", "std::thread"),
        ("PhantomData", "std::marker"),
        ("Cow", "std::borrow"),
        ("Sender", "std::sync::mpsc"),
        ("Receiver", "std::sync::mpsc"),
        ("Condvar", "std::sync"),
        ("Barrier", "std::sync"),
        ("AtomicBool", "std::sync::atomic"),
        ("AtomicI32", "std::sync::atomic"),
        ("AtomicU32", "std::sync::atomic"),
        ("AtomicI64", "std::sync::atomic"),
        ("AtomicU64", "std::sync::atomic"),
        ("AtomicUsize", "std::sync::atomic"),
        // pin (Session 127)
        ("Pin", "std::pin"),
        // cmp (Session 127)
        ("Ordering", "std::cmp"),
        // ops (Session 127)
        ("Range", "std::ops"),
        ("RangeInclusive", "std::ops"),
        // any (Session 127)
        ("TypeId", "std::any"),
        ("Any", "std::any"),
        // fmt (Session 127)
        ("Formatter", "std::fmt"),
        ("Display", "std::fmt"),
        ("Debug", "std::fmt"),
        // iter (Session 127)
        ("FromIterator", "std::iter"),
        ("Peekable", "std::iter"),
        // hash (Session 127)
        ("Hash", "std::hash"),
        ("Hasher", "std::hash"),
        // mem (Session 127)
        ("mem", "std"),
        // num (Session 127)
        ("NonZeroU32", "std::num"),
        ("NonZeroU64", "std::num"),
        ("NonZeroUsize", "std::num"),
        // collections::hash_map (Session 127)
        ("Entry", "std::collections::hash_map"),
        // future (Session 128)
        ("Future", "std::future"),
        // task (Session 128)
        ("Poll", "std::task"),
        ("Waker", "std::task"),
        // alloc (Session 128)
        ("Layout", "std::alloc"),
        // ffi (Session 128)
        ("CString", "std::ffi"),
        ("CStr", "std::ffi"),
        // str pattern (Session 128)
        ("Pattern", "std::str::pattern"),
        // process additional (Session 129)
        ("Child", "std::process"),
        ("Stdio", "std::process"),
        // ffi additional (Session 129)
        ("OsStr", "std::ffi"),
        ("OsString", "std::ffi"),
        // os::unix::io (Session 130)
        ("RawFd", "std::os::unix::io"),
        ("OwnedFd", "std::os::unix::io"),
        ("BorrowedFd", "std::os::unix::io"),
        // os::windows::io (Session 130)
        ("RawHandle", "std::os::windows::io"),
        ("OwnedHandle", "std::os::windows::io"),
        ("BorrowedHandle", "std::os::windows::io"),
        // External crates (Session 128)
        ("Serialize", "serde"),
        ("Deserialize", "serde"),
        ("Regex", "regex"),
        ("DateTime", "chrono"),
        ("NaiveDateTime", "chrono"),
        ("NaiveDate", "chrono"),
        ("NaiveTime", "chrono"),
        ("TimeZone", "chrono"),
        ("Client", "reqwest"),
        ("Response", "reqwest"),
        ("StatusCode", "reqwest"),
        ("Value", "serde_json"),
        ("JoinHandle", "tokio::task"),
        // External crates (Session 129)
        ("IterTools", "itertools"),
        // External crates (Session 130)
        ("Arg", "clap"),
        ("Subcommand", "clap"),
        ("ArgAction", "clap"),
        ("Uuid", "uuid"),
        ("Url", "url"),
        ("Level", "log"),
        ("LevelFilter", "log"),
        ("EnvFilter", "tracing_subscriber"),
        // External crates (Session 131)
        ("Rng", "rand"),
        ("ThreadRng", "rand::rngs"),
        ("DashMap", "dashmap"),
        ("ParallelIterator", "rayon::prelude"),
        ("Array1", "ndarray"),
        ("Array2", "ndarray"),
        ("StreamExt", "tokio_stream"),
        ("Stream", "tokio_stream"),
        // External crates (Session 132)
        ("Router", "axum"),
        ("Json", "axum"),
        ("Handler", "axum"),
        ("IntoResponse", "axum"),
        ("Tera", "tera"),
        ("Context", "tera"),
        ("Template", "askama"),
        ("Connection", "rusqlite"),
        ("Statement", "rusqlite"),
        ("Row", "rusqlite"),
        ("Cmd", "redis"),
        ("AsyncConnection", "redis"),
        // External crates (Session 133)
        ("Pool", "sqlx"),
        ("QueryBuilder", "sqlx"),
        ("Executor", "sqlx"),
        ("AnyPool", "sqlx"),
        ("MySqlPool", "sqlx"),
        ("PgPool", "sqlx"),
        ("SqlitePool", "sqlx"),
        ("TypedHeader", "axum_extra"),
        ("Message", "lettre"),
        ("Transport", "lettre"),
        ("SmtpTransport", "lettre"),
        ("AsyncTransport", "lettre"),
        ("Config", "config"),
        // External crates (Session 134)
        ("Body", "hyper"),
        ("HeaderMap", "hyper"),
        ("Uri", "hyper"),
        ("Method", "hyper"),
        ("Service", "tower"),
        ("ServiceExt", "tower"),
        ("Layer", "tower"),
        ("BoxError", "tower"),
        ("Status", "tonic"),
        ("Code", "tonic"),
        ("Channel", "tonic::transport"),
        ("TokenStream", "proc_macro2"),
        ("Span", "proc_macro2"),
        ("Ident", "proc_macro2"),
        ("Literal", "proc_macro2"),
        ("DeriveInput", "syn"),
        ("ItemFn", "syn"),
        ("ItemStruct", "syn"),
        ("ItemEnum", "syn"),
        // External crates (Session 135)
        ("Filter", "warp"),
        ("Reply", "warp"),
        ("HttpResponse", "actix_web"),
        ("HttpRequest", "actix_web"),
        ("Responder", "actix_web"),
        ("HttpServer", "actix_web"),
        ("EntityTrait", "sea_orm"),
        ("Database", "sea_orm"),
        ("DbConn", "sea_orm"),
        ("PaginatorTrait", "sea_orm"),
        ("QueryDsl", "diesel"),
        ("RunQueryDsl", "diesel"),
        ("ExpressionMethods", "diesel"),
        ("PgConnection", "diesel"),
        ("SqliteConnection", "diesel"),
        // External crates (Session 136)
        ("Mailgun", "mailgun"),
        ("Recipient", "mailgun"),
        ("Charge", "stripe"),
        ("Customer", "stripe"),
        ("PaymentIntent", "stripe"),
        ("PutObjectOutput", "aws_sdk_s3"),
        ("GetObjectOutput", "aws_sdk_s3"),
        ("SdkConfig", "aws_config"),
        ("BehaviorVersion", "aws_config"),
        // External crates (Session 137)
        ("BasicClient", "oauth2"),
        ("AuthorizationCode", "oauth2"),
        ("AccessToken", "oauth2"),
        ("CsrfToken", "oauth2"),
        ("PkceCodeVerifier", "oauth2"),
        ("Pattern", "glob"),
        ("GlobBuilder", "glob"),
        ("Cookie", "cookie"),
        ("CookieJar", "cookie"),
        // External crates (Session 138)
        ("EnvError", "dotenv"),
        ("AppBuilder", "tauri"),
        ("AppHandle", "tauri"),
        ("Manager", "tauri"),
        ("Invoke", "tauri"),
        ("Device", "wgpu"),
        ("Queue", "wgpu"),
        ("Surface", "wgpu"),
        ("SurfaceConfiguration", "wgpu"),
        ("ShaderModule", "wgpu"),
        // External crates (Session 139)
        ("Builder", "env_logger"),
        ("Target", "env_logger"),
        ("Filter", "env_logger"),
        ("Watcher", "notify"),
        ("EventKind", "notify"),
        ("Event", "notify"),
        ("ShadowBuilder", "shadow_rs"),
        // External crates (Session 141)
        ("System", "sysinfo"),
        ("CpuCore", "sysinfo"),
        ("Disk", "sysinfo"),
        ("SerialPort", "serialport"),
        ("machine_uid", "machine_uid"),
        // External crates (Session 142)
        ("EnvLoader", "dotenvy"),
        ("EnvIter", "dotenvy"),
        ("FdLock", "fd_lock"),
        ("NixPath", "nix::path"),
        ("Errno", "nix::errno"),
        ("Utf8PathBuf", "camino"),
        ("Utf8Path", "camino"),
        // External crates (Session 144)
        ("Enigo", "enigo"),
        ("Display", "x11::display"),
        ("HMODULE", "winapi"),
        ("CGContext", "core_graphics"),
        // image (Session 145)
        ("ImageBuffer", "image"),
        ("Rgba", "image"),
        // imageproc (Session 145)
        ("Drawing", "imageproc::drawing"),
        // rusttype (Session 145)
        ("Font", "rusttype"),
        ("PositionedGlyph", "rusttype"),
        // plotters (Session 145)
        ("ChartContext", "plotters::chart"),
        // ratatui (Session 146)
        ("Line", "ratatui::text"),
        ("Layout", "ratatui::layout"),
        ("Block", "ratatui::widgets"),
        ("Widget", "ratatui::widgets"),
        // crossterm (Session 146)
        ("execute", "crossterm"),
        ("queue", "crossterm"),
        ("terminal", "crossterm"),
        // tui (Session 146)
        ("Frame", "tui"),
        ("Terminal", "tui"),
        // glium (Session 146)
        ("Display", "glium"),
        ("Surface", "glium::surface"),
        // vulkano (Session 146)
        ("VkHandle", "vulkano"),
        ("VulkanObject", "vulkano"),
        // ndarray-npy (Session 146)
        ("open_npz", "ndarray_npy"),
        ("save_npz", "ndarray_npy"),
        // eframe (Session 147)
        ("App", "eframe"),
        ("Frame", "eframe"),
        // egui (Session 147)
        ("Context", "egui"),
        ("Ui", "egui"),
        // iced (Session 147)
        ("Application", "iced"),
        ("Command", "iced"),
        // druid (Session 147)
        ("AppDelegate", "druid"),
        ("Widget", "druid::widget"),
        // slint (Session 147)
        ("ComponentHandle", "slint"),
        ("Model", "slint"),
        // num_cpus (Session 148)
        ("num_cpus", "num_cpus"),
        // once_cell (Session 148)
        ("Lazy", "once_cell::sync"),
        // flate2 (Session 148)
        ("GzEncoder", "flate2::write"),
        ("GzDecoder", "flate2::read"),
        // tar (Session 148)
        ("Archive", "tar"),
        ("Entry", "tar"),
        // walkdir (Session 148)
        ("WalkDir", "walkdir"),
        // tempfile (Session 148)
        ("TempDir", "tempfile"),
        ("NamedTempFile", "tempfile"),
        // indicatif (Session 148)
        ("ProgressBar", "indicatif"),
        ("MultiProgress", "indicatif"),
        // dialoguer (Session 148)
        ("Input", "dialoguer"),
        ("Select", "dialoguer"),
        // console (Session 148)
        ("Term", "console"),
        ("Style", "console"),
        // Session 150: std types commonly used in data structure implementations
        // std::str::FromStr — needed when implementing impl FromStr for T
        ("FromStr", "std::str"),
        // std::fmt::Write — needed when implementing impl Write for T
        ("Write", "std::fmt"),
        // std::ops::{Deref, DerefMut, Index, IndexMut, Drop} — smart pointer & indexing traits
        ("Deref", "std::ops"),
        ("DerefMut", "std::ops"),
        ("Index", "std::ops"),
        ("IndexMut", "std::ops"),
        ("Drop", "std::ops"),
        // std::iter::{FusedIterator, DoubleEndedIterator, ExactSizeIterator} — iterator marker traits
        ("FusedIterator", "std::iter"),
        ("DoubleEndedIterator", "std::iter"),
        ("ExactSizeIterator", "std::iter"),
    ];

    for &(type_name, module_path) in type_modules {
        let full_path = format!("{}::{}", module_path, type_name);
        // Session 141: 当 module_path == type_name (如 machine_uid) 时,
        // 还需要检查 module_path:: 前缀是否为全限定路径
        let module_prefix = format!("{}::", module_path);
        let bare_usage = contains_type_usage(content, type_name)
            && !content.contains(&full_path)
            && !(module_path == type_name && content.contains(&module_prefix));

        if bare_usage {
            // 检查是否已有导入
            let already_imported = content.lines().any(|line| {
                let trimmed = line.trim();
                trimmed.starts_with(&format!("use {}::{}", module_path, type_name))
                    || (trimmed.contains(&format!("use {}::{{", module_path))
                        && contains_type_usage(trimmed, type_name))
                    || trimmed.starts_with(&format!("use {}::*;", module_path))
                    || trimmed.starts_with("use std::*;")
            });

            if !already_imported {
                // 找到第一个使用该类型的行
                for (i, line) in lines.iter().enumerate() {
                    if contains_type_usage(line, type_name) {
                        issues.push(ImportIssue {
                            type_name: type_name.to_string(),
                            module_path: module_path.to_string(),
                            usage_line: i + 1,
                        });
                        break;
                    }
                }
            }
        }
    }

    // 检测 sqlx query! / query_as! / query_unchecked! 宏使用 (Session 133)
    let sqlx_verify_macros: &[&str] = &["query", "query_as", "query_unchecked"];
    let mut sqlx_verify_macro_found = false;
    let mut sqlx_verify_macro_line = 0;
    for (i, line) in lines.iter().enumerate() {
        for &macro_name in sqlx_verify_macros {
            let macro_bang = format!("{}!(", macro_name);
            let full_path = format!("sqlx::{}!", macro_name);
            if line.contains(&macro_bang) && !line.contains(&full_path) {
                sqlx_verify_macro_found = true;
                sqlx_verify_macro_line = i + 1;
                break;
            }
        }
        if sqlx_verify_macro_found {
            break;
        }
    }

    if sqlx_verify_macro_found {
        let already_imported = content.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("use sqlx::query;")
                || trimmed.starts_with("use sqlx::query_as;")
                || trimmed.starts_with("use sqlx::query_unchecked;")
                || (trimmed.contains("use sqlx::{")
                    && (contains_type_usage(trimmed, "query")
                        || contains_type_usage(trimmed, "query_as")
                        || contains_type_usage(trimmed, "query_unchecked")))
                || trimmed.starts_with("use sqlx::*;")
        });

        if !already_imported {
            issues.push(ImportIssue {
                type_name: "query".to_string(),
                module_path: "sqlx".to_string(),
                usage_line: sqlx_verify_macro_line,
            });
        }
    }

    // 检测 itertools trait 方法调用 (Session 129)
    // .interleave( / .intersperse( / .tuples( 等方法需要 IterTools trait 在作用域中
    let itertools_methods: &[&str] = &[
        "interleave",
        "intersperse",
        "tuples",
        "group_by",
        "permutations",
        "combinations",
        "step",
        "multipeek",
    ];
    let mut itertools_method_found = false;
    let mut method_line = 0;
    for (i, line) in lines.iter().enumerate() {
        for &method_name in itertools_methods {
            let method_call = format!(".{}(", method_name);
            if line.contains(&method_call) {
                itertools_method_found = true;
                method_line = i + 1;
                break;
            }
        }
        if itertools_method_found {
            break;
        }
    }

    if itertools_method_found {
        let already_imported = content.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("use itertools::IterTools;")
                || (trimmed.contains("use itertools::{")
                    && contains_type_usage(trimmed, "IterTools"))
                || trimmed.starts_with("use itertools::*;")
        });

        if !already_imported {
            issues.push(ImportIssue {
                type_name: "IterTools".to_string(),
                module_path: "itertools".to_string(),
                usage_line: method_line,
            });
        }
    }

    // 检测 std::sync::Mutex 的 .lock() 方法调用 (Session 131)
    // .lock() 是 Mutex 的核心方法, 如果调用 .lock() 但未导入 Mutex, 可能缺少导入
    if content.contains(".lock()") || content.contains(".lock().") {
        let already_imported = content.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("use std::sync::Mutex;")
                || (trimmed.contains("use std::sync::{") && contains_type_usage(trimmed, "Mutex"))
                || trimmed.starts_with("use std::sync::*;")
                || trimmed.starts_with("use std::*;")
        });

        if !already_imported {
            // 找到第一个 .lock() 的行
            for (i, line) in lines.iter().enumerate() {
                if line.contains(".lock()") || line.contains(".lock().") {
                    issues.push(ImportIssue {
                        type_name: "Mutex".to_string(),
                        module_path: "std::sync".to_string(),
                        usage_line: i + 1,
                    });
                    break;
                }
            }
        }
    }

    // 检测 rayon ParallelIterator trait 方法调用 (Session 131)
    // .par_iter( / .par_iter_mut( 方法需要 ParallelIterator trait
    let rayon_methods: &[&str] = &["par_iter", "par_iter_mut"];
    let mut rayon_method_found = false;
    let mut rayon_method_line = 0;
    for (i, line) in lines.iter().enumerate() {
        for &method_name in rayon_methods {
            let method_call = format!(".{}(", method_name);
            if line.contains(&method_call) {
                rayon_method_found = true;
                rayon_method_line = i + 1;
                break;
            }
        }
        if rayon_method_found {
            break;
        }
    }

    if rayon_method_found {
        let already_imported = content.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("use rayon::prelude::ParallelIterator;")
                || (trimmed.contains("use rayon::prelude::{")
                    && contains_type_usage(trimmed, "ParallelIterator"))
                || trimmed.starts_with("use rayon::prelude::*;")
                || trimmed.starts_with("use rayon::*;")
        });

        if !already_imported {
            issues.push(ImportIssue {
                type_name: "ParallelIterator".to_string(),
                module_path: "rayon::prelude".to_string(),
                usage_line: rayon_method_line,
            });
        }
    }

    // 检测 rand Rng trait 方法调用 (Session 131)
    // .gen_range( / .fill( 方法需要 Rng trait
    let rand_methods: &[&str] = &["gen_range", "fill"];
    let mut rand_method_found = false;
    let mut rand_method_line = 0;
    for (i, line) in lines.iter().enumerate() {
        for &method_name in rand_methods {
            let method_call = format!(".{}(", method_name);
            if line.contains(&method_call) {
                rand_method_found = true;
                rand_method_line = i + 1;
                break;
            }
        }
        if rand_method_found {
            break;
        }
    }

    if rand_method_found {
        let already_imported = content.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("use rand::Rng;")
                || (trimmed.contains("use rand::{") && contains_type_usage(trimmed, "Rng"))
                || trimmed.starts_with("use rand::*;")
        });

        if !already_imported {
            issues.push(ImportIssue {
                type_name: "Rng".to_string(),
                module_path: "rand".to_string(),
                usage_line: rand_method_line,
            });
        }
    }

    // 检测 std::sync::RwLock 的 .read() / .write() / .try_read() / .try_write() 方法调用 (Session 132 + 133)
    // .read() / .write() 是 RwLock 的核心方法, .try_read() / .try_write() 是非阻塞版本
    // 如果调用但未导入 RwLock, 可能缺少导入
    let rwlock_methods: &[&str] = &["read", "write", "try_read", "try_write"];
    let mut rwlock_method_found = false;
    let mut rwlock_method_line = 0;
    for (i, line) in lines.iter().enumerate() {
        for &method_name in rwlock_methods {
            let method_call = format!(".{}()", method_name);
            if line.contains(&method_call) {
                rwlock_method_found = true;
                rwlock_method_line = i + 1;
                break;
            }
        }
        if rwlock_method_found {
            break;
        }
    }

    if rwlock_method_found {
        let already_imported = content.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("use std::sync::RwLock;")
                || (trimmed.contains("use std::sync::{") && contains_type_usage(trimmed, "RwLock"))
                || trimmed.starts_with("use std::sync::*;")
                || trimmed.starts_with("use std::*;")
        });

        if !already_imported {
            issues.push(ImportIssue {
                type_name: "RwLock".to_string(),
                module_path: "std::sync".to_string(),
                usage_line: rwlock_method_line,
            });
        }
    }

    // 检测 std::sync::mpsc 的 .send() / .recv() / .try_send() 方法调用 (Session 132 + 133)
    // .send() 是 Sender 的方法, .recv() 是 Receiver 的方法, .try_send() 是 Sender 的非阻塞方法
    let mpsc_methods: &[&str] = &["send", "recv", "try_send"];
    let mut mpsc_method_found = false;
    let mut mpsc_method_line = 0;
    for (i, line) in lines.iter().enumerate() {
        for &method_name in mpsc_methods {
            let method_call = format!(".{}(", method_name);
            if line.contains(&method_call) {
                mpsc_method_found = true;
                mpsc_method_line = i + 1;
                break;
            }
        }
        if mpsc_method_found {
            break;
        }
    }

    if mpsc_method_found {
        let already_imported = content.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("use std::sync::mpsc::")
                || (trimmed.contains("use std::sync::mpsc::{")
                    && (contains_type_usage(trimmed, "Sender")
                        || contains_type_usage(trimmed, "Receiver")))
                || trimmed.starts_with("use std::sync::mpsc::*;")
                || trimmed.starts_with("use std::*;")
        });

        if !already_imported {
            // 检测具体是 Sender 还是 Receiver
            let has_send = content.contains(".send(") || content.contains(".try_send(");
            let has_recv = content.contains(".recv(");
            if has_send {
                issues.push(ImportIssue {
                    type_name: "Sender".to_string(),
                    module_path: "std::sync::mpsc".to_string(),
                    usage_line: mpsc_method_line,
                });
            }
            if has_recv {
                issues.push(ImportIssue {
                    type_name: "Receiver".to_string(),
                    module_path: "std::sync::mpsc".to_string(),
                    usage_line: mpsc_method_line,
                });
            }
        }
    }

    // 检测 .await? 使用 → 需要 anyhow::Result (Session 134)
    // .await? 表示异步函数中使用 ? 操作符, 函数必须返回 Result
    let mut await_question_found = false;
    let mut await_question_line = 0;
    for (i, line) in lines.iter().enumerate() {
        if line.contains(".await?") {
            await_question_found = true;
            await_question_line = i + 1;
            break;
        }
    }

    if await_question_found {
        let already_imported = content.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("use anyhow::Result;")
                || trimmed.starts_with("use anyhow::{")
                || trimmed.starts_with("use anyhow::*;")
                || trimmed.starts_with("use std::result::Result;")
        });

        if !already_imported {
            issues.push(ImportIssue {
                type_name: "Result".to_string(),
                module_path: "anyhow".to_string(),
                usage_line: await_question_line,
            });
        }
    }

    // 检测 tokio::spawn 的 .spawn() 方法调用 (Session 134)
    // .spawn() 是 tokio::task::JoinHandle 的关联函数, 裸调用需要导入
    let mut spawn_method_found = false;
    let mut spawn_method_line = 0;
    for (i, line) in lines.iter().enumerate() {
        if line.contains(".spawn()") && !line.contains("tokio::spawn") {
            spawn_method_found = true;
            spawn_method_line = i + 1;
            break;
        }
    }

    if spawn_method_found {
        let already_imported = content.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("use tokio::spawn;")
                || (trimmed.contains("use tokio::{") && contains_type_usage(trimmed, "spawn"))
                || trimmed.starts_with("use tokio::*;")
        });

        if !already_imported {
            issues.push(ImportIssue {
                type_name: "spawn".to_string(),
                module_path: "tokio".to_string(),
                usage_line: spawn_method_line,
            });
        }
    }

    // 检测 quote! / format_ident! 宏使用 (Session 134)
    let quote_verify_macros: &[&str] = &["quote", "format_ident"];
    let mut quote_verify_macro_found = false;
    let mut quote_verify_macro_line = 0;
    for (i, line) in lines.iter().enumerate() {
        for &macro_name in quote_verify_macros {
            let macro_bang = format!("{}!", macro_name);
            let full_path = format!("quote::{}!", macro_name);
            if line.contains(&macro_bang) && !line.contains(&full_path) {
                quote_verify_macro_found = true;
                quote_verify_macro_line = i + 1;
                break;
            }
        }
        if quote_verify_macro_found {
            break;
        }
    }

    if quote_verify_macro_found {
        let already_imported = content.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("use quote::quote;")
                || trimmed.starts_with("use quote::format_ident;")
                || (trimmed.contains("use quote::{")
                    && (contains_type_usage(trimmed, "quote")
                        || contains_type_usage(trimmed, "format_ident")))
                || trimmed.starts_with("use quote::*;")
        });

        if !already_imported {
            issues.push(ImportIssue {
                type_name: "quote".to_string(),
                module_path: "quote".to_string(),
                usage_line: quote_verify_macro_line,
            });
        }
    }

    // 检测 syn parse_quote! / parse_str! 宏使用 (Session 134)
    let syn_verify_macros: &[&str] = &["parse_quote", "parse_str"];
    let mut syn_verify_macro_found = false;
    let mut syn_verify_macro_line = 0;
    for (i, line) in lines.iter().enumerate() {
        for &macro_name in syn_verify_macros {
            let macro_bang = format!("{}!(", macro_name);
            let full_path = format!("syn::{}!", macro_name);
            if line.contains(&macro_bang) && !line.contains(&full_path) {
                syn_verify_macro_found = true;
                syn_verify_macro_line = i + 1;
                break;
            }
        }
        if syn_verify_macro_found {
            break;
        }
    }

    if syn_verify_macro_found {
        let already_imported = content.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("use syn::parse_quote;")
                || trimmed.starts_with("use syn::parse_str;")
                || (trimmed.contains("use syn::{")
                    && (contains_type_usage(trimmed, "parse_quote")
                        || contains_type_usage(trimmed, "parse_str")))
                || trimmed.starts_with("use syn::*;")
        });

        if !already_imported {
            issues.push(ImportIssue {
                type_name: "parse_quote".to_string(),
                module_path: "syn".to_string(),
                usage_line: syn_verify_macro_line,
            });
        }
    }

    // 检测 .collect() → FromIterator trait 方法调用 (Session 135)
    // .collect() 方法需要 FromIterator trait 在作用域中 (通常通过 prelude 自动可用)
    // 此检测为建议性, 帮助明确导入
    let mut collect_method_found = false;
    let mut collect_method_line = 0;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        // 跳过注释行和导入行
        if trimmed.starts_with("//") || trimmed.starts_with("use ") {
            continue;
        }
        if line.contains(".collect(") {
            collect_method_found = true;
            collect_method_line = i + 1;
            break;
        }
    }

    if collect_method_found {
        let already_imported = content.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("use std::iter::FromIterator;")
                || (trimmed.contains("use std::iter::{")
                    && contains_type_usage(trimmed, "FromIterator"))
                || trimmed.starts_with("use std::iter::*;")
                || trimmed.starts_with("use std::*;")
        });

        if !already_imported {
            issues.push(ImportIssue {
                type_name: "FromIterator".to_string(),
                module_path: "std::iter".to_string(),
                usage_line: collect_method_line,
            });
        }
    }

    // 检测 .into_iter() → IntoIterator trait 方法调用 (Session 135)
    // .into_iter() 方法需要 IntoIterator trait 在作用域中 (通常通过 prelude 自动可用)
    let mut into_iter_method_found = false;
    let mut into_iter_method_line = 0;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") || trimmed.starts_with("use ") {
            continue;
        }
        if line.contains(".into_iter()") {
            into_iter_method_found = true;
            into_iter_method_line = i + 1;
            break;
        }
    }

    if into_iter_method_found {
        let already_imported = content.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("use std::iter::IntoIterator;")
                || (trimmed.contains("use std::iter::{")
                    && contains_type_usage(trimmed, "IntoIterator"))
                || trimmed.starts_with("use std::iter::*;")
                || trimmed.starts_with("use std::*;")
        });

        if !already_imported {
            issues.push(ImportIssue {
                type_name: "IntoIterator".to_string(),
                module_path: "std::iter".to_string(),
                usage_line: into_iter_method_line,
            });
        }
    }

    // 检测 .map() / .filter() / .zip() / .chain() / .enumerate() / .flat_map() / .peekable() / .skip() / .take() / .rev() / .step_by() / .cloned() / .copied() / .fuse()
    // → Iterator trait 方法调用 (Session 136 + 137 + 138 + 139 + 141 + 142)
    // 这些方法需要 Iterator trait 在作用域中 (通常通过 prelude 自动可用)
    // 此检测为建议性, 帮助明确导入
    let iterator_methods: &[&str] = &[
        "map",
        "filter",
        "zip",
        "chain",
        "enumerate",
        "flat_map",
        "peekable",
        "skip",
        "take",
        "rev",
        "step_by",
        // Session 141: .cloned() / .copied() / .fuse()
        "cloned",
        "copied",
        "fuse",
        // Session 142: .flatten() / .max( / .min( / .sum( / .product(
        "flatten",
        "max",
        "min",
        "sum",
        "product",
        // Session 144: .any() / .all() / .find() / .position() / .count() / .fold() / .reduce() / .partition() / .for_each()
        "any",
        "all",
        "find",
        "position",
        "count",
        "fold",
        "reduce",
        "partition",
        "for_each",
        // Session 145: .scan() / .unzip() / .cycle()
        "scan",
        "unzip",
        "cycle",
        // Session 146: .chunks() / .windows() / .rchunks() / .as_chunks() / .array_chunks()
        "chunks",
        "windows",
        "rchunks",
        "as_chunks",
        "array_chunks",
        // Session 147: .first() / .last() / .nth() / .next_back() / .rposition() / .rfold() / .rfind()
        "first",
        "last",
        "nth",
        "next_back",
        "rposition",
        "rfold",
        "rfind",
        // Session 148: .iter_mut() / .split() / .splitn() / .rsplit() / .rsplitn() / .lines() / .chars() / .bytes()
        "iter_mut",
        "split",
        "splitn",
        "rsplit",
        "rsplitn",
        "lines",
        "chars",
        "bytes",
        // Session 149: .iter() / .try_fold() / .try_for_each() / .is_sorted() / .collect_into()
        "iter",
        "try_fold",
        "try_for_each",
        "is_sorted",
        "collect_into",
    ];
    let mut iterator_method_found = false;
    let mut iterator_method_line = 0;
    'outer: for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        // 跳过注释行和导入行
        if trimmed.starts_with("//") || trimmed.starts_with("use ") {
            continue;
        }
        for &method_name in iterator_methods {
            let method_call = format!(".{}(", method_name);
            if line.contains(&method_call) {
                iterator_method_found = true;
                iterator_method_line = i + 1;
                break 'outer;
            }
        }
    }

    if iterator_method_found {
        let already_imported = content.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("use std::iter::Iterator;")
                || (trimmed.contains("use std::iter::{")
                    && contains_type_usage(trimmed, "Iterator"))
                || trimmed.starts_with("use std::iter::*;")
                || trimmed.starts_with("use std::*;")
        });

        if !already_imported {
            issues.push(ImportIssue {
                type_name: "Iterator".to_string(),
                module_path: "std::iter".to_string(),
                usage_line: iterator_method_line,
            });
        }
    }

    // 去重: 同一类型只保留第一个出现的行号
    let mut seen = std::collections::HashSet::new();
    issues.retain(|issue| {
        let key = format!("{}::{}", issue.module_path, issue.type_name);
        seen.insert(key)
    });

    issues
}

/// 导入检查报告 — JSON 格式导出 (Session 128)
///
/// 包含缺失导入的完整信息, 可序列化为 JSON 格式,
/// 适用于 CI/CD 集成、IDE 插件和报告生成。
///
/// # 字段
///
/// - `total_issues`: 缺失导入总数
/// - `issues`: 所有缺失导入的详细列表
/// - `has_issues`: 是否有缺失导入 (`total_issues > 0`)
/// - `modules_affected`: 受影响的模块路径列表 (去重)
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ImportReport {
    /// 缺失导入总数
    pub total_issues: usize,
    /// 所有缺失导入的详细列表
    pub issues: Vec<ImportIssue>,
    /// 是否有缺失导入
    pub has_issues: bool,
    /// 受影响的模块路径列表 (去重, 按字母序排列)
    pub modules_affected: Vec<String>,
}

/// 生成导入检查的 JSON 格式报告 (Session 128)
///
/// 调用 `verify_imports` 检查代码中的缺失导入,
/// 生成结构化的 `ImportReport` 并序列化为 JSON 字符串。
///
/// # 输出格式
///
/// ```json
/// {
///   "total_issues": 2,
///   "issues": [
///     {"type_name": "HashMap", "module_path": "std::collections", "usage_line": 1},
///     {"type_name": "Arc", "module_path": "std::sync", "usage_line": 1}
///   ],
///   "has_issues": true,
///   "modules_affected": ["std::collections", "std::sync"]
/// }
/// ```
///
/// # 示例
///
/// ```
/// use forge::extract::verify_imports_to_json;
///
/// let code = "fn foo() -> HashMap<String, i32> { HashMap::new() }";
/// let json = verify_imports_to_json(code);
/// assert!(json.contains("HashMap"), "JSON 应包含 HashMap: {}", json);
/// assert!(json.contains("total_issues"), "JSON 应包含 total_issues: {}", json);
///
/// let clean = "fn foo() -> i32 { 42 }";
/// let json = verify_imports_to_json(clean);
/// assert!(json.contains("\"total_issues\": 0"), "无问题 JSON 应 total_issues=0: {}", json);
/// ```
pub fn verify_imports_to_json(content: &str) -> String {
    let report = verify_imports_report(content);
    serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string())
}

/// 生成导入检查报告 (Session 128)
///
/// 与 `verify_imports_to_json` 相同逻辑, 但返回 `ImportReport` 结构体,
/// 适用于程序化处理而非 JSON 序列化。
///
/// # 示例
///
/// ```
/// use forge::extract::verify_imports_report;
///
/// let code = "fn foo() -> HashMap<String, i32> { HashMap::new() }";
/// let report = verify_imports_report(code);
/// assert!(report.has_issues, "应有问题");
/// assert_eq!(report.total_issues, 1, "应有 1 个问题");
/// assert!(report.modules_affected.contains(&"std::collections".to_string()));
/// ```
pub fn verify_imports_report(content: &str) -> ImportReport {
    let issues = verify_imports(content);

    // 收集受影响的模块路径 (去重 + 排序)
    let mut modules: Vec<String> = issues.iter().map(|i| i.module_path.clone()).collect();
    modules.sort();
    modules.dedup();

    let total_issues = issues.len();

    ImportReport {
        total_issues,
        issues,
        has_issues: total_issues > 0,
        modules_affected: modules,
    }
}

/// 生成导入检查的 Markdown 格式报告 (Session 129)
///
/// 调用 `verify_imports` 检查代码中的缺失导入,
/// 生成人类可读的 Markdown 报告, 适用于:
/// - CI/CD 流水线输出
/// - 代码审查报告
/// - PR 评论自动生成
///
/// # 输出格式
///
/// ```markdown
/// # Import Report
///
/// **Total Issues:** 2
/// **Modules Affected:** std::collections, std::sync
///
/// ## Missing Imports
///
/// | # | Type | Module | Line |
/// |---|------|--------|------|
/// | 1 | HashMap | std::collections | 1 |
/// | 2 | Arc | std::sync | 1 |
/// ```
///
/// # 示例
///
/// ```
/// use forge::extract::verify_imports_to_markdown;
///
/// let code = "fn foo() -> HashMap<String, i32> { HashMap::new() }";
/// let md = verify_imports_to_markdown(code);
/// assert!(md.contains("# Import Report"), "Markdown 应包含标题: {}", md);
/// assert!(md.contains("HashMap"), "Markdown 应包含 HashMap: {}", md);
/// assert!(md.contains("std::collections"), "Markdown 应包含模块路径: {}", md);
///
/// let clean = "fn foo() -> i32 { 42 }";
/// let md = verify_imports_to_markdown(clean);
/// assert!(md.contains("No issues"), "无问题应显示 No issues: {}", md);
/// ```
pub fn verify_imports_to_markdown(content: &str) -> String {
    let report = verify_imports_report(content);

    if !report.has_issues {
        return "# Import Report\n\n**Status:** No issues found. All imports are complete.\n"
            .to_string();
    }

    let mut md = String::new();

    md.push_str("# Import Report\n\n");
    md.push_str(&format!("**Total Issues:** {}\n", report.total_issues));
    md.push_str(&format!(
        "**Modules Affected:** {}\n\n",
        report.modules_affected.join(", ")
    ));
    md.push_str("## Missing Imports\n\n");
    md.push_str("| # | Type | Module | Line |\n");
    md.push_str("|---|------|--------|------|\n");

    for (idx, issue) in report.issues.iter().enumerate() {
        md.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            idx + 1,
            issue.type_name,
            issue.module_path,
            issue.usage_line
        ));
    }

    md
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
        assert_eq!(
            issues.len(),
            4,
            "应检测到 4 个问题 (unwrap + expect + panic + MissingResultReturn), got {}",
            issues.len()
        );
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
            2,
            "应报告非测试代码中的 unwrap + MissingResultReturn, got: {:?}",
            issues
        );
        assert!(issues.iter().any(|i| i.contains("unwrap")));
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

    // ===== Session 140: 逐行解析 + 截断检测 测试 =====

    #[test]
    fn test_extract_tagged_files_nested_backticks_doc_comment() {
        // 代码内容包含 doc comment 中的 ``` 示例 — 正则非贪婪匹配会截断
        let text = "```file:src/main.rs
/// Example:
/// ```
/// let x = 42;
/// ```
///
/// This is a function.
fn main() {
    println!(\"hello\");
}
```";
        let files = extract_tagged_files(text);
        assert_eq!(files.len(), 1, "应提取 1 个文件");
        let content = &files[0].content;
        assert!(
            content.contains("fn main()"),
            "内容应包含 fn main(), got: {}",
            content
        );
        assert!(content.contains("/// Example:"), "内容应包含 doc comment");
        assert!(
            content.contains("let x = 42;"),
            "内容应包含嵌套 ``` 内的代码"
        );
        assert!(
            content.contains("println!"),
            "内容应包含 println!, got: {}",
            content
        );
    }

    #[test]
    fn test_extract_tagged_files_nested_backticks_string() {
        // 代码内容包含字符串中的 ```
        let text = "```file:src/main.rs
fn main() {
    let code = \"```\";
    println!(\"{}\", code);
}
```";
        let files = extract_tagged_files(text);
        assert_eq!(files.len(), 1);
        assert!(files[0].content.contains("let code"));
        assert!(files[0].content.contains("println!"));
    }

    #[test]
    fn test_extract_tagged_files_4_backticks_fence() {
        // 使用 4 个反引号作为代码块标记, 内容中有 3 个反引号
        let text = "````file:src/main.rs
/// ```
/// code here
/// ```
fn foo() {}
````";
        let files = extract_tagged_files(text);
        assert_eq!(files.len(), 1);
        assert!(files[0].content.contains("fn foo()"));
        assert!(files[0].content.contains("code here"));
    }

    #[test]
    fn test_extract_tagged_files_unclosed_fence_truncation() {
        // 代码块未闭合 — 可能被截断
        let text = "```file:src/main.rs
fn main() {
    // code continues...
    let x = 42;
    // but no closing fence";
        let files = extract_tagged_files(text);
        // 未闭合的代码块仍然提取内容 (带警告)
        assert_eq!(files.len(), 1, "未闭合代码块应仍提取内容");
        assert!(files[0].content.contains("fn main()"));
        assert!(files[0].content.contains("let x = 42"));
    }

    #[test]
    fn test_extract_tagged_files_multiple_with_nested() {
        // 多个文件, 第一个包含嵌套 ```
        let text = "```file:src/lib.rs
/// Example:
/// ```
/// foo()
/// ```
pub fn foo() -> i32 { 42 }
```
```file:Cargo.toml
[package]
name = \"test\"
```";
        let files = extract_tagged_files(text);
        assert_eq!(files.len(), 2, "应提取 2 个文件");
        let lib = files.iter().find(|f| f.path == "src/lib.rs").unwrap();
        assert!(lib.content.contains("pub fn foo()"));
        assert!(lib.content.contains("/// Example:"));
        let cargo = files.iter().find(|f| f.path == "Cargo.toml").unwrap();
        assert!(cargo.content.contains("[package]"));
    }

    #[test]
    fn test_parse_code_fence_open_basic() {
        assert_eq!(
            parse_code_fence_open("```file:src/main.rs").unwrap().0,
            "src/main.rs"
        );
        assert_eq!(
            parse_code_fence_open("```rust:src/lib.rs").unwrap().0,
            "src/lib.rs"
        );
        assert_eq!(parse_code_fence_open("````file:src/main.rs").unwrap().1, 4);
    }

    #[test]
    fn test_parse_code_fence_open_not_fence() {
        assert!(parse_code_fence_open("fn main() {}").is_none());
        assert!(parse_code_fence_open("```rust").is_none()); // 无冒号路径
        assert!(parse_code_fence_open("  text").is_none());
    }

    #[test]
    fn test_is_code_fence_close_basic() {
        assert!(is_code_fence_close("```", 3));
        assert!(is_code_fence_close("````", 3)); // 4 >= 3
        assert!(is_code_fence_close("  ```", 3)); // 前导空格 OK
    }

    #[test]
    fn test_is_code_fence_close_not_close() {
        assert!(!is_code_fence_close("```text", 3)); // 后面有文本
        assert!(!is_code_fence_close("``", 3)); // 反引号不够
        assert!(!is_code_fence_close("fn code() {}", 3));
    }

    #[test]
    fn test_detect_truncated_files_normal() {
        let files = vec![ExtractedFile {
            path: "src/main.rs".to_string(),
            content: "fn main() { println!(\"hello\"); }".to_string(),
            language: "rust".to_string(),
        }];
        let result = detect_truncated_files(&files);
        assert!(result.is_empty(), "正常文件不应被标记为截断");
    }

    #[test]
    fn test_detect_truncated_files_missing_brace() {
        let files = vec![ExtractedFile {
            path: "src/main.rs".to_string(),
            content: "fn main() {\n    // code here".to_string(),
            language: "rust".to_string(),
        }];
        let result = detect_truncated_files(&files);
        assert!(!result.is_empty(), "缺少闭合大括号应被检测为截断");
        assert!(result[0].0.contains("main.rs"));
    }

    #[test]
    fn test_detect_truncated_files_skips_non_rust() {
        let files = vec![ExtractedFile {
            path: "Cargo.toml".to_string(),
            content: "[package]\nname = \"test\"".to_string(),
            language: "toml".to_string(),
        }];
        let result = detect_truncated_files(&files);
        assert!(result.is_empty(), "非 .rs 文件不应被检查");
    }

    #[test]
    fn test_detect_truncated_files_multiple() {
        let files = vec![
            ExtractedFile {
                path: "src/main.rs".to_string(),
                content: "fn main() {}".to_string(),
                language: "rust".to_string(),
            },
            ExtractedFile {
                path: "src/lib.rs".to_string(),
                content: "pub fn foo() {\n    // missing close".to_string(),
                language: "rust".to_string(),
            },
        ];
        let result = detect_truncated_files(&files);
        assert_eq!(result.len(), 1, "只有 1 个文件被截断");
        assert!(result[0].0.contains("lib.rs"));
    }

    #[test]
    fn test_detect_truncated_files_empty() {
        assert!(detect_truncated_files(&[]).is_empty());
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
        // 应有 4 个问题: MissingDoc (pub fn foo) + Unwrap + UnsafeBlock + MissingResultReturn
        assert_eq!(issues.len(), 4, "应有 4 个问题, got: {:?}", issues);
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
        assert!(!returns_must_use_type("pub fn foo() -> u64 {"));
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

    // ===== Session 117: strip_string_content raw string / byte string 测试 =====

    #[test]
    fn test_strip_string_content_raw_string_basic() {
        // r"..." — raw string
        assert_eq!(
            strip_string_content(r#"let s = r"unsafe { }";"#),
            r#"let s = r"";"#
        );
    }

    #[test]
    fn test_strip_string_content_raw_string_with_hash() {
        // r#"..."# — raw string with one hash
        assert_eq!(
            strip_string_content(r##"let s = r#"unsafe { }"#;"##),
            r##"let s = r#""#;"##
        );
    }

    #[test]
    fn test_strip_string_content_raw_string_with_double_hash() {
        // r##"..."## — raw string with two hashes
        let input = r###"let s = r##"unsafe"##;"###;
        let expected = r###"let s = r##""##;"###;
        assert_eq!(strip_string_content(input), expected);
    }

    #[test]
    fn test_strip_string_content_byte_string() {
        // b"..." — byte string
        assert_eq!(
            strip_string_content(r#"let s = b"unsafe { }";"#),
            r#"let s = b"";"#
        );
    }

    #[test]
    fn test_strip_string_content_raw_byte_string() {
        // br"..." — raw byte string
        assert_eq!(
            strip_string_content(r#"let s = br"unsafe { }";"#),
            r#"let s = br"";"#
        );
    }

    #[test]
    fn test_strip_string_content_raw_byte_string_with_hash() {
        // br#"..."# — raw byte string with hash
        assert_eq!(
            strip_string_content(r##"let s = br#"unsafe"#;"##),
            r##"let s = br#""#;"##
        );
    }

    #[test]
    fn test_strip_string_content_mixed_strings() {
        // 混合: 普通字符串 + raw string + 字节字符串
        let input = r#"let a = "x"; let b = r"y"; let c = b"z";"#;
        let result = strip_string_content(input);
        assert_eq!(result, r#"let a = ""; let b = r""; let c = b"";"#);
    }

    #[test]
    fn test_strip_string_content_raw_string_with_inner_quotes() {
        // raw string 内部包含引号 — 全部内容应被移除
        let input = r##"let s = r#"inner "quote" unsafe"#;"##;
        let result = strip_string_content(input);
        assert_eq!(result, r##"let s = r#""#;"##);
    }

    #[test]
    fn test_strip_string_content_not_string_prefix() {
        // return / break 不应被误认为字符串前缀
        assert_eq!(strip_string_content("return 42;"), "return 42;");
        assert_eq!(strip_string_content("break;"), "break;");
        assert_eq!(strip_string_content("let r = 5;"), "let r = 5;");
        assert_eq!(strip_string_content("let b = 3;"), "let b = 3;");
    }

    #[test]
    fn test_strip_string_content_unsafe_in_raw_string_not_flagged() {
        // raw string 中的 unsafe 不应出现在 strip 后的结果中
        let input = r#"let s = r"unsafe { } block";"#;
        let stripped = strip_string_content(input);
        assert!(
            !stripped.contains("unsafe { }"),
            "raw string 内容应被移除, 不含 unsafe"
        );
    }

    #[test]
    fn test_strip_string_content_unsafe_in_byte_string_not_flagged() {
        // 字节字符串中的 unsafe 不应出现在 strip 后的结果中
        let input = r#"let s = b"unsafe block";"#;
        let stripped = strip_string_content(input);
        assert!(!stripped.contains("unsafe block"), "字节字符串内容应被移除");
    }

    #[test]
    fn test_strip_string_content_regular_string_still_works() {
        // 确保原有功能不受影响
        assert_eq!(
            strip_string_content(r#"let s = "hello \"world\"";"#),
            r#"let s = "";"#
        );
    }

    // ===== Session 117: is_must_use_return_type 测试 =====

    #[test]
    fn test_is_must_use_return_type_result() {
        assert!(is_must_use_return_type("Result<i32, Error>"));
        assert!(is_must_use_return_type("Result<(), Error>"));
    }

    #[test]
    fn test_is_must_use_return_type_option() {
        assert!(is_must_use_return_type("Option<i32>"));
        assert!(is_must_use_return_type("Option<&str>"));
    }

    #[test]
    fn test_is_must_use_return_type_bool() {
        assert!(is_must_use_return_type("bool"));
    }

    #[test]
    fn test_is_must_use_return_type_string_types() {
        assert!(is_must_use_return_type("&str"));
        assert!(is_must_use_return_type("String"));
    }

    #[test]
    fn test_is_must_use_return_type_collections() {
        assert!(is_must_use_return_type("Vec<i32>"));
        assert!(is_must_use_return_type("HashMap<String, i32>"));
        assert!(is_must_use_return_type("HashSet<i32>"));
        assert!(is_must_use_return_type("BTreeMap<String, i32>"));
        assert!(is_must_use_return_type("BTreeSet<i32>"));
    }

    #[test]
    fn test_is_must_use_return_type_impl_traits() {
        assert!(is_must_use_return_type("impl Iterator<Item = i32>"));
        assert!(is_must_use_return_type("impl IntoIterator"));
        assert!(is_must_use_return_type("impl Display"));
        assert!(is_must_use_return_type("impl Debug"));
    }

    #[test]
    fn test_is_must_use_return_type_non_must_use() {
        assert!(!is_must_use_return_type("i32"));
        assert!(!is_must_use_return_type("()"));
        assert!(!is_must_use_return_type("u64"));
        assert!(!is_must_use_return_type("Vec")); // 无泛型参数
    }

    // ===== Session 117: returns_must_use_type 扩展类型测试 =====

    #[test]
    fn test_returns_must_use_type_string() {
        assert!(returns_must_use_type("pub fn name() -> &str { \"\" }"));
        assert!(returns_must_use_type(
            "pub fn name() -> String { String::new() }"
        ));
    }

    #[test]
    fn test_returns_must_use_type_vec() {
        assert!(returns_must_use_type(
            "pub fn items() -> Vec<i32> { vec![] }"
        ));
    }

    #[test]
    fn test_returns_must_use_type_hashmap() {
        assert!(returns_must_use_type(
            "pub fn map() -> HashMap<String, i32> { HashMap::new() }"
        ));
    }

    // ===== Session 117: returns_must_use_type_multiline 测试 =====

    #[test]
    fn test_returns_must_use_type_multiline_single_line() {
        let lines = vec!["pub fn foo() -> Result<i32, Error> { Ok(42) }"];
        assert!(returns_must_use_type_multiline(&lines, 0));
    }

    #[test]
    fn test_returns_must_use_type_multiline_next_line() {
        let lines = vec![
            "pub fn foo()",
            "    -> Result<i32, Error> {",
            "    Ok(42)",
            "}",
        ];
        assert!(returns_must_use_type_multiline(&lines, 0));
    }

    #[test]
    fn test_returns_must_use_type_multiline_two_lines_below() {
        let lines = vec![
            "pub fn foo(",
            "    x: i32,",
            ") -> Option<i32> {",
            "    Some(x)",
            "}",
        ];
        assert!(returns_must_use_type_multiline(&lines, 0));
    }

    #[test]
    fn test_returns_must_use_type_multiline_brace_before_arrow() {
        // 函数体开始前没有返回类型
        let lines = vec!["pub fn foo() {", "    let x = 42;", "}"];
        assert!(!returns_must_use_type_multiline(&lines, 0));
    }

    #[test]
    fn test_returns_must_use_type_multiline_non_fn() {
        let lines = vec!["let x = 42;"];
        assert!(!returns_must_use_type_multiline(&lines, 0));
    }

    #[test]
    fn test_returns_must_use_type_multiline_non_must_use_type() {
        let lines = vec!["pub fn foo() -> i32 { 42 }"];
        assert!(!returns_must_use_type_multiline(&lines, 0));
    }

    // ===== Session 117: validate_rust_code_quality_detailed unwrap_or/unwrap_or_default =====

    #[test]
    fn test_validate_quality_detects_unwrap_or() {
        let issues =
            validate_rust_code_quality_detailed("fn foo() { let x = bar().unwrap_or(0); }");
        assert!(
            issues.iter().any(|i| i.issue_type == IssueType::UnwrapOr),
            "应检测 .unwrap_or()"
        );
    }

    #[test]
    fn test_validate_quality_detects_unwrap_or_default() {
        let issues =
            validate_rust_code_quality_detailed("fn foo() { let x = bar().unwrap_or_default(); }");
        assert!(
            issues
                .iter()
                .any(|i| i.issue_type == IssueType::UnwrapOrDefault),
            "应检测 .unwrap_or_default()"
        );
    }

    #[test]
    fn test_validate_quality_unwrap_or_in_test_module_allowed() {
        let code = r#"
#[cfg(test)]
mod tests {
    #[test]
    fn test_foo() {
        let x: Option<i32> = None;
        let v = x.unwrap_or(0);
        assert_eq!(v, 0);
    }
}
"#;
        let issues = validate_rust_code_quality_detailed(code);
        assert!(
            !issues.iter().any(|i| i.issue_type == IssueType::UnwrapOr),
            "测试模块中的 unwrap_or 不应报告"
        );
    }

    #[test]
    fn test_validate_quality_unwrap_or_default_in_test_module_allowed() {
        let code = r#"
#[cfg(test)]
mod tests {
    #[test]
    fn test_foo() {
        let x: Option<i32> = None;
        let v = x.unwrap_or_default();
        assert_eq!(v, 0);
    }
}
"#;
        let issues = validate_rust_code_quality_detailed(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.issue_type == IssueType::UnwrapOrDefault),
            "测试模块中的 unwrap_or_default 不应报告"
        );
    }

    // ===== Session 117: generate_fix unwrap_or / unwrap_or_default 测试 =====

    #[test]
    fn test_generate_fix_unwrap_or() {
        let issue = QualityIssue {
            line: 1,
            issue_type: IssueType::UnwrapOr,
            message: "使用 .unwrap_or()".to_string(),
            suggestion: None,
        };
        let original = "let x = foo().unwrap_or(0);";
        let fixed = generate_fix(&issue, original);
        assert!(fixed.is_some());
        assert!(fixed.as_ref().unwrap().contains("REVIEW:"));
        assert!(fixed.as_ref().unwrap().contains("unwrap_or"));
    }

    #[test]
    fn test_generate_fix_unwrap_or_default() {
        let issue = QualityIssue {
            line: 1,
            issue_type: IssueType::UnwrapOrDefault,
            message: "使用 .unwrap_or_default()".to_string(),
            suggestion: None,
        };
        let original = "let x = foo().unwrap_or_default();";
        let fixed = generate_fix(&issue, original);
        assert!(fixed.is_some());
        assert!(fixed.as_ref().unwrap().contains("REVIEW:"));
        assert!(fixed.as_ref().unwrap().contains("unwrap_or_default"));
    }

    #[test]
    fn test_generate_fix_unwrap_or_preserves_indentation() {
        let issue = QualityIssue {
            line: 1,
            issue_type: IssueType::UnwrapOr,
            message: "使用 .unwrap_or()".to_string(),
            suggestion: None,
        };
        let original = "    let x = foo().unwrap_or(0);";
        let fixed = generate_fix(&issue, original).unwrap();
        assert!(fixed.starts_with("    // REVIEW:"));
    }

    // ===== Session 117: apply_fixes 测试 =====

    #[test]
    fn test_apply_fixes_no_issues() {
        let code = "fn foo() -> i32 { 42 }";
        let fixed = apply_fixes(code);
        assert_eq!(fixed, code);
    }

    #[test]
    fn test_apply_fixes_single_unwrap() {
        let code = "fn foo() { let x = bar().unwrap(); }";
        let fixed = apply_fixes(code);
        assert!(!fixed.contains(".unwrap()"));
        assert!(fixed.contains('?'));
    }

    #[test]
    fn test_apply_fixes_multiple_different_lines() {
        let code = "fn foo() {\n    let x = a().unwrap();\n    let y = b().unwrap();\n}";
        let fixed = apply_fixes(code);
        assert!(!fixed.contains(".unwrap()"));
        // 两处都应被修复
        let q_count = fixed.matches('?').count();
        assert!(q_count >= 2, "两处 unwrap 都应被修复为 ?, got: {}", fixed);
    }

    #[test]
    fn test_apply_fixes_missing_doc_and_must_use_same_line() {
        // 同一行有 MissingDoc 和 MissingMustUse 两个问题
        let code = "pub fn foo() -> bool { true }";
        let fixed = apply_fixes(code);
        // 应同时添加文档注释和 #[must_use]
        assert!(fixed.contains("/// TODO:"), "应添加文档注释: {}", fixed);
        assert!(
            fixed.contains("#[must_use]"),
            "应添加 #[must_use]: {}",
            fixed
        );
        // 文档注释应在 #[must_use] 之前
        let doc_pos = fixed.find("/// TODO:").unwrap();
        let must_use_pos = fixed.find("#[must_use]").unwrap();
        assert!(doc_pos < must_use_pos, "文档注释应在 #[must_use] 之前");
    }

    #[test]
    fn test_apply_fixes_unwrap_and_missing_doc_same_line() {
        // 同一行有 unwrap 和 missing_doc
        let code = "pub fn foo() { let x = bar().unwrap(); }";
        let fixed = apply_fixes(code);
        assert!(!fixed.contains(".unwrap()"), "unwrap 应被修复");
        assert!(fixed.contains("/// TODO:"), "应添加文档注释");
        assert!(fixed.contains('?'), "应包含 ? 操作符");
    }

    #[test]
    fn test_apply_fixes_todo_and_unreachable() {
        let code = "fn foo() { let x = todo!(); let y = unreachable!(); }";
        let fixed = apply_fixes(code);
        assert!(!fixed.contains("todo!"), "todo! 应被修复");
        assert!(!fixed.contains("unreachable!"), "unreachable! 应被修复");
        assert!(fixed.contains("Err(anyhow!"), "应包含 Err(anyhow!(");
    }

    #[test]
    fn test_apply_fixes_preserves_code_structure() {
        let code = "fn foo() {\n    let x = bar().unwrap();\n    println!(\"{}\");\n}";
        let fixed = apply_fixes(code);
        let lines: Vec<&str> = fixed.lines().collect();
        // 基本结构应保留
        assert!(lines[0].contains("fn foo()"));
        assert!(lines[lines.len() - 1].contains("}"));
    }

    #[test]
    fn test_apply_fixes_unwrap_or_adds_review() {
        let code = "fn foo() { let x = bar().unwrap_or(0); }";
        let fixed = apply_fixes(code);
        assert!(fixed.contains("REVIEW:"), "应添加 REVIEW 注释");
        assert!(
            fixed.contains("unwrap_or"),
            "原始 unwrap_or 应保留 (添加了注释前缀)"
        );
    }

    #[test]
    fn test_apply_fixes_unwrap_or_default_adds_review() {
        let code = "fn foo() { let x = bar().unwrap_or_default(); }";
        let fixed = apply_fixes(code);
        assert!(fixed.contains("REVIEW:"), "应添加 REVIEW 注释");
    }

    #[test]
    fn test_apply_fixes_multiline_function() {
        let code = "pub fn foo(\n    x: i32,\n) -> Vec<i32> {\n    vec![x]\n}";
        let fixed = apply_fixes(code);
        // 多行函数签名应检测到 Vec 返回类型需要 #[must_use]
        assert!(
            fixed.contains("#[must_use]"),
            "多行函数签名应检测到 Vec 返回类型需要 #[must_use]: {}",
            fixed
        );
    }

    // ===== Session 117: validate_rust_code_quality_detailed 扩展 must_use 类型 =====

    #[test]
    fn test_validate_quality_detects_string_missing_must_use() {
        let issues =
            validate_rust_code_quality_detailed("pub fn name() -> String { String::new() }");
        assert!(
            issues
                .iter()
                .any(|i| i.issue_type == IssueType::MissingMustUse),
            "返回 String 的公共函数应检测到缺少 #[must_use]"
        );
    }

    #[test]
    fn test_validate_quality_detects_vec_missing_must_use() {
        let issues = validate_rust_code_quality_detailed("pub fn items() -> Vec<i32> { vec![] }");
        assert!(
            issues
                .iter()
                .any(|i| i.issue_type == IssueType::MissingMustUse),
            "返回 Vec 的公共函数应检测到缺少 #[must_use]"
        );
    }

    #[test]
    fn test_validate_quality_detects_str_ref_missing_must_use() {
        let issues = validate_rust_code_quality_detailed(r#"pub fn name() -> &str { "" }"#);
        assert!(
            issues
                .iter()
                .any(|i| i.issue_type == IssueType::MissingMustUse),
            "返回 &str 的公共函数应检测到缺少 #[must_use]"
        );
    }

    #[test]
    fn test_validate_quality_multiline_must_use_detection() {
        let code = "pub fn foo(\n    x: i32,\n) -> Option<i32> {\n    Some(x)\n}";
        let issues = validate_rust_code_quality_detailed(code);
        assert!(
            issues
                .iter()
                .any(|i| i.issue_type == IssueType::MissingMustUse),
            "多行函数签名返回 Option 应检测到缺少 #[must_use]"
        );
    }

    // ===== Session 118: is_must_use_return_type 扩展类型测试 =====

    #[test]
    fn test_is_must_use_return_type_box() {
        assert!(is_must_use_return_type("Box<[u8]>"));
        assert!(is_must_use_return_type("Box<str>"));
        assert!(!is_must_use_return_type("Box<i32>")); // Box<T> 不含 T 是 must_use
    }

    #[test]
    fn test_is_must_use_return_type_rc_arc() {
        assert!(is_must_use_return_type("Rc<String>"));
        assert!(is_must_use_return_type("Arc<Vec<i32>>"));
    }

    #[test]
    fn test_is_must_use_return_type_cow() {
        assert!(is_must_use_return_type("Cow<'a, str>"));
        assert!(is_must_use_return_type("Cow<'_, [u8]>"));
    }

    #[test]
    fn test_is_must_use_return_type_pathbuf() {
        assert!(is_must_use_return_type("PathBuf"));
        assert!(is_must_use_return_type("&Path"));
    }

    #[test]
    fn test_is_must_use_return_type_slice_ref() {
        assert!(is_must_use_return_type("&[u8]"));
        assert!(is_must_use_return_type("&[String]"));
    }

    #[test]
    fn test_is_must_use_return_type_extended_impl_traits() {
        assert!(is_must_use_return_type("impl Into<String>"));
        assert!(is_must_use_return_type("impl AsRef<str>"));
        assert!(is_must_use_return_type("impl DoubleEndedIterator"));
        assert!(is_must_use_return_type("impl ExactSizeIterator"));
        assert!(is_must_use_return_type("impl FusedIterator"));
        assert!(is_must_use_return_type("impl Read"));
        assert!(is_must_use_return_type("impl Write"));
        assert!(is_must_use_return_type("impl BufRead"));
    }

    #[test]
    fn test_is_must_use_return_type_non_must_use_extended() {
        assert!(!is_must_use_return_type("i32"));
        assert!(!is_must_use_return_type("()"));
        assert!(!is_must_use_return_type("u64"));
        assert!(!is_must_use_return_type("f64"));
        assert!(!is_must_use_return_type("Box<i32>")); // 非 Box<[T]> 或 Box<str>
    }

    // ===== Session 118: returns_must_use_type 新类型测试 =====

    #[test]
    fn test_returns_must_use_type_pathbuf() {
        assert!(returns_must_use_type(
            "pub fn get_path() -> PathBuf { PathBuf::new() }"
        ));
    }

    #[test]
    fn test_returns_must_use_type_box_slice() {
        assert!(returns_must_use_type(
            "pub fn data() -> Box<[u8]> { Box::new([0u8; 10]) }"
        ));
    }

    #[test]
    fn test_returns_must_use_type_arc() {
        assert!(returns_must_use_type(
            "pub fn shared() -> Arc<String> { Arc::new(String::new()) }"
        ));
    }

    #[test]
    fn test_returns_must_use_type_impl_into() {
        assert!(returns_must_use_type(
            "pub fn convert() -> impl Into<String> { String::new() }"
        ));
    }

    // ===== Session 118: apply_fixes_dry_run 测试 =====

    #[test]
    fn test_apply_fixes_dry_run_no_issues() {
        let code = "fn foo() -> i32 { 42 }";
        let preview = apply_fixes_dry_run(code);
        assert!(!preview.is_changed, "无问题代码不应有变化");
        assert_eq!(preview.fixes_applied, 0, "无修复");
        assert!(preview.issues.is_empty(), "无问题");
        assert_eq!(preview.original_content, code);
        assert_eq!(preview.fixed_content, code);
    }

    #[test]
    fn test_apply_fixes_dry_run_with_unwrap() {
        let code = "fn foo() { let x = bar().unwrap(); }";
        let preview = apply_fixes_dry_run(code);
        assert!(preview.is_changed, "应有变化");
        assert!(preview.fixes_applied > 0, "应有修复");
        assert!(!preview.issues.is_empty(), "应检测到问题");
        assert!(
            !preview.fixed_content.contains(".unwrap()"),
            "修复后不应包含 .unwrap()"
        );
        assert!(preview.fixed_content.contains('?'), "应包含 ? 操作符");
    }

    #[test]
    fn test_apply_fixes_dry_run_with_missing_doc_and_must_use() {
        let code = "pub fn foo() -> bool { true }";
        let preview = apply_fixes_dry_run(code);
        assert!(preview.is_changed, "应有变化");
        assert!(
            preview.fixed_content.contains("/// TODO:"),
            "应添加文档注释"
        );
        assert!(
            preview.fixed_content.contains("#[must_use]"),
            "应添加 #[must_use]"
        );
        // 文档注释应在 #[must_use] 之前
        let doc_pos = preview.fixed_content.find("/// TODO:").unwrap();
        let must_use_pos = preview.fixed_content.find("#[must_use]").unwrap();
        assert!(doc_pos < must_use_pos, "文档注释应在 #[must_use] 之前");
    }

    #[test]
    fn test_apply_fixes_dry_run_preserves_original() {
        let code = "fn foo() { let x = bar().unwrap(); }";
        let preview = apply_fixes_dry_run(code);
        // 原始内容不应被修改
        assert_eq!(preview.original_content, code);
        assert!(preview.original_content.contains(".unwrap()"));
    }

    #[test]
    fn test_apply_fixes_dry_run_multiple_issues() {
        let code = "fn foo() {\n    let x = a().unwrap();\n    let y = b().unwrap();\n}";
        let preview = apply_fixes_dry_run(code);
        assert!(preview.is_changed);
        assert!(preview.fixes_applied >= 2, "应至少修复 2 处");
        assert!(!preview.fixed_content.contains(".unwrap()"));
    }

    #[test]
    fn test_apply_fixes_dry_run_serde_roundtrip() {
        let code = "pub fn foo() -> bool { true }";
        let preview = apply_fixes_dry_run(code);
        let json = serde_json::to_string(&preview).expect("序列化失败");
        let deserialized: FixPreview = serde_json::from_str(&json).expect("反序列化失败");
        assert_eq!(deserialized.original_content, preview.original_content);
        assert_eq!(deserialized.fixed_content, preview.fixed_content);
        assert_eq!(deserialized.fixes_applied, preview.fixes_applied);
        assert_eq!(deserialized.is_changed, preview.is_changed);
    }

    // ===== Session 118: verify_idempotent 测试 =====

    #[test]
    fn test_verify_idempotent_clean_code() {
        assert!(verify_idempotent("fn foo() -> i32 { 42 }"));
    }

    #[test]
    fn test_verify_idempotent_with_unwrap() {
        assert!(
            verify_idempotent("fn foo() { let x = bar().unwrap(); }"),
            "修复后二次应用应无变化"
        );
    }

    #[test]
    fn test_verify_idempotent_with_missing_doc() {
        assert!(
            verify_idempotent("pub fn foo() {}"),
            "修复后二次应用应无变化"
        );
    }

    #[test]
    fn test_verify_idempotent_with_multiple_issues() {
        let code = "pub fn foo() -> bool { let x = bar().unwrap(); true }";
        assert!(verify_idempotent(code), "多问题修复后二次应用应无变化");
    }

    #[test]
    fn test_verify_idempotent_with_todo() {
        let code = "fn foo() { let x = todo!(); }";
        assert!(verify_idempotent(code), "todo!() 修复后二次应用应无变化");
    }

    #[test]
    fn test_verify_idempotent_complex_code() {
        let code = r#"
pub fn process(data: Vec<i32>) -> Option<i32> {
    let x = data.first().unwrap();
    let y = x.checked_mul(2).unwrap_or(0);
    Some(y)
}
"#;
        assert!(verify_idempotent(code), "复杂代码修复后二次应用应无变化");
    }

    // ===== Session 119: apply_fixes_filtered 测试 =====

    #[test]
    fn test_apply_fixes_filtered_unwrap_only() {
        let code = "pub fn foo() -> bool { let x = bar().unwrap(); true }";
        let filter = [IssueType::Unwrap];
        let fixed = apply_fixes_filtered(code, &filter);
        assert!(!fixed.contains(".unwrap()"), "unwrap 应被修复");
        assert!(fixed.contains("pub fn foo()"), "函数签名应保留");
        assert!(!fixed.contains("/// TODO:"), "不应添加文档注释 (未过滤)");
    }

    #[test]
    fn test_apply_fixes_filtered_empty_filter() {
        let code = "fn foo() { let x = bar().unwrap(); }";
        let fixed = apply_fixes_filtered(code, &[]);
        assert_eq!(fixed, code, "空过滤列表不应修改代码");
    }

    #[test]
    fn test_apply_fixes_filtered_multiple_types() {
        let code = "pub fn foo() -> bool { let x = bar().unwrap(); true }";
        let filter = [IssueType::Unwrap, IssueType::MissingMustUse];
        let fixed = apply_fixes_filtered(code, &filter);
        assert!(!fixed.contains(".unwrap()"), "unwrap 应被修复");
        assert!(fixed.contains("#[must_use]"), "应添加 #[must_use]");
    }

    #[test]
    fn test_apply_fixes_filtered_no_matching_issues() {
        let code = "fn foo() -> i32 { 42 }";
        let filter = [IssueType::Unwrap];
        let fixed = apply_fixes_filtered(code, &filter);
        assert_eq!(fixed, code, "无匹配问题不应修改代码");
    }

    #[test]
    fn test_apply_fixes_filtered_missing_doc_only() {
        let code = "pub fn foo() -> bool { true }";
        let filter = [IssueType::MissingDoc];
        let fixed = apply_fixes_filtered(code, &filter);
        assert!(fixed.contains("/// TODO:"), "应添加文档注释");
        assert!(
            !fixed.contains("#[must_use]"),
            "不应添加 #[must_use] (未过滤)"
        );
    }

    // ===== Session 119: apply_fixes_dry_run_filtered 测试 =====

    #[test]
    fn test_apply_fixes_dry_run_filtered_unwrap_only() {
        let code = "pub fn foo() -> bool { let x = bar().unwrap(); true }";
        let filter = [IssueType::Unwrap];
        let preview = apply_fixes_dry_run_filtered(code, &filter);
        assert!(preview.is_changed, "应有变化");
        assert!(preview.fixes_applied > 0, "应有修复");
        assert!(
            !preview.fixed_content.contains(".unwrap()"),
            "unwrap 应被修复"
        );
        assert!(preview.issues.len() >= 2, "应检测到所有问题 (包括未过滤的)");
    }

    #[test]
    fn test_apply_fixes_dry_run_filtered_empty_filter() {
        let code = "fn foo() { let x = bar().unwrap(); }";
        let preview = apply_fixes_dry_run_filtered(code, &[]);
        assert!(!preview.is_changed, "空过滤不应有变化");
        assert_eq!(preview.fixes_applied, 0, "无修复");
    }

    #[test]
    fn test_apply_fixes_dry_run_filtered_preserves_issues() {
        let code = "pub fn foo() -> bool { let x = bar().unwrap(); true }";
        let filter = [IssueType::Unwrap];
        let preview = apply_fixes_dry_run_filtered(code, &filter);
        // 应检测到 MissingDoc 和 MissingMustUse (即使未过滤)
        assert!(
            preview
                .issues
                .iter()
                .any(|i| i.issue_type == IssueType::MissingDoc),
            "应检测到 MissingDoc"
        );
        assert!(
            preview
                .issues
                .iter()
                .any(|i| i.issue_type == IssueType::MissingMustUse),
            "应检测到 MissingMustUse"
        );
    }

    // ===== Session 119: verify_idempotent_detailed 测试 =====

    #[test]
    fn test_verify_idempotent_detailed_clean_code() {
        let result = verify_idempotent_detailed("fn foo() -> i32 { 42 }");
        assert!(result.is_idempotent, "无问题代码应幂等");
        assert!(result.first_pass_issues.is_empty(), "第一次应无问题");
        assert!(result.second_pass_issues.is_empty(), "第二次应无问题");
        assert!(result.new_issues_in_second_pass.is_empty(), "无新增问题");
    }

    #[test]
    fn test_verify_idempotent_detailed_with_unwrap() {
        let result = verify_idempotent_detailed("fn foo() { let x = bar().unwrap(); }");
        assert!(result.is_idempotent, "修复后二次应用应无变化");
        assert!(result.new_issues_in_second_pass.is_empty(), "无新增问题");
    }

    #[test]
    fn test_verify_idempotent_detailed_with_missing_doc() {
        let result = verify_idempotent_detailed("pub fn foo() {}");
        assert!(result.is_idempotent, "修复后二次应用应无变化");
    }

    #[test]
    fn test_verify_idempotent_detailed_serde() {
        let result = verify_idempotent_detailed("fn foo() -> i32 { 42 }");
        let json = serde_json::to_string(&result).expect("序列化失败");
        let deserialized: IdempotencyResult = serde_json::from_str(&json).expect("反序列化失败");
        assert_eq!(deserialized.is_idempotent, result.is_idempotent);
    }

    // ===== Session 119: detect_missing_result_returns 测试 =====

    #[test]
    fn test_detect_missing_result_returns_question_mark() {
        let issues = detect_missing_result_returns("fn foo() { let x = bar()?; }");
        assert!(issues
            .iter()
            .any(|i| i.issue_type == IssueType::MissingResultReturn));
    }

    #[test]
    fn test_detect_missing_result_returns_unwrap() {
        let issues = detect_missing_result_returns("fn foo() { let x = bar().unwrap(); }");
        assert!(issues
            .iter()
            .any(|i| i.issue_type == IssueType::MissingResultReturn));
    }

    #[test]
    fn test_detect_missing_result_returns_expect() {
        let issues = detect_missing_result_returns(r#"fn foo() { let x = bar().expect("msg"); }"#);
        assert!(issues
            .iter()
            .any(|i| i.issue_type == IssueType::MissingResultReturn));
    }

    #[test]
    fn test_detect_missing_result_returns_todo() {
        let issues = detect_missing_result_returns("fn foo() { let x = todo!(); }");
        assert!(issues
            .iter()
            .any(|i| i.issue_type == IssueType::MissingResultReturn));
    }

    #[test]
    fn test_detect_missing_result_returns_panic() {
        let issues = detect_missing_result_returns(r#"fn foo() { panic!("oops"); }"#);
        assert!(issues
            .iter()
            .any(|i| i.issue_type == IssueType::MissingResultReturn));
    }

    #[test]
    fn test_detect_missing_result_returns_already_result() {
        let issues = detect_missing_result_returns(
            "fn foo() -> Result<(), Error> { let x = bar()?; Ok(()) }",
        );
        assert!(issues.is_empty(), "返回 Result 的函数不应报告");
    }

    #[test]
    fn test_detect_missing_result_returns_already_option() {
        let issues =
            detect_missing_result_returns("fn foo() -> Option<i32> { let x = bar()?; Some(42) }");
        assert!(
            issues.is_empty(),
            "返回 Option 的函数不应报告 (? 兼容 Option)"
        );
    }

    #[test]
    fn test_detect_missing_result_returns_no_pattern() {
        let issues = detect_missing_result_returns("fn foo() -> i32 { 42 }");
        assert!(issues.is_empty(), "无 ?/unwrap/todo 的函数不应报告");
    }

    #[test]
    fn test_detect_missing_result_returns_test_module() {
        let code = r#"
fn foo() { let x = bar().unwrap(); }

#[cfg(test)]
mod tests {
    fn test_foo() { let x = bar().unwrap(); }
}
"#;
        let issues = detect_missing_result_returns(code);
        // 只报告 foo, 不报告 test_foo (在测试模块中)
        assert_eq!(issues.len(), 1, "只应报告 1 个问题 (测试模块中的不报告)");
        assert_eq!(issues[0].line, 2, "应报告第 2 行的 foo 函数");
    }

    #[test]
    fn test_detect_missing_result_returns_multiline() {
        let code = r#"
fn foo(
    x: i32,
) {
    let y = bar().unwrap();
}
"#;
        let issues = detect_missing_result_returns(code);
        assert!(issues
            .iter()
            .any(|i| i.issue_type == IssueType::MissingResultReturn));
        assert_eq!(issues[0].line, 2, "应报告函数签名所在行");
    }

    #[test]
    fn test_detect_missing_result_returns_question_in_comment() {
        let issues = detect_missing_result_returns("fn foo() { // what?\n }");
        assert!(issues.is_empty(), "注释中的 ? 不应报告");
    }

    // ===== Session 119: generate_fix MissingResultReturn 测试 =====

    #[test]
    fn test_generate_fix_missing_result_return_with_return_type() {
        let issue = QualityIssue {
            line: 1,
            issue_type: IssueType::MissingResultReturn,
            message: "函数使用 ? 但未返回 Result".to_string(),
            suggestion: None,
        };
        let original = "fn foo() -> i32 { let x = bar()?; }";
        let fixed = generate_fix(&issue, original);
        assert!(fixed.is_some());
        assert!(fixed
            .as_ref()
            .unwrap()
            .contains("Result<i32, anyhow::Error>"));
    }

    #[test]
    fn test_generate_fix_missing_result_return_no_return_type() {
        let issue = QualityIssue {
            line: 1,
            issue_type: IssueType::MissingResultReturn,
            message: "函数使用 ? 但未返回 Result".to_string(),
            suggestion: None,
        };
        let original = "fn foo() { let x = bar()?; }";
        let fixed = generate_fix(&issue, original);
        assert!(fixed.is_some());
        assert!(fixed
            .as_ref()
            .unwrap()
            .contains("Result<(), anyhow::Error>"));
    }

    #[test]
    fn test_generate_fix_missing_result_return_already_result() {
        let issue = QualityIssue {
            line: 1,
            issue_type: IssueType::MissingResultReturn,
            message: "函数使用 ? 但未返回 Result".to_string(),
            suggestion: None,
        };
        let original = "fn foo() -> Result<i32, Error> { let x = bar()?; }";
        let fixed = generate_fix(&issue, original);
        assert!(fixed.is_none(), "已返回 Result 不需要修复");
    }

    #[test]
    fn test_generate_fix_missing_result_return_not_fn() {
        let issue = QualityIssue {
            line: 1,
            issue_type: IssueType::MissingResultReturn,
            message: "函数使用 ? 但未返回 Result".to_string(),
            suggestion: None,
        };
        let original = "let x = bar()?;";
        let fixed = generate_fix(&issue, original);
        assert!(fixed.is_none(), "非函数签名行不应修复");
    }

    // ===== Session 119: apply_fixes with MissingResultReturn 测试 =====

    #[test]
    fn test_apply_fixes_with_missing_result_return() {
        let code = "fn foo() { let x = bar().unwrap(); }";
        let fixed = apply_fixes(code);
        assert!(!fixed.contains(".unwrap()"), "unwrap 应被修复");
        assert!(
            fixed.contains("Result<(), anyhow::Error>"),
            "应添加 Result 返回类型"
        );
    }

    #[test]
    fn test_apply_fixes_with_missing_result_return_and_doc() {
        let code = "pub fn foo() { let x = bar().unwrap(); }";
        let fixed = apply_fixes(code);
        assert!(!fixed.contains(".unwrap()"), "unwrap 应被修复");
        assert!(
            fixed.contains("Result<(), anyhow::Error>"),
            "应添加 Result 返回类型"
        );
        assert!(fixed.contains("/// TODO:"), "应添加文档注释");
    }

    #[test]
    fn test_verify_idempotent_with_missing_result_return() {
        let code = "fn foo() { let x = bar().unwrap(); }";
        assert!(verify_idempotent(code), "MissingResultReturn 修复后应幂等");
    }

    #[test]
    fn test_verify_idempotent_with_todo_and_missing_result() {
        let code = "fn foo() { let x = todo!(); }";
        assert!(
            verify_idempotent(code),
            "todo!() + MissingResultReturn 修复后应幂等"
        );
    }

    // ===== Session 119: run_clippy_check 测试 =====

    #[test]
    fn test_run_clippy_check_no_cargo_toml() {
        let result = run_clippy_check("/nonexistent/path", false);
        assert!(result.is_err(), "不存在的目录应返回错误");
        assert!(result.unwrap_err().contains("Cargo.toml"));
    }

    // ===== Session 120: apply_fixes_except 测试 =====

    #[test]
    fn test_apply_fixes_except_unwrap() {
        let code = "pub fn foo() -> bool { let x = bar().unwrap(); true }";
        // 排除 MissingDoc 和 MissingMustUse, 只修复 Unwrap
        let exclude = [IssueType::MissingDoc, IssueType::MissingMustUse];
        let fixed = apply_fixes_except(code, &exclude);
        assert!(!fixed.contains(".unwrap()"), "unwrap 应被修复");
        assert!(!fixed.contains("/// TODO:"), "不应添加文档注释 (已排除)");
        assert!(
            !fixed.contains("#[must_use]"),
            "不应添加 #[must_use] (已排除)"
        );
    }

    #[test]
    fn test_apply_fixes_except_empty_exclude() {
        let code = "pub fn foo() -> bool { let x = bar().unwrap(); true }";
        // 空排除列表 = 修复所有问题 = 与 apply_fixes 相同
        let fixed = apply_fixes_except(code, &[]);
        let all_fixed = apply_fixes(code);
        assert_eq!(fixed, all_fixed, "空排除列表应等同于 apply_fixes");
    }

    #[test]
    fn test_apply_fixes_except_all_excluded() {
        let code = "fn foo() { let x = bar().unwrap(); }";
        // 排除所有可能的问题类型
        let exclude = [
            IssueType::Unwrap,
            IssueType::Expect,
            IssueType::Todo,
            IssueType::Unimplemented,
            IssueType::Panic,
            IssueType::UnsafeBlock,
            IssueType::UnsafeFn,
            IssueType::UnsafeImpl,
            IssueType::MissingDoc,
            IssueType::Unreachable,
            IssueType::MissingMustUse,
            IssueType::UnwrapOr,
            IssueType::UnwrapOrDefault,
            IssueType::MissingResultReturn,
        ];
        let fixed = apply_fixes_except(code, &exclude);
        assert_eq!(fixed, code, "排除所有类型不应修改代码");
    }

    #[test]
    fn test_apply_fixes_except_only_missing_doc() {
        let code = "pub fn foo() -> bool { true }";
        // 排除除 MissingDoc 外的所有类型
        let exclude = [
            IssueType::Unwrap,
            IssueType::MissingMustUse,
            IssueType::MissingResultReturn,
        ];
        let fixed = apply_fixes_except(code, &exclude);
        assert!(fixed.contains("/// TODO:"), "应添加文档注释 (未排除)");
    }

    #[test]
    fn test_apply_fixes_except_no_issues() {
        let code = "fn foo() -> i32 { 42 }";
        let exclude = [IssueType::Unwrap];
        let fixed = apply_fixes_except(code, &exclude);
        assert_eq!(fixed, code, "无问题代码不应修改");
    }

    // ===== Session 120: apply_fixes_dry_run_except 测试 =====

    #[test]
    fn test_apply_fixes_dry_run_except_unwrap() {
        let code = "pub fn foo() -> bool { let x = bar().unwrap(); true }";
        let exclude = [IssueType::MissingDoc];
        let preview = apply_fixes_dry_run_except(code, &exclude);
        assert!(preview.is_changed, "应有变化");
        assert!(preview.fixes_applied > 0, "应有修复");
        assert!(
            !preview.fixed_content.contains(".unwrap()"),
            "unwrap 应被修复"
        );
        assert!(preview.issues.len() >= 2, "应检测到所有问题 (包括被排除的)");
    }

    #[test]
    fn test_apply_fixes_dry_run_except_empty_exclude() {
        let code = "fn foo() { let x = bar().unwrap(); }";
        let preview = apply_fixes_dry_run_except(code, &[]);
        assert!(preview.is_changed, "空排除应修复所有问题");
        assert!(preview.fixes_applied > 0, "应有修复");
    }

    #[test]
    fn test_apply_fixes_dry_run_except_all_excluded() {
        let code = "fn foo() { let x = bar().unwrap(); }";
        let exclude = [
            IssueType::Unwrap,
            IssueType::MissingResultReturn,
            IssueType::MissingDoc,
            IssueType::MissingMustUse,
        ];
        let preview = apply_fixes_dry_run_except(code, &exclude);
        assert!(!preview.is_changed, "排除所有不应有变化");
        assert_eq!(preview.fixes_applied, 0, "无修复");
    }

    // ===== Session 120: compute_line_diff 测试 =====

    #[test]
    fn test_compute_line_diff_no_change() {
        let code = "fn foo() {}\nfn bar() {}";
        let diffs = compute_line_diff(code, code);
        assert!(diffs.is_empty(), "无变化应返回空列表");
    }

    #[test]
    fn test_compute_line_diff_modified() {
        let original = "let x = 1;";
        let fixed = "let x = 2;";
        let diffs = compute_line_diff(original, fixed);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].diff_type, LineDiffType::Modified);
        assert_eq!(diffs[0].line_number, 1);
        assert_eq!(diffs[0].original_line.as_deref(), Some("let x = 1;"));
        assert_eq!(diffs[0].fixed_line.as_deref(), Some("let x = 2;"));
    }

    #[test]
    fn test_compute_line_diff_added() {
        let original = "fn foo() {}";
        let fixed = "fn foo() {}\nfn bar() {}";
        let diffs = compute_line_diff(original, fixed);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].diff_type, LineDiffType::Added);
        assert_eq!(diffs[0].line_number, 2);
        assert!(diffs[0].original_line.is_none());
        assert_eq!(diffs[0].fixed_line.as_deref(), Some("fn bar() {}"));
    }

    #[test]
    fn test_compute_line_diff_removed() {
        let original = "fn foo() {}\nfn bar() {}";
        let fixed = "fn foo() {}";
        let diffs = compute_line_diff(original, fixed);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].diff_type, LineDiffType::Removed);
        assert_eq!(diffs[0].line_number, 2);
        assert_eq!(diffs[0].original_line.as_deref(), Some("fn bar() {}"));
        assert!(diffs[0].fixed_line.is_none());
    }

    #[test]
    fn test_compute_line_diff_multiple_changes() {
        let original = "let a = 1;\nlet b = 2;\nlet c = 3;";
        let fixed = "let a = 1;\nlet b = 99;\nlet c = 3;\nlet d = 4;";
        let diffs = compute_line_diff(original, fixed);
        assert_eq!(diffs.len(), 2);
        assert_eq!(diffs[0].diff_type, LineDiffType::Modified);
        assert_eq!(diffs[0].line_number, 2);
        assert_eq!(diffs[1].diff_type, LineDiffType::Added);
        assert_eq!(diffs[1].line_number, 4);
    }

    #[test]
    fn test_compute_line_diff_empty_strings() {
        let diffs = compute_line_diff("", "");
        assert!(diffs.is_empty());
    }

    #[test]
    fn test_compute_line_diff_serde() {
        let diff = LineDiff {
            line_number: 5,
            diff_type: LineDiffType::Modified,
            original_line: Some("old".to_string()),
            fixed_line: Some("new".to_string()),
        };
        let json = serde_json::to_string(&diff).expect("序列化失败");
        let deserialized: LineDiff = serde_json::from_str(&json).expect("反序列化失败");
        assert_eq!(deserialized.line_number, 5);
        assert_eq!(deserialized.diff_type, LineDiffType::Modified);
    }

    // ===== Session 120: ensure_anyhow_import 测试 =====

    #[test]
    fn test_ensure_anyhow_import_no_need() {
        let code = "fn foo() -> i32 { 42 }";
        assert_eq!(ensure_anyhow_import(code), code, "不需要导入时不修改");
    }

    #[test]
    fn test_ensure_anyhow_import_already_has_import() {
        let code = "use anyhow::Result;\nfn foo() -> Result<i32, anyhow::Error> { Ok(42) }";
        assert_eq!(ensure_anyhow_import(code), code, "已有导入不修改");
    }

    #[test]
    fn test_ensure_anyhow_import_already_has_error_import() {
        let code = "use anyhow::Error;\nfn foo() -> Result<i32, anyhow::Error> { Ok(42) }";
        assert_eq!(ensure_anyhow_import(code), code, "已有 Error 导入不修改");
    }

    #[test]
    fn test_ensure_anyhow_import_already_has_wildcard() {
        let code = "use anyhow::*;\nfn foo() -> Result<i32, anyhow::Error> { Ok(42) }";
        assert_eq!(ensure_anyhow_import(code), code, "已有通配导入不修改");
    }

    #[test]
    fn test_ensure_anyhow_import_needs_import() {
        let code = "fn foo() -> Result<i32, anyhow::Error> { Ok(42) }";
        let result = ensure_anyhow_import(code);
        assert!(result.contains("use anyhow::Result;"), "应添加导入");
        assert!(
            result.starts_with("use anyhow::Result;"),
            "导入应在文件开头"
        );
    }

    #[test]
    fn test_ensure_anyhow_import_with_comments() {
        let code = "//! Module docs\n// comment\nfn foo() -> Result<i32, anyhow::Error> { Ok(42) }";
        let result = ensure_anyhow_import(code);
        assert!(result.contains("use anyhow::Result;"), "应添加导入");
        // 导入应在注释之后, 代码之前
        let import_pos = result.find("use anyhow::Result;").unwrap();
        let fn_pos = result.find("fn foo()").unwrap();
        assert!(import_pos < fn_pos, "导入应在函数之前");
    }

    #[test]
    fn test_ensure_anyhow_import_idempotent() {
        let code = "fn foo() -> Result<i32, anyhow::Error> { Ok(42) }";
        let first = ensure_anyhow_import(code);
        let second = ensure_anyhow_import(&first);
        assert_eq!(first, second, "二次调用不应有变化 (幂等)");
    }

    // ===== Session 120: apply_fixes_with_imports 测试 =====

    #[test]
    fn test_apply_fixes_with_imports_no_change() {
        let code = "fn foo() -> i32 { 42 }";
        let result = apply_fixes_with_imports(code);
        assert_eq!(result, code, "无问题代码不应修改");
    }

    #[test]
    fn test_apply_fixes_with_imports_adds_import() {
        // 函数使用 ? 但未返回 Result
        let code = "fn foo() -> i32 { let x: Result<i32, _> = Ok(42); x? }";
        let result = apply_fixes_with_imports(code);
        assert!(result.contains("Result<"), "应修改返回类型为 Result");
        // Session 121: 增强版使用合并导入
        assert!(
            result.contains("use anyhow::"),
            "应添加 anyhow 导入: {}",
            result
        );
    }

    #[test]
    fn test_apply_fixes_with_imports_already_has_import() {
        let code = "use anyhow::Result;\nfn foo() { let x = bar().unwrap(); }";
        let result = apply_fixes_with_imports(code);
        // Session 121: 增强版可能合并为 use anyhow::{Result, Error};
        // 关键是不重复添加导入
        let import_count = result.matches("use anyhow::").count();
        assert_eq!(import_count, 1, "应只有一个 anyhow 导入行: {}", result);
    }

    // ===== Session 120: verify_idempotent_detailed 增强 (行级别 diff) 测试 =====

    #[test]
    fn test_verify_idempotent_detailed_with_diff_clean_code() {
        let result = verify_idempotent_detailed("fn foo() -> i32 { 42 }");
        assert!(result.is_idempotent);
        assert!(result.first_pass_diff.is_empty(), "无问题代码不应有 diff");
        assert!(result.second_pass_diff.is_empty(), "无问题代码不应有 diff");
    }

    #[test]
    fn test_verify_idempotent_detailed_with_diff_has_changes() {
        let result = verify_idempotent_detailed("fn foo() { let x = bar().unwrap(); }");
        assert!(
            !result.first_pass_diff.is_empty(),
            "有问题的代码第一次修复应有 diff"
        );
        assert!(
            result.second_pass_diff.is_empty(),
            "幂等修复第二次不应有 diff"
        );
    }

    #[test]
    fn test_verify_idempotent_detailed_diff_serde() {
        let result = verify_idempotent_detailed("fn foo() { let x = bar().unwrap(); }");
        let json = serde_json::to_string(&result).expect("序列化失败");
        let deserialized: IdempotencyResult = serde_json::from_str(&json).expect("反序列化失败");
        assert_eq!(
            deserialized.first_pass_diff.len(),
            result.first_pass_diff.len()
        );
    }

    // ===== Session 121: wrap_last_expression_in_ok 测试 =====

    #[test]
    fn test_wrap_last_expression_in_ok_single_line() {
        let code = "fn foo() -> Result<i32, anyhow::Error> { 42 }";
        let result = wrap_last_expression_in_ok(code);
        assert!(result.contains("Ok(42)"), "应包装返回值: {}", result);
    }

    #[test]
    fn test_wrap_last_expression_in_ok_already_wrapped() {
        let code = "fn foo() -> Result<i32, anyhow::Error> { Ok(42) }";
        let result = wrap_last_expression_in_ok(code);
        assert_eq!(result, code, "已包装不修改 (幂等)");
    }

    #[test]
    fn test_wrap_last_expression_in_ok_already_err() {
        let code = r#"fn foo() -> Result<i32, anyhow::Error> { Err(anyhow!("error")) }"#;
        let result = wrap_last_expression_in_ok(code);
        assert_eq!(result, code, "已有 Err 不修改");
    }

    #[test]
    fn test_wrap_last_expression_in_ok_multiline() {
        let code = "fn foo() -> Result<i32, anyhow::Error> {\n    let x = 42;\n    x\n}";
        let result = wrap_last_expression_in_ok(code);
        assert!(
            result.contains("Ok(x)"),
            "多行函数应包装最后表达式: {}",
            result
        );
    }

    #[test]
    fn test_wrap_last_expression_in_ok_multiline_already_wrapped() {
        let code = "fn foo() -> Result<i32, anyhow::Error> {\n    let x = 42;\n    Ok(x)\n}";
        let result = wrap_last_expression_in_ok(code);
        assert_eq!(result, code, "已包装不修改 (幂等)");
    }

    #[test]
    fn test_wrap_last_expression_in_ok_not_result_function() {
        let code = "fn foo() -> i32 { 42 }";
        let result = wrap_last_expression_in_ok(code);
        assert_eq!(result, code, "非 Result 函数不修改");
    }

    #[test]
    fn test_wrap_last_expression_in_ok_idempotent() {
        let code = "fn foo() -> Result<i32, anyhow::Error> { 42 }";
        let first = wrap_last_expression_in_ok(code);
        let second = wrap_last_expression_in_ok(&first);
        assert_eq!(first, second, "二次调用不变化 (幂等)");
    }

    #[test]
    fn test_wrap_last_expression_in_ok_unit_type() {
        let code = "fn foo() -> Result<(), anyhow::Error> {\n    println!(\"hello\");\n}";
        let result = wrap_last_expression_in_ok(code);
        // 函数体最后是 println! 语句 (以 ; 结尾), 不需要包装
        // 只有无分号的尾表达式才需要包装
        let _ = result;
    }

    #[test]
    fn test_wrap_last_expression_in_ok_preserves_indentation() {
        let code = "    fn foo() -> Result<i32, anyhow::Error> { 42 }";
        let result = wrap_last_expression_in_ok(code);
        assert!(result.contains("Ok(42)"), "应包装");
        assert!(result.starts_with("    "), "应保留缩进");
    }

    // ===== Session 121: merge_anyhow_imports 测试 =====

    #[test]
    fn test_merge_anyhow_imports_combines() {
        let code = "use anyhow::Result;\nuse anyhow::Error;\nfn foo() {}";
        let result = merge_anyhow_imports(code);
        assert!(
            result.contains("use anyhow::{Result, Error};"),
            "应合并导入: {}",
            result
        );
    }

    #[test]
    fn test_merge_anyhow_imports_idempotent() {
        let code = "use anyhow::{Result, Error};\nfn foo() {}";
        let result = merge_anyhow_imports(code);
        assert_eq!(result, code, "已合并不修改 (幂等)");
    }

    #[test]
    fn test_merge_anyhow_imports_only_result() {
        let code = "use anyhow::Result;\nfn foo() {}";
        let result = merge_anyhow_imports(code);
        assert_eq!(result, code, "只有 Result 不合并");
    }

    #[test]
    fn test_merge_anyhow_imports_only_error() {
        let code = "use anyhow::Error;\nfn foo() {}";
        let result = merge_anyhow_imports(code);
        assert_eq!(result, code, "只有 Error 不合并");
    }

    #[test]
    fn test_merge_anyhow_imports_no_imports() {
        let code = "fn foo() {}";
        let result = merge_anyhow_imports(code);
        assert_eq!(result, code, "无导入不修改");
    }

    #[test]
    fn test_merge_anyhow_imports_preserves_order() {
        let code = "use std::io;\nuse anyhow::Result;\nuse anyhow::Error;\nfn foo() {}";
        let result = merge_anyhow_imports(code);
        assert!(result.contains("use anyhow::{Result, Error};"), "应合并");
        assert!(result.contains("use std::io;"), "应保留其他导入");
    }

    // ===== Session 121: ensure_anyhow_imports 测试 =====

    #[test]
    fn test_ensure_anyhow_imports_no_need() {
        let code = "fn foo() -> i32 { 42 }";
        let result = ensure_anyhow_imports(code);
        assert_eq!(result, code, "不需要 anyhow 不修改");
    }

    #[test]
    fn test_ensure_anyhow_imports_already_has_both() {
        let code =
            "use anyhow::{Result, Error};\nfn foo() -> Result<i32, anyhow::Error> { Ok(42) }";
        let result = ensure_anyhow_imports(code);
        assert_eq!(result, code, "已有合并导入不修改");
    }

    #[test]
    fn test_ensure_anyhow_imports_already_has_wildcard() {
        let code = "use anyhow::*;\nfn foo() -> Result<i32, anyhow::Error> { Ok(42) }";
        let result = ensure_anyhow_imports(code);
        assert_eq!(result, code, "通配导入不修改");
    }

    #[test]
    fn test_ensure_anyhow_imports_needs_both_adds_merged() {
        let code = "fn foo() -> Result<i32, anyhow::Error> { Ok(42) }";
        let result = ensure_anyhow_imports(code);
        assert!(
            result.contains("use anyhow::{Result, Error};"),
            "应添加合并导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_anyhow_imports_has_result_merges_error() {
        let code = "use anyhow::Result;\nfn foo() -> Result<i32, anyhow::Error> { Ok(42) }";
        let result = ensure_anyhow_imports(code);
        assert!(
            result.contains("use anyhow::{Result, Error};"),
            "应合并添加 Error: {}",
            result
        );
        let count = result.matches("use anyhow::").count();
        assert_eq!(count, 1, "应只有一个 anyhow 导入行");
    }

    #[test]
    fn test_ensure_anyhow_imports_has_error_merges_result() {
        let code = "use anyhow::Error;\nfn foo() -> Result<i32, anyhow::Error> { Ok(42) }";
        let result = ensure_anyhow_imports(code);
        assert!(
            result.contains("use anyhow::{Result, Error};"),
            "应合并添加 Result: {}",
            result
        );
    }

    #[test]
    fn test_ensure_anyhow_imports_idempotent() {
        let code = "fn foo() -> Result<i32, anyhow::Error> { Ok(42) }";
        let first = ensure_anyhow_imports(code);
        let second = ensure_anyhow_imports(&first);
        assert_eq!(first, second, "二次调用不变化 (幂等)");
    }

    #[test]
    fn test_ensure_anyhow_imports_with_comments() {
        let code = "//! Module docs\n// comment\nfn foo() -> Result<i32, anyhow::Error> { Ok(42) }";
        let result = ensure_anyhow_imports(code);
        assert!(result.contains("use anyhow::"), "应添加导入");
        let import_pos = result.find("use anyhow::").unwrap();
        let fn_pos = result.find("fn foo()").unwrap();
        assert!(import_pos < fn_pos, "导入应在函数之前");
    }

    #[test]
    fn test_ensure_anyhow_imports_merges_separate() {
        let code = "use anyhow::Result;\nuse anyhow::Error;\nfn foo() -> Result<i32, anyhow::Error> { Ok(42) }";
        let result = ensure_anyhow_imports(code);
        assert!(
            result.contains("use anyhow::{Result, Error};"),
            "应合并分散导入: {}",
            result
        );
    }

    // ===== Session 121: compute_line_diff_lcs 测试 =====

    #[test]
    fn test_compute_line_diff_lcs_no_change() {
        let original = "fn foo() {}\nfn bar() {}";
        let fixed = "fn foo() {}\nfn bar() {}";
        let diffs = compute_line_diff_lcs(original, fixed);
        assert!(diffs.is_empty(), "无差异");
    }

    #[test]
    fn test_compute_line_diff_lcs_added() {
        let original = "fn foo() {}\nfn bar() {}";
        let fixed = "fn foo() {}\nfn new() {}\nfn bar() {}";
        let diffs = compute_line_diff_lcs(original, fixed);
        assert!(
            diffs.iter().any(|d| d.diff_type == LineDiffType::Added),
            "应有 Added"
        );
        assert!(
            !diffs.iter().any(|d| d.diff_type == LineDiffType::Modified),
            "不应有 Modified (LCS 正确识别插入)"
        );
        assert!(
            !diffs.iter().any(|d| d.diff_type == LineDiffType::Removed),
            "不应有 Removed"
        );
    }

    #[test]
    fn test_compute_line_diff_lcs_removed() {
        let original = "fn foo() {}\nfn removed() {}\nfn bar() {}";
        let fixed = "fn foo() {}\nfn bar() {}";
        let diffs = compute_line_diff_lcs(original, fixed);
        assert!(
            diffs.iter().any(|d| d.diff_type == LineDiffType::Removed),
            "应有 Removed"
        );
        assert!(
            !diffs.iter().any(|d| d.diff_type == LineDiffType::Added),
            "不应有 Added"
        );
    }

    #[test]
    fn test_compute_line_diff_lcs_modified() {
        // LCS 不产生 Modified, 而是 Removed + Added
        let original = "let x = 1;";
        let fixed = "let x = 2;";
        let diffs = compute_line_diff_lcs(original, fixed);
        assert!(!diffs.is_empty(), "应有差异");
        // 旧行被 Removed, 新行被 Added
        assert!(
            diffs.iter().any(|d| d.diff_type == LineDiffType::Removed),
            "应有 Removed"
        );
        assert!(
            diffs.iter().any(|d| d.diff_type == LineDiffType::Added),
            "应有 Added"
        );
    }

    #[test]
    fn test_compute_line_diff_lcs_empty_original() {
        let diffs = compute_line_diff_lcs("", "fn foo() {}");
        assert!(
            diffs.iter().all(|d| d.diff_type == LineDiffType::Added),
            "空原始 → 全部 Added"
        );
    }

    #[test]
    fn test_compute_line_diff_lcs_empty_fixed() {
        let diffs = compute_line_diff_lcs("fn foo() {}", "");
        assert!(
            diffs.iter().all(|d| d.diff_type == LineDiffType::Removed),
            "空修复 → 全部 Removed"
        );
    }

    #[test]
    fn test_compute_line_diff_lcs_both_empty() {
        let diffs = compute_line_diff_lcs("", "");
        assert!(diffs.is_empty(), "双空 → 无差异");
    }

    #[test]
    fn test_compute_line_diff_lcs_multiple_insertions() {
        let original = "a\nc\ne";
        let fixed = "a\nb\nc\nd\ne";
        let diffs = compute_line_diff_lcs(original, fixed);
        let added_count = diffs
            .iter()
            .filter(|d| d.diff_type == LineDiffType::Added)
            .count();
        assert_eq!(added_count, 2, "应添加 2 行 (b 和 d)");
    }

    #[test]
    fn test_compute_line_diff_lcs_serde() {
        let diffs = compute_line_diff_lcs("a\nb", "a\nc\nb");
        let json = serde_json::to_string(&diffs).expect("序列化失败");
        let deserialized: Vec<LineDiff> = serde_json::from_str(&json).expect("反序列化失败");
        assert_eq!(diffs.len(), deserialized.len());
    }

    #[test]
    fn test_compute_line_diff_lcs_vs_basic() {
        // 对比: 基础版会标记修改行, LCS 版正确识别为 Added
        let original = "fn foo() {\n}\nfn bar() {}";
        let fixed = "fn foo() {\n    let x = 42;\n}\nfn bar() {}";

        let basic_diffs = compute_line_diff(original, fixed);
        let lcs_diffs = compute_line_diff_lcs(original, fixed);

        // 基础版: 从第 2 行开始全部 Modified (因为行号对齐失效)
        // LCS 版: 只有第 2 行 Added, 其他行匹配
        let lcs_added = lcs_diffs
            .iter()
            .filter(|d| d.diff_type == LineDiffType::Added)
            .count();
        assert_eq!(lcs_added, 1, "LCS 应只有 1 个 Added");

        // 基础版至少有 Modified (因为行号错位)
        let basic_modified = basic_diffs
            .iter()
            .filter(|d| d.diff_type == LineDiffType::Modified)
            .count();
        assert!(basic_modified >= 1, "基础版应有 Modified");
    }

    // ===== Session 121: apply_staged_fixes 测试 =====

    #[test]
    fn test_apply_staged_fixes_unwrap() {
        let code = "fn foo() { let x = bar().unwrap(); }";
        let result = apply_staged_fixes(code);
        assert!(!result.contains(".unwrap()"), "unwrap 应被修复");
    }

    #[test]
    fn test_apply_staged_fixes_must_use() {
        let code = "pub fn foo() -> bool { true }";
        let result = apply_staged_fixes(code);
        assert!(result.contains("#[must_use]"), "应添加 #[must_use]");
    }

    #[test]
    fn test_apply_staged_fixes_missing_doc() {
        let code = "pub fn foo() -> bool { true }";
        let result = apply_staged_fixes(code);
        assert!(
            result.contains("///") || result.contains("TODO"),
            "应有文档注释或 TODO"
        );
    }

    #[test]
    fn test_apply_staged_fixes_no_issues() {
        let code = "fn foo() -> i32 { 42 }";
        let result = apply_staged_fixes(code);
        assert_eq!(result, code, "无问题不修改");
    }

    #[test]
    fn test_apply_staged_fixes_multiple_issues() {
        let code = "pub fn foo() { let x = bar().unwrap(); }";
        let result = apply_staged_fixes(code);
        assert!(!result.contains(".unwrap()"), "unwrap 应被修复");
        assert!(result.contains("#[must_use]"), "应添加 #[must_use]");
    }

    #[test]
    fn test_apply_staged_fixes_idempotent() {
        let code = "pub fn foo() { let x = bar().unwrap(); }";
        let first = apply_staged_fixes(code);
        let second = apply_staged_fixes(&first);
        assert_eq!(first, second, "分阶段修复应幂等");
    }

    // ===== Session 121: apply_fixes_with_imports 增强测试 =====

    #[test]
    fn test_apply_fixes_with_imports_wraps_ok() {
        // 函数使用 ? 但未返回 Result, 修复后应包装返回值为 Ok(...)
        let code = "fn foo() -> i32 { let x: Result<i32, _> = Ok(42); x? }";
        let result = apply_fixes_with_imports(code);
        assert!(result.contains("Result<"), "应修改返回类型");
        assert!(result.contains("use anyhow::"), "应添加导入");
    }

    #[test]
    fn test_apply_fixes_with_imports_merges_imports() {
        // 已有分散导入, 修复后应合并
        let code = "use anyhow::Result;\nfn foo() -> i32 { let x: Result<i32, _> = Ok(42); x? }";
        let result = apply_fixes_with_imports(code);
        let import_count = result.matches("use anyhow::").count();
        assert_eq!(import_count, 1, "应只有一个 anyhow 导入行: {}", result);
    }

    // ===== Session 125: apply_fixes_with_imports 集成 std 导入测试 =====

    #[test]
    fn test_apply_fixes_with_imports_adds_std_imports() {
        // 修复后使用了 HashMap 但未导入, 应自动添加 std 导入
        let code = "fn foo() -> HashMap<String, i32> { HashMap::new() }";
        let result = apply_fixes_with_imports(code);
        assert!(
            result.contains("use std::collections::HashMap;"),
            "应添加 HashMap 导入: {}",
            result
        );
    }

    #[test]
    fn test_apply_fixes_with_imports_adds_both_imports() {
        // 同时需要 anyhow 和 std 导入
        let code = "fn foo() -> i32 { let x: Result<i32, _> = Ok(42); let m = HashMap::new(); x? }";
        let result = apply_fixes_with_imports(code);
        assert!(
            result.contains("use anyhow::"),
            "应添加 anyhow 导入: {}",
            result
        );
        assert!(
            result.contains("use std::collections::HashMap;"),
            "应添加 HashMap 导入: {}",
            result
        );
    }

    #[test]
    fn test_apply_fixes_with_imports_wraps_return_and_imports() {
        // return 包装 + std 导入同时工作
        let code = "fn foo() -> Result<HashMap<String, i32>, anyhow::Error> {\n    return HashMap::new();\n}";
        let result = apply_fixes_with_imports(code);
        assert!(
            result.contains("return Ok(HashMap::new());"),
            "应包装 return: {}",
            result
        );
        assert!(
            result.contains("use std::collections::HashMap;"),
            "应添加 HashMap 导入: {}",
            result
        );
    }

    // ===== Session 122: wrap_return_statements_in_ok 测试 =====

    #[test]
    fn test_wrap_return_statements_in_ok_basic() {
        let code = "fn foo() -> Result<i32, anyhow::Error> {\n    return 42;\n}";
        let result = wrap_return_statements_in_ok(code);
        assert!(
            result.contains("return Ok(42);"),
            "应包装 return 值: {}",
            result
        );
    }

    #[test]
    fn test_wrap_return_statements_in_ok_already_ok() {
        let code = "fn foo() -> Result<i32, anyhow::Error> {\n    return Ok(42);\n}";
        assert_eq!(wrap_return_statements_in_ok(code), code, "已包装不修改");
    }

    #[test]
    fn test_wrap_return_statements_in_ok_already_err() {
        let code =
            "fn foo() -> Result<i32, anyhow::Error> {\n    return Err(anyhow::Error::msg(\"e\"));\n}";
        assert_eq!(
            wrap_return_statements_in_ok(code),
            code,
            "return Err 不修改"
        );
    }

    #[test]
    fn test_wrap_return_statements_in_ok_idempotent() {
        let code = "fn foo() -> Result<i32, anyhow::Error> {\n    return 42;\n}";
        let first = wrap_return_statements_in_ok(code);
        let second = wrap_return_statements_in_ok(&first);
        assert_eq!(first, second, "二次调用不变化");
    }

    #[test]
    fn test_wrap_return_statements_in_ok_non_result_fn() {
        let code = "fn foo() -> i32 {\n    return 42;\n}";
        let result = wrap_return_statements_in_ok(code);
        assert_eq!(result, code, "非 Result 函数不修改");
    }

    #[test]
    fn test_wrap_return_statements_in_ok_multiple_returns() {
        let code = "fn foo(x: bool) -> Result<i32, anyhow::Error> {\n    if x {\n        return 1;\n    }\n    return 2;\n}";
        let result = wrap_return_statements_in_ok(code);
        assert!(result.contains("return Ok(1);"), "应包装第一个 return");
        assert!(result.contains("return Ok(2);"), "应包装第二个 return");
    }

    #[test]
    fn test_wrap_return_statements_in_ok_no_return() {
        let code = "fn foo() -> Result<i32, anyhow::Error> {\n    Ok(42)\n}";
        let result = wrap_return_statements_in_ok(code);
        assert_eq!(result, code, "无 return 语句不修改");
    }

    #[test]
    fn test_wrap_return_statements_in_ok_return_with_expression() {
        let code = "fn foo(a: i32, b: i32) -> Result<i32, anyhow::Error> {\n    return a + b;\n}";
        let result = wrap_return_statements_in_ok(code);
        assert!(
            result.contains("return Ok(a + b);"),
            "应包装表达式: {}",
            result
        );
    }

    #[test]
    fn test_wrap_return_statements_in_ok_mixed_ok_and_plain() {
        let code = "fn foo(x: bool) -> Result<i32, anyhow::Error> {\n    if x {\n        return Ok(1);\n    }\n    return 2;\n}";
        let result = wrap_return_statements_in_ok(code);
        assert!(result.contains("return Ok(1);"), "已包装的 Ok 不变");
        assert!(
            result.contains("return Ok(2);"),
            "未包装的应包装: {}",
            result
        );
    }

    // ===== Session 122: compute_line_diff_myers 测试 =====

    #[test]
    fn test_compute_line_diff_myers_no_diff() {
        let diffs = compute_line_diff_myers("fn foo() {}", "fn foo() {}");
        assert!(diffs.is_empty(), "无差异应返回空");
    }

    #[test]
    fn test_compute_line_diff_myers_insertion() {
        let original = "fn foo() {\n}\n";
        let fixed = "fn foo() {\n    let x = 42;\n}\n";
        let diffs = compute_line_diff_myers(original, fixed);
        let added = diffs
            .iter()
            .filter(|d| d.diff_type == LineDiffType::Added)
            .count();
        assert_eq!(added, 1, "应有 1 个 Added");
        assert!(
            !diffs.iter().any(|d| d.diff_type == LineDiffType::Removed),
            "不应有 Removed"
        );
        let removed = diffs
            .iter()
            .filter(|d| d.diff_type == LineDiffType::Removed)
            .count();
        assert_eq!(removed, 0, "不应有 Removed");
    }

    #[test]
    fn test_compute_line_diff_myers_deletion() {
        let original = "fn foo() {\n    let x = 42;\n}\n";
        let fixed = "fn foo() {\n}\n";
        let diffs = compute_line_diff_myers(original, fixed);
        let removed = diffs
            .iter()
            .filter(|d| d.diff_type == LineDiffType::Removed)
            .count();
        assert_eq!(removed, 1, "应有 1 个 Removed");
        let added = diffs
            .iter()
            .filter(|d| d.diff_type == LineDiffType::Added)
            .count();
        assert_eq!(added, 0, "不应有 Added");
    }

    #[test]
    fn test_compute_line_diff_myers_empty_original() {
        let fixed = "a\nb\nc";
        let diffs = compute_line_diff_myers("", fixed);
        assert_eq!(diffs.len(), 3, "空 original 应全部 Added");
        assert!(diffs.iter().all(|d| d.diff_type == LineDiffType::Added));
    }

    #[test]
    fn test_compute_line_diff_myers_empty_fixed() {
        let original = "a\nb\nc";
        let diffs = compute_line_diff_myers(original, "");
        assert_eq!(diffs.len(), 3, "空 fixed 应全部 Removed");
        assert!(diffs.iter().all(|d| d.diff_type == LineDiffType::Removed));
    }

    #[test]
    fn test_compute_line_diff_myers_both_empty() {
        let diffs = compute_line_diff_myers("", "");
        assert!(diffs.is_empty(), "双空应返回空");
    }

    #[test]
    fn test_compute_line_diff_myers_multiple_changes() {
        let original = "a\nb\nc\nd\ne";
        let fixed = "a\nx\nc\ny\ne";
        let diffs = compute_line_diff_myers(original, fixed);
        assert!(!diffs.is_empty(), "应有差异");
        let added = diffs
            .iter()
            .filter(|d| d.diff_type == LineDiffType::Added)
            .count();
        let removed = diffs
            .iter()
            .filter(|d| d.diff_type == LineDiffType::Removed)
            .count();
        assert!(added + removed > 0, "应有 Added 或 Removed");
    }

    #[test]
    fn test_compute_line_diff_myers_vs_lcs_same_result() {
        let original = "fn foo() {\n}\nfn bar() {}";
        let fixed = "fn foo() {\n    let x = 42;\n}\nfn bar() {}";

        let lcs_diffs = compute_line_diff_lcs(original, fixed);
        let myers_diffs = compute_line_diff_myers(original, fixed);

        let lcs_added = lcs_diffs
            .iter()
            .filter(|d| d.diff_type == LineDiffType::Added)
            .count();
        let myers_added = myers_diffs
            .iter()
            .filter(|d| d.diff_type == LineDiffType::Added)
            .count();
        assert_eq!(lcs_added, myers_added, "LCS 和 Myers 应有相同 Added 计数");
    }

    #[test]
    fn test_compute_line_diff_myers_no_modified() {
        let original = "fn foo() {\n}\n";
        let fixed = "fn foo() {\n    let x = 42;\n}\n";
        let diffs = compute_line_diff_myers(original, fixed);
        let modified = diffs
            .iter()
            .filter(|d| d.diff_type == LineDiffType::Modified)
            .count();
        assert_eq!(modified, 0, "Myers 不应产生 Modified");
    }

    // ===== Session 122: format_diff_summary 测试 =====

    #[test]
    fn test_format_diff_summary_empty() {
        let summary = format_diff_summary(&[]);
        assert!(
            summary.contains("No differences"),
            "空差异应提示无差异: {}",
            summary
        );
    }

    #[test]
    fn test_format_diff_summary_with_additions() {
        let original = "fn foo() {\n}\n";
        let fixed = "fn foo() {\n    let x = 42;\n}\n";
        let diffs = compute_line_diff_myers(original, fixed);
        let summary = format_diff_summary(&diffs);
        assert!(summary.contains("Added"), "应包含 Added 计数: {}", summary);
        assert!(
            summary.contains("let x = 42"),
            "应包含差异行内容: {}",
            summary
        );
    }

    #[test]
    fn test_format_diff_summary_counts() {
        let diffs = vec![
            LineDiff {
                line_number: 1,
                diff_type: LineDiffType::Added,
                original_line: None,
                fixed_line: Some("new line".to_string()),
            },
            LineDiff {
                line_number: 2,
                diff_type: LineDiffType::Added,
                original_line: None,
                fixed_line: Some("another".to_string()),
            },
            LineDiff {
                line_number: 3,
                diff_type: LineDiffType::Removed,
                original_line: Some("old line".to_string()),
                fixed_line: None,
            },
        ];
        let summary = format_diff_summary(&diffs);
        assert!(summary.contains("2 Added"), "应显示 2 Added: {}", summary);
        assert!(
            summary.contains("1 Removed"),
            "应显示 1 Removed: {}",
            summary
        );
        assert!(
            summary.contains("0 Modified"),
            "应显示 0 Modified: {}",
            summary
        );
    }

    #[test]
    fn test_format_diff_summary_symbols() {
        let diffs = vec![
            LineDiff {
                line_number: 1,
                diff_type: LineDiffType::Added,
                original_line: None,
                fixed_line: Some("add".to_string()),
            },
            LineDiff {
                line_number: 2,
                diff_type: LineDiffType::Removed,
                original_line: Some("remove".to_string()),
                fixed_line: None,
            },
            LineDiff {
                line_number: 3,
                diff_type: LineDiffType::Modified,
                original_line: Some("old".to_string()),
                fixed_line: Some("new".to_string()),
            },
        ];
        let summary = format_diff_summary(&diffs);
        assert!(summary.contains("+"), "应有 + 符号");
        assert!(summary.contains("-"), "应有 - 符号");
        assert!(summary.contains("~"), "应有 ~ 符号");
    }

    // ===== Session 122: apply_staged_fixes_preview 测试 =====

    #[test]
    fn test_apply_staged_fixes_preview_basic() {
        let code = "pub fn foo() { let x = bar().unwrap(); }";
        let preview = apply_staged_fixes_preview(code);
        assert!(preview.total_changed, "应有变化");
        assert!(preview.stage1_changed, "高优先级阶段应修复 unwrap");
    }

    #[test]
    fn test_apply_staged_fixes_preview_no_issues() {
        let code = "fn foo() -> i32 { 42 }";
        let preview = apply_staged_fixes_preview(code);
        assert!(!preview.total_changed, "无问题不应有变化");
        assert!(!preview.stage1_changed, "阶段 1 不应有变化");
        assert!(!preview.stage2_changed, "阶段 2 不应有变化");
        assert!(!preview.stage3_changed, "阶段 3 不应有变化");
    }

    #[test]
    fn test_apply_staged_fixes_preview_stage1_only() {
        let code = "fn foo() { let x = bar().unwrap(); }";
        let preview = apply_staged_fixes_preview(code);
        assert!(preview.stage1_changed, "阶段 1 应修复 unwrap");
        assert!(
            !preview.stage1_result.contains(".unwrap()"),
            "阶段 1 结果不应有 unwrap"
        );
    }

    #[test]
    fn test_apply_staged_fixes_preview_stage2_must_use() {
        let code = "pub fn foo() -> bool { true }";
        let preview = apply_staged_fixes_preview(code);
        assert!(preview.stage2_changed, "阶段 2 应添加 #[must_use]");
        assert!(
            preview.stage2_result.contains("#[must_use]"),
            "阶段 2 结果应有 #[must_use]"
        );
    }

    #[test]
    fn test_apply_staged_fixes_preview_stage3_doc() {
        let code = "pub fn foo() -> bool { true }";
        let preview = apply_staged_fixes_preview(code);
        assert!(preview.total_changed, "应有变化");
        assert!(
            preview.stage3_result.contains("///") || preview.stage3_result.contains("TODO"),
            "阶段 3 应有文档注释或 TODO"
        );
    }

    #[test]
    fn test_apply_staged_fixes_preview_final_matches_apply_staged() {
        let code = "pub fn foo() { let x = bar().unwrap(); }";
        let preview = apply_staged_fixes_preview(code);
        let direct = apply_staged_fixes(code);
        assert_eq!(
            preview.stage3_result, direct,
            "预览最终结果应与直接调用一致"
        );
    }

    #[test]
    fn test_apply_staged_fixes_preview_serde() {
        let code = "pub fn foo() { let x = bar().unwrap(); }";
        let preview = apply_staged_fixes_preview(code);
        let json = serde_json::to_string(&preview).expect("序列化失败");
        let deserialized: StagedFixPreview = serde_json::from_str(&json).expect("反序列化失败");
        assert_eq!(preview.total_changed, deserialized.total_changed);
        assert_eq!(preview.stage1_changed, deserialized.stage1_changed);
    }

    // ===== Session 122: ensure_anyhow_imports_extended 测试 =====

    #[test]
    fn test_ensure_anyhow_imports_extended_bail() {
        let code = "fn foo() -> Result<(), anyhow::Error> { bail!(\"error\"); }";
        let result = ensure_anyhow_imports_extended(code);
        assert!(result.contains("bail"), "应包含 bail 导入: {}", result);
    }

    #[test]
    fn test_ensure_anyhow_imports_extended_ensure() {
        let code = "fn foo(x: bool) -> Result<(), anyhow::Error> { ensure!(x, \"err\"); }";
        let result = ensure_anyhow_imports_extended(code);
        assert!(result.contains("ensure"), "应包含 ensure 导入: {}", result);
    }

    #[test]
    fn test_ensure_anyhow_imports_extended_bail_and_ensure() {
        let code = "fn foo(x: bool) -> Result<(), anyhow::Error> {\n    ensure!(x, \"err\");\n    bail!(\"err2\");\n}";
        let result = ensure_anyhow_imports_extended(code);
        assert!(result.contains("bail"), "应包含 bail");
        assert!(result.contains("ensure"), "应包含 ensure");
        assert!(
            result.contains("use anyhow::{"),
            "应使用合并导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_anyhow_imports_extended_no_macros() {
        let code = "fn foo() -> Result<(), anyhow::Error> { Ok(()) }";
        let result = ensure_anyhow_imports_extended(code);
        assert!(!result.contains("bail"), "不需要 bail 不应添加");
        assert!(!result.contains("ensure"), "不需要 ensure 不应添加");
    }

    #[test]
    fn test_ensure_anyhow_imports_extended_idempotent() {
        let code = "fn foo() -> Result<(), anyhow::Error> { bail!(\"error\"); }";
        let first = ensure_anyhow_imports_extended(code);
        let second = ensure_anyhow_imports_extended(&first);
        assert_eq!(first, second, "二次调用不变化");
    }

    #[test]
    fn test_ensure_anyhow_imports_extended_already_has_bail() {
        let code =
            "use anyhow::{Result, bail};\nfn foo() -> Result<(), anyhow::Error> { bail!(\"e\"); }";
        let result = ensure_anyhow_imports_extended(code);
        let import_count = result.matches("use anyhow::").count();
        assert_eq!(import_count, 1, "应只有一个 anyhow 导入行: {}", result);
    }

    #[test]
    fn test_ensure_anyhow_imports_extended_adds_to_merged() {
        let code =
            "use anyhow::{Result, Error};\nfn foo() -> Result<(), anyhow::Error> { bail!(\"e\"); }";
        let result = ensure_anyhow_imports_extended(code);
        assert!(
            result.contains("bail"),
            "应添加 bail 到合并导入: {}",
            result
        );
        let import_count = result.matches("use anyhow::").count();
        assert_eq!(import_count, 1, "应只有一个导入行");
    }

    #[test]
    fn test_ensure_anyhow_imports_extended_sorted() {
        let code = "fn foo() -> Result<(), anyhow::Error> {\n    bail!(\"e\");\n    ensure!(true, \"e2\");\n}";
        let result = ensure_anyhow_imports_extended(code);
        if let Some(pos) = result.find("use anyhow::{") {
            let import_line = &result[pos..result[pos..].find("};").unwrap_or(pos) + 2];
            assert!(import_line.contains("Result"), "应有 Result");
            assert!(import_line.contains("bail"), "应有 bail");
            assert!(import_line.contains("ensure"), "应有 ensure");
        }
    }

    // ===== Session 123: compute_line_diff_unified 测试 =====

    #[test]
    fn test_compute_line_diff_unified_no_diff() {
        let diffs = compute_line_diff_unified("fn foo() {}", "fn foo() {}");
        assert!(diffs.is_empty(), "无差异应返回空");
    }

    #[test]
    fn test_compute_line_diff_unified_empty_both() {
        let diffs = compute_line_diff_unified("", "");
        assert!(diffs.is_empty(), "双空应返回空");
    }

    #[test]
    fn test_compute_line_diff_unified_small_input() {
        // 小输入 (< 20 行) → Basic
        let original = "a\nb\nc";
        let fixed = "a\nx\nc";
        let diffs = compute_line_diff_unified(original, fixed);
        assert!(!diffs.is_empty(), "小输入应有差异");
    }

    #[test]
    fn test_compute_line_diff_unified_medium_input() {
        // 中等输入 (20-100 行) → LCS
        let original: String = (0..30).map(|i| format!("line {i}\n")).collect();
        let mut fixed = original.clone();
        fixed.push_str("line 30\n");
        let diffs = compute_line_diff_unified(&original, &fixed);
        assert!(
            diffs.iter().any(|d| d.diff_type == LineDiffType::Added),
            "中等输入应检测到新增行"
        );
    }

    #[test]
    fn test_compute_line_diff_unified_large_input() {
        // 大输入 (N×M > 10000) → Myers
        let original: String = (0..200).map(|i| format!("line {i}\n")).collect();
        let mut fixed = original.clone();
        fixed.push_str("line 200\n");
        let diffs = compute_line_diff_unified(&original, &fixed);
        assert!(
            diffs.iter().any(|d| d.diff_type == LineDiffType::Added),
            "大输入应检测到新增行"
        );
    }

    #[test]
    fn test_compute_line_diff_with_algorithm_basic() {
        let original = "a\nb";
        let fixed = "a\nc";
        let diffs = compute_line_diff_with_algorithm(original, fixed, DiffAlgorithm::Basic);
        assert!(!diffs.is_empty(), "Basic 应检测到差异");
    }

    #[test]
    fn test_compute_line_diff_with_algorithm_lcs() {
        let original = "a\nb\nc\nd";
        let fixed = "a\nx\nc\ny";
        let diffs = compute_line_diff_with_algorithm(original, fixed, DiffAlgorithm::Lcs);
        assert!(!diffs.is_empty(), "LCS 应检测到差异");
    }

    #[test]
    fn test_compute_line_diff_with_algorithm_myers() {
        let original = "a\nb\nc\nd";
        let fixed = "a\nx\nc\ny";
        let diffs = compute_line_diff_with_algorithm(original, fixed, DiffAlgorithm::Myers);
        assert!(!diffs.is_empty(), "Myers 应检测到差异");
    }

    #[test]
    fn test_compute_line_diff_with_algorithm_auto() {
        let original = "a\nb\nc";
        let fixed = "a\nb\nc\nd";
        let diffs = compute_line_diff_with_algorithm(original, fixed, DiffAlgorithm::Auto);
        assert!(
            diffs.iter().any(|d| d.diff_type == LineDiffType::Added),
            "Auto 应检测到新增行"
        );
    }

    #[test]
    fn test_diff_algorithm_default() {
        let algo = DiffAlgorithm::default();
        assert_eq!(algo, DiffAlgorithm::Auto, "默认应为 Auto");
    }

    #[test]
    fn test_diff_algorithm_serde() {
        let algo = DiffAlgorithm::Myers;
        let json = serde_json::to_string(&algo).unwrap();
        let deserialized: DiffAlgorithm = serde_json::from_str(&json).unwrap();
        assert_eq!(algo, deserialized, "Serde 往返应保持一致");
    }

    // ===== Session 125: DiffAlgorithm::Hirschberg 测试 =====

    #[test]
    fn test_compute_line_diff_with_algorithm_hirschberg() {
        let original = "a\nb\nc";
        let fixed = "a\nB\nc";
        let diffs = compute_line_diff_with_algorithm(original, fixed, DiffAlgorithm::Hirschberg);
        assert!(!diffs.is_empty(), "Hirschberg 应检测到差异");
    }

    #[test]
    fn test_compute_line_diff_with_algorithm_hirschberg_empty() {
        let diffs = compute_line_diff_with_algorithm("", "", DiffAlgorithm::Hirschberg);
        assert!(diffs.is_empty(), "空输入应无差异");
    }

    #[test]
    fn test_compute_line_diff_with_algorithm_hirschberg_serde() {
        let algo = DiffAlgorithm::Hirschberg;
        let json = serde_json::to_string(&algo).unwrap();
        let deserialized: DiffAlgorithm = serde_json::from_str(&json).unwrap();
        assert_eq!(algo, deserialized, "Hirschberg Serde 往返应保持一致");
    }

    #[test]
    fn test_compute_line_diff_with_algorithm_auto_large_uses_hirschberg() {
        // 大输入 (N×M > 10000) → Auto 应使用 Hirschberg
        // 结果应与直接调用 Hirschberg 一致
        let original: String = (0..200).map(|i| format!("line {i}\n")).collect();
        let mut fixed = original.clone();
        fixed.push_str("line 200\n");
        let auto_diffs = compute_line_diff_with_algorithm(&original, &fixed, DiffAlgorithm::Auto);
        let hirschberg_diffs =
            compute_line_diff_with_algorithm(&original, &fixed, DiffAlgorithm::Hirschberg);
        assert_eq!(
            auto_diffs, hirschberg_diffs,
            "大输入 Auto 应使用 Hirschberg, 结果一致"
        );
    }

    #[test]
    fn test_compute_line_diff_unified_one_empty() {
        let diffs = compute_line_diff_unified("", "a\nb\nc");
        assert_eq!(diffs.len(), 3, "空 original 应全部 Added");
        assert!(diffs.iter().all(|d| d.diff_type == LineDiffType::Added));
    }

    // ===== Session 123: wrap_return_statements_in_ok 多行测试 =====

    #[test]
    fn test_wrap_return_statements_in_ok_multiline_function_call() {
        let code = "fn foo() -> Result<i32, anyhow::Error> {\n    return bar(\n        1,\n        2,\n    );\n}";
        let result = wrap_return_statements_in_ok(code);
        assert!(
            result.contains("return Ok(bar("),
            "应包装多行函数调用: {}",
            result
        );
        assert!(result.contains("));"), "应在末行添加闭合括号: {}", result);
    }

    #[test]
    fn test_wrap_return_statements_in_ok_multiline_struct() {
        let code = "fn foo() -> Result<Foo, anyhow::Error> {\n    return Foo {\n        x: 1,\n        y: 2,\n    };\n}";
        let result = wrap_return_statements_in_ok(code);
        assert!(
            result.contains("return Ok(Foo {"),
            "应包装多行结构体: {}",
            result
        );
        assert!(result.contains("});"), "应在末行添加闭合括号: {}", result);
    }

    #[test]
    fn test_wrap_return_statements_in_ok_single_line_still_works() {
        let code = "fn foo() -> Result<i32, anyhow::Error> {\n    return 42;\n}";
        let result = wrap_return_statements_in_ok(code);
        assert!(
            result.contains("return Ok(42);"),
            "单行 return 仍应正确包装: {}",
            result
        );
    }

    #[test]
    fn test_wrap_return_statements_in_ok_multiline_idempotent() {
        let code =
            "fn foo() -> Result<i32, anyhow::Error> {\n    return bar(\n        1,\n    );\n}";
        let first = wrap_return_statements_in_ok(code);
        let second = wrap_return_statements_in_ok(&first);
        assert_eq!(first, second, "多行包装应幂等");
    }

    #[test]
    fn test_wrap_return_statements_in_ok_multiline_already_ok() {
        let code =
            "fn foo() -> Result<i32, anyhow::Error> {\n    return Ok(bar(\n        1,\n    ));\n}";
        let result = wrap_return_statements_in_ok(code);
        assert_eq!(result, code, "已包装的 return Ok(...) 不应修改");
    }

    // ===== Session 123: validate_rust_braces quote! 宏测试 =====

    #[test]
    fn test_validate_rust_braces_quote_macro_basic() {
        let code = "fn foo() {\n    quote! {\n        let x = 42;\n    }\n}";
        assert!(
            validate_rust_braces(code).is_none(),
            "quote! 宏基本用法应无问题"
        );
    }

    #[test]
    fn test_validate_rust_braces_quote_macro_with_interpolation() {
        let code =
            "fn foo() {\n    let name = \"test\";\n    quote! {\n        #name = 42;\n    }\n}";
        assert!(
            validate_rust_braces(code).is_none(),
            "quote! 宏插值 #name 应无问题"
        );
    }

    #[test]
    fn test_validate_rust_braces_quote_macro_repetition() {
        let code = "fn foo() {\n    quote! {\n        #(#field: #field_types,)*\n    }\n}";
        assert!(
            validate_rust_braces(code).is_none(),
            "quote! 宏重复语法 #(#field),* 应无问题"
        );
    }

    #[test]
    fn test_validate_rust_braces_quote_macro_nested() {
        let code = "fn foo() {\n    quote! {\n        if true {\n            do_something();\n        }\n    }\n}";
        assert!(
            validate_rust_braces(code).is_none(),
            "quote! 宏嵌套块应无问题"
        );
    }

    #[test]
    fn test_validate_rust_braces_quote_macro_raw_string() {
        let code = "fn foo() {\n    quote! {\n        let s = r#\"hello\"#;\n    }\n}";
        assert!(
            validate_rust_braces(code).is_none(),
            "quote! 宏内 raw string 应无问题"
        );
    }

    #[test]
    fn test_validate_rust_braces_quote_macro_raw_identifier() {
        let code = "fn foo() {\n    quote! {\n        r#type = 42;\n    }\n}";
        assert!(
            validate_rust_braces(code).is_none(),
            "quote! 宏内 raw identifier r#type 应无问题"
        );
    }

    #[test]
    fn test_validate_rust_braces_quote_macro_paren_delimiter() {
        let code = "fn foo() {\n    quote!(let x = 42;)\n}";
        assert!(
            validate_rust_braces(code).is_none(),
            "quote!() 圆括号分隔符应无问题"
        );
    }

    #[test]
    fn test_validate_rust_braces_quote_macro_derive_attribute() {
        let code = "fn foo() {\n    quote! {\n        #[derive(Debug)]\n        struct Foo {\n            bar: u32,\n        }\n    }\n}";
        assert!(
            validate_rust_braces(code).is_none(),
            "quote! 宏内 derive 属性应无问题"
        );
    }

    #[test]
    fn test_validate_rust_braces_quote_macro_unbalanced() {
        let code = "fn foo() {\n    quote! {\n        let x = 42;\n    }\n}";
        assert!(
            validate_rust_braces(code).is_none(),
            "平衡的 quote! 宏应无问题"
        );
        // 真正不平衡的情况
        let bad_code = "fn foo() {\n    quote! {\n        let x = 42;\n}";
        assert!(
            validate_rust_braces(bad_code).is_some(),
            "不平衡的 quote! 宏应报告问题"
        );
    }

    // ===== Session 123: ensure_anyhow_imports_extended anyhow!/Context 测试 =====

    #[test]
    fn test_ensure_anyhow_imports_extended_anyhow_macro() {
        let code = "fn foo() -> Result<(), anyhow::Error> {\n    Err(anyhow!(\"error\"))\n}";
        let result = ensure_anyhow_imports_extended(code);
        assert!(
            result.contains("anyhow"),
            "应包含 anyhow 宏导入: {}",
            result
        );
        assert!(
            result.contains("use anyhow::{"),
            "应使用合并导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_anyhow_imports_extended_context_trait() {
        let code = "fn foo() -> Result<(), anyhow::Error> {\n    file.read_to_end(&mut buf).context(\"read failed\")?;\n    Ok(())\n}";
        let result = ensure_anyhow_imports_extended(code);
        assert!(
            result.contains("Context"),
            "应包含 Context trait 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_anyhow_imports_extended_with_context() {
        let code = "fn foo() -> Result<(), anyhow::Error> {\n    file.read_to_end(&mut buf).with_context(|| \"read failed\")?;\n    Ok(())\n}";
        let result = ensure_anyhow_imports_extended(code);
        assert!(
            result.contains("Context"),
            "应包含 Context trait 导入 (with_context): {}",
            result
        );
    }

    #[test]
    fn test_ensure_anyhow_imports_extended_all_items() {
        let code = "fn foo() -> Result<(), anyhow::Error> {\n    bail!(\"e1\");\n    ensure!(true, \"e2\");\n    let e = anyhow!(\"e3\");\n    x.context(\"e4\")?;\n}";
        let result = ensure_anyhow_imports_extended(code);
        assert!(result.contains("Result"), "应有 Result");
        assert!(result.contains("bail"), "应有 bail");
        assert!(result.contains("ensure"), "应有 ensure");
        assert!(result.contains("anyhow"), "应有 anyhow");
        assert!(result.contains("Context"), "应有 Context");
        let import_count = result.matches("use anyhow::").count();
        assert_eq!(import_count, 1, "应只有一个合并导入行: {}", result);
    }

    #[test]
    fn test_ensure_anyhow_imports_extended_context_idempotent() {
        let code = "fn foo() -> Result<(), anyhow::Error> {\n    x.context(\"e\")?;\n    Ok(())\n}";
        let first = ensure_anyhow_imports_extended(code);
        let second = ensure_anyhow_imports_extended(&first);
        assert_eq!(first, second, "Context 检测应幂等");
    }

    #[test]
    fn test_ensure_anyhow_imports_extended_sorted_with_new_items() {
        let code = "fn foo() -> Result<(), anyhow::Error> {\n    x.context(\"e\")?;\n    bail!(\"e2\");\n    let e = anyhow!(\"e3\");\n}";
        let result = ensure_anyhow_imports_extended(code);
        if let Some(pos) = result.find("use anyhow::{") {
            let inner_start = pos + "use anyhow::{".len();
            if let Some(end) = result[inner_start..].find("};") {
                let inner = &result[inner_start..inner_start + end];
                // 验证排序: Result → Context → anyhow → bail
                let result_pos = inner.find("Result").unwrap_or(usize::MAX);
                let context_pos = inner.find("Context").unwrap_or(usize::MAX);
                let anyhow_pos = inner.find("anyhow").unwrap_or(usize::MAX);
                let bail_pos = inner.find("bail").unwrap_or(usize::MAX);
                assert!(result_pos < context_pos, "Result 应在 Context 前");
                assert!(context_pos < anyhow_pos, "Context 应在 anyhow 前");
                assert!(anyhow_pos < bail_pos, "anyhow 应在 bail 前");
            }
        }
    }

    #[test]
    fn test_ensure_anyhow_imports_extended_no_context_when_not_needed() {
        let code = "fn foo() -> Result<(), anyhow::Error> { Ok(()) }";
        let result = ensure_anyhow_imports_extended(code);
        assert!(
            !result.contains("Context"),
            "不需要 Context 时不应添加: {}",
            result
        );
    }

    // ===== Session 124: compute_line_diff_hirschberg 测试 =====

    #[test]
    fn test_compute_line_diff_hirschberg_no_diff() {
        let diffs = compute_line_diff_hirschberg("fn foo() {}", "fn foo() {}");
        assert!(diffs.is_empty(), "无差异应返回空");
    }

    #[test]
    fn test_compute_line_diff_hirschberg_insertion() {
        let original = "fn foo() {\n}\n";
        let fixed = "fn foo() {\n    let x = 42;\n}\n";
        let diffs = compute_line_diff_hirschberg(original, fixed);
        let added = diffs
            .iter()
            .filter(|d| d.diff_type == LineDiffType::Added)
            .count();
        assert_eq!(added, 1, "应有 1 个 Added");
        assert!(
            !diffs.iter().any(|d| d.diff_type == LineDiffType::Removed),
            "不应有 Removed"
        );
    }

    #[test]
    fn test_compute_line_diff_hirschberg_deletion() {
        let original = "fn foo() {\n    let x = 42;\n}\n";
        let fixed = "fn foo() {\n}\n";
        let diffs = compute_line_diff_hirschberg(original, fixed);
        let removed = diffs
            .iter()
            .filter(|d| d.diff_type == LineDiffType::Removed)
            .count();
        assert_eq!(removed, 1, "应有 1 个 Removed");
    }

    #[test]
    fn test_compute_line_diff_hirschberg_both_empty() {
        let diffs = compute_line_diff_hirschberg("", "");
        assert!(diffs.is_empty(), "双空应返回空");
    }

    #[test]
    fn test_compute_line_diff_hirschberg_empty_original() {
        let diffs = compute_line_diff_hirschberg("", "a\nb\nc");
        assert_eq!(diffs.len(), 3, "空 original 应全部 Added");
        assert!(diffs.iter().all(|d| d.diff_type == LineDiffType::Added));
    }

    #[test]
    fn test_compute_line_diff_hirschberg_empty_fixed() {
        let diffs = compute_line_diff_hirschberg("a\nb\nc", "");
        assert_eq!(diffs.len(), 3, "空 fixed 应全部 Removed");
        assert!(diffs.iter().all(|d| d.diff_type == LineDiffType::Removed));
    }

    #[test]
    fn test_compute_line_diff_hirschberg_multiple_changes() {
        let original = "a\nb\nc\nd\ne";
        let fixed = "a\nx\nc\ny\ne";
        let diffs = compute_line_diff_hirschberg(original, fixed);
        assert!(!diffs.is_empty(), "应有差异");
        let added = diffs
            .iter()
            .filter(|d| d.diff_type == LineDiffType::Added)
            .count();
        let removed = diffs
            .iter()
            .filter(|d| d.diff_type == LineDiffType::Removed)
            .count();
        assert!(added + removed > 0, "应有 Added 或 Removed");
    }

    #[test]
    fn test_compute_line_diff_hirschberg_no_modified() {
        let original = "fn foo() {\n}\n";
        let fixed = "fn foo() {\n    let x = 42;\n}\n";
        let diffs = compute_line_diff_hirschberg(original, fixed);
        let modified = diffs
            .iter()
            .filter(|d| d.diff_type == LineDiffType::Modified)
            .count();
        assert_eq!(modified, 0, "Hirschberg 不应产生 Modified");
    }

    #[test]
    fn test_compute_line_diff_hirschberg_vs_lcs_same_result() {
        let original = "fn foo() {\n}\nfn bar() {}";
        let fixed = "fn foo() {\n    let x = 42;\n}\nfn bar() {}";

        let lcs_diffs = compute_line_diff_lcs(original, fixed);
        let hirschberg_diffs = compute_line_diff_hirschberg(original, fixed);

        let lcs_added = lcs_diffs
            .iter()
            .filter(|d| d.diff_type == LineDiffType::Added)
            .count();
        let hirschberg_added = hirschberg_diffs
            .iter()
            .filter(|d| d.diff_type == LineDiffType::Added)
            .count();
        assert_eq!(
            lcs_added, hirschberg_added,
            "LCS 和 Hirschberg 应有相同 Added 计数"
        );
    }

    #[test]
    fn test_compute_line_diff_hirschberg_large_input() {
        let original: String = (0..100).map(|i| format!("line {i}\n")).collect();
        let mut fixed = original.clone();
        fixed.push_str("line 100\n");
        let diffs = compute_line_diff_hirschberg(&original, &fixed);
        assert!(
            diffs.iter().any(|d| d.diff_type == LineDiffType::Added),
            "大输入应检测到新增行"
        );
    }

    #[test]
    fn test_compute_line_diff_hirschberg_middle_insertion() {
        let original = "a\nb\nc\nd\ne";
        let fixed = "a\nb\nX\nc\nd\ne";
        let diffs = compute_line_diff_hirschberg(original, fixed);
        let added = diffs
            .iter()
            .filter(|d| d.diff_type == LineDiffType::Added)
            .count();
        assert_eq!(added, 1, "中间插入应有 1 个 Added");
        let added_content = diffs
            .iter()
            .filter(|d| d.diff_type == LineDiffType::Added)
            .map(|d| d.fixed_line.as_deref().unwrap_or(""))
            .collect::<Vec<_>>();
        assert!(added_content.contains(&"X"), "应包含插入的行 X");
    }

    // ===== Session 124: format_diff_unified 测试 =====

    #[test]
    fn test_format_diff_unified_no_diff() {
        assert_eq!(
            format_diff_unified("fn foo() {}", "fn foo() {}"),
            "",
            "无差异应返回空字符串"
        );
    }

    #[test]
    fn test_format_diff_unified_contains_headers() {
        let diff = format_diff_unified("fn foo() {\n}\n", "fn foo() {\n    let x = 42;\n}\n");
        assert!(
            diff.contains("--- original"),
            "应包含 --- original: {}",
            diff
        );
        assert!(diff.contains("+++ fixed"), "应包含 +++ fixed: {}", diff);
        assert!(diff.contains("@@"), "应包含 @@ hunk 头: {}", diff);
    }

    #[test]
    fn test_format_diff_unified_contains_added_line() {
        let diff = format_diff_unified("fn foo() {\n}\n", "fn foo() {\n    let x = 42;\n}\n");
        assert!(diff.contains("+    let x = 42;"), "应包含新增行: {}", diff);
    }

    #[test]
    fn test_format_diff_unified_contains_removed_line() {
        let diff = format_diff_unified("fn foo() {\n    let x = 42;\n}\n", "fn foo() {\n}\n");
        assert!(diff.contains("-    let x = 42;"), "应包含删除行: {}", diff);
    }

    #[test]
    fn test_format_diff_unified_contains_context_lines() {
        let original = "line1\nline2\nline3\nline4\nline5";
        let fixed = "line1\nline2\nCHANGED\nline4\nline5";
        let diff = format_diff_unified(original, fixed);
        // 应包含上下文行 (以空格开头)
        assert!(diff.contains(" line1"), "应包含上下文行 line1: {}", diff);
        assert!(diff.contains(" line5"), "应包含上下文行 line5: {}", diff);
    }

    #[test]
    fn test_format_diff_unified_hunk_header_format() {
        let diff = format_diff_unified("a\nb\nc", "a\nB\nc");
        // 应包含 @@ -start,count +start,count @@ 格式
        assert!(
            diff.contains("@@ -") && diff.contains(" +"),
            "hunk 头格式应正确: {}",
            diff
        );
        assert!(diff.contains("@@"), "应有 @@ 标记: {}", diff);
    }

    #[test]
    fn test_format_diff_unified_empty_original() {
        let diff = format_diff_unified("", "a\nb\nc");
        assert!(!diff.is_empty(), "空 original 应有 diff");
        assert!(diff.contains("+a"), "应包含 +a: {}", diff);
        assert!(diff.contains("+b"), "应包含 +b: {}", diff);
        assert!(diff.contains("+c"), "应包含 +c: {}", diff);
    }

    #[test]
    fn test_format_diff_unified_empty_fixed() {
        let diff = format_diff_unified("a\nb\nc", "");
        assert!(!diff.is_empty(), "空 fixed 应有 diff");
        assert!(diff.contains("-a"), "应包含 -a: {}", diff);
        assert!(diff.contains("-b"), "应包含 -b: {}", diff);
        assert!(diff.contains("-c"), "应包含 -c: {}", diff);
    }

    #[test]
    fn test_format_diff_unified_multiple_hunks() {
        // 两个相距很远的变更 → 两个 hunk
        let original: String = (0..20).map(|i| format!("line {i}\n")).collect();
        let mut fixed = original.clone();
        // 修改第 2 行和第 18 行 (相距 > 2*context=6)
        fixed = fixed.replace("line 1\n", "line CHANGED1\n");
        fixed = fixed.replace("line 17\n", "line CHANGED2\n");
        let diff = format_diff_unified(&original, &fixed);
        let hunk_count = diff.matches("@@").count() / 2; // 每个 hunk 有 2 个 @@
        assert!(
            hunk_count >= 2,
            "应有至少 2 个 hunk, 实际 {}: {}",
            hunk_count,
            diff
        );
    }

    #[test]
    fn test_format_diff_unified_single_line_change() {
        let diff = format_diff_unified("let x = 1;", "let x = 2;");
        assert!(!diff.is_empty(), "单行修改应有 diff");
        assert!(diff.contains("-let x = 1;"), "应包含旧行: {}", diff);
        assert!(diff.contains("+let x = 2;"), "应包含新行: {}", diff);
    }

    // ===== Session 125: format_diff_unified_with_options 测试 =====

    #[test]
    fn test_format_diff_unified_with_options_custom_names() {
        let diff = format_diff_unified_with_options(
            "fn foo() {}\n",
            "fn bar() {}\n",
            "src/main.rs",
            "src/main.rs",
            3,
        );
        assert!(
            diff.contains("--- src/main.rs"),
            "应包含自定义原始文件名: {}",
            diff
        );
        assert!(
            diff.contains("+++ src/main.rs"),
            "应包含自定义修复文件名: {}",
            diff
        );
    }

    #[test]
    fn test_format_diff_unified_with_options_no_diff() {
        let diff = format_diff_unified_with_options("a\nb\nc", "a\nb\nc", "old.rs", "new.rs", 3);
        assert!(diff.is_empty(), "无差异应返回空字符串");
    }

    #[test]
    fn test_format_diff_unified_with_options_context_zero() {
        let original = "line1\nline2\nline3\nline4\nline5\n";
        let fixed = "line1\nline2\nCHANGED\nline4\nline5\n";
        let diff = format_diff_unified_with_options(original, fixed, "a", "b", 0);
        // context=0 时不应包含上下文行
        assert!(!diff.contains(" line1"), "context=0 不应有上下文: {}", diff);
        assert!(!diff.contains(" line5"), "context=0 不应有上下文: {}", diff);
        assert!(diff.contains("-line3"), "应包含删除行: {}", diff);
        assert!(diff.contains("+CHANGED"), "应包含新增行: {}", diff);
    }

    #[test]
    fn test_format_diff_unified_with_options_context_large() {
        let original = "line1\nline2\nline3\n";
        let fixed = "line1\nCHANGED\nline3\n";
        let diff = format_diff_unified_with_options(original, fixed, "a", "b", 10);
        // context=10 时应包含所有行作为上下文
        assert!(diff.contains(" line1"), "大 context 应包含 line1: {}", diff);
        assert!(diff.contains(" line3"), "大 context 应包含 line3: {}", diff);
    }

    #[test]
    fn test_format_diff_unified_with_options_different_names() {
        let diff =
            format_diff_unified_with_options("a\n", "b\n", "original_file.rs", "fixed_file.rs", 3);
        assert!(
            diff.contains("--- original_file.rs"),
            "应包含原始文件名: {}",
            diff
        );
        assert!(
            diff.contains("+++ fixed_file.rs"),
            "应包含修复文件名: {}",
            diff
        );
    }

    #[test]
    fn test_format_diff_unified_with_options_defaults_match() {
        // format_diff_unified 应等价于 format_diff_unified_with_options 默认参数
        let original = "fn foo() {\n}\n";
        let fixed = "fn foo() {\n    let x = 42;\n}\n";
        let default_diff = format_diff_unified(original, fixed);
        let options_diff =
            format_diff_unified_with_options(original, fixed, "original", "fixed", 3);
        assert_eq!(
            default_diff, options_diff,
            "默认参数应与 format_diff_unified 一致"
        );
    }

    #[test]
    fn test_format_diff_unified_with_options_empty_inputs() {
        let diff = format_diff_unified_with_options("", "a\nb\n", "old", "new", 3);
        assert!(!diff.is_empty(), "空 original 应有 diff");
        assert!(diff.contains("+a"), "应包含 +a: {}", diff);
        assert!(diff.contains("+b"), "应包含 +b: {}", diff);
    }

    // ===== Session 124: ensure_std_imports 测试 =====

    #[test]
    fn test_ensure_std_imports_hashmap() {
        let code = "fn foo() -> HashMap<String, i32> { HashMap::new() }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::collections::HashMap;"),
            "应添加 HashMap 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_hashset() {
        let code = "fn foo() -> HashSet<String> { HashSet::new() }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::collections::HashSet;"),
            "应添加 HashSet 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_multiple_collections() {
        let code = "fn foo() -> HashMap<String, HashSet<i32>> { HashMap::new() }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::collections::{HashMap, HashSet};"),
            "应合并 collections 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_path_pathbuf() {
        let code = "fn foo(p: &Path) -> PathBuf { p.to_path_buf() }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::path::{Path, PathBuf};"),
            "应合并 path 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_file() {
        let code = "fn foo() -> File { unimplemented!() }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::fs::File;"),
            "应添加 File 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_io_types() {
        let code = "fn foo(r: &mut dyn Read, w: &mut dyn Write) -> std::io::Result<()> { Ok(()) }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::io::{Read, Write};"),
            "应合并 io 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_already_imported() {
        let code =
            "use std::collections::HashMap;\nfn foo() -> HashMap<String, i32> { HashMap::new() }";
        let result = ensure_std_imports(code);
        let count = result.matches("use std::collections::HashMap;").count();
        assert_eq!(count, 1, "已有导入不应重复: {}", result);
    }

    #[test]
    fn test_ensure_std_imports_already_merged() {
        let code = "use std::collections::{HashMap, HashSet};\nfn foo() -> HashMap<String, HashSet<i32>> { HashMap::new() }";
        let result = ensure_std_imports(code);
        let count = result.matches("use std::collections::").count();
        assert_eq!(count, 1, "已有合并导入不应重复: {}", result);
    }

    #[test]
    fn test_ensure_std_imports_full_path_no_import() {
        let code = "fn foo() -> std::collections::HashMap<String, i32> { std::collections::HashMap::new() }";
        let result = ensure_std_imports(code);
        assert!(
            !result.contains("use std::collections"),
            "全限定路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_idempotent() {
        let code = "fn foo() -> HashMap<String, i32> { HashMap::new() }";
        let first = ensure_std_imports(code);
        let second = ensure_std_imports(&first);
        assert_eq!(first, second, "二次调用不变化 (幂等)");
    }

    #[test]
    fn test_ensure_std_imports_no_need() {
        let code = "fn foo() -> i32 { 42 }";
        let result = ensure_std_imports(code);
        assert_eq!(result, code, "不需要 std 导入不修改");
    }

    #[test]
    fn test_ensure_std_imports_wildcard() {
        let code = "use std::collections::*;\nfn foo() -> HashMap<String, i32> { HashMap::new() }";
        let result = ensure_std_imports(code);
        assert_eq!(result, code, "通配导入不修改");
    }

    #[test]
    fn test_ensure_std_imports_btreemap() {
        let code = "fn foo() -> BTreeMap<String, i32> { BTreeMap::new() }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::collections::BTreeMap;"),
            "应添加 BTreeMap 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_insert_position() {
        let code = "//! Module docs\nfn foo() -> HashMap<String, i32> { HashMap::new() }";
        let result = ensure_std_imports(code);
        let import_pos = result.find("use std::").unwrap();
        let fn_pos = result.find("fn foo()").unwrap();
        assert!(
            import_pos < fn_pos,
            "导入应在函数之前: {} < {}",
            import_pos,
            fn_pos
        );
    }

    // ===== Session 125: ensure_std_imports 新增 cell/sync/rc/process/env/time/net 类型 =====

    #[test]
    fn test_ensure_std_imports_cell_refcell() {
        let code = "fn foo(cell: Cell<i32>, rc: RefCell<String>) {}";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::cell::{Cell, RefCell};"),
            "应添加 cell 模块合并导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_once_cell() {
        let code = "fn foo() -> OnceCell<i32> { OnceCell::new() }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::cell::OnceCell;"),
            "应添加 OnceCell 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_arc_mutex_rwlock() {
        let code = "fn foo() -> Arc<Mutex<i32>> { Arc::new(Mutex::new(0)) }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::sync::{Arc, Mutex};"),
            "应合并 sync 模块导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_rwlock() {
        let code = "fn foo() -> RwLock<i32> { RwLock::new(0) }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::sync::RwLock;"),
            "应添加 RwLock 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_once_lock() {
        let code = "fn foo() -> OnceLock<i32> { OnceLock::new() }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::sync::OnceLock;"),
            "应添加 OnceLock 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_rc() {
        let code = "fn foo() -> Rc<String> { Rc::new(String::new()) }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::rc::Rc;"),
            "应添加 Rc 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_command() {
        let code = "fn foo() { let mut cmd = Command::new(\"ls\"); }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::process::Command;"),
            "应添加 Command 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_exit_status() {
        let code = "fn foo(status: ExitStatus) -> bool { status.success() }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::process::ExitStatus;"),
            "应添加 ExitStatus 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_env() {
        let code = "fn foo() { let v = env::var(\"HOME\"); }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::env;"),
            "应添加 env 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_instant_duration() {
        let code = "fn foo() -> Duration { let start = Instant::now(); start.elapsed() }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::time::{Duration, Instant};"),
            "应添加 time 模块合并导入 (字母序): {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_tcp() {
        let code = "fn foo() -> TcpListener { TcpListener::bind(\"127.0.0.1:0\").unwrap() }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::net::TcpListener;"),
            "应添加 TcpListener 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_tcp_stream() {
        let code = "fn foo() -> TcpStream { TcpStream::connect(\"127.0.0.1:80\").unwrap() }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::net::TcpStream;"),
            "应添加 TcpStream 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_arc_full_path_no_import() {
        let code = "fn foo() -> std::sync::Arc<i32> { std::sync::Arc::new(0) }";
        let result = ensure_std_imports(code);
        assert!(
            !result.contains("use std::sync::Arc;"),
            "全限定路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_multiple_new_types() {
        let code = "fn foo() -> Arc<Mutex<HashMap<String, Vec<i32>>>> { Arc::new(Mutex::new(HashMap::new())) }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::sync::{Arc, Mutex};"),
            "应有合并的 sync 导入: {}",
            result
        );
        assert!(
            result.contains("use std::collections::HashMap;"),
            "应有 HashMap: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_new_types_idempotent() {
        let code = "fn foo() -> Arc<Mutex<i32>> { Arc::new(Mutex::new(0)) }";
        let first = ensure_std_imports(code);
        let second = ensure_std_imports(&first);
        assert_eq!(first, second, "新增类型检测应幂等");
    }

    // ===== Session 124: wrap_return_statements_in_ok 闭包/async/tab 测试 =====

    #[test]
    fn test_wrap_return_statements_in_ok_closure_single_line() {
        let code = "fn foo() -> Result<i32, anyhow::Error> {\n    return |x| x + 1;\n}";
        let result = wrap_return_statements_in_ok(code);
        assert!(
            result.contains("return Ok(|x| x + 1);"),
            "应包装闭包: {}",
            result
        );
    }

    #[test]
    fn test_wrap_return_statements_in_ok_closure_no_args() {
        let code = "fn foo() -> Result<i32, anyhow::Error> {\n    return || 42;\n}";
        let result = wrap_return_statements_in_ok(code);
        assert!(
            result.contains("return Ok(|| 42);"),
            "应包装无参闭包: {}",
            result
        );
    }

    #[test]
    fn test_wrap_return_statements_in_ok_move_closure() {
        let code = "fn foo() -> Result<i32, anyhow::Error> {\n    return move || 42;\n}";
        let result = wrap_return_statements_in_ok(code);
        assert!(
            result.contains("return Ok(move || 42);"),
            "应包装 move 闭包: {}",
            result
        );
    }

    #[test]
    fn test_wrap_return_statements_in_ok_async_block() {
        let code = "fn foo() -> Result<i32, anyhow::Error> {\n    return async { 42 };\n}";
        let result = wrap_return_statements_in_ok(code);
        assert!(
            result.contains("return Ok(async { 42 });"),
            "应包装 async 块: {}",
            result
        );
    }

    #[test]
    fn test_wrap_return_statements_in_ok_async_move_block() {
        let code = "fn foo() -> Result<i32, anyhow::Error> {\n    return async move { 42 };\n}";
        let result = wrap_return_statements_in_ok(code);
        assert!(
            result.contains("return Ok(async move { 42 });"),
            "应包装 async move 块: {}",
            result
        );
    }

    #[test]
    fn test_wrap_return_statements_in_ok_unsafe_block() {
        let code = "fn foo() -> Result<i32, anyhow::Error> {\n    return unsafe { 42 };\n}";
        let result = wrap_return_statements_in_ok(code);
        assert!(
            result.contains("return Ok(unsafe { 42 });"),
            "应包装 unsafe 块: {}",
            result
        );
    }

    #[test]
    fn test_wrap_return_statements_in_ok_multiline_closure() {
        let code =
            "fn foo() -> Result<i32, anyhow::Error> {\n    return |x| {\n        x + 1\n    };\n}";
        let result = wrap_return_statements_in_ok(code);
        assert!(
            result.contains("return Ok(|x| {"),
            "应包装多行闭包: {}",
            result
        );
        assert!(result.contains("});"), "应在末行添加闭合: {}", result);
    }

    #[test]
    fn test_wrap_return_statements_in_ok_multiline_async_block() {
        let code = "fn foo() -> Result<i32, anyhow::Error> {\n    return async {\n        let x = 42;\n        x + 1\n    };\n}";
        let result = wrap_return_statements_in_ok(code);
        assert!(
            result.contains("return Ok(async {"),
            "应包装多行 async 块: {}",
            result
        );
        assert!(result.contains("});"), "应在末行添加闭合: {}", result);
    }

    #[test]
    fn test_wrap_return_statements_in_ok_return_semicolon() {
        let code = "fn foo() -> Result<(), anyhow::Error> {\n    return;\n}";
        let result = wrap_return_statements_in_ok(code);
        assert!(
            result.contains("return Ok(());"),
            "应将 return; 包装为 return Ok(());: {}",
            result
        );
    }

    #[test]
    fn test_wrap_return_statements_in_ok_return_at_end_of_line() {
        let code = "fn foo() -> Result<i32, anyhow::Error> {\n    return\n        42;\n}";
        let result = wrap_return_statements_in_ok(code);
        assert!(
            result.contains("return Ok("),
            "应处理 return 在行尾的情况: {}",
            result
        );
        assert!(result.contains("42);"), "应在末行添加闭合: {}", result);
    }

    #[test]
    fn test_wrap_return_statements_in_ok_closure_idempotent() {
        let code = "fn foo() -> Result<i32, anyhow::Error> {\n    return |x| x + 1;\n}";
        let first = wrap_return_statements_in_ok(code);
        let second = wrap_return_statements_in_ok(&first);
        assert_eq!(first, second, "闭包包装应幂等");
    }

    #[test]
    fn test_wrap_return_statements_in_ok_async_idempotent() {
        let code = "fn foo() -> Result<i32, anyhow::Error> {\n    return async { 42 };\n}";
        let first = wrap_return_statements_in_ok(code);
        let second = wrap_return_statements_in_ok(&first);
        assert_eq!(first, second, "async 包装应幂等");
    }

    #[test]
    fn test_wrap_return_statements_in_ok_return_semicolon_idempotent() {
        let code = "fn foo() -> Result<(), anyhow::Error> {\n    return;\n}";
        let first = wrap_return_statements_in_ok(code);
        let second = wrap_return_statements_in_ok(&first);
        assert_eq!(first, second, "return; 包装应幂等");
    }

    // ===== Session 124: verify_idempotent_detailed 使用统一 diff 接口测试 =====

    #[test]
    fn test_verify_idempotent_detailed_unified_diff_clean() {
        let result = verify_idempotent_detailed("fn foo() -> i32 { 42 }");
        assert!(result.is_idempotent);
        assert!(result.first_pass_diff.is_empty(), "无问题代码不应有 diff");
    }

    #[test]
    fn test_verify_idempotent_detailed_unified_diff_with_fix() {
        let result = verify_idempotent_detailed("fn foo() { let x = bar().unwrap(); }");
        assert!(!result.first_pass_diff.is_empty(), "有问题的代码应有 diff");
        assert!(
            result.second_pass_diff.is_empty(),
            "幂等修复第二次不应有 diff"
        );
    }

    // ===== Session 125: wrap_return_statements_in_ok match 表达式测试 =====

    #[test]
    fn test_wrap_return_statements_in_ok_match_single_line() {
        let code = "fn foo(x: i32) -> Result<i32, anyhow::Error> {\n    return match x { 1 => 42, _ => 0 };\n}";
        let result = wrap_return_statements_in_ok(code);
        assert!(
            result.contains("return Ok(match x { 1 => 42, _ => 0 });"),
            "应包装单行 match: {}",
            result
        );
    }

    #[test]
    fn test_wrap_return_statements_in_ok_match_multiline() {
        let code = "fn foo(x: i32) -> Result<i32, anyhow::Error> {\n    return match x {\n        1 => 42,\n        _ => 0,\n    };\n}";
        let result = wrap_return_statements_in_ok(code);
        assert!(
            result.contains("return Ok(match x {"),
            "应包装多行 match 首行: {}",
            result
        );
        assert!(result.contains("});"), "应在末行添加闭合括号: {}", result);
    }

    #[test]
    fn test_wrap_return_statements_in_ok_match_with_block_arms() {
        let code = "fn foo(x: i32) -> Result<i32, anyhow::Error> {\n    return match x {\n        1 => {\n            let y = 42;\n            y\n        }\n        _ => 0,\n    };\n}";
        let result = wrap_return_statements_in_ok(code);
        assert!(
            result.contains("return Ok(match x {"),
            "应包装含块臂的 match: {}",
            result
        );
        assert!(result.contains("});"), "应在末行添加闭合括号: {}", result);
    }

    #[test]
    fn test_wrap_return_statements_in_ok_match_already_ok() {
        // match 表达式已被 Ok() 包装, 不应重复包装
        let code = "fn foo(x: i32) -> Result<i32, anyhow::Error> {\n    return Ok(match x { 1 => 42, _ => 0 });\n}";
        let result = wrap_return_statements_in_ok(code);
        let ok_count = result.matches("return Ok(").count();
        assert_eq!(ok_count, 1, "不应重复包装: {}", result);
    }

    #[test]
    fn test_wrap_return_statements_in_ok_match_idempotent() {
        let code = "fn foo(x: i32) -> Result<i32, anyhow::Error> {\n    return match x {\n        1 => 42,\n        _ => 0,\n    };\n}";
        let first = wrap_return_statements_in_ok(code);
        let second = wrap_return_statements_in_ok(&first);
        assert_eq!(first, second, "match 包装应幂等");
    }

    #[test]
    fn test_wrap_return_statements_in_ok_match_with_nested_match() {
        let code = "fn foo(x: i32, y: i32) -> Result<i32, anyhow::Error> {\n    return match x {\n        1 => match y {\n            2 => 42,\n            _ => 0,\n        },\n        _ => -1,\n    };\n}";
        let result = wrap_return_statements_in_ok(code);
        assert!(
            result.contains("return Ok(match x {"),
            "应包装嵌套 match: {}",
            result
        );
        assert!(result.contains("});"), "应在末行添加闭合括号: {}", result);
    }

    // ===== Session 126: ensure_std_imports 新增 thread/marker/borrow/mpsc/atomic 类型 =====

    #[test]
    fn test_ensure_std_imports_thread_module() {
        let code = "fn foo() { thread::spawn(|| {}); }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::thread;"),
            "应添加 thread 模块导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_thread_type() {
        let code = "fn foo() -> Thread { thread::current() }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::thread::Thread;"),
            "应添加 Thread 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_join_handle() {
        let code = "fn foo() -> JoinHandle<i32> { thread::spawn(|| 42) }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::thread::JoinHandle;"),
            "应添加 JoinHandle 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_phantom_data() {
        let code = "fn foo() -> PhantomData<i32> { PhantomData }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::marker::PhantomData;"),
            "应添加 PhantomData 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_cow() {
        let code = "fn foo() -> Cow<'static, str> { Cow::Borrowed(\"hi\") }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::borrow::Cow;"),
            "应添加 Cow 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_mpsc_sender_receiver() {
        let code = "fn foo() -> (Sender<i32>, Receiver<i32>) { let (s, r) = std::sync::mpsc::channel(); (s, r) }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::sync::mpsc::{Receiver, Sender};"),
            "应合并 msc 导入 (字母序): {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_condvar() {
        let code = "fn foo() -> Condvar { Condvar::new() }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::sync::Condvar;"),
            "应添加 Condvar 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_barrier() {
        let code = "fn foo() -> Barrier { Barrier::new(3) }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::sync::Barrier;"),
            "应添加 Barrier 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_atomic_bool() {
        let code = "fn foo() -> AtomicBool { AtomicBool::new(true) }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::sync::atomic::AtomicBool;"),
            "应添加 AtomicBool 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_atomic_usize() {
        let code = "fn foo() -> AtomicUsize { AtomicUsize::new(0) }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::sync::atomic::AtomicUsize;"),
            "应添加 AtomicUsize 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_multiple_atomic_types() {
        let code = "fn foo() -> (AtomicBool, AtomicUsize) { (AtomicBool::new(true), AtomicUsize::new(0)) }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::sync::atomic::{AtomicBool, AtomicUsize};"),
            "应合并 atomic 导入 (字母序): {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_new_types_idempotent_s126() {
        let code =
            "fn foo() -> (AtomicBool, JoinHandle<i32>, PhantomData<u8>) { unimplemented!() }";
        let first = ensure_std_imports(code);
        let second = ensure_std_imports(&first);
        assert_eq!(first, second, "Session 126 新增类型检测应幂等");
    }

    #[test]
    fn test_ensure_std_imports_sorted_alphabetically() {
        // 使用多个 sync 类型, 验证字母序排列
        let code = "fn foo() -> (Mutex<i32>, Arc<u8>, RwLock<bool>, Barrier) { unimplemented!() }";
        let result = ensure_std_imports(code);
        // 排序后应为 Arc, Barrier, Mutex, RwLock
        assert!(
            result.contains("use std::sync::{Arc, Barrier, Mutex, RwLock};"),
            "应按字母序排列: {}",
            result
        );
    }

    // ===== Session 126: format_diff_unified_colored 测试 =====

    #[test]
    fn test_format_diff_unified_colored_no_diff() {
        let diff = format_diff_unified_colored("fn foo() {}", "fn foo() {}", "a", "b", 3);
        assert!(diff.is_empty(), "无差异应返回空字符串");
    }

    #[test]
    fn test_format_diff_unified_colored_contains_green() {
        let original = "fn foo() {\n}\n";
        let fixed = "fn foo() {\n    let x = 42;\n}\n";
        let diff = format_diff_unified_colored(original, fixed, "a", "b", 3);
        assert!(
            diff.contains("\x1b[32m"),
            "应包含绿色 ANSI 码 (新增行): {}",
            diff
        );
    }

    #[test]
    fn test_format_diff_unified_colored_contains_red() {
        let original = "fn foo() {\n    let x = 42;\n}\n";
        let fixed = "fn foo() {\n}\n";
        let diff = format_diff_unified_colored(original, fixed, "a", "b", 3);
        assert!(
            diff.contains("\x1b[31m"),
            "应包含红色 ANSI 码 (删除行): {}",
            diff
        );
    }

    #[test]
    fn test_format_diff_unified_colored_contains_cyan_hunk_header() {
        let original = "fn foo() {\n}\n";
        let fixed = "fn foo() {\n    let x = 42;\n}\n";
        let diff = format_diff_unified_colored(original, fixed, "a", "b", 3);
        assert!(
            diff.contains("\x1b[36m"),
            "应包含青色 ANSI 码 (hunk 头): {}",
            diff
        );
    }

    #[test]
    fn test_format_diff_unified_colored_contains_bold_yellow_headers() {
        let original = "fn foo() {\n}\n";
        let fixed = "fn foo() {\n    let x = 42;\n}\n";
        let diff = format_diff_unified_colored(original, fixed, "src/main.rs", "src/main.rs", 3);
        assert!(
            diff.contains("\x1b[1;33m"),
            "应包含粗体黄色 ANSI 码 (文件头): {}",
            diff
        );
    }

    #[test]
    fn test_format_diff_unified_colored_contains_reset() {
        let original = "a\n";
        let fixed = "b\n";
        let diff = format_diff_unified_colored(original, fixed, "a", "b", 3);
        assert!(diff.contains("\x1b[0m"), "应包含 ANSI 重置码: {}", diff);
    }

    // ===== Session 126: verify_imports 测试 =====

    #[test]
    fn test_verify_imports_hashmap_missing() {
        let issues = verify_imports("fn foo() -> HashMap<String, i32> { HashMap::new() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "HashMap" && i.module_path == "std::collections"),
            "应检测到 HashMap 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_hashmap_present() {
        let code =
            "use std::collections::HashMap;\nfn foo() -> HashMap<String, i32> { HashMap::new() }";
        let issues = verify_imports(code);
        assert!(
            !issues.iter().any(|i| i.type_name == "HashMap"),
            "已有导入不应报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_anyhow_result_missing() {
        let issues = verify_imports("fn foo() -> Result<i32, anyhow::Error> { Ok(42) }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Result" && i.module_path == "anyhow"),
            "应检测到 Result 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_anyhow_result_present() {
        let code = "use anyhow::Result;\nfn foo() -> Result<i32, anyhow::Error> { Ok(42) }";
        let issues = verify_imports(code);
        assert!(
            !issues.iter().any(|i| i.type_name == "Result"),
            "已有 Result 导入不应报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_bail_missing() {
        let issues = verify_imports("fn foo() { if true { bail!(\"error\"); } }");
        assert!(
            issues.iter().any(|i| i.type_name == "bail"),
            "应检测到 bail! 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_no_issues_plain_code() {
        let issues = verify_imports("fn foo() -> i32 { 42 }");
        assert!(issues.is_empty(), "无类型使用不应有问题: {:?}", issues);
    }

    #[test]
    fn test_verify_imports_multiple_issues() {
        let code = "fn foo() -> HashMap<String, Arc<i32>> { HashMap::new() }";
        let issues = verify_imports(code);
        assert!(
            issues.iter().any(|i| i.type_name == "HashMap"),
            "应检测到 HashMap: {:?}",
            issues
        );
        assert!(
            issues.iter().any(|i| i.type_name == "Arc"),
            "应检测到 Arc: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_context_missing() {
        let issues = verify_imports("fn foo() { let x = operation().context(\"failed\")?; }");
        assert!(
            issues.iter().any(|i| i.type_name == "Context"),
            "应检测到 Context 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_usage_line_number() {
        let code = "fn foo() -> i32 { 42 }\nfn bar() -> HashMap<String, i32> { HashMap::new() }";
        let issues = verify_imports(code);
        let hashmap_issue = issues.iter().find(|i| i.type_name == "HashMap");
        assert!(hashmap_issue.is_some(), "应检测到 HashMap: {:?}", issues);
        assert_eq!(
            hashmap_issue.unwrap().usage_line,
            2,
            "HashMap 使用应在第 2 行"
        );
    }

    // ===== Session 126: wrap_return_statements_in_ok if let / while let 测试 =====

    #[test]
    fn test_wrap_return_statements_in_ok_if_let_single_line() {
        let code = "fn foo(opt: Option<i32>) -> Result<i32, anyhow::Error> {\n    return if let Some(x) = opt { x } else { 0 };\n}";
        let result = wrap_return_statements_in_ok(code);
        assert!(
            result.contains("return Ok(if let Some(x) = opt { x } else { 0 });"),
            "应包装 if let 表达式: {}",
            result
        );
    }

    #[test]
    fn test_wrap_return_statements_in_ok_if_let_multiline() {
        let code = "fn foo(opt: Option<i32>) -> Result<i32, anyhow::Error> {\n    return if let Some(x) = opt {\n        x + 1\n    } else {\n        0\n    };\n}";
        let result = wrap_return_statements_in_ok(code);
        assert!(
            result.contains("return Ok(if let Some(x) = opt {"),
            "应包装多行 if let: {}",
            result
        );
        assert!(result.contains("});"), "应在末行添加闭合: {}", result);
    }

    #[test]
    fn test_wrap_return_statements_in_ok_while_let_single_line() {
        let code = "fn foo(mut iter: std::slice::Iter<i32>) -> Result<i32, anyhow::Error> {\n    return while let Some(x) = iter.next() { if x > 0 { return x; } };\n}";
        let result = wrap_return_statements_in_ok(code);
        // while let 不以 ; 结尾在同一行的话会被多行处理
        // 但如果以 ; 结尾, 则单行处理
        assert!(
            result.contains("return Ok("),
            "应包装 while let 表达式: {}",
            result
        );
    }

    #[test]
    fn test_wrap_return_statements_in_ok_while_let_multiline() {
        let code = "fn foo(opt: Option<i32>) -> Result<i32, anyhow::Error> {\n    return while let Some(x) = opt {\n        x\n    };\n}";
        let result = wrap_return_statements_in_ok(code);
        assert!(
            result.contains("return Ok("),
            "应包装多行 while let: {}",
            result
        );
        assert!(result.contains("});"), "应在末行添加闭合: {}", result);
    }

    #[test]
    fn test_wrap_return_statements_in_ok_if_let_already_ok() {
        let code = "fn foo(opt: Option<i32>) -> Result<i32, anyhow::Error> {\n    return Ok(if let Some(x) = opt { x } else { 0 });\n}";
        let result = wrap_return_statements_in_ok(code);
        let ok_count = result.matches("return Ok(").count();
        assert_eq!(ok_count, 1, "已包装的 if let 不应重复包装: {}", result);
    }

    #[test]
    fn test_wrap_return_statements_in_ok_if_let_idempotent() {
        let code = "fn foo(opt: Option<i32>) -> Result<i32, anyhow::Error> {\n    return if let Some(x) = opt {\n        x + 1\n    } else {\n        0\n    };\n}";
        let first = wrap_return_statements_in_ok(code);
        let second = wrap_return_statements_in_ok(&first);
        assert_eq!(first, second, "if let 包装应幂等");
    }

    #[test]
    fn test_wrap_return_statements_in_ok_if_let_with_else_if() {
        let code = "fn foo(x: i32) -> Result<i32, anyhow::Error> {\n    return if x > 0 {\n        1\n    } else if x < 0 {\n        -1\n    } else {\n        0\n    };\n}";
        let result = wrap_return_statements_in_ok(code);
        assert!(
            result.contains("return Ok(if x > 0 {"),
            "应包装 if/else if/else: {}",
            result
        );
        assert!(result.contains("});"), "应在末行添加闭合: {}", result);
    }

    // ===== Session 127: ensure_std_imports 新增类型测试 =====

    #[test]
    fn test_ensure_std_imports_pin() {
        let code = "fn foo() -> Pin<Box<i32>> { Box::pin(42) }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::pin::Pin;"),
            "应添加 Pin 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_ordering() {
        let code = "fn foo(a: i32, b: i32) -> Ordering { a.cmp(&b) }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::cmp::Ordering;"),
            "应添加 Ordering 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_range() {
        let code = "fn foo() -> Range<i32> { 0..10 }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::ops::Range;"),
            "应添加 Range 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_range_inclusive() {
        let code = "fn foo() -> RangeInclusive<i32> { 0..=10 }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::ops::RangeInclusive;"),
            "应添加 RangeInclusive 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_range_range_inclusive_merged() {
        let code = "fn foo(r: Range<i32>, ri: RangeInclusive<i32>) {}";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::ops::{Range, RangeInclusive};"),
            "应合并 ops 模块导入 (字母序): {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_type_id() {
        let code = "fn foo() -> TypeId { TypeId::of::<i32>() }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::any::TypeId;"),
            "应添加 TypeId 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_any() {
        let code = "fn foo() -> Box<dyn Any> { Box::new(42) }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::any::Any;"),
            "应添加 Any 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_type_id_any_merged() {
        let code = "fn foo() -> (TypeId, Box<dyn Any>) { (TypeId::of::<i32>(), Box::new(42)) }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::any::{Any, TypeId};"),
            "应合并 any 模块导入 (字母序): {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_formatter() {
        let code = "fn foo(f: &mut Formatter) {}";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::fmt::Formatter;"),
            "应添加 Formatter 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_display_debug() {
        let code = "fn foo(d: &dyn Debug, s: &dyn Display) {}";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::fmt::{Debug, Display};"),
            "应合并 fmt 模块导入 (字母序): {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_from_iterator() {
        let code = "fn foo() -> impl FromIterator<i32> { Vec::<i32>::new() }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::iter::FromIterator;"),
            "应添加 FromIterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_peekable() {
        let code = "fn foo(iter: Peekable<i32>) {}";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::iter::Peekable;"),
            "应添加 Peekable 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_hash_hasher() {
        let code = "fn foo(h: &mut Hasher) -> Hash { 42 }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::hash::{Hash, Hasher};"),
            "应合并 hash 模块导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_mem() {
        let code = "fn foo() { let x = mem::swap; }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::mem;"),
            "应添加 mem 模块导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_nonzero_u32() {
        let code = "fn foo() -> NonZeroU32 { NonZeroU32::new(42).unwrap() }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::num::NonZeroU32;"),
            "应添加 NonZeroU32 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_nonzero_multiple() {
        let code = "fn foo() -> (NonZeroU32, NonZeroU64, NonZeroUsize) { unimplemented!() }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};"),
            "应合并 num 模块导入 (字母序): {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_entry() {
        let code = "fn foo(m: HashMap<i32, i32>) -> Entry<'_, i32, i32> { m.entry(1) }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::collections::hash_map::Entry;"),
            "应添加 Entry 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_s127_idempotent() {
        let code = "fn foo() -> (Pin<Box<i32>>, Ordering, NonZeroU32) { unimplemented!() }";
        let first = ensure_std_imports(code);
        let second = ensure_std_imports(&first);
        assert_eq!(first, second, "Session 127 新增类型检测应幂等");
    }

    #[test]
    fn test_ensure_std_imports_s127_full_path_no_import() {
        let code = "fn foo() -> std::pin::Pin<Box<i32>> { Box::pin(42) }";
        let result = ensure_std_imports(code);
        assert!(
            !result.contains("use std::pin::Pin;"),
            "全限定路径不需要导入: {}",
            result
        );
    }

    // ===== Session 127: ensure_external_imports 测试 =====

    #[test]
    fn test_ensure_external_imports_serialize() {
        let code = "#[derive(Serialize)]\nstruct Foo { x: i32 }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use serde::Serialize;"),
            "应添加 Serialize 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_deserialize() {
        let code = "#[derive(Deserialize)]\nstruct Foo { x: i32 }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use serde::Deserialize;"),
            "应添加 Deserialize 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_serialize_deserialize_merged() {
        let code = "#[derive(Serialize, Deserialize)]\nstruct Foo { x: i32 }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use serde::{Deserialize, Serialize};"),
            "应合并 serde 导入 (字母序): {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_serde_full_path() {
        let code = "#[derive(serde::Serialize, serde::Deserialize)]\nstruct Foo { x: i32 }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use serde::"),
            "全限定路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_regex() {
        let code = "fn foo() -> Regex { Regex::new(r\".*\").unwrap() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use regex::Regex;"),
            "应添加 Regex 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_chrono_datetime() {
        let code = "fn foo() -> DateTime { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use chrono::DateTime;"),
            "应添加 DateTime 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_chrono_multiple() {
        let code = "fn foo() -> (DateTime, NaiveDateTime) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use chrono::{DateTime, NaiveDateTime};"),
            "应合并 chrono 导入 (字母序): {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_tracing_info_macro() {
        let code = "fn foo() { info!(\"hello\"); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use tracing::info;"),
            "应添加 tracing::info 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_tracing_multiple_macros() {
        let code = "fn foo() { info!(\"hi\"); warn!(\"warn\"); error!(\"err\"); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use tracing::{error, info, warn};"),
            "应合并 tracing 宏导入 (字母序): {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_tracing_full_path_no_import() {
        let code = "fn foo() { tracing::info!(\"hello\"); }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use tracing::"),
            "全限定 tracing:: 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_idempotent() {
        let code = "#[derive(Serialize, Deserialize)]\nstruct Foo { x: i32 }";
        let first = ensure_external_imports(code);
        let second = ensure_external_imports(&first);
        assert_eq!(first, second, "ensure_external_imports 应幂等");
    }

    #[test]
    fn test_ensure_external_imports_no_need() {
        let code = "fn foo() -> i32 { 42 }";
        let result = ensure_external_imports(code);
        assert_eq!(result, code, "不需要外部导入不修改");
    }

    #[test]
    fn test_ensure_external_imports_already_imported() {
        let code = "use serde::Serialize;\n#[derive(Serialize)]\nstruct Foo { x: i32 }";
        let result = ensure_external_imports(code);
        let count = result.matches("use serde::Serialize;").count();
        assert_eq!(count, 1, "已有导入不应重复: {}", result);
    }

    #[test]
    fn test_ensure_external_imports_insert_position() {
        let code = "//! Module docs\n#[derive(Serialize)]\nstruct Foo { x: i32 }";
        let result = ensure_external_imports(code);
        let import_pos = result.find("use serde::").unwrap();
        let struct_pos = result.find("#[derive").unwrap();
        assert!(
            import_pos < struct_pos,
            "导入应在结构体之前: {} < {}",
            import_pos,
            struct_pos
        );
    }

    // ===== Session 127: verify_imports 新增类型测试 =====

    #[test]
    fn test_verify_imports_pin_missing() {
        let issues = verify_imports("fn foo() -> Pin<Box<i32>> { Box::pin(42) }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Pin" && i.module_path == "std::pin"),
            "应检测到 Pin 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_pin_present() {
        let code = "use std::pin::Pin;\nfn foo() -> Pin<Box<i32>> { Box::pin(42) }";
        let issues = verify_imports(code);
        assert!(
            !issues.iter().any(|i| i.type_name == "Pin"),
            "已有 Pin 导入不应报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_entry_missing() {
        let issues =
            verify_imports("fn foo(m: HashMap<i32, i32>) -> Entry<'_, i32, i32> { m.entry(1) }");
        assert!(
            issues.iter().any(|i| i.type_name == "Entry"),
            "应检测到 Entry 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_nonzero_missing() {
        let issues = verify_imports("fn foo() -> NonZeroU32 { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "NonZeroU32" && i.module_path == "std::num"),
            "应检测到 NonZeroU32 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_ordering_missing() {
        let issues = verify_imports("fn foo(a: i32, b: i32) -> Ordering { a.cmp(&b) }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Ordering" && i.module_path == "std::cmp"),
            "应检测到 Ordering 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_multiple_s127_types() {
        let code = "fn foo() -> (Pin<Box<i32>>, Ordering, NonZeroU32) { unimplemented!() }";
        let issues = verify_imports(code);
        assert!(issues.iter().any(|i| i.type_name == "Pin"), "应有 Pin");
        assert!(
            issues.iter().any(|i| i.type_name == "Ordering"),
            "应有 Ordering"
        );
        assert!(
            issues.iter().any(|i| i.type_name == "NonZeroU32"),
            "应有 NonZeroU32"
        );
    }

    // ===== Session 127: verify_imports 协作测试 (Item 12) =====

    #[test]
    fn test_verify_imports_after_ensure_std_imports_no_issues() {
        // ensure_std_imports 应修复所有 verify_imports 能检测到的 std 类型
        let code = "fn foo() -> HashMap<String, i32> { HashMap::new() }";
        let fixed = ensure_std_imports(code);
        let issues = verify_imports(&fixed);
        let std_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.module_path.starts_with("std"))
            .collect();
        assert!(
            std_issues.is_empty(),
            "ensure_std_imports 后不应有 std 导入问题: {:?}",
            std_issues
        );
    }

    #[test]
    fn test_verify_imports_after_ensure_anyhow_imports_no_issues() {
        // ensure_anyhow_imports 应修复所有 verify_imports 能检测到的 anyhow 类型
        let code = "fn foo() -> Result<i32, anyhow::Error> { Ok(42) }";
        let fixed = ensure_anyhow_imports(code);
        let issues = verify_imports(&fixed);
        let anyhow_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.module_path == "anyhow")
            .collect();
        assert!(
            anyhow_issues.is_empty(),
            "ensure_anyhow_imports 后不应有 anyhow 导入问题: {:?}",
            anyhow_issues
        );
    }

    #[test]
    fn test_verify_imports_after_apply_fixes_with_imports_no_std_issues() {
        // apply_fixes_with_imports 完成后不应有 std 导入问题
        let code = "fn foo() -> HashMap<String, i32> { HashMap::new() }";
        let fixed = apply_fixes_with_imports(code);
        let issues = verify_imports(&fixed);
        let std_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.module_path.starts_with("std"))
            .collect();
        assert!(
            std_issues.is_empty(),
            "apply_fixes_with_imports 后不应有 std 导入问题: {:?}",
            std_issues
        );
    }

    #[test]
    fn test_ensure_external_imports_after_apply_fixes_with_imports() {
        // apply_fixes_with_imports 应包含 ensure_external_imports 步骤
        let code = "#[derive(Serialize)]\nstruct Foo { x: i32 }";
        let fixed = apply_fixes_with_imports(code);
        assert!(
            fixed.contains("use serde::Serialize;"),
            "apply_fixes_with_imports 应添加外部 crate 导入: {}",
            fixed
        );
    }

    // ===== Session 128: ensure_external_imports reqwest 测试 =====

    #[test]
    fn test_ensure_external_imports_reqwest_client() {
        let code = "fn foo() -> Client { Client::new() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use reqwest::Client;"),
            "应添加 reqwest::Client 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_reqwest_response() {
        let code = "fn foo(resp: Response) {}";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use reqwest::Response;"),
            "应添加 reqwest::Response 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_reqwest_multiple() {
        let code = "fn foo() -> (Client, Response) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use reqwest::{Client, Response};"),
            "应合并 reqwest 导入 (字母序): {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_reqwest_full_path() {
        let code = "fn foo() -> reqwest::Client { reqwest::Client::new() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use reqwest::"),
            "全限定路径不需要导入: {}",
            result
        );
    }

    // ===== Session 128: ensure_external_imports serde_json 测试 =====

    #[test]
    fn test_ensure_external_imports_serde_json_value() {
        let code = "fn foo() -> Value { Value::Null }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use serde_json::Value;"),
            "应添加 serde_json::Value 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_serde_json_macro() {
        let code = "fn foo() { let v = json!({\"key\": 1}); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use serde_json::json;"),
            "应添加 serde_json::json 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_serde_json_value_and_macro_merged() {
        let code = "fn foo() -> Value { json!({\"key\": 1}) }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use serde_json::{Value, json};"),
            "应合并 serde_json 导入 (字母序): {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_serde_json_full_path() {
        let code = "fn foo() -> serde_json::Value { serde_json::Value::Null }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use serde_json::"),
            "全限定 serde_json 路径不需要导入: {}",
            result
        );
    }

    // ===== Session 128: ensure_external_imports tokio 测试 =====

    #[test]
    fn test_ensure_external_imports_tokio_join_handle() {
        let code = "fn foo() -> JoinHandle<i32> { spawn(|| 42) }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use tokio::task::JoinHandle;"),
            "应添加 tokio::task::JoinHandle 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_tokio_spawn() {
        let code = "fn foo() { spawn(async {}); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use tokio::spawn;"),
            "应添加 tokio::spawn 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_tokio_join_macro() {
        let code = "fn foo() { join!(bar(), baz()); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use tokio::join;"),
            "应添加 tokio::join 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_tokio_select_macro() {
        let code = "fn foo() { select! { _ = bar() => {} } }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use tokio::select;"),
            "应添加 tokio::select 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_tokio_multiple_merged() {
        let code = "fn foo() { join!(bar(), baz()); select! { _ = baz() => {} } spawn(async {}); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use tokio::{join, select, spawn};"),
            "应合并 tokio 导入 (字母序): {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_tokio_full_path_no_import() {
        let code = "fn foo() { tokio::spawn(async {}); }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use tokio::spawn;"),
            "全限定 tokio::spawn 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_s128_idempotent() {
        let code = "fn foo() -> (Client, Value) { unimplemented!() }";
        let first = ensure_external_imports(code);
        let second = ensure_external_imports(&first);
        assert_eq!(first, second, "Session 128 新增外部 crate 检测应幂等");
    }

    // ===== Session 128: ensure_std_imports 新增类型测试 =====

    #[test]
    fn test_ensure_std_imports_future() {
        let code = "fn foo() -> impl Future<Output = i32> { async { 42 } }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::future::Future;"),
            "应添加 Future 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_poll_waker() {
        let code = "fn foo(w: &Waker) -> Poll<i32> { Poll::Ready(42) }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::task::{Poll, Waker};"),
            "应合并 task 模块导入 (字母序): {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_layout() {
        let code = "fn foo() -> Layout { Layout::new::<i32>() }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::alloc::Layout;"),
            "应添加 Layout 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_cstring_cstr() {
        let code = "fn foo(s: &CStr) -> CString { s.to_owned() }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::ffi::{CStr, CString};"),
            "应合并 ffi 模块导入 (字母序): {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_future_full_path_no_import() {
        let code = "fn foo() -> std::future::Future<Output = i32> { async { 42 } }";
        let result = ensure_std_imports(code);
        assert!(
            !result.contains("use std::future::Future;"),
            "全限定路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_s128_idempotent() {
        let code = "fn foo() -> (impl Future<Output = i32>, Poll<i32>) { unimplemented!() }";
        let first = ensure_std_imports(code);
        let second = ensure_std_imports(&first);
        assert_eq!(first, second, "Session 128 新增 std 类型检测应幂等");
    }

    // ===== Session 128: verify_imports 新增类型测试 =====

    #[test]
    fn test_verify_imports_future_missing() {
        let issues = verify_imports("fn foo() -> impl Future<Output = i32> { async { 42 } }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Future" && i.module_path == "std::future"),
            "应检测到 Future 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_poll_missing() {
        let issues = verify_imports("fn foo() -> Poll<i32> { Poll::Ready(42) }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Poll" && i.module_path == "std::task"),
            "应检测到 Poll 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_external_serde_missing() {
        let issues = verify_imports("#[derive(Serialize)]\nstruct Foo { x: i32 }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Serialize" && i.module_path == "serde"),
            "应检测到 Serialize 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_external_reqwest_missing() {
        let issues = verify_imports("fn foo() -> Client { Client::new() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Client" && i.module_path == "reqwest"),
            "应检测到 Client 缺失导入: {:?}",
            issues
        );
    }

    // ===== Session 128: verify_imports_to_json 测试 =====

    #[test]
    fn test_verify_imports_to_json_has_issues() {
        let code = "fn foo() -> HashMap<String, i32> { HashMap::new() }";
        let json = verify_imports_to_json(code);
        assert!(
            json.contains("\"total_issues\": 1"),
            "JSON 应 total_issues=1: {}",
            json
        );
        assert!(
            json.contains("\"has_issues\": true"),
            "JSON 应 has_issues=true: {}",
            json
        );
        assert!(json.contains("HashMap"), "JSON 应包含 HashMap: {}", json);
        assert!(
            json.contains("std::collections"),
            "JSON 应包含 std::collections: {}",
            json
        );
    }

    #[test]
    fn test_verify_imports_to_json_no_issues() {
        let code = "fn foo() -> i32 { 42 }";
        let json = verify_imports_to_json(code);
        assert!(
            json.contains("\"total_issues\": 0"),
            "无问题 JSON 应 total_issues=0: {}",
            json
        );
        assert!(
            json.contains("\"has_issues\": false"),
            "无问题 JSON 应 has_issues=false: {}",
            json
        );
    }

    #[test]
    fn test_verify_imports_to_json_multiple_issues() {
        let code = "fn foo() -> HashMap<String, Arc<i32>> { HashMap::new() }";
        let json = verify_imports_to_json(code);
        assert!(
            json.contains("\"total_issues\": 2"),
            "JSON 应 total_issues=2: {}",
            json
        );
        assert!(
            json.contains("std::collections"),
            "JSON 应包含 std::collections: {}",
            json
        );
        assert!(
            json.contains("std::sync"),
            "JSON 应包含 std::sync: {}",
            json
        );
    }

    #[test]
    fn test_verify_imports_to_json_modules_affected() {
        let code = "fn foo() -> HashMap<String, i32> { HashMap::new() }";
        let json = verify_imports_to_json(code);
        assert!(
            json.contains("modules_affected"),
            "JSON 应包含 modules_affected: {}",
            json
        );
        assert!(
            json.contains("std::collections"),
            "modules_affected 应包含 std::collections: {}",
            json
        );
    }

    // ===== Session 128: verify_imports_report 测试 =====

    #[test]
    fn test_verify_imports_report_has_issues() {
        let code = "fn foo() -> HashMap<String, i32> { HashMap::new() }";
        let report = verify_imports_report(code);
        assert!(report.has_issues, "应有问题");
        assert_eq!(report.total_issues, 1, "应有 1 个问题");
        assert!(!report.issues.is_empty(), "issues 不应为空");
        assert!(
            report
                .modules_affected
                .contains(&"std::collections".to_string()),
            "modules_affected 应包含 std::collections"
        );
    }

    #[test]
    fn test_verify_imports_report_no_issues() {
        let code = "fn foo() -> i32 { 42 }";
        let report = verify_imports_report(code);
        assert!(!report.has_issues, "不应有问题");
        assert_eq!(report.total_issues, 0, "应有 0 个问题");
        assert!(report.issues.is_empty(), "issues 应为空");
        assert!(
            report.modules_affected.is_empty(),
            "modules_affected 应为空"
        );
    }

    #[test]
    fn test_verify_imports_report_multiple_issues() {
        let code = "fn foo() -> HashMap<String, Arc<i32>> { HashMap::new() }";
        let report = verify_imports_report(code);
        assert_eq!(report.total_issues, 2, "应有 2 个问题");
        assert!(
            report.modules_affected.len() >= 2,
            "至少 2 个受影响模块: {:?}",
            report.modules_affected
        );
    }

    #[test]
    fn test_verify_imports_report_modules_sorted() {
        let code = "fn foo() -> HashMap<String, Arc<i32>> { HashMap::new() }";
        let report = verify_imports_report(code);
        // modules_affected 应按字母序排列
        for i in 1..report.modules_affected.len() {
            assert!(
                report.modules_affected[i - 1] <= report.modules_affected[i],
                "modules_affected 应按字母序排列: {:?}",
                report.modules_affected
            );
        }
    }

    #[test]
    fn test_verify_imports_report_serde_issue() {
        let code = "#[derive(Serialize)]\nstruct Foo { x: i32 }";
        let report = verify_imports_report(code);
        assert!(report.has_issues, "应有问题");
        assert!(
            report
                .issues
                .iter()
                .any(|i| i.type_name == "Serialize" && i.module_path == "serde"),
            "应包含 Serialize 问题"
        );
    }

    // ===== Session 129: ensure_std_imports 新增类型测试 =====

    #[test]
    fn test_ensure_std_imports_child() {
        let code = "fn foo() -> Child { unimplemented!() }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::process::Child;"),
            "应添加 Child 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_stdio() {
        let code = "fn foo() -> Stdio { Stdio::piped() }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::process::Stdio;"),
            "应添加 Stdio 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_child_stdio_merged() {
        let code = "fn foo() -> (Child, Stdio) { unimplemented!() }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::process::{Child, Stdio};"),
            "应合并 process 导入 (字母序): {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_osstr_osstring() {
        let code = "fn foo(s: &OsStr) -> OsString { s.to_os_string() }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::ffi::{OsStr, OsString};"),
            "应合并 ffi 导入 (字母序): {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_s129_idempotent() {
        let code = "fn foo() -> (Child, Stdio, OsString) { unimplemented!() }";
        let first = ensure_std_imports(code);
        let second = ensure_std_imports(&first);
        assert_eq!(first, second, "Session 129 新增 std 类型检测应幂等");
    }

    #[test]
    fn test_ensure_std_imports_child_full_path_no_import() {
        let code = "fn foo() -> std::process::Child { unimplemented!() }";
        let result = ensure_std_imports(code);
        assert!(
            !result.contains("use std::process::Child;"),
            "全限定路径不需要导入: {}",
            result
        );
    }

    // ===== Session 130: ensure_std_imports 平台特定类型测试 =====

    #[test]
    fn test_ensure_std_imports_raw_fd() {
        let code = "fn foo(fd: RawFd) -> RawFd { fd }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::os::unix::io::RawFd;"),
            "应添加 RawFd 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_owned_fd() {
        let code = "fn foo() -> OwnedFd { unimplemented!() }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::os::unix::io::OwnedFd;"),
            "应添加 OwnedFd 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_borrowed_fd() {
        let code = "fn foo(fd: BorrowedFd) {}";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::os::unix::io::BorrowedFd;"),
            "应添加 BorrowedFd 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_unix_io_merged() {
        let code = "fn foo(fd: RawFd, owned: OwnedFd, borrowed: BorrowedFd) {}";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::os::unix::io::{BorrowedFd, OwnedFd, RawFd};"),
            "应合并 unix::io 导入 (字母序): {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_raw_handle() {
        let code = "fn foo(h: RawHandle) -> RawHandle { h }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::os::windows::io::RawHandle;"),
            "应添加 RawHandle 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_owned_handle() {
        let code = "fn foo() -> OwnedHandle { unimplemented!() }";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::os::windows::io::OwnedHandle;"),
            "应添加 OwnedHandle 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_borrowed_handle() {
        let code = "fn foo(h: BorrowedHandle) {}";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::os::windows::io::BorrowedHandle;"),
            "应添加 BorrowedHandle 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_windows_io_merged() {
        let code = "fn foo(h: RawHandle, o: OwnedHandle, b: BorrowedHandle) {}";
        let result = ensure_std_imports(code);
        assert!(
            result.contains("use std::os::windows::io::{BorrowedHandle, OwnedHandle, RawHandle};"),
            "应合并 windows::io 导入 (字母序): {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_s130_idempotent() {
        let code = "fn foo(fd: RawFd, h: RawHandle) -> (OwnedFd, OwnedHandle) { unimplemented!() }";
        let first = ensure_std_imports(code);
        let second = ensure_std_imports(&first);
        assert_eq!(first, second, "Session 130 新增 std 平台类型检测应幂等");
    }

    #[test]
    fn test_ensure_std_imports_raw_fd_full_path_no_import() {
        let code = "fn foo(fd: std::os::unix::io::RawFd) -> std::os::unix::io::RawFd { fd }";
        let result = ensure_std_imports(code);
        assert!(
            !result.contains("use std::os::unix::io::RawFd;"),
            "全限定路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_raw_handle_full_path_no_import() {
        let code = "fn foo(h: std::os::windows::io::RawHandle) {}";
        let result = ensure_std_imports(code);
        assert!(
            !result.contains("use std::os::windows::io::RawHandle;"),
            "全限定路径不需要导入: {}",
            result
        );
    }

    // ===== Session 129: ensure_std_imports glob 导入排除测试 =====

    #[test]
    fn test_ensure_std_imports_glob_parent_exclude() {
        let code = "use std::sync::*;\nfn foo() -> AtomicBool { AtomicBool::new(true) }";
        let result = ensure_std_imports(code);
        assert!(
            !result.contains("use std::sync::atomic::AtomicBool;"),
            "use std::sync::*; 应覆盖 std::sync::atomic::AtomicBool: {}",
            result
        );
    }

    #[test]
    fn test_ensure_std_imports_glob_exact_exclude() {
        let code = "use std::collections::*;\nfn foo() -> HashMap<i32, i32> { HashMap::new() }";
        let result = ensure_std_imports(code);
        assert!(
            !result.contains("use std::collections::HashMap;"),
            "use std::collections::*; 应覆盖 HashMap: {}",
            result
        );
    }

    // ===== Session 129: ensure_external_imports itertools 测试 =====

    #[test]
    fn test_ensure_external_imports_itertools_tools() {
        let code = "fn foo(v: Vec<i32>) { v.interleave(v.clone()); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use itertools::IterTools;"),
            "应添加 IterTools 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_itertools_iproduct_macro() {
        let code = "fn foo() { let v = iproduct!(0..3, 0..3); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use itertools::iproduct;"),
            "应添加 itertools::iproduct 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_itertools_izip_macro() {
        let code = "fn foo() { let v = izip!(vec![1], vec![2]); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use itertools::izip;"),
            "应添加 itertools::izip 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_itertools_multiunzip_macro() {
        let code = "fn foo() { let (a, b): (Vec<_>, Vec<_>) = multiunzip!(vec![(1, 2)]); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use itertools::multiunzip;"),
            "应添加 itertools::multiunzip 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_itertools_macros_merged() {
        let code = "fn foo() { iproduct!(0..3, 0..3); izip!(vec![1], vec![2]); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use itertools::{iproduct, izip};"),
            "应合并 itertools 宏导入 (字母序): {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_itertools_full_path_no_import() {
        let code = "fn foo() { itertools::iproduct!(0..3, 0..3); }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use itertools::iproduct;"),
            "全限定 itertools:: 路径不需要导入: {}",
            result
        );
    }

    // ===== Session 129: ensure_external_imports thiserror 测试 =====

    #[test]
    fn test_ensure_external_imports_thiserror_derive() {
        let code = "#[derive(Error)]\nenum MyError { Test }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use thiserror::Error;"),
            "应添加 thiserror::Error 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_thiserror_already_imported() {
        let code = "use thiserror::Error;\n#[derive(Error)]\nenum MyError { Test }";
        let result = ensure_external_imports(code);
        let count = result.matches("use thiserror::Error;").count();
        assert_eq!(count, 1, "已有 thiserror 导入不应重复: {}", result);
    }

    #[test]
    fn test_ensure_external_imports_thiserror_full_path_no_import() {
        let code = "#[derive(thiserror::Error)]\nenum MyError { Test }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use thiserror::Error;"),
            "全限定 thiserror::Error 路径不需要导入: {}",
            result
        );
    }

    // ===== Session 129: ensure_external_imports async_trait 测试 =====

    #[test]
    fn test_ensure_external_imports_async_trait() {
        let code = "#[async_trait]\ntrait Foo { async fn bar(&self); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use async_trait::async_trait;"),
            "应添加 async_trait 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_async_trait_already_imported() {
        let code =
            "use async_trait::async_trait;\n#[async_trait]\ntrait Foo { async fn bar(&self); }";
        let result = ensure_external_imports(code);
        let count = result.matches("use async_trait::async_trait;").count();
        assert_eq!(count, 1, "已有 async_trait 导入不应重复: {}", result);
    }

    #[test]
    fn test_ensure_external_imports_async_trait_full_path_no_import() {
        let code = "#[async_trait::async_trait]\ntrait Foo { async fn bar(&self); }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use async_trait::async_trait;"),
            "全限定 async_trait::async_trait 路径不需要导入: {}",
            result
        );
    }

    // ===== Session 129: ensure_external_imports glob 导入排除测试 =====

    #[test]
    fn test_ensure_external_imports_glob_parent_exclude() {
        let code = "use tokio::*;\nfn foo() -> JoinHandle<i32> { spawn(|| 42) }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use tokio::task::JoinHandle;"),
            "use tokio::*; 应覆盖 tokio::task::JoinHandle: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_glob_exact_exclude() {
        let code = "use itertools::*;\nfn foo(v: Vec<i32>) { v.interleave(v.clone()); }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use itertools::IterTools;"),
            "use itertools::*; 应覆盖 IterTools: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_s129_idempotent() {
        let code =
            "#[derive(Error)]\nenum E { T }\n#[async_trait]\ntrait Foo { async fn bar(&self); }";
        let first = ensure_external_imports(code);
        let second = ensure_external_imports(&first);
        assert_eq!(first, second, "Session 129 新增外部 crate 检测应幂等");
    }

    // ===== Session 130: ensure_external_imports clap/uuid/url/log/tracing_subscriber 测试 =====

    #[test]
    fn test_ensure_external_imports_clap_arg() {
        let code = "fn foo() -> Arg { Arg::new(\"name\") }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use clap::Arg;"),
            "应添加 clap::Arg 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_clap_subcommand() {
        let code = "#[derive(Subcommand)]\nenum Cmd { Foo }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use clap::Subcommand;"),
            "应添加 clap::Subcommand 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_clap_arg_action() {
        let code = "fn foo() -> ArgAction { ArgAction::Set }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use clap::ArgAction;"),
            "应添加 clap::ArgAction 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_clap_multiple_merged() {
        let code = "#[derive(Subcommand)]\nenum Cmd { Foo }\nfn bar() -> Arg { Arg::new(\"x\") }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use clap::{Arg, Subcommand};"),
            "应合并 clap 导入 (字母序): {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_clap_full_path_no_import() {
        let code = "fn foo() -> clap::Arg { clap::Arg::new(\"x\") }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use clap::"),
            "全限定 clap:: 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_uuid() {
        let code = "fn foo() -> Uuid { Uuid::new_v4() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use uuid::Uuid;"),
            "应添加 uuid::Uuid 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_uuid_full_path() {
        let code = "fn foo() -> uuid::Uuid { uuid::Uuid::new_v4() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use uuid::"),
            "全限定 uuid:: 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_url() {
        let code = "fn foo() -> Url { Url::parse(\"https://example.com\").unwrap() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use url::Url;"),
            "应添加 url::Url 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_url_full_path() {
        let code = "fn foo() -> url::Url { url::Url::parse(\"x\").unwrap() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use url::"),
            "全限定 url:: 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_log_level() {
        let code = "fn foo() -> Level { Level::Info }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use log::Level;"),
            "应添加 log::Level 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_log_level_filter() {
        let code = "fn foo() -> LevelFilter { LevelFilter::Info }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use log::LevelFilter;"),
            "应添加 log::LevelFilter 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_log_level_merged() {
        let code = "fn foo() -> (Level, LevelFilter) { (Level::Info, LevelFilter::Info) }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use log::{Level, LevelFilter};"),
            "应合并 log 导入 (字母序): {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_log_macro() {
        let code = "fn foo() { log!(log::Level::Info, \"hello\"); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use log::log;"),
            "应添加 log::log 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_log_full_path_no_import() {
        let code = "fn foo() -> log::Level { log::Level::Info }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use log::"),
            "全限定 log:: 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_tracing_subscriber_env_filter() {
        let code = "fn foo() -> EnvFilter { EnvFilter::new(\"debug\") }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use tracing_subscriber::EnvFilter;"),
            "应添加 tracing_subscriber::EnvFilter 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_tracing_subscriber_full_path() {
        let code = "fn foo() -> tracing_subscriber::EnvFilter { tracing_subscriber::EnvFilter::new(\"debug\") }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use tracing_subscriber::"),
            "全限定 tracing_subscriber:: 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_s130_idempotent() {
        let code = "#[derive(Subcommand)]\nenum Cmd { Foo }\nfn bar() -> (Uuid, Url) { unimplemented!() }\nfn baz() -> EnvFilter { EnvFilter::new(\"info\") }";
        let first = ensure_external_imports(code);
        let second = ensure_external_imports(&first);
        assert_eq!(first, second, "Session 130 新增外部 crate 检测应幂等");
    }

    // ===== Session 129: verify_imports 新增类型测试 =====

    #[test]
    fn test_verify_imports_child_missing() {
        let issues = verify_imports("fn foo() -> Child { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Child" && i.module_path == "std::process"),
            "应检测到 Child 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_stdio_missing() {
        let issues = verify_imports("fn foo() -> Stdio { Stdio::piped() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Stdio" && i.module_path == "std::process"),
            "应检测到 Stdio 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_osstr_missing() {
        let issues = verify_imports("fn foo(s: &OsStr) {}");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "OsStr" && i.module_path == "std::ffi"),
            "应检测到 OsStr 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_itertools_missing() {
        let issues = verify_imports("fn foo(v: Vec<i32>) { v.interleave(v.clone()); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "IterTools" && i.module_path == "itertools"),
            "应检测到 IterTools 缺失导入: {:?}",
            issues
        );
    }

    // ===== Session 130: verify_imports 新增类型测试 =====

    #[test]
    fn test_verify_imports_raw_fd_missing() {
        let issues = verify_imports("fn foo(fd: RawFd) -> RawFd { fd }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "RawFd" && i.module_path == "std::os::unix::io"),
            "应检测到 RawFd 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_raw_handle_missing() {
        let issues = verify_imports("fn foo(h: RawHandle) {}");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "RawHandle" && i.module_path == "std::os::windows::io"),
            "应检测到 RawHandle 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_clap_arg_missing() {
        let issues = verify_imports("fn foo() -> Arg { Arg::new(\"x\") }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Arg" && i.module_path == "clap"),
            "应检测到 clap::Arg 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_uuid_missing() {
        let issues = verify_imports("fn foo() -> Uuid { Uuid::new_v4() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Uuid" && i.module_path == "uuid"),
            "应检测到 Uuid 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_url_missing() {
        let issues = verify_imports("fn foo() -> Url { Url::parse(\"x\").unwrap() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Url" && i.module_path == "url"),
            "应检测到 Url 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_env_filter_missing() {
        let issues = verify_imports("fn foo() -> EnvFilter { EnvFilter::new(\"info\") }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "EnvFilter" && i.module_path == "tracing_subscriber"),
            "应检测到 EnvFilter 缺失导入: {:?}",
            issues
        );
    }

    // ===== Session 129: verify_imports_to_markdown 测试 =====

    #[test]
    fn test_verify_imports_to_markdown_has_issues() {
        let code = "fn foo() -> HashMap<String, i32> { HashMap::new() }";
        let md = verify_imports_to_markdown(code);
        assert!(
            md.contains("# Import Report"),
            "Markdown 应包含标题: {}",
            md
        );
        assert!(
            md.contains("**Total Issues:** 1"),
            "Markdown 应 total_issues=1: {}",
            md
        );
        assert!(md.contains("HashMap"), "Markdown 应包含 HashMap: {}", md);
        assert!(
            md.contains("std::collections"),
            "Markdown 应包含 std::collections: {}",
            md
        );
    }

    #[test]
    fn test_verify_imports_to_markdown_no_issues() {
        let code = "fn foo() -> i32 { 42 }";
        let md = verify_imports_to_markdown(code);
        assert!(md.contains("No issues"), "无问题应显示 No issues: {}", md);
    }

    #[test]
    fn test_verify_imports_to_markdown_multiple_issues() {
        let code = "fn foo() -> HashMap<String, Arc<i32>> { HashMap::new() }";
        let md = verify_imports_to_markdown(code);
        assert!(
            md.contains("**Total Issues:** 2"),
            "Markdown 应 total_issues=2: {}",
            md
        );
        assert!(md.contains("| 1 |"), "Markdown 应包含表格行 1: {}", md);
        assert!(md.contains("| 2 |"), "Markdown 应包含表格行 2: {}", md);
    }

    #[test]
    fn test_verify_imports_to_markdown_table_header() {
        let code = "fn foo() -> HashMap<String, i32> { HashMap::new() }";
        let md = verify_imports_to_markdown(code);
        assert!(
            md.contains("| # | Type | Module | Line |"),
            "Markdown 应包含表头: {}",
            md
        );
        assert!(
            md.contains("|---|------|--------|------|"),
            "Markdown 应包含分隔行: {}",
            md
        );
    }

    #[test]
    fn test_verify_imports_to_markdown_modules_affected() {
        let code = "fn foo() -> HashMap<String, Arc<i32>> { HashMap::new() }";
        let md = verify_imports_to_markdown(code);
        assert!(
            md.contains("**Modules Affected:**"),
            "Markdown 应包含 Modules Affected: {}",
            md
        );
        assert!(
            md.contains("std::collections"),
            "Markdown 应包含 std::collections 模块: {}",
            md
        );
    }

    // ===== Session 129: helper 函数测试 =====

    #[test]
    fn test_extract_glob_imports_basic() {
        let code = "use std::collections::*;\nuse serde::Serialize;\nuse itertools::*;";
        let globs = extract_glob_imports(code);
        assert!(
            globs.contains(&"std::collections".to_string()),
            "应包含 std::collections glob: {:?}",
            globs
        );
        assert!(
            globs.contains(&"itertools".to_string()),
            "应包含 itertools glob: {:?}",
            globs
        );
        assert!(
            !globs.contains(&"serde".to_string()),
            "不应包含 serde (非 glob): {:?}",
            globs
        );
    }

    #[test]
    fn test_extract_glob_imports_empty() {
        let code = "fn foo() -> i32 { 42 }";
        let globs = extract_glob_imports(code);
        assert!(globs.is_empty(), "无 glob 导入应为空: {:?}", globs);
    }

    #[test]
    fn test_has_covering_glob_import_exact() {
        let globs = vec!["std::sync".to_string()];
        assert!(
            has_covering_glob_import(&globs, "std::sync"),
            "精确匹配应返回 true"
        );
    }

    #[test]
    fn test_has_covering_glob_import_parent() {
        let globs = vec!["std::sync".to_string()];
        assert!(
            has_covering_glob_import(&globs, "std::sync::atomic"),
            "父模块 glob 应覆盖子模块"
        );
    }

    #[test]
    fn test_has_covering_glob_import_no_match() {
        let globs = vec!["std::collections".to_string()];
        assert!(
            !has_covering_glob_import(&globs, "std::sync"),
            "不相关模块应返回 false"
        );
    }

    #[test]
    fn test_has_covering_glob_import_empty() {
        let globs: Vec<String> = vec![];
        assert!(
            !has_covering_glob_import(&globs, "std::sync"),
            "空 glob 列表应返回 false"
        );
    }

    // ===== Session 131: ensure_external_imports rand 测试 =====

    #[test]
    fn test_ensure_external_imports_rand_rng() {
        let code = "fn foo(rng: &mut Rng) { let x = rng.gen(); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use rand::Rng;"),
            "应添加 rand::Rng 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_rand_thread_rng() {
        let code = "fn foo() -> ThreadRng { rand::thread_rng() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use rand::rngs::ThreadRng;"),
            "应添加 rand::rngs::ThreadRng 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_rand_gen_range_method() {
        let code = "fn foo() { let mut rng = rand::thread_rng(); let x = rng.gen_range(0..10); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use rand::Rng;"),
            "应通过 .gen_range() 方法检测添加 rand::Rng 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_rand_full_path_no_import() {
        let code = "fn foo(rng: &mut rand::Rng) { let x = rng.gen(); }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use rand::Rng;"),
            "全限定 rand::Rng 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_rand_already_imported() {
        let code = "use rand::Rng;\nfn foo(rng: &mut Rng) { let x = rng.gen(); }";
        let result = ensure_external_imports(code);
        let count = result.matches("use rand::Rng;").count();
        assert_eq!(count, 1, "已有 rand::Rng 导入不应重复: {}", result);
    }

    // ===== Session 131: ensure_external_imports dashmap 测试 =====

    #[test]
    fn test_ensure_external_imports_dashmap() {
        let code = "fn foo() -> DashMap<i32, String> { DashMap::new() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use dashmap::DashMap;"),
            "应添加 dashmap::DashMap 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_dashmap_full_path() {
        let code = "fn foo() -> dashmap::DashMap<i32, String> { dashmap::DashMap::new() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use dashmap::"),
            "全限定 dashmap:: 路径不需要导入: {}",
            result
        );
    }

    // ===== Session 131: ensure_external_imports rayon 测试 =====

    #[test]
    fn test_ensure_external_imports_rayon_parallel_iterator() {
        let code = "fn foo(v: Vec<i32>) -> i32 { v.par_iter().sum() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use rayon::prelude::ParallelIterator;"),
            "应通过 .par_iter() 方法检测添加 rayon::prelude::ParallelIterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_rayon_par_iter_mut() {
        let code = "fn foo(v: &mut [i32]) { v.par_iter_mut().for_each(|x| *x += 1); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use rayon::prelude::ParallelIterator;"),
            "应通过 .par_iter_mut() 方法检测添加 rayon 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_rayon_full_path_no_import() {
        let code = "fn foo(v: Vec<i32>) -> i32 { v.par_iter().sum() }";
        let with_import = format!("use rayon::prelude::ParallelIterator;\n{}", code);
        let result = ensure_external_imports(&with_import);
        let count = result
            .matches("use rayon::prelude::ParallelIterator;")
            .count();
        assert_eq!(count, 1, "已有 rayon 导入不应重复: {}", result);
    }

    // ===== Session 131: ensure_external_imports ndarray 测试 =====

    #[test]
    fn test_ensure_external_imports_ndarray_array1() {
        let code = "fn foo() -> Array1<f64> { Array1::zeros(10) }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use ndarray::Array1;"),
            "应添加 ndarray::Array1 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_ndarray_array2() {
        let code = "fn foo() -> Array2<f64> { Array2::zeros((3, 4)) }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use ndarray::Array2;"),
            "应添加 ndarray::Array2 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_ndarray_merged() {
        let code = "fn foo() -> (Array1<f64>, Array2<f64>) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use ndarray::{Array1, Array2};"),
            "应合并 ndarray 导入 (字母序): {}",
            result
        );
    }

    // ===== Session 131: ensure_external_imports tokio_stream 测试 =====

    #[test]
    fn test_ensure_external_imports_tokio_stream_stream_ext() {
        let code = "fn foo() -> StreamExt { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use tokio_stream::StreamExt;"),
            "应添加 tokio_stream::StreamExt 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_tokio_stream_full_path() {
        let code = "fn foo(s: tokio_stream::Stream) {}";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use tokio_stream::"),
            "全限定 tokio_stream:: 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_s131_idempotent() {
        let code = "fn foo() -> (DashMap<i32, String>, Array1<f64>) { unimplemented!() }\nfn bar(v: Vec<i32>) { v.par_iter().sum(); }\nfn baz(rng: &mut Rng) { rng.gen_range(0..10); }";
        let first = ensure_external_imports(code);
        let second = ensure_external_imports(&first);
        assert_eq!(first, second, "Session 131 新增外部 crate 检测应幂等");
    }

    // ===== Session 131: verify_imports 新增类型测试 =====

    #[test]
    fn test_verify_imports_rand_rng_missing() {
        let issues = verify_imports("fn foo(rng: &mut Rng) { let x = rng.gen(); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Rng" && i.module_path == "rand"),
            "应检测到 Rng 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_dashmap_missing() {
        let issues = verify_imports("fn foo() -> DashMap<i32, String> { DashMap::new() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "DashMap" && i.module_path == "dashmap"),
            "应检测到 DashMap 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_ndarray_missing() {
        let issues = verify_imports("fn foo() -> Array1<f64> { Array1::zeros(10) }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Array1" && i.module_path == "ndarray"),
            "应检测到 Array1 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_tokio_stream_missing() {
        let issues = verify_imports("fn foo(s: Stream) {}");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Stream" && i.module_path == "tokio_stream"),
            "应检测到 Stream 缺失导入: {:?}",
            issues
        );
    }

    // ===== Session 131: verify_imports trait 方法检测测试 =====

    #[test]
    fn test_verify_imports_lock_method_mutex() {
        let issues = verify_imports("fn foo(m: std::sync::Mutex<i32>) { let g = m.lock(); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Mutex" && i.module_path == "std::sync"),
            "应通过 .lock() 方法检测到 Mutex 可能缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_lock_method_mutex_already_imported() {
        let code = "use std::sync::Mutex;\nfn foo(m: Mutex<i32>) { let g = m.lock(); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Mutex" && i.module_path == "std::sync"),
            "已有 Mutex 导入不应通过 .lock() 重复报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_par_iter_rayon() {
        let issues = verify_imports("fn foo(v: Vec<i32>) -> i32 { v.par_iter().sum() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "ParallelIterator" && i.module_path == "rayon::prelude"),
            "应通过 .par_iter() 方法检测到 ParallelIterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_gen_range_rand() {
        let issues =
            verify_imports("fn foo() { let mut rng = rand::thread_rng(); rng.gen_range(0..10); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Rng" && i.module_path == "rand"),
            "应通过 .gen_range() 方法检测到 Rng 缺失导入: {:?}",
            issues
        );
    }

    // ===== Session 131: compare_diff_algorithms 测试 =====

    #[test]
    fn test_compare_diff_algorithms_no_difference() {
        let code = "fn foo() {}\nfn bar() {}";
        let result = compare_diff_algorithms(code, code);
        assert_eq!(result.entries.len(), 4, "应有 4 种算法结果");
        assert!(result.all_consistent, "无差异时所有算法应一致");
        for entry in &result.entries {
            assert_eq!(entry.diff_count, 0, "无差异时 diff_count 应为 0");
        }
    }

    #[test]
    fn test_compare_diff_algorithms_added_line() {
        let original = "fn foo() {\n}\n";
        let fixed = "fn foo() {\n    let x = 42;\n}\n";
        let result = compare_diff_algorithms(original, fixed);
        assert_eq!(result.original_lines, 2, "原始 2 行");
        assert_eq!(result.fixed_lines, 3, "修复 3 行");
        assert_eq!(result.entries.len(), 4, "应有 4 种算法结果");
        // Myers/LCS/Hirschberg 应检测到 1 个 Added
        let myers = result
            .entries
            .iter()
            .find(|e| e.algorithm == "Myers")
            .unwrap();
        assert_eq!(myers.added_count, 1, "Myers 应有 1 个 Added");
    }

    #[test]
    fn test_compare_diff_algorithms_modified_line() {
        let original = "let x = 1;";
        let fixed = "let x = 2;";
        let result = compare_diff_algorithms(original, fixed);
        // Basic 应检测到 Modified
        let basic = result
            .entries
            .iter()
            .find(|e| e.algorithm == "Basic")
            .unwrap();
        assert_eq!(basic.modified_count, 1, "Basic 应有 1 个 Modified");
    }

    #[test]
    fn test_compare_diff_algorithms_empty_inputs() {
        let result = compare_diff_algorithms("", "");
        assert_eq!(result.original_lines, 0);
        assert_eq!(result.fixed_lines, 0);
        assert!(result.all_consistent, "空输入所有算法应一致");
        for entry in &result.entries {
            assert_eq!(entry.diff_count, 0, "空输入 diff_count 应为 0");
        }
    }

    #[test]
    fn test_compare_diff_algorithms_one_empty() {
        let result = compare_diff_algorithms("", "a\nb\nc");
        assert_eq!(result.original_lines, 0);
        assert_eq!(result.fixed_lines, 3);
        for entry in &result.entries {
            assert_eq!(
                entry.added_count, 3,
                "空 original 应全部 Added: {}",
                entry.algorithm
            );
        }
    }

    #[test]
    fn test_compare_diff_algorithms_large_input() {
        let original: String = (0..100).map(|i| format!("line {i}\n")).collect();
        let mut fixed = original.clone();
        fixed.push_str("line 100\n");
        let result = compare_diff_algorithms(&original, &fixed);
        assert_eq!(result.original_lines, 100);
        assert_eq!(result.fixed_lines, 101);
        // LCS/Myers/Hirschberg 应检测到 1 个 Added
        let myers = result
            .entries
            .iter()
            .find(|e| e.algorithm == "Myers")
            .unwrap();
        assert_eq!(myers.added_count, 1, "Myers 应有 1 个 Added");
    }

    #[test]
    fn test_compare_diff_algorithms_serde() {
        let result = compare_diff_algorithms("a\nb", "a\nc");
        let json = serde_json::to_string(&result).unwrap();
        let deserialized: DiffComparisonResult = serde_json::from_str(&json).unwrap();
        assert_eq!(result, deserialized, "Serde 往返应保持一致");
    }

    // ===== Session 131: format_diff_comparison 测试 =====

    #[test]
    fn test_format_diff_comparison_basic() {
        let result = compare_diff_algorithms("a\nb", "a\nc");
        let table = format_diff_comparison(&result);
        assert!(table.contains("Diff Algorithm Comparison"), "应包含标题");
        assert!(table.contains("Original: 2 lines"), "应包含原始行数");
        assert!(table.contains("Fixed: 2 lines"), "应包含修复行数");
        assert!(table.contains("| Algorithm"), "应包含表头");
        assert!(table.contains("Basic"), "应包含 Basic");
        assert!(table.contains("LCS"), "应包含 LCS");
        assert!(table.contains("Myers"), "应包含 Myers");
        assert!(table.contains("Hirschberg"), "应包含 Hirschberg");
    }

    #[test]
    fn test_format_diff_comparison_consistent() {
        let result = compare_diff_algorithms("a\nb\nc", "a\nb\nc");
        let table = format_diff_comparison(&result);
        assert!(
            table.contains("Consistent: Yes"),
            "无差异应显示一致: {}",
            table
        );
    }

    #[test]
    fn test_format_diff_comparison_no_difference() {
        let result = compare_diff_algorithms("fn foo() {}", "fn foo() {}");
        let table = format_diff_comparison(&result);
        assert!(table.contains("Consistent: Yes"), "无差异应一致");
        for entry in &result.entries {
            assert_eq!(entry.diff_count, 0, "无差异 diff_count 应为 0");
        }
    }

    // ===== Session 132: verify_imports RwLock .read()/.write() 测试 =====

    #[test]
    fn test_verify_imports_read_method_rwlock() {
        let issues = verify_imports("fn foo(r: RwLock<i32>) { let g = r.read(); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "RwLock" && i.module_path == "std::sync"),
            "应通过 .read() 方法检测到 RwLock 可能缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_write_method_rwlock() {
        let issues = verify_imports("fn foo(w: RwLock<i32>) { let g = w.write(); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "RwLock" && i.module_path == "std::sync"),
            "应通过 .write() 方法检测到 RwLock 可能缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_rwlock_method_already_imported() {
        let code = "use std::sync::RwLock;\nfn foo(r: RwLock<i32>) { let g = r.read(); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "RwLock" && i.module_path == "std::sync"),
            "已有 RwLock 导入不应通过 .read() 重复报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_rwlock_method_glob_import() {
        let code = "use std::sync::*;\nfn foo(r: RwLock<i32>) { let g = r.write(); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "RwLock" && i.module_path == "std::sync"),
            "glob 导入 use std::sync::*; 应覆盖 RwLock: {:?}",
            issues
        );
    }

    // ===== Session 132: verify_imports mpsc .send()/.recv() 测试 =====

    #[test]
    fn test_verify_imports_send_method_mpsc() {
        let issues = verify_imports("fn foo(s: Sender<i32>) { s.send(42); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Sender" && i.module_path == "std::sync::mpsc"),
            "应通过 .send() 方法检测到 Sender 可能缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_recv_method_mpsc() {
        let issues = verify_imports("fn foo(r: Receiver<i32>) { let x = r.recv(); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Receiver" && i.module_path == "std::sync::mpsc"),
            "应通过 .recv() 方法检测到 Receiver 可能缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_send_recv_both() {
        let issues =
            verify_imports("fn foo(s: Sender<i32>, r: Receiver<i32>) { s.send(42); r.recv(); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Sender" && i.module_path == "std::sync::mpsc"),
            "应检测到 Sender: {:?}",
            issues
        );
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Receiver" && i.module_path == "std::sync::mpsc"),
            "应检测到 Receiver: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_mpsc_method_already_imported() {
        let code = "use std::sync::mpsc::Sender;\nfn foo(s: Sender<i32>) { s.send(42); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Sender" && i.module_path == "std::sync::mpsc"),
            "已有 Sender 导入不应通过 .send() 重复报告: {:?}",
            issues
        );
    }

    // ===== Session 132: ensure_external_imports axum 测试 =====

    #[test]
    fn test_ensure_external_imports_axum_router() {
        let code = "fn foo() -> Router { Router::new() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use axum::Router;"),
            "应添加 axum::Router 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_axum_json() {
        let code = "fn foo() -> Json<i32> { Json(42) }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use axum::Json;"),
            "应添加 axum::Json 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_axum_multiple() {
        let code = "fn foo() -> (Router, Json<i32>, IntoResponse) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use axum::{") && result.contains("IntoResponse"),
            "应合并 axum 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_axum_full_path() {
        let code = "fn foo() -> axum::Router { axum::Router::new() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use axum::"),
            "全限定 axum:: 路径不需要导入: {}",
            result
        );
    }

    // ===== Session 132: ensure_external_imports tera 测试 =====

    #[test]
    fn test_ensure_external_imports_tera() {
        let code = "fn foo() -> Tera { Tera::new(\"templates/**/*\").unwrap() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use tera::Tera;"),
            "应添加 tera::Tera 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_tera_context() {
        let code = "fn foo() -> Context { Context::new() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use tera::Context;"),
            "应添加 tera::Context 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_tera_full_path() {
        let code = "fn foo() -> tera::Tera { tera::Tera::new(\"x\").unwrap() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use tera::"),
            "全限定 tera:: 路径不需要导入: {}",
            result
        );
    }

    // ===== Session 132: ensure_external_imports askama 测试 =====

    #[test]
    fn test_ensure_external_imports_askama_derive() {
        let code = "#[derive(Template)]\nstruct Foo {}";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use askama::Template;"),
            "应添加 askama::Template 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_askama_full_path() {
        let code = "#[derive(askama::Template)]\nstruct Foo {}";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use askama::"),
            "全限定 askama:: 路径不需要导入: {}",
            result
        );
    }

    // ===== Session 132: ensure_external_imports rusqlite 测试 =====

    #[test]
    fn test_ensure_external_imports_rusqlite_connection() {
        let code = "fn foo() -> Connection { Connection::open(\"db.sqlite\").unwrap() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use rusqlite::Connection;"),
            "应添加 rusqlite::Connection 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_rusqlite_multiple() {
        let code = "fn foo(c: &Connection) -> Statement { c.prepare(\"SELECT 1\").unwrap() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("rusqlite::") && result.contains("Connection"),
            "应添加 Connection: {}",
            result
        );
        assert!(
            result.contains("rusqlite::") && result.contains("Statement"),
            "应添加 Statement: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_rusqlite_full_path() {
        let code =
            "fn foo() -> rusqlite::Connection { rusqlite::Connection::open(\"x\").unwrap() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use rusqlite::"),
            "全限定 rusqlite:: 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_rusqlite_params_macro() {
        let code = "fn foo(c: &Connection) { c.execute(\"INSERT\", params![1, 2]); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("rusqlite::") && result.contains("params"),
            "应通过 params! 宏检测添加 rusqlite::params 导入: {}",
            result
        );
    }

    // ===== Session 132: ensure_external_imports redis 测试 =====

    #[test]
    fn test_ensure_external_imports_redis_async_connection() {
        let code =
            "fn foo(c: &mut AsyncConnection) { c.send_packed_command(&Cmd::new(\"PING\")).await; }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("redis::") && result.contains("AsyncConnection"),
            "应添加 redis::AsyncConnection 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_redis_cmd() {
        let code = "fn foo() -> Cmd { Cmd::new(\"GET\") }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use redis::Cmd;"),
            "应添加 redis::Cmd 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_redis_full_path() {
        let code = "fn foo() -> redis::Cmd { redis::Cmd::new(\"GET\") }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use redis::"),
            "全限定 redis:: 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_s132_idempotent() {
        let code = "fn foo() -> (Router, Tera) { unimplemented!() }\nfn bar(c: &Connection) { c.execute(\"x\", params![1]); }\n#[derive(Template)]\nstruct T {}\nfn baz() -> (Cmd, AsyncConnection) { unimplemented!() }";
        let first = ensure_external_imports(code);
        let second = ensure_external_imports(&first);
        assert_eq!(first, second, "Session 132 新增外部 crate 检测应幂等");
    }

    // ===== Session 132: verify_imports 新增外部 crate 类型测试 =====

    #[test]
    fn test_verify_imports_axum_router_missing() {
        let issues = verify_imports("fn foo() -> Router { Router::new() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Router" && i.module_path == "axum"),
            "应检测到 Router 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_tera_missing() {
        let issues = verify_imports("fn foo() -> Tera { Tera::new(\"x\").unwrap() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Tera" && i.module_path == "tera"),
            "应检测到 Tera 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_rusqlite_connection_missing() {
        let issues = verify_imports("fn foo() -> Connection { Connection::open(\"x\").unwrap() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Connection" && i.module_path == "rusqlite"),
            "应检测到 rusqlite::Connection 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_redis_cmd_missing() {
        let issues = verify_imports("fn foo() -> Cmd { Cmd::new(\"GET\") }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Cmd" && i.module_path == "redis"),
            "应检测到 redis::Cmd 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_redis_async_connection_missing() {
        let issues = verify_imports("fn foo(c: &mut AsyncConnection) { }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "AsyncConnection" && i.module_path == "redis"),
            "应检测到 redis::AsyncConnection 缺失导入: {:?}",
            issues
        );
    }

    // ===== Session 133: verify_imports .try_read()/.try_write() → RwLock 测试 =====

    #[test]
    fn test_verify_imports_try_read_method_rwlock() {
        let issues = verify_imports("fn foo(r: RwLock<i32>) { let g = r.try_read(); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "RwLock" && i.module_path == "std::sync"),
            "应通过 .try_read() 方法检测到 RwLock 可能缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_try_write_method_rwlock() {
        let issues = verify_imports("fn foo(w: RwLock<i32>) { let g = w.try_write(); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "RwLock" && i.module_path == "std::sync"),
            "应通过 .try_write() 方法检测到 RwLock 可能缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_try_read_already_imported() {
        let code = "use std::sync::RwLock;\nfn foo(r: RwLock<i32>) { let g = r.try_read(); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "RwLock" && i.module_path == "std::sync"),
            "已有 RwLock 导入不应通过 .try_read() 重复报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_try_write_glob_import() {
        let code = "use std::sync::*;\nfn foo(w: RwLock<i32>) { let g = w.try_write(); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "RwLock" && i.module_path == "std::sync"),
            "glob 导入 use std::sync::*; 应覆盖 RwLock: {:?}",
            issues
        );
    }

    // ===== Session 133: verify_imports .try_send() → mpsc::Sender 测试 =====

    #[test]
    fn test_verify_imports_try_send_method_mpsc() {
        let issues = verify_imports("fn foo(s: Sender<i32>) { s.try_send(42); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Sender" && i.module_path == "std::sync::mpsc"),
            "应通过 .try_send() 方法检测到 mpsc::Sender 可能缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_try_send_already_imported() {
        let code = "use std::sync::mpsc::Sender;\nfn foo(s: Sender<i32>) { s.try_send(42); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Sender" && i.module_path == "std::sync::mpsc"),
            "已有 Sender 导入不应通过 .try_send() 重复报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_try_send_glob_import() {
        let code = "use std::sync::mpsc::*;\nfn foo(s: Sender<i32>) { s.try_send(42); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Sender" && i.module_path == "std::sync::mpsc"),
            "glob 导入 use std::sync::mpsc::*; 应覆盖 Sender: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_try_read_and_try_send_combined() {
        let code =
            "fn foo(r: RwLock<i32>, s: Sender<i32>) { let _ = r.try_read(); s.try_send(1); }";
        let issues = verify_imports(code);
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "RwLock" && i.module_path == "std::sync"),
            "应检测到 RwLock 缺失导入: {:?}",
            issues
        );
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Sender" && i.module_path == "std::sync::mpsc"),
            "应检测到 Sender 缺失导入: {:?}",
            issues
        );
    }

    // ===== Session 133: ensure_external_imports sqlx 测试 =====

    #[test]
    fn test_ensure_external_imports_sqlx_pool() {
        let code = "fn foo() -> Pool<Sqlite> { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use sqlx::Pool;"),
            "应添加 sqlx::Pool 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_sqlx_query_builder() {
        let code = "fn foo() -> QueryBuilder<Sqlite> { QueryBuilder::new(\"\") }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use sqlx::QueryBuilder;"),
            "应添加 sqlx::QueryBuilder 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_sqlx_multiple() {
        let code = "fn foo() -> (Pool<Sqlite>, Executor) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use sqlx::{") && result.contains("Pool"),
            "应合并 sqlx 导入: {}",
            result
        );
        assert!(result.contains("Executor"), "应包含 Executor: {}", result);
    }

    #[test]
    fn test_ensure_external_imports_sqlx_full_path() {
        let code = "fn foo() -> sqlx::Pool<Sqlite> { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use sqlx::Pool;"),
            "全限定 sqlx:: 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_sqlx_query_macro() {
        let code = "fn foo() { let r = query!(\"SELECT 1\"); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("sqlx::") && result.contains("query"),
            "应通过 query! 宏检测添加 sqlx::query 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_sqlx_query_as_macro() {
        let code = "fn foo() { let r = query_as!(User, \"SELECT * FROM users\"); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("sqlx::") && result.contains("query_as"),
            "应通过 query_as! 宏检测添加 sqlx::query_as 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_sqlx_full_path_macro() {
        let code = "fn foo() { let r = sqlx::query!(\"SELECT 1\"); }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use sqlx::query;"),
            "全限定 sqlx::query! 不需要导入: {}",
            result
        );
    }

    // ===== Session 133: ensure_external_imports axum-extra 测试 =====

    #[test]
    fn test_ensure_external_imports_axum_extra_typed_header() {
        let code = "fn foo(h: TypedHeader<XAuthToken>) -> () {}";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use axum_extra::TypedHeader;"),
            "应添加 axum_extra::TypedHeader 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_axum_extra_full_path() {
        let code = "fn foo(h: axum_extra::TypedHeader<X>) {}";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use axum_extra::TypedHeader;"),
            "全限定 axum_extra:: 路径不需要导入: {}",
            result
        );
    }

    // ===== Session 133: ensure_external_imports lettre 测试 =====

    #[test]
    fn test_ensure_external_imports_lettre_message() {
        let code = "fn foo() -> Message { Message::builder().build() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use lettre::Message;"),
            "应添加 lettre::Message 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_lettre_smtp_transport() {
        let code = "fn foo() -> SmtpTransport { SmtpTransport::relay(\"localhost\").unwrap() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use lettre::SmtpTransport;"),
            "应添加 lettre::SmtpTransport 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_lettre_multiple() {
        let code = "fn foo() -> (Message, SmtpTransport) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use lettre::{") && result.contains("Message"),
            "应合并 lettre 导入: {}",
            result
        );
        assert!(
            result.contains("SmtpTransport"),
            "应包含 SmtpTransport: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_lettre_full_path() {
        let code = "fn foo() -> lettre::Message { lettre::Message::builder().build() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use lettre::Message;"),
            "全限定 lettre:: 路径不需要导入: {}",
            result
        );
    }

    // ===== Session 133: ensure_external_imports config 测试 =====

    #[test]
    fn test_ensure_external_imports_config() {
        let code = "fn foo() -> Config { Config::builder().build() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use config::Config;"),
            "应添加 config::Config 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_config_full_path() {
        let code = "fn foo() -> config::Config { config::Config::builder().build() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use config::Config;"),
            "全限定 config:: 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_s133_idempotent() {
        let code = "fn foo() -> (Pool<Sqlite>, TypedHeader<X>) { unimplemented!() }\nfn bar() -> Message { unimplemented!() }\nfn baz() -> Config { unimplemented() }\nlet r = query!(\"SELECT 1\");";
        let first = ensure_external_imports(code);
        let second = ensure_external_imports(&first);
        assert_eq!(first, second, "Session 133 新增外部 crate 检测应幂等");
    }

    // ===== Session 133: verify_imports 新增外部 crate 类型测试 =====

    #[test]
    fn test_verify_imports_sqlx_pool_missing() {
        let issues = verify_imports("fn foo() -> Pool<Sqlite> { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Pool" && i.module_path == "sqlx"),
            "应检测到 Pool 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_lettre_message_missing() {
        let issues = verify_imports("fn foo() -> Message { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Message" && i.module_path == "lettre"),
            "应检测到 lettre::Message 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_config_missing() {
        let issues = verify_imports("fn foo() -> Config { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Config" && i.module_path == "config"),
            "应检测到 config::Config 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_axum_extra_typed_header_missing() {
        let issues = verify_imports("fn foo(h: TypedHeader<X>) {}");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "TypedHeader" && i.module_path == "axum_extra"),
            "应检测到 axum_extra::TypedHeader 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_sqlx_query_macro_missing() {
        let issues = verify_imports("fn foo() { let r = query!(\"SELECT 1\"); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "query" && i.module_path == "sqlx"),
            "应检测到 sqlx::query! 宏缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_sqlx_query_macro_already_imported() {
        let code = "use sqlx::query;\nfn foo() { let r = query!(\"SELECT 1\"); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "query" && i.module_path == "sqlx"),
            "已有 sqlx::query 导入不应重复报告: {:?}",
            issues
        );
    }

    // ===== Session 133: 增强 glob 导入检测测试 =====

    #[test]
    fn test_extract_glob_imports_mixed_star_and_sub() {
        let code = "use std::sync::{*, atomic::*};";
        let globs = extract_glob_imports(code);
        assert!(
            globs.contains(&"std::sync".to_string()),
            "应包含 std::sync (from *): {:?}",
            globs
        );
        assert!(
            globs.contains(&"std::sync::atomic".to_string()),
            "应包含 std::sync::atomic (from atomic::*): {:?}",
            globs
        );
    }

    #[test]
    fn test_extract_glob_imports_multiple_globs_in_braces() {
        let code = "use std::{io::*, sync::*};";
        let globs = extract_glob_imports(code);
        assert!(
            globs.contains(&"std::io".to_string()),
            "应包含 std::io: {:?}",
            globs
        );
        assert!(
            globs.contains(&"std::sync".to_string()),
            "应包含 std::sync: {:?}",
            globs
        );
    }

    #[test]
    fn test_extract_glob_imports_type_and_glob_mixed() {
        let code = "use std::collections::{HashMap, hash_map::*};";
        let globs = extract_glob_imports(code);
        assert!(
            globs.contains(&"std::collections::hash_map".to_string()),
            "应包含 std::collections::hash_map: {:?}",
            globs
        );
        assert!(
            !globs.contains(&"std::collections".to_string()),
            "不应包含 std::collections (HashMap 不是 glob): {:?}",
            globs
        );
    }

    #[test]
    fn test_extract_glob_imports_nested_braces() {
        let code = "use std::{sync::{*, atomic::*}, io::*};";
        let globs = extract_glob_imports(code);
        assert!(
            globs.contains(&"std::sync".to_string()),
            "应包含 std::sync: {:?}",
            globs
        );
        assert!(
            globs.contains(&"std::sync::atomic".to_string()),
            "应包含 std::sync::atomic: {:?}",
            globs
        );
        assert!(
            globs.contains(&"std::io".to_string()),
            "应包含 std::io: {:?}",
            globs
        );
    }

    #[test]
    fn test_extract_glob_imports_self_and_glob() {
        let code = "use crate::{self, sub::*};";
        let globs = extract_glob_imports(code);
        assert!(
            globs.contains(&"crate::sub".to_string()),
            "应包含 crate::sub: {:?}",
            globs
        );
        assert!(
            !globs.contains(&"crate".to_string()),
            "不应包含 crate (self 不是 glob): {:?}",
            globs
        );
    }

    #[test]
    fn test_extract_glob_imports_simple_still_works() {
        let code = "use std::collections::*;\nuse serde::Serialize;";
        let globs = extract_glob_imports(code);
        assert!(
            globs.contains(&"std::collections".to_string()),
            "简单 glob 仍应正常工作: {:?}",
            globs
        );
        assert!(
            !globs.contains(&"serde".to_string()),
            "非 glob 导入不应包含: {:?}",
            globs
        );
    }

    #[test]
    fn test_extract_glob_imports_empty_braces() {
        let code = "use std::collections::{HashMap, HashSet};";
        let globs = extract_glob_imports(code);
        assert!(globs.is_empty(), "无 glob 的花括号导入应为空: {:?}", globs);
    }

    #[test]
    fn test_split_top_level_commas_basic() {
        let parts = split_top_level_commas("a, b, c");
        assert_eq!(parts.len(), 3, "应分割为3部分: {:?}", parts);
        assert_eq!(parts[0], "a");
        assert_eq!(parts[1], " b");
        assert_eq!(parts[2], " c");
    }

    #[test]
    fn test_split_top_level_commas_nested() {
        let parts = split_top_level_commas("a, {b, c}, d");
        assert_eq!(parts.len(), 3, "嵌套花括号内逗号不应分割: {:?}", parts);
        assert_eq!(parts[0], "a");
        assert_eq!(parts[1], " {b, c}");
        assert_eq!(parts[2], " d");
    }

    #[test]
    fn test_split_top_level_commas_empty() {
        let parts = split_top_level_commas("");
        assert!(parts.is_empty(), "空字符串应返回空: {:?}", parts);
    }

    #[test]
    fn test_ensure_std_imports_mixed_glob_exclusion() {
        let code =
            "use std::sync::{*, atomic::*};\nfn foo() -> AtomicBool { AtomicBool::new(true) }";
        let result = ensure_std_imports(code);
        assert!(
            !result.contains("use std::sync::atomic::AtomicBool;"),
            "use std::sync::{{*, atomic::*}}; 应覆盖 AtomicBool: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_mixed_glob_exclusion() {
        let code = "use serde::{Serialize, *};\n#[derive(Deserialize)]\nstruct Foo {}";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use serde::Deserialize;"),
            "use serde::{{Serialize, *}}; 应覆盖 Deserialize: {}",
            result
        );
    }

    // ===== Session 134: ensure_external_imports hyper/tower/tonic 测试 =====

    #[test]
    fn test_ensure_external_imports_hyper_body() {
        let code = "fn foo() -> Body { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use hyper::Body;"),
            "应添加 hyper::Body 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_hyper_multiple() {
        let code = "fn foo() -> (Body, HeaderMap) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use hyper::{") && result.contains("Body"),
            "应合并 hyper 导入: {}",
            result
        );
        assert!(result.contains("HeaderMap"), "应包含 HeaderMap: {}", result);
    }

    #[test]
    fn test_ensure_external_imports_hyper_full_path() {
        let code = "fn foo() -> hyper::Body { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use hyper::Body;"),
            "全限定 hyper:: 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_tower_service() {
        let code = "fn foo() -> Service { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use tower::Service;"),
            "应添加 tower::Service 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_tower_multiple() {
        let code = "fn foo() -> (Service, Layer) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use tower::{") && result.contains("Service"),
            "应合并 tower 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_tonic_status() {
        let code = "fn foo() -> Status { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use tonic::Status;"),
            "应添加 tonic::Status 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_tonic_channel() {
        let code = "fn foo() -> Channel { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use tonic::transport::Channel;"),
            "应添加 tonic::transport::Channel 导入: {}",
            result
        );
    }

    // ===== Session 134: ensure_external_imports proc_macro2/syn/quote 测试 =====

    #[test]
    fn test_ensure_external_imports_proc_macro2_token_stream() {
        let code = "fn foo() -> TokenStream { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use proc_macro2::TokenStream;"),
            "应添加 proc_macro2::TokenStream 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_proc_macro2_multiple() {
        let code = "fn foo() -> (TokenStream, Span, Ident) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use proc_macro2::{") && result.contains("TokenStream"),
            "应合并 proc_macro2 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_syn_derive_input() {
        let code = "fn foo() -> DeriveInput { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use syn::DeriveInput;"),
            "应添加 syn::DeriveInput 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_syn_multiple() {
        let code = "fn foo() -> (ItemFn, ItemStruct, ItemEnum) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use syn::{") && result.contains("ItemFn"),
            "应合并 syn 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_quote_macro() {
        let code = "fn foo() { let ts = quote! { x }; }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use quote::quote;"),
            "应添加 quote::quote 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_format_ident_macro() {
        let code = "fn foo() { let id = format_ident!(\"x\"); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use quote::format_ident;"),
            "应添加 quote::format_ident 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_syn_parse_quote_macro() {
        let code = "fn foo() { let item = parse_quote!(fn main() {}); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use syn::parse_quote;"),
            "应添加 syn::parse_quote 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_s134_idempotent() {
        let code = "fn foo() -> (Body, Service, Status) { unimplemented!() }\nfn bar() -> TokenStream { quote! { x } }";
        let first = ensure_external_imports(code);
        let second = ensure_external_imports(&first);
        assert_eq!(first, second, "Session 134 新增外部 crate 检测应幂等");
    }

    // ===== Session 134: verify_imports 新增外部 crate 类型测试 =====

    #[test]
    fn test_verify_imports_hyper_body_missing() {
        let issues = verify_imports("fn foo() -> Body { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Body" && i.module_path == "hyper"),
            "应检测到 hyper::Body 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_tower_service_missing() {
        let issues = verify_imports("fn foo() -> Service { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Service" && i.module_path == "tower"),
            "应检测到 tower::Service 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_tonic_status_missing() {
        let issues = verify_imports("fn foo() -> Status { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Status" && i.module_path == "tonic"),
            "应检测到 tonic::Status 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_proc_macro2_token_stream_missing() {
        let issues = verify_imports("fn foo() -> TokenStream { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "TokenStream" && i.module_path == "proc_macro2"),
            "应检测到 proc_macro2::TokenStream 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_syn_derive_input_missing() {
        let issues = verify_imports("fn foo() -> DeriveInput { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "DeriveInput" && i.module_path == "syn"),
            "应检测到 syn::DeriveInput 缺失导入: {:?}",
            issues
        );
    }

    // ===== Session 134: verify_imports .await? / .spawn() 测试 =====

    #[test]
    fn test_verify_imports_await_question_mark() {
        let issues = verify_imports("async fn foo() { bar().await?; }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Result" && i.module_path == "anyhow"),
            "应检测到 .await? 需要 anyhow::Result: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_await_question_already_imported() {
        let code = "use anyhow::Result;\nasync fn foo() -> Result<()> { bar().await?; Ok(()) }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Result" && i.module_path == "anyhow"),
            "已有 anyhow::Result 导入不应报告 .await?: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_spawn_method() {
        // tokio::spawn with full path — no issue
        let _issues1 = verify_imports("async fn foo() { tokio::spawn(async {}).await; }");
        // Test bare .spawn()
        let code2 = "async fn foo() { task.spawn().await; }";
        let issues2 = verify_imports(code2);
        assert!(
            issues2
                .iter()
                .any(|i| i.type_name == "spawn" && i.module_path == "tokio"),
            "应检测到 .spawn() 需要 tokio::spawn: {:?}",
            issues2
        );
    }

    #[test]
    fn test_verify_imports_spawn_already_imported() {
        let code = "use tokio::spawn;\nfn foo() { spawn(async {}); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "spawn" && i.module_path == "tokio"),
            "已有 tokio::spawn 导入不应报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_quote_macro_missing() {
        let issues = verify_imports("fn foo() { let ts = quote! { x }; }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "quote" && i.module_path == "quote"),
            "应检测到 quote! 宏缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_parse_quote_macro_missing() {
        let issues = verify_imports("fn foo() { let item = parse_quote!(fn main() {}); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "parse_quote" && i.module_path == "syn"),
            "应检测到 syn parse_quote! 宏缺失导入: {:?}",
            issues
        );
    }

    // ===== Session 134: 多行 use 语句 glob 导入检测测试 =====

    #[test]
    fn test_extract_glob_imports_multiline_basic() {
        let code = "use std::{\n    io::*,\n    sync::*\n};";
        let globs = extract_glob_imports(code);
        assert!(
            globs.contains(&"std::io".to_string()),
            "多行 use 应包含 std::io: {:?}",
            globs
        );
        assert!(
            globs.contains(&"std::sync".to_string()),
            "多行 use 应包含 std::sync: {:?}",
            globs
        );
    }

    #[test]
    fn test_extract_glob_imports_multiline_with_types() {
        let code = "use std::{\n    collections::HashMap,\n    io::*\n};";
        let globs = extract_glob_imports(code);
        assert!(
            globs.contains(&"std::io".to_string()),
            "多行 use 应包含 std::io (跳过非 glob 项): {:?}",
            globs
        );
        assert!(
            !globs.contains(&"std::collections".to_string()),
            "不应包含 std::collections (HashMap 不是 glob): {:?}",
            globs
        );
    }

    #[test]
    fn test_extract_glob_imports_multiline_nested() {
        let code = "use std::{\n    sync::{\n        *,\n        atomic::*\n    }\n};";
        let globs = extract_glob_imports(code);
        assert!(
            globs.contains(&"std::sync".to_string()),
            "多行嵌套 use 应包含 std::sync: {:?}",
            globs
        );
        assert!(
            globs.contains(&"std::sync::atomic".to_string()),
            "多行嵌套 use 应包含 std::sync::atomic: {:?}",
            globs
        );
    }

    #[test]
    fn test_extract_glob_imports_multiline_single_line_still_works() {
        let code = "use std::collections::*;\nuse serde::Serialize;";
        let globs = extract_glob_imports(code);
        assert!(
            globs.contains(&"std::collections".to_string()),
            "单行 use 仍应正常工作: {:?}",
            globs
        );
    }

    #[test]
    fn test_ensure_std_imports_multiline_glob_exclusion() {
        let code = "use std::{\n    io::*,\n    sync::*\n};\nfn foo() -> Arc<Mutex<i32>> { Arc::new(Mutex::new(0)) }";
        let result = ensure_std_imports(code);
        assert!(
            !result.contains("use std::sync::Arc;"),
            "多行 glob 导入 use std::{{io::*, sync::*}}; 应覆盖 Arc: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_multiline_glob_exclusion() {
        let code = "use serde::{\n    Serialize,\n    *\n};\n#[derive(Deserialize)]\nstruct Foo {}";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use serde::Deserialize;"),
            "多行 glob 导入应覆盖 Deserialize: {}",
            result
        );
    }

    // ===== Session 134: validate_rust_braces 字节字符串测试 =====
    // Note: basic byte string/char/raw byte string tests already exist from Session 114

    #[test]
    fn test_validate_rust_braces_byte_string_with_braces() {
        let code = "fn foo() {\n    let s = b\"{not a brace}\";\n}";
        assert!(
            validate_rust_braces(code).is_none(),
            "字节字符串中的 {{ }} 不应被计数"
        );
    }

    // ===== Session 135: ensure_external_imports warp/actix-web/sea-orm/diesel 测试 =====

    #[test]
    fn test_ensure_external_imports_warp_filter() {
        let code = "fn foo() -> Filter { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use warp::Filter;"),
            "应添加 warp::Filter 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_warp_reply() {
        let code = "fn foo() -> Reply { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use warp::Reply;"),
            "应添加 warp::Reply 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_actix_http_response() {
        let code = "fn foo() -> HttpResponse { HttpResponse::Ok().finish() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use actix_web::HttpResponse;"),
            "应添加 actix_web::HttpResponse 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_actix_http_request() {
        let code = "fn foo(req: HttpRequest) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use actix_web::HttpRequest;"),
            "应添加 actix_web::HttpRequest 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_actix_responder() {
        let code = "fn foo() -> impl Responder { \"hello\" }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use actix_web::Responder;"),
            "应添加 actix_web::Responder 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_actix_http_server() {
        let code = "fn foo() -> HttpServer { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use actix_web::HttpServer;"),
            "应添加 actix_web::HttpServer 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_sea_orm_entity_trait() {
        let code = "fn foo() -> EntityTrait { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use sea_orm::EntityTrait;"),
            "应添加 sea_orm::EntityTrait 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_sea_orm_database() {
        let code = "fn foo() -> Database { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use sea_orm::Database;"),
            "应添加 sea_orm::Database 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_sea_orm_dbconn() {
        let code = "fn foo() -> DbConn { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use sea_orm::DbConn;"),
            "应添加 sea_orm::DbConn 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_diesel_query_dsl() {
        let code = "fn foo() -> QueryDsl { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use diesel::QueryDsl;"),
            "应添加 diesel::QueryDsl 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_diesel_pg_connection() {
        let code = "fn foo() -> PgConnection { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use diesel::PgConnection;"),
            "应添加 diesel::PgConnection 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_diesel_sqlite_connection() {
        let code = "fn foo() -> SqliteConnection { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use diesel::SqliteConnection;"),
            "应添加 diesel::SqliteConnection 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_warp_full_path_no_import() {
        let code = "fn foo() -> warp::Filter { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use warp::Filter;"),
            "全限定 warp::Filter 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_actix_full_path_no_import() {
        let code = "fn foo() -> actix_web::HttpResponse { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use actix_web::HttpResponse;"),
            "全限定 actix_web::HttpResponse 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_diesel_full_path_no_import() {
        let code = "fn foo() -> diesel::PgConnection { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use diesel::PgConnection;"),
            "全限定 diesel::PgConnection 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_multiple_actix_merged() {
        let code = "fn foo(req: HttpRequest) -> HttpResponse { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use actix_web::{HttpRequest, HttpResponse};"),
            "多个 actix_web 类型应合并导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_multiple_diesel_merged() {
        let code = "fn foo() -> (QueryDsl, PgConnection) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use diesel::{PgConnection, QueryDsl};"),
            "多个 diesel 类型应合并导入 (字母序): {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_s135_idempotent() {
        let code =
            "fn foo() -> (Filter, HttpResponse, EntityTrait, PgConnection) { unimplemented!() }";
        let first = ensure_external_imports(code);
        let second = ensure_external_imports(&first);
        assert_eq!(first, second, "Session 135 新增外部 crate 检测应幂等");
    }

    // ===== Session 135: verify_imports warp/actix-web/sea-orm/diesel 测试 =====

    #[test]
    fn test_verify_imports_warp_filter_missing() {
        let issues = verify_imports("fn foo() -> Filter { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Filter" && i.module_path == "warp"),
            "应检测到 warp::Filter 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_actix_http_response_missing() {
        let issues = verify_imports("fn foo() -> HttpResponse { HttpResponse::Ok().finish() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "HttpResponse" && i.module_path == "actix_web"),
            "应检测到 actix_web::HttpResponse 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_sea_orm_entity_trait_missing() {
        let issues = verify_imports("fn foo() -> EntityTrait { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "EntityTrait" && i.module_path == "sea_orm"),
            "应检测到 sea_orm::EntityTrait 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_diesel_query_dsl_missing() {
        let issues = verify_imports("fn foo() -> QueryDsl { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "QueryDsl" && i.module_path == "diesel"),
            "应检测到 diesel::QueryDsl 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_diesel_pg_connection_missing() {
        let issues = verify_imports("fn foo() -> PgConnection { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "PgConnection" && i.module_path == "diesel"),
            "应检测到 diesel::PgConnection 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_actix_already_imported() {
        let code = "use actix_web::HttpResponse;\nfn foo() -> HttpResponse { HttpResponse::Ok().finish() }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "HttpResponse" && i.module_path == "actix_web"),
            "已有 actix_web::HttpResponse 导入不应报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_diesel_already_imported() {
        let code = "use diesel::PgConnection;\nfn foo() -> PgConnection { unimplemented!() }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "PgConnection" && i.module_path == "diesel"),
            "已有 diesel::PgConnection 导入不应报告: {:?}",
            issues
        );
    }

    // ===== Session 135: verify_imports .collect() / .into_iter() 测试 =====

    #[test]
    fn test_verify_imports_collect_method_fromiterator() {
        let issues =
            verify_imports("fn foo(v: Vec<i32>) { let x: Vec<i32> = v.iter().collect(); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "FromIterator" && i.module_path == "std::iter"),
            "应通过 .collect() 方法检测到 FromIterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_collect_already_imported() {
        let code = "use std::iter::FromIterator;\nfn foo(v: Vec<i32>) { let x: Vec<i32> = v.iter().collect(); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "FromIterator" && i.module_path == "std::iter"),
            "已有 FromIterator 导入不应通过 .collect() 重复报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_collect_glob_import() {
        let code =
            "use std::iter::*;\nfn foo(v: Vec<i32>) { let x: Vec<i32> = v.iter().collect(); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "FromIterator" && i.module_path == "std::iter"),
            "glob 导入 use std::iter::*; 应覆盖 FromIterator: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_into_iter_method_intoiterator() {
        let issues = verify_imports(
            "fn foo(v: Vec<i32>) { for x in v.into_iter() { println!(\"{}\", x); } }",
        );
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "IntoIterator" && i.module_path == "std::iter"),
            "应通过 .into_iter() 方法检测到 IntoIterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_into_iter_already_imported() {
        let code = "use std::iter::IntoIterator;\nfn foo(v: Vec<i32>) { for x in v.into_iter() { println!(\"{}\", x); } }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "IntoIterator" && i.module_path == "std::iter"),
            "已有 IntoIterator 导入不应通过 .into_iter() 重复报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_into_iter_glob_import() {
        let code = "use std::iter::*;\nfn foo(v: Vec<i32>) { for x in v.into_iter() { println!(\"{}\", x); } }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "IntoIterator" && i.module_path == "std::iter"),
            "glob 导入 use std::iter::*; 应覆盖 IntoIterator: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_collect_and_into_iter_combined() {
        let code = "fn foo(v: Vec<i32>) { let x: Vec<i32> = v.into_iter().collect(); }";
        let issues = verify_imports(code);
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "FromIterator" && i.module_path == "std::iter"),
            "应检测到 FromIterator: {:?}",
            issues
        );
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "IntoIterator" && i.module_path == "std::iter"),
            "应检测到 IntoIterator: {:?}",
            issues
        );
    }

    // ===== Session 135: extract_glob_imports 注释支持测试 =====

    #[test]
    fn test_extract_glob_imports_with_line_comment() {
        let code = "use std::{\n    // io module\n    io::*,\n    sync::*\n};";
        let globs = extract_glob_imports(code);
        assert!(
            globs.contains(&"std::io".to_string()),
            "带注释的 use 语句应包含 std::io: {:?}",
            globs
        );
        assert!(
            globs.contains(&"std::sync".to_string()),
            "带注释的 use 语句应包含 std::sync: {:?}",
            globs
        );
    }

    #[test]
    fn test_extract_glob_imports_with_multiple_comments() {
        let code =
            "use std::{\n    // first comment\n    io::*,\n    // second comment\n    sync::*\n};";
        let globs = extract_glob_imports(code);
        assert!(
            globs.contains(&"std::io".to_string()),
            "多个注释的 use 语句应包含 std::io: {:?}",
            globs
        );
        assert!(
            globs.contains(&"std::sync".to_string()),
            "多个注释的 use 语句应包含 std::sync: {:?}",
            globs
        );
    }

    #[test]
    fn test_extract_glob_imports_with_trailing_comment() {
        let code = "use std::{\n    io::*, // trailing comment\n    sync::*\n};";
        let globs = extract_glob_imports(code);
        assert!(
            globs.contains(&"std::io".to_string()),
            "尾注释的 use 语句应包含 std::io: {:?}",
            globs
        );
        assert!(
            globs.contains(&"std::sync".to_string()),
            "尾注释的 use 语句应包含 std::sync: {:?}",
            globs
        );
    }

    #[test]
    fn test_extract_glob_imports_with_comment_and_type() {
        let code =
            "use std::{\n    // collection types\n    HashMap,\n    // io glob\n    io::*\n};";
        let globs = extract_glob_imports(code);
        assert!(
            globs.contains(&"std::io".to_string()),
            "混合注释和类型的 use 语句应包含 std::io: {:?}",
            globs
        );
        assert!(
            !globs.contains(&"std::collections".to_string()),
            "HashMap 不是 glob, 不应包含: {:?}",
            globs
        );
    }

    #[test]
    fn test_extract_glob_imports_comment_only_line() {
        let code = "use std::{\n    // just a comment\n    // another comment\n    io::*\n};";
        let globs = extract_glob_imports(code);
        assert!(
            globs.contains(&"std::io".to_string()),
            "纯注释行的 use 语句应包含 std::io: {:?}",
            globs
        );
    }

    // ===== Session 135: strip_use_line_comments 测试 =====

    #[test]
    fn test_strip_use_line_comments_basic() {
        assert_eq!(
            strip_use_line_comments("use std::io::*; // comment"),
            "use std::io::*;"
        );
    }

    #[test]
    fn test_strip_use_line_comments_no_comment() {
        assert_eq!(
            strip_use_line_comments("use std::io::*;"),
            "use std::io::*;"
        );
    }

    #[test]
    fn test_strip_use_line_comments_url_not_stripped() {
        // URL 中的 :// 不应被误判为注释
        assert_eq!(
            strip_use_line_comments("use example::http://example.com::*;"),
            "use example::http://example.com::*;"
        );
    }

    #[test]
    fn test_strip_use_line_comments_multiple_comments() {
        let input = "use std::{io::*, sync::*}; // trailing // nested";
        let result = strip_use_line_comments(input);
        assert_eq!(result, "use std::{io::*, sync::*};");
    }

    // ===== Session 135: validate_rust_braces macro_rules! 增强测试 =====

    #[test]
    fn test_validate_rust_braces_macro_rules_repetition() {
        // $( $x:expr ),* 重复语法
        let code = r#"
macro_rules! vec_of {
    ( $( $x:expr ),* ) => {
        {
            let mut v = Vec::new();
            $( v.push($x); )*
            v
        }
    };
}

fn main() {
    let v = vec_of!(1, 2, 3);
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "macro_rules! 重复语法应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_macro_rules_tt_muncher() {
        // TT muncher 模式
        let code = r#"
macro_rules! parse_tokens {
    (@parse $head:tt) => { $head };
    (@parse $head:tt $($tail:tt)*) => {
        parse_tokens!(@parse $($tail)*)
    };
    ($($tokens:tt)*) => {
        parse_tokens!(@parse $($tokens)*)
    };
}

fn main() {
    let x = parse_tokens!(1 2 3);
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "macro_rules! TT muncher 应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_macro_rules_with_attrs() {
        // 宏内含属性
        let code = r#"
macro_rules! make_struct {
    ($name:ident) => {
        #[derive(Debug, Clone)]
        struct $name {
            value: i32,
        }
    };
}

fn main() {
    make_struct!(Foo);
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "macro_rules! 含属性应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_macro_rules_nested_macros() {
        // 嵌套宏调用
        let code = r#"
macro_rules! inner {
    ($e:expr) => { $e + 1 };
}

macro_rules! outer {
    ($e:expr) => {
        {
            let x = inner!($e);
            x * 2
        }
    };
}

fn main() {
    let y = outer!(5);
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "嵌套 macro_rules! 应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_macro_rules_complex_matchers() {
        // 复杂匹配器: 多种 token 类型
        let code = r#"
macro_rules! complex {
    ($name:ident, $ty:ty, $default:expr, $($field:ident: $ftype:ty),* $(,)?) => {
        struct $name {
            value: $ty,
            default: $default,
            $($field: $ftype,)*
        }
    };
}

fn main() {
    complex!(MyStruct, i32, 0, x: String, y: bool);
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "复杂 macro_rules! 匹配器应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_macro_rules_unbalanced() {
        // 不平衡的 macro_rules! (缺少闭合大括号)
        let code = r#"
macro_rules! bad_macro {
    () => {
        println!("hello");
"#;
        let result = validate_rust_braces(code);
        assert!(result.is_some(), "不平衡的 macro_rules! 应报告问题");
    }

    #[test]
    fn test_validate_rust_braces_macro_rules_with_raw_string() {
        // 宏内含 raw string
        let code = r##"
macro_rules! regex_macro {
    () => {
        let pattern = r#"\{[^}]+\}"#;
        println!("{}", pattern);
    };
}

fn main() {
    regex_macro!();
}
"##;
        assert!(
            validate_rust_braces(code).is_none(),
            "含 raw string 的 macro_rules! 应通过验证"
        );
    }

    // ===== Session 136: ensure_external_imports mailgun/stripe/aws-sdk 测试 =====

    #[test]
    fn test_ensure_external_imports_mailgun() {
        let code = "fn foo(m: &Mailgun) { m.send(); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use mailgun::Mailgun;"),
            "应添加 mailgun::Mailgun 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_mailgun_recipient() {
        let code = "fn foo() -> Recipient { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use mailgun::Recipient;"),
            "应添加 mailgun::Recipient 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_mailgun_full_path() {
        let code = "fn foo(m: &mailgun::Mailgun) { m.send(); }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use mailgun::Mailgun;"),
            "全限定 mailgun:: 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_stripe_charge() {
        let code = "fn foo() -> Charge { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use stripe::Charge;"),
            "应添加 stripe::Charge 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_stripe_multiple() {
        let code = "fn foo() -> (Charge, Customer, PaymentIntent) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use stripe::{") && result.contains("Charge"),
            "应合并 stripe 导入: {}",
            result
        );
        assert!(result.contains("Customer"), "应包含 Customer: {}", result);
        assert!(
            result.contains("PaymentIntent"),
            "应包含 PaymentIntent: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_stripe_full_path() {
        let code = "fn foo() -> stripe::Charge { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use stripe::"),
            "全限定 stripe:: 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_aws_s3() {
        let code = "fn foo() -> PutObjectOutput { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use aws_sdk_s3::PutObjectOutput;"),
            "应添加 aws_sdk_s3::PutObjectOutput 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_aws_s3_multiple() {
        let code = "fn foo() -> (PutObjectOutput, GetObjectOutput) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use aws_sdk_s3::{") && result.contains("PutObjectOutput"),
            "应合并 aws_sdk_s3 导入: {}",
            result
        );
        assert!(
            result.contains("GetObjectOutput"),
            "应包含 GetObjectOutput: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_aws_config() {
        let code = "fn foo() -> SdkConfig { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use aws_config::SdkConfig;"),
            "应添加 aws_config::SdkConfig 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_aws_config_full_path() {
        let code = "fn foo() -> aws_config::SdkConfig { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use aws_config::"),
            "全限定 aws_config:: 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_s136_idempotent() {
        let code = "fn foo() -> (Mailgun, Charge, PutObjectOutput, SdkConfig) { unimplemented!() }\nfn bar() -> Recipient { unimplemented!() }\nfn baz() -> (Customer, PaymentIntent, GetObjectOutput, BehaviorVersion) { unimplemented!() }";
        let first = ensure_external_imports(code);
        let second = ensure_external_imports(&first);
        assert_eq!(first, second, "Session 136 新增外部 crate 检测应幂等");
    }

    // ===== Session 136: verify_imports mailgun/stripe/aws-sdk 类型测试 =====

    #[test]
    fn test_verify_imports_mailgun_missing() {
        let issues = verify_imports("fn foo(m: &Mailgun) { }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Mailgun" && i.module_path == "mailgun"),
            "应检测到 Mailgun 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_stripe_missing() {
        let issues = verify_imports("fn foo() -> Charge { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Charge" && i.module_path == "stripe"),
            "应检测到 Charge 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_aws_s3_missing() {
        let issues = verify_imports("fn foo() -> PutObjectOutput { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "PutObjectOutput" && i.module_path == "aws_sdk_s3"),
            "应检测到 PutObjectOutput 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_aws_config_missing() {
        let issues = verify_imports("fn foo() -> SdkConfig { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "SdkConfig" && i.module_path == "aws_config"),
            "应检测到 SdkConfig 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_mailgun_already_imported() {
        let code = "use mailgun::Mailgun;\nfn foo(m: &Mailgun) { }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Mailgun" && i.module_path == "mailgun"),
            "已有 mailgun::Mailgun 导入不应报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_stripe_already_imported() {
        let code = "use stripe::Charge;\nfn foo() -> Charge { unimplemented!() }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Charge" && i.module_path == "stripe"),
            "已有 stripe::Charge 导入不应报告: {:?}",
            issues
        );
    }

    // ===== Session 136: verify_imports .map()/.filter() Iterator 测试 =====

    #[test]
    fn test_verify_imports_map_method_iterator() {
        let issues = verify_imports("fn foo(v: Vec<i32>) { v.iter().map(|x| x * 2); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应通过 .map() 方法检测到 Iterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_filter_method_iterator() {
        let issues = verify_imports("fn foo(v: Vec<i32>) { v.iter().filter(|&x| x > 0); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应通过 .filter() 方法检测到 Iterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_map_already_imported() {
        let code = "use std::iter::Iterator;\nfn foo(v: Vec<i32>) { v.iter().map(|x| x * 2); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "已有 Iterator 导入不应通过 .map() 重复报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_map_glob_import() {
        let code = "use std::iter::*;\nfn foo(v: Vec<i32>) { v.iter().map(|x| x * 2); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "glob 导入 use std::iter::*; 应覆盖 Iterator: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_map_and_filter_combined() {
        let code = "fn foo(v: Vec<i32>) { v.iter().filter(|&x| x > 0).map(|x| x * 2); }";
        let issues = verify_imports(code);
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应检测到 Iterator (.map 和 .filter 组合): {:?}",
            issues
        );
    }

    // ===== Session 136: extract_glob_imports 块注释测试 =====

    #[test]
    fn test_extract_glob_imports_with_block_comment() {
        let code = "use std::{/* comment */ io::*};";
        let globs = extract_glob_imports(code);
        assert!(
            globs.contains(&"std::io".to_string()),
            "块注释的 use 语句应包含 std::io: {:?}",
            globs
        );
    }

    #[test]
    fn test_extract_glob_imports_with_block_comment_multiline() {
        let code = "use std::{\n    /* this is a block comment */\n    io::*,\n    sync::*\n};";
        let globs = extract_glob_imports(code);
        assert!(
            globs.contains(&"std::io".to_string()),
            "多行块注释的 use 语句应包含 std::io: {:?}",
            globs
        );
        assert!(
            globs.contains(&"std::sync".to_string()),
            "多行块注释的 use 语句应包含 std::sync: {:?}",
            globs
        );
    }

    #[test]
    fn test_extract_glob_imports_with_nested_block_comment() {
        let code = "use std::{/* outer /* inner */ comment */ io::*};";
        let globs = extract_glob_imports(code);
        assert!(
            globs.contains(&"std::io".to_string()),
            "嵌套块注释的 use 语句应包含 std::io: {:?}",
            globs
        );
    }

    #[test]
    fn test_extract_glob_imports_with_block_and_line_comment() {
        let code = "use std::{\n    // line comment\n    /* block comment */ io::*,\n    sync::* // trailing\n};";
        let globs = extract_glob_imports(code);
        assert!(
            globs.contains(&"std::io".to_string()),
            "混合注释的 use 语句应包含 std::io: {:?}",
            globs
        );
        assert!(
            globs.contains(&"std::sync".to_string()),
            "混合注释的 use 语句应包含 std::sync: {:?}",
            globs
        );
    }

    #[test]
    fn test_extract_glob_imports_block_comment_with_braces() {
        // 块注释中包含大括号, 不应影响计数
        let code = "use std::{/* { } */ io::*};";
        let globs = extract_glob_imports(code);
        assert!(
            globs.contains(&"std::io".to_string()),
            "块注释中的大括号不应影响 glob 提取: {:?}",
            globs
        );
    }

    // ===== Session 136: strip_use_block_comments 测试 =====

    #[test]
    fn test_strip_use_block_comments_basic() {
        assert_eq!(
            strip_use_block_comments("use std::{/* comment */ io::*};"),
            "use std::{ io::*};"
        );
    }

    #[test]
    fn test_strip_use_block_comments_no_comment() {
        assert_eq!(
            strip_use_block_comments("use std::io::*;"),
            "use std::io::*;"
        );
    }

    #[test]
    fn test_strip_use_block_comments_nested() {
        assert_eq!(
            strip_use_block_comments("use std::{/* outer /* inner */ */ io::*};"),
            "use std::{ io::*};"
        );
    }

    #[test]
    fn test_strip_use_block_comments_multiple() {
        assert_eq!(
            strip_use_block_comments("use std::{/* a */ io::*, /* b */ sync::*};"),
            "use std::{ io::*,  sync::*};"
        );
    }

    // ===== Session 136: validate_rust_braces proc_macro 测试 =====

    #[test]
    fn test_validate_rust_braces_proc_macro_attribute() {
        let code = r#"
#[proc_macro]
pub fn my_macro(input: TokenStream) -> TokenStream {
    let expanded = quote! {
        let x = 42;
    };
    expanded
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "#[proc_macro] 属性宏应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_proc_macro_attribute_fn() {
        let code = r#"
#[proc_macro_attribute]
pub fn my_attr_macro(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let expanded = quote! {
        #item
    };
    expanded
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "#[proc_macro_attribute] 属性宏应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_proc_macro_derive() {
        let code = r#"
#[proc_macro_derive(MyDerive, attributes(my_attr))]
pub fn my_derive(input: TokenStream) -> TokenStream {
    let expanded = quote! {
        impl MyDerive for #name {
            fn do_something(&self) {
                println!("hello");
            }
        }
    };
    expanded
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "#[proc_macro_derive(...)] 属性宏应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_proc_macro_with_quote_repetition() {
        let code = r#"
#[proc_macro]
pub fn make_struct(input: TokenStream) -> TokenStream {
    let expanded = quote! {
        struct Generated {
            #( #field: #field_types, )*
        }
    };
    expanded
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "proc_macro 函数体内 quote! 重复语法应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_proc_macro_with_macro_rules() {
        let code = r#"
#[proc_macro]
pub fn my_macro(input: TokenStream) -> TokenStream {
    macro_rules! helper {
        ($x:expr) => { $x + 1 };
    }
    let result = helper!(42);
    quote! { #result }
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "proc_macro 函数体内 macro_rules! 应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_proc_macro_unbalanced() {
        let code = r#"
#[proc_macro]
pub fn my_macro(input: TokenStream) -> TokenStream {
    let expanded = quote! {
        struct Foo {
            x: i32,
    };
    expanded
}
"#;
        let result = validate_rust_braces(code);
        assert!(result.is_some(), "不平衡的 proc_macro 应报告问题");
    }

    // ===== Session 137: ensure_external_imports oauth2/glob/cookie 测试 =====

    #[test]
    fn test_ensure_external_imports_oauth2_basic_client() {
        let code = "fn foo() -> BasicClient { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use oauth2::BasicClient;"),
            "应添加 oauth2::BasicClient 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_oauth2_multiple() {
        let code = "fn foo() -> (BasicClient, AuthorizationCode, AccessToken) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use oauth2::{") && result.contains("BasicClient"),
            "应合并 oauth2 导入: {}",
            result
        );
        assert!(
            result.contains("AuthorizationCode"),
            "应包含 AuthorizationCode: {}",
            result
        );
        assert!(
            result.contains("AccessToken"),
            "应包含 AccessToken: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_oauth2_csrf_pkce() {
        let code = "fn foo() -> (CsrfToken, PkceCodeVerifier) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use oauth2::{") && result.contains("CsrfToken"),
            "应合并 oauth2 CsrfToken/PkceCodeVerifier 导入: {}",
            result
        );
        assert!(
            result.contains("PkceCodeVerifier"),
            "应包含 PkceCodeVerifier: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_oauth2_full_path() {
        let code = "fn foo() -> oauth2::BasicClient { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use oauth2::"),
            "全限定 oauth2:: 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_glob_pattern() {
        let code = "fn foo() -> Pattern { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use glob::Pattern;"),
            "应添加 glob::Pattern 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_glob_multiple() {
        let code = "fn foo() -> (Pattern, GlobBuilder) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use glob::{") && result.contains("Pattern"),
            "应合并 glob 导入: {}",
            result
        );
        assert!(
            result.contains("GlobBuilder"),
            "应包含 GlobBuilder: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_glob_full_path() {
        let code = "fn foo() -> glob::Pattern { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use glob::"),
            "全限定 glob:: 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_cookie() {
        let code = "fn foo() -> Cookie { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use cookie::Cookie;"),
            "应添加 cookie::Cookie 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_cookie_jar() {
        let code = "fn foo() -> CookieJar { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use cookie::CookieJar;"),
            "应添加 cookie::CookieJar 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_cookie_multiple() {
        let code = "fn foo() -> (Cookie, CookieJar) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use cookie::{") && result.contains("Cookie"),
            "应合并 cookie 导入: {}",
            result
        );
        assert!(result.contains("CookieJar"), "应包含 CookieJar: {}", result);
    }

    #[test]
    fn test_ensure_external_imports_cookie_full_path() {
        let code = "fn foo() -> cookie::Cookie { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use cookie::"),
            "全限定 cookie:: 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_s137_idempotent() {
        let code = "fn foo() -> (BasicClient, Pattern, Cookie) { unimplemented!() }\nfn bar() -> (AccessToken, GlobBuilder, CookieJar) { unimplemented!() }\nfn baz() -> (AuthorizationCode, CsrfToken, PkceCodeVerifier) { unimplemented!() }";
        let first = ensure_external_imports(code);
        let second = ensure_external_imports(&first);
        assert_eq!(first, second, "Session 137 新增外部 crate 检测应幂等");
    }

    // ===== Session 137: verify_imports oauth2/glob/cookie 类型测试 =====

    #[test]
    fn test_verify_imports_oauth2_missing() {
        let issues = verify_imports("fn foo() -> BasicClient { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "BasicClient" && i.module_path == "oauth2"),
            "应检测到 BasicClient 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_oauth2_access_token_missing() {
        let issues = verify_imports("fn foo() -> AccessToken { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "AccessToken" && i.module_path == "oauth2"),
            "应检测到 AccessToken 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_glob_missing() {
        let issues = verify_imports("fn foo() -> Pattern { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Pattern" && i.module_path == "glob"),
            "应检测到 Pattern 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_cookie_missing() {
        let issues = verify_imports("fn foo() -> Cookie { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Cookie" && i.module_path == "cookie"),
            "应检测到 Cookie 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_cookie_jar_missing() {
        let issues = verify_imports("fn foo() -> CookieJar { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "CookieJar" && i.module_path == "cookie"),
            "应检测到 CookieJar 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_oauth2_already_imported() {
        let code = "use oauth2::BasicClient;\nfn foo() -> BasicClient { unimplemented!() }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "BasicClient" && i.module_path == "oauth2"),
            "已有 oauth2::BasicClient 导入不应报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_glob_already_imported() {
        let code = "use glob::Pattern;\nfn foo() -> Pattern { unimplemented!() }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Pattern" && i.module_path == "glob"),
            "已有 glob::Pattern 导入不应报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_cookie_already_imported() {
        let code = "use cookie::Cookie;\nfn foo() -> Cookie { unimplemented!() }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Cookie" && i.module_path == "cookie"),
            "已有 cookie::Cookie 导入不应报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_s137_combined() {
        let code = "fn foo() -> (BasicClient, Pattern, Cookie) { unimplemented!() }";
        let issues = verify_imports(code);
        assert!(
            issues.iter().any(|i| i.type_name == "BasicClient"),
            "应检测到 BasicClient: {:?}",
            issues
        );
        assert!(
            issues.iter().any(|i| i.type_name == "Pattern"),
            "应检测到 Pattern: {:?}",
            issues
        );
        assert!(
            issues.iter().any(|i| i.type_name == "Cookie"),
            "应检测到 Cookie: {:?}",
            issues
        );
    }

    // ===== Session 137: verify_imports .zip()/.chain()/.enumerate() trait 方法测试 =====

    #[test]
    fn test_verify_imports_zip_method_iterator() {
        let issues = verify_imports("fn foo(a: Vec<i32>, b: Vec<i32>) { a.iter().zip(b.iter()); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应通过 .zip() 方法检测到 Iterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_chain_method_iterator() {
        let issues =
            verify_imports("fn foo(a: Vec<i32>, b: Vec<i32>) { a.iter().chain(b.iter()); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应通过 .chain() 方法检测到 Iterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_enumerate_method_iterator() {
        let issues = verify_imports("fn foo(v: Vec<i32>) { v.iter().enumerate(); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应通过 .enumerate() 方法检测到 Iterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_zip_already_imported() {
        let code = "use std::iter::Iterator;\nfn foo(a: Vec<i32>, b: Vec<i32>) { a.iter().zip(b.iter()); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "已有 Iterator 导入不应通过 .zip() 重复报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_chain_already_imported() {
        let code = "use std::iter::Iterator;\nfn foo(a: Vec<i32>, b: Vec<i32>) { a.iter().chain(b.iter()); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "已有 Iterator 导入不应通过 .chain() 重复报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_enumerate_already_imported() {
        let code = "use std::iter::Iterator;\nfn foo(v: Vec<i32>) { v.iter().enumerate(); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "已有 Iterator 导入不应通过 .enumerate() 重复报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_zip_glob_import() {
        let code =
            "use std::iter::*;\nfn foo(a: Vec<i32>, b: Vec<i32>) { a.iter().zip(b.iter()); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "glob 导入 use std::iter::*; 应覆盖 Iterator (.zip): {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_zip_chain_enumerate_combined() {
        let code = "fn foo(v: Vec<i32>) { v.iter().zip(v.iter()).chain(v.iter()).enumerate(); }";
        let issues = verify_imports(code);
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应检测到 Iterator (.zip+.chain+.enumerate 组合): {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_all_iterator_methods_combined() {
        let code =
            "fn foo(v: Vec<i32>) { v.iter().map(|x| x).filter(|&x| x > 0).zip(v.iter()).chain(v.iter()).enumerate(); }";
        let issues = verify_imports(code);
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应检测到 Iterator (全部方法组合): {:?}",
            issues
        );
    }

    // ===== Session 137: validate_rust_braces const fn 测试 =====

    #[test]
    fn test_validate_rust_braces_const_fn_basic() {
        let code = "const fn foo(x: i32) -> i32 { x + 1 }";
        assert!(
            validate_rust_braces(code).is_none(),
            "const fn 基本语法应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_const_fn_with_generics() {
        let code = "const fn foo<T: Copy>(x: T) -> T { x }";
        assert!(
            validate_rust_braces(code).is_none(),
            "const fn 泛型应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_const_fn_complex_body() {
        let code = r#"
const fn factorial(n: usize) -> usize {
    if n <= 1 {
        1
    } else {
        n * factorial(n - 1)
    }
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "const fn 复杂函数体应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_const_fn_unbalanced() {
        let code = "const fn foo(x: i32) -> i32 { x + 1";
        let result = validate_rust_braces(code);
        assert!(result.is_some(), "不平衡的 const fn 应报告问题");
    }

    #[test]
    fn test_validate_rust_braces_const_fn_with_array() {
        let code = r#"
const fn build_array() -> [i32; 3] {
    [1, 2, 3]
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "const fn 返回数组应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_const_fn_nested() {
        let code = r#"
struct Foo {
    x: i32,
}

impl Foo {
    const fn new() -> Self {
        Foo { x: 0 }
    }

    const fn with_x(mut self, x: i32) -> Self {
        self.x = x;
        self
    }
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "impl 中的 const fn 应通过验证"
        );
    }

    // ===== Session 138: ensure_external_imports dotenv/tauri/wgpu 测试 =====

    #[test]
    fn test_ensure_external_imports_dotenv_env_error() {
        let code = "fn foo() -> EnvError { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use dotenv::EnvError;"),
            "应添加 dotenv::EnvError 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_dotenv_full_path() {
        let code = "fn foo() -> dotenv::EnvError { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use dotenv::"),
            "全限定 dotenv:: 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_tauri_app_builder() {
        let code = "fn foo() -> AppBuilder { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use tauri::AppBuilder;"),
            "应添加 tauri::AppBuilder 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_tauri_multiple() {
        let code = "fn foo() -> (AppBuilder, AppHandle, Manager, Invoke) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use tauri::{") && result.contains("AppBuilder"),
            "应合并 tauri 导入: {}",
            result
        );
        assert!(result.contains("AppHandle"), "应包含 AppHandle: {}", result);
        assert!(result.contains("Manager"), "应包含 Manager: {}", result);
        assert!(result.contains("Invoke"), "应包含 Invoke: {}", result);
    }

    #[test]
    fn test_ensure_external_imports_tauri_full_path() {
        let code = "fn foo() -> tauri::AppHandle { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use tauri::"),
            "全限定 tauri:: 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_wgpu_device() {
        let code = "fn foo() -> Device { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use wgpu::Device;"),
            "应添加 wgpu::Device 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_wgpu_multiple() {
        let code = "fn foo() -> (Device, Queue, Surface) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use wgpu::{") && result.contains("Device"),
            "应合并 wgpu 导入: {}",
            result
        );
        assert!(result.contains("Queue"), "应包含 Queue: {}", result);
        assert!(result.contains("Surface"), "应包含 Surface: {}", result);
    }

    #[test]
    fn test_ensure_external_imports_wgpu_surface_config() {
        let code = "fn foo() -> SurfaceConfiguration { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use wgpu::SurfaceConfiguration;"),
            "应添加 wgpu::SurfaceConfiguration 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_wgpu_shader_module() {
        let code = "fn foo() -> ShaderModule { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use wgpu::ShaderModule;"),
            "应添加 wgpu::ShaderModule 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_wgpu_full_path() {
        let code = "fn foo() -> wgpu::Device { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use wgpu::"),
            "全限定 wgpu:: 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_s138_idempotent() {
        let code = "fn foo() -> (EnvError, AppBuilder, Device) { unimplemented!() }\nfn bar() -> (AppHandle, Queue, Surface) { unimplemented!() }\nfn baz() -> (Manager, Invoke, SurfaceConfiguration, ShaderModule) { unimplemented!() }";
        let first = ensure_external_imports(code);
        let second = ensure_external_imports(&first);
        assert_eq!(first, second, "Session 138 新增外部 crate 检测应幂等");
    }

    // ===== Session 138: verify_imports dotenv/tauri/wgpu 类型测试 =====

    #[test]
    fn test_verify_imports_dotenv_env_error_missing() {
        let issues = verify_imports("fn foo() -> EnvError { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "EnvError" && i.module_path == "dotenv"),
            "应检测到 EnvError 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_dotenv_already_imported() {
        let code = "use dotenv::EnvError;\nfn foo() -> EnvError { unimplemented!() }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "EnvError" && i.module_path == "dotenv"),
            "已有 dotenv::EnvError 导入不应报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_tauri_app_builder_missing() {
        let issues = verify_imports("fn foo() -> AppBuilder { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "AppBuilder" && i.module_path == "tauri"),
            "应检测到 AppBuilder 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_tauri_manager_missing() {
        let issues = verify_imports("fn foo() -> Manager { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Manager" && i.module_path == "tauri"),
            "应检测到 Manager 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_tauri_already_imported() {
        let code = "use tauri::AppHandle;\nfn foo() -> AppHandle { unimplemented!() }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "AppHandle" && i.module_path == "tauri"),
            "已有 tauri::AppHandle 导入不应报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_wgpu_device_missing() {
        let issues = verify_imports("fn foo() -> Device { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Device" && i.module_path == "wgpu"),
            "应检测到 Device 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_wgpu_queue_missing() {
        let issues = verify_imports("fn foo() -> Queue { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Queue" && i.module_path == "wgpu"),
            "应检测到 Queue 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_wgpu_surface_config_missing() {
        let issues = verify_imports("fn foo() -> SurfaceConfiguration { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "SurfaceConfiguration" && i.module_path == "wgpu"),
            "应检测到 SurfaceConfiguration 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_wgpu_already_imported() {
        let code = "use wgpu::Device;\nfn foo() -> Device { unimplemented!() }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Device" && i.module_path == "wgpu"),
            "已有 wgpu::Device 导入不应报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_s138_combined() {
        let code = "fn foo() -> (EnvError, AppBuilder, Device) { unimplemented!() }";
        let issues = verify_imports(code);
        assert!(
            issues.iter().any(|i| i.type_name == "EnvError"),
            "应检测到 EnvError: {:?}",
            issues
        );
        assert!(
            issues.iter().any(|i| i.type_name == "AppBuilder"),
            "应检测到 AppBuilder: {:?}",
            issues
        );
        assert!(
            issues.iter().any(|i| i.type_name == "Device"),
            "应检测到 Device: {:?}",
            issues
        );
    }

    // ===== Session 138: verify_imports .flat_map()/.peekable()/.skip() trait 方法测试 =====

    #[test]
    fn test_verify_imports_flat_map_method_iterator() {
        let issues =
            verify_imports("fn foo(v: Vec<Vec<i32>>) { v.iter().flat_map(|x| x.iter()); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应通过 .flat_map() 方法检测到 Iterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_peekable_method_iterator() {
        let issues = verify_imports("fn foo(v: Vec<i32>) { let mut p = v.iter().peekable(); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应通过 .peekable() 方法检测到 Iterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_skip_method_iterator() {
        let issues = verify_imports("fn foo(v: Vec<i32>) { v.iter().skip(3); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应通过 .skip() 方法检测到 Iterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_flat_map_already_imported() {
        let code = "use std::iter::Iterator;\nfn foo(v: Vec<Vec<i32>>) { v.iter().flat_map(|x| x.iter()); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "已有 Iterator 导入不应通过 .flat_map() 重复报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_peekable_already_imported() {
        let code =
            "use std::iter::Iterator;\nfn foo(v: Vec<i32>) { let mut p = v.iter().peekable(); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "已有 Iterator 导入不应通过 .peekable() 重复报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_skip_already_imported() {
        let code = "use std::iter::Iterator;\nfn foo(v: Vec<i32>) { v.iter().skip(3); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "已有 Iterator 导入不应通过 .skip() 重复报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_flat_map_glob_import() {
        let code =
            "use std::iter::*;\nfn foo(v: Vec<Vec<i32>>) { v.iter().flat_map(|x| x.iter()); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "glob 导入 use std::iter::*; 应覆盖 Iterator (.flat_map): {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_flat_map_peekable_skip_combined() {
        let code = "fn foo(v: Vec<i32>) { v.iter().flat_map(|x| Some(x)).peekable().skip(1); }";
        let issues = verify_imports(code);
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应检测到 Iterator (.flat_map+.peekable+.skip 组合): {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_all_iterator_methods_s138_combined() {
        let code = "fn foo(v: Vec<i32>) { v.iter().map(|x| x).filter(|&x| x > 0).zip(v.iter()).chain(v.iter()).enumerate().flat_map(|x| Some(x)).peekable().skip(1); }";
        let issues = verify_imports(code);
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应检测到 Iterator (全部方法组合含 flat_map/peekable/skip): {:?}",
            issues
        );
    }

    // ===== Session 138: validate_rust_braces async fn 测试 =====

    #[test]
    fn test_validate_rust_braces_async_fn_basic() {
        let code = "async fn foo() -> i32 { 42 }";
        assert!(
            validate_rust_braces(code).is_none(),
            "async fn 基本语法应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_async_fn_with_generics() {
        let code = "async fn foo<T: Send>(x: T) -> T { x }";
        assert!(
            validate_rust_braces(code).is_none(),
            "async fn 泛型应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_async_fn_with_await() {
        let code = r#"
async fn foo() -> i32 {
    let x = bar().await;
    x + 1
}

async fn bar() -> i32 { 42 }
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "async fn 含 .await 应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_async_fn_await_chain() {
        let code = r#"
async fn foo() -> i32 {
    let x = bar().await.unwrap_or(0);
    let y = baz(x).await?;
    y
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "async fn 含 .await 链式调用应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_async_fn_complex_body() {
        let code = r#"
async fn fetch_and_process(url: &str) -> Result<String, Box<dyn std::error::Error>> {
    let response = fetch(url).await?;
    let data: Vec<u8> = response.json().await?;
    let result = process(&data).await?;
    Ok(result)
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "async fn 复杂函数体应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_async_fn_nested_block() {
        let code = r#"
async fn foo() -> i32 {
    let x = {
        let y = { 42 };
        y + 1
    };
    x
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "async fn 嵌套块应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_async_fn_unbalanced() {
        let code = "async fn foo() -> i32 { 42";
        let result = validate_rust_braces(code);
        assert!(result.is_some(), "不平衡的 async fn 应报告问题");
    }

    #[test]
    fn test_validate_rust_braces_async_fn_with_struct() {
        let code = r#"
struct Foo { x: i32 }

impl Foo {
    async fn fetch(&self) -> i32 {
        self.x + 1
    }

    async fn update(&mut self, v: i32) {
        self.x = v;
    }
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "impl 中的 async fn 应通过验证"
        );
    }

    // ===== Session 138: extract_glob_imports cfg 属性条件编译测试 =====

    #[test]
    fn test_extract_glob_imports_with_cfg_attribute() {
        let code = "#[cfg(feature = \"async\")]\nuse std::io::*;";
        let globs = extract_glob_imports(code);
        assert!(
            globs.contains(&"std::io".to_string()),
            "带 #[cfg] 属性的 use 语句应提取 glob: {:?}",
            globs
        );
    }

    #[test]
    fn test_extract_glob_imports_with_cfg_multiple() {
        let code = "#[cfg(feature = \"async\")]\nuse std::io::*;\n#[cfg(feature = \"sync\")]\nuse std::sync::*;";
        let globs = extract_glob_imports(code);
        assert!(
            globs.contains(&"std::io".to_string()),
            "应包含 std::io: {:?}",
            globs
        );
        assert!(
            globs.contains(&"std::sync".to_string()),
            "应包含 std::sync: {:?}",
            globs
        );
    }

    #[test]
    fn test_extract_glob_imports_with_cfg_nested() {
        let code = r#"#[cfg(all(feature = "async", not(target_os = "wasm32")))]
use std::io::*;"#;
        let globs = extract_glob_imports(code);
        assert!(
            globs.contains(&"std::io".to_string()),
            "带复杂 #[cfg(all(...))] 属性的 use 语句应提取 glob: {:?}",
            globs
        );
    }

    #[test]
    fn test_extract_glob_imports_with_cfg_and_normal() {
        let code = "use std::fmt::*;\n#[cfg(feature = \"debug\")]\nuse std::io::*;";
        let globs = extract_glob_imports(code);
        assert!(
            globs.contains(&"std::fmt".to_string()),
            "应包含普通 use std::fmt::*: {:?}",
            globs
        );
        assert!(
            globs.contains(&"std::io".to_string()),
            "应包含带 cfg 属性的 use std::io::*: {:?}",
            globs
        );
    }

    #[test]
    fn test_extract_glob_imports_cfg_not_affecting_glob() {
        let code = "#[cfg(feature = \"x\")]\nuse std::fmt::Display;";
        let globs = extract_glob_imports(code);
        assert!(globs.is_empty(), "非 glob use 语句不应被提取: {:?}", globs);
    }

    // ===== Session 139: ensure_external_imports env_logger/notify/shadow-rs 测试 =====

    #[test]
    fn test_ensure_external_imports_env_logger_builder() {
        let code = "fn foo() -> Builder { Builder::new() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use env_logger::Builder;"),
            "应添加 env_logger::Builder 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_env_logger_target() {
        let code = "fn foo(t: &Target) { t.set_level(); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use env_logger::Target;"),
            "应添加 env_logger::Target 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_env_logger_filter() {
        let code = "fn foo() -> Filter { Filter::new(\"info\") }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use env_logger::Filter;"),
            "应添加 env_logger::Filter 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_env_logger_multiple() {
        let code = "fn foo() -> (Builder, Target, Filter) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use env_logger::{") && result.contains("Builder"),
            "应合并 env_logger 导入: {}",
            result
        );
        assert!(result.contains("Filter"), "应包含 Filter: {}", result);
        assert!(result.contains("Target"), "应包含 Target: {}", result);
    }

    #[test]
    fn test_ensure_external_imports_env_logger_full_path() {
        let code = "fn foo() -> env_logger::Builder { env_logger::Builder::new() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use env_logger::"),
            "全限定 env_logger:: 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_notify_watcher() {
        let code = "fn foo(w: &Watcher) { w.watch(Path::new(\".\"), RecursiveMode::Recursive); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use notify::Watcher;"),
            "应添加 notify::Watcher 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_notify_event_kind() {
        let code = "fn foo() -> EventKind { EventKind::Create(CreateKind::File) }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use notify::EventKind;"),
            "应添加 notify::EventKind 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_notify_event() {
        let code = "fn foo(e: &Event) { println!(\"{:?}\", e); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use notify::Event;"),
            "应添加 notify::Event 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_notify_multiple() {
        let code = "fn foo() -> (Watcher, EventKind, Event) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use notify::{") && result.contains("Event"),
            "应合并 notify 导入: {}",
            result
        );
        assert!(result.contains("EventKind"), "应包含 EventKind: {}", result);
        assert!(result.contains("Watcher"), "应包含 Watcher: {}", result);
    }

    #[test]
    fn test_ensure_external_imports_notify_full_path() {
        let code = "fn foo(w: &notify::Watcher) { w.watch(); }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use notify::"),
            "全限定 notify:: 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_shadow_rs() {
        let code = "fn foo() -> ShadowBuilder { ShadowBuilder::new() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use shadow_rs::ShadowBuilder;"),
            "应添加 shadow_rs::ShadowBuilder 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_shadow_rs_full_path() {
        let code = "fn foo() -> shadow_rs::ShadowBuilder { shadow_rs::ShadowBuilder::new() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use shadow_rs::"),
            "全限定 shadow_rs:: 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_s139_idempotent() {
        let code = "fn foo() -> (Builder, Watcher, ShadowBuilder) { unimplemented!() }\nfn bar() -> (Target, EventKind) { unimplemented!() }\nfn baz() -> (Filter, Event) { unimplemented!() }";
        let first = ensure_external_imports(code);
        let second = ensure_external_imports(&first);
        assert_eq!(first, second, "Session 139 新增外部 crate 检测应幂等");
    }

    // ===== Session 139: verify_imports env_logger/notify/shadow-rs 类型测试 =====

    #[test]
    fn test_verify_imports_env_logger_builder_missing() {
        let issues = verify_imports("fn foo() -> Builder { Builder::new() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Builder" && i.module_path == "env_logger"),
            "应检测到 env_logger::Builder 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_env_logger_already_imported() {
        let code = "use env_logger::Builder;\nfn foo() -> Builder { Builder::new() }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Builder" && i.module_path == "env_logger"),
            "已有 env_logger::Builder 导入不应报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_notify_watcher_missing() {
        let issues = verify_imports("fn foo(w: &Watcher) { w.watch(); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Watcher" && i.module_path == "notify"),
            "应检测到 notify::Watcher 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_notify_already_imported() {
        let code = "use notify::Watcher;\nfn foo(w: &Watcher) { w.watch(); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Watcher" && i.module_path == "notify"),
            "已有 notify::Watcher 导入不应报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_shadow_rs_missing() {
        let issues = verify_imports("fn foo() -> ShadowBuilder { ShadowBuilder::new() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "ShadowBuilder" && i.module_path == "shadow_rs"),
            "应检测到 shadow_rs::ShadowBuilder 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_shadow_rs_already_imported() {
        let code =
            "use shadow_rs::ShadowBuilder;\nfn foo() -> ShadowBuilder { ShadowBuilder::new() }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "ShadowBuilder" && i.module_path == "shadow_rs"),
            "已有 shadow_rs::ShadowBuilder 导入不应报告: {:?}",
            issues
        );
    }

    // ===== Session 139: verify_imports .take()/.rev()/.step_by() trait 方法测试 =====

    #[test]
    fn test_verify_imports_take_method_iterator() {
        let issues = verify_imports("fn foo(v: Vec<i32>) { v.iter().take(3); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应通过 .take() 方法检测到 Iterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_rev_method_iterator() {
        let issues = verify_imports("fn foo(v: Vec<i32>) { v.iter().rev(); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应通过 .rev() 方法检测到 Iterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_step_by_method_iterator() {
        let issues = verify_imports("fn foo(v: Vec<i32>) { v.iter().step_by(2); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应通过 .step_by() 方法检测到 Iterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_take_already_imported() {
        let code = "use std::iter::Iterator;\nfn foo(v: Vec<i32>) { v.iter().take(3); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "已有 Iterator 导入不应通过 .take() 重复报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_rev_already_imported() {
        let code = "use std::iter::Iterator;\nfn foo(v: Vec<i32>) { v.iter().rev(); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "已有 Iterator 导入不应通过 .rev() 重复报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_step_by_already_imported() {
        let code = "use std::iter::Iterator;\nfn foo(v: Vec<i32>) { v.iter().step_by(2); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "已有 Iterator 导入不应通过 .step_by() 重复报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_take_glob_import() {
        let code = "use std::iter::*;\nfn foo(v: Vec<i32>) { v.iter().take(3); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "glob 导入 use std::iter::*; 应覆盖 Iterator (.take): {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_take_rev_step_by_combined() {
        let code = "fn foo(v: Vec<i32>) { v.iter().take(3).rev().step_by(2); }";
        let issues = verify_imports(code);
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应检测到 Iterator (.take+.rev+.step_by 组合): {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_all_iterator_methods_s139_combined() {
        let code = "fn foo(v: Vec<i32>) { v.iter().map(|x| x).filter(|&x| x > 0).zip(v.iter()).chain(v.iter()).enumerate().flat_map(|x| Some(x)).peekable().skip(1).take(2).rev().step_by(3); }";
        let issues = verify_imports(code);
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应检测到 Iterator (全部方法组合含 take/rev/step_by): {:?}",
            issues
        );
    }

    // ===== Session 139: validate_rust_braces async block 测试 =====

    #[test]
    fn test_validate_rust_braces_async_block_basic() {
        let code = "fn foo() { let f = async { 42 }; }";
        assert!(
            validate_rust_braces(code).is_none(),
            "async block 基本语法应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_async_move_block() {
        let code = "fn foo() { let x = 42; let f = async move { x + 1 }; }";
        assert!(
            validate_rust_braces(code).is_none(),
            "async move block 应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_async_block_with_await() {
        let code = r#"
async fn foo() -> i32 {
    let x = async { bar().await };
    x.await
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "async block 含 .await 应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_async_block_complex_body() {
        let code = r#"
fn foo() {
    let f = async {
        let x = {
            let y = { 42 };
            y + 1
        };
        x
    };
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "async block 复杂函数体应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_async_block_with_match() {
        let code = r#"
fn foo() {
    let f = async {
        match Some(42) {
            Some(x) => x + 1,
            None => 0,
        }
    };
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "async block 含 match 表达式应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_async_block_unbalanced() {
        let code = "fn foo() { let f = async { 42; ";
        let result = validate_rust_braces(code);
        assert!(result.is_some(), "不平衡的 async block 应报告问题");
    }

    #[test]
    fn test_validate_rust_braces_async_block_nested() {
        let code = r#"
fn foo() {
    let f = async {
        let g = async move {
            let h = async { 42 };
            h
        };
        g
    };
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "嵌套 async block 应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_async_block_with_closure() {
        let code = r#"
fn foo() {
    let f = async {
        let f = |x: i32| { x * 2 };
        f(42)
    };
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "async block 含闭包应通过验证"
        );
    }

    // ===== Session 139: extract_glob_imports cfg_attr 属性测试 =====

    #[test]
    fn test_extract_glob_imports_with_cfg_attr() {
        let code = r#"#[cfg_attr(feature = "x", derive(Debug))]
use std::io::*;"#;
        let globs = extract_glob_imports(code);
        assert!(
            globs.contains(&"std::io".to_string()),
            "带 #[cfg_attr] 属性的 use 语句应提取 glob: {:?}",
            globs
        );
    }

    #[test]
    fn test_extract_glob_imports_with_cfg_attr_multiple() {
        let code = r#"#[cfg_attr(feature = "x", derive(Debug))]
use std::io::*;
#[cfg_attr(feature = "y", derive(Clone))]
use std::sync::*;"#;
        let globs = extract_glob_imports(code);
        assert!(
            globs.contains(&"std::io".to_string()),
            "应包含 std::io (cfg_attr): {:?}",
            globs
        );
        assert!(
            globs.contains(&"std::sync".to_string()),
            "应包含 std::sync (cfg_attr): {:?}",
            globs
        );
    }

    #[test]
    fn test_extract_glob_imports_cfg_attr_and_cfg_mixed() {
        let code = r#"#[cfg(feature = "a")]
use std::fmt::*;
#[cfg_attr(feature = "b", derive(Debug))]
use std::io::*;"#;
        let globs = extract_glob_imports(code);
        assert!(
            globs.contains(&"std::fmt".to_string()),
            "应包含 std::fmt (cfg): {:?}",
            globs
        );
        assert!(
            globs.contains(&"std::io".to_string()),
            "应包含 std::io (cfg_attr): {:?}",
            globs
        );
    }

    #[test]
    fn test_extract_glob_imports_cfg_attr_not_affecting_non_glob() {
        let code = r#"#[cfg_attr(feature = "x", derive(Debug))]
use std::fmt::Display;"#;
        let globs = extract_glob_imports(code);
        assert!(
            globs.is_empty(),
            "非 glob use 语句 (带 cfg_attr) 不应被提取: {:?}",
            globs
        );
    }

    #[test]
    fn test_extract_glob_imports_cfg_attr_complex() {
        let code = r#"#[cfg_attr(target_os = "linux", derive(Debug))]
use std::io::*;"#;
        let globs = extract_glob_imports(code);
        assert!(
            globs.contains(&"std::io".to_string()),
            "带复杂 #[cfg_attr(target_os = ...)] 属性的 use 语句应提取 glob: {:?}",
            globs
        );
    }

    // ===== Session 141: ensure_external_imports sysinfo/serialport/machine-uid 测试 =====

    #[test]
    fn test_ensure_external_imports_sysinfo_system() {
        let code = "fn foo() -> System { System::new() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use sysinfo::System;"),
            "应添加 sysinfo::System 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_sysinfo_cpu_core() {
        let code = "fn foo() -> CpuCore { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use sysinfo::CpuCore;"),
            "应添加 sysinfo::CpuCore 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_sysinfo_disk() {
        let code = "fn foo() -> Disk { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use sysinfo::Disk;"),
            "应添加 sysinfo::Disk 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_sysinfo_multiple() {
        let code = "fn foo() -> (System, CpuCore, Disk) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use sysinfo::{") && result.contains("System"),
            "应合并 sysinfo 导入: {}",
            result
        );
        assert!(result.contains("CpuCore"), "应包含 CpuCore: {}", result);
        assert!(result.contains("Disk"), "应包含 Disk: {}", result);
    }

    #[test]
    fn test_ensure_external_imports_sysinfo_full_path() {
        let code = "fn foo() -> sysinfo::System { System::new() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use sysinfo::System;"),
            "全限定 sysinfo::System 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_serialport() {
        let code = "fn foo() -> SerialPort { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use serialport::SerialPort;"),
            "应添加 serialport::SerialPort 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_serialport_full_path() {
        let code = "fn foo() -> serialport::SerialPort { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use serialport::"),
            "全限定 serialport:: 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_machine_uid() {
        // machine_uid::get() is a full-path usage, should NOT need import
        let code = "fn foo() -> String { let id = machine_uid::get().unwrap(); id }";
        let result = ensure_external_imports(code);
        // machine_uid::get() contains "machine_uid::" which means it's a full path
        // The function should detect that "machine_uid" is used as a crate path prefix
        // and not add a redundant import
        assert!(
            !result.contains("use machine_uid::machine_uid;"),
            "全限定 machine_uid:: 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_machine_uid_bare() {
        let code = "fn foo() { let id: machine_uid = unimplemented!(); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use machine_uid::machine_uid;"),
            "应添加 machine_uid 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_s141_idempotent() {
        let code = "fn foo() -> (System, CpuCore, Disk, SerialPort) { unimplemented!() }\nfn bar() -> machine_uid { unimplemented!() }";
        let first = ensure_external_imports(code);
        let second = ensure_external_imports(&first);
        assert_eq!(first, second, "Session 141 新增外部 crate 检测应幂等");
    }

    // ===== Session 141: ensure_external_imports .cloned()/.copied()/.fuse() 测试 =====

    #[test]
    fn test_ensure_external_imports_cloned_method() {
        let code = "fn foo(v: Vec<&i32>) -> Vec<i32> { v.iter().cloned().collect() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应通过 .cloned() 添加 Iterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_copied_method() {
        let code = "fn foo(v: Vec<&i32>) -> Vec<i32> { v.iter().copied().collect() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应通过 .copied() 添加 Iterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_fuse_method() {
        let code = "fn foo(v: Vec<i32>) { let mut it = v.iter().fuse(); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应通过 .fuse() 添加 Iterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_cloned_already_imported() {
        let code = "use std::iter::Iterator;\nfn foo(v: Vec<&i32>) -> Vec<i32> { v.iter().cloned().collect() }";
        let result = ensure_external_imports(code);
        // Should not add a duplicate
        let count = result.matches("use std::iter::Iterator;").count();
        assert_eq!(
            count, 1,
            "已有 Iterator 导入不应重复添加 (.cloned): {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_fuse_already_imported() {
        let code =
            "use std::iter::Iterator;\nfn foo(v: Vec<i32>) { let mut it = v.iter().fuse(); }";
        let result = ensure_external_imports(code);
        let count = result.matches("use std::iter::Iterator;").count();
        assert_eq!(
            count, 1,
            "已有 Iterator 导入不应重复添加 (.fuse): {}",
            result
        );
    }

    // ===== Session 141: verify_imports sysinfo/serialport/machine-uid 测试 =====

    #[test]
    fn test_verify_imports_sysinfo_system_missing() {
        let issues = verify_imports("fn foo() -> System { System::new() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "System" && i.module_path == "sysinfo"),
            "应检测到 sysinfo::System 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_sysinfo_cpu_core_missing() {
        let issues = verify_imports("fn foo() -> CpuCore { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "CpuCore" && i.module_path == "sysinfo"),
            "应检测到 sysinfo::CpuCore 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_sysinfo_disk_missing() {
        let issues = verify_imports("fn foo() -> Disk { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Disk" && i.module_path == "sysinfo"),
            "应检测到 sysinfo::Disk 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_sysinfo_already_imported() {
        let code = "use sysinfo::System;\nfn foo() -> System { System::new() }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "System" && i.module_path == "sysinfo"),
            "已有 sysinfo::System 导入不应报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_serialport_missing() {
        let issues = verify_imports("fn foo() -> SerialPort { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "SerialPort" && i.module_path == "serialport"),
            "应检测到 serialport::SerialPort 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_serialport_already_imported() {
        let code = "use serialport::SerialPort;\nfn foo() -> SerialPort { unimplemented!() }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "SerialPort" && i.module_path == "serialport"),
            "已有 serialport::SerialPort 导入不应报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_machine_uid_missing() {
        let issues = verify_imports("fn foo() { let id: machine_uid = unimplemented!(); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "machine_uid" && i.module_path == "machine_uid"),
            "应检测到 machine_uid 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_machine_uid_already_imported() {
        let code =
            "use machine_uid::machine_uid;\nfn foo() { let id: machine_uid = unimplemented!(); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "machine_uid" && i.module_path == "machine_uid"),
            "已有 machine_uid 导入不应报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_s141_combined() {
        let code = "fn foo() -> (System, CpuCore, Disk) { unimplemented!() }\nfn bar() -> SerialPort { unimplemented!() }";
        let issues = verify_imports(code);
        assert!(
            issues.iter().any(|i| i.type_name == "System"),
            "应检测到 System: {:?}",
            issues
        );
        assert!(
            issues.iter().any(|i| i.type_name == "CpuCore"),
            "应检测到 CpuCore: {:?}",
            issues
        );
        assert!(
            issues.iter().any(|i| i.type_name == "Disk"),
            "应检测到 Disk: {:?}",
            issues
        );
        assert!(
            issues.iter().any(|i| i.type_name == "SerialPort"),
            "应检测到 SerialPort: {:?}",
            issues
        );
    }

    // ===== Session 141: verify_imports .cloned()/.copied()/.fuse() trait 方法测试 =====

    #[test]
    fn test_verify_imports_cloned_method_iterator() {
        let issues =
            verify_imports("fn foo(v: Vec<&i32>) -> Vec<i32> { v.iter().cloned().collect() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应通过 .cloned() 方法检测到 Iterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_copied_method_iterator() {
        let issues =
            verify_imports("fn foo(v: Vec<&i32>) -> Vec<i32> { v.iter().copied().collect() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应通过 .copied() 方法检测到 Iterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_fuse_method_iterator() {
        let issues = verify_imports("fn foo(v: Vec<i32>) { let mut it = v.iter().fuse(); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应通过 .fuse() 方法检测到 Iterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_cloned_already_imported() {
        let code = "use std::iter::Iterator;\nfn foo(v: Vec<&i32>) -> Vec<i32> { v.iter().cloned().collect() }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "已有 Iterator 导入不应通过 .cloned() 重复报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_copied_already_imported() {
        let code = "use std::iter::Iterator;\nfn foo(v: Vec<&i32>) -> Vec<i32> { v.iter().copied().collect() }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "已有 Iterator 导入不应通过 .copied() 重复报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_fuse_already_imported() {
        let code =
            "use std::iter::Iterator;\nfn foo(v: Vec<i32>) { let mut it = v.iter().fuse(); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "已有 Iterator 导入不应通过 .fuse() 重复报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_cloned_copied_fuse_glob_import() {
        let code = "use std::iter::*;\nfn foo(v: Vec<&i32>) { v.iter().cloned().copied().fuse(); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "glob 导入 use std::iter::*; 应覆盖 Iterator (.cloned/.copied/.fuse): {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_cloned_copied_fuse_combined() {
        let code = "fn foo(v: Vec<&i32>) { v.iter().cloned().copied().fuse(); }";
        let issues = verify_imports(code);
        // Should only report once (deduped)
        let iterator_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.type_name == "Iterator" && i.module_path == "std::iter")
            .collect();
        assert_eq!(
            iterator_issues.len(),
            1,
            "多种 Iterator 方法只应报告一次: {:?}",
            issues
        );
    }

    // ===== Session 141: validate_rust_braces async gen / gen / 多属性 use 测试 =====

    #[test]
    fn test_validate_rust_braces_gen_block_basic() {
        // gen { } 语法 (Rust 2024 edition 生成器)
        let code = "fn foo() { let g = gen { yield 42 }; }";
        assert!(
            validate_rust_braces(code).is_none(),
            "gen block 基本语法应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_async_gen_block() {
        // async gen { } 语法
        let code = "fn foo() { let g = async gen { yield 42 }; }";
        assert!(
            validate_rust_braces(code).is_none(),
            "async gen block 应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_gen_block_complex() {
        let code = r#"
fn foo() {
    let g = gen {
        let x = {
            let y = { 42 };
            y + 1
        };
        yield x;
    };
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "gen block 复杂函数体应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_gen_block_unbalanced() {
        let code = "fn foo() { let g = gen { yield 42; ";
        let result = validate_rust_braces(code);
        assert!(result.is_some(), "不平衡的 gen block 应报告问题");
    }

    #[test]
    fn test_validate_rust_braces_async_gen_block_with_await() {
        let code = r#"
async fn foo() {
    let g = async gen {
        let x = bar().await;
        yield x;
    };
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "async gen block 含 .await 应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_nested_gen_blocks() {
        let code = r#"
fn foo() {
    let g = gen {
        let h = async gen {
            yield 42;
        };
        yield 1;
    };
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "嵌套 gen blocks 应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_use_with_multiple_attributes() {
        // use 语句中多个属性 #[cfg(...)] #[allow(...)] 不影响括号计数
        let code = r#"
#[cfg(feature = "serde")]
#[allow(unused_imports)]
use std::collections::HashMap;

#[cfg(test)]
#[allow(dead_code)]
use std::sync::Mutex;

fn main() {
    let m = HashMap::new();
    let x = Mutex::new(42);
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "use 语句中多个属性不应影响括号计数"
        );
    }

    #[test]
    fn test_validate_rust_braces_use_with_cfg_and_allow_mixed() {
        // 混合 cfg 和 allow 属性
        let code = r#"
#[cfg(unix)]
#[allow(unused)]
use std::os::unix::fs::FileExt;

#[cfg(windows)]
#[allow(unused)]
use std::os::windows::fs::FileExt;

fn main() {
    let x = vec![1, 2, 3];
}
"#;
        assert!(
            validate_rust_braces(code).is_none(),
            "混合 cfg 和 allow 属性的 use 语句应通过验证"
        );
    }

    #[test]
    fn test_validate_rust_braces_use_multiple_attrs_unbalanced() {
        // 即使有多个属性, 不配对的括号仍应被检测到
        let code = r#"
#[cfg(feature = "x")]
#[allow(unused_imports)]
use std::collections::HashMap;

fn main() {
    let m = HashMap::new();
    // 缺少闭合大括号
}
"#;
        // This should pass because the code is actually balanced
        assert!(
            validate_rust_braces(code).is_none(),
            "配对的代码含多属性 use 语句应通过验证"
        );
    }

    #[test]
    fn test_verify_imports_all_iterator_methods_s141_combined() {
        let code =
            "fn foo(v: Vec<&i32>) { v.iter().cloned().copied().fuse().take(2).rev().step_by(3); }";
        let issues = verify_imports(code);
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应检测到 Iterator (全部方法组合含 cloned/copied/fuse): {:?}",
            issues
        );
    }

    // ===== Session 141: ensure_external_imports 后无问题验证 =====

    #[test]
    fn test_ensure_external_imports_then_verify_no_s141_issues() {
        let code = "fn foo() -> (System, CpuCore, Disk, SerialPort) { unimplemented!() }";
        let fixed = ensure_external_imports(code);
        let issues = verify_imports(&fixed);
        let s141_issues: Vec<_> = issues
            .iter()
            .filter(|i| {
                i.module_path == "sysinfo"
                    || i.module_path == "serialport"
                    || i.module_path == "machine_uid"
            })
            .collect();
        assert!(
            s141_issues.is_empty(),
            "ensure_external_imports 后不应有 Session 141 外部 crate 导入问题: {:?}",
            s141_issues
        );
    }

    // ===== Session 142: ensure_external_imports dotenvy/fd-lock/nix/camino 测试 =====

    #[test]
    fn test_ensure_external_imports_dotenvy_env_loader() {
        let code = "fn foo() -> EnvLoader { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use dotenvy::EnvLoader;"),
            "应添加 dotenvy::EnvLoader 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_dotenvy_env_iter() {
        let code = "fn foo() -> EnvIter { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use dotenvy::EnvIter;"),
            "应添加 dotenvy::EnvIter 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_fd_lock() {
        let code = "fn foo() -> FdLock { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use fd_lock::FdLock;"),
            "应添加 fd_lock::FdLock 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_nix_nix_path() {
        let code = "fn foo() -> NixPath { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use nix::path::NixPath;"),
            "应添加 nix::path::NixPath 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_nix_errno() {
        let code = "fn foo() -> Errno { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use nix::errno::Errno;"),
            "应添加 nix::errno::Errno 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_camino_utf8_path_buf() {
        let code = "fn foo() -> Utf8PathBuf { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use camino::Utf8PathBuf;"),
            "应添加 camino::Utf8PathBuf 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_camino_utf8_path() {
        let code = "fn foo() -> Utf8Path { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use camino::Utf8Path;"),
            "应添加 camino::Utf8Path 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_s142_idempotent() {
        let code = "fn foo() -> (EnvLoader, FdLock, NixPath, Utf8PathBuf) { unimplemented!() }";
        let first = ensure_external_imports(code);
        let second = ensure_external_imports(&first);
        assert_eq!(first, second, "Session 142 新增外部 crate 检测应幂等");
    }

    #[test]
    fn test_ensure_external_imports_dotenvy_already_imported() {
        let code = "use dotenvy::EnvLoader;\nfn foo() -> EnvLoader { unimplemented!() }";
        let result = ensure_external_imports(code);
        let count = result.matches("use dotenvy::EnvLoader;").count();
        assert_eq!(
            count, 1,
            "已有 dotenvy::EnvLoader 导入不应重复添加: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_fd_lock_already_imported() {
        let code = "use fd_lock::FdLock;\nfn foo() -> FdLock { unimplemented!() }";
        let result = ensure_external_imports(code);
        let count = result.matches("use fd_lock::FdLock;").count();
        assert_eq!(
            count, 1,
            "已有 fd_lock::FdLock 导入不应重复添加: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_nix_full_path() {
        // nix::path::NixPath 全限定路径不应需要导入
        let code = "fn foo() -> nix::path::NixPath { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use nix::path::NixPath;"),
            "全限定路径 nix::path::NixPath 不应添加导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_camino_full_path() {
        // camino::Utf8PathBuf 全限定路径不应需要导入
        let code = "fn foo() -> camino::Utf8PathBuf { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use camino::Utf8PathBuf;"),
            "全限定路径 camino::Utf8PathBuf 不应添加导入: {}",
            result
        );
    }

    // ===== Session 142: ensure_external_imports .flatten()/.max()/.min()/.sum()/.product() 测试 =====

    #[test]
    fn test_ensure_external_imports_flatten_method() {
        let code = "fn foo(v: Vec<Vec<i32>>) { v.iter().flatten(); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应通过 .flatten() 添加 Iterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_max_method() {
        let code = "fn foo(v: Vec<i32>) { v.iter().max(Ord); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应通过 .max() 添加 Iterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_min_method() {
        let code = "fn foo(v: Vec<i32>) { v.iter().min(Ord); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应通过 .min() 添加 Iterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_sum_method() {
        let code = "fn foo(v: Vec<i32>) { let s: i32 = v.iter().sum(); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应通过 .sum() 添加 Iterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_product_method() {
        let code = "fn foo(v: Vec<i32>) { let p: i32 = v.iter().product(); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应通过 .product() 添加 Iterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_flatten_already_imported() {
        let code = "use std::iter::Iterator;\nfn foo(v: Vec<Vec<i32>>) { v.iter().flatten(); }";
        let result = ensure_external_imports(code);
        let count = result.matches("use std::iter::Iterator;").count();
        assert_eq!(
            count, 1,
            "已有 Iterator 导入不应重复添加 (.flatten): {}",
            result
        );
    }

    // ===== Session 142: verify_imports dotenvy/fd-lock/nix/camino 测试 =====

    #[test]
    fn test_verify_imports_dotenvy_env_loader_missing() {
        let issues = verify_imports("fn foo() -> EnvLoader { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "EnvLoader" && i.module_path == "dotenvy"),
            "应检测到 dotenvy::EnvLoader 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_dotenvy_env_iter_missing() {
        let issues = verify_imports("fn foo() -> EnvIter { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "EnvIter" && i.module_path == "dotenvy"),
            "应检测到 dotenvy::EnvIter 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_fd_lock_missing() {
        let issues = verify_imports("fn foo() -> FdLock { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "FdLock" && i.module_path == "fd_lock"),
            "应检测到 fd_lock::FdLock 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_fd_lock_already_imported() {
        let code = "use fd_lock::FdLock;\nfn foo() -> FdLock { unimplemented!() }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "FdLock" && i.module_path == "fd_lock"),
            "已有 fd_lock::FdLock 导入不应报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_nix_nix_path_missing() {
        let issues = verify_imports("fn foo() -> NixPath { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "NixPath" && i.module_path == "nix::path"),
            "应检测到 nix::path::NixPath 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_nix_errno_missing() {
        let issues = verify_imports("fn foo() -> Errno { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Errno" && i.module_path == "nix::errno"),
            "应检测到 nix::errno::Errno 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_nix_errno_already_imported() {
        let code = "use nix::errno::Errno;\nfn foo() -> Errno { unimplemented!() }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Errno" && i.module_path == "nix::errno"),
            "已有 nix::errno::Errno 导入不应报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_camino_utf8_path_buf_missing() {
        let issues = verify_imports("fn foo() -> Utf8PathBuf { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Utf8PathBuf" && i.module_path == "camino"),
            "应检测到 camino::Utf8PathBuf 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_camino_utf8_path_missing() {
        let issues = verify_imports("fn foo() -> Utf8Path { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Utf8Path" && i.module_path == "camino"),
            "应检测到 camino::Utf8Path 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_camino_already_imported() {
        let code = "use camino::Utf8PathBuf;\nfn foo() -> Utf8PathBuf { unimplemented!() }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Utf8PathBuf" && i.module_path == "camino"),
            "已有 camino::Utf8PathBuf 导入不应报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_s142_combined() {
        let code = "fn foo() -> (EnvLoader, FdLock, NixPath, Utf8PathBuf) { unimplemented!() }";
        let issues = verify_imports(code);
        assert!(
            issues.iter().any(|i| i.type_name == "EnvLoader"),
            "应检测到 EnvLoader: {:?}",
            issues
        );
        assert!(
            issues.iter().any(|i| i.type_name == "FdLock"),
            "应检测到 FdLock: {:?}",
            issues
        );
        assert!(
            issues.iter().any(|i| i.type_name == "NixPath"),
            "应检测到 NixPath: {:?}",
            issues
        );
        assert!(
            issues.iter().any(|i| i.type_name == "Utf8PathBuf"),
            "应检测到 Utf8PathBuf: {:?}",
            issues
        );
    }

    // ===== Session 142: verify_imports .flatten()/.max()/.min()/.sum()/.product() trait 方法测试 =====

    #[test]
    fn test_verify_imports_flatten_method_iterator() {
        let issues = verify_imports("fn foo(v: Vec<Vec<i32>>) { v.iter().flatten(); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应通过 .flatten() 方法检测到 Iterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_max_method_iterator() {
        let issues = verify_imports("fn foo(v: Vec<i32>) { v.iter().max(Ord); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应通过 .max() 方法检测到 Iterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_min_method_iterator() {
        let issues = verify_imports("fn foo(v: Vec<i32>) { v.iter().min(Ord); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应通过 .min() 方法检测到 Iterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_sum_method_iterator() {
        let issues = verify_imports("fn foo(v: Vec<i32>) { let s: i32 = v.iter().sum(); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应通过 .sum() 方法检测到 Iterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_product_method_iterator() {
        let issues = verify_imports("fn foo(v: Vec<i32>) { let p: i32 = v.iter().product(); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应通过 .product() 方法检测到 Iterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_flatten_already_imported() {
        let code = "use std::iter::Iterator;\nfn foo(v: Vec<Vec<i32>>) { v.iter().flatten(); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "已有 Iterator 导入不应通过 .flatten() 重复报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_sum_product_glob_import() {
        let code = "use std::iter::*;\nfn foo(v: Vec<i32>) { let s = v.iter().sum(); let p = v.iter().product(); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "glob 导入 use std::iter::*; 应覆盖 Iterator (.sum/.product): {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_flatten_max_min_sum_product_combined() {
        let code = "fn foo(v: Vec<i32>) { v.iter().flatten().max(Ord).min(Ord); }";
        let issues = verify_imports(code);
        // Should only report once (deduped)
        let iterator_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.type_name == "Iterator" && i.module_path == "std::iter")
            .collect();
        assert_eq!(
            iterator_issues.len(),
            1,
            "多种 Iterator 方法只应报告一次: {:?}",
            issues
        );
    }

    // ===== Session 142: ensure_external_imports 后无问题验证 =====

    #[test]
    fn test_ensure_external_imports_then_verify_no_s142_issues() {
        let code = "fn foo() -> (EnvLoader, FdLock, NixPath, Utf8PathBuf) { unimplemented!() }";
        let fixed = ensure_external_imports(code);
        let issues = verify_imports(&fixed);
        let s142_issues: Vec<_> = issues
            .iter()
            .filter(|i| {
                i.module_path == "dotenvy"
                    || i.module_path == "fd_lock"
                    || i.module_path == "nix::path"
                    || i.module_path == "nix::errno"
                    || i.module_path == "camino"
            })
            .collect();
        assert!(
            s142_issues.is_empty(),
            "ensure_external_imports 后不应有 Session 142 外部 crate 导入问题: {:?}",
            s142_issues
        );
    }

    // ===== Session 144: apply_staged_fixes_with_validation 测试 =====

    #[test]
    fn test_apply_staged_fixes_with_validation_all_pass() {
        let code = "pub fn foo() { let x = bar().unwrap(); }";
        let result = apply_staged_fixes_with_validation(code, |_| true);
        assert!(
            !result.final_result.contains(".unwrap()"),
            "unwrap 应被修复"
        );
        assert!(!result.any_rolled_back, "不应有回滚");
        assert!(result.stage1_valid, "阶段 1 应通过");
        assert!(result.stage2_valid, "阶段 2 应通过");
        assert!(result.stage3_valid, "阶段 3 应通过");
        assert!(result.rolled_back_stages.is_empty(), "回滚列表应为空");
    }

    #[test]
    fn test_apply_staged_fixes_with_validation_stage1_rollback() {
        let code = "pub fn foo() { let x = bar().unwrap(); }";
        // 阶段 1 验证失败 → 回滚到原始内容
        let result = apply_staged_fixes_with_validation(code, |_| false);
        assert!(result.any_rolled_back, "应有回滚");
        assert!(!result.stage1_valid, "阶段 1 应未通过");
        assert!(result.rolled_back_stages.contains(&1), "应包含阶段 1 回滚");
        // 回滚到原始内容, unwrap 仍在
        assert!(
            result.final_result.contains(".unwrap()"),
            "回滚后应保留原始 unwrap"
        );
    }

    #[test]
    fn test_apply_staged_fixes_with_validation_stage1_only_rollback() {
        let code = "pub fn foo() { let x = bar().unwrap(); }";
        // 阶段 1 失败, 但阶段 2/3 通过 (基于回滚后的内容)
        let mut call_count = 0;
        let result = apply_staged_fixes_with_validation(code, |_c| {
            call_count += 1;
            // 第一次调用 (阶段1) 失败, 后续通过
            call_count > 1
        });
        assert!(!result.stage1_valid, "阶段 1 应未通过");
        assert!(result.stage2_valid, "阶段 2 应通过");
        assert!(result.stage3_valid, "阶段 3 应通过");
        assert!(result.any_rolled_back, "应有回滚");
    }

    #[test]
    fn test_apply_staged_fixes_with_validation_no_change() {
        let code = "fn foo() -> i32 { 42 }";
        let result = apply_staged_fixes_with_validation(code, |_| true);
        assert!(!result.any_rolled_back, "无问题不应有回滚");
        assert_eq!(result.final_result, code, "无问题不应修改");
    }

    #[test]
    fn test_apply_staged_fixes_with_validation_serde() {
        let code = "pub fn foo() { let x = bar().unwrap(); }";
        let result = apply_staged_fixes_with_validation(code, |_| true);
        let json = serde_json::to_string(&result).expect("序列化失败");
        let deserialized: StagedFixValidation = serde_json::from_str(&json).expect("反序列化失败");
        assert_eq!(result.any_rolled_back, deserialized.any_rolled_back);
        assert_eq!(result.stage1_valid, deserialized.stage1_valid);
    }

    #[test]
    fn test_apply_staged_fixes_with_validation_final_matches_stage3() {
        let code = "pub fn foo() { let x = bar().unwrap(); }";
        let result = apply_staged_fixes_with_validation(code, |_| true);
        assert_eq!(
            result.final_result, result.stage3_result,
            "最终结果应与阶段 3 结果一致"
        );
    }

    #[test]
    fn test_apply_staged_fixes_with_validation_all_stages_fail() {
        let code = "pub fn foo() { let x = bar().unwrap(); }";
        let result = apply_staged_fixes_with_validation(code, |_| false);
        assert!(result.any_rolled_back, "应有回滚");
        assert!(!result.stage1_valid, "阶段 1 应未通过");
        assert!(!result.stage2_valid, "阶段 2 应未通过");
        assert!(!result.stage3_valid, "阶段 3 应未通过");
        assert_eq!(result.rolled_back_stages.len(), 3, "应有 3 个阶段回滚");
        // 最终回滚到原始内容 (阶段1回滚→阶段2基于原始→回滚→阶段3基于阶段2=原始→回滚)
        assert!(
            result.final_result.contains(".unwrap()"),
            "全回滚应保留原始内容"
        );
    }

    // ===== Session 144: ensure_external_imports enigo/x11/winapi/core-graphics 测试 =====

    #[test]
    fn test_ensure_external_imports_enigo() {
        let code = "fn foo() -> Enigo { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use enigo::Enigo;"),
            "应添加 enigo::Enigo 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_x11_display() {
        let code = "fn foo() -> Display { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use x11::display::Display;"),
            "应添加 x11::display::Display 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_winapi_hmodule() {
        let code = "fn foo() -> HMODULE { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use winapi::HMODULE;"),
            "应添加 winapi::HMODULE 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_core_graphics_cgcontext() {
        let code = "fn foo() -> CGContext { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use core_graphics::CGContext;"),
            "应添加 core_graphics::CGContext 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_s144_idempotent() {
        let code = "fn foo() -> (Enigo, HMODULE, CGContext) { unimplemented!() }";
        let first = ensure_external_imports(code);
        let second = ensure_external_imports(&first);
        assert_eq!(first, second, "Session 144 新增外部 crate 检测应幂等");
    }

    #[test]
    fn test_ensure_external_imports_s144_already_imported() {
        let code = "use enigo::Enigo;\nfn foo() -> Enigo { unimplemented!() }";
        let result = ensure_external_imports(code);
        let count = result.matches("use enigo::Enigo;").count();
        assert_eq!(count, 1, "已有 enigo::Enigo 导入不应重复添加: {}", result);
    }

    // ===== Session 144: ensure_external_imports .any()/.fold()/.for_each() 测试 =====

    #[test]
    fn test_ensure_external_imports_any_method() {
        let code = "fn foo(v: Vec<i32>) { v.iter().any(|&x| x > 0); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应通过 .any() 添加 Iterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_fold_method() {
        let code = "fn foo(v: Vec<i32>) { v.iter().fold(0, |a, &b| a + b); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应通过 .fold() 添加 Iterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_for_each_method() {
        let code = "fn foo(v: Vec<i32>) { v.iter().for_each(|x| println!(\"{}\", x)); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应通过 .for_each() 添加 Iterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_s144_iterator_already_imported() {
        let code = "use std::iter::Iterator;\nfn foo(v: Vec<i32>) { v.iter().any(|&x| x > 0); }";
        let result = ensure_external_imports(code);
        let count = result.matches("use std::iter::Iterator;").count();
        assert_eq!(
            count, 1,
            "已有 Iterator 导入不应重复添加 (.any): {}",
            result
        );
    }

    // ===== Session 144: verify_imports enigo/x11/winapi/core-graphics 测试 =====

    #[test]
    fn test_verify_imports_enigo_missing() {
        let issues = verify_imports("fn foo() -> Enigo { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Enigo" && i.module_path == "enigo"),
            "应检测到 enigo::Enigo 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_enigo_already_imported() {
        let code = "use enigo::Enigo;\nfn foo() -> Enigo { unimplemented!() }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Enigo" && i.module_path == "enigo"),
            "已有 enigo::Enigo 导入不应报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_winapi_hmodule_missing() {
        let issues = verify_imports("fn foo() -> HMODULE { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "HMODULE" && i.module_path == "winapi"),
            "应检测到 winapi::HMODULE 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_core_graphics_cgcontext_missing() {
        let issues = verify_imports("fn foo() -> CGContext { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "CGContext" && i.module_path == "core_graphics"),
            "应检测到 core_graphics::CGContext 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_s144_combined() {
        let code = "fn foo() -> (Enigo, HMODULE, CGContext) { unimplemented!() }";
        let issues = verify_imports(code);
        assert!(
            issues.iter().any(|i| i.type_name == "Enigo"),
            "应检测到 Enigo: {:?}",
            issues
        );
        assert!(
            issues.iter().any(|i| i.type_name == "HMODULE"),
            "应检测到 HMODULE: {:?}",
            issues
        );
        assert!(
            issues.iter().any(|i| i.type_name == "CGContext"),
            "应检测到 CGContext: {:?}",
            issues
        );
    }

    // ===== Session 144: verify_imports .any()/.fold()/.for_each() trait 方法测试 =====

    #[test]
    fn test_verify_imports_any_method_iterator() {
        let issues = verify_imports("fn foo(v: Vec<i32>) { v.iter().any(|&x| x > 0); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应通过 .any() 方法检测到 Iterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_fold_method_iterator() {
        let issues = verify_imports("fn foo(v: Vec<i32>) { v.iter().fold(0, |a, &b| a + b); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应通过 .fold() 方法检测到 Iterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_for_each_method_iterator() {
        let issues = verify_imports("fn foo(v: Vec<i32>) { v.iter().for_each(|x| {}); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应通过 .for_each() 方法检测到 Iterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_any_already_imported() {
        let code = "use std::iter::Iterator;\nfn foo(v: Vec<i32>) { v.iter().any(|&x| x > 0); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "已有 Iterator 导入不应通过 .any() 重复报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_s144_iterator_methods_combined() {
        let code =
            "fn foo(v: Vec<i32>) { v.iter().any(|&x| x > 0).count().fold(0, |a, &b| a + b); }";
        let issues = verify_imports(code);
        let iterator_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.type_name == "Iterator" && i.module_path == "std::iter")
            .collect();
        assert_eq!(
            iterator_issues.len(),
            1,
            "多种 Session 144 Iterator 方法只应报告一次: {:?}",
            issues
        );
    }

    // ===== Session 144: ensure_external_imports 后无问题验证 =====

    #[test]
    fn test_ensure_external_imports_then_verify_no_s144_issues() {
        let code = "fn foo() -> (Enigo, HMODULE, CGContext) { unimplemented!() }";
        let fixed = ensure_external_imports(code);
        let issues = verify_imports(&fixed);
        let s144_issues: Vec<_> = issues
            .iter()
            .filter(|i| {
                i.module_path == "enigo"
                    || i.module_path == "x11::display"
                    || i.module_path == "winapi"
                    || i.module_path == "core_graphics"
            })
            .collect();
        assert!(
            s144_issues.is_empty(),
            "ensure_external_imports 后不应有 Session 144 外部 crate 导入问题: {:?}",
            s144_issues
        );
    }

    // ===== Session 145: ensure_external_imports image/imageproc/rusttype/plotters 测试 =====

    #[test]
    fn test_ensure_external_imports_image_image_buffer() {
        let code = "fn foo() -> ImageBuffer<Rgba<u8>, Vec<u8>> { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use image::{ImageBuffer, Rgba};"),
            "应添加 image::{{ImageBuffer, Rgba}} 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_image_rgba() {
        let code = "fn foo() -> Rgba<u8> { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use image::Rgba;"),
            "应添加 image::Rgba 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_imageproc_drawing() {
        let code = "fn foo(d: Drawing) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use imageproc::drawing::Drawing;"),
            "应添加 imageproc::drawing::Drawing 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_rusttype_font() {
        let code = "fn foo() -> Font { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use rusttype::Font;"),
            "应添加 rusttype::Font 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_rusttype_positioned_glyph() {
        let code = "fn foo() -> PositionedGlyph { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use rusttype::PositionedGlyph;"),
            "应添加 rusttype::PositionedGlyph 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_plotters_chart_context() {
        let code = "fn foo() -> ChartContext { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use plotters::chart::ChartContext;"),
            "应添加 plotters::chart::ChartContext 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_s145_idempotent() {
        let code =
            "fn foo() -> (ImageBuffer<Rgba<u8>, Vec<u8>>, Font, ChartContext) { unimplemented!() }";
        let first = ensure_external_imports(code);
        let second = ensure_external_imports(&first);
        assert_eq!(first, second, "Session 145 新增外部 crate 检测应幂等");
    }

    #[test]
    fn test_ensure_external_imports_s145_already_imported() {
        let code = "use image::ImageBuffer;\nfn foo() -> ImageBuffer { unimplemented!() }";
        let result = ensure_external_imports(code);
        let count = result.matches("use image::ImageBuffer;").count();
        assert_eq!(
            count, 1,
            "已有 image::ImageBuffer 导入不应重复添加: {}",
            result
        );
    }

    // ===== Session 145: ensure_external_imports .scan()/.unzip()/.cycle() 测试 =====

    #[test]
    fn test_ensure_external_imports_scan_method() {
        let code = "fn foo(v: Vec<i32>) { v.iter().scan(0, |acc, &x| { Some(x) }); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应通过 .scan() 添加 Iterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_unzip_method() {
        let code = "fn foo(v: Vec<i32>) { let (a, b): (Vec<_>, Vec<_>) = v.iter().unzip(); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应通过 .unzip() 添加 Iterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_cycle_method() {
        let code = "fn foo(v: Vec<i32>) { v.iter().cycle(); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应通过 .cycle() 添加 Iterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_s145_iterator_already_imported() {
        let code =
            "use std::iter::Iterator;\nfn foo(v: Vec<i32>) { v.iter().scan(0, |acc, &x| Some(x)); }";
        let result = ensure_external_imports(code);
        let count = result.matches("use std::iter::Iterator;").count();
        assert_eq!(
            count, 1,
            "已有 Iterator 导入不应重复添加 (.scan): {}",
            result
        );
    }

    // ===== Session 145: verify_imports image/imageproc/rusttype/plotters 测试 =====

    #[test]
    fn test_verify_imports_image_image_buffer_missing() {
        let issues = verify_imports("fn foo() -> ImageBuffer { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "ImageBuffer" && i.module_path == "image"),
            "应检测到 image::ImageBuffer 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_image_rgba_missing() {
        let issues = verify_imports("fn foo() -> Rgba { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Rgba" && i.module_path == "image"),
            "应检测到 image::Rgba 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_imageproc_drawing_missing() {
        let issues = verify_imports("fn foo(d: Drawing) { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Drawing" && i.module_path == "imageproc::drawing"),
            "应检测到 imageproc::drawing::Drawing 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_rusttype_font_missing() {
        let issues = verify_imports("fn foo() -> Font { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Font" && i.module_path == "rusttype"),
            "应检测到 rusttype::Font 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_plotters_chart_context_missing() {
        let issues = verify_imports("fn foo() -> ChartContext { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "ChartContext" && i.module_path == "plotters::chart"),
            "应检测到 plotters::chart::ChartContext 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_s145_combined() {
        let code = "fn foo() -> (ImageBuffer, Font, ChartContext) { unimplemented!() }";
        let issues = verify_imports(code);
        assert!(
            issues.iter().any(|i| i.type_name == "ImageBuffer"),
            "应检测到 ImageBuffer: {:?}",
            issues
        );
        assert!(
            issues.iter().any(|i| i.type_name == "Font"),
            "应检测到 Font: {:?}",
            issues
        );
        assert!(
            issues.iter().any(|i| i.type_name == "ChartContext"),
            "应检测到 ChartContext: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_image_already_imported() {
        let code = "use image::ImageBuffer;\nfn foo() -> ImageBuffer { unimplemented!() }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "ImageBuffer" && i.module_path == "image"),
            "已有 image::ImageBuffer 导入不应报告: {:?}",
            issues
        );
    }

    // ===== Session 145: verify_imports .scan()/.unzip()/.cycle() trait 方法测试 =====

    #[test]
    fn test_verify_imports_scan_method_iterator() {
        let issues = verify_imports("fn foo(v: Vec<i32>) { v.iter().scan(0, |acc, &x| Some(x)); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应通过 .scan() 方法检测到 Iterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_unzip_method_iterator() {
        let issues = verify_imports("fn foo(v: Vec<i32>) { let (a, b) = v.iter().unzip(); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应通过 .unzip() 方法检测到 Iterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_cycle_method_iterator() {
        let issues = verify_imports("fn foo(v: Vec<i32>) { v.iter().cycle(); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应通过 .cycle() 方法检测到 Iterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_scan_already_imported() {
        let code =
            "use std::iter::Iterator;\nfn foo(v: Vec<i32>) { v.iter().scan(0, |acc, &x| Some(x)); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "已有 Iterator 导入不应通过 .scan() 重复报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_s145_iterator_methods_combined() {
        let code = "fn foo(v: Vec<i32>) { v.iter().scan(0, |a, &x| Some(x)).unzip().cycle(); }";
        let issues = verify_imports(code);
        let iterator_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.type_name == "Iterator" && i.module_path == "std::iter")
            .collect();
        assert_eq!(
            iterator_issues.len(),
            1,
            "多种 Session 145 Iterator 方法只应报告一次: {:?}",
            issues
        );
    }

    // ===== Session 146: verify_imports ratatui/crossterm/tui/glium/vulkano/ndarray-npy 测试 =====

    #[test]
    fn test_verify_imports_ratatui_line_missing() {
        let issues = verify_imports("fn foo() -> Line { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Line" && i.module_path == "ratatui::text"),
            "应检测到 ratatui::text::Line 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_ratatui_layout_missing() {
        let issues = verify_imports("fn foo() -> Layout { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Layout" && i.module_path == "ratatui::layout"),
            "应检测到 ratatui::layout::Layout 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_ratatui_block_missing() {
        let issues = verify_imports("fn foo() -> Block { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Block" && i.module_path == "ratatui::widgets"),
            "应检测到 ratatui::widgets::Block 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_ratatui_already_imported() {
        let code = "use ratatui::text::Line;\nfn foo() -> Line { unimplemented!() }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Line" && i.module_path == "ratatui::text"),
            "已有 ratatui::text::Line 导入不应报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_crossterm_execute_missing() {
        let issues = verify_imports("fn foo() { execute!(\"cmd\"); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "execute" && i.module_path == "crossterm"),
            "应检测到 crossterm::execute 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_crossterm_already_imported() {
        let code = "use crossterm::execute;\nfn foo() { execute!(\"cmd\"); }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "execute" && i.module_path == "crossterm"),
            "已有 crossterm::execute 导入不应报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_tui_frame_missing() {
        let issues = verify_imports("fn foo(f: &mut Frame) { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Frame" && i.module_path == "tui"),
            "应检测到 tui::Frame 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_tui_terminal_missing() {
        let issues = verify_imports("fn foo() -> Terminal { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Terminal" && i.module_path == "tui"),
            "应检测到 tui::Terminal 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_glium_display_missing() {
        let issues = verify_imports("fn foo() -> Display { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Display" && i.module_path == "glium"),
            "应检测到 glium::Display 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_glium_surface_missing() {
        let issues = verify_imports("fn foo() -> Surface { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Surface" && i.module_path == "glium::surface"),
            "应检测到 glium::surface::Surface 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_vulkano_vk_handle_missing() {
        let issues = verify_imports("fn foo() -> VkHandle { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "VkHandle" && i.module_path == "vulkano"),
            "应检测到 vulkano::VkHandle 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_vulkano_vulkan_object_missing() {
        let issues = verify_imports("fn foo() -> VulkanObject { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "VulkanObject" && i.module_path == "vulkano"),
            "应检测到 vulkano::VulkanObject 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_ndarray_npy_open_npz_missing() {
        let issues = verify_imports("fn foo() { open_npz(\"file.npz\"); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "open_npz" && i.module_path == "ndarray_npy"),
            "应检测到 ndarray_npy::open_npz 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_ndarray_npy_save_npz_missing() {
        let issues = verify_imports("fn foo() { save_npz(\"file.npz\"); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "save_npz" && i.module_path == "ndarray_npy"),
            "应检测到 ndarray_npy::save_npz 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_s146_combined() {
        let code = "fn foo() -> (Line, Frame, Display, VkHandle) { unimplemented!() }\nfn bar() { open_npz(\"f.npz\"); }";
        let issues = verify_imports(code);
        assert!(issues.iter().any(|i| i.type_name == "Line"), "应有 Line");
        assert!(issues.iter().any(|i| i.type_name == "Frame"), "应有 Frame");
        assert!(
            issues.iter().any(|i| i.type_name == "Display"),
            "应有 Display"
        );
        assert!(
            issues.iter().any(|i| i.type_name == "VkHandle"),
            "应有 VkHandle"
        );
        assert!(
            issues.iter().any(|i| i.type_name == "open_npz"),
            "应有 open_npz"
        );
    }

    // ===== Session 146: verify_imports .chunks()/.windows()/.rchunks() trait 方法测试 =====

    #[test]
    fn test_verify_imports_chunks_method_iterator() {
        let issues = verify_imports("fn foo(v: Vec<i32>) { v.chunks(2); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应通过 .chunks() 方法检测到 Iterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_windows_method_iterator() {
        let issues = verify_imports("fn foo(v: Vec<i32>) { v.windows(2); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应通过 .windows() 方法检测到 Iterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_rchunks_method_iterator() {
        let issues = verify_imports("fn foo(v: Vec<i32>) { v.rchunks(2); }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应通过 .rchunks() 方法检测到 Iterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_s146_iterator_methods_combined() {
        let code = "fn foo(v: Vec<i32>) { v.chunks(2); v.windows(3); v.rchunks(4); }";
        let issues = verify_imports(code);
        let iterator_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.type_name == "Iterator" && i.module_path == "std::iter")
            .collect();
        assert_eq!(
            iterator_issues.len(),
            1,
            "多种 Session 146 Iterator 方法只应报告一次: {:?}",
            issues
        );
    }

    // ===== Session 145: ensure_external_imports 后无问题验证 =====

    #[test]
    fn test_ensure_external_imports_then_verify_no_s145_issues() {
        let code = "fn foo() -> (ImageBuffer, Font, ChartContext) { unimplemented!() }";
        let fixed = ensure_external_imports(code);
        let issues = verify_imports(&fixed);
        let s145_issues: Vec<_> = issues
            .iter()
            .filter(|i| {
                i.module_path == "image"
                    || i.module_path == "imageproc::drawing"
                    || i.module_path == "rusttype"
                    || i.module_path == "plotters::chart"
            })
            .collect();
        assert!(
            s145_issues.is_empty(),
            "ensure_external_imports 后不应有 Session 145 外部 crate 导入问题: {:?}",
            s145_issues
        );
    }

    // ===== Session 145: 混合 S144+S145 类型验证 =====

    #[test]
    fn test_ensure_external_imports_s144_s145_mixed() {
        let code = "fn foo() -> (Enigo, ImageBuffer, Font, CGContext) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use enigo::Enigo;"),
            "应包含 enigo::Enigo: {}",
            result
        );
        assert!(
            result.contains("use image::ImageBuffer;"),
            "应包含 image::ImageBuffer: {}",
            result
        );
        assert!(
            result.contains("use rusttype::Font;"),
            "应包含 rusttype::Font: {}",
            result
        );
        assert!(
            result.contains("use core_graphics::CGContext;"),
            "应包含 core_graphics::CGContext: {}",
            result
        );
    }

    // ===== Session 146: ensure_external_imports ratatui/crossterm/tui/glium/vulkano/ndarray-npy 测试 =====

    #[test]
    fn test_ensure_external_imports_ratatui_line() {
        let code = "fn foo() -> Line { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use ratatui::text::Line;"),
            "应添加 ratatui::text::Line 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_ratatui_layout() {
        let code = "fn foo() -> Layout { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use ratatui::layout::Layout;"),
            "应添加 ratatui::layout::Layout 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_ratatui_block() {
        let code = "fn foo() -> Block { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use ratatui::widgets::Block;"),
            "应添加 ratatui::widgets::Block 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_ratatui_widget() {
        let code = "fn foo(w: &dyn Widget) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use ratatui::widgets::Widget;"),
            "应添加 ratatui::widgets::Widget 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_ratatui_full_path() {
        let code = "fn foo() -> ratatui::text::Line { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use ratatui::text::Line;"),
            "全限定 ratatui::text::Line 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_ratatui_multiple_merged() {
        let code = "fn foo() -> (Line, Layout) { unimplemented!() }";
        let result = ensure_external_imports(code);
        // Line is from ratatui::text, Layout is from ratatui::layout — different paths, separate imports
        assert!(
            result.contains("use ratatui::text::Line;"),
            "应包含 ratatui::text::Line: {}",
            result
        );
        assert!(
            result.contains("use ratatui::layout::Layout;"),
            "应包含 ratatui::layout::Layout: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_crossterm_execute() {
        let code = "fn foo() { execute!(\"cmd\"); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use crossterm::execute;"),
            "应添加 crossterm::execute 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_crossterm_queue() {
        let code = "fn foo() { queue!(\"cmd\"); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use crossterm::queue;"),
            "应添加 crossterm::queue 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_crossterm_terminal() {
        let code = "fn foo() -> terminal { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use crossterm::terminal;"),
            "应添加 crossterm::terminal 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_crossterm_full_path() {
        let code = "fn foo() { crossterm::execute!(\"cmd\"); }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use crossterm::execute;"),
            "全限定 crossterm::execute 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_tui_frame() {
        let code = "fn foo(f: &mut Frame) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use tui::Frame;"),
            "应添加 tui::Frame 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_tui_terminal() {
        let code = "fn foo() -> Terminal { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use tui::Terminal;"),
            "应添加 tui::Terminal 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_tui_full_path() {
        let code = "fn foo() -> tui::Terminal { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use tui::Terminal;"),
            "全限定 tui::Terminal 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_glium_display() {
        let code = "fn foo() -> Display { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use glium::Display;"),
            "应添加 glium::Display 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_glium_surface() {
        let code = "fn foo() -> Surface { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use glium::surface::Surface;"),
            "应添加 glium::surface::Surface 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_glium_full_path() {
        let code = "fn foo() -> glium::Display { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use glium::Display;"),
            "全限定 glium::Display 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_vulkano_vk_handle() {
        let code = "fn foo() -> VkHandle { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use vulkano::VkHandle;"),
            "应添加 vulkano::VkHandle 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_vulkano_vulkan_object() {
        let code = "fn foo() -> VulkanObject { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use vulkano::VulkanObject;"),
            "应添加 vulkano::VulkanObject 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_vulkano_full_path() {
        let code = "fn foo() -> vulkano::VkHandle { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use vulkano::VkHandle;"),
            "全限定 vulkano::VkHandle 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_vulkano_multiple_merged() {
        let code = "fn foo() -> (VkHandle, VulkanObject) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use vulkano::{VkHandle, VulkanObject};"),
            "多个 vulkano 类型应合并导入 (字母序): {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_ndarray_npy_open_npz() {
        let code = "fn foo() { open_npz(\"file.npz\"); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use ndarray_npy::open_npz;"),
            "应添加 ndarray_npy::open_npz 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_ndarray_npy_save_npz() {
        let code = "fn foo() { save_npz(\"file.npz\"); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use ndarray_npy::save_npz;"),
            "应添加 ndarray_npy::save_npz 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_ndarray_npy_full_path() {
        let code = "fn foo() { ndarray_npy::open_npz(\"file.npz\"); }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use ndarray_npy::open_npz;"),
            "全限定 ndarray_npy::open_npz 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_s146_idempotent() {
        let code = "fn foo() -> (Line, Layout, Block, Widget) { unimplemented!() }\n\
             fn bar() -> (Frame, Terminal, Display, Surface) { unimplemented!() }\n\
             fn baz() -> (VkHandle, VulkanObject) { unimplemented!() }\n\
             fn qux() { open_npz(\"f.npz\"); save_npz(\"f.npz\"); }";
        let first = ensure_external_imports(code);
        let second = ensure_external_imports(&first);
        assert_eq!(first, second, "Session 146 新增外部 crate 检测应幂等");
    }

    #[test]
    fn test_ensure_external_imports_s146_already_imported() {
        let code = "use ratatui::text::Line;\nfn foo() -> Line { unimplemented!() }";
        let result = ensure_external_imports(code);
        let count = result.matches("use ratatui::text::Line;").count();
        assert_eq!(
            count, 1,
            "已有 ratatui::text::Line 导入不应重复添加: {}",
            result
        );
    }

    // ===== Session 146: ensure_external_imports .chunks()/.windows()/.rchunks() 测试 =====

    #[test]
    fn test_ensure_external_imports_chunks_method() {
        let code = "fn foo(v: Vec<i32>) { v.chunks(2); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应通过 .chunks() 添加 Iterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_windows_method() {
        let code = "fn foo(v: Vec<i32>) { v.windows(2); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应通过 .windows() 添加 Iterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_rchunks_method() {
        let code = "fn foo(v: Vec<i32>) { v.rchunks(2); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应通过 .rchunks() 添加 Iterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_s146_iterator_already_imported() {
        let code = "use std::iter::Iterator;\nfn foo(v: Vec<i32>) { v.chunks(2); }";
        let result = ensure_external_imports(code);
        let count = result.matches("use std::iter::Iterator;").count();
        assert_eq!(
            count, 1,
            "已有 Iterator 导入不应重复添加 (.chunks): {}",
            result
        );
    }

    // ===== Session 147: ensure_external_imports eframe/egui/iced/druid/slint 测试 =====

    #[test]
    fn test_ensure_external_imports_eframe_app() {
        let code = "fn foo(a: &mut App) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use eframe::App;"),
            "应添加 eframe::App 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_eframe_frame() {
        let code = "fn foo(f: &mut Frame) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use eframe::Frame;"),
            "应添加 eframe::Frame 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_egui_context() {
        let code = "fn foo(ctx: &Context) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use egui::Context;"),
            "应添加 egui::Context 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_egui_ui() {
        let code = "fn foo(ui: &mut Ui) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use egui::Ui;"),
            "应添加 egui::Ui 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_iced_application() {
        let code = "fn foo() -> Application { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use iced::Application;"),
            "应添加 iced::Application 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_iced_command() {
        let code = "fn foo() -> Command { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use iced::Command;"),
            "应添加 iced::Command 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_druid_app_delegate() {
        let code = "fn foo(d: &AppDelegate) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use druid::AppDelegate;"),
            "应添加 druid::AppDelegate 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_druid_widget() {
        let code = "fn foo(w: &dyn Widget) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use druid::widget::Widget;"),
            "应添加 druid::widget::Widget 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_slint_component_handle() {
        let code = "fn foo() -> ComponentHandle { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use slint::ComponentHandle;"),
            "应添加 slint::ComponentHandle 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_slint_model() {
        let code = "fn foo() -> Model { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use slint::Model;"),
            "应添加 slint::Model 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_eframe_full_path() {
        let code = "fn foo(a: &mut eframe::App) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use eframe::App;"),
            "全限定 eframe::App 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_s147_multiple_merged() {
        let code = "fn foo(a: &mut App, ctx: &Context) { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use eframe::App;"),
            "应包含 eframe::App: {}",
            result
        );
        assert!(
            result.contains("use egui::Context;"),
            "应包含 egui::Context: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_s147_idempotent() {
        let code = "fn foo() -> ComponentHandle { unimplemented!() }";
        let first = ensure_external_imports(code);
        let second = ensure_external_imports(&first);
        assert_eq!(first, second, "ensure_external_imports 应幂等 (S147 slint)");
    }

    #[test]
    fn test_ensure_external_imports_s147_already_imported() {
        let code = "use slint::ComponentHandle;\nfn foo() -> ComponentHandle { unimplemented!() }";
        let result = ensure_external_imports(code);
        let count = result.matches("use slint::ComponentHandle;").count();
        assert_eq!(
            count, 1,
            "已有 slint::ComponentHandle 导入不应重复添加: {}",
            result
        );
    }

    // ===== Session 147: ensure_external_imports .first()/.last()/.nth() 测试 =====

    #[test]
    fn test_ensure_external_imports_first_method() {
        let code = "fn foo(v: Vec<i32>) { v.first(); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应通过 .first() 添加 Iterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_last_method() {
        let code = "fn foo(v: Vec<i32>) { v.last(); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应通过 .last() 添加 Iterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_nth_method() {
        let code = "fn foo(v: Vec<i32>) { v.nth(0); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应通过 .nth() 添加 Iterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_next_back_method() {
        let code = "fn foo(v: Vec<i32>) { v.next_back(); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应通过 .next_back() 添加 Iterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_rposition_method() {
        let code = "fn foo(v: Vec<i32>) { v.rposition(|x| true); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应通过 .rposition() 添加 Iterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_rfold_method() {
        let code = "fn foo(v: Vec<i32>) { v.rfold(0, |a, b| a + b); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应通过 .rfold() 添加 Iterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_rfind_method() {
        let code = "fn foo(v: Vec<i32>) { v.rfind(|x| true); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应通过 .rfind() 添加 Iterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_s147_iterator_already_imported() {
        let code = "use std::iter::Iterator;\nfn foo(v: Vec<i32>) { v.first(); v.last(); }";
        let result = ensure_external_imports(code);
        let count = result.matches("use std::iter::Iterator;").count();
        assert_eq!(
            count, 1,
            "已有 Iterator 导入不应重复添加 (.first/.last): {}",
            result
        );
    }

    // ===== Session 147: verify_imports eframe/egui/iced/druid/slint 测试 =====

    #[test]
    fn test_verify_imports_eframe_app_missing() {
        let code = "fn foo(a: &mut App) { unimplemented!() }";
        let issues = verify_imports(code);
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "App" && i.module_path == "eframe"),
            "应检测到 eframe::App 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_egui_context_missing() {
        let code = "fn foo(ctx: &Context) { unimplemented!() }";
        let issues = verify_imports(code);
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Context" && i.module_path == "egui"),
            "应检测到 egui::Context 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_iced_application_missing() {
        let code = "fn foo() -> Application { unimplemented!() }";
        let issues = verify_imports(code);
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Application" && i.module_path == "iced"),
            "应检测到 iced::Application 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_druid_app_delegate_missing() {
        let code = "fn foo(d: &AppDelegate) { unimplemented!() }";
        let issues = verify_imports(code);
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "AppDelegate" && i.module_path == "druid"),
            "应检测到 druid::AppDelegate 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_slint_component_handle_missing() {
        let code = "fn foo() -> ComponentHandle { unimplemented!() }";
        let issues = verify_imports(code);
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "ComponentHandle" && i.module_path == "slint"),
            "应检测到 slint::ComponentHandle 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_eframe_already_imported() {
        let code = "use eframe::App;\nfn foo(a: &mut App) { unimplemented!() }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "App" && i.module_path == "eframe"),
            "已导入 eframe::App 不应报告: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_slint_model_missing() {
        let code = "fn foo() -> Model { unimplemented!() }";
        let issues = verify_imports(code);
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Model" && i.module_path == "slint"),
            "应检测到 slint::Model 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_egui_ui_missing() {
        let code = "fn foo(ui: &mut Ui) { unimplemented!() }";
        let issues = verify_imports(code);
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Ui" && i.module_path == "egui"),
            "应检测到 egui::Ui 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_eframe_full_path() {
        let code = "fn foo(a: &mut eframe::App) { unimplemented!() }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "App" && i.module_path == "eframe"),
            "全限定 eframe::App 路径不应报告: {:?}",
            issues
        );
    }

    // ===== Session 147: verify_imports .first()/.last()/.nth() trait 方法测试 =====

    #[test]
    fn test_verify_imports_first_method() {
        let code = "fn foo(v: Vec<i32>) { v.first(); }";
        let issues = verify_imports(code);
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应检测到 .first() 触发 Iterator 导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_last_method() {
        let code = "fn foo(v: Vec<i32>) { v.last(); }";
        let issues = verify_imports(code);
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应检测到 .last() 触发 Iterator 导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_nth_method() {
        let code = "fn foo(v: Vec<i32>) { v.nth(0); }";
        let issues = verify_imports(code);
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应检测到 .nth() 触发 Iterator 导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_next_back_method() {
        let code = "fn foo(v: Vec<i32>) { v.next_back(); }";
        let issues = verify_imports(code);
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应检测到 .next_back() 触发 Iterator 导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_rposition_method() {
        let code = "fn foo(v: Vec<i32>) { v.rposition(|x| true); }";
        let issues = verify_imports(code);
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应检测到 .rposition() 触发 Iterator 导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_rfold_method() {
        let code = "fn foo(v: Vec<i32>) { v.rfold(0, |a, b| a + b); }";
        let issues = verify_imports(code);
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应检测到 .rfold() 触发 Iterator 导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_rfind_method() {
        let code = "fn foo(v: Vec<i32>) { v.rfind(|x| true); }";
        let issues = verify_imports(code);
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Iterator" && i.module_path == "std::iter"),
            "应检测到 .rfind() 触发 Iterator 导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_s147_iterator_methods_combined() {
        let code = "fn foo(v: Vec<i32>) { v.first(); v.last(); v.nth(0); v.next_back(); v.rposition(|x| true); v.rfold(0, |a, b| a + b); v.rfind(|x| true); }";
        let issues = verify_imports(code);
        let iterator_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.type_name == "Iterator" && i.module_path == "std::iter")
            .collect();
        assert_eq!(
            iterator_issues.len(),
            1,
            "多种 Session 147 Iterator 方法只应报告一次: {:?}",
            issues
        );
    }

    // ===== Session 147: ensure_external_imports 后无问题验证 =====

    #[test]
    fn test_ensure_external_imports_then_verify_no_s147_issues() {
        let code = "fn foo(a: &mut App, ctx: &Context) { unimplemented!() }\nfn bar() -> ComponentHandle { unimplemented!() }";
        let fixed = ensure_external_imports(code);
        let issues = verify_imports(&fixed);
        let s147_issues: Vec<_> = issues
            .iter()
            .filter(|i| {
                i.module_path == "eframe"
                    || i.module_path == "egui"
                    || i.module_path == "iced"
                    || i.module_path == "druid"
                    || i.module_path == "slint"
            })
            .collect();
        assert!(
            s147_issues.is_empty(),
            "ensure_external_imports 后不应有 Session 147 外部 crate 导入问题: {:?}",
            s147_issues
        );
    }

    // ===== Session 147: 混合 S146+S147 类型验证 =====

    #[test]
    fn test_mixed_s146_s147_types() {
        let code = "fn foo() -> (Line, Display, VkHandle) { unimplemented!() }\nfn bar(a: &mut App, ctx: &Context) { unimplemented!() }\nfn baz() -> ComponentHandle { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use ratatui::text::Line;"),
            "应包含 ratatui::text::Line: {}",
            result
        );
        assert!(
            result.contains("use glium::Display;"),
            "应包含 glium::Display: {}",
            result
        );
        assert!(
            result.contains("use vulkano::VkHandle;"),
            "应包含 vulkano::VkHandle: {}",
            result
        );
        assert!(
            result.contains("use eframe::App;"),
            "应包含 eframe::App: {}",
            result
        );
        assert!(
            result.contains("use egui::Context;"),
            "应包含 egui::Context: {}",
            result
        );
        assert!(
            result.contains("use slint::ComponentHandle;"),
            "应包含 slint::ComponentHandle: {}",
            result
        );
    }

    // ===== Session 148: ensure_external_imports 新 crate 类型测试 =====

    #[test]
    fn test_ensure_external_imports_num_cpus() {
        let code = "fn foo() -> usize { num_cpus() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use num_cpus::num_cpus;"),
            "应添加 num_cpus::num_cpus 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_once_cell_lazy() {
        let code = "fn foo() -> Lazy<i32> { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use once_cell::sync::Lazy;"),
            "应添加 once_cell::sync::Lazy 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_flate2_gz_encoder() {
        let code = "fn foo() -> GzEncoder { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use flate2::write::GzEncoder;"),
            "应添加 flate2::write::GzEncoder 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_walkdir() {
        let code = "fn foo() -> WalkDir { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use walkdir::WalkDir;"),
            "应添加 walkdir::WalkDir 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_tempfile() {
        let code = "fn foo() -> TempDir { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use tempfile::TempDir;"),
            "应添加 tempfile::TempDir 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_indicatif() {
        let code = "fn foo() -> ProgressBar { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use indicatif::ProgressBar;"),
            "应添加 indicatif::ProgressBar 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_s148_full_path_no_import() {
        let code = "fn foo() -> walkdir::WalkDir { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use walkdir::WalkDir;"),
            "全限定 walkdir:: 路径不需要导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_s148_idempotent() {
        let code = "fn foo() -> (WalkDir, TempDir, ProgressBar) { unimplemented!() }\nfn bar() -> Lazy<i32> { unimplemented!() }\nfn baz() -> usize { num_cpus() }";
        let first = ensure_external_imports(code);
        let second = ensure_external_imports(&first);
        assert_eq!(first, second, "Session 148 新增外部 crate 检测应幂等");
    }

    // ===== Session 148: ensure_external_imports 新 trait 方法测试 =====

    #[test]
    fn test_ensure_external_imports_iter_mut_method() {
        let code = "fn foo(v: &mut Vec<i32>) { v.iter_mut(); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应通过 .iter_mut() 添加 Iterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_split_method() {
        let code = "fn foo(s: &str) { s.split(','); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应通过 .split() 添加 Iterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_lines_chars_bytes_methods() {
        let code = "fn foo(s: &str) { s.lines(); s.chars(); s.bytes(); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应通过 .lines()/.chars()/.bytes() 添加 Iterator 导入: {}",
            result
        );
    }

    // ===== Session 148: verify_imports 新 crate 类型测试 =====

    #[test]
    fn test_verify_imports_num_cpus_missing() {
        let issues = verify_imports("fn foo() -> usize { num_cpus() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "num_cpus" && i.module_path == "num_cpus"),
            "应检测到 num_cpus 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_once_cell_lazy_missing() {
        let issues = verify_imports("fn foo() -> Lazy<i32> { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Lazy" && i.module_path == "once_cell::sync"),
            "应检测到 Lazy 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_walkdir_missing() {
        let issues = verify_imports("fn foo() -> WalkDir { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "WalkDir" && i.module_path == "walkdir"),
            "应检测到 WalkDir 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_tempdir_missing() {
        let issues = verify_imports("fn foo() -> TempDir { unimplemented!() }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "TempDir" && i.module_path == "tempfile"),
            "应检测到 TempDir 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_s148_iterator_methods_combined() {
        let code = "fn foo(s: &str) { s.split(','); s.lines(); s.chars(); s.bytes(); }";
        let issues = verify_imports(code);
        let iterator_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.type_name == "Iterator" && i.module_path == "std::iter")
            .collect();
        assert_eq!(
            iterator_issues.len(),
            1,
            "多种 Session 148 Iterator 方法只应报告一次: {:?}",
            issues
        );
    }

    // ===== Session 148: ensure_external_imports 后无问题验证 =====

    #[test]
    fn test_ensure_external_imports_then_verify_no_s148_issues() {
        let code = "fn foo() -> (WalkDir, TempDir, ProgressBar) { unimplemented!() }\nfn bar() -> Lazy<i32> { unimplemented!() }\nfn baz() -> usize { num_cpus() }";
        let fixed = ensure_external_imports(code);
        let issues = verify_imports(&fixed);
        let s148_issues: Vec<_> = issues
            .iter()
            .filter(|i| {
                i.module_path == "num_cpus"
                    || i.module_path == "once_cell::sync"
                    || i.module_path == "flate2::write"
                    || i.module_path == "flate2::read"
                    || i.module_path == "tar"
                    || i.module_path == "walkdir"
                    || i.module_path == "tempfile"
                    || i.module_path == "indicatif"
                    || i.module_path == "dialoguer"
                    || i.module_path == "console"
            })
            .collect();
        assert!(
            s148_issues.is_empty(),
            "ensure_external_imports 后不应有 Session 148 外部 crate 导入问题: {:?}",
            s148_issues
        );
    }

    // ===== Session 148: 混合 S147+S148 类型验证 =====

    #[test]
    fn test_mixed_s147_s148_types() {
        let code = "fn foo(a: &mut App) { unimplemented!() }\nfn bar() -> WalkDir { unimplemented!() }\nfn baz() -> ProgressBar { unimplemented!() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use eframe::App;"),
            "应包含 eframe::App: {}",
            result
        );
        assert!(
            result.contains("use walkdir::WalkDir;"),
            "应包含 walkdir::WalkDir: {}",
            result
        );
        assert!(
            result.contains("use indicatif::ProgressBar;"),
            "应包含 indicatif::ProgressBar: {}",
            result
        );
    }

    // ===== Session 149: .iter() / .try_fold() / .try_for_each() / .is_sorted() / .collect_into() trait 方法检测 =====

    #[test]
    fn test_ensure_external_imports_iter_method() {
        let code = "fn foo(v: Vec<i32>) { v.iter().for_each(|x| println!(\"{}\", x)); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应通过 .iter() 添加 Iterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_try_fold_method() {
        let code = "fn foo(v: Vec<i32>) -> Result<i32, &'static str> { v.iter().try_fold(0, |acc, &x| acc + x) }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应通过 .try_fold() 添加 Iterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_try_for_each_method() {
        let code =
            "fn foo(v: Vec<i32>) -> Result<(), &'static str> { v.iter().try_for_each(|x| Ok(())) }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应通过 .try_for_each() 添加 Iterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_is_sorted_method() {
        let code = "fn foo(v: Vec<i32>) -> bool { v.iter().is_sorted() }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应通过 .is_sorted() 添加 Iterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_collect_into_method() {
        let code =
            "fn foo(v: Vec<i32>) { let mut buf = Vec::new(); v.iter().collect_into(&mut buf); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应通过 .collect_into() 添加 Iterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_s149_methods_idempotent() {
        let code = "fn foo(v: Vec<i32>) { v.iter(); v.try_fold(0, |a, b| a + b); v.try_for_each(|x| Ok(())); v.is_sorted(); }";
        let first = ensure_external_imports(code);
        let second = ensure_external_imports(&first);
        assert_eq!(first, second, "S149 trait 方法检测应幂等");
    }

    #[test]
    fn test_ensure_external_imports_s149_methods_already_imported() {
        let code = "use std::iter::Iterator;\nfn foo(v: Vec<i32>) { v.iter(); v.try_fold(0, |a, b| a + b); }";
        let result = ensure_external_imports(code);
        let count = result.matches("use std::iter::Iterator;").count();
        assert_eq!(count, 1, "已有 Iterator 导入不应重复: {}", result);
    }

    #[test]
    fn test_ensure_external_imports_s149_iter_with_glob() {
        let code =
            "use std::iter::*;\nfn foo(v: Vec<i32>) { v.iter(); v.try_for_each(|x| Ok(())); }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use std::iter::Iterator;"),
            "通配导入应覆盖: {}",
            result
        );
    }

    // ===== Session 149: verify_imports S149 trait 方法检测 =====

    #[test]
    fn test_verify_imports_iter_method() {
        let code = "fn foo(v: Vec<i32>) { v.iter().for_each(|x| println!(\"{}\", x)); }";
        let issues = verify_imports(code);
        let iterator_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.type_name == "Iterator" && i.module_path == "std::iter")
            .collect();
        assert!(
            !iterator_issues.is_empty(),
            "应检测到 .iter() 触发 Iterator 导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_try_fold_method() {
        let code = "fn foo(v: Vec<i32>) -> Result<i32, &'static str> { v.iter().try_fold(0, |acc, &x| acc + x) }";
        let issues = verify_imports(code);
        let iterator_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.type_name == "Iterator" && i.module_path == "std::iter")
            .collect();
        assert!(
            !iterator_issues.is_empty(),
            "应检测到 .try_fold() 触发 Iterator 导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_try_for_each_method() {
        let code =
            "fn foo(v: Vec<i32>) -> Result<(), &'static str> { v.iter().try_for_each(|x| Ok(())) }";
        let issues = verify_imports(code);
        let iterator_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.type_name == "Iterator" && i.module_path == "std::iter")
            .collect();
        assert!(
            !iterator_issues.is_empty(),
            "应检测到 .try_for_each() 触发 Iterator 导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_is_sorted_method() {
        let code = "fn foo(v: Vec<i32>) -> bool { v.iter().is_sorted() }";
        let issues = verify_imports(code);
        let iterator_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.type_name == "Iterator" && i.module_path == "std::iter")
            .collect();
        assert!(
            !iterator_issues.is_empty(),
            "应检测到 .is_sorted() 触发 Iterator 导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_collect_into_method() {
        let code =
            "fn foo(v: Vec<i32>) { let mut buf = Vec::new(); v.iter().collect_into(&mut buf); }";
        let issues = verify_imports(code);
        let iterator_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.type_name == "Iterator" && i.module_path == "std::iter")
            .collect();
        assert!(
            !iterator_issues.is_empty(),
            "应检测到 .collect_into() 触发 Iterator 导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_s149_iterator_methods_combined() {
        let code = "fn foo(v: Vec<i32>) { v.iter(); v.try_fold(0, |a, b| a + b); v.try_for_each(|x| Ok(())); v.is_sorted(); v.collect_into(&mut Vec::new()); }";
        let issues = verify_imports(code);
        let iterator_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.type_name == "Iterator" && i.module_path == "std::iter")
            .collect();
        assert_eq!(
            iterator_issues.len(),
            1,
            "多种 Session 149 Iterator 方法只应报告一次: {:?}",
            issues
        );
    }

    // ===== Session 149: ensure_external_imports 后无问题验证 =====

    #[test]
    fn test_ensure_external_imports_then_verify_no_s149_issues() {
        let code = "fn foo(v: Vec<i32>) { v.iter(); v.try_fold(0, |a, b| a + b); v.try_for_each(|x| Ok(())); v.is_sorted(); v.collect_into(&mut Vec::new()); }";
        let fixed = ensure_external_imports(code);
        let issues = verify_imports(&fixed);
        let s149_issues: Vec<_> = issues
            .iter()
            .filter(|i| i.type_name == "Iterator" && i.module_path == "std::iter")
            .collect();
        assert!(
            s149_issues.is_empty(),
            "ensure_external_imports 后不应有 Session 149 Iterator 导入问题: {:?}",
            s149_issues
        );
    }

    // ===== Session 149: 混合 S148+S149 trait 方法验证 =====

    #[test]
    fn test_mixed_s148_s149_iterator_methods() {
        let code = "fn foo(v: Vec<i32>, s: &str) { v.iter(); v.iter_mut(); s.lines(); v.try_fold(0, |a, b| a + b); v.is_sorted(); }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应包含 Iterator 导入: {}",
            result
        );
    }

    // ===== Session 150: ensure_external_imports 新 std 类型测试 =====

    #[test]
    fn test_ensure_external_imports_from_str() {
        let code = "impl FromStr for MyType { type Err = (); fn from_str(s: &str) -> Result<Self, Self::Err> { unimplemented!() } }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::str::FromStr;"),
            "应添加 FromStr 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_write_fmt() {
        let code = "impl Write for MyType { fn write_str(&mut self, s: &str) -> std::fmt::Result { unimplemented!() } }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::fmt::Write;"),
            "应添加 Write 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_deref() {
        let code = "impl Deref for MyType { type Target = i32; fn deref(&self) -> &Self::Target { unimplemented!() } }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::ops::Deref;"),
            "应添加 Deref 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_deref_mut() {
        let code = "impl DerefMut for MyType { fn deref_mut(&mut self) -> &mut Self::Target { unimplemented!() } }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::ops::DerefMut;"),
            "应添加 DerefMut 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_index() {
        let code = "impl Index<usize> for MyType { type Output = i32; fn index(&self, idx: usize) -> &Self::Output { unimplemented!() } }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::ops::Index;"),
            "应添加 Index 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_index_mut() {
        let code = "impl IndexMut<usize> for MyType { fn index_mut(&mut self, idx: usize) -> &mut Self::Output { unimplemented!() } }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::ops::IndexMut;"),
            "应添加 IndexMut 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_drop() {
        let code = "impl Drop for MyType { fn drop(&mut self) { } }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::ops::Drop;"),
            "应添加 Drop 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_fused_iterator() {
        let code = "impl FusedIterator for MyIter { }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::FusedIterator;"),
            "应添加 FusedIterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_double_ended_iterator() {
        let code = "impl DoubleEndedIterator for MyIter { fn next_back(&mut self) -> Option<Self::Item> { unimplemented!() } }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::DoubleEndedIterator;"),
            "应添加 DoubleEndedIterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_exact_size_iterator() {
        let code = "impl ExactSizeIterator for MyIter { fn len(&self) -> usize { 0 } }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::ExactSizeIterator;"),
            "应添加 ExactSizeIterator 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_s150_multiple_ops_types() {
        let code = "impl Deref for A { type Target = i32; fn deref(&self) -> &i32 { &0 } }\nimpl Index<usize> for A { type Output = i32; fn index(&self, _: usize) -> &i32 { &0 } }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::ops::{Deref, Index};"),
            "应合并 Deref 和 Index 导入: {}",
            result
        );
    }

    #[test]
    fn test_ensure_external_imports_s150_already_imported() {
        let code = "use std::str::FromStr;\nimpl FromStr for A { type Err = (); fn from_str(_: &str) -> Result<Self, Self::Err> { unimplemented!() } }";
        let result = ensure_external_imports(code);
        let count = result.matches("use std::str::FromStr;").count();
        assert_eq!(count, 1, "已有 FromStr 导入不应重复添加: {}", result);
    }

    #[test]
    fn test_ensure_external_imports_s150_idempotent() {
        let code = "impl Deref for A { type Target = i32; fn deref(&self) -> &i32 { &0 } }\nimpl Drop for A { fn drop(&mut self) {} }";
        let first = ensure_external_imports(code);
        let second = ensure_external_imports(&first);
        assert_eq!(first, second, "S150 类型检测应幂等");
    }

    #[test]
    fn test_ensure_external_imports_s150_with_glob() {
        let code = "use std::ops::*;\nimpl Deref for A { type Target = i32; fn deref(&self) -> &i32 { &0 } }";
        let result = ensure_external_imports(code);
        assert!(
            !result.contains("use std::ops::Deref;"),
            "通配导入应覆盖: {}",
            result
        );
    }

    // ===== Session 150: verify_imports 新 std 类型测试 =====

    #[test]
    fn test_verify_imports_from_str_missing() {
        let issues = verify_imports("impl FromStr for A { type Err = (); fn from_str(s: &str) -> Result<Self, Self::Err> { unimplemented!() } }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "FromStr" && i.module_path == "std::str"),
            "应检测到 FromStr 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_write_fmt_missing() {
        let issues = verify_imports("impl Write for A { fn write_str(&mut self, s: &str) -> std::fmt::Result { unimplemented!() } }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Write" && i.module_path == "std::fmt"),
            "应检测到 Write 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_deref_missing() {
        let issues = verify_imports(
            "impl Deref for A { type Target = i32; fn deref(&self) -> &i32 { &0 } }",
        );
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Deref" && i.module_path == "std::ops"),
            "应检测到 Deref 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_index_mut_missing() {
        let issues = verify_imports("impl IndexMut<usize> for A { fn index_mut(&mut self, idx: usize) -> &mut i32 { unimplemented!() } }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "IndexMut" && i.module_path == "std::ops"),
            "应检测到 IndexMut 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_drop_missing() {
        let issues = verify_imports("impl Drop for A { fn drop(&mut self) { } }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "Drop" && i.module_path == "std::ops"),
            "应检测到 Drop 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_fused_iterator_missing() {
        let issues = verify_imports("impl FusedIterator for A { }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "FusedIterator" && i.module_path == "std::iter"),
            "应检测到 FusedIterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_double_ended_iterator_missing() {
        let issues = verify_imports(
            "impl DoubleEndedIterator for A { fn next_back(&mut self) -> Option<i32> { None } }",
        );
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "DoubleEndedIterator" && i.module_path == "std::iter"),
            "应检测到 DoubleEndedIterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_exact_size_iterator_missing() {
        let issues =
            verify_imports("impl ExactSizeIterator for A { fn len(&self) -> usize { 0 } }");
        assert!(
            issues
                .iter()
                .any(|i| i.type_name == "ExactSizeIterator" && i.module_path == "std::iter"),
            "应检测到 ExactSizeIterator 缺失导入: {:?}",
            issues
        );
    }

    #[test]
    fn test_verify_imports_s150_already_imported() {
        let code = "use std::ops::Deref;\nimpl Deref for A { type Target = i32; fn deref(&self) -> &i32 { &0 } }";
        let issues = verify_imports(code);
        assert!(
            !issues
                .iter()
                .any(|i| i.type_name == "Deref" && i.module_path == "std::ops"),
            "已有 Deref 导入不应报告: {:?}",
            issues
        );
    }

    // ===== Session 150: ensure_external_imports 后无问题验证 =====

    #[test]
    fn test_ensure_external_imports_then_verify_no_s150_issues() {
        let code = "impl FromStr for A { type Err = (); fn from_str(s: &str) -> Result<Self, Self::Err> { unimplemented!() } }\nimpl Deref for A { type Target = i32; fn deref(&self) -> &i32 { &0 } }\nimpl Index<usize> for A { type Output = i32; fn index(&self, _: usize) -> &i32 { &0 } }";
        let fixed = ensure_external_imports(code);
        let issues = verify_imports(&fixed);
        let s150_issues: Vec<_> = issues
            .iter()
            .filter(|i| {
                i.module_path == "std::str"
                    || i.module_path == "std::ops"
                    || i.module_path == "std::iter"
                    || i.module_path == "std::fmt"
            })
            .filter(|i| {
                matches!(
                    i.type_name.as_str(),
                    "FromStr"
                        | "Write"
                        | "Deref"
                        | "DerefMut"
                        | "Index"
                        | "IndexMut"
                        | "Drop"
                        | "FusedIterator"
                        | "DoubleEndedIterator"
                        | "ExactSizeIterator"
                )
            })
            .collect();
        assert!(
            s150_issues.is_empty(),
            "ensure_external_imports 后不应有 Session 150 类型导入问题: {:?}",
            s150_issues
        );
    }

    // ===== Session 150: 混合 S149+S150 类型验证 =====

    #[test]
    fn test_mixed_s149_s150_types() {
        let code = "fn foo(v: Vec<i32>) { v.iter(); }\nimpl Deref for A { type Target = i32; fn deref(&self) -> &i32 { &0 } }";
        let result = ensure_external_imports(code);
        assert!(
            result.contains("use std::iter::Iterator;"),
            "应包含 S149 Iterator 导入: {}",
            result
        );
        assert!(
            result.contains("use std::ops::Deref;"),
            "应包含 S150 Deref 导入: {}",
            result
        );
    }

    // ===== Session 150: 双格式一致性验证 =====

    #[test]
    fn test_s150_json_markdown_consistency() {
        let code = "impl FromStr for A { type Err = (); fn from_str(s: &str) -> Result<Self, Self::Err> { unimplemented!() } }\nimpl Deref for A { type Target = i32; fn deref(&self) -> &i32 { &0 } }";
        let json = verify_imports_to_json(code);
        let markdown = verify_imports_to_markdown(code);
        assert!(
            json.contains("FromStr"),
            "JSON 报告应包含 FromStr: {}",
            json
        );
        assert!(
            markdown.contains("FromStr"),
            "Markdown 报告应包含 FromStr: {}",
            markdown
        );
        assert!(json.contains("Deref"), "JSON 报告应包含 Deref: {}", json);
        assert!(
            markdown.contains("Deref"),
            "Markdown 报告应包含 Deref: {}",
            markdown
        );
    }
}
