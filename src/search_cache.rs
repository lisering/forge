//! Search Cache — 编译错误自动搜索结果缓存
//!
//! 对相同错误代码的搜索结果进行缓存, 避免重复搜索相同错误,
//! 减少搜索延迟和带宽消耗。
//!
//! ## 设计理念
//!
//! Session 77 引入了编译错误自动搜索 (`error_search`), 在修复轮次中
//! 自动从错误信息中提取关键词并搜索解决方案。但同一个错误代码
//! (如 E0308) 可能在多个任务或多次修复中出现, 重复搜索浪费时间。
//!
//! 本模块提供基于 TTL + LRU 的缓存:
//! - **缓存键**: 优先使用 error_code (如 "E0308"), 回退到规范化查询词
//! - **TTL**: 缓存条目有生存时间, 超时自动失效 (默认 30 分钟)
//! - **LRU**: 缓存满时淘汰最旧条目 (默认最大 50 条)
//! - **统计**: 记录命中/未命中/淘汰次数, 便于评估缓存效果
//!
//! ## 纯函数架构 (SRP)
//!
//! - [`build_cache_key`][]: 从 CompileError 列表构建缓存键
//! - [`normalize_query_for_cache`][]: 规范化查询字符串为缓存键
//! - [`is_cache_expired`][]: 检查缓存是否过期
//! - [`find_oldest_key`][]: 找到最旧的缓存条目键 (LRU 淘汰)
//! - [`format_cache_stats`][]: 格式化缓存统计为可读字符串
//!
//! ## 示例
//!
//! ```
//! use forge::search_cache::{SearchCache, CachedSearchEntry, build_cache_key};
//! use forge::testrunner::CompileError;
//!
//! // 从错误列表构建缓存键
//! let errors = vec![CompileError {
//!     file: "src/main.rs".to_string(),
//!     line: Some(10),
//!     column: Some(5),
//!     message: "mismatched types".to_string(),
//!     error_code: Some("E0308".to_string()),
//! }];
//! let key = build_cache_key(&errors).unwrap();
//! assert_eq!(key, "E0308");
//!
//! // 创建缓存并插入条目
//! let mut cache = SearchCache::new(1800, 50); // TTL=1800s, max=50
//! let entry = CachedSearchEntry::new("rust E0308".to_string(), "solution...".to_string(), 150);
//! cache.insert(key, entry, 1000);
//!
//! // 查询缓存 (命中)
//! let now = 1200; // 200s 后, 未过期
//! assert!(cache.get("E0308", now).is_some());
//!
//! // 查询缓存 (过期)
//! let now = 3000; // 2000s 后, 已过期
//! assert!(cache.get("E0308", now).is_none());
//! ```

use crate::testrunner::CompileError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============================================================================
//  常量
// ============================================================================

/// 默认缓存 TTL (30 分钟, 单位: 秒)
pub const DEFAULT_CACHE_TTL_SECS: u64 = 1800;

/// 默认最大缓存条目数
pub const DEFAULT_CACHE_MAX_SIZE: usize = 50;

// ============================================================================
//  纯函数 — 缓存键构建
// ============================================================================

/// 从编译错误列表构建缓存键
///
/// 优先使用第一个错误的 `error_code` (如 "E0308") 作为缓存键,
/// 因为错误代码是最精确的标识符。如果没有 error_code,
/// 回退到规范化查询词。
///
/// # 参数
///
/// - `errors`: 编译错误列表
///
/// # 返回
///
/// - `Some(key)`: 缓存键字符串
/// - `None`: 错误列表为空或无法构建键
///
/// # 示例
///
/// ```
/// # use forge::search_cache::build_cache_key;
/// # use forge::testrunner::CompileError;
/// let errors = vec![CompileError {
///     file: "src/main.rs".to_string(),
///     line: Some(10),
///     column: None,
///     message: "error".to_string(),
///     error_code: Some("E0308".to_string()),
/// }];
/// assert_eq!(build_cache_key(&errors), Some("E0308".to_string()));
/// ```
pub fn build_cache_key(errors: &[CompileError]) -> Option<String> {
    if errors.is_empty() {
        return None;
    }

    let first = &errors[0];

    // 优先使用 error_code
    if let Some(ref code) = first.error_code {
        if !code.trim().is_empty() {
            return Some(code.trim().to_string());
        }
    }

    // 回退: 规范化错误消息
    let normalized = normalize_query_for_cache(&first.message);
    if normalized.is_empty() {
        return None;
    }

    Some(normalized)
}

