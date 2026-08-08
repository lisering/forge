//! 结构化开发追踪 — 借鉴方向 4
//!
//! 为 24 小时不间断运行提供可观测性, 记录每一轮 AI 交互的详细信息
//! (时间戳、阶段、任务、操作类型、输入摘要、输出摘要、结果),
//! 持久化到 `.forge/devtrace.jsonl` (JSON Lines 格式, 便于流式写入和后续分析)。
//!
//! ## 核心思路
//!
//! 24 小时运行后, 人类需要知道这 24 小时 Forge 做了什么。
//! 没有结构化追踪, 24 小时的运行结果就是一个黑箱。
//! DevTrace 提供时间线视图, 让人类可以快速了解:
//! 哪些任务成功了、哪些失败了、每轮交互花了多长时间、AI 的回复质量如何。
//!
//! ## 与现有机制的关系
//!
//! - **Memory (memory.json)**: 记录宏观状态 (阶段/任务/决策/对话历史)
//! - **DevTrace (devtrace.jsonl)**: 记录微观时间线 (每一轮交互的详细 trace)
//! - **ErrorHistory (error_history.json)**: 记录错误模式 (用于智能错误诊断)
//! - 三者互补: Memory 是"做了什么", DevTrace 是"什么时候怎么做的",
//!   ErrorHistory 是"犯了什么错"
//!
//! ## JSONL 格式
//!
//! 每行一个 JSON 对象, 便于流式写入和后续分析:
//! ```jsonl
//! {"timestamp":"2024-01-01T00:00:00Z","action":"Planning","input_summary":"...","output_summary":"...","duration_ms":5000,"success":true}
//! {"timestamp":"2024-01-01T00:05:00Z","action":"TaskExecution","phase_idx":0,"task_idx":0,"task_name":"初始化项目","input_summary":"...","output_summary":"...","duration_ms":3000,"success":true}
//! ```

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use tracing::warn;

// ============================================================================
//  TraceAction — 操作类型
// ============================================================================

/// Trace 操作类型 — 标识每一轮交互的操作性质
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TraceAction {
    /// 阶段规划 — AI 拆解终极目标为开发阶段
    Planning,
    /// 任务执行 — attempt 1, 首次执行任务
    TaskExecution,
    /// 修复尝试 — attempt > 1, 修复编译/测试错误
    FixAttempt,
    /// 自主追问 — 检查 AI 回复后发起追问
    Clarification,
    /// 上下文衔接 — 对话过长时新开对话并交接上下文
    ContextHandoff,
    /// 转向提醒 — 每隔 N 轮注入提醒, 防止 AI 跑偏
    SteerReminder,
    /// 循环终止检测 — 检测到修复死循环, 改变策略
    LoopDetection,
    /// 编译检查 — cargo check / 语言适配器 check
    CompileCheck,
    /// 测试运行 — cargo test / 语言适配器 test
    TestRun,
    /// E2E 测试 — 运行二进制进行端到端测试
    E2ETest,
    /// 需求变更 — 检测到需求变更并重新规划
    RequirementChange,
    /// AI 自主指令 — AI 回复中包含 slash command 并被执行
    SlashCommand,
    /// 自动恢复 — Chrome 断连后自动重连 (24h 可靠性)
    Recovery,
    /// 网站健康检查 — 检测网站是否健康 (登录/限流/维护)
    HealthCheck,
    /// 网站自动切换 — 主网站不健康时切换到备用标签页
    SiteFailover,
    /// 性能统计 — 运行结束后写入各网站性能统计摘要
    PerformanceStats,
}

impl std::fmt::Display for TraceAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TraceAction::Planning => write!(f, "Planning"),
            TraceAction::TaskExecution => write!(f, "TaskExecution"),
            TraceAction::FixAttempt => write!(f, "FixAttempt"),
            TraceAction::Clarification => write!(f, "Clarification"),
            TraceAction::ContextHandoff => write!(f, "ContextHandoff"),
            TraceAction::SteerReminder => write!(f, "SteerReminder"),
            TraceAction::LoopDetection => write!(f, "LoopDetection"),
            TraceAction::CompileCheck => write!(f, "CompileCheck"),
            TraceAction::TestRun => write!(f, "TestRun"),
            TraceAction::E2ETest => write!(f, "E2ETest"),
            TraceAction::RequirementChange => write!(f, "RequirementChange"),
            TraceAction::SlashCommand => write!(f, "SlashCommand"),
            TraceAction::Recovery => write!(f, "Recovery"),
            TraceAction::HealthCheck => write!(f, "HealthCheck"),
            TraceAction::SiteFailover => write!(f, "SiteFailover"),
            TraceAction::PerformanceStats => write!(f, "PerformanceStats"),
        }
    }
}

impl TraceAction {
    /// 获取操作的中文描述
    pub fn description(&self) -> &'static str {
        match self {
            TraceAction::Planning => "阶段规划",
            TraceAction::TaskExecution => "任务执行",
            TraceAction::FixAttempt => "修复尝试",
            TraceAction::Clarification => "自主追问",
            TraceAction::ContextHandoff => "上下文衔接",
            TraceAction::SteerReminder => "转向提醒",
            TraceAction::LoopDetection => "循环终止检测",
            TraceAction::CompileCheck => "编译检查",
            TraceAction::TestRun => "测试运行",
            TraceAction::E2ETest => "E2E 测试",
            TraceAction::RequirementChange => "需求变更",
            TraceAction::SlashCommand => "AI 自主指令",
            TraceAction::Recovery => "自动恢复",
            TraceAction::HealthCheck => "健康检查",
            TraceAction::SiteFailover => "网站切换",
            TraceAction::PerformanceStats => "性能统计",
        }
    }

    /// 所有操作类型
    pub fn all() -> Vec<TraceAction> {
        vec![
            TraceAction::Planning,
            TraceAction::TaskExecution,
            TraceAction::FixAttempt,
            TraceAction::Clarification,
            TraceAction::ContextHandoff,
            TraceAction::SteerReminder,
            TraceAction::LoopDetection,
            TraceAction::CompileCheck,
            TraceAction::TestRun,
            TraceAction::E2ETest,
            TraceAction::RequirementChange,
            TraceAction::SlashCommand,
            TraceAction::Recovery,
            TraceAction::HealthCheck,
            TraceAction::SiteFailover,
            TraceAction::PerformanceStats,
        ]
    }
}

// ============================================================================
//  DevTraceEntry — 单条 Trace 记录
// ============================================================================

