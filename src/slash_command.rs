//! AI 自主指令 (Slash Commands) — 借鉴方向 5
//!
//! 增强 Forge 核心中的核心 (自主提问能力), 让 AI 不只是被动回答问题,
//! 而是可以主动发出指令来影响 Forge 的行为。
//!
//! ## 核心思路
//!
//! 在 AI 的回复中检测特殊的指令标记 (如 `/compact`、`/skip`、`/refocus`),
//! Forge 解析这些指令并执行对应操作:
//! - `/compact` — AI 建议压缩上下文 → 触发上下文衔接
//! - `/skip`    — AI 建议跳过当前任务 → 标记任务为 Failed 并继续
//! - `/refocus` — AI 建议重新聚焦 → 触发转向提醒
//! - `/retry`   — AI 建议用不同方法重试 → 重置循环终止检测器
//! - `/escalate`— AI 请求人工干预 → 触发人工干预接口
//!
//! ## 检测规则
//!
//! - 指令以 `/` 开头, 后跟关键字 (如 `/compact`)
//! - 指令必须作为独立单词出现 (不是文件路径的一部分)
//! - 代码块 (``` ... ```) 内的文本不被检测 (避免误报)
//! - 大小写不敏感 (`/Compact` 和 `/compact` 等效)
//! - 多个指令可以在同一条回复中检测
//!
//! ## 与现有机制的关系
//!
//! - **自主追问 (Clarification)**: AI 被动回答后 Forge 判断是否追问
//! - **Slash Commands (本模块)**: AI 主动发出指令, Forge 解析执行
//! - 两者互补: 追问是 Forge → AI, Slash Commands 是 AI → Forge

use std::fmt;

// ============================================================================
//  SlashCommand — 指令类型
// ============================================================================

/// AI 自主指令类型 — 从 AI 回复中解析出的指令
///
/// 每种指令对应一种 Forge 行为变更:
/// - `Compact` → 触发上下文衔接 (新开对话 + 交接)
/// - `Skip` → 跳过当前任务 (标记为 Failed)
/// - `Refocus` → 注入转向提醒 (重新锚定 AI 注意力)
/// - `Retry` → 重置循环终止检测器 (允许全新方法重试)
/// - `Escalate` → 触发人工干预 (请求人类决策)
/// - `Unknown` → 未识别的指令 (保留原始文本, 不执行任何操作)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SlashCommand {
    /// `/compact` — AI 建议压缩上下文 → 触发上下文衔接
    Compact,
    /// `/skip` — AI 建议跳过当前任务 → 标记任务为 Failed 并继续
    Skip,
    /// `/refocus` — AI 建议重新聚焦 → 触发转向提醒
    Refocus,
    /// `/retry` — AI 建议用不同方法重试 → 重置循环终止检测器
    Retry,
    /// `/escalate` — AI 请求人工干预 → 触发人工干预接口
    Escalate,
    /// 未识别的指令 (保留原始指令文本)
    Unknown(String),
}

impl SlashCommand {
    /// 获取指令的关键字 (不含 `/` 前缀)
    ///
    /// 如 `Compact` → `"compact"`, `Unknown("foo")` → `"foo"`
    pub fn keyword(&self) -> &str {
        match self {
            SlashCommand::Compact => "compact",
            SlashCommand::Skip => "skip",
            SlashCommand::Refocus => "refocus",
            SlashCommand::Retry => "retry",
            SlashCommand::Escalate => "escalate",
            SlashCommand::Unknown(s) => s,
        }
    }

    /// 获取指令的完整形式 (含 `/` 前缀)
    ///
    /// 如 `Compact` → `"/compact"`
    pub fn full_command(&self) -> String {
        format!("/{}", self.keyword())
    }

    /// 获取指令的中文描述
    pub fn description(&self) -> &str {
        match self {
            SlashCommand::Compact => "压缩上下文",
            SlashCommand::Skip => "跳过任务",
            SlashCommand::Refocus => "重新聚焦",
            SlashCommand::Retry => "换方法重试",
            SlashCommand::Escalate => "请求人工干预",
            SlashCommand::Unknown(_) => "未知指令",
        }
    }