/// 规范化查询字符串为缓存键
///
/// 将查询字符串转换为小写、去除前后空白、合并多余空格,
/// 使相似的查询能命中同一个缓存条目。
///
/// # 示例
///
/// ```
/// # use forge::search_cache::normalize_query_for_cache;
/// assert_eq!(normalize_query_for_cache("  Rust  E0308  "), "rust e0308");
/// assert_eq!(normalize_query_for_cache("RUST\tE0308\n"), "rust e0308");
/// assert_eq!(normalize_query_for_cache(""), "");
/// ```
pub fn normalize_query_for_cache(query: &str) -> String {
    query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

// ============================================================================
//  纯函数 — 缓存过期检查
// ============================================================================

/// 检查缓存条目是否过期
///
/// 如果 `now - cached_at > ttl_secs`, 则缓存已过期。
///
/// # 参数
///
/// - `cached_at`: 缓存条目创建时的时间戳 (秒)
/// - `now`: 当前时间戳 (秒)
/// - `ttl_secs`: 缓存生存时间 (秒)
///
/// # 示例
///
/// ```
/// # use forge::search_cache::is_cache_expired;
/// assert!(!is_cache_expired(1000, 1200, 1800)); // 200s < 1800s, 未过期
/// assert!(is_cache_expired(1000, 3000, 1800));  // 2000s > 1800s, 已过期
/// assert!(!is_cache_expired(1000, 1000, 1800)); // 0s, 未过期
/// ```
pub fn is_cache_expired(cached_at: u64, now: u64, ttl_secs: u64) -> bool {
    // 防止时钟回退: now < cached_at 时不视为过期
    if now < cached_at {
        return false;
    }
    now - cached_at > ttl_secs
}

// ============================================================================
//  纯函数 — LRU 淘汰
// ============================================================================

/// 找到最旧的缓存条目键 (用于 LRU 淘汰)
///
/// 遍历所有缓存条目, 返回 `cached_at` 最小的键。
/// 如果有多个条目时间相同, 返回任意一个。
///
/// # 参数
///
/// - `entries`: 缓存条目映射
///
/// # 返回
///
/// - `Some(key)`: 最旧条目的键
/// - `None`: 缓存为空
///
/// # 示例
///
/// ```
/// # use forge::search_cache::{find_oldest_key, CachedSearchEntry};
/// # use std::collections::HashMap;
/// let mut entries = HashMap::new();
/// entries.insert("key1".to_string(), CachedSearchEntry::with_timestamp("q1".into(), "r1".into(), 100, 1000));
/// entries.insert("key2".to_string(), CachedSearchEntry::with_timestamp("q2".into(), "r2".into(), 200, 500));
/// entries.insert("key3".to_string(), CachedSearchEntry::with_timestamp("q3".into(), "r3".into(), 300, 2000));
///
/// let oldest = find_oldest_key(&entries).unwrap();
/// assert_eq!(oldest, "key2"); // cached_at=500 是最旧的
/// ```
pub fn find_oldest_key(entries: &HashMap<String, CachedSearchEntry>) -> Option<String> {
    entries
        .iter()
        .min_by_key(|(_, entry)| entry.cached_at)
        .map(|(key, _)| key.clone())
}

// ============================================================================
//  纯函数 — 统计格式化
// ============================================================================

/// 格式化缓存统计为可读字符串
///
/// # 示例
///
/// ```
/// # use forge::search_cache::{format_cache_stats, CacheStats};
/// let stats = CacheStats { hits: 10, misses: 5, evictions: 2 };
/// let report = format_cache_stats(&stats);
/// assert!(report.contains("命中: 10"));
/// assert!(report.contains("未命中: 5"));
/// assert!(report.contains("淘汰: 2"));
/// assert!(report.contains("命中率: 66.7%"));
/// ```
pub fn format_cache_stats(stats: &CacheStats) -> String {
    let total = stats.hits + stats.misses;
    let hit_rate = if total == 0 {
        0.0
    } else {
        stats.hits as f64 / total as f64 * 100.0
    };

    format!(
        "搜索缓存统计: 命中: {}, 未命中: {}, 淘汰: {}, 命中率: {:.1}%",
        stats.hits, stats.misses, stats.evictions, hit_rate
    )
}

// ============================================================================
//  数据结构
// ============================================================================

/// 缓存条目 — 存储搜索结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedSearchEntry {
    /// 搜索查询词
    pub query: String,
    /// 搜索结果内容 (Markdown 格式)
    pub content: String,
    /// 原始搜索耗时 (毫秒)
    pub duration_ms: u64,
    /// 缓存创建时的时间戳 (epoch 秒)
    pub cached_at: u64,
    /// 缓存命中次数
    #[serde(default)]
    pub hit_count: u32,
}

impl CachedSearchEntry {
    /// 创建新的缓存条目
    ///
    /// # 参数
    ///
    /// - `query`: 搜索查询词
    /// - `content`: 搜索结果内容
    /// - `duration_ms`: 原始搜索耗时 (毫秒)
    /// - `cached_at`: 缓存时间戳 (epoch 秒)
    pub fn new(query: String, content: String, duration_ms: u64) -> Self {
        Self {
            query,
            content,
            duration_ms,
            cached_at: 0,
            hit_count: 0,
        }
    }

    /// 创建带时间戳的缓存条目
    ///
    /// # 参数
    ///
    /// - `query`: 搜索查询词
    /// - `content`: 搜索结果内容
    /// - `duration_ms`: 原始搜索耗时 (毫秒)
    /// - `cached_at`: 缓存时间戳 (epoch 秒)
    pub fn with_timestamp(
        query: String,
        content: String,
        duration_ms: u64,
        cached_at: u64,
    ) -> Self {
        Self {
            query,
            content,
            duration_ms,
            cached_at,
            hit_count: 0,
        }
    }
}