/// 单条开发追踪记录 — 记录一轮 AI 交互的详细信息
///
/// 每条记录包含时间戳、操作类型、阶段/任务索引、输入/输出摘要、
/// 耗时和结果, 序列化为 JSONL 格式写入 `.forge/devtrace.jsonl`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevTraceEntry {
    /// 时间戳 (UTC)
    pub timestamp: DateTime<Utc>,
    /// 阶段索引 (None = planning 阶段或全局操作)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase_idx: Option<usize>,
    /// 任务索引 (None = 阶段级操作)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_idx: Option<usize>,
    /// 任务名称 (便于人类阅读)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_name: Option<String>,
    /// 操作类型
    pub action: TraceAction,
    /// 输入摘要 (前 200 字符)
    pub input_summary: String,
    /// 输出摘要 (前 200 字符)
    pub output_summary: String,
    /// 耗时 (毫秒)
    pub duration_ms: u64,
    /// 是否成功
    pub success: bool,
    /// 错误信息 (如有)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl DevTraceEntry {
    /// 创建一条新的 Trace 记录
    ///
    /// 自动截断输入/输出摘要到 200 字符, 设置当前时间戳。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        action: TraceAction,
        phase_idx: Option<usize>,
        task_idx: Option<usize>,
        task_name: Option<&str>,
        input: &str,
        output: &str,
        duration_ms: u64,
        success: bool,
        error: Option<&str>,
    ) -> Self {
        Self {
            timestamp: Utc::now(),
            phase_idx,
            task_idx,
            task_name: task_name.map(String::from),
            action,
            input_summary: truncate_str(input, 200),
            output_summary: truncate_str(output, 200),
            duration_ms,
            success,
            error: error.map(String::from),
        }
    }

    /// 序列化为 JSON 字符串 (单行, 用于 JSONL)
    pub fn to_jsonl(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    /// 从 JSON 字符串反序列化
    pub fn from_jsonl(line: &str) -> Result<Self> {
        Ok(serde_json::from_str(line)?)
    }
}

/// 截断字符串到指定字符数
fn truncate_str(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

// ============================================================================
//  ActionStats — 操作统计
// ============================================================================

/// 单个操作类型的统计信息
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ActionStats {
    /// 总次数
    pub count: usize,
    /// 成功次数
    pub success_count: usize,
    /// 总耗时 (毫秒)
    pub total_duration_ms: u64,
}

impl ActionStats {
    /// 创建空统计
    pub fn new() -> Self {
        Self::default()
    }

    /// 成功率 (0.0 ~ 1.0)
    pub fn success_rate(&self) -> f64 {
        if self.count == 0 {
            return 0.0;
        }
        self.success_count as f64 / self.count as f64
    }

    /// 平均耗时 (毫秒)
    pub fn avg_duration_ms(&self) -> u64 {
        if self.count == 0 {
            return 0;
        }
        self.total_duration_ms / self.count as u64
    }

    /// 记录一次操作
    pub fn record(&mut self, duration_ms: u64, success: bool) {
        self.count += 1;
        if success {
            self.success_count += 1;
        }
        self.total_duration_ms += duration_ms;
    }
}

// ============================================================================
//  TimelineEntry — 简化的时间线条目
// ============================================================================

/// 简化的时间线条目 — 用于 DevTraceSummary 的概览
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineEntry {
    /// 时间戳 (UTC)
    pub timestamp: DateTime<Utc>,
    /// 操作类型
    pub action: TraceAction,
    /// 任务名称 (便于人类阅读)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_name: Option<String>,
    /// 是否成功
    pub success: bool,
    /// 耗时 (毫秒)
    pub duration_ms: u64,
}

impl TimelineEntry {
    /// 从 DevTraceEntry 创建
    pub fn from_entry(entry: &DevTraceEntry) -> Self {
        Self {
            timestamp: entry.timestamp,
            action: entry.action,
            task_name: entry.task_name.clone(),
            success: entry.success,
            duration_ms: entry.duration_ms,
        }
    }
}

// ============================================================================
//  DevTraceSummary — 追踪摘要
// ============================================================================

/// 开发追踪摘要 — 24 小时运行后的快速概览
///
/// 包含总条目数、总耗时、按操作类型的统计、成功率和简化时间线。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevTraceSummary {
    /// 总条目数
    pub total_entries: usize,
    /// 总耗时 (毫秒)
    pub total_duration_ms: u64,
    /// 按操作类型的统计
    pub by_action: HashMap<TraceAction, ActionStats>,
    /// 总体成功率 (0.0 ~ 1.0)
    pub success_rate: f64,
    /// 简化时间线 (最近 100 条)
    pub timeline: Vec<TimelineEntry>,
}

impl DevTraceSummary {
    /// 创建空摘要
    pub fn empty() -> Self {
        Self {
            total_entries: 0,
            total_duration_ms: 0,
            by_action: HashMap::new(),
            success_rate: 0.0,
            timeline: vec![],
        }
    }

    /// 从 trace 条目列表构建摘要
    pub fn from_entries(entries: &[DevTraceEntry]) -> Self {
        let total_entries = entries.len();
        let total_duration_ms: u64 = entries.iter().map(|e| e.duration_ms).sum();
        let success_count = entries.iter().filter(|e| e.success).count();
        let success_rate = if total_entries == 0 {
            0.0
        } else {
            success_count as f64 / total_entries as f64
        };

        // 按操作类型统计
        let mut by_action: HashMap<TraceAction, ActionStats> = HashMap::new();
        for entry in entries {
            let stats = by_action.entry(entry.action).or_default();
            stats.record(entry.duration_ms, entry.success);
        }

        // 时间线: 最近 100 条
        let timeline: Vec<TimelineEntry> = if entries.len() <= 100 {
            entries.iter().map(TimelineEntry::from_entry).collect()
        } else {
            entries[entries.len() - 100..]
                .iter()
                .map(TimelineEntry::from_entry)
                .collect()
        };

        Self {
            total_entries,
            total_duration_ms,
            by_action,
            success_rate,
            timeline,
        }
    }

    /// 获取某个操作类型的统计 (如有)
    pub fn get_action_stats(&self, action: TraceAction) -> Option<&ActionStats> {
        self.by_action.get(&action)
    }

    /// 生成可读的报告文本
    pub fn to_report(&self) -> String {
        let mut report = String::new();
        report.push_str("═══════════════════════════════════════════════════\n");
        report.push_str("  📊 DevTrace 开发追踪报告\n");
        report.push_str("═══════════════════════════════════════════════════\n\n");

        report.push_str(&format!("  总条目: {}\n", self.total_entries));
        report.push_str(&format!(
            "  总耗时: {:.1}s ({:.1}m)\n",
            self.total_duration_ms as f64 / 1000.0,
            self.total_duration_ms as f64 / 60000.0
        ));
        report.push_str(&format!("  成功率: {:.1}%\n\n", self.success_rate * 100.0));

        report.push_str("  ── 按操作类型统计 ──\n");
        for action in TraceAction::all() {
            if let Some(stats) = self.by_action.get(&action) {
                report.push_str(&format!(
                    "  {:20} 次数: {:4}  成功: {:4} ({:5.1}%)  平均: {:5}ms\n",
                    action.description(),
                    stats.count,
                    stats.success_count,
                    stats.success_rate() * 100.0,
                    stats.avg_duration_ms()
                ));
            }
        }

        if !self.timeline.is_empty() {
            report.push_str("\n  ── 时间线 (最近 100 条) ──\n");
            for entry in &self.timeline {
                let status = if entry.success { "✅" } else { "❌" };
                let task = entry.task_name.as_deref().unwrap_or("-");
                report.push_str(&format!(
                    "  {} {} {:20} {} ({}ms)\n",
                    entry.timestamp.format("%H:%M:%S"),
                    status,
                    entry.action.description(),
                    task,
                    entry.duration_ms
                ));
            }
        }

        report
    }
}

// ============================================================================
//  DevTraceWriter — JSONL 写入/读取器
// ============================================================================

