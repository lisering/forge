//! 代理 IP 池 + 自动刷新 Mixin — 借鉴 MediaCrawler ProxyRefreshMixin 设计
//!
//! 为 Forge 的 HTTP 请求 (如 Ollama LLM 调用、GitHub API) 提供代理支持。
//! 虽然 Forge 通过浏览器控制 AI 网站 (代理需要在浏览器层面设置),
//! 但 Forge 自身的 HTTP 请求仍可受益于代理 IP 池。
//!
//! ## 设计
//!
//! - [`ProxyPool`][]: 代理 IP 池, 支持自动刷新
//! - [`ProxyRefresh`] trait: Mixin 模式, 任何需要 HTTP 请求的客户端可混入
//! - [`ProxyConfig`][]: 代理配置
//!
//! ## 示例
//!
//! ```
//! use forge::proxy_pool::{ProxyConfig, ProxyPool, ProxyRefresh};
//!
//! let config = ProxyConfig::default();
//! let mut pool = ProxyPool::new(config);
//! pool.refresh_if_expired();
//! ```

use anyhow::Result;
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// 允许的代理 URL scheme
#[allow(dead_code)]
const ALLOWED_PROXY_SCHEMES: &[&str] = &["http", "https", "socks5", "socks4"];

// ============================================================================
//  ProxyConfig — 代理配置
// ============================================================================

/// 代理配置
#[derive(Debug, Clone)]
pub struct ProxyConfig {
    /// 代理列表 (格式: "http://ip:port" 或 "socks5://ip:port")
    pub proxies: Vec<String>,
    /// 代理过期时间 (秒)
    pub ttl_secs: u64,
    /// 最大重试次数
    pub max_retries: u32,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            proxies: vec![],
            ttl_secs: 300, // 5 分钟
            max_retries: 3,
        }
    }
}

// ============================================================================
//  ProxyPool — 代理 IP 池
// ============================================================================

/// 代理 IP 池 — 轮询使用代理, 自动刷新过期代理
///
/// 借鉴 MediaCrawler ProxyRefreshMixin 的设计:
/// - 每次请求前检查代理是否过期
/// - 过期则自动获取新代理 (从列表中轮询)
/// - 支持代理失效后自动切换
pub struct ProxyPool {
    config: ProxyConfig,
    current_index: Mutex<usize>,
    current_proxy: Mutex<Option<String>>,
    last_refresh: Mutex<Option<Instant>>,
}

impl ProxyPool {
    /// 创建新的代理池
    pub fn new(config: ProxyConfig) -> Self {
        let initial_proxy = config.proxies.first().cloned();
        let has_proxy = initial_proxy.is_some();
        Self {
            config,
            current_index: Mutex::new(0),
            current_proxy: Mutex::new(initial_proxy),
            last_refresh: Mutex::new(if has_proxy {
                Some(Instant::now())
            } else {
                None
            }),
        }
    }

    /// 获取当前代理
    pub fn current(&self) -> Option<String> {
        self.current_proxy.lock().unwrap().clone()
    }

    /// 检查代理是否过期
    pub fn is_expired(&self) -> bool {
        let last = self.last_refresh.lock().unwrap();
        match *last {
            Some(time) => time.elapsed() > Duration::from_secs(self.config.ttl_secs),
            None => true, // 从未刷新过
        }
    }

    /// 刷新代理 (轮询到下一个)
    pub fn refresh(&self) -> Result<()> {
        if self.config.proxies.is_empty() {
            warn!("代理池为空, 无法刷新");
            return Ok(());
        }

        let mut idx = self.current_index.lock().unwrap();
        *idx = (*idx + 1) % self.config.proxies.len();
        let new_proxy = self.config.proxies[*idx].clone();

        let mut current = self.current_proxy.lock().unwrap();
        *current = Some(new_proxy.clone());
        let mut last = self.last_refresh.lock().unwrap();
        *last = Some(Instant::now());

        info!(
            "代理已刷新: {} (索引 {}/{})",
            new_proxy,
            *idx,
            self.config.proxies.len()
        );
        Ok(())
    }