    /// 是否为已知指令 (非 Unknown)
    pub fn is_known(&self) -> bool {
        !matches!(self, SlashCommand::Unknown(_))
    }

    /// 所有已知指令列表
    pub fn all_known() -> Vec<SlashCommand> {
        vec![
            SlashCommand::Compact,
            SlashCommand::Skip,
            SlashCommand::Refocus,
            SlashCommand::Retry,
            SlashCommand::Escalate,
        ]
    }

    /// 从关键字字符串创建指令 (大小写不敏感)
    ///
    /// 如 `"compact"` → `Some(Compact)`, `"foo"` → `Some(Unknown("foo"))`
    pub fn from_keyword(keyword: &str) -> SlashCommand {
        let lower = keyword.to_lowercase();
        match lower.as_str() {
            "compact" => SlashCommand::Compact,
            "skip" => SlashCommand::Skip,
            "refocus" => SlashCommand::Refocus,
            "retry" => SlashCommand::Retry,
            "escalate" => SlashCommand::Escalate,
            _ => SlashCommand::Unknown(keyword.to_string()),
        }
    }

    /// 从 AI 回复中解析所有 slash commands (关联函数便捷入口)
    ///
    /// 等价于调用自由函数 `parse_from_response`。
    pub fn parse_from_response(text: &str) -> Vec<SlashCommand> {
        crate::slash_command::parse_from_response(text)
    }
}

impl fmt::Display for SlashCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.full_command())
    }
}

// ============================================================================
//  SlashCommandAction — 指令执行结果
// ============================================================================

/// 指令执行后对任务的影响
///
/// - `Continue` — 继续正常处理 (副作用指令如 compact/refocus/retry/escalate)
/// - `SkipTask` — 跳过当前任务 (Skip 指令)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlashCommandAction {
    /// 继续正常处理
    Continue,
    /// 跳过当前任务
    SkipTask,
}

impl SlashCommandAction {
    /// 是否应该跳过任务
    pub fn should_skip(&self) -> bool {
        matches!(self, SlashCommandAction::SkipTask)
    }
}

// ============================================================================
//  解析器
// ============================================================================

/// 已知指令关键字 (小写, 用于匹配)
const KNOWN_KEYWORDS: &[&str] = &["compact", "skip", "refocus", "retry", "escalate"];

/// 从 AI 回复中解析所有 slash commands
///
/// 检测规则:
/// 1. 以 `/` 开头, 后跟已知关键字
/// 2. 指令作为独立单词出现 (后面是空白/标点/行尾)
/// 3. 代码块内的文本不被检测
/// 4. 大小写不敏感
/// 5. 去重: 同一指令只返回一次
///
/// # 示例
/// ```
/// use forge::slash_command::{SlashCommand, parse_from_response};
/// let cmds = parse_from_response("代码写好了\n/skip\n/compact");
/// assert!(cmds.contains(&SlashCommand::Skip));
/// assert!(cmds.contains(&SlashCommand::Compact));
/// ```
pub fn parse_from_response(text: &str) -> Vec<SlashCommand> {
    let mut commands = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    // 跟踪是否在代码块内
    let mut in_code_block = false;

    for line in text.lines() {
        // 检测代码块边界
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }

        // 代码块内不检测
        if in_code_block {
            continue;
        }

        // 在行中查找 /command 模式
        for cmd in find_commands_in_line(line) {
            let key = cmd.keyword().to_lowercase();
            if seen.insert(key) {
                commands.push(cmd);
            }
        }
    }

    commands
}

