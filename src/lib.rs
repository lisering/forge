//! # Forge — Autonomous Software Development Agent
//!
//! Forge is a Rust-based autonomous software development agent that controls
//! Chrome browser via the [Chrome DevTools Protocol (CDP)] to drive web-based
//! AI chat services for multi-stage autonomous software development.
//!
//! ## Architecture Overview
//!
//! ```text
//! Mission → AI decompose → Task-by-task code generation
//!   → Compile/test → Error feedback & fix → Repeat until complete → ZIP
//! ```
//!
//! ## Core Modules
//!
//! | Module | Responsibility |
//! |--------|---------------|
//! | [`orchestrator`] | Core orchestration engine (DIP architecture) |
//! | [`traits`] | DIP trait definitions for all abstractions |
//! | [`cdp`] | CDP WebSocket low-level connection |
//! | [`chat`] | Chat page operations (send/wait/streaming) |
//! | [`browser`] | Browser manager (tab discovery, site detection) |
//! | [`workspace`] | Workspace management (files/snapshots) |
//! | [`extract`] | Code extraction from AI responses |
//! | [`clarify`] | Heuristic autonomous clarification |
//! | [`llm_clarify`] | LLM-enhanced clarification (Ollama) |
//! | [`failover_chat`] | Multi-site automatic failover |
//! | [`site_health`] | Website health checking |
//! | [`task_graph`] | Task dependency graph (DAG parallelism) |
//! | [`error_diagnosis`] | Intelligent error diagnosis |
//! | [`context_handoff`] | Context handoff for long conversations |
//! | [`dev_trace`] | Structured development tracing |
//!
//! ## Supported AI Chat Sites
//!
//! - `chat.deepseek.com` (primary)
//! - `chat.z.ai` (backup)
//! - `kimi.moonshot.cn`
//! - `tongyi.aliyun.com`
//! - `claude.ai`
//!
//! ## Design Principles
//!
//! - **SOLID**: Especially DIP — the Orchestrator depends on trait abstractions,
//!   enabling core logic testing without Chrome.
//! - **TDD**: Test-Driven Development (Red-Green-Refactor).
//! - **Spec-Driven**: Phase 1 Specification → Phase 2 Implementation → Phase 3 Validation.
//!
//! [Chrome DevTools Protocol (CDP)]: https://chromedevtools.github.io/devtools-protocol/
//!
//! ## Example
//!
//! Forge is primarily a CLI tool — see `src/main.rs` for the entry point.
//! Library modules can be used independently for testing and integration.

pub mod auto_recovery;
pub mod browser;
pub mod cdp;
pub mod chat;
pub mod clarify;
pub mod connection_monitor;
pub mod context_handoff;
pub mod dev_trace;
pub mod error_diagnosis;
pub mod extract;
pub mod failover_chat;
pub mod interaction;
pub mod language;
pub mod llm_clarify;
pub mod loop_detector;
pub mod memory;
pub mod orchestrator;
pub mod package;
pub mod prompt_builder;
pub mod site_health;
pub mod slash_command;
pub mod steer_reminder;
pub mod task_graph;
pub mod testrunner;
pub mod traits;
pub mod workspace;

pub use auto_recovery::{AutoRecovery, BackoffStrategy, RecoveryConfig, RecoveryResult};
pub use browser::{BrowserManager, ChatTab, SiteType};
pub use chat::{ChatMessage, ChatSession, TimeoutConfig};
pub use clarify::HeuristicClarificationChecker;
pub use connection_monitor::{
    ConnectionMonitor, ConnectionMonitorSummary, ConnectionStatus, MonitorConfig, RecoveryEvent,
};
pub use context_handoff::{
    build_phase_summary, build_task_summary, collect_completed_tasks,
    format_completed_tasks_section, format_error_code_badge, format_error_history_section,
    format_known_issues_section, format_phase_section, format_recent_errors_section,
    format_task_section, format_workspace_files_section, is_workspace_file_included,
    should_trigger_handoff, truncate_text, ContextHandoff,
};
pub use dev_trace::{
    ActionStats, DevTraceEntry, DevTraceSummary, DevTraceWriter, TimelineEntry, TraceAction,
};
pub use error_diagnosis::{
    DiagnosisContext, DiagnosisResult, ErrorCategory, ErrorDiagnoser, ErrorHistory, ErrorPattern,
    HeuristicErrorDiagnoser, HybridErrorDiagnoser, LlmErrorDiagnoser, MockErrorDiagnoser,
};
pub use extract::{extract_files, DefaultExtractor};
pub use failover_chat::{FailoverChatClient, SitePerformanceStats};
pub use interaction::{AutoApprove, CliInteraction, MockCallCounts, MockInteraction};
pub use language::{
    detect_language, GoAdapter, MultiLanguageTestRunner, NodeAdapter, PythonAdapter, RustAdapter,
};
pub use llm_clarify::{
    build_default_follow_up_message, build_judge_prompt_text, classify_llm_failure,
    is_duplicate_question, parse_llm_judge_result, should_retry_llm, truncate_response,
    HybridClarificationChecker, LlmClarificationChecker, LlmClient, LlmFailureType, OllamaClient,
};
pub use loop_detector::{ErrorRound, LoopDetector};
pub use memory::{Memory, RequirementChange};
pub use orchestrator::Orchestrator;
pub use prompt_builder::SystemPrompt;
pub use site_health::{
    check_all_tabs, check_and_log, HealthCheckResult, SiteFailover, SiteHealthChecker,
    SiteHealthStatus,
};
pub use slash_command::{SlashCommand, SlashCommandAction, SlashCommandSummary};
pub use steer_reminder::SteerReminder;
pub use task_graph::{TaskGraph, TaskGraphError};
pub use testrunner::{CargoTestRunner, E2ETestCase, E2ETestResult, E2ETestSummary};
pub use traits::{
    ChatClient, ChatResult, ClarificationChecker, ClarificationContext, ClarificationResult,
    Failoverable, FileExtractor, FixContext, HumanInteraction, Language, LanguageAdapter,
    PhaseInfo, PlanInfo, TaskAction, TaskInfo, TestRunner,
};
pub use workspace::Workspace;
