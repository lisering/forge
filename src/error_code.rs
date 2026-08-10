//! 结构化错误码系统 — 借鉴 us/crw `CrwError` 设计
//!
//! Forge 当前使用 `anyhow::Error` 处理错误, 缺少机器可读的错误码。
//! 本模块提供 `ForgeError` 枚举, 每个变体有 `error_code()` 方法返回
//! 机器可读的字符串代码, 便于日志分析、监控告警和自动恢复决策。
//!
//! ## 设计
//!
//! - [`ForgeError`][]: 结构化错误枚举, 每个变体对应一类错误
//! - `error_code()`: 返回 `&'static str` 机器可读代码
//! - `is_recoverable()`: 判断是否可自动恢复
//! - `severity()`: 返回严重级别 (1=info, 2=warning, 3=critical)
//!
//! ## 使用方式
//!
//! ```no_run
//! use forge::error_code::ForgeError;
//!
//! let err = ForgeError::CdpTimeout("Runtime.evaluate".to_string(), 30000);
//! assert_eq!(err.error_code(), "cdp_timeout");
//! assert!(err.is_recoverable());
//! assert_eq!(err.severity(), 2);
//! ```

use thiserror::Error;

/// Forge 结构化错误 — 借鉴 us/crw CrwError
#[derive(Debug, Error)]
pub enum ForgeError {
    // ===== CDP 相关 =====
    #[error("CDP 命令超时 ({1}ms): {0}")]
    CdpTimeout(String, u64),

    #[error("CDP 命令失败: {0} - {1}")]
    CdpCommandFailed(String, String),

    #[error("CDP WebSocket 连接失败: {0}")]
    CdpConnectionFailed(String),

    #[error("CDP WebSocket 已关闭")]
    CdpWebSocketClosed,

    #[error("CDP 响应通道关闭")]
    CdpChannelClosed,

    // ===== 浏览器相关 =====
    #[error("浏览器不可达: {0}")]
    BrowserUnreachable(String),

    #[error("浏览器进程已退出: {0}")]
    BrowserProcessExited(String),

    #[error("标签页已关闭: {0}")]
    TabClosed(String),

    #[error("未找到可用的聊天标签页")]
    NoChatTab,

    // ===== 聊天相关 =====
    #[error("AI 回复超时 ({0}s)")]
    ChatTimeout(u64),

    #[error("AI 回复为空")]
    ChatEmptyResponse,

    #[error("聊天网站不可用: {0}")]
    ChatSiteUnavailable(String),

    #[error("发送消息失败: {0}")]
    SendMessageFailed(String),

    // ===== 编译/测试相关 =====
    #[error("编译失败: {0}")]
    CompileFailed(String),

    #[error("测试失败: {0}")]
    TestFailed(String),

    #[error("运行时错误: {0}")]
    RuntimeError(String),

    // ===== 文件相关 =====
    #[error("文件不存在: {0}")]
    FileNotFound(String),

    #[error("文件写入失败: {0}")]
    FileWriteFailed(String),

    #[error("代码提取失败: {0}")]
    ExtractFailed(String),

    // ===== 代理相关 =====
    #[error("代理 URL 无效: {0}")]
    InvalidProxyUrl(String),

    #[error("代理连接失败: {0}")]
    ProxyConnectionFailed(String),

    // ===== 配置相关 =====
    #[error("配置错误: {0}")]
    ConfigError(String),

    #[error("环境变量无效: {0}={1}")]
    InvalidEnvVar(String, String),

    // ===== 网络相关 =====
    #[error("HTTP 请求失败: {0}")]
    HttpError(String),

    #[error("URL 解析错误: {0}")]
    UrlParseError(String),

    // ===== 恢复相关 =====
    #[error("恢复失败: {0}")]
    RecoveryFailed(String),

    #[error("恢复尝试次数耗尽 ({0}次)")]
    RecoveryExhausted(u32),

    // ===== 内部错误 =====
    #[error("内部错误: {0}")]
    Internal(String),

    #[error("未知错误: {0}")]
    Unknown(String),
}