/// 在单行文本中查找所有 slash commands
///
/// 查找 `/keyword` 模式, 其中 keyword 是已知指令或未知指令。
/// 只检测已知指令 (未知指令不返回, 避免误报文件路径)。
fn find_commands_in_line(line: &str) -> Vec<SlashCommand> {
    let mut commands = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        // 查找 '/' 字符
        if chars[i] == '/' {
            // 检查前面是否为空白或行首 (避免匹配文件路径中间的 /)
            let prev_is_boundary = i == 0 || chars[i - 1].is_whitespace() || chars[i - 1] == '\n';

            if prev_is_boundary {
                // 提取 '/' 后面的关键字
                let mut end = i + 1;
                while end < chars.len() && is_keyword_char(chars[end]) {
                    end += 1;
                }

                if end > i + 1 {
                    let keyword: String = chars[i + 1..end].iter().collect();
                    let lower = keyword.to_lowercase();

                    // 检查后面是否为边界 (空白/标点/行尾)
                    let next_is_boundary = end >= chars.len() || !is_keyword_char(chars[end]);

                    // 只匹配已知指令, 避免文件路径误报
                    if next_is_boundary && KNOWN_KEYWORDS.contains(&lower.as_str()) {
                        commands.push(SlashCommand::from_keyword(&keyword));
                    }
                }
            }
        }
        i += 1;
    }

    commands
}

/// 判断字符是否为关键字字符 (字母, 不含数字/斜杠)
fn is_keyword_char(c: char) -> bool {
    c.is_ascii_alphabetic()
}

/// 检查 AI 回复中是否包含特定指令
///
/// # 示例
/// ```
/// use forge::slash_command::{SlashCommand, has_command};
/// assert!(has_command("done\n/skip", SlashCommand::Skip));
/// assert!(!has_command("done", SlashCommand::Skip));
/// ```
pub fn has_command(text: &str, command: SlashCommand) -> bool {
    parse_from_response(text).contains(&command)
}

/// 从 AI 回复中移除 slash command 标记
///
/// 移除 `/compact`、`/skip` 等指令标记, 返回干净的文本。
/// 代码块内的文本不受影响。
pub fn strip_commands(text: &str) -> String {
    let mut result = Vec::new();
    let mut in_code_block = false;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            result.push(line.to_string());
            continue;
        }

        if in_code_block {
            result.push(line.to_string());
            continue;
        }

        // 移除行中的指令标记
        let cleaned = strip_commands_from_line(line);
        result.push(cleaned);
    }

    result.join("\n")
}

/// 从单行中移除 slash command 标记
fn strip_commands_from_line(line: &str) -> String {
    let commands = find_commands_in_line(line);
    if commands.is_empty() {
        return line.to_string();
    }

    let mut result = line.to_string();
    for cmd in &commands {
        // 移除指令标记 (大小写不敏感)
        let full = cmd.full_command();
        let full_lower = full.to_lowercase();
        // 查找并移除
        if let Some(pos) = result.to_lowercase().find(&full_lower) {
            // 确保是完整匹配 (边界检查)
            let end_pos = pos + full.len();
            if end_pos >= result.len()
                || !is_keyword_char(result.chars().nth(end_pos).unwrap_or(' '))
            {
                result = format!("{}{}", &result[..pos], &result[end_pos..]);
            }
        }
    }

    // 清理多余的空格
    result.trim_end().to_string()
}

// ============================================================================
//  SlashCommandSummary — 指令统计
// ============================================================================

/// Slash Command 执行统计 — 用于 DevTrace 报告
#[derive(Debug, Clone, Default)]
pub struct SlashCommandSummary {
    /// 检测到的指令总数
    pub total_detected: usize,
    /// 执行的指令数 (已知指令)
    pub executed: usize,
    /// 跳过的任务数 (Skip 指令触发)
    pub tasks_skipped: usize,
    /// 上下文压缩次数 (Compact 指令触发)
    pub compacts: usize,
    /// 重新聚焦次数 (Refocus 指令触发)
    pub refocuses: usize,
    /// 换方法重试次数 (Retry 指令触发)
    pub retries: usize,
    /// 人工干预请求次数 (Escalate 指令触发)
    pub escalations: usize,
}

impl SlashCommandSummary {
    /// 创建空统计
    pub fn new() -> Self {
        Self::default()
    }

    /// 记算已知指令执行率 (0.0 ~ 1.0)
    pub fn execution_rate(&self) -> f64 {
        if self.total_detected == 0 {
            return 0.0;
        }
        self.executed as f64 / self.total_detected as f64
    }