/// 开发追踪写入器 — 将 trace 条目流式写入 JSONL 文件
///
/// 文件位于 `<workspace>/.forge/devtrace.jsonl`, 每行一个 JSON 对象。
/// 使用追加模式写入, 支持 24 小时不间断运行。
/// `write_entry` 接受 `&self` (非 `&mut self`), 避免与 Orchestrator 的借用冲突。
pub struct DevTraceWriter {
    /// trace 文件路径 (`workspace`/.forge/devtrace.jsonl)
    pub trace_path: PathBuf,
}

impl Clone for DevTraceWriter {
    /// 克隆 DevTraceWriter — 共享同一 trace 文件路径
    ///
    /// 用于将 DevTraceWriter 共享给 FailoverChatClient,
    /// 使健康检查和网站切换事件也能写入同一 trace 文件。
    fn clone(&self) -> Self {
        Self {
            trace_path: self.trace_path.clone(),
        }
    }
}

impl DevTraceWriter {
    /// 创建 DevTraceWriter
    ///
    /// trace 文件路径为 `<workspace_root>/.forge/devtrace.jsonl`。
    /// 文件在首次 `write_entry` 时自动创建 (追加模式)。
    pub fn new(workspace_root: &Path) -> Self {
        let trace_path = workspace_root.join(".forge").join("devtrace.jsonl");
        Self { trace_path }
    }

