//! Memory — 对话历史 + 决策记录 + 上下文管理
//!
//! 贯穿整个自主开发过程,记录所有交互和决策
//! 支持持久化到磁盘 (memory.json),实现断点续传

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// 一个开发阶段的定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phase {
    pub id: usize,
    pub name: String,
    pub description: String,
    pub status: PhaseStatus,
    pub tasks: Vec<Task>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum PhaseStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Skipped,
}

/// 一个具体任务
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub phase_id: usize,
    pub name: String,
    pub prompt: String,
    pub status: TaskStatus,
    pub result: Option<String>,
    pub attempts: u32,
    pub files_written: Vec<String>,
    pub test_result: Option<String>,
    /// 最近一次通过 cargo check 的快照ID (用于版本回滚)
    #[serde(default)]
    pub last_good_snapshot: Option<u32>,
    /// 自主追问历史 (记录每次追问的问题, 用于防循环)
    #[serde(default)]
    pub clarifications: Vec<String>,
    /// 依赖的任务 ID 列表 (用于并行任务执行的依赖分析)
    ///
    /// 格式: ["0-1", "0-2"] 表示此任务需要在 0-1 和 0-2 完成后才能执行。
    /// 空列表表示无依赖, 可以并行执行。
    #[serde(default)]
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

/// 对话历史记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationTurn {
    pub role: String, // "user" | "assistant" | "system"
    pub content: String,
    pub timestamp: DateTime<Utc>,
    pub task_id: Option<String>,
    pub summary: String, // 内容摘要 (前100字)
}

/// 决策记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    pub timestamp: DateTime<Utc>,
    pub phase_id: usize,
    pub task_id: Option<String>,
    pub decision: String,
    pub reason: String,
}

/// 需求变更 — 运行中接收的新需求
///
/// 支持在开发过程中动态接收需求变更, Agent 在阶段间检查并调整计划。
/// 变更来源: 文件监听 / CLI 交互式输入 / 自动检测
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequirementChange {
    /// 变更描述
    pub description: String,
    /// 变更时间
    pub timestamp: DateTime<Utc>,
    /// 来源: "file" | "cli" | "auto"
    #[serde(default = "default_change_source")]
    pub source: String,
    /// 是否已处理
    #[serde(default)]
    pub processed: bool,
}

fn default_change_source() -> String {
    "file".to_string()
}

/// 开发记忆体 — 可序列化到磁盘实现断点续传
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    /// Forge 版本号 (用于加载时兼容性校验)
    #[serde(default = "default_forge_version")]
    pub forge_version: String,
    pub goal: String,
    pub phases: Vec<Phase>,
    pub conversations: Vec<ConversationTurn>,
    pub decisions: Vec<Decision>,
    pub current_phase: usize,
    pub current_task: Option<String>,
    /// 当前工作区的文件列表 (每次操作后更新)
    #[serde(default)]
    pub workspace_files: Vec<String>,
    /// 每个任务最大自主追问次数 (防循环)
    #[serde(default = "default_max_clarifications")]
    pub max_clarifications: u32,
    /// 待处理的需求变更列表 (运行中接收的新需求)
    #[serde(default)]
    pub pending_requirement_changes: Vec<RequirementChange>,
}

/// 默认 forge 版本号 (兼容旧格式)
fn default_forge_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// 默认每个任务最大自主追问次数
fn default_max_clarifications() -> u32 {
    2
}

impl Memory {
    pub fn new(goal: &str) -> Self {
        Self {
            forge_version: env!("CARGO_PKG_VERSION").to_string(),
            goal: goal.to_string(),
            phases: Vec::new(),
            conversations: Vec::new(),
            decisions: Vec::new(),
            current_phase: 0,
            current_task: None,
            workspace_files: Vec::new(),
            max_clarifications: 2,
            pending_requirement_changes: Vec::new(),
        }
    }