impl ForgeError {
    /// 机器可读的错误代码 — 用于日志分析、监控告警
    ///
    /// 返回 `snake_case` 格式的错误代码字符串。
    pub fn error_code(&self) -> &'static str {
        match self {
            // CDP
            ForgeError::CdpTimeout(..) => "cdp_timeout",
            ForgeError::CdpCommandFailed(..) => "cdp_command_failed",
            ForgeError::CdpConnectionFailed(..) => "cdp_connection_failed",
            ForgeError::CdpWebSocketClosed => "cdp_websocket_closed",
            ForgeError::CdpChannelClosed => "cdp_channel_closed",
            // 浏览器
            ForgeError::BrowserUnreachable(..) => "browser_unreachable",
            ForgeError::BrowserProcessExited(..) => "browser_process_exited",
            ForgeError::TabClosed(..) => "tab_closed",
            ForgeError::NoChatTab => "no_chat_tab",
            // 聊天
            ForgeError::ChatTimeout(..) => "chat_timeout",
            ForgeError::ChatEmptyResponse => "chat_empty_response",
            ForgeError::ChatSiteUnavailable(..) => "chat_site_unavailable",
            ForgeError::SendMessageFailed(..) => "send_message_failed",
            // 编译/测试
            ForgeError::CompileFailed(..) => "compile_failed",
            ForgeError::TestFailed(..) => "test_failed",
            ForgeError::RuntimeError(..) => "runtime_error",
            // 文件
            ForgeError::FileNotFound(..) => "file_not_found",
            ForgeError::FileWriteFailed(..) => "file_write_failed",
            ForgeError::ExtractFailed(..) => "extract_failed",
            // 代理
            ForgeError::InvalidProxyUrl(..) => "invalid_proxy_url",
            ForgeError::ProxyConnectionFailed(..) => "proxy_connection_failed",
            // 配置
            ForgeError::ConfigError(..) => "config_error",
            ForgeError::InvalidEnvVar(..) => "invalid_env_var",
            // 网络
            ForgeError::HttpError(..) => "http_error",
            ForgeError::UrlParseError(..) => "url_parse_error",
            // 恢复
            ForgeError::RecoveryFailed(..) => "recovery_failed",
            ForgeError::RecoveryExhausted(..) => "recovery_exhausted",
            // 内部
            ForgeError::Internal(..) => "internal_error",
            ForgeError::Unknown(..) => "unknown_error",
        }
    }

    /// 是否可自动恢复 — 用于 AutoRecovery 决策
    ///
    /// 可恢复: CDP 超时、WebSocket 断开、浏览器不可达、标签页关闭等
    /// 不可恢复: 配置错误、文件不存在、代码提取失败等
    pub fn is_recoverable(&self) -> bool {
        match self {
            // 可恢复 — 通常是临时故障
            ForgeError::CdpTimeout(..) => true,
            ForgeError::CdpConnectionFailed(..) => true,
            ForgeError::CdpWebSocketClosed => true,
            ForgeError::CdpChannelClosed => true,
            ForgeError::BrowserUnreachable(..) => true,
            ForgeError::TabClosed(..) => true,
            ForgeError::ChatTimeout(..) => true,
            ForgeError::ChatSiteUnavailable(..) => true,
            ForgeError::SendMessageFailed(..) => true,
            ForgeError::ProxyConnectionFailed(..) => true,
            ForgeError::HttpError(..) => true,
            ForgeError::RecoveryFailed(..) => true,

            // 不可恢复 — 需要人工干预或代码修复
            ForgeError::CdpCommandFailed(..) => false,
            ForgeError::BrowserProcessExited(..) => false,
            ForgeError::NoChatTab => false,
            ForgeError::ChatEmptyResponse => false,
            ForgeError::CompileFailed(..) => false,
            ForgeError::TestFailed(..) => false,
            ForgeError::RuntimeError(..) => false,
            ForgeError::FileNotFound(..) => false,
            ForgeError::FileWriteFailed(..) => false,
            ForgeError::ExtractFailed(..) => false,
            ForgeError::InvalidProxyUrl(..) => false,
            ForgeError::ConfigError(..) => false,
            ForgeError::InvalidEnvVar(..) => false,
            ForgeError::UrlParseError(..) => false,
            ForgeError::RecoveryExhausted(..) => false,
            ForgeError::Internal(..) => false,
            ForgeError::Unknown(..) => false,
        }
    }

    /// 严重级别 (1=info, 2=warning, 3=critical)
    ///
    /// critical: 浏览器崩溃、WebSocket 断开等需要立即恢复
    /// warning: 超时、标签页关闭等可以重试
    /// info: 配置错误、文件不存在等不需要自动恢复
    pub fn severity(&self) -> u8 {
        match self {
            // Critical (3) — 需要立即恢复
            ForgeError::CdpConnectionFailed(..) => 3,
            ForgeError::CdpWebSocketClosed => 3,
            ForgeError::CdpChannelClosed => 3,
            ForgeError::BrowserUnreachable(..) => 3,
            ForgeError::BrowserProcessExited(..) => 3,
            ForgeError::RecoveryExhausted(..) => 3,

            // Warning (2) — 可以重试
            ForgeError::CdpTimeout(..) => 2,
            ForgeError::CdpCommandFailed(..) => 2,
            ForgeError::TabClosed(..) => 2,
            ForgeError::NoChatTab => 2,
            ForgeError::ChatTimeout(..) => 2,
            ForgeError::ChatSiteUnavailable(..) => 2,
            ForgeError::SendMessageFailed(..) => 2,
            ForgeError::CompileFailed(..) => 2,
            ForgeError::TestFailed(..) => 2,
            ForgeError::ProxyConnectionFailed(..) => 2,
            ForgeError::HttpError(..) => 2,
            ForgeError::RecoveryFailed(..) => 2,

            // Info (1) — 不需要自动恢复
            ForgeError::ChatEmptyResponse => 1,
            ForgeError::RuntimeError(..) => 1,
            ForgeError::FileNotFound(..) => 1,
            ForgeError::FileWriteFailed(..) => 1,
            ForgeError::ExtractFailed(..) => 1,
            ForgeError::InvalidProxyUrl(..) => 1,
            ForgeError::ConfigError(..) => 1,
            ForgeError::InvalidEnvVar(..) => 1,
            ForgeError::UrlParseError(..) => 1,
            ForgeError::Internal(..) => 1,
            ForgeError::Unknown(..) => 1,
        }
    }

    /// 错误类别 — 用于统计和分组
    pub fn category(&self) -> ErrorCategory {
        match self {
            ForgeError::CdpTimeout(..)
            | ForgeError::CdpCommandFailed(..)
            | ForgeError::CdpConnectionFailed(..)
            | ForgeError::CdpWebSocketClosed
            | ForgeError::CdpChannelClosed => ErrorCategory::Cdp,

            ForgeError::BrowserUnreachable(..)
            | ForgeError::BrowserProcessExited(..)
            | ForgeError::TabClosed(..)
            | ForgeError::NoChatTab => ErrorCategory::Browser,

            ForgeError::ChatTimeout(..)
            | ForgeError::ChatEmptyResponse(..)
            | ForgeError::ChatSiteUnavailable(..)
            | ForgeError::SendMessageFailed(..) => ErrorCategory::Chat,

            ForgeError::CompileFailed(..)
            | ForgeError::TestFailed(..)
            | ForgeError::RuntimeError(..) => ErrorCategory::Build,

            ForgeError::FileNotFound(..)
            | ForgeError::FileWriteFailed(..)
            | ForgeError::ExtractFailed(..) => ErrorCategory::File,

            ForgeError::InvalidProxyUrl(..)
            | ForgeError::ProxyConnectionFailed(..) => ErrorCategory::Proxy,

            ForgeError::ConfigError(..)
            | ForgeError::InvalidEnvVar(..) => ErrorCategory::Config,

            ForgeError::HttpError(..)
            | ForgeError::UrlParseError(..) => ErrorCategory::Network,

            ForgeError::RecoveryFailed(..)
            | ForgeError::RecoveryExhausted(..) => ErrorCategory::Recovery,

            ForgeError::Internal(..)
            | ForgeError::Unknown(..) => ErrorCategory::Internal,
        }
    }
}