    /// 如果过期则刷新代理 (Mixin 模式的核心方法)
    pub fn refresh_if_expired(&self) -> Result<()> {
        if self.is_expired() {
            debug!("代理已过期, 正在刷新...");
            self.refresh()?;
        }
        Ok(())
    }

    /// 标记当前代理失效, 切换到下一个
    pub fn mark_failed(&self) {
        debug!("当前代理标记为失效, 切换中...");
        let _ = self.refresh();
    }

    /// 代理池大小
    pub fn len(&self) -> usize {
        self.config.proxies.len()
    }

    /// 代理池是否为空
    pub fn is_empty(&self) -> bool {
        self.config.proxies.is_empty()
    }
}

// ============================================================================
//  ProxyRefresh — Mixin trait (借鉴 MediaCrawler ProxyRefreshMixin)
// ============================================================================

/// 代理刷新 Mixin — 借鉴 MediaCrawler ProxyRefreshMixin
///
/// 任何需要 HTTP 请求的客户端都可以实现此 trait,
/// 在每次请求前自动检查并刷新过期的代理。
#[async_trait]
pub trait ProxyRefresh: Send + Sync {
    /// 检查代理是否过期, 过期则刷新
    async fn refresh_proxy_if_expired(&self) -> Result<()>;

    /// 获取当前代理 URL
    async fn current_proxy(&self) -> Option<String>;
}

/// 为 Arc<ProxyPool> 实现 ProxyRefresh (最常见的用法)
#[async_trait]
impl ProxyRefresh for Arc<ProxyPool> {
    async fn refresh_proxy_if_expired(&self) -> Result<()> {
        self.refresh_if_expired()
    }

    async fn current_proxy(&self) -> Option<String> {
        self.current()
    }
}

// ============================================================================
//  纯函数 — 代理 URL 解析和验证
// ============================================================================

/// 验证代理 URL 格式是否正确
///
/// 支持的格式: `http://ip:port`, `https://ip:port`, `socks5://ip:port`
///
/// # 示例
///
/// ```
/// use forge::proxy_pool::is_valid_proxy_url;
///
/// assert!(is_valid_proxy_url("http://127.0.0.1:8080"));
/// assert!(is_valid_proxy_url("socks5://10.0.0.1:1080"));
/// assert!(!is_valid_proxy_url("invalid"));
/// assert!(!is_valid_proxy_url(""));
/// ```
pub fn is_valid_proxy_url(url: &str) -> bool {
    if url.is_empty() {
        return false;
    }
    let lower = url.to_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("socks5://")
        || lower.starts_with("socks4://")
}

/// 已验证的代理条目 — 借鉴 us/crw ProxyEntry 设计
///
/// 包含原始 URL (用于 reqwest) 和解析后的各部分。
/// 安全特性: scheme 白名单验证, 禁止 silent fallback 到直连。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyEntry {
    /// 原始代理 URL (含凭据)
    pub raw: String,
    /// scheme (http/https/socks5/socks4)
    pub scheme: String,
    /// host:port
    pub host_port: String,
    /// 是否包含认证信息
    pub has_auth: bool,
}

impl ProxyEntry {
    /// 解析和验证代理 URL
    ///
    /// # 安全
    ///
    /// 代理 URL 会被预先验证。格式错误的条目是硬错误 ——
    /// 永远不会静默回退到直连 (无代理), 否则会泄露主机的真实 IP。
    ///
    /// # 错误
    ///
    /// - scheme 不支持
    /// - host 缺失
    /// - URL 格式无效
    ///
    /// # 示例
    ///
    /// ```
    /// use forge::proxy_pool::ProxyEntry;
    ///
    /// let entry = ProxyEntry::parse("http://127.0.0.1:8080").unwrap();
    /// assert_eq!(entry.scheme, "http");
    /// assert_eq!(entry.host_port, "127.0.0.1:8080");
    /// ```
    pub fn parse(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err("empty proxy URL".to_string());
        }

        let lower = trimmed.to_lowercase();
        let scheme = if lower.starts_with("https://") {
            "https"
        } else if lower.starts_with("http://") {
            "http"
        } else if lower.starts_with("socks5://") {
            "socks5"
        } else if lower.starts_with("socks4://") {
            "socks4"
        } else {
            return Err(format!(
                "unsupported proxy scheme in '{}' (allowed: http, https, socks5, socks4)",
                trimmed
            ));
        };