    /// 添加一个对话轮次
    pub fn add_conversation(&mut self, role: &str, content: &str, task_id: Option<&str>) {
        let summary = content.chars().take(100).collect::<String>();
        self.conversations.push(ConversationTurn {
            role: role.to_string(),
            content: content.to_string(),
            timestamp: Utc::now(),
            task_id: task_id.map(|s| s.to_string()),
            summary,
        });
    }

    /// 添加一个决策
    pub fn add_decision(
        &mut self,
        phase_id: usize,
        task_id: Option<&str>,
        decision: &str,
        reason: &str,
    ) {
        info!("决策 [Phase {}]: {} ({})", phase_id, decision, reason);
        self.decisions.push(Decision {
            timestamp: Utc::now(),
            phase_id,
            task_id: task_id.map(|s| s.to_string()),
            decision: decision.to_string(),
            reason: reason.to_string(),
        });
    }

    /// 设置阶段列表
    pub fn set_phases(&mut self, phases: Vec<Phase>) {
        self.phases = phases;
    }

    /// 获取当前阶段
    pub fn current_phase(&self) -> Option<&Phase> {
        self.phases.get(self.current_phase)
    }

    /// 获取当前阶段 (可变)
    pub fn current_phase_mut(&mut self) -> Option<&mut Phase> {
        self.phases.get_mut(self.current_phase)
    }

    /// 进入下一个阶段
    pub fn advance_phase(&mut self) {
        if self.current_phase < self.phases.len() {
            self.current_phase += 1;
        }
    }

    /// 构建给 AI 的上下文摘要 (最近的对话历史 + 当前状态)
    pub fn build_context(&self, max_turns: usize) -> String {
        let mut ctx = String::new();

        // 终极目标
        ctx.push_str(&format!("终极目标: {}\n\n", self.goal));

        // 当前阶段信息
        if let Some(phase) = self.current_phase() {
            ctx.push_str(&format!(
                "当前阶段: {} - {}\n",
                phase.name, phase.description
            ));
            ctx.push_str(&format!("阶段状态: {:?}\n", phase.status));

            // 已完成的任务
            let completed: Vec<_> = phase
                .tasks
                .iter()
                .filter(|t| t.status == TaskStatus::Completed)
                .collect();
            if !completed.is_empty() {
                ctx.push_str(&format!("已完成任务 ({}):\n", completed.len()));
                for t in completed {
                    ctx.push_str(&format!(
                        "  ✅ {} ({})\n",
                        t.name,
                        t.files_written.join(", ")
                    ));
                }
            }
        }

        // 最近几轮对话的摘要
        let recent: Vec<_> = self.conversations.iter().rev().take(max_turns).collect();
        if !recent.is_empty() {
            ctx.push_str("\n最近对话:\n");
            for turn in recent.iter().rev() {
                ctx.push_str(&format!("  [{}] {}\n", turn.role, turn.summary));
            }
        }

        // 当前工作区文件
        if !self.workspace_files.is_empty() {
            ctx.push_str(&format!(
                "\n当前项目文件 ({}):\n",
                self.workspace_files.len()
            ));
            for f in &self.workspace_files {
                ctx.push_str(&format!("  {}\n", f));
            }
        }

        ctx
    }