/// 错误类别 — 用于统计和分组
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCategory {
    /// CDP 协议相关
    Cdp,
    /// 浏览器进程相关
    Browser,
    /// AI 聊天相关
    Chat,
    /// 编译/测试相关
    Build,
    /// 文件操作相关
    File,
    /// 代理相关
    Proxy,
    /// 配置相关
    Config,
    /// 网络相关
    Network,
    /// 恢复相关
    Recovery,
    /// 内部错误
    Internal,
}

impl ErrorCategory {
    /// 类别名称 (用于日志)
    pub fn name(&self) -> &'static str {
        match self {
            ErrorCategory::Cdp => "cdp",
            ErrorCategory::Browser => "browser",
            ErrorCategory::Chat => "chat",
            ErrorCategory::Build => "build",
            ErrorCategory::File => "file",
            ErrorCategory::Proxy => "proxy",
            ErrorCategory::Config => "config",
            ErrorCategory::Network => "network",
            ErrorCategory::Recovery => "recovery",
            ErrorCategory::Internal => "internal",
        }
    }
}

/// 将 `anyhow::Error` 转换为 `ForgeError` (尽力匹配)
///
/// 通过错误消息内容匹配最接近的 `ForgeError` 变体。
/// 无法匹配的返回 `ForgeError::Unknown`。
pub fn classify_anyhow(err: &anyhow::Error) -> ForgeError {
    let msg = err.to_string();

    if msg.contains("超时") || msg.contains("timeout") {
        if msg.contains("CDP") {
            ForgeError::CdpTimeout(msg, 0)
        } else if msg.contains("聊天") || msg.contains("chat") || msg.contains("回复") {
            ForgeError::ChatTimeout(0)
        } else {
            ForgeError::Unknown(msg)
        }
    } else if msg.contains("WebSocket") || msg.contains("ws") {
        if msg.contains("关闭") || msg.contains("close") {
            ForgeError::CdpWebSocketClosed
        } else {
            ForgeError::CdpConnectionFailed(msg)
        }
    } else if msg.contains("浏览器") || msg.contains("Chrome") || msg.contains("chrome") {
        if msg.contains("不可达") || msg.contains("unreachable") {
            ForgeError::BrowserUnreachable(msg)
        } else if msg.contains("退出") || msg.contains("exit") {
            ForgeError::BrowserProcessExited(msg)
        } else {
            ForgeError::BrowserUnreachable(msg)
        }
    } else if msg.contains("标签页") || msg.contains("tab") {
        ForgeError::TabClosed(msg)
    } else if msg.contains("编译") || msg.contains("compile") {
        ForgeError::CompileFailed(msg)
    } else if msg.contains("测试") || msg.contains("test") {
        ForgeError::TestFailed(msg)
    } else if msg.contains("代理") || msg.contains("proxy") {
        if msg.contains("无效") || msg.contains("invalid") {
            ForgeError::InvalidProxyUrl(msg)
        } else {
            ForgeError::ProxyConnectionFailed(msg)
        }
    } else if msg.contains("文件") || msg.contains("file") {
        if msg.contains("不存在") || msg.contains("not found") {
            ForgeError::FileNotFound(msg)
        } else {
            ForgeError::FileWriteFailed(msg)
        }
    } else if msg.contains("配置") || msg.contains("config") {
        ForgeError::ConfigError(msg)
    } else {
        ForgeError::Unknown(msg)
    }
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ===== error_code 测试 =====

    #[test]
    fn test_error_code_cdp_timeout() {
        let err = ForgeError::CdpTimeout("Runtime.evaluate".to_string(), 30000);
        assert_eq!(err.error_code(), "cdp_timeout");
    }

    #[test]
    fn test_error_code_browser_unreachable() {
        let err = ForgeError::BrowserUnreachable("port 9222".to_string());
        assert_eq!(err.error_code(), "browser_unreachable");
    }

    #[test]
    fn test_error_code_chat_timeout() {
        let err = ForgeError::ChatTimeout(120);
        assert_eq!(err.error_code(), "chat_timeout");
    }

    #[test]
    fn test_error_code_compile_failed() {
        let err = ForgeError::CompileFailed("syntax error".to_string());
        assert_eq!(err.error_code(), "compile_failed");
    }

    #[test]
    fn test_error_code_recovery_exhausted() {
        let err = ForgeError::RecoveryExhausted(5);
        assert_eq!(err.error_code(), "recovery_exhausted");
    }

    #[test]
    fn test_error_code_all_variants_unique() {
        // 确保所有 error_code 都是唯一的
        let codes = vec![
            ForgeError::CdpTimeout("".to_string(), 0).error_code(),
            ForgeError::CdpCommandFailed("".to_string(), "".to_string()).error_code(),
            ForgeError::CdpConnectionFailed("".to_string()).error_code(),
            ForgeError::CdpWebSocketClosed.error_code(),
            ForgeError::CdpChannelClosed.error_code(),
            ForgeError::BrowserUnreachable("".to_string()).error_code(),
            ForgeError::BrowserProcessExited("".to_string()).error_code(),
            ForgeError::TabClosed("".to_string()).error_code(),
            ForgeError::NoChatTab.error_code(),
            ForgeError::ChatTimeout(0).error_code(),
            ForgeError::ChatEmptyResponse.error_code(),
            ForgeError::ChatSiteUnavailable("".to_string()).error_code(),
            ForgeError::SendMessageFailed("".to_string()).error_code(),
            ForgeError::CompileFailed("".to_string()).error_code(),
            ForgeError::TestFailed("".to_string()).error_code(),
            ForgeError::RuntimeError("".to_string()).error_code(),
            ForgeError::FileNotFound("".to_string()).error_code(),
            ForgeError::FileWriteFailed("".to_string()).error_code(),
            ForgeError::ExtractFailed("".to_string()).error_code(),
            ForgeError::InvalidProxyUrl("".to_string()).error_code(),
            ForgeError::ProxyConnectionFailed("".to_string()).error_code(),
            ForgeError::ConfigError("".to_string()).error_code(),
            ForgeError::InvalidEnvVar("".to_string(), "".to_string()).error_code(),
            ForgeError::HttpError("".to_string()).error_code(),
            ForgeError::UrlParseError("".to_string()).error_code(),
            ForgeError::RecoveryFailed("".to_string()).error_code(),
            ForgeError::RecoveryExhausted(0).error_code(),
            ForgeError::Internal("".to_string()).error_code(),
            ForgeError::Unknown("".to_string()).error_code(),
        ];
        let unique: std::collections::HashSet<_> = codes.iter().collect();
        assert_eq!(codes.len(), unique.len(), "error_code 有重复值");
    }

    // ===== is_recoverable 测试 =====

    #[test]
    fn test_is_recoverable_cdp_timeout() {
        assert!(ForgeError::CdpTimeout("test".to_string(), 0).is_recoverable());
    }

    #[test]
    fn test_is_recoverable_browser_unreachable() {
        assert!(ForgeError::BrowserUnreachable("test".to_string()).is_recoverable());
    }

    #[test]
    fn test_is_recoverable_config_error() {
        assert!(!ForgeError::ConfigError("test".to_string()).is_recoverable());
    }

    #[test]
    fn test_is_recoverable_file_not_found() {
        assert!(!ForgeError::FileNotFound("test".to_string()).is_recoverable());
    }

    #[test]
    fn test_is_recoverable_recovery_exhausted() {
        assert!(!ForgeError::RecoveryExhausted(5).is_recoverable());
    }

    // ===== severity 测试 =====

    #[test]
    fn test_severity_critical() {
        assert_eq!(ForgeError::CdpWebSocketClosed.severity(), 3);
        assert_eq!(ForgeError::BrowserUnreachable("".to_string()).severity(), 3);
        assert_eq!(ForgeError::RecoveryExhausted(0).severity(), 3);
    }

    #[test]
    fn test_severity_warning() {
        assert_eq!(ForgeError::CdpTimeout("".to_string(), 0).severity(), 2);
        assert_eq!(ForgeError::ChatTimeout(0).severity(), 2);
        assert_eq!(ForgeError::CompileFailed("".to_string()).severity(), 2);
    }

    #[test]
    fn test_severity_info() {
        assert_eq!(ForgeError::ChatEmptyResponse.severity(), 1);
        assert_eq!(ForgeError::ConfigError("".to_string()).severity(), 1);
        assert_eq!(ForgeError::FileNotFound("".to_string()).severity(), 1);
    }

    // ===== category 测试 =====

    #[test]
    fn test_category_cdp() {
        assert_eq!(ForgeError::CdpTimeout("".to_string(), 0).category(), ErrorCategory::Cdp);
        assert_eq!(ForgeError::CdpWebSocketClosed.category(), ErrorCategory::Cdp);
    }

    #[test]
    fn test_category_browser() {
        assert_eq!(ForgeError::BrowserUnreachable("".to_string()).category(), ErrorCategory::Browser);
        assert_eq!(ForgeError::TabClosed("".to_string()).category(), ErrorCategory::Browser);
    }

    #[test]
    fn test_category_chat() {
        assert_eq!(ForgeError::ChatTimeout(0).category(), ErrorCategory::Chat);
        assert_eq!(ForgeError::ChatEmptyResponse.category(), ErrorCategory::Chat);
    }

    #[test]
    fn test_category_build() {
        assert_eq!(ForgeError::CompileFailed("".to_string()).category(), ErrorCategory::Build);
        assert_eq!(ForgeError::TestFailed("".to_string()).category(), ErrorCategory::Build);
    }

    #[test]
    fn test_category_name() {
        assert_eq!(ErrorCategory::Cdp.name(), "cdp");
        assert_eq!(ErrorCategory::Browser.name(), "browser");
        assert_eq!(ErrorCategory::Chat.name(), "chat");
        assert_eq!(ErrorCategory::Internal.name(), "internal");
    }

    // ===== classify_anyhow 测试 =====

    #[test]
    fn test_classify_anyhow_cdp_timeout() {
        let err = anyhow::anyhow!("CDP 命令超时 (30s): Runtime.evaluate");
        let classified = classify_anyhow(&err);
        assert!(matches!(classified, ForgeError::CdpTimeout(..)));
    }

    #[test]
    fn test_classify_anyhow_websocket_closed() {
        let err = anyhow::anyhow!("CDP WebSocket 已关闭");
        let classified = classify_anyhow(&err);
        assert!(matches!(classified, ForgeError::CdpWebSocketClosed));
    }

    #[test]
    fn test_classify_anyhow_browser_unreachable() {
        let err = anyhow::anyhow!("浏览器不可达: port 9222");
        let classified = classify_anyhow(&err);
        assert!(matches!(classified, ForgeError::BrowserUnreachable(..)));
    }

    #[test]
    fn test_classify_anyhow_compile_failed() {
        let err = anyhow::anyhow!("编译失败: syntax error");
        let classified = classify_anyhow(&err);
        assert!(matches!(classified, ForgeError::CompileFailed(..)));
    }

    #[test]
    fn test_classify_anyhow_unknown() {
        let err = anyhow::anyhow!("some weird error");
        let classified = classify_anyhow(&err);
        assert!(matches!(classified, ForgeError::Unknown(..)));
    }

    #[test]
    fn test_classify_anyhow_proxy_invalid() {
        let err = anyhow::anyhow!("代理 URL 无效: ftp://bad");
        let classified = classify_anyhow(&err);
        assert!(matches!(classified, ForgeError::InvalidProxyUrl(..)));
    }

    #[test]
    fn test_classify_anyhow_file_not_found() {
        let err = anyhow::anyhow!("文件不存在: /tmp/test.rs");
        let classified = classify_anyhow(&err);
        assert!(matches!(classified, ForgeError::FileNotFound(..)));
    }

    // ===== Display 测试 =====

    #[test]
    fn test_display_cdp_timeout() {
        let err = ForgeError::CdpTimeout("Runtime.evaluate".to_string(), 30000);
        let msg = format!("{}", err);
        assert!(msg.contains("Runtime.evaluate"));
        assert!(msg.contains("30000"));
    }

    #[test]
    fn test_display_browser_unreachable() {
        let err = ForgeError::BrowserUnreachable("port 9222".to_string());
        let msg = format!("{}", err);
        assert!(msg.contains("port 9222"));
    }
}