    /// 生成可读的报告文本
    pub fn to_report(&self) -> String {
        if self.total_detected == 0 {
            return String::new();
        }

        let mut report = String::new();
        report.push_str("  ── Slash Commands 统计 ──\n");
        report.push_str(&format!(
            "  检测到: {}  执行: {}  ({:.0}%)\n",
            self.total_detected,
            self.executed,
            self.execution_rate() * 100.0
        ));
        report.push_str(&format!(
            "  /skip: {}  /compact: {}  /refocus: {}  /retry: {}  /escalate: {}\n",
            self.tasks_skipped, self.compacts, self.refocuses, self.retries, self.escalations
        ));
        report
    }
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ===== SlashCommand::keyword / full_command / description =====

    #[test]
    fn test_keyword() {
        assert_eq!(SlashCommand::Compact.keyword(), "compact");
        assert_eq!(SlashCommand::Skip.keyword(), "skip");
        assert_eq!(SlashCommand::Refocus.keyword(), "refocus");
        assert_eq!(SlashCommand::Retry.keyword(), "retry");
        assert_eq!(SlashCommand::Escalate.keyword(), "escalate");
        assert_eq!(SlashCommand::Unknown("foo".to_string()).keyword(), "foo");
    }

    #[test]
    fn test_full_command() {
        assert_eq!(SlashCommand::Compact.full_command(), "/compact");
        assert_eq!(SlashCommand::Skip.full_command(), "/skip");
        assert_eq!(
            SlashCommand::Unknown("bar".to_string()).full_command(),
            "/bar"
        );
    }

    #[test]
    fn test_description() {
        assert_eq!(SlashCommand::Compact.description(), "压缩上下文");
        assert_eq!(SlashCommand::Skip.description(), "跳过任务");
        assert_eq!(SlashCommand::Refocus.description(), "重新聚焦");
        assert_eq!(SlashCommand::Retry.description(), "换方法重试");
        assert_eq!(SlashCommand::Escalate.description(), "请求人工干预");
    }

    #[test]
    fn test_is_known() {
        assert!(SlashCommand::Compact.is_known());
        assert!(SlashCommand::Skip.is_known());
        assert!(!SlashCommand::Unknown("foo".to_string()).is_known());
    }

    #[test]
    fn test_all_known() {
        let all = SlashCommand::all_known();
        assert_eq!(all.len(), 5);
        assert!(all.contains(&SlashCommand::Compact));
        assert!(all.contains(&SlashCommand::Skip));
        assert!(all.contains(&SlashCommand::Refocus));
        assert!(all.contains(&SlashCommand::Retry));
        assert!(all.contains(&SlashCommand::Escalate));
    }

    #[test]
    fn test_from_keyword() {
        assert_eq!(SlashCommand::from_keyword("compact"), SlashCommand::Compact);
        assert_eq!(SlashCommand::from_keyword("skip"), SlashCommand::Skip);
        assert_eq!(SlashCommand::from_keyword("refocus"), SlashCommand::Refocus);
        assert_eq!(SlashCommand::from_keyword("retry"), SlashCommand::Retry);
        assert_eq!(
            SlashCommand::from_keyword("escalate"),
            SlashCommand::Escalate
        );
        // 大小写不敏感
        assert_eq!(SlashCommand::from_keyword("Compact"), SlashCommand::Compact);
        assert_eq!(SlashCommand::from_keyword("SKIP"), SlashCommand::Skip);
        // 未知
        assert_eq!(
            SlashCommand::from_keyword("foo"),
            SlashCommand::Unknown("foo".to_string())
        );
    }

    #[test]
    fn test_display() {
        assert_eq!(SlashCommand::Compact.to_string(), "/compact");
        assert_eq!(SlashCommand::Skip.to_string(), "/skip");
    }

    // ===== parse_from_response — 基本检测 =====