    /// 获取所有阶段的执行报告
    pub fn execution_report(&self) -> String {
        let mut report = String::new();
        report.push_str("═══════════════════════════════════════════════════\n");
        report.push_str("  执行报告\n");
        report.push_str("═══════════════════════════════════════════════════\n\n");
        report.push_str(&format!("终极目标: {}\n\n", self.goal));

        for phase in &self.phases {
            report.push_str(&format!(
                "阶段 {}: {} [{:?}]\n",
                phase.id, phase.name, phase.status
            ));
            for task in &phase.tasks {
                let icon = match task.status {
                    TaskStatus::Completed => "✅",
                    TaskStatus::Failed => "❌",
                    TaskStatus::InProgress => "🔄",
                    TaskStatus::Pending => "⏳",
                };
                report.push_str(&format!(
                    "  {} {} ({}次尝试)\n",
                    icon, task.name, task.attempts
                ));
                if !task.files_written.is_empty() {
                    report.push_str(&format!("     文件: {}\n", task.files_written.join(", ")));
                }
                if let Some(snap_id) = &task.last_good_snapshot {
                    report.push_str(&format!("     版本: known good 快照 #{}\n", snap_id));
                }
                if let Some(result) = &task.test_result {
                    report.push_str(&format!("     测试: {}\n", result));
                }
            }
            report.push('\n');
        }

        report.push_str(&format!("总对话轮次: {}\n", self.conversations.len()));
        report.push_str(&format!("总决策数: {}\n", self.decisions.len()));

        report
    }

    /// 保存完整状态到 JSON 文件 (断点续传)
    ///
    /// 序列化所有字段: forge_version, goal, phases, conversations,
    /// decisions, current_phase, current_task, workspace_files
    pub fn save(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        info!(
            "Memory 已保存: {} ({} 阶段, {} 对话, {} 决策)",
            path.display(),
            self.phases.len(),
            self.conversations.len(),
            self.decisions.len()
        );
        Ok(())
    }