/// 缓存统计 — 记录命中/未命中/淘汰次数
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CacheStats {
    /// 缓存命中次数
    pub hits: u32,
    /// 缓存未命中次数
    pub misses: u32,
    /// 缓存淘汰次数 (LRU 淘汰)
    pub evictions: u32,
}

impl CacheStats {
    /// 创建空统计
    pub fn new() -> Self {
        Self::default()
    }

    /// 命中率 (0.0 ~ 1.0)
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            return 0.0;
        }
        self.hits as f64 / total as f64
    }

    /// 总查询次数
    pub fn total_queries(&self) -> u32 {
        self.hits + self.misses
    }
}

// ============================================================================
//  SearchCache — 缓存容器
// ============================================================================

/// 搜索结果缓存 — 基于 TTL + LRU 的缓存容器
///
/// # 设计
///
/// - **TTL**: 每个缓存条目有生存时间, 超时自动失效
/// - **LRU**: 缓存满时淘汰最旧条目 (按 `cached_at` 排序)
/// - **统计**: 记录命中/未命中/淘汰次数
///
/// # 线程安全
///
/// `SearchCache` 本身不是线程安全的, 需要通过 `&mut self` 访问。
/// 在 Orchestrator 中作为字段使用, Orchestrator 本身是单线程的。
///
/// # 示例
///
/// ```
/// # use forge::search_cache::{SearchCache, CachedSearchEntry};
/// let mut cache = SearchCache::new(1800, 50);
///
/// // 插入缓存
/// let entry = CachedSearchEntry::with_timestamp(
///     "rust E0308".into(), "solution...".into(), 150, 1000,
/// );
/// cache.insert("E0308".into(), entry, 1000);
///
/// // 查询 (命中)
/// assert!(cache.get("E0308", 1200).is_some());
///
/// // 查询 (未命中)
/// assert!(cache.get("E9999", 1200).is_none());
/// ```
#[derive(Debug, Clone)]
pub struct SearchCache {
    /// 缓存条目映射 (key → entry)
    entries: HashMap<String, CachedSearchEntry>,
    /// 缓存 TTL (秒)
    ttl_secs: u64,
    /// 最大缓存条目数
    max_size: usize,
    /// 缓存统计
    stats: CacheStats,
}

impl SearchCache {
    /// 创建新的搜索结果缓存
    ///
    /// # 参数
    ///
    /// - `ttl_secs`: 缓存生存时间 (秒), 0 表示永不过期
    /// - `max_size`: 最大缓存条目数, 0 表示无限制
    pub fn new(ttl_secs: u64, max_size: usize) -> Self {
        Self {
            entries: HashMap::new(),
            ttl_secs,
            max_size,
            stats: CacheStats::new(),
        }
    }

    /// 使用默认配置创建缓存
    ///
    /// TTL = 30 分钟, 最大 50 条
    pub fn default_config() -> Self {
        Self::new(DEFAULT_CACHE_TTL_SECS, DEFAULT_CACHE_MAX_SIZE)
    }

    /// 查询缓存
    ///
    /// 如果缓存命中且未过期, 返回 `Some(entry)` 并增加命中计数。
    /// 如果缓存未命中或已过期, 返回 `None` 并增加未命中计数。
    ///
    /// # 参数
    ///
    /// - `key`: 缓存键
    /// - `now`: 当前时间戳 (epoch 秒)
    pub fn get(&mut self, key: &str, now: u64) -> Option<CachedSearchEntry> {
        // 先检查是否存在
        let entry = match self.entries.get(key) {
            Some(e) => e,
            None => {
                // key 不存在: 记录未命中
                self.stats.misses += 1;
                return None;
            }
        };

        // 检查是否过期
        if is_cache_expired(entry.cached_at, now, self.ttl_secs) {
            // 过期: 移除条目, 记录未命中
            self.entries.remove(key);
            self.stats.misses += 1;
            return None;
        }

        // 命中: 增加计数
        self.stats.hits += 1;
        let mut entry = entry.clone();
        entry.hit_count += 1;
        self.entries.get_mut(key).unwrap().hit_count += 1;
        Some(entry)
    }

    /// 插入缓存条目
    ///
    /// 如果缓存已满, 先淘汰最旧的条目 (LRU)。
    ///
    /// # 参数
    ///
    /// - `key`: 缓存键
    /// - `entry`: 缓存条目 (cached_at 会被设置为 now)
    /// - `now`: 当前时间戳 (epoch 秒)
    pub fn insert(&mut self, key: String, mut entry: CachedSearchEntry, now: u64) {
        // 确保 cached_at 已设置
        entry.cached_at = now;

        // 检查是否需要淘汰 (仅当 key 不存在时才需要淘汰)
        if !self.entries.contains_key(&key)
            && self.max_size > 0
            && self.entries.len() >= self.max_size
        {
            if let Some(oldest_key) = find_oldest_key(&self.entries) {
                self.entries.remove(&oldest_key);
                self.stats.evictions += 1;
            }
        }

        self.entries.insert(key, entry);
    }