    #[test]
    fn test_parse_single_command() {
        let cmds = parse_from_response("代码写好了\n/skip");
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0], SlashCommand::Skip);
    }

    #[test]
    fn test_parse_multiple_commands() {
        let cmds = parse_from_response("/skip\n/compact\n/refocus");
        assert_eq!(cmds.len(), 3);
        assert!(cmds.contains(&SlashCommand::Skip));
        assert!(cmds.contains(&SlashCommand::Compact));
        assert!(cmds.contains(&SlashCommand::Refocus));
    }

    #[test]
    fn test_parse_no_commands() {
        let cmds = parse_from_response("这是普通回复,没有指令\n代码内容");
        assert!(cmds.is_empty());
    }

    #[test]
    fn test_parse_empty_text() {
        let cmds = parse_from_response("");
        assert!(cmds.is_empty());
    }

    #[test]
    fn test_parse_all_known_commands() {
        let cmds = parse_from_response("/compact\n/skip\n/refocus\n/retry\n/escalate");
        assert_eq!(cmds.len(), 5);
        for cmd in SlashCommand::all_known() {
            assert!(cmds.contains(&cmd), "缺少指令: {:?}", cmd);
        }
    }

    // ===== parse_from_response — 大小写不敏感 =====

    #[test]
    fn test_parse_case_insensitive() {
        let cmds = parse_from_response("/Compact\n/SKIP\n/Refocus");
        assert_eq!(cmds.len(), 3);
        assert!(cmds.contains(&SlashCommand::Compact));
        assert!(cmds.contains(&SlashCommand::Skip));
        assert!(cmds.contains(&SlashCommand::Refocus));
    }

    #[test]
    fn test_parse_mixed_case() {
        let cmds = parse_from_response("/compAcT");
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0], SlashCommand::Compact);
    }

    // ===== parse_from_response — 去重 =====

    #[test]
    fn test_parse_dedup_same_command() {
        let cmds = parse_from_response("/skip\n/skip\n/skip");
        assert_eq!(cmds.len(), 1); // 去重
        assert_eq!(cmds[0], SlashCommand::Skip);
    }

    #[test]
    fn test_parse_dedup_different_case() {
        let cmds = parse_from_response("/skip\n/SKIP\n/Skip");
        assert_eq!(cmds.len(), 1); // 去重 (大小写不敏感)
    }

    // ===== parse_from_response — 代码块内不检测 =====

    #[test]
    fn test_parse_skip_code_blocks() {
        let text = "```\n/skip\n/compact\n```\n/refocus";
        let cmds = parse_from_response(text);
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0], SlashCommand::Refocus);
    }

    #[test]
    fn test_parse_multiple_code_blocks() {
        let text = "/skip\n```\n/compact\n```\n/refocus\n```\n/retry\n```";
        let cmds = parse_from_response(text);
        assert_eq!(cmds.len(), 2);
        assert!(cmds.contains(&SlashCommand::Skip));
        assert!(cmds.contains(&SlashCommand::Refocus));
    }

    #[test]
    fn test_parse_code_block_not_closed() {
        // 未闭合的代码块 — 内部不检测
        let text = "```\n/skip\n/compact";
        let cmds = parse_from_response(text);
        assert!(cmds.is_empty());
    }

    // ===== parse_from_response — 避免文件路径误报 =====

    #[test]
    fn test_parse_not_in_file_path() {
        // 文件路径中间的 /skip 不应被检测
        let cmds = parse_from_response("文件在 /home/user/skip/test.rs");
        assert!(cmds.is_empty());
    }

    #[test]
    fn test_parse_not_url_path() {
        let cmds = parse_from_response("URL: https://example.com/skip");
        assert!(cmds.is_empty());
    }

    #[test]
    fn test_parse_command_at_line_start() {
        let cmds = parse_from_response("/skip");
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0], SlashCommand::Skip);
    }

    #[test]
    fn test_parse_command_after_whitespace() {
        let cmds = parse_from_response("  /skip");
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0], SlashCommand::Skip);
    }

    #[test]
    fn test_parse_command_with_trailing_text() {
        // /skip 后面有文本 — 应该仍然检测到 (后面是空格边界)
        let cmds = parse_from_response("/skip 因为这个任务不可能完成");
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0], SlashCommand::Skip);
    }

    #[test]
    fn test_parse_command_with_trailing_punctuation() {
        let cmds = parse_from_response("/skip.");
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0], SlashCommand::Skip);
    }

    #[test]
    fn test_parse_command_in_middle_of_line() {
        // 行中间的指令 (前面有空格) 也可以检测
        let cmds = parse_from_response("我觉得应该 /skip 这个任务");
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0], SlashCommand::Skip);
    }

    #[test]
    fn test_parse_not_part_of_word() {
        // /skipx 不是 /skip
        let cmds = parse_from_response("/skipx");
        assert!(cmds.is_empty());
    }

    #[test]
    fn test_parse_not_after_alphanumeric() {
        // abc/skip 不应被检测 (前面不是边界)
        let cmds = parse_from_response("abc/skip");
        assert!(cmds.is_empty());
    }

    // ===== has_command =====

    #[test]
    fn test_has_command_true() {
        assert!(has_command("done\n/skip", SlashCommand::Skip));
        assert!(has_command("/compact", SlashCommand::Compact));
    }

    #[test]
    fn test_has_command_false() {
        assert!(!has_command("done", SlashCommand::Skip));
        assert!(!has_command("/compact", SlashCommand::Skip));
    }

    #[test]
    fn test_has_command_in_code_block() {
        assert!(!has_command("```\n/skip\n```", SlashCommand::Skip));
    }

    // ===== strip_commands =====

    #[test]
    fn test_strip_single_command() {
        let result = strip_commands("代码写好了\n/skip");
        assert!(!result.contains("/skip"));
        assert!(result.contains("代码写好了"));
    }

    #[test]
    fn test_strip_multiple_commands() {
        let result = strip_commands("/skip\n/compact\n代码");
        assert!(!result.contains("/skip"));
        assert!(!result.contains("/compact"));
        assert!(result.contains("代码"));
    }

    #[test]
    fn test_strip_no_commands() {
        let result = strip_commands("普通文本\n代码");
        assert_eq!(result, "普通文本\n代码");
    }

    #[test]
    fn test_strip_preserves_code_blocks() {
        let text = "```\n/skip\n```\n/compact";
        let result = strip_commands(text);
        // 代码块内不移除
        assert!(result.contains("/skip"));
        // 代码块外移除
        assert!(!result.contains("/compact"));
    }

    #[test]
    fn test_strip_empty() {
        assert_eq!(strip_commands(""), "");
    }

    // ===== SlashCommandAction =====

    #[test]
    fn test_action_continue() {
        assert!(!SlashCommandAction::Continue.should_skip());
    }

    #[test]
    fn test_action_skip() {
        assert!(SlashCommandAction::SkipTask.should_skip());
    }

    // ===== SlashCommandSummary =====

    #[test]
    fn test_summary_new() {
        let s = SlashCommandSummary::new();
        assert_eq!(s.total_detected, 0);
        assert_eq!(s.executed, 0);
        assert_eq!(s.tasks_skipped, 0);
    }

    #[test]
    fn test_summary_execution_rate() {
        let mut s = SlashCommandSummary::new();
        assert_eq!(s.execution_rate(), 0.0);

        s.total_detected = 10;
        s.executed = 8;
        assert!((s.execution_rate() - 0.8).abs() < 0.001);
    }

    #[test]
    fn test_summary_to_report_empty() {
        let s = SlashCommandSummary::new();
        assert_eq!(s.to_report(), "");
    }

    #[test]
    fn test_summary_to_report_with_data() {
        let mut s = SlashCommandSummary::new();
        s.total_detected = 5;
        s.executed = 4;
        s.tasks_skipped = 2;
        s.compacts = 1;
        s.refocuses = 1;

        let report = s.to_report();
        assert!(report.contains("Slash Commands 统计"));
        assert!(report.contains("检测到: 5"));
        assert!(report.contains("/skip: 2"));
        assert!(report.contains("/compact: 1"));
        assert!(report.contains("/refocus: 1"));
    }

    // ===== find_commands_in_line =====

    #[test]
    fn test_find_in_line_single() {
        let cmds = find_commands_in_line("/skip");
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0], SlashCommand::Skip);
    }

    #[test]
    fn test_find_in_line_multiple() {
        let cmds = find_commands_in_line("/skip /compact");
        assert_eq!(cmds.len(), 2);
    }

    #[test]
    fn test_find_in_line_none() {
        let cmds = find_commands_in_line("普通文本");
        assert!(cmds.is_empty());
    }

    #[test]
    fn test_find_in_line_path() {
        let cmds = find_commands_in_line("/home/user/skip");
        assert!(cmds.is_empty());
    }

    // ===== is_keyword_char =====

    #[test]
    fn test_is_keyword_char() {
        assert!(is_keyword_char('a'));
        assert!(is_keyword_char('Z'));
        assert!(!is_keyword_char('1'));
        assert!(!is_keyword_char('/'));
        assert!(!is_keyword_char(' '));
        assert!(!is_keyword_char('-'));
    }

    // ===== 集成场景测试 =====

    #[test]
    fn test_scenario_real_ai_response_with_skip() {
        let response = r#"我认为当前任务无法完成，因为缺少必要的依赖。

/skip

建议在后续阶段重新尝试此任务。"#;
        let cmds = parse_from_response(response);
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0], SlashCommand::Skip);
    }

    #[test]
    fn test_scenario_real_ai_response_with_compact_and_refocus() {
        let response = r#"代码已经写完了，但上下文太长了。

/compact
/refocus

```file:src/main.rs
fn main() { println!("hello"); }
```"#;
        let cmds = parse_from_response(response);
        assert_eq!(cmds.len(), 2);
        assert!(cmds.contains(&SlashCommand::Compact));
        assert!(cmds.contains(&SlashCommand::Refocus));
    }

    #[test]
    fn test_scenario_commands_in_code_not_detected() {
        let response = r#"这是回复

```rust
let path = "/skip/test";
let cmd = "/compact";
```

/retry
"#;
        let cmds = parse_from_response(response);
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0], SlashCommand::Retry);
    }

    #[test]
    fn test_scenario_all_five_commands() {
        let response = "/compact\n/skip\n/refocus\n/retry\n/escalate";
        let cmds = parse_from_response(response);
        assert_eq!(cmds.len(), 5);
    }

    #[test]
    fn test_scenario_strip_then_parse() {
        let text = "代码\n/skip\n/compact";
        let stripped = strip_commands(text);
        let cmds = parse_from_response(&stripped);
        assert!(cmds.is_empty()); // strip 后不应再检测到指令
    }

    #[test]
    fn test_scenario_mixed_text_and_commands() {
        let response = r#"我已完成任务1。

```file:src/lib.rs
pub fn add(a: i32, b: i32) -> i32 { a + b }
```

/refocus

接下来需要注意类型安全。"#;
        let cmds = parse_from_response(response);
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0], SlashCommand::Refocus);

        let stripped = strip_commands(response);
        assert!(!stripped.contains("/refocus"));
        assert!(stripped.contains("pub fn add"));
    }

    #[test]
    fn test_scenario_unknown_command_ignored() {
        // 未知指令不应出现在结果中 (避免文件路径误报)
        let cmds = parse_from_response("/foobar\n/skip");
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0], SlashCommand::Skip);
    }

    #[test]
    fn test_scenario_command_at_end_of_response() {
        let cmds = parse_from_response("代码写好了\n```file:src/main.rs\nfn main() {}\n```\n/skip");
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0], SlashCommand::Skip);
    }

    #[test]
    fn test_scenario_command_at_start_of_response() {
        let cmds = parse_from_response("/compact\n开始写代码...");
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0], SlashCommand::Compact);
    }

    #[test]
    fn test_scenario_repeated_commands_dedup() {
        let cmds = parse_from_response("/skip\n代码\n/skip\n更多代码\n/skip");
        assert_eq!(cmds.len(), 1);
    }

    #[test]
    fn test_scenario_command_with_explanation() {
        let response = "/skip  因为依赖缺失, 无法完成此任务";
        let cmds = parse_from_response(response);
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0], SlashCommand::Skip);
    }
}