    /// 写入一条 trace 条目 (追加模式)
    ///
    /// 将条目序列化为 JSON 并追加到文件末尾。
    /// 使用 `&self` (非 `&mut self`), 每次调用打开-写入-关闭文件,
    /// 避免与 Orchestrator 的借用冲突。
    pub fn write_entry(&self, entry: &DevTraceEntry) -> Result<()> {
        let line = entry.to_jsonl()?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.trace_path)?;
        writeln!(file, "{}", line)?;
        Ok(())
    }

    /// 便捷方法: 创建并写入一条 trace 条目
    #[allow(clippy::too_many_arguments)]
    pub fn trace(
        &self,
        action: TraceAction,
        phase_idx: Option<usize>,
        task_idx: Option<usize>,
        task_name: Option<&str>,
        input: &str,
        output: &str,
        duration_ms: u64,
        success: bool,
        error: Option<&str>,
    ) -> Result<()> {
        let entry = DevTraceEntry::new(
            action,
            phase_idx,
            task_idx,
            task_name,
            input,
            output,
            duration_ms,
            success,
            error,
        );
        self.write_entry(&entry)
    }

    /// 读取所有 trace 条目
    ///
    /// 逐行读取 JSONL 文件并反序列化。
    /// 空行和格式错误的行会被跳过 (不中断读取)。
    /// 文件不存在时返回空 Vec。
    pub fn read_all(&self) -> Result<Vec<DevTraceEntry>> {
        if !self.trace_path.exists() {
            return Ok(vec![]);
        }

        let file = std::fs::File::open(&self.trace_path)?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();

        for (line_num, line) in reader.lines().enumerate() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match DevTraceEntry::from_jsonl(trimmed) {
                Ok(entry) => entries.push(entry),
                Err(e) => {
                    warn!(
                        "DevTrace: 跳过格式错误的行 {} ({}): {}",
                        line_num + 1,
                        e,
                        &trimmed[..trimmed.len().min(100)]
                    );
                }
            }
        }

        Ok(entries)
    }

    /// 生成追踪摘要
    ///
    /// 读取所有条目并计算统计信息 (总条目数、总耗时、按操作类型统计、
    /// 成功率、时间线)。
    /// 文件不存在或为空时返回空摘要。
    pub fn summary(&self) -> DevTraceSummary {
        match self.read_all() {
            Ok(entries) => DevTraceSummary::from_entries(&entries),
            Err(e) => {
                warn!("DevTrace: 读取 trace 文件失败: {}", e);
                DevTraceSummary::empty()
            }
        }
    }

    /// 清空 trace 文件 (重新开始时调用)
    pub fn clear(&self) -> Result<()> {
        std::fs::write(&self.trace_path, "")?;
        Ok(())
    }

    /// 获取当前条目数
    pub fn entry_count(&self) -> usize {
        self.read_all().map(|entries| entries.len()).unwrap_or(0)
    }
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // ===== 辅助函数 =====

    /// 创建临时 DevTraceWriter
    fn make_writer() -> (tempfile::TempDir, DevTraceWriter) {
        let dir = tempdir().unwrap();
        // 创建 .forge 目录
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
        let writer = DevTraceWriter::new(dir.path());
        (dir, writer)
    }

    /// 创建一个简单的 trace 条目
    fn make_entry(action: TraceAction, success: bool) -> DevTraceEntry {
        DevTraceEntry::new(
            action,
            Some(0),
            Some(0),
            Some("测试任务"),
            "输入内容",
            "输出内容",
            1000,
            success,
            if success { None } else { Some("测试错误") },
        )
    }

    // ===== truncate_str =====

    #[test]
    fn test_truncate_str_short() {
        let result = truncate_str("hello", 200);
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_truncate_str_exact_200() {
        let input: String = "x".repeat(200);
        let result = truncate_str(&input, 200);
        assert_eq!(result.len(), 200);
    }

    #[test]
    fn test_truncate_str_long() {
        let input: String = "x".repeat(300);
        let result = truncate_str(&input, 200);
        assert_eq!(result.chars().count(), 200);
    }

    #[test]
    fn test_truncate_str_empty() {
        let result = truncate_str("", 200);
        assert_eq!(result, "");
    }

    #[test]
    fn test_truncate_str_unicode() {
        let input = "你好世界".repeat(100); // 500 chars
        let result = truncate_str(&input, 200);
        assert_eq!(result.chars().count(), 200);
    }

    // ===== TraceAction =====

    #[test]
    fn test_trace_action_display() {
        assert_eq!(TraceAction::Planning.to_string(), "Planning");
        assert_eq!(TraceAction::TaskExecution.to_string(), "TaskExecution");
        assert_eq!(TraceAction::FixAttempt.to_string(), "FixAttempt");
        assert_eq!(TraceAction::Clarification.to_string(), "Clarification");
        assert_eq!(TraceAction::ContextHandoff.to_string(), "ContextHandoff");
        assert_eq!(TraceAction::SteerReminder.to_string(), "SteerReminder");
        assert_eq!(TraceAction::LoopDetection.to_string(), "LoopDetection");
        assert_eq!(TraceAction::CompileCheck.to_string(), "CompileCheck");
        assert_eq!(TraceAction::TestRun.to_string(), "TestRun");
        assert_eq!(TraceAction::E2ETest.to_string(), "E2ETest");
        assert_eq!(
            TraceAction::RequirementChange.to_string(),
            "RequirementChange"
        );
        assert_eq!(TraceAction::SlashCommand.to_string(), "SlashCommand");
        assert_eq!(TraceAction::Recovery.to_string(), "Recovery");
        assert_eq!(TraceAction::HealthCheck.to_string(), "HealthCheck");
        assert_eq!(TraceAction::SiteFailover.to_string(), "SiteFailover");
        assert_eq!(
            TraceAction::PerformanceStats.to_string(),
            "PerformanceStats"
        );
    }

    #[test]
    fn test_trace_action_description() {
        assert_eq!(TraceAction::Planning.description(), "阶段规划");
        assert_eq!(TraceAction::TaskExecution.description(), "任务执行");
        assert_eq!(TraceAction::FixAttempt.description(), "修复尝试");
        assert_eq!(TraceAction::Clarification.description(), "自主追问");
        assert_eq!(TraceAction::ContextHandoff.description(), "上下文衔接");
        assert_eq!(TraceAction::SteerReminder.description(), "转向提醒");
        assert_eq!(TraceAction::LoopDetection.description(), "循环终止检测");
        assert_eq!(TraceAction::CompileCheck.description(), "编译检查");
        assert_eq!(TraceAction::TestRun.description(), "测试运行");
        assert_eq!(TraceAction::E2ETest.description(), "E2E 测试");
        assert_eq!(TraceAction::RequirementChange.description(), "需求变更");
        assert_eq!(TraceAction::SlashCommand.description(), "AI 自主指令");
        assert_eq!(TraceAction::Recovery.description(), "自动恢复");
        assert_eq!(TraceAction::HealthCheck.description(), "健康检查");
        assert_eq!(TraceAction::SiteFailover.description(), "网站切换");
        assert_eq!(TraceAction::PerformanceStats.description(), "性能统计");
    }

    #[test]
    fn test_trace_action_all() {
        let all = TraceAction::all();
        assert_eq!(all.len(), 16);
        assert!(all.contains(&TraceAction::Planning));
        assert!(all.contains(&TraceAction::TaskExecution));
        assert!(all.contains(&TraceAction::FixAttempt));
        assert!(all.contains(&TraceAction::Clarification));
        assert!(all.contains(&TraceAction::ContextHandoff));
        assert!(all.contains(&TraceAction::SteerReminder));
        assert!(all.contains(&TraceAction::LoopDetection));
        assert!(all.contains(&TraceAction::CompileCheck));
        assert!(all.contains(&TraceAction::TestRun));
        assert!(all.contains(&TraceAction::E2ETest));
        assert!(all.contains(&TraceAction::RequirementChange));
        assert!(all.contains(&TraceAction::SlashCommand));
        assert!(all.contains(&TraceAction::Recovery));
        assert!(all.contains(&TraceAction::HealthCheck));
        assert!(all.contains(&TraceAction::SiteFailover));
        assert!(all.contains(&TraceAction::PerformanceStats));
    }

    #[test]
    fn test_trace_action_serde() {
        let action = TraceAction::TaskExecution;
        let json = serde_json::to_string(&action).unwrap();
        assert_eq!(json, "\"TaskExecution\"");

        let parsed: TraceAction = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, action);
    }

    #[test]
    fn test_trace_action_hash_eq() {
        let mut map: HashMap<TraceAction, usize> = HashMap::new();
        map.insert(TraceAction::Planning, 1);
        map.insert(TraceAction::Planning, 2);
        assert_eq!(map.get(&TraceAction::Planning), Some(&2));
        assert_eq!(map.len(), 1);
    }

    // ===== DevTraceEntry =====

    #[test]
    fn test_entry_new_basic() {
        let entry = DevTraceEntry::new(
            TraceAction::TaskExecution,
            Some(0),
            Some(1),
            Some("初始化项目"),
            "请创建项目",
            "已创建项目",
            5000,
            true,
            None,
        );

        assert_eq!(entry.phase_idx, Some(0));
        assert_eq!(entry.task_idx, Some(1));
        assert_eq!(entry.task_name, Some("初始化项目".to_string()));
        assert_eq!(entry.action, TraceAction::TaskExecution);
        assert_eq!(entry.input_summary, "请创建项目");
        assert_eq!(entry.output_summary, "已创建项目");
        assert_eq!(entry.duration_ms, 5000);
        assert!(entry.success);
        assert!(entry.error.is_none());
    }

    #[test]
    fn test_entry_new_truncates_input() {
        let long_input: String = "x".repeat(500);
        let entry = DevTraceEntry::new(
            TraceAction::Planning,
            None,
            None,
            None,
            &long_input,
            "output",
            100,
            true,
            None,
        );
        assert_eq!(entry.input_summary.chars().count(), 200);
    }

    #[test]
    fn test_entry_new_truncates_output() {
        let long_output: String = "y".repeat(500);
        let entry = DevTraceEntry::new(
            TraceAction::Planning,
            None,
            None,
            None,
            "input",
            &long_output,
            100,
            true,
            None,
        );
        assert_eq!(entry.output_summary.chars().count(), 200);
    }

    #[test]
    fn test_entry_new_with_error() {
        let entry = DevTraceEntry::new(
            TraceAction::CompileCheck,
            Some(0),
            Some(0),
            None,
            "check",
            "failed",
            200,
            false,
            Some("E0308: mismatched types"),
        );
        assert!(!entry.success);
        assert_eq!(entry.error, Some("E0308: mismatched types".to_string()));
    }

    #[test]
    fn test_entry_new_none_fields() {
        let entry = DevTraceEntry::new(
            TraceAction::Planning,
            None,
            None,
            None,
            "input",
            "output",
            100,
            true,
            None,
        );
        assert!(entry.phase_idx.is_none());
        assert!(entry.task_idx.is_none());
        assert!(entry.task_name.is_none());
        assert!(entry.error.is_none());
    }

    #[test]
    fn test_entry_to_jsonl() {
        let entry = make_entry(TraceAction::TaskExecution, true);
        let json = entry.to_jsonl().unwrap();
        assert!(json.contains("\"action\":\"TaskExecution\""));
        assert!(json.contains("\"success\":true"));
        assert!(json.contains("\"duration_ms\":1000"));
        assert!(!json.contains('\n')); // 单行
    }

    #[test]
    fn test_entry_from_jsonl() {
        let entry = make_entry(TraceAction::CompileCheck, false);
        let json = entry.to_jsonl().unwrap();
        let parsed = DevTraceEntry::from_jsonl(&json).unwrap();
        assert_eq!(parsed.action, TraceAction::CompileCheck);
        assert!(!parsed.success);
        assert_eq!(parsed.duration_ms, 1000);
        assert_eq!(parsed.error, Some("测试错误".to_string()));
    }

    #[test]
    fn test_entry_jsonl_roundtrip() {
        let entry = DevTraceEntry::new(
            TraceAction::Planning,
            Some(2),
            Some(1),
            Some("测试任务"),
            "输入",
            "输出",
            3000,
            true,
            None,
        );
        let json = entry.to_jsonl().unwrap();
        let parsed = DevTraceEntry::from_jsonl(&json).unwrap();
        assert_eq!(parsed.action, entry.action);
        assert_eq!(parsed.phase_idx, entry.phase_idx);
        assert_eq!(parsed.task_idx, entry.task_idx);
        assert_eq!(parsed.task_name, entry.task_name);
        assert_eq!(parsed.input_summary, entry.input_summary);
        assert_eq!(parsed.output_summary, entry.output_summary);
        assert_eq!(parsed.duration_ms, entry.duration_ms);
        assert_eq!(parsed.success, entry.success);
        assert_eq!(parsed.error, entry.error);
    }

    #[test]
    fn test_entry_jsonl_skip_none_fields() {
        let entry = DevTraceEntry::new(
            TraceAction::Planning,
            None,
            None,
            None,
            "input",
            "output",
            100,
            true,
            None,
        );
        let json = entry.to_jsonl().unwrap();
        assert!(!json.contains("phase_idx"));
        assert!(!json.contains("task_idx"));
        assert!(!json.contains("task_name"));
        assert!(!json.contains("error"));
    }

    #[test]
    fn test_entry_jsonl_includes_none_fields_when_present() {
        let entry = DevTraceEntry::new(
            TraceAction::TaskExecution,
            Some(0),
            Some(0),
            Some("task"),
            "input",
            "output",
            100,
            false,
            Some("error"),
        );
        let json = entry.to_jsonl().unwrap();
        assert!(json.contains("phase_idx"));
        assert!(json.contains("task_idx"));
        assert!(json.contains("task_name"));
        assert!(json.contains("error"));
    }

    // ===== ActionStats =====

    #[test]
    fn test_action_stats_new() {
        let stats = ActionStats::new();
        assert_eq!(stats.count, 0);
        assert_eq!(stats.success_count, 0);
        assert_eq!(stats.total_duration_ms, 0);
    }

    #[test]
    fn test_action_stats_record_success() {
        let mut stats = ActionStats::new();
        stats.record(1000, true);
        assert_eq!(stats.count, 1);
        assert_eq!(stats.success_count, 1);
        assert_eq!(stats.total_duration_ms, 1000);
    }

    #[test]
    fn test_action_stats_record_failure() {
        let mut stats = ActionStats::new();
        stats.record(500, false);
        assert_eq!(stats.count, 1);
        assert_eq!(stats.success_count, 0);
        assert_eq!(stats.total_duration_ms, 500);
    }

    #[test]
    fn test_action_stats_record_multiple() {
        let mut stats = ActionStats::new();
        stats.record(1000, true);
        stats.record(2000, true);
        stats.record(3000, false);
        assert_eq!(stats.count, 3);
        assert_eq!(stats.success_count, 2);
        assert_eq!(stats.total_duration_ms, 6000);
    }

    #[test]
    fn test_action_stats_success_rate() {
        let mut stats = ActionStats::new();
        assert_eq!(stats.success_rate(), 0.0);

        stats.record(100, true);
        stats.record(200, true);
        stats.record(300, false);
        assert!((stats.success_rate() - 2.0 / 3.0).abs() < 0.001);
    }

    #[test]
    fn test_action_stats_avg_duration() {
        let mut stats = ActionStats::new();
        assert_eq!(stats.avg_duration_ms(), 0);

        stats.record(1000, true);
        stats.record(2000, true);
        stats.record(3000, false);
        assert_eq!(stats.avg_duration_ms(), 2000); // 6000 / 3
    }

    #[test]
    fn test_action_stats_default() {
        let stats = ActionStats::default();
        assert_eq!(stats.count, 0);
    }

    // ===== TimelineEntry =====

    #[test]
    fn test_timeline_entry_from_entry() {
        let entry = make_entry(TraceAction::TaskExecution, true);
        let timeline = TimelineEntry::from_entry(&entry);
        assert_eq!(timeline.action, TraceAction::TaskExecution);
        assert_eq!(timeline.task_name, Some("测试任务".to_string()));
        assert!(timeline.success);
        assert_eq!(timeline.duration_ms, 1000);
    }

    #[test]
    fn test_timeline_entry_from_entry_no_task_name() {
        let entry = DevTraceEntry::new(
            TraceAction::Planning,
            None,
            None,
            None,
            "input",
            "output",
            100,
            true,
            None,
        );
        let timeline = TimelineEntry::from_entry(&entry);
        assert!(timeline.task_name.is_none());
    }

    // ===== DevTraceSummary =====

    #[test]
    fn test_summary_empty() {
        let summary = DevTraceSummary::empty();
        assert_eq!(summary.total_entries, 0);
        assert_eq!(summary.total_duration_ms, 0);
        assert_eq!(summary.success_rate, 0.0);
        assert!(summary.timeline.is_empty());
        assert!(summary.by_action.is_empty());
    }

    #[test]
    fn test_summary_from_empty_entries() {
        let summary = DevTraceSummary::from_entries(&[]);
        assert_eq!(summary.total_entries, 0);
        assert_eq!(summary.success_rate, 0.0);
    }

    #[test]
    fn test_summary_from_single_entry() {
        let entry = make_entry(TraceAction::TaskExecution, true);
        let summary = DevTraceSummary::from_entries(&[entry]);
        assert_eq!(summary.total_entries, 1);
        assert_eq!(summary.total_duration_ms, 1000);
        assert!((summary.success_rate - 1.0).abs() < 0.001);
        assert_eq!(summary.timeline.len(), 1);
    }

    #[test]
    fn test_summary_from_multiple_entries() {
        let entries = vec![
            make_entry(TraceAction::TaskExecution, true),
            make_entry(TraceAction::FixAttempt, false),
            make_entry(TraceAction::CompileCheck, false),
            make_entry(TraceAction::TestRun, true),
        ];
        let summary = DevTraceSummary::from_entries(&entries);
        assert_eq!(summary.total_entries, 4);
        assert_eq!(summary.total_duration_ms, 4000);
        assert!((summary.success_rate - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_summary_by_action() {
        let entries = vec![
            make_entry(TraceAction::TaskExecution, true),
            make_entry(TraceAction::TaskExecution, true),
            make_entry(TraceAction::TaskExecution, false),
            make_entry(TraceAction::CompileCheck, true),
        ];
        let summary = DevTraceSummary::from_entries(&entries);

        let task_stats = summary
            .get_action_stats(TraceAction::TaskExecution)
            .unwrap();
        assert_eq!(task_stats.count, 3);
        assert_eq!(task_stats.success_count, 2);

        let check_stats = summary.get_action_stats(TraceAction::CompileCheck).unwrap();
        assert_eq!(check_stats.count, 1);
        assert_eq!(check_stats.success_count, 1);

        assert!(summary.get_action_stats(TraceAction::E2ETest).is_none());
    }

    #[test]
    fn test_summary_timeline_limit_100() {
        let entries: Vec<DevTraceEntry> = (0..150)
            .map(|i| {
                DevTraceEntry::new(
                    TraceAction::TaskExecution,
                    Some(0),
                    Some(i),
                    Some(&format!("任务{}", i)),
                    "input",
                    "output",
                    100 * i as u64,
                    i % 2 == 0,
                    None,
                )
            })
            .collect();

        let summary = DevTraceSummary::from_entries(&entries);
        assert_eq!(summary.total_entries, 150);
        assert_eq!(summary.timeline.len(), 100); // 限制为最近 100 条
    }

    #[test]
    fn test_summary_timeline_under_100() {
        let entries: Vec<DevTraceEntry> = (0..50)
            .map(|_| make_entry(TraceAction::TaskExecution, true))
            .collect();

        let summary = DevTraceSummary::from_entries(&entries);
        assert_eq!(summary.timeline.len(), 50);
    }

    #[test]
    fn test_summary_success_rate_all_success() {
        let entries = vec![
            make_entry(TraceAction::TaskExecution, true),
            make_entry(TraceAction::TestRun, true),
        ];
        let summary = DevTraceSummary::from_entries(&entries);
        assert!((summary.success_rate - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_summary_success_rate_all_failure() {
        let entries = vec![
            make_entry(TraceAction::TaskExecution, false),
            make_entry(TraceAction::TestRun, false),
        ];
        let summary = DevTraceSummary::from_entries(&entries);
        assert!((summary.success_rate - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_summary_to_report() {
        let entries = vec![
            make_entry(TraceAction::TaskExecution, true),
            make_entry(TraceAction::FixAttempt, false),
        ];
        let summary = DevTraceSummary::from_entries(&entries);
        let report = summary.to_report();

        assert!(report.contains("DevTrace 开发追踪报告"));
        assert!(report.contains("总条目: 2"));
        assert!(report.contains("按操作类型统计"));
        assert!(report.contains("任务执行"));
        assert!(report.contains("修复尝试"));
        assert!(report.contains("时间线"));
    }

    #[test]
    fn test_summary_to_report_empty() {
        let summary = DevTraceSummary::empty();
        let report = summary.to_report();

        assert!(report.contains("DevTrace 开发追踪报告"));
        assert!(report.contains("总条目: 0"));
    }

    // ===== DevTraceWriter =====

    #[test]
    fn test_writer_new() {
        let dir = tempdir().unwrap();
        let writer = DevTraceWriter::new(dir.path());
        assert!(writer.trace_path.ends_with(".forge/devtrace.jsonl"));
    }

    #[test]
    fn test_writer_write_and_read_single() {
        let (_dir, writer) = make_writer();
        let entry = make_entry(TraceAction::TaskExecution, true);
        writer.write_entry(&entry).unwrap();

        let entries = writer.read_all().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, TraceAction::TaskExecution);
        assert!(entries[0].success);
    }

    #[test]
    fn test_writer_write_multiple() {
        let (_dir, writer) = make_writer();

        for i in 0..10 {
            let entry = DevTraceEntry::new(
                TraceAction::TaskExecution,
                Some(0),
                Some(i),
                Some(&format!("任务{}", i)),
                "input",
                "output",
                1000 * (i + 1) as u64,
                true,
                None,
            );
            writer.write_entry(&entry).unwrap();
        }

        let entries = writer.read_all().unwrap();
        assert_eq!(entries.len(), 10);
        assert_eq!(entries[0].task_idx, Some(0));
        assert_eq!(entries[9].task_idx, Some(9));
    }

    #[test]
    fn test_writer_write_appends() {
        let (_dir, writer) = make_writer();

        writer
            .write_entry(&make_entry(TraceAction::Planning, true))
            .unwrap();
        writer
            .write_entry(&make_entry(TraceAction::TaskExecution, true))
            .unwrap();
        writer
            .write_entry(&make_entry(TraceAction::CompileCheck, false))
            .unwrap();

        let entries = writer.read_all().unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].action, TraceAction::Planning);
        assert_eq!(entries[1].action, TraceAction::TaskExecution);
        assert_eq!(entries[2].action, TraceAction::CompileCheck);
    }

    #[test]
    fn test_writer_read_empty_file() {
        let (_dir, writer) = make_writer();
        // 文件不存在
        let entries = writer.read_all().unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_writer_read_empty_lines() {
        let (_dir, writer) = make_writer();

        // 写入一条有效条目
        writer
            .write_entry(&make_entry(TraceAction::Planning, true))
            .unwrap();

        // 手动追加空行
        std::fs::OpenOptions::new()
            .append(true)
            .open(&writer.trace_path)
            .unwrap()
            .write_all(b"\n\n\n")
            .unwrap();

        let entries = writer.read_all().unwrap();
        assert_eq!(entries.len(), 1); // 空行被跳过
    }

    #[test]
    fn test_writer_read_malformed_lines() {
        let (_dir, writer) = make_writer();

        // 写入一条有效条目
        writer
            .write_entry(&make_entry(TraceAction::Planning, true))
            .unwrap();

        // 手动追加格式错误的行
        std::fs::OpenOptions::new()
            .append(true)
            .open(&writer.trace_path)
            .unwrap()
            .write_all(b"this is not json\n")
            .unwrap();

        // 再写一条有效条目
        writer
            .write_entry(&make_entry(TraceAction::TaskExecution, true))
            .unwrap();

        let entries = writer.read_all().unwrap();
        assert_eq!(entries.len(), 2); // 格式错误的行被跳过
    }

    #[test]
    fn test_writer_summary() {
        let (_dir, writer) = make_writer();

        writer
            .write_entry(&make_entry(TraceAction::TaskExecution, true))
            .unwrap();
        writer
            .write_entry(&make_entry(TraceAction::TaskExecution, true))
            .unwrap();
        writer
            .write_entry(&make_entry(TraceAction::FixAttempt, false))
            .unwrap();

        let summary = writer.summary();
        assert_eq!(summary.total_entries, 3);
        assert_eq!(summary.total_duration_ms, 3000);
        assert!((summary.success_rate - 2.0 / 3.0).abs() < 0.001);

        let task_stats = summary
            .get_action_stats(TraceAction::TaskExecution)
            .unwrap();
        assert_eq!(task_stats.count, 2);
        assert_eq!(task_stats.success_count, 2);

        let fix_stats = summary.get_action_stats(TraceAction::FixAttempt).unwrap();
        assert_eq!(fix_stats.count, 1);
        assert_eq!(fix_stats.success_count, 0);
    }

    #[test]
    fn test_writer_summary_empty() {
        let (_dir, writer) = make_writer();
        let summary = writer.summary();
        assert_eq!(summary.total_entries, 0);
    }

    #[test]
    fn test_writer_clear() {
        let (_dir, writer) = make_writer();

        writer
            .write_entry(&make_entry(TraceAction::Planning, true))
            .unwrap();
        writer
            .write_entry(&make_entry(TraceAction::TaskExecution, true))
            .unwrap();
        assert_eq!(writer.entry_count(), 2);

        writer.clear().unwrap();
        assert_eq!(writer.entry_count(), 0);
    }

    #[test]
    fn test_writer_entry_count() {
        let (_dir, writer) = make_writer();
        assert_eq!(writer.entry_count(), 0);

        writer
            .write_entry(&make_entry(TraceAction::Planning, true))
            .unwrap();
        assert_eq!(writer.entry_count(), 1);

        writer
            .write_entry(&make_entry(TraceAction::TaskExecution, true))
            .unwrap();
        assert_eq!(writer.entry_count(), 2);
    }

    #[test]
    fn test_writer_trace_helper() {
        let (_dir, writer) = make_writer();
        writer
            .trace(
                TraceAction::CompileCheck,
                Some(0),
                Some(0),
                Some("测试任务"),
                "cargo check",
                "compilation succeeded",
                500,
                true,
                None,
            )
            .unwrap();

        let entries = writer.read_all().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, TraceAction::CompileCheck);
        assert_eq!(entries[0].duration_ms, 500);
        assert!(entries[0].success);
    }

    #[test]
    fn test_writer_trace_helper_with_error() {
        let (_dir, writer) = make_writer();
        writer
            .trace(
                TraceAction::CompileCheck,
                Some(0),
                Some(0),
                None,
                "cargo check",
                "compilation failed",
                500,
                false,
                Some("E0308: type mismatch"),
            )
            .unwrap();

        let entries = writer.read_all().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].success);
        assert_eq!(entries[0].error, Some("E0308: type mismatch".to_string()));
    }

    #[test]
    fn test_writer_creates_file_on_write() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
        let writer = DevTraceWriter::new(dir.path());

        // 文件不存在
        assert!(!writer.trace_path.exists());

        writer
            .write_entry(&make_entry(TraceAction::Planning, true))
            .unwrap();

        // 文件已创建
        assert!(writer.trace_path.exists());
    }

    #[test]
    fn test_writer_all_action_types() {
        let (_dir, writer) = make_writer();

        for action in TraceAction::all() {
            writer
                .trace(action, None, None, None, "input", "output", 100, true, None)
                .unwrap();
        }

        let entries = writer.read_all().unwrap();
        assert_eq!(entries.len(), 16); // 所有 16 种操作类型

        let summary = writer.summary();
        for action in TraceAction::all() {
            assert!(summary.by_action.contains_key(&action));
        }
    }

    #[test]
    fn test_writer_unicode_content() {
        let (_dir, writer) = make_writer();
        writer
            .trace(
                TraceAction::TaskExecution,
                Some(0),
                Some(0),
                Some("初始化项目结构"),
                "请创建一个 Hello World 程序",
                "已创建 src/main.rs 文件",
                3000,
                true,
                None,
            )
            .unwrap();

        let entries = writer.read_all().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].task_name, Some("初始化项目结构".to_string()));
        assert!(entries[0].input_summary.contains("Hello World"));
        assert!(entries[0].output_summary.contains("main.rs"));
    }

    #[test]
    fn test_writer_large_volume() {
        let (_dir, writer) = make_writer();

        // 写入 1000 条
        for i in 0..1000 {
            writer
                .trace(
                    TraceAction::TaskExecution,
                    Some(i / 100),
                    Some(i % 100),
                    Some(&format!("任务{}", i)),
                    &format!("输入{}", i),
                    &format!("输出{}", i),
                    i as u64,
                    i % 3 != 0,
                    if i % 3 == 0 { Some("失败") } else { None },
                )
                .unwrap();
        }

        let entries = writer.read_all().unwrap();
        assert_eq!(entries.len(), 1000);

        let summary = writer.summary();
        assert_eq!(summary.total_entries, 1000);
        assert!(summary.timeline.len() <= 100);
    }

    #[test]
    fn test_summary_to_report_with_all_actions() {
        let (_dir, writer) = make_writer();

        for action in TraceAction::all() {
            writer
                .trace(
                    action,
                    Some(0),
                    Some(0),
                    Some("任务"),
                    "input",
                    "output",
                    100,
                    true,
                    None,
                )
                .unwrap();
        }

        let summary = writer.summary();
        let report = summary.to_report();

        for action in TraceAction::all() {
            assert!(
                report.contains(action.description()),
                "报告应包含操作类型: {}",
                action.description()
            );
        }
    }

    #[test]
    fn test_entry_timestamp_is_recent() {
        let before = Utc::now();
        let entry = DevTraceEntry::new(
            TraceAction::Planning,
            None,
            None,
            None,
            "input",
            "output",
            100,
            true,
            None,
        );
        let after = Utc::now();

        assert!(entry.timestamp >= before);
        assert!(entry.timestamp <= after);
    }

    #[test]
    fn test_writer_read_after_clear_and_rewrite() {
        let (_dir, writer) = make_writer();

        writer
            .write_entry(&make_entry(TraceAction::Planning, true))
            .unwrap();
        writer.clear().unwrap();
        writer
            .write_entry(&make_entry(TraceAction::TaskExecution, true))
            .unwrap();

        let entries = writer.read_all().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, TraceAction::TaskExecution);
    }

    // ===== 集成场景测试 =====

    #[test]
    fn test_scenario_full_development_cycle() {
        let (_dir, writer) = make_writer();

        // 模拟一次完整的开发周期
        // 1. Planning
        writer
            .trace(
                TraceAction::Planning,
                None,
                None,
                None,
                "拆解目标",
                "3个阶段5个任务",
                5000,
                true,
                None,
            )
            .unwrap();

        // 2. Task 1: 执行 → 编译成功 → 测试成功
        writer
            .trace(
                TraceAction::TaskExecution,
                Some(0),
                Some(0),
                Some("初始化"),
                "创建项目",
                "Cargo.toml + main.rs",
                3000,
                true,
                None,
            )
            .unwrap();
        writer
            .trace(
                TraceAction::CompileCheck,
                Some(0),
                Some(0),
                Some("初始化"),
                "cargo check",
                "成功",
                500,
                true,
                None,
            )
            .unwrap();
        writer
            .trace(
                TraceAction::TestRun,
                Some(0),
                Some(0),
                Some("初始化"),
                "cargo test",
                "3 passed",
                1000,
                true,
                None,
            )
            .unwrap();

        // 3. Task 2: 执行 → 编译失败 → 修复 → 编译成功 → 测试成功
        writer
            .trace(
                TraceAction::TaskExecution,
                Some(0),
                Some(1),
                Some("功能实现"),
                "实现功能",
                "代码",
                3000,
                true,
                None,
            )
            .unwrap();
        writer
            .trace(
                TraceAction::CompileCheck,
                Some(0),
                Some(1),
                Some("功能实现"),
                "cargo check",
                "E0308错误",
                500,
                false,
                Some("类型不匹配"),
            )
            .unwrap();
        writer
            .trace(
                TraceAction::FixAttempt,
                Some(0),
                Some(1),
                Some("功能实现"),
                "修复类型",
                "修复后代码",
                2500,
                true,
                None,
            )
            .unwrap();
        writer
            .trace(
                TraceAction::CompileCheck,
                Some(0),
                Some(1),
                Some("功能实现"),
                "cargo check",
                "成功",
                400,
                true,
                None,
            )
            .unwrap();
        writer
            .trace(
                TraceAction::TestRun,
                Some(0),
                Some(1),
                Some("功能实现"),
                "cargo test",
                "5 passed",
                1200,
                true,
                None,
            )
            .unwrap();

        // 4. 自主追问
        writer
            .trace(
                TraceAction::Clarification,
                Some(0),
                Some(1),
                Some("功能实现"),
                "追问类型",
                "AI补充了类型信息",
                2000,
                true,
                None,
            )
            .unwrap();

        // 验证
        let summary = writer.summary();
        assert_eq!(summary.total_entries, 10);
        assert!((summary.success_rate - 9.0 / 10.0).abs() < 0.001);

        let check_stats = summary.get_action_stats(TraceAction::CompileCheck).unwrap();
        assert_eq!(check_stats.count, 3);
        assert_eq!(check_stats.success_count, 2);

        let task_stats = summary
            .get_action_stats(TraceAction::TaskExecution)
            .unwrap();
        assert_eq!(task_stats.count, 2);
        assert_eq!(task_stats.success_count, 2);
    }

    #[test]
    fn test_scenario_24h_simulation() {
        let (_dir, writer) = make_writer();

        // 模拟 24 小时运行: 100 个任务, 每个任务平均 2 次 attempt
        for task_idx in 0..100 {
            let success = task_idx % 5 != 0; // 20% 失败率
            let action = if task_idx % 3 == 0 {
                TraceAction::TaskExecution
            } else {
                TraceAction::FixAttempt
            };

            writer
                .trace(
                    action,
                    Some(task_idx / 10),
                    Some(task_idx % 10),
                    Some(&format!("任务{}", task_idx)),
                    "input",
                    "output",
                    1000 + task_idx as u64 * 10,
                    success,
                    if success { None } else { Some("编译失败") },
                )
                .unwrap();
        }

        let summary = writer.summary();
        assert_eq!(summary.total_entries, 100);
        assert!(summary.timeline.len() <= 100);

        // 验证报告可读
        let report = summary.to_report();
        assert!(report.contains("DevTrace 开发追踪报告"));
        assert!(report.contains("总条目: 100"));
    }

    // ===== DevTraceWriter Clone 测试 =====

    #[test]
    fn test_writer_clone_writes_to_same_file() {
        let (_dir, writer) = make_writer();

        // 克隆 writer
        let cloned = writer.clone();

        // 两个 writer 写入同一文件
        writer
            .trace(
                TraceAction::Planning,
                None,
                None,
                None,
                "原始",
                "输出1",
                100,
                true,
                None,
            )
            .unwrap();
        cloned
            .trace(
                TraceAction::TaskExecution,
                None,
                None,
                None,
                "克隆",
                "输出2",
                200,
                true,
                None,
            )
            .unwrap();

        // 验证两条记录都在同一文件中
        let entries = writer.read_all().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].action, TraceAction::Planning);
        assert_eq!(entries[0].input_summary, "原始");
        assert_eq!(entries[1].action, TraceAction::TaskExecution);
        assert_eq!(entries[1].input_summary, "克隆");
    }

    #[test]
    fn test_writer_clone_independent_summary() {
        let (_dir, writer) = make_writer();
        let cloned = writer.clone();

        // 原始 writer 写入
        writer
            .trace(
                TraceAction::Planning,
                None,
                None,
                None,
                "原始",
                "输出",
                100,
                true,
                None,
            )
            .unwrap();

        // 克隆 writer 读取 (应该看到原始 writer 写入的内容)
        let entries = cloned.read_all().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, TraceAction::Planning);
    }

    #[test]
    fn test_writer_clone_path_matches() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
        let writer = DevTraceWriter::new(dir.path());
        let cloned = writer.clone();
        assert_eq!(writer.trace_path, cloned.trace_path);
    }

    // ===== TraceAction::PerformanceStats 测试 =====

    #[test]
    fn test_trace_action_performance_stats_display() {
        assert_eq!(
            TraceAction::PerformanceStats.to_string(),
            "PerformanceStats"
        );
    }

    #[test]
    fn test_trace_action_performance_stats_description() {
        assert_eq!(TraceAction::PerformanceStats.description(), "性能统计");
    }

    #[test]
    fn test_trace_action_performance_stats_in_all() {
        let all = TraceAction::all();
        assert!(all.contains(&TraceAction::PerformanceStats));
        assert_eq!(all.len(), 16);
    }

    #[test]
    fn test_trace_action_performance_stats_serde() {
        let action = TraceAction::PerformanceStats;
        let json = serde_json::to_string(&action).unwrap();
        assert_eq!(json, "\"PerformanceStats\"");

        let parsed: TraceAction = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, action);
    }

    #[test]
    fn test_writer_performance_stats_trace() {
        let (_dir, writer) = make_writer();
        writer
            .trace(
                TraceAction::PerformanceStats,
                None,
                None,
                None,
                "性能统计 [0] Zai",
                "发送:10 成功:8 失败:2 成功率:80.0%",
                0,
                true,
                None,
            )
            .unwrap();

        let entries = writer.read_all().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, TraceAction::PerformanceStats);
        assert!(entries[0].input_summary.contains("Zai"));
        assert!(entries[0].output_summary.contains("发送:10"));
        assert!(entries[0].output_summary.contains("成功率:80.0%"));
    }

    #[test]
    fn test_writer_performance_stats_in_summary() {
        let (_dir, writer) = make_writer();

        // 写入多种操作类型, 包括 PerformanceStats
        writer
            .trace(
                TraceAction::TaskExecution,
                Some(0),
                Some(0),
                Some("任务"),
                "input",
                "output",
                100,
                true,
                None,
            )
            .unwrap();
        writer
            .trace(
                TraceAction::PerformanceStats,
                None,
                None,
                None,
                "统计 [0]",
                "发送:5",
                0,
                true,
                None,
            )
            .unwrap();

        let summary = writer.summary();
        assert_eq!(summary.total_entries, 2);

        let stats = summary
            .get_action_stats(TraceAction::PerformanceStats)
            .unwrap();
        assert_eq!(stats.count, 1);
        assert_eq!(stats.success_count, 1);
    }

    #[test]
    fn test_writer_performance_stats_in_report() {
        let (_dir, writer) = make_writer();
        writer
            .trace(
                TraceAction::PerformanceStats,
                None,
                None,
                None,
                "统计",
                "发送:5",
                0,
                true,
                None,
            )
            .unwrap();

        let summary = writer.summary();
        let report = summary.to_report();
        assert!(report.contains("性能统计"));
    }

    #[test]
    fn test_writer_all_action_types_includes_performance_stats() {
        let (_dir, writer) = make_writer();

        for action in TraceAction::all() {
            writer
                .trace(action, None, None, None, "input", "output", 100, true, None)
                .unwrap();
        }

        let entries = writer.read_all().unwrap();
        assert_eq!(entries.len(), 16); // 所有 16 种操作类型

        // 确保 PerformanceStats 被包含
        let has_performance_stats = entries
            .iter()
            .any(|e| e.action == TraceAction::PerformanceStats);
        assert!(has_performance_stats);
    }

    #[test]
    fn test_writer_clone_shared_write_for_failover_simulation() {
        // 模拟 FailoverChatClient + Orchestrator 共享同一 DevTraceWriter 的场景:
        // - Orchestrator 通过原始 writer 写入 Planning/TaskExecution 等
        // - FailoverChatClient 通过 cloned writer 写入 HealthCheck/SiteFailover
        // - 两者写入同一文件
        let (_dir, writer) = make_writer();
        let failover_writer = writer.clone();

        // Orchestrator 写入
        writer
            .trace(
                TraceAction::Planning,
                None,
                None,
                None,
                "拆解目标",
                "3阶段5任务",
                5000,
                true,
                None,
            )
            .unwrap();
        writer
            .trace(
                TraceAction::TaskExecution,
                Some(0),
                Some(0),
                Some("初始化"),
                "创建项目",
                "main.rs",
                3000,
                true,
                None,
            )
            .unwrap();

        // FailoverChatClient 写入 (通过克隆的 writer)
        failover_writer
            .trace(
                TraceAction::HealthCheck,
                None,
                None,
                None,
                "检查 [0] Zai",
                "Healthy",
                50,
                true,
                None,
            )
            .unwrap();
        failover_writer
            .trace(
                TraceAction::SiteFailover,
                None,
                None,
                None,
                "切换 [0] Zai → [1] DeepSeek",
                "成功",
                0,
                true,
                None,
            )
            .unwrap();
        failover_writer
            .trace(
                TraceAction::PerformanceStats,
                None,
                None,
                None,
                "统计 [0] Zai",
                "发送:5 成功:4",
                0,
                true,
                None,
            )
            .unwrap();

        // 验证所有条目在同一文件中
        let entries = writer.read_all().unwrap();
        assert_eq!(entries.len(), 5);
        assert_eq!(entries[0].action, TraceAction::Planning);
        assert_eq!(entries[1].action, TraceAction::TaskExecution);
        assert_eq!(entries[2].action, TraceAction::HealthCheck);
        assert_eq!(entries[3].action, TraceAction::SiteFailover);
        assert_eq!(entries[4].action, TraceAction::PerformanceStats);

        // 验证 summary 包含所有类型
        let summary = writer.summary();
        assert!(summary.by_action.contains_key(&TraceAction::HealthCheck));
        assert!(summary.by_action.contains_key(&TraceAction::SiteFailover));
        assert!(summary
            .by_action
            .contains_key(&TraceAction::PerformanceStats));
    }
}
