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
//! | [`browser_launcher`] | Browser auto-detection, launch, and lifecycle management |
//! | [`config`] | TOML configuration + environment variable override |
//! | [`workspace`] | Workspace management (files/snapshots) |
//! | [`extract`] | Code extraction from AI responses |
//! | [`clarify`] | Heuristic autonomous clarification |
//! | [`llm_clarify`] | LLM-enhanced clarification (Ollama) |
//! | [`failover_chat`] | Multi-site automatic failover |
//! | [`site_health`] | Website health checking (triple-check mechanism) |
//! | [`proxy_pool`] | Proxy IP pool with auto-refresh (Mixin pattern) |
//! | [`response_handler`] | Handler chain for AI response processing (callback pattern) |
//! | [`trace_store`] | Pluggable trace storage backend (factory pattern) |
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
pub mod ax_snapshot;
pub mod browser;
pub mod browser_launcher;
pub mod cache_tuning;
pub mod cancellation_token;
pub mod cdp;
pub mod chat;
pub mod clarify;
pub mod config;
pub mod connection_monitor;
pub mod context_handoff;
pub mod deadline;
pub mod dev_trace;
pub mod error_diagnosis;
pub mod error_search;
pub mod extract;
pub mod failover_chat;
pub mod interaction;
pub mod language;
pub mod live_continuation;
pub mod llm_clarify;
pub mod loop_detector;
pub mod memory;
pub mod orchestrator;
pub mod package;
pub mod prompt_builder;
pub mod proxy_pool;
pub mod radix_tree;
pub mod response_handler;
pub mod search_cache;
pub mod search_quality;
pub mod site_health;
pub mod slash_command;
pub mod stealth_patches;
pub mod steer_reminder;
pub mod task_graph;
pub mod testrunner;
pub mod trace_store;
pub mod traits;
pub mod watchdog;
pub mod web_tool;
pub mod workspace;

