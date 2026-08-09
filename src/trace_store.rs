//! 存储后端工厂模式 — 借鉴 MediaCrawler XhsStoreFactory 设计
//!
//! 为 `dev_trace`、`memory`、`error_history` 提供可插拔的存储后端。
//! 当前 Forge 的 DevTraceWriter 只支持 JSONL，本模块定义 trait 抽象，
//! 未来可以扩展 SQLite、PostgreSQL 等后端，24h 压测时方便查询分析。
//!
//! ## 设计
//!
//! - [`TraceStore`] trait: 定义写入/查询接口 (ISP: 小而精)
//! - [`JsonlTraceStore`]: JSONL 文件存储 (当前默认)
//! - [`create_trace_store`]: 工厂函数, 根据配置创建存储后端
//!
//! ## 示例
//!
//! ```
//! use forge::trace_store::{create_trace_store, StorageBackend, StorageConfig};
//!
//! let config = StorageConfig {
//!     backend: StorageBackend::Jsonl,
//!     path: std::path::PathBuf::from("/tmp/trace.jsonl"),
//! };
//! let store = create_trace_store(&config);
//! ```

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::debug;

// ============================================================================
//  StorageConfig — 存储配置
// ============================================================================

/// 存储后端类型
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum StorageBackend {
    /// JSONL 文件存储 (默认, 每行一个 JSON 对象)
    #[default]
    Jsonl,
    /// JSON 文件存储 (整个文件一个 JSON 数组)
    Json,
    /// SQLite 数据库 (未来扩展)
    Sqlite,
    /// PostgreSQL 数据库 (未来扩展)
    Postgres,
}

impl std::fmt::Display for StorageBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Jsonl => write!(f, "JSONL"),
            Self::Json => write!(f, "JSON"),
            Self::Sqlite => write!(f, "SQLite"),
            Self::Postgres => write!(f, "PostgreSQL"),
        }
    }
}

/// 存储配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    /// 存储后端类型
    pub backend: StorageBackend,
    /// 存储路径 (文件路径或数据库连接字符串)
    pub path: PathBuf,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            backend: StorageBackend::default(),
            path: PathBuf::from(".forge/devtrace.jsonl"),
        }
    }
}

// ============================================================================
//  TraceEntry — 可存储的 Trace 条目 (简化版, 兼容 DevTraceEntry)
// ============================================================================

/// 可存储的 Trace 条目 — 与 `dev_trace::DevTraceEntry` 兼容的简化结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEntry {
    /// 时间戳 (ISO 8601)
    pub timestamp: String,
    /// 动作类型 (如 "send_message", "extract_code", "compile")
    pub action: String,
    /// 阶段 (如 "plan", "develop", "fix")
    pub phase: String,
    /// 任务名称
    pub task: String,
    /// 是否成功
    pub success: bool,
    /// 耗时 (毫秒)
    pub duration_ms: u64,
    /// 附加信息 (JSON 字符串)
    pub detail: Option<String>,
}

impl TraceEntry {
    /// 创建一个新的 Trace 条目
    pub fn new(action: &str, phase: &str, task: &str) -> Self {
        Self {
            timestamp: chrono::Utc::now().to_rfc3339(),
            action: action.to_string(),
            phase: phase.to_string(),
            task: task.to_string(),
            success: true,
            duration_ms: 0,
            detail: None,
        }
    }

    /// 设置成功状态
    pub fn with_success(mut self, success: bool) -> Self {
        self.success = success;
        self
    }

    /// 设置耗时
    pub fn with_duration(mut self, ms: u64) -> Self {
        self.duration_ms = ms;
        self
    }

    /// 设置附加信息
    pub fn with_detail(mut self, detail: &str) -> Self {
        self.detail = Some(detail.to_string());
        self
    }
}

// ============================================================================
//  TraceStore — 存储后端 trait (DIP: 依赖抽象)
// ============================================================================