        // 去掉 scheme:// 前缀
        let rest = &trimmed[scheme.len() + 3..];

        // 分离认证信息和 host:port
        let (auth_part, host_part) = match rest.find('@') {
            Some(pos) => (Some(&rest[..pos]), &rest[pos + 1..]),
            None => (None, rest),
        };

        // 去掉路径部分 (如果有)
        let host_port = match host_part.find('/') {
            Some(pos) => &host_part[..pos],
            None => host_part,
        };

        if host_port.is_empty() {
            return Err(format!("proxy URL '{}' has no host", trimmed));
        }

        Ok(Self {
            raw: trimmed.to_string(),
            scheme: scheme.to_string(),
            host_port: host_port.to_string(),
            has_auth: auth_part.is_some(),
        })
    }

    /// 构建 Chrome `--proxy-server` 参数值
    ///
    /// Chrome 的 `--proxy-server` 参数格式: `scheme://host:port`
    /// (不含凭据, 凭据通过 CDP `Fetch.authRequired` 传递)
    pub fn chrome_proxy_arg(&self) -> String {
        format!("--proxy-server={}://{}", self.scheme, self.host_port)
    }
}

/// 验证代理列表, 返回有效条目和无效条目
///
/// 无效条目不会静默丢弃 —— 返回错误信息供调用方决策。
/// 借鉴 us/crw 的设计: 格式错误的代理是硬错误, 不 fallback 到直连。
///
/// # 示例
///
/// ```
/// use forge::proxy_pool::validate_proxy_list;
///
/// let (valid, invalid) = validate_proxy_list(&["http://valid:8080", "invalid"]);
/// assert_eq!(valid.len(), 1);
/// assert_eq!(invalid.len(), 1);
/// ```
pub fn validate_proxy_list(urls: &[&str]) -> (Vec<ProxyEntry>, Vec<(String, String)>) {
    let mut valid = Vec::new();
    let mut invalid = Vec::new();
    for url in urls {
        match ProxyEntry::parse(url) {
            Ok(entry) => valid.push(entry),
            Err(e) => invalid.push((url.to_string(), e)),
        }
    }
    (valid, invalid)
}

/// 从环境变量加载代理列表
///
/// 支持的环境变量:
/// - `FORGE_PROXY`: 单个代理 URL
/// - `FORGE_PROXY_LIST`: 逗号分隔的代理 URL 列表
pub fn load_proxies_from_env() -> Vec<String> {
    let mut proxies = Vec::new();

    // 单个代理
    if let Some(proxy) = std::env::var_os("FORGE_PROXY") {
        let proxy = proxy.to_string_lossy().to_string();
        if is_valid_proxy_url(&proxy) {
            proxies.push(proxy);
        }
    }

    // 代理列表
    if let Some(list) = std::env::var_os("FORGE_PROXY_LIST") {
        for proxy in list.to_string_lossy().split(',') {
            let proxy = proxy.trim();
            if is_valid_proxy_url(proxy) {
                proxies.push(proxy.to_string());
            }
        }
    }

    proxies
}