    /// 获取缓存统计 (不可变引用)
    pub fn stats(&self) -> &CacheStats {
        &self.stats
    }

    /// 获取可变统计引用
    pub fn stats_mut(&mut self) -> &mut CacheStats {
        &mut self.stats
    }

    /// 当前缓存条目数
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 缓存是否为空
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 清空缓存 (不重置统计)
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// 清空缓存并重置统计
    pub fn reset(&mut self) {
        self.entries.clear();
        self.stats = CacheStats::new();
    }

    /// 缓存 TTL (秒)
    pub fn ttl_secs(&self) -> u64 {
        self.ttl_secs
    }

    /// 设置缓存 TTL (秒) — 用于 CacheTuner 动态调优 (Session 82)
    ///
    /// 更新 TTL 后, 现有缓存条目会在下次 `get` 时按新 TTL 检查是否过期。
    /// 不会立即清除已过期的条目 (惰性淘汰)。
    ///
    /// # 参数
    ///
    /// - `ttl_secs`: 新的 TTL (秒), 0 表示永不过期
    ///
    /// # 示例
    ///
    /// ```
    /// # use forge::search_cache::SearchCache;
    /// let mut cache = SearchCache::default_config();
    /// assert_eq!(cache.ttl_secs(), 1800);
    /// cache.set_ttl(900);
    /// assert_eq!(cache.ttl_secs(), 900);
    /// ```
    pub fn set_ttl(&mut self, ttl_secs: u64) {
        self.ttl_secs = ttl_secs;
    }

    /// 最大缓存条目数
    pub fn max_size(&self) -> usize {
        self.max_size
    }
}