    /// 从 JSON 文件加载完整状态 (断点续传)
    ///
    /// 加载时校验 forge 版本号, 不匹配时发出警告但继续加载
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        let json = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("读取 memory.json 失败: {}", e))?;

        let memory: Memory = serde_json::from_str(&json)
            .map_err(|e| anyhow::anyhow!("解析 memory.json 失败: {}", e))?;

        // 版本兼容性校验
        let current_version = env!("CARGO_PKG_VERSION");
        if memory.forge_version != current_version {
            warn!(
                "Memory 版本不匹配: 文件版本 {}, 当前版本 {} — 尝试继续加载",
                memory.forge_version, current_version
            );
        }

        info!(
            "Memory 已加载: {} ({} 阶段, {} 对话, {} 决策)",
            path.display(),
            memory.phases.len(),
            memory.conversations.len(),
            memory.decisions.len()
        );

        Ok(memory)
    }

    /// 计算断点恢复点: 第一个未完成的阶段和任务
    ///
    /// 返回 Some((phase_idx, task_idx)) 表示第一个需要执行的任务,
    /// 返回 None 表示所有阶段都已完成。
    pub fn resume_point(&self) -> Option<(usize, usize)> {
        for (phase_idx, phase) in self.phases.iter().enumerate() {
            if phase.status == PhaseStatus::Completed {
                continue;
            }
            for (task_idx, task) in phase.tasks.iter().enumerate() {
                if task.status != TaskStatus::Completed {
                    return Some((phase_idx, task_idx));
                }
            }
        }
        None
    }

    /// 检查是否所有阶段都已完成
    pub fn all_phases_completed(&self) -> bool {
        !self.phases.is_empty()
            && self
                .phases
                .iter()
                .all(|p| p.status == PhaseStatus::Completed)
    }

    /// 统计已完成的任务数
    pub fn completed_task_count(&self) -> usize {
        self.phases
            .iter()
            .flat_map(|p| &p.tasks)
            .filter(|t| t.status == TaskStatus::Completed)
            .count()
    }

    /// 统计总任务数
    pub fn total_task_count(&self) -> usize {
        self.phases.iter().map(|p| p.tasks.len()).sum()
    }

    // ===== 需求变更管理 =====

    /// 添加一个需求变更
    pub fn add_requirement_change(&mut self, description: &str, source: &str) {
        let change = RequirementChange {
            description: description.to_string(),
            timestamp: Utc::now(),
            source: source.to_string(),
            processed: false,
        };
        info!("需求变更 [{}]: {}", source, description);
        self.pending_requirement_changes.push(change);
    }

    /// 检查是否有待处理的需求变更
    pub fn has_pending_changes(&self) -> bool {
        self.pending_requirement_changes
            .iter()
            .any(|c| !c.processed)
    }

    /// 获取所有待处理的需求变更 (未处理的)
    pub fn pending_changes(&self) -> Vec<&RequirementChange> {
        self.pending_requirement_changes
            .iter()
            .filter(|c| !c.processed)
            .collect()
    }

    /// 将待处理的需求变更格式化为给 AI 的文本
    pub fn pending_changes_summary(&self) -> String {
        let pending: Vec<_> = self.pending_changes();
        if pending.is_empty() {
            return String::new();
        }
        let mut summary = format!("需求变更 ({} 项):\n", pending.len());
        for (i, change) in pending.iter().enumerate() {
            summary.push_str(&format!(
                "  {}. [{}] {}\n",
                i + 1,
                change.source,
                change.description
            ));
        }
        summary
    }

    /// 标记所有待处理的需求变更为已处理
    pub fn mark_changes_processed(&mut self) {
        for change in &mut self.pending_requirement_changes {
            change.processed = true;
        }
    }

    /// 追加新的阶段到计划末尾
    ///
    /// 用于需求变更后, AI 重新规划产生的新阶段。
    pub fn append_phases(&mut self, mut new_phases: Vec<Phase>) {
        let start_id = self.phases.len();
        for phase in &mut new_phases {
            phase.id += start_id;
            for task in &mut phase.tasks {
                task.phase_id = phase.id;
                task.id = format!(
                    "{}-{}",
                    phase.id,
                    task.id.split('-').next_back().unwrap_or("0")
                );
            }
        }
        self.phases.extend(new_phases);
    }

    /// 从需求变更文件加载变更
    ///
    /// 读取指定文件, 每行作为一个需求变更添加。
    /// 如果文件不存在或为空, 不做任何操作。
    pub fn load_changes_from_file(&mut self, path: &std::path::Path) {
        if !path.exists() {
            return;
        }
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                warn!("读取需求变更文件失败: {}", e);
                return;
            }
        };
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            self.add_requirement_change(line, "file");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// 构建一个包含完整状态的测试 Memory
    fn make_full_memory() -> Memory {
        let mut mem = Memory::new("构建一个 CLI 计算器");
        mem.set_phases(vec![
            Phase {
                id: 0,
                name: "初始化".to_string(),
                description: "创建项目结构".to_string(),
                status: PhaseStatus::Completed,
                tasks: vec![Task {
                    id: "0-0".to_string(),
                    phase_id: 0,
                    name: "初始化项目".to_string(),
                    prompt: "创建 Cargo.toml 和 main.rs".to_string(),
                    status: TaskStatus::Completed,
                    result: Some("成功".to_string()),
                    attempts: 1,
                    files_written: vec!["Cargo.toml".to_string(), "src/main.rs".to_string()],
                    test_result: Some("✅ 编译成功".to_string()),
                    last_good_snapshot: Some(1),
                    clarifications: vec![],
                    depends_on: vec![],
                }],
            },
            Phase {
                id: 1,
                name: "功能实现".to_string(),
                description: "实现核心功能".to_string(),
                status: PhaseStatus::InProgress,
                tasks: vec![
                    Task {
                        id: "1-0".to_string(),
                        phase_id: 1,
                        name: "解析输入".to_string(),
                        prompt: "实现参数解析".to_string(),
                        status: TaskStatus::Completed,
                        result: Some("成功".to_string()),
                        attempts: 2,
                        files_written: vec!["src/parser.rs".to_string()],
                        test_result: Some("✅ 测试通过".to_string()),
                        last_good_snapshot: Some(3),
                        clarifications: vec![],
                        depends_on: vec![],
                    },
                    Task {
                        id: "1-1".to_string(),
                        phase_id: 1,
                        name: "计算逻辑".to_string(),
                        prompt: "实现计算功能".to_string(),
                        status: TaskStatus::InProgress,
                        result: None,
                        attempts: 1,
                        files_written: vec!["src/calc.rs".to_string()],
                        test_result: Some("❌ 编译失败".to_string()),
                        last_good_snapshot: None,
                        clarifications: vec![],
                        depends_on: vec![],
                    },
                    Task {
                        id: "1-2".to_string(),
                        phase_id: 1,
                        name: "输出格式化".to_string(),
                        prompt: "格式化输出结果".to_string(),
                        status: TaskStatus::Pending,
                        result: None,
                        attempts: 0,
                        files_written: vec![],
                        test_result: None,
                        last_good_snapshot: None,
                        clarifications: vec![],
                        depends_on: vec![],
                    },
                ],
            },
            Phase {
                id: 2,
                name: "测试和文档".to_string(),
                description: "完善测试和文档".to_string(),
                status: PhaseStatus::Pending,
                tasks: vec![Task {
                    id: "2-0".to_string(),
                    phase_id: 2,
                    name: "编写测试".to_string(),
                    prompt: "编写单元测试".to_string(),
                    status: TaskStatus::Pending,
                    result: None,
                    attempts: 0,
                    files_written: vec![],
                    test_result: None,
                    last_good_snapshot: None,
                    clarifications: vec![],
                    depends_on: vec![],
                }],
            },
        ]);

        mem.current_phase = 1;
        mem.current_task = Some("1-1".to_string());
        mem.workspace_files = vec![
            "Cargo.toml".to_string(),
            "src/main.rs".to_string(),
            "src/parser.rs".to_string(),
            "src/calc.rs".to_string(),
        ];

        mem.add_conversation("user", "请拆解以下目标", None);
        mem.add_conversation("assistant", "好的，我来拆解", None);
        mem.add_conversation("user", "实现计算功能", Some("1-1"));
        mem.add_conversation("assistant", "fn calc() { ... }", Some("1-1"));

        mem.add_decision(0, None, "完成目标拆解", "AI 返回了计划");
        mem.add_decision(1, Some("1-0"), "任务完成", "测试通过");
        mem.add_decision(1, Some("1-1"), "编译失败", "需要修复语法错误");

        mem
    }

    // ===== save / load 往返测试 =====

    #[test]
    fn test_save_load_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("memory.json");

        let original = make_full_memory();
        original.save(&path).unwrap();
        assert!(path.exists(), "memory.json 应存在");

        let loaded = Memory::load(&path).unwrap();

        // 验证基本字段
        assert_eq!(loaded.goal, original.goal);
        assert_eq!(loaded.current_phase, original.current_phase);
        assert_eq!(loaded.current_task, original.current_task);
        assert_eq!(loaded.workspace_files, original.workspace_files);
        assert_eq!(loaded.forge_version, original.forge_version);

        // 验证阶段
        assert_eq!(loaded.phases.len(), original.phases.len());
        assert_eq!(loaded.phases[0].status, PhaseStatus::Completed);
        assert_eq!(loaded.phases[1].status, PhaseStatus::InProgress);
        assert_eq!(loaded.phases[2].status, PhaseStatus::Pending);

        // 验证任务
        assert_eq!(loaded.phases[1].tasks.len(), 3);
        assert_eq!(loaded.phases[1].tasks[0].status, TaskStatus::Completed);
        assert_eq!(loaded.phases[1].tasks[1].status, TaskStatus::InProgress);
        assert_eq!(loaded.phases[1].tasks[2].status, TaskStatus::Pending);
        assert_eq!(
            loaded.phases[1].tasks[0].files_written,
            vec!["src/parser.rs"]
        );
        assert_eq!(loaded.phases[1].tasks[0].last_good_snapshot, Some(3));
        assert_eq!(loaded.phases[1].tasks[1].attempts, 1);
        assert_eq!(
            loaded.phases[1].tasks[1].test_result,
            Some("❌ 编译失败".to_string())
        );

        // 验证对话
        assert_eq!(loaded.conversations.len(), 4);
        assert_eq!(loaded.conversations[0].role, "user");
        assert_eq!(loaded.conversations[1].role, "assistant");
        assert_eq!(loaded.conversations[2].task_id, Some("1-1".to_string()));
        assert_eq!(loaded.conversations[3].content, "fn calc() { ... }");

        // 验证决策
        assert_eq!(loaded.decisions.len(), 3);
        assert_eq!(loaded.decisions[0].phase_id, 0);
        assert_eq!(loaded.decisions[1].task_id, Some("1-0".to_string()));
        assert_eq!(loaded.decisions[2].decision, "编译失败");
    }

    #[test]
    fn test_save_load_empty_memory() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("memory.json");

        let empty = Memory::new("空目标");
        empty.save(&path).unwrap();

        let loaded = Memory::load(&path).unwrap();
        assert_eq!(loaded.goal, "空目标");
        assert!(loaded.phases.is_empty());
        assert!(loaded.conversations.is_empty());
        assert!(loaded.decisions.is_empty());
        assert_eq!(loaded.current_phase, 0);
        assert!(loaded.current_task.is_none());
        assert!(loaded.workspace_files.is_empty());
    }

    #[test]
    fn test_load_nonexistent_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nonexistent.json");
        assert!(Memory::load(&path).is_err());
    }

    #[test]
    fn test_load_corrupt_json() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("corrupt.json");
        std::fs::write(&path, "{ not valid json }}}").unwrap();
        assert!(Memory::load(&path).is_err());
    }

    #[test]
    fn test_save_records_forge_version() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("memory.json");

        let mem = Memory::new("测试");
        mem.save(&path).unwrap();

        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            json["forge_version"].as_str().unwrap(),
            env!("CARGO_PKG_VERSION")
        );
    }

    #[test]
    fn test_load_version_mismatch_still_loads() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("old_version.json");

        // 手动写入旧版本 memory.json
        let json = r#"{"forge_version":"0.0.1","goal":"旧目标","phases":[],"conversations":[],"decisions":[],"current_phase":0,"current_task":null,"workspace_files":[]}"#;
        std::fs::write(&path, json).unwrap();

        // 版本不匹配但应仍能加载
        let loaded = Memory::load(&path).unwrap();
        assert_eq!(loaded.goal, "旧目标");
        assert_eq!(loaded.forge_version, "0.0.1");
    }

    // ===== resume_point 测试 =====

    #[test]
    fn test_resume_point_start() {
        let mut mem = Memory::new("目标");
        mem.set_phases(vec![Phase {
            id: 0,
            name: "阶段1".to_string(),
            description: "".to_string(),
            status: PhaseStatus::Pending,
            tasks: vec![Task {
                id: "0-0".to_string(),
                phase_id: 0,
                name: "任务1".to_string(),
                prompt: "".to_string(),
                status: TaskStatus::Pending,
                result: None,
                attempts: 0,
                files_written: vec![],
                test_result: None,
                last_good_snapshot: None,
                clarifications: vec![],
                depends_on: vec![],
            }],
        }]);
        assert_eq!(mem.resume_point(), Some((0, 0)));
    }

    #[test]
    fn test_resume_point_skip_completed_phase() {
        let mem = make_full_memory();
        // 阶段 0 已完成, 阶段 1 InProgress
        // 阶段 1 任务 0 已完成, 任务 1 InProgress
        assert_eq!(mem.resume_point(), Some((1, 1)));
    }

    #[test]
    fn test_resume_point_all_completed() {
        let mut mem = Memory::new("目标");
        mem.set_phases(vec![Phase {
            id: 0,
            name: "阶段1".to_string(),
            description: "".to_string(),
            status: PhaseStatus::Completed,
            tasks: vec![Task {
                id: "0-0".to_string(),
                phase_id: 0,
                name: "任务1".to_string(),
                prompt: "".to_string(),
                status: TaskStatus::Completed,
                result: None,
                attempts: 1,
                files_written: vec![],
                test_result: None,
                last_good_snapshot: None,
                clarifications: vec![],
                depends_on: vec![],
            }],
        }]);
        assert_eq!(mem.resume_point(), None);
        assert!(mem.all_phases_completed());
    }

    #[test]
    fn test_resume_point_empty_phases() {
        let mem = Memory::new("目标");
        assert_eq!(mem.resume_point(), None);
        assert!(!mem.all_phases_completed());
    }

    #[test]
    fn test_resume_point_skip_completed_tasks_in_phase() {
        let mut mem = Memory::new("目标");
        mem.set_phases(vec![Phase {
            id: 0,
            name: "阶段1".to_string(),
            description: "".to_string(),
            status: PhaseStatus::InProgress,
            tasks: vec![
                Task {
                    id: "0-0".to_string(),
                    phase_id: 0,
                    name: "已完成任务".to_string(),
                    prompt: "".to_string(),
                    status: TaskStatus::Completed,
                    result: None,
                    attempts: 1,
                    files_written: vec![],
                    test_result: None,
                    last_good_snapshot: None,
                    clarifications: vec![],
                    depends_on: vec![],
                },
                Task {
                    id: "0-1".to_string(),
                    phase_id: 0,
                    name: "待执行任务".to_string(),
                    prompt: "".to_string(),
                    status: TaskStatus::Pending,
                    result: None,
                    attempts: 0,
                    files_written: vec![],
                    test_result: None,
                    last_good_snapshot: None,
                    clarifications: vec![],
                    depends_on: vec![],
                },
            ],
        }]);
        assert_eq!(mem.resume_point(), Some((0, 1)));
    }

    // ===== 统计方法测试 =====

    #[test]
    fn test_task_count_stats() {
        let mem = make_full_memory();
        assert_eq!(mem.total_task_count(), 5); // 1 + 3 + 1
        assert_eq!(mem.completed_task_count(), 2); // 0-0 和 1-0
    }

    #[test]
    fn test_all_phases_completed_partial() {
        let mem = make_full_memory();
        assert!(!mem.all_phases_completed());
    }

    // ===== 需求变更管理测试 =====

    #[test]
    fn test_add_requirement_change() {
        let mut mem = Memory::new("test");
        assert!(!mem.has_pending_changes());

        mem.add_requirement_change("添加用户认证功能", "cli");
        assert!(mem.has_pending_changes());
        assert_eq!(mem.pending_requirement_changes.len(), 1);
        assert_eq!(
            mem.pending_requirement_changes[0].description,
            "添加用户认证功能"
        );
        assert_eq!(mem.pending_requirement_changes[0].source, "cli");
        assert!(!mem.pending_requirement_changes[0].processed);
    }

    #[test]
    fn test_multiple_changes() {
        let mut mem = Memory::new("test");
        mem.add_requirement_change("变更1", "file");
        mem.add_requirement_change("变更2", "cli");
        mem.add_requirement_change("变更3", "auto");

        assert!(mem.has_pending_changes());
        assert_eq!(mem.pending_changes().len(), 3);
    }

    #[test]
    fn test_pending_changes_summary() {
        let mut mem = Memory::new("test");
        mem.add_requirement_change("添加登录功能", "cli");
        mem.add_requirement_change("支持多语言", "file");

        let summary = mem.pending_changes_summary();
        assert!(summary.contains("需求变更 (2 项)"));
        assert!(summary.contains("添加登录功能"));
        assert!(summary.contains("支持多语言"));
        assert!(summary.contains("cli"));
        assert!(summary.contains("file"));
    }

    #[test]
    fn test_pending_changes_summary_empty() {
        let mem = Memory::new("test");
        assert!(mem.pending_changes_summary().is_empty());
    }

    #[test]
    fn test_mark_changes_processed() {
        let mut mem = Memory::new("test");
        mem.add_requirement_change("变更1", "file");
        mem.add_requirement_change("变更2", "cli");

        assert!(mem.has_pending_changes());
        mem.mark_changes_processed();
        assert!(!mem.has_pending_changes());
        assert!(mem.pending_requirement_changes.iter().all(|c| c.processed));
    }

    #[test]
    fn test_changes_persisted_in_save_load() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("memory.json");

        let mut mem = Memory::new("test goal");
        mem.add_requirement_change("持久化测试变更", "file");

        mem.save(&path).unwrap();
        let loaded = Memory::load(&path).unwrap();

        assert!(loaded.has_pending_changes());
        assert_eq!(loaded.pending_requirement_changes.len(), 1);
        assert_eq!(
            loaded.pending_requirement_changes[0].description,
            "持久化测试变更"
        );
        assert_eq!(loaded.pending_requirement_changes[0].source, "file");
    }

    #[test]
    fn test_load_changes_from_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("changes.txt");
        std::fs::write(
            &path,
            "# 注释行\n添加用户认证\n\n支持多语言\n# 另一个注释\n优化性能\n",
        )
        .unwrap();

        let mut mem = Memory::new("test");
        mem.load_changes_from_file(&path);

        assert!(mem.has_pending_changes());
        assert_eq!(mem.pending_requirement_changes.len(), 3);
        assert_eq!(
            mem.pending_requirement_changes[0].description,
            "添加用户认证"
        );
        assert_eq!(mem.pending_requirement_changes[1].description, "支持多语言");
        assert_eq!(mem.pending_requirement_changes[2].description, "优化性能");
        assert!(mem
            .pending_requirement_changes
            .iter()
            .all(|c| c.source == "file"));
    }

    #[test]
    fn test_load_changes_from_nonexistent_file() {
        let mut mem = Memory::new("test");
        mem.load_changes_from_file(std::path::Path::new("/nonexistent/file.txt"));
        assert!(!mem.has_pending_changes());
    }

    #[test]
    fn test_append_phases() {
        let mut mem = Memory::new("test");
        mem.set_phases(vec![Phase {
            id: 0,
            name: "阶段1".to_string(),
            description: "".to_string(),
            status: PhaseStatus::Completed,
            tasks: vec![Task {
                id: "0-0".to_string(),
                phase_id: 0,
                name: "任务1".to_string(),
                prompt: "".to_string(),
                status: TaskStatus::Completed,
                result: None,
                attempts: 1,
                files_written: vec![],
                test_result: None,
                last_good_snapshot: None,
                clarifications: vec![],
                depends_on: vec![],
            }],
        }]);

        // 追加新阶段
        let new_phases = vec![Phase {
            id: 0,
            name: "新阶段".to_string(),
            description: "需求变更新增".to_string(),
            status: PhaseStatus::Pending,
            tasks: vec![Task {
                id: "0-0".to_string(),
                phase_id: 0,
                name: "新任务".to_string(),
                prompt: "do something".to_string(),
                status: TaskStatus::Pending,
                result: None,
                attempts: 0,
                files_written: vec![],
                test_result: None,
                last_good_snapshot: None,
                clarifications: vec![],
                depends_on: vec![],
            }],
        }];
        mem.append_phases(new_phases);

        assert_eq!(mem.phases.len(), 2);
        assert_eq!(mem.phases[1].id, 1, "新阶段 ID 应为 1");
        assert_eq!(mem.phases[1].name, "新阶段");
        assert_eq!(mem.phases[1].tasks[0].phase_id, 1);
        assert_eq!(mem.phases[1].tasks[0].id, "1-0");
    }
}