/// 构建 reqwest 代理配置
///
/// 将代理 URL 字符串转换为 `reqwest::Proxy` (如果可用)
pub fn build_reqwest_proxy(url: &str) -> Result<reqwest::Proxy> {
    if !is_valid_proxy_url(url) {
        anyhow::bail!("无效的代理 URL: {}", url);
    }

    if url.starts_with("socks") {
        reqwest::Proxy::all(url).map_err(|e| anyhow::anyhow!("创建 SOCKS 代理失败: {}", e))
    } else {
        reqwest::Proxy::all(url).map_err(|e| anyhow::anyhow!("创建 HTTP 代理失败: {}", e))
    }
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ===== is_valid_proxy_url 测试 =====

    #[test]
    fn test_is_valid_proxy_url_http() {
        assert!(is_valid_proxy_url("http://127.0.0.1:8080"));
        assert!(is_valid_proxy_url("http://10.0.0.1:3128"));
    }

    #[test]
    fn test_is_valid_proxy_url_https() {
        assert!(is_valid_proxy_url("https://proxy.example.com:443"));
    }

    #[test]
    fn test_is_valid_proxy_url_socks5() {
        assert!(is_valid_proxy_url("socks5://127.0.0.1:1080"));
    }

    #[test]
    fn test_is_valid_proxy_url_socks4() {
        assert!(is_valid_proxy_url("socks4://127.0.0.1:1080"));
    }

    #[test]
    fn test_is_valid_proxy_url_invalid() {
        assert!(!is_valid_proxy_url("invalid"));
        assert!(!is_valid_proxy_url(""));
        assert!(!is_valid_proxy_url("ftp://example.com"));
        assert!(!is_valid_proxy_url("127.0.0.1:8080")); // 缺少 scheme
    }

    #[test]
    fn test_is_valid_proxy_url_case_insensitive() {
        assert!(is_valid_proxy_url("HTTP://127.0.0.1:8080"));
        assert!(is_valid_proxy_url("SOCKS5://127.0.0.1:1080"));
    }

    // ===== ProxyConfig 测试 =====

    #[test]
    fn test_proxy_config_default() {
        let config = ProxyConfig::default();
        assert!(config.proxies.is_empty());
        assert_eq!(config.ttl_secs, 300);
        assert_eq!(config.max_retries, 3);
    }

    // ===== ProxyPool 测试 =====

    #[test]
    fn test_proxy_pool_empty() {
        let pool = ProxyPool::new(ProxyConfig::default());
        assert!(pool.is_empty());
        assert_eq!(pool.len(), 0);
        assert!(pool.current().is_none());
    }

    #[test]
    fn test_proxy_pool_with_proxies() {
        let config = ProxyConfig {
            proxies: vec![
                "http://proxy1:8080".to_string(),
                "http://proxy2:8080".to_string(),
            ],
            ttl_secs: 300,
            max_retries: 3,
        };
        let pool = ProxyPool::new(config);
        assert_eq!(pool.len(), 2);
        assert!(!pool.is_empty());
        assert_eq!(pool.current(), Some("http://proxy1:8080".to_string()));
    }

    #[test]
    fn test_proxy_pool_refresh() {
        let config = ProxyConfig {
            proxies: vec![
                "http://proxy1:8080".to_string(),
                "http://proxy2:8080".to_string(),
                "http://proxy3:8080".to_string(),
            ],
            ttl_secs: 300,
            max_retries: 3,
        };
        let pool = ProxyPool::new(config);

        assert_eq!(pool.current(), Some("http://proxy1:8080".to_string()));

        pool.refresh().unwrap();
        assert_eq!(pool.current(), Some("http://proxy2:8080".to_string()));

        pool.refresh().unwrap();
        assert_eq!(pool.current(), Some("http://proxy3:8080".to_string()));

        // 循环回到第一个
        pool.refresh().unwrap();
        assert_eq!(pool.current(), Some("http://proxy1:8080".to_string()));
    }

    #[test]
    fn test_proxy_pool_is_expired_initially() {
        let config = ProxyConfig {
            proxies: vec!["http://proxy:8080".to_string()],
            ttl_secs: 300,
            max_retries: 3,
        };
        let pool = ProxyPool::new(config);
        assert!(!pool.is_expired()); // 刚创建, 未过期
    }

    #[test]
    fn test_proxy_pool_is_expired_when_empty() {
        let pool = ProxyPool::new(ProxyConfig::default());
        assert!(pool.is_expired()); // 空池, 视为过期
    }

    #[test]
    fn test_proxy_pool_is_expired_after_ttl() {
        let config = ProxyConfig {
            proxies: vec!["http://proxy:8080".to_string()],
            ttl_secs: 0, // 立即过期
            max_retries: 3,
        };
        let pool = ProxyPool::new(config);
        std::thread::sleep(Duration::from_millis(10));
        assert!(pool.is_expired());
    }

    #[test]
    fn test_proxy_pool_refresh_if_expired() {
        let config = ProxyConfig {
            proxies: vec![
                "http://proxy1:8080".to_string(),
                "http://proxy2:8080".to_string(),
            ],
            ttl_secs: 0, // 立即过期
            max_retries: 3,
        };
        let pool = ProxyPool::new(config);

        // 过期后自动刷新
        pool.refresh_if_expired().unwrap();
        assert_eq!(pool.current(), Some("http://proxy2:8080".to_string()));
    }

    #[test]
    fn test_proxy_pool_mark_failed() {
        let config = ProxyConfig {
            proxies: vec![
                "http://proxy1:8080".to_string(),
                "http://proxy2:8080".to_string(),
            ],
            ttl_secs: 300,
            max_retries: 3,
        };
        let pool = ProxyPool::new(config);
        assert_eq!(pool.current(), Some("http://proxy1:8080".to_string()));

        pool.mark_failed();
        assert_eq!(pool.current(), Some("http://proxy2:8080".to_string()));
    }

    #[test]
    fn test_proxy_pool_refresh_empty_no_panic() {
        let pool = ProxyPool::new(ProxyConfig::default());
        let result = pool.refresh();
        assert!(result.is_ok()); // 空池不 panic, 返回 Ok
    }

    // ===== load_proxies_from_env 测试 =====

    #[test]
    fn test_load_proxies_from_env_not_set() {
        let saved1 = std::env::var_os("FORGE_PROXY");
        let saved2 = std::env::var_os("FORGE_PROXY_LIST");
        std::env::remove_var("FORGE_PROXY");
        std::env::remove_var("FORGE_PROXY_LIST");

        assert!(load_proxies_from_env().is_empty());

        if let Some(v) = saved1 {
            std::env::set_var("FORGE_PROXY", v);
        }
        if let Some(v) = saved2 {
            std::env::set_var("FORGE_PROXY_LIST", v);
        }
    }

    #[test]
    fn test_load_proxies_from_env_single() {
        let saved = std::env::var_os("FORGE_PROXY");
        std::env::set_var("FORGE_PROXY", "http://proxy:8080");

        let proxies = load_proxies_from_env();
        assert!(proxies.contains(&"http://proxy:8080".to_string()));

        if let Some(v) = saved {
            std::env::set_var("FORGE_PROXY", v);
        } else {
            std::env::remove_var("FORGE_PROXY");
        }
    }

    #[test]
    fn test_load_proxies_from_env_list() {
        let saved = std::env::var_os("FORGE_PROXY_LIST");
        std::env::set_var(
            "FORGE_PROXY_LIST",
            "http://p1:8080,http://p2:8080,http://p3:8080",
        );

        let proxies = load_proxies_from_env();
        assert!(proxies.contains(&"http://p1:8080".to_string()));
        assert!(proxies.contains(&"http://p2:8080".to_string()));
        assert!(proxies.contains(&"http://p3:8080".to_string()));

        if let Some(v) = saved {
            std::env::set_var("FORGE_PROXY_LIST", v);
        } else {
            std::env::remove_var("FORGE_PROXY_LIST");
        }
    }

    #[test]
    fn test_load_proxies_from_env_invalid_ignored() {
        let saved = std::env::var_os("FORGE_PROXY_LIST");
        std::env::set_var("FORGE_PROXY_LIST", "invalid,http://valid:8080,also-invalid");

        let proxies = load_proxies_from_env();
        assert_eq!(proxies.len(), 1);
        assert_eq!(proxies[0], "http://valid:8080");

        if let Some(v) = saved {
            std::env::set_var("FORGE_PROXY_LIST", v);
        } else {
            std::env::remove_var("FORGE_PROXY_LIST");
        }
    }

    // ===== build_reqwest_proxy 测试 =====

    #[test]
    fn test_build_reqwest_proxy_http() {
        let result = build_reqwest_proxy("http://127.0.0.1:8080");
        assert!(result.is_ok());
    }

    #[test]
    fn test_build_reqwest_proxy_socks5() {
        let result = build_reqwest_proxy("socks5://127.0.0.1:1080");
        assert!(result.is_ok());
    }

    #[test]
    fn test_build_reqwest_proxy_invalid() {
        let result = build_reqwest_proxy("invalid");
        assert!(result.is_err());
    }

    #[test]
    fn test_build_reqwest_proxy_empty() {
        let result = build_reqwest_proxy("");
        assert!(result.is_err());
    }

    // ===== ProxyEntry 测试 =====

    #[test]
    fn test_proxy_entry_parse_http() {
        let entry = ProxyEntry::parse("http://127.0.0.1:8080").unwrap();
        assert_eq!(entry.scheme, "http");
        assert_eq!(entry.host_port, "127.0.0.1:8080");
        assert!(!entry.has_auth);
    }

    #[test]
    fn test_proxy_entry_parse_socks5() {
        let entry = ProxyEntry::parse("socks5://10.0.0.1:1080").unwrap();
        assert_eq!(entry.scheme, "socks5");
        assert_eq!(entry.host_port, "10.0.0.1:1080");
    }

    #[test]
    fn test_proxy_entry_parse_https() {
        let entry = ProxyEntry::parse("https://proxy.example.com:443").unwrap();
        assert_eq!(entry.scheme, "https");
        assert_eq!(entry.host_port, "proxy.example.com:443");
    }

    #[test]
    fn test_proxy_entry_parse_with_auth() {
        let entry = ProxyEntry::parse("http://user:pass@proxy:8080").unwrap();
        assert_eq!(entry.scheme, "http");
        assert_eq!(entry.host_port, "proxy:8080");
        assert!(entry.has_auth);
    }

    #[test]
    fn test_proxy_entry_parse_invalid_scheme() {
        assert!(ProxyEntry::parse("ftp://proxy:8080").is_err());
        assert!(ProxyEntry::parse("invalid").is_err());
    }

    #[test]
    fn test_proxy_entry_parse_empty() {
        assert!(ProxyEntry::parse("").is_err());
        assert!(ProxyEntry::parse("   ").is_err());
    }

    #[test]
    fn test_proxy_entry_parse_no_host() {
        assert!(ProxyEntry::parse("http://").is_err());
        assert!(ProxyEntry::parse("http:///path").is_err());
    }

    #[test]
    fn test_proxy_entry_parse_with_path() {
        let entry = ProxyEntry::parse("http://proxy:8080/some/path").unwrap();
        assert_eq!(entry.host_port, "proxy:8080");
    }

    #[test]
    fn test_proxy_entry_chrome_proxy_arg() {
        let entry = ProxyEntry::parse("http://127.0.0.1:8080").unwrap();
        assert_eq!(
            entry.chrome_proxy_arg(),
            "--proxy-server=http://127.0.0.1:8080"
        );
    }

    #[test]
    fn test_proxy_entry_chrome_proxy_arg_socks5() {
        let entry = ProxyEntry::parse("socks5://10.0.0.1:1080").unwrap();
        assert_eq!(
            entry.chrome_proxy_arg(),
            "--proxy-server=socks5://10.0.0.1:1080"
        );
    }

    #[test]
    fn test_proxy_entry_case_insensitive_scheme() {
        let entry = ProxyEntry::parse("HTTP://127.0.0.1:8080").unwrap();
        assert_eq!(entry.scheme, "http");
    }

    // ===== validate_proxy_list 测试 =====

    #[test]
    fn test_validate_proxy_list_all_valid() {
        let (valid, invalid) = validate_proxy_list(&["http://p1:8080", "socks5://p2:1080"]);
        assert_eq!(valid.len(), 2);
        assert!(invalid.is_empty());
    }

    #[test]
    fn test_validate_proxy_list_mixed() {
        let (valid, invalid) =
            validate_proxy_list(&["http://valid:8080", "invalid", "socks5://also:1080"]);
        assert_eq!(valid.len(), 2);
        assert_eq!(invalid.len(), 1);
        assert_eq!(invalid[0].0, "invalid");
    }

    #[test]
    fn test_validate_proxy_list_all_invalid() {
        let (valid, invalid) = validate_proxy_list(&["invalid1", "invalid2"]);
        assert!(valid.is_empty());
        assert_eq!(invalid.len(), 2);
    }

    #[test]
    fn test_validate_proxy_list_empty() {
        let (valid, invalid) = validate_proxy_list(&[]);
        assert!(valid.is_empty());
        assert!(invalid.is_empty());
    }
}