/// Trace 存储后端 trait — 借鉴 MediaCrawler AbstractStore 设计
///
/// 实现此 trait 可创建新的存储后端 (如 SQLite、PostgreSQL)。
/// 工厂函数 [`create_trace_store`] 根据配置创建具体实现。
#[async_trait]
pub trait TraceStore: Send + Sync {
    /// 写入一条 Trace 条目
    async fn write_entry(&self, entry: &TraceEntry) -> Result<()>;

    /// 批量写入 Trace 条目
    async fn write_batch(&self, entries: &[TraceEntry]) -> Result<()> {
        for entry in entries {
            self.write_entry(entry).await?;
        }
        Ok(())
    }

    /// 查询所有 Trace 条目 (未来可扩展为带过滤条件的查询)
    async fn query_all(&self) -> Result<Vec<TraceEntry>>;

    /// 获取存储后端类型
    fn backend_type(&self) -> StorageBackend;
}

// ============================================================================
//  JsonlTraceStore — JSONL 文件存储实现
// ============================================================================

/// JSONL 文件存储 — 每行一个 JSON 对象
///
/// 这是当前 Forge 的默认存储方式, 兼容现有的 `.forge/devtrace.jsonl` 文件格式。
pub struct JsonlTraceStore {
    path: PathBuf,
}

impl JsonlTraceStore {
    /// 创建新的 JSONL 存储实例
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

#[async_trait]
impl TraceStore for JsonlTraceStore {
    async fn write_entry(&self, entry: &TraceEntry) -> Result<()> {
        let line = serde_json::to_string(entry)?;
        let line_len = line.len();
        // 异步写入 (使用 tokio 的文件 API)
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            use std::io::Write;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?;
            writeln!(file, "{}", line)?;
            Ok(())
        })
        .await??;
        debug!("JSONL 写入: {} ({} bytes)", self.path.display(), line_len);
        Ok(())
    }

    async fn query_all(&self) -> Result<Vec<TraceEntry>> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<TraceEntry>> {
            if !path.exists() {
                return Ok(vec![]);
            }
            let content = std::fs::read_to_string(&path)?;
            let entries: Vec<TraceEntry> = content
                .lines()
                .filter(|l| !l.is_empty())
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect();
            Ok(entries)
        })
        .await?
    }

    fn backend_type(&self) -> StorageBackend {
        StorageBackend::Jsonl
    }
}

// ============================================================================
//  JsonTraceStore — JSON 文件存储实现
// ============================================================================

/// JSON 文件存储 — 整个文件是一个 JSON 数组
pub struct JsonTraceStore {
    path: PathBuf,
}

impl JsonTraceStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

#[async_trait]
impl TraceStore for JsonTraceStore {
    async fn write_entry(&self, entry: &TraceEntry) -> Result<()> {
        // 读取现有条目
        let mut entries = self.query_all().await.unwrap_or_default();
        entries.push(entry.clone());

        // 写入全部
        let json = serde_json::to_string_pretty(&entries)?;
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, json)?;
            Ok(())
        })
        .await??;
        Ok(())
    }

    async fn query_all(&self) -> Result<Vec<TraceEntry>> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<TraceEntry>> {
            if !path.exists() {
                return Ok(vec![]);
            }
            let content = std::fs::read_to_string(&path)?;
            let entries: Vec<TraceEntry> = serde_json::from_str(&content).unwrap_or_default();
            Ok(entries)
        })
        .await?
    }

    fn backend_type(&self) -> StorageBackend {
        StorageBackend::Json
    }
}

// ============================================================================
//  工厂函数 — 借鉴 MediaCrawler create_store
// ============================================================================

