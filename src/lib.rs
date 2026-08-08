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
pub use context_handoff::ContextHandoff;
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
    HybridClarificationChecker, LlmClarificationChecker, LlmClient, OllamaClient,
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
