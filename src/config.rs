//! 配置文件 + 环境变量覆盖 — 借鉴 MediaCrawler 配置分离设计
//!
//! 支持 `~/.forge/config.toml` 配置文件 + 环境变量覆盖。
//! 优先级: CLI 参数 > 环境变量 > 配置文件 > 默认值
//!
//! ## 配置文件示例
//!
//! ```toml
//! [browser]
//! port = 9222
//! auto_launch = false
//! connect_existing = false
//!
//! [chat]
//! default_site = "deepseek"
//! phase1_timeout = 30
//! phase2_timeout = 60
//! phase3_timeout = 45
//!
//! [storage]
//! trace_backend = "jsonl"
//! memory_path = "~/.forge/memory"
//!
//! [recovery]
//! max_retries = 10
//! auto_recovery = true
//! ```

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::{debug, info};

// ============================================================================
//  ForgeConfig — 顶层配置
// ============================================================================

/// Forge 配置 — 顶层配置结构
///
/// 支持 TOML 配置文件 + 环境变量覆盖。
/// 优先级: CLI > 环境变量 > 配置文件 > 默认值
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ForgeConfig {
    /// 浏览器配置
    #[serde(default)]
    pub browser: BrowserConfig,

    /// 聊天配置
    #[serde(default)]
    pub chat: ChatConfig,

    /// 存储配置
    #[serde(default)]
    pub storage: StorageConfig,

    /// 自动恢复配置
    #[serde(default)]
    pub recovery: RecoveryConfig,
}

// ============================================================================
//  BrowserConfig — 浏览器配置
// ============================================================================

/// 浏览器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserConfig {
    /// Chrome 调试端口
    #[serde(default = "default_browser_port")]
    pub port: u16,

    /// 是否自动启动浏览器
    #[serde(default)]
    pub auto_launch: bool,

    /// 是否连接已有浏览器
    #[serde(default)]
    pub connect_existing: bool,

    /// 浏览器路径 (None 则自动检测)
    #[serde(default)]
    pub browser_path: Option<String>,

    /// 用户数据目录
    #[serde(default)]
    pub user_data_dir: Option<String>,
}

fn default_browser_port() -> u16 {
    9222
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            port: 9222,
            auto_launch: false,
            connect_existing: false,
            browser_path: None,
            user_data_dir: None,
        }
    }
}

// ============================================================================
//  ChatConfig — 聊天配置
// ============================================================================

/// 聊天配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatConfig {
    /// 默认网站
    #[serde(default = "default_site")]
    pub default_site: String,

    /// Phase 1 超时 (秒): 等待新 AI 消息出现
    #[serde(default = "default_phase1_timeout")]
    pub phase1_timeout: u64,

    /// Phase 2 超时 (秒): 等待实际回答内容出现
    #[serde(default = "default_phase2_timeout")]
    pub phase2_timeout: u64,

    /// Phase 3 超时 (秒): 等待文本稳定
    #[serde(default = "default_phase3_timeout")]
    pub phase3_timeout: u64,

    /// 卡死检测阈值 (秒, 0=禁用)
    #[serde(default = "default_stuck_threshold")]
    pub stuck_threshold: u64,

    /// 上下文衔接最大对话轮数
    #[serde(default = "default_max_context_turns")]
    pub max_context_turns: usize,

    /// 转向提醒间隔
    #[serde(default = "default_steer_interval")]
    pub steer_interval: usize,

    /// 循环终止检测阈值
    #[serde(default = "default_loop_detection")]
    pub loop_detection: usize,
}

fn default_site() -> String {
    "deepseek".to_string()
}
fn default_phase1_timeout() -> u64 {
    30
}
fn default_phase2_timeout() -> u64 {
    60
}
fn default_phase3_timeout() -> u64 {
    45
}
fn default_stuck_threshold() -> u64 {
    180
}
fn default_max_context_turns() -> usize {
    30
}
fn default_steer_interval() -> usize {
    10
}
fn default_loop_detection() -> usize {
    3
}

impl Default for ChatConfig {
    fn default() -> Self {
        Self {
            default_site: default_site(),
            phase1_timeout: default_phase1_timeout(),
            phase2_timeout: default_phase2_timeout(),
            phase3_timeout: default_phase3_timeout(),
            stuck_threshold: default_stuck_threshold(),
            max_context_turns: default_max_context_turns(),
            steer_interval: default_steer_interval(),
            loop_detection: default_loop_detection(),
        }
    }
}

// ============================================================================
//  StorageConfig — 存储配置
// ============================================================================

/// 存储配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    /// Trace 存储后端 ("jsonl" / "json" / "sqlite" / "postgres")
    #[serde(default = "default_trace_backend")]
    pub trace_backend: String,

    /// Memory 存储路径
    #[serde(default = "default_memory_path")]
    pub memory_path: String,
}