pub use auto_recovery::{
    assess_recovery_urgency, compute_backoff_schedule, compute_recovery_success_rate,
    decide_recovery_action, estimate_max_recovery_secs, format_recovery_rate, make_failed_result,
    make_success_result, recovery_efficiency, result_error, select_recovery_strategy,
    should_continue_retrying, AutoRecovery, AutoRecoverySummary, BackoffStrategy, RecoveryAction,
    RecoveryConfig, RecoveryResult, RecoveryStrategy, RecoveryUrgency,
};
pub use ax_snapshot::{
    build_snapshot_js, is_content_role, is_interactive_role, is_known_role, is_structural_role,
    AxNode, AxSnapshot, SnapshotOptions, CONTENT_ROLES, INTERACTIVE_ROLES, STRUCTURAL_ROLES,
};
pub use browser::{BrowserManager, ChatTab, SiteType};
pub use browser_launcher::{
    browser_exists, browser_from_env, browser_name, build_launch_args, connect_existing_browser,
    default_user_data_dir, detect_browser_paths, find_available_port_sync, find_browser,
    is_browser_running, is_port_available_sync, BrowserLauncher,
};
pub use cache_tuning::{
    compute_new_ttl, has_sufficient_data, make_tuning_decision, should_adjust_ttl,
    should_disable_cache, CacheTuner, CacheTuningConfig, CacheTuningDecision, CacheTuningHistory,
    TuningAction, DEFAULT_DISABLE_THRESHOLD, DEFAULT_INCREASE_TTL_THRESHOLD, DEFAULT_MAX_TTL_SECS,
    DEFAULT_MIN_SAMPLES, DEFAULT_MIN_TTL_SECS, DEFAULT_REDUCE_TTL_THRESHOLD,
    DEFAULT_TTL_INCREASE_FACTOR, DEFAULT_TTL_REDUCE_FACTOR, TUNING_HISTORY_FILENAME,
};
pub use cancellation_token::{CancelError, CancellationToken, CancellationTokenSource};
pub use chat::{ChatMessage, ChatSession, TimeoutConfig};
pub use clarify::HeuristicClarificationChecker;
pub use config::{
    apply_env_overrides, default_config_path, expand_tilde, load_config, load_from_file,
    parse_bool, BrowserConfig, ChatConfig, ForgeConfig, RecoveryConfig as ConfigRecovery,
    StorageConfig as ConfigStorage,
};
pub use connection_monitor::{
    calculate_monitor_success_rate, classify_connection_severity, compute_next_check_delay,
    determine_health_level, format_monitor_success_rate, format_recovery_event_line, format_uptime,
    is_chrome_crashed_status, should_trigger_recovery, ConnectionMonitor, ConnectionMonitorSummary,
    ConnectionSeverity, ConnectionStatus, HealthLevel, MonitorConfig, RecoveryEvent,
};
pub use context_handoff::{
    build_phase_summary, build_task_summary, collect_completed_tasks,
    format_completed_tasks_section, format_error_code_badge, format_error_history_section,
    format_known_issues_section, format_phase_section, format_recent_errors_section,
    format_task_section, format_workspace_files_section, is_workspace_file_included,
    should_trigger_handoff, truncate_text, ContextHandoff,
};
pub use deadline::{no_deadline, Deadline};
pub use dev_trace::{
    build_cache_fix_correlation, build_cache_summary, build_cache_tuning_history_summary,
    build_search_quality_history_summary, build_search_quality_stats, build_timeline,
    calculate_success_rate, find_next_compile_check, format_action_stats_line,
    format_duration_human, format_success_rate_percent, format_timeline_line,
    group_entries_by_action, is_cache_miss, is_search_failure, parse_cache_entry,
    parse_cache_hit_duration, parse_incremental_entry, parse_jsonl_line, ActionStats,
    CacheEntryInfo, CacheFixCorrelation, CacheStatsSummary, CacheTuningHistorySummary,
    CacheTuningSummary, DevTraceEntry, DevTraceSummary, DevTraceWriter, IncrementalStats,
    SearchQualityHistorySummary, SearchQualityStats, TimelineEntry, TraceAction,
};
pub use error_diagnosis::{
    DiagnosisContext, DiagnosisResult, ErrorCategory, ErrorDiagnoser, ErrorHistory, ErrorPattern,
    HeuristicErrorDiagnoser, HybridErrorDiagnoser, LlmErrorDiagnoser, MockErrorDiagnoser,
};
pub use error_search::{
    build_error_search_query, extract_error_keywords, format_search_results_section,
    should_search_errors, truncate_search_results,
};
pub use extract::{extract_files, DefaultExtractor};
pub use failover_chat::{FailoverChatClient, SitePerformanceStats};
pub use interaction::{AutoApprove, CliInteraction, MockCallCounts, MockInteraction};
pub use language::{
    detect_language, GoAdapter, MultiLanguageTestRunner, NodeAdapter, PythonAdapter, RustAdapter,
};
pub use live_continuation::{
    compute_diff, compute_message_ids, deduplicate, find_duplicates, IncrementalResult,
    LiveContinuation, MessageId, MessageTracker,
};
pub use llm_clarify::{
    build_default_follow_up_message, build_judge_prompt_text, classify_llm_failure,
    is_duplicate_question, parse_llm_judge_result, should_retry_llm, truncate_response,
    HybridClarificationChecker, LlmClarificationChecker, LlmClient, LlmFailureType, OllamaClient,
};
pub use loop_detector::{
    build_skip_prompt_text, build_strategy_change_prompt_text, collect_repeated_files,
    collect_repeated_signatures, format_repeated_summary, has_any_repeated_codes,
    has_any_repeated_files, has_any_repeated_signatures, make_error_signature, should_detect_loop,
    should_skip_task, ErrorRound, LoopDetector,
};
pub use memory::{Memory, RequirementChange};
pub use orchestrator::Orchestrator;
pub use prompt_builder::SystemPrompt;
pub use proxy_pool::{
    build_reqwest_proxy, is_valid_proxy_url, load_proxies_from_env, ProxyConfig, ProxyPool,
    ProxyRefresh,
};
pub use radix_tree::{
    common_prefix_length, compute_delta_with_stats, compute_fingerprints,
    compute_fingerprints_owned, ConversationTracker, DeltaResult, MessageFingerprint, RadixTree,
};
pub use response_handler::{
    CodeExtractorHandler, HandlerChain, HandlerResult, MemoryUpdaterHandler, ResponseHandler,
    TaskContext, TraceWriterHandler,
};
pub use search_cache::{
    build_cache_key, find_oldest_key, format_cache_stats, is_cache_expired,
    normalize_query_for_cache, CacheStats, CachedSearchEntry, SearchCache, DEFAULT_CACHE_MAX_SIZE,
    DEFAULT_CACHE_TTL_SECS,
};
pub use search_quality::{
    compute_search_quality_decision, has_sufficient_search_data, should_disable_search,
    SearchQualityAction, SearchQualityConfig, SearchQualityDecision, SearchQualityEvaluator,
    SearchQualityHistory, DEFAULT_BENEFICIAL_THRESHOLD, SEARCH_QUALITY_HISTORY_FILENAME,
};
pub use site_health::{
    build_detailed_check_js, calculate_health_rate, check_all_tabs, check_and_log,
    classify_health_severity, compute_health_check_interval, determine_failover_priority,
    format_health_rate, format_health_result_line, interpret_detailed_result,
    interpret_health_json, select_best_healthy_tab, should_skip_tab, DetailedHealthStatus,
    HealthCheckJson, HealthCheckResult, HealthSeverity, SiteFailover, SiteHealthChecker,
    SiteHealthStatus,
};
pub use slash_command::{
    classify_command_action, compute_execution_rate, deduplicate_commands, extract_keyword_at,
    format_summary_report, is_boundary_char, is_code_block_boundary, is_known_keyword,
    is_prefix_boundary, strip_command_from_text, SlashCommand, SlashCommandAction,
    SlashCommandSummary,
};
pub use stealth_patches::{
    build_bootstrap_script, needs_stealth_patches, patch_names, validate_bootstrap_script,
};
pub use steer_reminder::{
    check_remind_needed, extract_phase_name, extract_task_name, format_constraints_section,
    format_goal_line, format_phase_task_line, SteerReminder,
};
pub use task_graph::{TaskGraph, TaskGraphError};
pub use testrunner::{CargoTestRunner, E2ETestCase, E2ETestResult, E2ETestSummary};
pub use trace_store::{
    create_trace_store, JsonTraceStore, JsonlTraceStore, StorageBackend,
    StorageConfig as TraceStorageConfig, TraceEntry, TraceStore,
};
pub use traits::{
    ChatClient, ChatResult, ClarificationChecker, ClarificationContext, ClarificationResult,
    Failoverable, FileExtractor, FixContext, HumanInteraction, Language, LanguageAdapter,
    PhaseInfo, PlanInfo, TaskAction, TaskInfo, TestRunner,
};
pub use watchdog::{
    event_priority, should_handle_event, should_trigger_auto_recovery, CaptchaWatchdog,
    ChromeWatchdog, PopupWatchdog, Watchdog, WatchdogRegistry,
};
pub use workspace::Workspace;
