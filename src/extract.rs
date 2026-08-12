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

/// 验证 Rust 代码的括号配对 (Session 113)
///
/// AI 生成的 Rust 代码 (特别是 GLM 模型) 可能存在括号不配对的问题:
/// - `{` 和 `}` 不配对 (最常见的大括号匹配问题)
/// - `(` 和 `)` 不配对
/// - `[` 和 `]` 不配对
///
/// 本函数在跳过字符串、字符字面量和注释中的括号后, 检查括号配对。
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
/// ```
pub fn validate_rust_braces(content: &str) -> Option<String> {
    let mut issues = Vec::new();

    // 状态机: 跟踪字符串/注释状态, 只在代码状态中计数括号
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Code,
        LineComment,      // // ...
        BlockComment,     // /* ... */
        String,           // "..."
        RawString(usize), // r#"..."# (hash_depth)
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
                State::BlockComment
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

            // === 块注释: 遇到 */ 结束 ===
            (State::BlockComment, '*', Some('/')) => {
                i += 1; // 跳过 /
                State::Code
            }
            (State::BlockComment, _, _) => State::BlockComment,

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
pub fn extract_files(text: &str) -> Vec<ExtractedFile> {
    // 清理 DeepSeek 等 UI 文本污染 (如 "复制下载" 按钮文本)
    let cleaned = clean_ui_text(text);
    // 规范化 file: 标记 (处理 path 和内容在同一行的情况)
    let normalized = normalize_file_markers(&cleaned);
    let text = normalized.as_str();

    let mut files = Vec::new();

    // 模式1: ```file:path\n...``` 或 ```lang:path\n...```
    let re_tagged = tagged_regex();

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

    // 模式2: 普通 ```lang\n...``` 代码块 (无路径)
    if files.is_empty() {
        let re_plain = plain_regex();
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
    }

    // 模式3: 无反引号 — "file:path\n...代码..." (AI 没用代码块格式时)
    if files.is_empty() {
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
    }

    // 去重: 同一路径取最后一个
    let mut seen: HashMap<String, ExtractedFile> = HashMap::new();
    for f in files {
        seen.insert(f.path.clone(), f);
    }

    let result: Vec<ExtractedFile> = seen.into_values().collect();
    if !result.is_empty() {
        info!("提取到 {} 个文件", result.len());
        for f in &result {
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
        }
    }

    result
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
}