fn default_trace_backend() -> String {
    "jsonl".to_string()
}
fn default_memory_path() -> String {
    "~/.forge/memory".to_string()
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            trace_backend: default_trace_backend(),
            memory_path: default_memory_path(),
        }
    }
}

// ============================================================================
//  RecoveryConfig — 自动恢复配置
// ============================================================================

/// 自动恢复配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryConfig {
    /// 是否启用自动恢复
    #[serde(default = "default_auto_recovery")]
    pub auto_recovery: bool,

    /// 最大重试次数
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,

    /// 多网站自动切换
    #[serde(default)]
    pub auto_failover: bool,

    /// 健康检查间隔
    #[serde(default = "default_health_check_interval")]
    pub health_check_interval: usize,
}

fn default_auto_recovery() -> bool {
    true
}
fn default_max_retries() -> u32 {
    10
}
fn default_health_check_interval() -> usize {
    5
}

impl Default for RecoveryConfig {
    fn default() -> Self {
        Self {
            auto_recovery: default_auto_recovery(),
            max_retries: default_max_retries(),
            auto_failover: false,
            health_check_interval: default_health_check_interval(),
        }
    }
}

// ============================================================================
//  配置加载逻辑
// ============================================================================

/// 配置文件默认路径: `~/.forge/config.toml`
pub fn default_config_path() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".forge").join("config.toml")
    } else {
        PathBuf::from(".forge").join("config.toml")
    }
}

/// 从 TOML 文件加载配置
pub fn load_from_file(path: &PathBuf) -> Result<ForgeConfig> {
    if !path.exists() {
        debug!("配置文件不存在, 使用默认配置: {}", path.display());
        return Ok(ForgeConfig::default());
    }

    let content = std::fs::read_to_string(path)
        .with_context(|| format!("读取配置文件失败: {}", path.display()))?;

    let config: ForgeConfig = toml::from_str(&content)
        .with_context(|| format!("解析配置文件失败: {}", path.display()))?;

    info!("已加载配置文件: {}", path.display());
    Ok(config)
}

/// 从环境变量覆盖配置
///
/// 环境变量命名规则: `FORGE_<SECTION>_<KEY>`
/// - `FORGE_BROWSER_PORT=9223`
/// - `FORGE_CHAT_PHASE1_TIMEOUT=60`
/// - `FORGE_STORAGE_TRACE_BACKEND=sqlite`
/// - `FORGE_RECOVERY_MAX_RETRIES=5`
pub fn apply_env_overrides(config: &mut ForgeConfig) {
    // Browser
    if let Ok(val) = std::env::var("FORGE_BROWSER_PORT") {
        if let Ok(port) = val.parse::<u16>() {
            config.browser.port = port;
            debug!("环境变量覆盖 browser.port = {}", port);
        }
    }
    if let Ok(val) = std::env::var("FORGE_BROWSER_AUTO_LAUNCH") {
        config.browser.auto_launch = parse_bool(&val);
    }
    if let Ok(val) = std::env::var("FORGE_BROWSER_CONNECT_EXISTING") {
        config.browser.connect_existing = parse_bool(&val);
    }
    if let Ok(val) = std::env::var("FORGE_BROWSER_PATH") {
        config.browser.browser_path = Some(val);
    }

    // Chat
    if let Ok(val) = std::env::var("FORGE_CHAT_DEFAULT_SITE") {
        config.chat.default_site = val;
    }
    if let Ok(val) = std::env::var("FORGE_CHAT_PHASE1_TIMEOUT") {
        if let Ok(v) = val.parse() {
            config.chat.phase1_timeout = v;
        }
    }
    if let Ok(val) = std::env::var("FORGE_CHAT_PHASE2_TIMEOUT") {
        if let Ok(v) = val.parse() {
            config.chat.phase2_timeout = v;
        }
    }
    if let Ok(val) = std::env::var("FORGE_CHAT_PHASE3_TIMEOUT") {
        if let Ok(v) = val.parse() {
            config.chat.phase3_timeout = v;
        }
    }

    // Storage
    if let Ok(val) = std::env::var("FORGE_STORAGE_TRACE_BACKEND") {
        config.storage.trace_backend = val;
    }

    // Recovery
    if let Ok(val) = std::env::var("FORGE_RECOVERY_AUTO_RECOVERY") {
        config.recovery.auto_recovery = parse_bool(&val);
    }
    if let Ok(val) = std::env::var("FORGE_RECOVERY_MAX_RETRIES") {
        if let Ok(v) = val.parse() {
            config.recovery.max_retries = v;
        }
    }
    if let Ok(val) = std::env::var("FORGE_RECOVERY_AUTO_FAILOVER") {
        config.recovery.auto_failover = parse_bool(&val);
    }
}