/// 根据配置创建 Trace 存储后端
///
/// # 示例
///
/// ```
/// use forge::trace_store::{create_trace_store, StorageBackend, StorageConfig};
/// use std::path::PathBuf;
///
/// let config = StorageConfig {
///     backend: StorageBackend::Jsonl,
///     path: PathBuf::from("/tmp/trace.jsonl"),
/// };
/// let store = create_trace_store(&config);
/// assert_eq!(store.backend_type(), StorageBackend::Jsonl);
/// ```
pub fn create_trace_store(config: &StorageConfig) -> Box<dyn TraceStore> {
    match config.backend {
        StorageBackend::Jsonl => {
            debug!("创建 JSONL 存储后端: {}", config.path.display());
            Box::new(JsonlTraceStore::new(config.path.clone()))
        }
        StorageBackend::Json => {
            debug!("创建 JSON 存储后端: {}", config.path.display());
            Box::new(JsonTraceStore::new(config.path.clone()))
        }
        StorageBackend::Sqlite => {
            debug!("SQLite 存储后端尚未实现, 回退到 JSONL");
            Box::new(JsonlTraceStore::new(config.path.with_extension("jsonl")))
        }
        StorageBackend::Postgres => {
            debug!("PostgreSQL 存储后端尚未实现, 回退到 JSONL");
            Box::new(JsonlTraceStore::new(config.path.with_extension("jsonl")))
        }
    }
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ===== StorageBackend 测试 =====

    #[test]
    fn test_storage_backend_default() {
        assert_eq!(StorageBackend::default(), StorageBackend::Jsonl);
    }

    #[test]
    fn test_storage_backend_display() {
        assert_eq!(format!("{}", StorageBackend::Jsonl), "JSONL");
        assert_eq!(format!("{}", StorageBackend::Json), "JSON");
        assert_eq!(format!("{}", StorageBackend::Sqlite), "SQLite");
        assert_eq!(format!("{}", StorageBackend::Postgres), "PostgreSQL");
    }

    #[test]
    fn test_storage_backend_serde() {
        let json = serde_json::to_string(&StorageBackend::Sqlite).unwrap();
        let parsed: StorageBackend = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, StorageBackend::Sqlite);
    }

    // ===== StorageConfig 测试 =====

    #[test]
    fn test_storage_config_default() {
        let config = StorageConfig::default();
        assert_eq!(config.backend, StorageBackend::Jsonl);
        assert!(config.path.to_string_lossy().contains("devtrace"));
    }

    // ===== TraceEntry 测试 =====

    #[test]
    fn test_trace_entry_new() {
        let entry = TraceEntry::new("send_message", "develop", "task1");
        assert_eq!(entry.action, "send_message");
        assert_eq!(entry.phase, "develop");
        assert_eq!(entry.task, "task1");
        assert!(entry.success);
        assert_eq!(entry.duration_ms, 0);
        assert!(entry.detail.is_none());
    }

    #[test]
    fn test_trace_entry_with_success() {
        let entry = TraceEntry::new("compile", "fix", "task2").with_success(false);
        assert!(!entry.success);
    }

    #[test]
    fn test_trace_entry_with_duration() {
        let entry = TraceEntry::new("test", "validate", "task3").with_duration(5000);
        assert_eq!(entry.duration_ms, 5000);
    }

    #[test]
    fn test_trace_entry_with_detail() {
        let entry = TraceEntry::new("extract", "develop", "task4").with_detail("5 files extracted");
        assert_eq!(entry.detail, Some("5 files extracted".to_string()));
    }

    #[test]
    fn test_trace_entry_builder_chain() {
        let entry = TraceEntry::new("send", "plan", "task5")
            .with_success(true)
            .with_duration(100)
            .with_detail("ok");
        assert!(entry.success);
        assert_eq!(entry.duration_ms, 100);
        assert_eq!(entry.detail.as_deref(), Some("ok"));
    }

    #[test]
    fn test_trace_entry_serde() {
        let entry = TraceEntry::new("action", "phase", "task").with_duration(42);
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: TraceEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.action, "action");
        assert_eq!(parsed.duration_ms, 42);
    }

    // ===== create_trace_store 工厂函数测试 =====

    #[test]
    fn test_create_trace_store_jsonl() {
        let config = StorageConfig {
            backend: StorageBackend::Jsonl,
            path: PathBuf::from("/tmp/test_trace.jsonl"),
        };
        let store = create_trace_store(&config);
        assert_eq!(store.backend_type(), StorageBackend::Jsonl);
    }

    #[test]
    fn test_create_trace_store_json() {
        let config = StorageConfig {
            backend: StorageBackend::Json,
            path: PathBuf::from("/tmp/test_trace.json"),
        };
        let store = create_trace_store(&config);
        assert_eq!(store.backend_type(), StorageBackend::Json);
    }

    #[test]
    fn test_create_trace_store_sqlite_fallback() {
        // SQLite 未实现, 应回退到 JSONL
        let config = StorageConfig {
            backend: StorageBackend::Sqlite,
            path: PathBuf::from("/tmp/test_trace.db"),
        };
        let store = create_trace_store(&config);
        assert_eq!(store.backend_type(), StorageBackend::Jsonl);
    }

    #[test]
    fn test_create_trace_store_postgres_fallback() {
        // PostgreSQL 未实现, 应回退到 JSONL
        let config = StorageConfig {
            backend: StorageBackend::Postgres,
            path: PathBuf::from("/tmp/test_trace.pg"),
        };
        let store = create_trace_store(&config);
        assert_eq!(store.backend_type(), StorageBackend::Jsonl);
    }

    // ===== JsonlTraceStore 集成测试 =====

    #[tokio::test]
    async fn test_jsonl_trace_store_write_and_query() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("trace.jsonl");
        let store = JsonlTraceStore::new(path.clone());

        let entry = TraceEntry::new("test_action", "test_phase", "test_task");
        store.write_entry(&entry).await.unwrap();

        let entries = store.query_all().await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, "test_action");
    }

    #[tokio::test]
    async fn test_jsonl_trace_store_write_batch() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("batch.jsonl");
        let store = JsonlTraceStore::new(path);

        let entries = vec![
            TraceEntry::new("action1", "phase1", "task1"),
            TraceEntry::new("action2", "phase2", "task2"),
            TraceEntry::new("action3", "phase3", "task3"),
        ];
        store.write_batch(&entries).await.unwrap();

        let queried = store.query_all().await.unwrap();
        assert_eq!(queried.len(), 3);
        assert_eq!(queried[0].action, "action1");
        assert_eq!(queried[2].action, "action3");
    }

    #[tokio::test]
    async fn test_jsonl_trace_store_query_empty() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("nonexistent.jsonl");
        let store = JsonlTraceStore::new(path);

        let entries = store.query_all().await.unwrap();
        assert!(entries.is_empty());
    }

    #[tokio::test]
    async fn test_jsonl_trace_store_creates_parent_dir() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp
            .path()
            .join("subdir")
            .join("nested")
            .join("trace.jsonl");
        let store = JsonlTraceStore::new(path.clone());

        let entry = TraceEntry::new("test", "test", "test");
        store.write_entry(&entry).await.unwrap();

        assert!(path.exists());
    }

    // ===== JsonTraceStore 集成测试 =====

    #[tokio::test]
    async fn test_json_trace_store_write_and_query() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("trace.json");
        let store = JsonTraceStore::new(path.clone());

        let entry = TraceEntry::new("json_action", "json_phase", "json_task");
        store.write_entry(&entry).await.unwrap();

        let entries = store.query_all().await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, "json_action");
    }

    #[tokio::test]
    async fn test_json_trace_store_multiple_writes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("multi.json");
        let store = JsonTraceStore::new(path);

        for i in 0..5 {
            let entry = TraceEntry::new(&format!("action{}", i), "phase", "task");
            store.write_entry(&entry).await.unwrap();
        }

        let entries = store.query_all().await.unwrap();
        assert_eq!(entries.len(), 5);
    }
}