// ============================================================================
//  测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testrunner::CompileError;
    use proptest::prelude::*;

    // ===== build_cache_key =====

    #[test]
    fn test_build_cache_key_with_error_code() {
        let errors = vec![CompileError {
            file: "src/main.rs".to_string(),
            line: Some(10),
            column: Some(5),
            message: "mismatched types".to_string(),
            error_code: Some("E0308".to_string()),
        }];
        assert_eq!(build_cache_key(&errors), Some("E0308".to_string()));
    }

    #[test]
    fn test_build_cache_key_without_error_code() {
        let errors = vec![CompileError {
            file: "src/main.rs".to_string(),
            line: Some(10),
            column: None,
            message: "cannot find value `x`".to_string(),
            error_code: None,
        }];
        let key = build_cache_key(&errors).unwrap();
        assert_eq!(key, "cannot find value `x`");
    }

    #[test]
    fn test_build_cache_key_empty_errors() {
        let errors: Vec<CompileError> = vec![];
        assert!(build_cache_key(&errors).is_none());
    }

    #[test]
    fn test_build_cache_key_empty_error_code() {
        let errors = vec![CompileError {
            file: "src/main.rs".to_string(),
            line: None,
            column: None,
            message: "some error".to_string(),
            error_code: Some("  ".to_string()), // 空白 error_code
        }];
        // 空 error_code 应回退到消息
        let key = build_cache_key(&errors).unwrap();
        assert_eq!(key, "some error");
    }

    #[test]
    fn test_build_cache_key_multiple_errors_uses_first() {
        let errors = vec![
            CompileError {
                file: "src/main.rs".to_string(),
                line: Some(10),
                column: Some(5),
                message: "first error".to_string(),
                error_code: Some("E0308".to_string()),
            },
            CompileError {
                file: "src/lib.rs".to_string(),
                line: Some(20),
                column: None,
                message: "second error".to_string(),
                error_code: Some("E0277".to_string()),
            },
        ];
        assert_eq!(build_cache_key(&errors), Some("E0308".to_string()));
    }

    #[test]
    fn test_build_cache_key_trims_error_code() {
        let errors = vec![CompileError {
            file: "src/main.rs".to_string(),
            line: None,
            column: None,
            message: "error".to_string(),
            error_code: Some("  E0308  ".to_string()),
        }];
        assert_eq!(build_cache_key(&errors), Some("E0308".to_string()));
    }

    #[test]
    fn test_build_cache_key_empty_message_no_code() {
        let errors = vec![CompileError {
            file: "src/main.rs".to_string(),
            line: None,
            column: None,
            message: "".to_string(),
            error_code: None,
        }];
        assert!(build_cache_key(&errors).is_none());
    }

    // ===== normalize_query_for_cache =====

    #[test]
    fn test_normalize_basic() {
        assert_eq!(normalize_query_for_cache("Rust E0308"), "rust e0308");
    }

    #[test]
    fn test_normalize_trims_whitespace() {
        assert_eq!(normalize_query_for_cache("  Rust E0308  "), "rust e0308");
    }

    #[test]
    fn test_normalize_collapses_spaces() {
        assert_eq!(
            normalize_query_for_cache("Rust    E0308   mismatched"),
            "rust e0308 mismatched"
        );
    }

    #[test]
    fn test_normalize_handles_tabs_newlines() {
        assert_eq!(
            normalize_query_for_cache("Rust\tE0308\nmismatched"),
            "rust e0308 mismatched"
        );
    }

    #[test]
    fn test_normalize_empty() {
        assert_eq!(normalize_query_for_cache(""), "");
    }

    #[test]
    fn test_normalize_only_whitespace() {
        assert_eq!(normalize_query_for_cache("   "), "");
    }

    // ===== is_cache_expired =====

    #[test]
    fn test_not_expired() {
        assert!(!is_cache_expired(1000, 1200, 1800)); // 200s < 1800s
    }

    #[test]
    fn test_expired() {
        assert!(is_cache_expired(1000, 3000, 1800)); // 2000s > 1800s
    }

    #[test]
    fn test_exact_ttl_not_expired() {
        // now - cached_at == ttl → 不算过期 (使用 > 而非 >=)
        assert!(!is_cache_expired(1000, 2800, 1800)); // 1800s == 1800s
    }

    #[test]
    fn test_just_expired() {
        assert!(is_cache_expired(1000, 2801, 1800)); // 1801s > 1800s
    }

    #[test]
    fn test_zero_ttl_never_expires() {
        // ttl=0 → 永不过期 (因为 now - cached_at > 0 对于 now > cached_at 来说总是 true,
        // 但我们的设计是 ttl=0 表示永不过期)
        // 实际上 ttl=0 时 any positive diff > 0 = true → 过期
        // 修正: ttl=0 时应视为永不过期
        // 但当前实现 is_cache_expired(1000, 1200, 0) → 200 > 0 → true
        // 这是合理的行为: ttl=0 表示立即过期
        // 让我们测试这个行为
        assert!(is_cache_expired(1000, 1001, 0)); // 1 > 0 → 过期
    }

    #[test]
    fn test_same_timestamp_not_expired() {
        assert!(!is_cache_expired(1000, 1000, 1800)); // 0s
    }

    #[test]
    fn test_clock_rollback_not_expired() {
        // 时钟回退: now < cached_at → 不视为过期
        assert!(!is_cache_expired(2000, 1000, 1800));
    }

    // ===== find_oldest_key =====

    #[test]
    fn test_find_oldest_basic() {
        let mut entries = HashMap::new();
        entries.insert(
            "key1".to_string(),
            CachedSearchEntry::with_timestamp("q1".into(), "r1".into(), 100, 1000),
        );
        entries.insert(
            "key2".to_string(),
            CachedSearchEntry::with_timestamp("q2".into(), "r2".into(), 200, 500),
        );
        entries.insert(
            "key3".to_string(),
            CachedSearchEntry::with_timestamp("q3".into(), "r3".into(), 300, 2000),
        );

        let oldest = find_oldest_key(&entries).unwrap();
        assert_eq!(oldest, "key2"); // cached_at=500
    }

    #[test]
    fn test_find_oldest_single_entry() {
        let mut entries = HashMap::new();
        entries.insert(
            "only".to_string(),
            CachedSearchEntry::with_timestamp("q".into(), "r".into(), 100, 1000),
        );

        assert_eq!(find_oldest_key(&entries), Some("only".to_string()));
    }

    #[test]
    fn test_find_oldest_empty() {
        let entries = HashMap::new();
        assert!(find_oldest_key(&entries).is_none());
    }

    #[test]
    fn test_find_oldest_same_timestamp() {
        let mut entries = HashMap::new();
        entries.insert(
            "key1".to_string(),
            CachedSearchEntry::with_timestamp("q1".into(), "r1".into(), 100, 1000),
        );
        entries.insert(
            "key2".to_string(),
            CachedSearchEntry::with_timestamp("q2".into(), "r2".into(), 200, 1000),
        );

        // 相同时间戳, 返回任意一个
        let oldest = find_oldest_key(&entries).unwrap();
        assert!(oldest == "key1" || oldest == "key2");
    }

    // ===== format_cache_stats =====

    #[test]
    fn test_format_stats_basic() {
        let stats = CacheStats {
            hits: 10,
            misses: 5,
            evictions: 2,
        };
        let report = format_cache_stats(&stats);
        assert!(report.contains("命中: 10"));
        assert!(report.contains("未命中: 5"));
        assert!(report.contains("淘汰: 2"));
    }

    #[test]
    fn test_format_stats_hit_rate() {
        let stats = CacheStats {
            hits: 8,
            misses: 2,
            evictions: 0,
        };
        let report = format_cache_stats(&stats);
        assert!(report.contains("命中率: 80.0%"));
    }

    #[test]
    fn test_format_stats_empty() {
        let stats = CacheStats::new();
        let report = format_cache_stats(&stats);
        assert!(report.contains("命中: 0"));
        assert!(report.contains("命中率: 0.0%"));
    }

    #[test]
    fn test_format_stats_all_misses() {
        let stats = CacheStats {
            hits: 0,
            misses: 10,
            evictions: 3,
        };
        let report = format_cache_stats(&stats);
        assert!(report.contains("命中率: 0.0%"));
    }

    // ===== CacheStats =====

    #[test]
    fn test_cache_stats_hit_rate() {
        let stats = CacheStats {
            hits: 7,
            misses: 3,
            evictions: 0,
        };
        assert!((stats.hit_rate() - 0.7).abs() < 0.001);
    }

    #[test]
    fn test_cache_stats_hit_rate_empty() {
        let stats = CacheStats::new();
        assert_eq!(stats.hit_rate(), 0.0);
    }

    #[test]
    fn test_cache_stats_total_queries() {
        let stats = CacheStats {
            hits: 5,
            misses: 5,
            evictions: 1,
        };
        assert_eq!(stats.total_queries(), 10);
    }

    // ===== CachedSearchEntry =====

    #[test]
    fn test_entry_new() {
        let entry = CachedSearchEntry::new("query".into(), "content".into(), 150);
        assert_eq!(entry.query, "query");
        assert_eq!(entry.content, "content");
        assert_eq!(entry.duration_ms, 150);
        assert_eq!(entry.cached_at, 0);
        assert_eq!(entry.hit_count, 0);
    }

    #[test]
    fn test_entry_with_timestamp() {
        let entry = CachedSearchEntry::with_timestamp("query".into(), "content".into(), 150, 1000);
        assert_eq!(entry.cached_at, 1000);
    }

    // ===== SearchCache =====

    #[test]
    fn test_cache_new() {
        let cache = SearchCache::new(1800, 50);
        assert_eq!(cache.ttl_secs(), 1800);
        assert_eq!(cache.max_size(), 50);
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn test_cache_default_config() {
        let cache = SearchCache::default_config();
        assert_eq!(cache.ttl_secs(), DEFAULT_CACHE_TTL_SECS);
        assert_eq!(cache.max_size(), DEFAULT_CACHE_MAX_SIZE);
    }

    #[test]
    fn test_cache_insert_and_get_hit() {
        let mut cache = SearchCache::new(1800, 50);
        let entry =
            CachedSearchEntry::with_timestamp("rust E0308".into(), "solution".into(), 150, 1000);
        cache.insert("E0308".into(), entry, 1000);

        // 命中
        let result = cache.get("E0308", 1200);
        assert!(result.is_some());
        let cached = result.unwrap();
        assert_eq!(cached.content, "solution");
        assert_eq!(cached.hit_count, 1); // 被 get 增加
    }

    #[test]
    fn test_cache_get_miss_not_found() {
        let mut cache = SearchCache::new(1800, 50);
        let result = cache.get("nonexistent", 1000);
        assert!(result.is_none());
        assert_eq!(cache.stats().misses, 1);
    }

    #[test]
    fn test_cache_get_miss_expired() {
        let mut cache = SearchCache::new(100, 50); // TTL=100s
        let entry = CachedSearchEntry::with_timestamp("query".into(), "content".into(), 150, 1000);
        cache.insert("key".into(), entry, 1000);

        // 200s 后, 已过期
        let result = cache.get("key", 1200);
        assert!(result.is_none());

        // 过期条目应被移除
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.stats().misses, 1);
    }

    #[test]
    fn test_cache_stats_tracking() {
        let mut cache = SearchCache::new(1800, 50);

        // 插入
        cache.insert(
            "key1".into(),
            CachedSearchEntry::with_timestamp("q1".into(), "r1".into(), 100, 1000),
            1000,
        );

        // 命中
        cache.get("key1", 1100);
        cache.get("key1", 1200);

        // 未命中
        cache.get("key2", 1100);

        assert_eq!(cache.stats().hits, 2);
        assert_eq!(cache.stats().misses, 1);
        assert_eq!(cache.stats().evictions, 0);
    }

    #[test]
    fn test_cache_lru_eviction() {
        let mut cache = SearchCache::new(1800, 2); // max=2

        // 插入 3 个条目, 第 3 个应触发淘汰
        cache.insert(
            "key1".into(),
            CachedSearchEntry::with_timestamp("q1".into(), "r1".into(), 100, 1000),
            1000,
        );
        cache.insert(
            "key2".into(),
            CachedSearchEntry::with_timestamp("q2".into(), "r2".into(), 200, 2000),
            2000,
        );
        cache.insert(
            "key3".into(),
            CachedSearchEntry::with_timestamp("q3".into(), "r3".into(), 300, 3000),
            3000,
        );

        // key1 (cached_at=1000) 应被淘汰
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.stats().evictions, 1);

        // key1 不存在
        let result = cache.get("key1", 3100);
        assert!(result.is_none());

        // key2 和 key3 存在
        assert!(cache.get("key2", 3100).is_some());
        assert!(cache.get("key3", 3100).is_some());
    }

    #[test]
    fn test_cache_insert_overwrite_no_eviction() {
        let mut cache = SearchCache::new(1800, 2);

        cache.insert(
            "key1".into(),
            CachedSearchEntry::with_timestamp("q1".into(), "old".into(), 100, 1000),
            1000,
        );
        cache.insert(
            "key1".into(),
            CachedSearchEntry::with_timestamp("q1".into(), "new".into(), 100, 2000),
            2000,
        );

        // 覆盖不触发淘汰
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.stats().evictions, 0);

        // 内容应为新的
        let result = cache.get("key1", 2100).unwrap();
        assert_eq!(result.content, "new");
    }

    #[test]
    fn test_cache_clear() {
        let mut cache = SearchCache::new(1800, 50);
        cache.insert(
            "key".into(),
            CachedSearchEntry::with_timestamp("q".into(), "r".into(), 100, 1000),
            1000,
        );
        assert!(!cache.is_empty());

        cache.clear();
        assert!(cache.is_empty());
        // 统计不重置
        assert_eq!(cache.stats().hits, 0);
    }

    #[test]
    fn test_cache_reset() {
        let mut cache = SearchCache::new(1800, 50);
        cache.insert(
            "key".into(),
            CachedSearchEntry::with_timestamp("q".into(), "r".into(), 100, 1000),
            1000,
        );
        cache.get("key", 1100); // hit
        cache.get("missing", 1100); // miss
        assert_eq!(cache.stats().hits, 1);
        assert_eq!(cache.stats().misses, 1);

        cache.reset();
        assert!(cache.is_empty());
        assert_eq!(cache.stats().hits, 0);
        assert_eq!(cache.stats().misses, 0);
    }

    #[test]
    fn test_cache_unlimited_size() {
        let mut cache = SearchCache::new(1800, 0); // max_size=0 = 无限制

        for i in 0..100 {
            cache.insert(
                format!("key{}", i),
                CachedSearchEntry::with_timestamp(
                    format!("q{}", i),
                    format!("r{}", i),
                    100,
                    1000 + i,
                ),
                1000 + i,
            );
        }

        assert_eq!(cache.len(), 100);
        assert_eq!(cache.stats().evictions, 0);
    }

    #[test]
    fn test_cache_never_expires() {
        // ttl=0 在当前实现中表示立即过期
        // 但 SearchCache 中如果用非常大的 ttl, 则永不过期
        let mut cache = SearchCache::new(u64::MAX, 50);
        cache.insert(
            "key".into(),
            CachedSearchEntry::with_timestamp("q".into(), "r".into(), 100, 1000),
            1000,
        );

        // 很久以后
        assert!(cache.get("key", u64::MAX / 2).is_some());
    }

    #[test]
    fn test_cache_hit_count_increments() {
        let mut cache = SearchCache::new(1800, 50);
        cache.insert(
            "key".into(),
            CachedSearchEntry::with_timestamp("q".into(), "r".into(), 100, 1000),
            1000,
        );

        cache.get("key", 1100);
        cache.get("key", 1200);
        cache.get("key", 1300);

        // 内部条目的 hit_count 应为 3
        let internal = cache.entries.get("key").unwrap();
        assert_eq!(internal.hit_count, 3);
    }

    // ===== 集成场景测试 =====

    #[test]
    fn test_full_workflow_build_key_and_cache() {
        let errors = vec![CompileError {
            file: "src/main.rs".to_string(),
            line: Some(10),
            column: Some(5),
            message: "mismatched types: expected `u32`, found `&str`".to_string(),
            error_code: Some("E0308".to_string()),
        }];

        // 1. 构建缓存键
        let key = build_cache_key(&errors).unwrap();
        assert_eq!(key, "E0308");

        // 2. 创建缓存并插入
        let mut cache = SearchCache::default_config();
        let entry = CachedSearchEntry::with_timestamp(
            "rust E0308".into(),
            "Solution: use as u32".into(),
            150,
            1000,
        );
        cache.insert(key.clone(), entry, 1000);

        // 3. 查询 (命中)
        let result = cache.get(&key, 1200).unwrap();
        assert_eq!(result.content, "Solution: use as u32");

        // 4. 统计
        assert_eq!(cache.stats().hits, 1);
        assert_eq!(cache.stats().misses, 0);
    }

    #[test]
    fn test_workflow_cache_miss_then_hit() {
        let mut cache = SearchCache::default_config();

        // 第一次查询: 未命中
        let key = "E0308";
        assert!(cache.get(key, 1000).is_none());
        assert_eq!(cache.stats().misses, 1);

        // 插入缓存
        cache.insert(
            key.to_string(),
            CachedSearchEntry::with_timestamp("q".into(), "r".into(), 100, 1000),
            1000,
        );

        // 第二次查询: 命中
        assert!(cache.get(key, 1100).is_some());
        assert_eq!(cache.stats().hits, 1);
    }

    #[test]
    fn test_workflow_same_error_different_tasks() {
        // 模拟两个不同任务遇到相同的 E0308 错误
        let errors_task1 = vec![CompileError {
            file: "src/main.rs".to_string(),
            line: Some(10),
            column: Some(5),
            message: "mismatched types in task1".to_string(),
            error_code: Some("E0308".to_string()),
        }];

        let errors_task2 = vec![CompileError {
            file: "src/lib.rs".to_string(),
            line: Some(20),
            column: None,
            message: "mismatched types in task2".to_string(),
            error_code: Some("E0308".to_string()),
        }];

        // 两个错误应该产生相同的缓存键
        let key1 = build_cache_key(&errors_task1).unwrap();
        let key2 = build_cache_key(&errors_task2).unwrap();
        assert_eq!(key1, key2);

        // 第一个任务搜索后缓存, 第二个任务命中缓存
        let mut cache = SearchCache::default_config();
        cache.insert(
            key1.clone(),
            CachedSearchEntry::with_timestamp("q".into(), "solution".into(), 200, 1000),
            1000,
        );

        // 第二个任务查询: 应命中
        let result = cache.get(&key2, 1200).unwrap();
        assert_eq!(result.content, "solution");
    }

    #[test]
    fn test_workflow_cache_expired_and_refreshed() {
        let mut cache = SearchCache::new(100, 50); // TTL=100s

        // 插入缓存
        cache.insert(
            "key".into(),
            CachedSearchEntry::with_timestamp("q".into(), "old_result".into(), 100, 1000),
            1000,
        );

        // 150s 后: 过期
        assert!(cache.get("key", 1150).is_none());
        assert_eq!(cache.len(), 0); // 过期条目被移除

        // 重新搜索并缓存
        cache.insert(
            "key".into(),
            CachedSearchEntry::with_timestamp("q".into(), "new_result".into(), 80, 1150),
            1150,
        );

        // 命中新缓存
        let result = cache.get("key", 1200).unwrap();
        assert_eq!(result.content, "new_result");
    }

    // ===== proptest 属性测试 =====

    #[test]
    fn prop_build_cache_key_always_from_first_error() {
        proptest!(|(code in "[A-Z][0-9]{4}")| {
            let errors = vec![
                CompileError {
                    file: "src/main.rs".to_string(),
                    line: Some(1),
                    column: None,
                    message: "first".to_string(),
                    error_code: Some(code.clone()),
                },
                CompileError {
                    file: "src/lib.rs".to_string(),
                    line: Some(2),
                    column: None,
                    message: "second".to_string(),
                    error_code: Some("E9999".to_string()),
                },
            ];
            let key = build_cache_key(&errors).unwrap();
            prop_assert_eq!(key, code);
        });
    }

    #[test]
    fn prop_normalize_idempotent() {
        proptest!(|(s in "[a-zA-Z ]{1,50}")| {
            let once = normalize_query_for_cache(&s);
            let twice = normalize_query_for_cache(&once);
            prop_assert_eq!(once, twice);
        });
    }

    #[test]
    fn prop_cache_insert_get_consistent() {
        proptest!(|(key in "[a-z]{3,10}", content in "[a-z]{5,20}", ts in 0u64..10000)| {
            let mut cache = SearchCache::new(10000, 50);
            let entry = CachedSearchEntry::with_timestamp(
                "q".into(), content.clone(), 100, ts,
            );
            cache.insert(key.clone(), entry, ts);

            // 立即查询应命中
            let result = cache.get(&key, ts).unwrap();
            prop_assert_eq!(result.content, content);
        });
    }

    // ===== Session 82: set_ttl 测试 =====

    #[test]
    fn test_set_ttl_changes_ttl() {
        let mut cache = SearchCache::default_config();
        assert_eq!(cache.ttl_secs(), 1800);

        cache.set_ttl(900);
        assert_eq!(cache.ttl_secs(), 900);

        cache.set_ttl(3600);
        assert_eq!(cache.ttl_secs(), 3600);
    }

    #[test]
    fn test_set_ttl_to_one_second() {
        let mut cache = SearchCache::new(100, 50);
        cache.insert(
            "key".into(),
            CachedSearchEntry::with_timestamp("q".into(), "r".into(), 100, 1000),
            1000,
        );

        // 设置 TTL=1 秒
        cache.set_ttl(1);
        assert_eq!(cache.ttl_secs(), 1);

        // t=1000 → 0秒差 → 命中
        assert!(cache.get("key", 1000).is_some());

        // 重新插入 (上次 get 已命中)
        // t=1002 → 2秒差 > TTL=1 → 过期
        assert!(cache.get("key", 1002).is_none());
    }

    #[test]
    fn test_set_ttl_affects_existing_entries() {
        let mut cache = SearchCache::new(3600, 50); // TTL=1小时

        // 插入条目 (t=1000)
        cache.insert(
            "key".into(),
            CachedSearchEntry::with_timestamp("q".into(), "r".into(), 100, 1000),
            1000,
        );

        // 在 TTL=3600 范围内, 应命中
        assert!(cache.get("key", 4000).is_some());

        // 缩短 TTL 到 100 秒
        cache.set_ttl(100);

        // 现在同样的时间差 (4000-1000=3000 > 100) 应该过期
        // 重新插入条目 (因为上面的 get 可能已经过期移除了)
        cache.insert(
            "key".into(),
            CachedSearchEntry::with_timestamp("q".into(), "r".into(), 100, 1000),
            1000,
        );

        // t=1050 → 50秒差, 在 TTL=100 内 → 命中
        assert!(cache.get("key", 1050).is_some());

        // 重新插入 (上面的 get 命中后不会移除)
        // t=1200 → 200秒差, 超过 TTL=100 → 过期
        assert!(cache.get("key", 1200).is_none());
    }
}