/// 加载完整配置 (配置文件 + 环境变量覆盖)
///
/// 1. 从 `~/.forge/config.toml` 加载 (如果存在)
/// 2. 从环境变量覆盖
/// 3. 返回最终配置
pub fn load_config() -> Result<ForgeConfig> {
    let path = default_config_path();
    let mut config = load_from_file(&path)?;
    apply_env_overrides(&mut config);
    Ok(config)
}

/// 解析布尔值 (支持 "true"/"false"/"1"/"0"/"yes"/"no")
pub fn parse_bool(val: &str) -> bool {
    matches!(val.to_lowercase().as_str(), "true" | "1" | "yes" | "on")
}

/// 将 `~` 展开为用户主目录
pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(stripped) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(stripped);
        }
    }
    PathBuf::from(path)
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ===== 默认值测试 =====

    #[test]
    fn test_default_config() {
        let config = ForgeConfig::default();
        assert_eq!(config.browser.port, 9222);
        assert!(!config.browser.auto_launch);
        assert_eq!(config.chat.default_site, "deepseek");
        assert_eq!(config.chat.phase1_timeout, 30);
        assert!(config.recovery.auto_recovery);
    }

    #[test]
    fn test_browser_config_default() {
        let config = BrowserConfig::default();
        assert_eq!(config.port, 9222);
        assert!(!config.auto_launch);
        assert!(!config.connect_existing);
        assert!(config.browser_path.is_none());
    }

    #[test]
    fn test_chat_config_default() {
        let config = ChatConfig::default();
        assert_eq!(config.phase1_timeout, 30);
        assert_eq!(config.phase2_timeout, 60);
        assert_eq!(config.phase3_timeout, 45);
        assert_eq!(config.stuck_threshold, 180);
        assert_eq!(config.max_context_turns, 30);
    }

    #[test]
    fn test_storage_config_default() {
        let config = StorageConfig::default();
        assert_eq!(config.trace_backend, "jsonl");
        assert!(config.memory_path.contains("forge"));
    }

    #[test]
    fn test_recovery_config_default() {
        let config = RecoveryConfig::default();
        assert!(config.auto_recovery);
        assert_eq!(config.max_retries, 10);
        assert!(!config.auto_failover);
        assert_eq!(config.health_check_interval, 5);
    }

    // ===== parse_bool 测试 =====

    #[test]
    fn test_parse_bool_true_values() {
        assert!(parse_bool("true"));
        assert!(parse_bool("TRUE"));
        assert!(parse_bool("True"));
        assert!(parse_bool("1"));
        assert!(parse_bool("yes"));
        assert!(parse_bool("YES"));
        assert!(parse_bool("on"));
    }

    #[test]
    fn test_parse_bool_false_values() {
        assert!(!parse_bool("false"));
        assert!(!parse_bool("0"));
        assert!(!parse_bool("no"));
        assert!(!parse_bool("off"));
        assert!(!parse_bool(""));
        assert!(!parse_bool("invalid"));
    }

    // ===== expand_tilde 测试 =====

    #[test]
    fn test_expand_tilde_with_home() {
        let path = expand_tilde("~/test");
        // 应该展开为 /home/user/test 或 /Users/user/test
        assert!(!path.to_string_lossy().starts_with("~"));
    }

    #[test]
    fn test_expand_tilde_without_tilde() {
        let path = expand_tilde("/absolute/path");
        assert_eq!(path, PathBuf::from("/absolute/path"));
    }

    #[test]
    fn test_expand_tilde_relative() {
        let path = expand_tilde("relative/path");
        assert_eq!(path, PathBuf::from("relative/path"));
    }

    // ===== default_config_path 测试 =====

    #[test]
    fn test_default_config_path() {
        let path = default_config_path();
        assert!(path.to_string_lossy().contains("forge"));
        assert!(path.to_string_lossy().contains("config.toml"));
    }

    // ===== load_from_file 测试 =====

    #[test]
    fn test_load_from_file_nonexistent() {
        let path = PathBuf::from("/nonexistent/config.toml");
        let config = load_from_file(&path).unwrap();
        // 不存在的文件应返回默认配置
        assert_eq!(config.browser.port, 9222);
    }

    #[test]
    fn test_load_from_file_valid() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[browser]
port = 9223
auto_launch = true

[chat]
phase1_timeout = 60
default_site = "zai"
"#,
        )
        .unwrap();

        let config = load_from_file(&path).unwrap();
        assert_eq!(config.browser.port, 9223);
        assert!(config.browser.auto_launch);
        assert_eq!(config.chat.phase1_timeout, 60);
        assert_eq!(config.chat.default_site, "zai");
    }

    #[test]
    fn test_load_from_file_partial_config() {
        // 只设置部分字段, 其余应使用默认值
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("partial.toml");
        std::fs::write(&path, "[browser]\nport = 8080\n").unwrap();

        let config = load_from_file(&path).unwrap();
        assert_eq!(config.browser.port, 8080);
        // 未设置的字段应使用默认值
        assert_eq!(config.chat.phase1_timeout, 30);
    }

    #[test]
    fn test_load_from_file_invalid_toml() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("invalid.toml");
        std::fs::write(&path, "this is not valid toml {{{{").unwrap();

        let result = load_from_file(&path);
        assert!(result.is_err());
    }

    // ===== apply_env_overrides 测试 =====

    #[test]
    fn test_apply_env_overrides_port() {
        let saved = std::env::var_os("FORGE_BROWSER_PORT");
        std::env::set_var("FORGE_BROWSER_PORT", "9224");

        let mut config = ForgeConfig::default();
        apply_env_overrides(&mut config);
        assert_eq!(config.browser.port, 9224);

        if let Some(v) = saved {
            std::env::set_var("FORGE_BROWSER_PORT", v);
        } else {
            std::env::remove_var("FORGE_BROWSER_PORT");
        }
    }

    #[test]
    fn test_apply_env_overrides_auto_launch() {
        let saved = std::env::var_os("FORGE_BROWSER_AUTO_LAUNCH");
        std::env::set_var("FORGE_BROWSER_AUTO_LAUNCH", "true");

        let mut config = ForgeConfig::default();
        apply_env_overrides(&mut config);
        assert!(config.browser.auto_launch);

        if let Some(v) = saved {
            std::env::set_var("FORGE_BROWSER_AUTO_LAUNCH", v);
        } else {
            std::env::remove_var("FORGE_BROWSER_AUTO_LAUNCH");
        }
    }

    #[test]
    fn test_apply_env_overrides_chat_timeout() {
        let saved = std::env::var_os("FORGE_CHAT_PHASE1_TIMEOUT");
        std::env::set_var("FORGE_CHAT_PHASE1_TIMEOUT", "90");

        let mut config = ForgeConfig::default();
        apply_env_overrides(&mut config);
        assert_eq!(config.chat.phase1_timeout, 90);

        if let Some(v) = saved {
            std::env::set_var("FORGE_CHAT_PHASE1_TIMEOUT", v);
        } else {
            std::env::remove_var("FORGE_CHAT_PHASE1_TIMEOUT");
        }
    }

    #[test]
    fn test_apply_env_overrides_recovery() {
        let saved1 = std::env::var_os("FORGE_RECOVERY_MAX_RETRIES");
        let saved2 = std::env::var_os("FORGE_RECOVERY_AUTO_FAILOVER");
        std::env::set_var("FORGE_RECOVERY_MAX_RETRIES", "5");
        std::env::set_var("FORGE_RECOVERY_AUTO_FAILOVER", "true");

        let mut config = ForgeConfig::default();
        apply_env_overrides(&mut config);
        assert_eq!(config.recovery.max_retries, 5);
        assert!(config.recovery.auto_failover);

        if let Some(v) = saved1 {
            std::env::set_var("FORGE_RECOVERY_MAX_RETRIES", v);
        } else {
            std::env::remove_var("FORGE_RECOVERY_MAX_RETRIES");
        }
        if let Some(v) = saved2 {
            std::env::set_var("FORGE_RECOVERY_AUTO_FAILOVER", v);
        } else {
            std::env::remove_var("FORGE_RECOVERY_AUTO_FAILOVER");
        }
    }

    #[test]
    fn test_apply_env_overrides_invalid_port_ignored() {
        let saved = std::env::var_os("FORGE_BROWSER_PORT");
        std::env::set_var("FORGE_BROWSER_PORT", "not-a-number");

        let mut config = ForgeConfig::default();
        apply_env_overrides(&mut config);
        // 无效值应被忽略, 保持默认值
        assert_eq!(config.browser.port, 9222);

        if let Some(v) = saved {
            std::env::set_var("FORGE_BROWSER_PORT", v);
        } else {
            std::env::remove_var("FORGE_BROWSER_PORT");
        }
    }

    #[test]
    fn test_apply_env_overrides_no_vars() {
        // 确保环境变量未设置
        let saved = std::env::var_os("FORGE_BROWSER_PORT");
        std::env::remove_var("FORGE_BROWSER_PORT");

        let mut config = ForgeConfig::default();
        config.browser.port = 8080;
        apply_env_overrides(&mut config);
        // 没有环境变量, 应保持原值
        assert_eq!(config.browser.port, 8080);

        if let Some(v) = saved {
            std::env::set_var("FORGE_BROWSER_PORT", v);
        }
    }

    // ===== Serde 测试 =====

    #[test]
    fn test_config_serde_roundtrip() {
        let config = ForgeConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let parsed: ForgeConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.browser.port, config.browser.port);
        assert_eq!(parsed.chat.phase1_timeout, config.chat.phase1_timeout);
    }
}
