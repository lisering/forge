//! Live Continuation — 借鉴 ds4 `ANTHROPIC_LIVE_CONTINUATION.md` 设计
//!
//! 追踪消息 ID 和内容指纹，当需要继续对话时，
//! 只发送增量部分而非完整上下文。
//!
//! ## 核心思路
//!
//! 网页 AI 的上下文窗口有限，但网页本身已经保存了历史对话。
//! 与其重复发送完整上下文，不如追踪已发送的消息 ID，
//! 只发送新增的增量部分。
//!
//! ## 流程
//!
//! ```text
//! 每条消息 → 生成 MessageId (哈希指纹) → 存入 MessageTracker
//!   → 新消息到来时 → 检查是否已发送过 → 只发送未发送的增量
//!   → 对话被压缩/重置 → 清空 tracker → 下次全量发送
//! ```
//!
//! ## 与 RadixTree 的关系
//!
//! - `RadixTree` (radix_tree.rs): 存储完整对话序列，查找最长公共前缀
//! - `LiveContinuation`: 追踪单条消息 ID，检查是否已发送
//! - 两者互补：RadixTree 用于对话级增量，LiveContinuation 用于消息级增量

use std::collections::HashSet;

// ============================================================================
//  MessageId — 消息唯一标识
// ============================================================================

/// 消息 ID — 基于内容的指纹
///
/// 使用 FNV-1a 64-bit 哈希将消息内容映射为唯一 ID。
/// 相同内容 → 相同 ID，不同内容 → (极大概率) 不同 ID。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct MessageId(u64);

impl MessageId {
    /// 从消息文本计算 ID
    ///
    /// 使用 FNV-1a 64-bit 哈希。
    ///
    /// # 示例
    ///
    /// ```
    /// # use forge::live_continuation::MessageId;
    /// let id1 = MessageId::from_text("hello");
    /// let id2 = MessageId::from_text("hello");
    /// let id3 = MessageId::from_text("world");
    /// assert_eq!(id1, id2);
    /// assert_ne!(id1, id3);
    /// ```
    pub fn from_text(text: &str) -> Self {
        // FNV-1a 64-bit (与 radix_tree 模块保持一致)
        const FNV_OFFSET: u64 = 0xcbf29ce484222325;
        const FNV_PRIME: u64 = 0x00000100000001B3;

        let mut hash = FNV_OFFSET;
        for byte in text.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        MessageId(hash)
    }

    /// 从已计算的哈希值创建 ID
    pub fn from_hash(hash: u64) -> Self {
        MessageId(hash)
    }

    /// 获取原始哈希值
    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for MessageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

// ============================================================================
//  MessageTracker — 消息跟踪器
// ============================================================================

/// 消息跟踪器 — 追踪已发送的消息 ID
///
/// 核心功能：
/// - `register()` — 注册已发送的消息
/// - `is_sent()` — 检查消息是否已发送
/// - `filter_unsent()` — 过滤出未发送的消息（增量）
/// - `compute_incremental()` — 计算增量消息并发送结果
///
/// # 设计
///
/// 当对话被压缩或重置后，网页 AI 仍然保存了历史消息。
/// 通过追踪已发送的消息 ID，可以只发送增量部分。
///
/// # 示例
///
/// ```
/// # use forge::live_continuation::MessageTracker;
/// let mut tracker = MessageTracker::new();
///
/// // 注册已发送的消息
/// tracker.register("system_prompt");
/// tracker.register("task_description");
///
/// // 检查新消息是否需要发送
/// assert!(!tracker.is_sent("new_message"));  // 未发送 → 需要发送
/// assert!(tracker.is_sent("system_prompt"));  // 已发送 → 跳过
/// ```
#[derive(Debug, Clone)]
pub struct MessageTracker {
    /// 已发送的消息 ID 集合
    sent_ids: HashSet<MessageId>,
    /// 已发送的消息总数 (统计用)
    total_sent: usize,
    /// 通过复用跳过的消息数 (统计用)
    skipped_count: usize,
}

impl MessageTracker {
    /// 创建新的消息跟踪器
    pub fn new() -> Self {
        Self {
            sent_ids: HashSet::new(),
            total_sent: 0,
            skipped_count: 0,
        }
    }

    /// 注册已发送的消息
    ///
    /// 将消息内容计算为 ID 并存入跟踪器。
    pub fn register(&mut self, text: &str) {
        let id = MessageId::from_text(text);
        self.sent_ids.insert(id);
        self.total_sent += 1;
    }

    /// 注册多条已发送的消息
    pub fn register_many(&mut self, texts: &[&str]) {
        for text in texts {
            self.register(text);
        }
    }

    /// 注册多条已发送的消息 (String 版本)
    pub fn register_many_owned(&mut self, texts: &[String]) {
        for text in texts {
            self.register(text);
        }
    }

    /// 检查消息是否已发送
    pub fn is_sent(&self, text: &str) -> bool {
        let id = MessageId::from_text(text);
        self.sent_ids.contains(&id)
    }

    /// 过滤出未发送的消息（需要发送的增量）
    ///
    /// 返回输入消息中未发送过的那些。
    pub fn filter_unsent(&self, messages: &[String]) -> Vec<String> {
        messages
            .iter()
            .filter(|msg| {
                let id = MessageId::from_text(msg);
                !self.sent_ids.contains(&id)
            })
            .cloned()
            .collect()
    }

    /// 计算增量并发送结果
    ///
    /// 1. 找出未发送的消息（增量）
    /// 2. 将全部消息注册为已发送
    /// 3. 返回增量消息和统计信息
    pub fn compute_incremental(&mut self, messages: &[String]) -> IncrementalResult {
        let delta: Vec<String> = messages
            .iter()
            .filter(|msg| {
                let id = MessageId::from_text(msg);
                !self.sent_ids.contains(&id)
            })
            .cloned()
            .collect();

        let skipped = messages.len() - delta.len();
        self.skipped_count += skipped;

        // 注册全部消息为已发送
        for msg in messages {
            self.register(msg);
        }

        IncrementalResult {
            delta_messages: delta,
            total_messages: messages.len(),
            skipped_count: skipped,
        }
    }

    /// 已发送的独立消息数 (去重后)
    pub fn sent_count(&self) -> usize {
        self.sent_ids.len()
    }

    /// 已发送消息总数 (含重复)
    pub fn total_sent_count(&self) -> usize {
        self.total_sent
    }

    /// 通过复用跳过的消息总数
    pub fn skipped_total(&self) -> usize {
        self.skipped_count
    }

    /// 清空跟踪状态 (对话重置/压缩后调用)
    pub fn clear(&mut self) {
        self.sent_ids.clear();
        self.total_sent = 0;
        self.skipped_count = 0;
    }

    /// 获取已发送消息 ID 的引用 (用于序列化/调试)
    pub fn sent_ids(&self) -> &HashSet<MessageId> {
        &self.sent_ids
    }
}

impl Default for MessageTracker {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
//  IncrementalResult — 增量计算结果
// ============================================================================

/// 增量计算结果 — 包含增量消息和统计信息
#[derive(Debug, Clone)]
pub struct IncrementalResult {
    /// 需要发送的增量消息
    pub delta_messages: Vec<String>,
    /// 总消息数
    pub total_messages: usize,
    /// 跳过的消息数 (已发送过)
    pub skipped_count: usize,
}

impl IncrementalResult {
    /// 是否全部为增量 (无跳过)
    pub fn is_full_send(&self) -> bool {
        self.skipped_count == 0
    }

    /// 节省比例 (0.0 ~ 1.0)
    pub fn saved_ratio(&self) -> f64 {
        if self.total_messages == 0 {
            return 0.0;
        }
        self.skipped_count as f64 / self.total_messages as f64
    }
}

// ============================================================================
//  LiveContinuation — 对话续接管理器
// ============================================================================

/// 对话续接管理器 — 组合消息跟踪和增量计算
///
/// 结合 `MessageTracker` (消息级增量) 和 `ConversationTracker` (对话级增量)，
/// 提供完整的对话续接能力。
///
/// # 工作流
///
/// 1. 新对话开始时，检查是否有已发送的公共前缀 (对话级增量)
/// 2. 对增量部分的消息，检查是否有已发送的单条消息 (消息级增量)
/// 3. 只发送最终的增量消息
/// 4. 将所有消息注册为已发送
///
/// # 示例
///
/// ```
/// # use forge::live_continuation::LiveContinuation;
/// let mut lc = LiveContinuation::new();
///
/// // 第一次：全部需要发送
/// let msgs = vec![
///     "system".to_string(),
///     "task1".to_string(),
///     "result1".to_string(),
/// ];
/// let result = lc.compute_delta(&msgs);
/// assert_eq!(result.delta_messages.len(), 3);
/// lc.mark_sent(&msgs);
///
/// // 第二次：有增量
/// let msgs2 = vec![
///     "system".to_string(),
///     "task1".to_string(),
///     "result1".to_string(),
///     "task2".to_string(),
/// ];
/// let result2 = lc.compute_delta(&msgs2);
/// // system/task1/result1 已发送过 → 跳过
/// assert_eq!(result2.delta_messages.len(), 1);
/// assert_eq!(result2.delta_messages[0], "task2");
/// ```
#[derive(Debug, Clone)]
pub struct LiveContinuation {
    tracker: MessageTracker,
    /// 是否已重置 (对话被压缩/新开后需要重置)
    is_reset: bool,
    /// 重置次数 (统计)
    reset_count: usize,
}

impl LiveContinuation {
    /// 创建新的对话续接管理器
    pub fn new() -> Self {
        Self {
            tracker: MessageTracker::new(),
            is_reset: false,
            reset_count: 0,
        }
    }

    /// 计算增量消息
    ///
    /// 检查每条消息是否已发送过，返回未发送的增量。
    pub fn compute_delta(&self, messages: &[String]) -> IncrementalResult {
        self.tracker.filter_unsent_as_result(messages)
    }

    /// 标记消息为已发送
    pub fn mark_sent(&mut self, messages: &[String]) {
        self.tracker.register_many_owned(messages);
        self.is_reset = false;
    }

    /// 重置续接状态 (对话被压缩/新开后调用)
    ///
    /// 清空消息跟踪器，下次发送将全量。
    pub fn reset(&mut self) {
        self.tracker.clear();
        self.is_reset = true;
        self.reset_count += 1;
    }

    /// 是否处于重置状态
    pub fn is_reset(&self) -> bool {
        self.is_reset
    }

    /// 重置次数
    pub fn reset_count(&self) -> usize {
        self.reset_count
    }

    /// 已发送的独立消息数
    pub fn sent_count(&self) -> usize {
        self.tracker.sent_count()
    }

    /// 通过复用跳过的消息总数
    pub fn skipped_total(&self) -> usize {
        self.tracker.skipped_total()
    }

    /// 获取内部跟踪器引用
    pub fn tracker(&self) -> &MessageTracker {
        &self.tracker
    }

    /// 获取内部跟踪器可变引用
    pub fn tracker_mut(&mut self) -> &mut MessageTracker {
        &mut self.tracker
    }
}

impl Default for LiveContinuation {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
//  MessageTracker 扩展方法
// ============================================================================

impl MessageTracker {
    /// 过滤未发送消息并返回带统计的结果 (不修改状态)
    fn filter_unsent_as_result(&self, messages: &[String]) -> IncrementalResult {
        let delta = self.filter_unsent(messages);
        let skipped = messages.len() - delta.len();
        IncrementalResult {
            delta_messages: delta,
            total_messages: messages.len(),
            skipped_count: skipped,
        }
    }
}

// ============================================================================
//  纯函数 — 工具函数
// ============================================================================

/// 计算消息列表的 ID 集合 (纯函数)
///
/// # 示例
///
/// ```
/// # use forge::live_continuation::{compute_message_ids, MessageId};
/// let msgs = vec!["hello", "world"];
/// let ids = compute_message_ids(&msgs);
/// assert_eq!(ids.len(), 2);
/// assert!(ids.contains(&MessageId::from_text("hello")));
/// ```
pub fn compute_message_ids(messages: &[&str]) -> HashSet<MessageId> {
    messages
        .iter()
        .map(|msg| MessageId::from_text(msg))
        .collect()
}

/// 计算两个消息列表的差异 (纯函数)
///
/// 返回 `messages` 中存在但 `sent` 中不存在的消息。
///
/// # 示例
///
/// ```
/// # use forge::live_continuation::compute_diff;
/// let sent = vec!["a", "b", "c"];
/// let messages = vec!["a", "b", "d", "e"];
/// let diff = compute_diff(&sent, &messages);
/// assert_eq!(diff.len(), 2);
/// assert!(diff.contains(&"d".to_string()));
/// assert!(diff.contains(&"e".to_string()));
/// ```
pub fn compute_diff(sent: &[&str], messages: &[&str]) -> Vec<String> {
    let sent_ids = compute_message_ids(sent);
    messages
        .iter()
        .filter(|msg| {
            let id = MessageId::from_text(msg);
            !sent_ids.contains(&id)
        })
        .map(|s| s.to_string())
        .collect()
}

/// 计算消息列表中重复消息的位置 (纯函数)
///
/// 返回 (索引, 消息内容) 列表，表示该索引处的消息在之前已出现过。
///
/// # 示例
///
/// ```
/// # use forge::live_continuation::find_duplicates;
/// let msgs = vec!["a", "b", "a", "c", "b"];
/// let dups = find_duplicates(&msgs);
/// assert_eq!(dups.len(), 2);
/// assert_eq!(dups[0].0, 2);  // 第2个位置是重复的 "a"
/// assert_eq!(dups[1].0, 4);  // 第4个位置是重复的 "b"
/// ```
pub fn find_duplicates(messages: &[&str]) -> Vec<(usize, String)> {
    let mut seen = HashSet::new();
    let mut result = vec![];

    for (i, msg) in messages.iter().enumerate() {
        let id = MessageId::from_text(msg);
        if seen.contains(&id) {
            result.push((i, msg.to_string()));
        } else {
            seen.insert(id);
        }
    }

    result
}

/// 去重并保持顺序 (纯函数)
///
/// # 示例
///
/// ```
/// # use forge::live_continuation::deduplicate;
/// let msgs = vec!["a", "b", "a", "c", "b"];
/// let deduped = deduplicate(&msgs);
/// assert_eq!(deduped, vec!["a", "b", "c"]);
/// ```
pub fn deduplicate(messages: &[&str]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut result = vec![];

    for msg in messages {
        let id = MessageId::from_text(msg);
        if seen.insert(id) {
            result.push(msg.to_string());
        }
    }

    result
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ===== MessageId 测试 =====

    #[test]
    fn test_message_id_same_text() {
        let id1 = MessageId::from_text("hello");
        let id2 = MessageId::from_text("hello");
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_message_id_different_text() {
        let id1 = MessageId::from_text("hello");
        let id2 = MessageId::from_text("world");
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_message_id_empty_text() {
        let id = MessageId::from_text("");
        assert_eq!(id.as_u64(), 0xcbf29ce484222325);
    }

    #[test]
    fn test_message_id_from_hash() {
        let id = MessageId::from_hash(42);
        assert_eq!(id.as_u64(), 42);
    }

    #[test]
    fn test_message_id_display() {
        let id = MessageId::from_hash(0xdeadbeef);
        assert_eq!(format!("{}", id), "00000000deadbeef");
    }

    #[test]
    fn test_message_id_ordering() {
        let id1 = MessageId::from_hash(1);
        let id2 = MessageId::from_hash(2);
        assert!(id1 < id2);
    }

    // ===== MessageTracker 测试 =====

    #[test]
    fn test_tracker_new() {
        let tracker = MessageTracker::new();
        assert_eq!(tracker.sent_count(), 0);
        assert_eq!(tracker.total_sent_count(), 0);
        assert_eq!(tracker.skipped_total(), 0);
    }

    #[test]
    fn test_tracker_register_and_is_sent() {
        let mut tracker = MessageTracker::new();
        tracker.register("hello");
        assert!(tracker.is_sent("hello"));
        assert!(!tracker.is_sent("world"));
    }

    #[test]
    fn test_tracker_register_many() {
        let mut tracker = MessageTracker::new();
        tracker.register_many(&["a", "b", "c"]);
        assert!(tracker.is_sent("a"));
        assert!(tracker.is_sent("b"));
        assert!(tracker.is_sent("c"));
        assert_eq!(tracker.sent_count(), 3);
    }

    #[test]
    fn test_tracker_register_many_owned() {
        let mut tracker = MessageTracker::new();
        let msgs = vec!["x".to_string(), "y".to_string()];
        tracker.register_many_owned(&msgs);
        assert_eq!(tracker.sent_count(), 2);
    }

    #[test]
    fn test_tracker_filter_unsent_all_new() {
        let tracker = MessageTracker::new();
        let msgs = vec!["a".to_string(), "b".to_string()];
        let unsent = tracker.filter_unsent(&msgs);
        assert_eq!(unsent.len(), 2);
    }

    #[test]
    fn test_tracker_filter_unsent_partial() {
        let mut tracker = MessageTracker::new();
        tracker.register("a");
        let msgs = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let unsent = tracker.filter_unsent(&msgs);
        assert_eq!(unsent.len(), 2);
        assert!(unsent.contains(&"b".to_string()));
        assert!(unsent.contains(&"c".to_string()));
    }

    #[test]
    fn test_tracker_filter_unsent_none() {
        let mut tracker = MessageTracker::new();
        tracker.register("a");
        let msgs = vec!["a".to_string()];
        let unsent = tracker.filter_unsent(&msgs);
        assert!(unsent.is_empty());
    }

    #[test]
    fn test_tracker_compute_incremental_first_time() {
        let mut tracker = MessageTracker::new();
        let msgs = vec!["a".to_string(), "b".to_string()];
        let result = tracker.compute_incremental(&msgs);
        assert_eq!(result.delta_messages.len(), 2);
        assert_eq!(result.skipped_count, 0);
        assert!(result.is_full_send());
        assert_eq!(tracker.sent_count(), 2);
    }

    #[test]
    fn test_tracker_compute_incremental_second_time() {
        let mut tracker = MessageTracker::new();
        let msgs1 = vec!["a".to_string(), "b".to_string()];
        tracker.compute_incremental(&msgs1);

        let msgs2 = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let result = tracker.compute_incremental(&msgs2);
        assert_eq!(result.delta_messages.len(), 1);
        assert_eq!(result.delta_messages[0], "c");
        assert_eq!(result.skipped_count, 2);
        assert!(!result.is_full_send());
        assert_eq!(tracker.skipped_total(), 2);
    }

    #[test]
    fn test_tracker_clear() {
        let mut tracker = MessageTracker::new();
        tracker.register("a");
        tracker.register("b");
        tracker.clear();
        assert_eq!(tracker.sent_count(), 0);
        assert_eq!(tracker.total_sent_count(), 0);
        assert_eq!(tracker.skipped_total(), 0);
    }

    #[test]
    fn test_tracker_empty_messages() {
        let mut tracker = MessageTracker::new();
        let result = tracker.compute_incremental(&[]);
        assert!(result.delta_messages.is_empty());
        assert_eq!(result.total_messages, 0);
    }

    #[test]
    fn test_tracker_duplicate_messages() {
        let mut tracker = MessageTracker::new();
        // 先注册 "a"
        tracker.register("a");
        // 然后发送包含 "a" 的消息
        let msgs = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let result = tracker.compute_incremental(&msgs);
        // "a" 已发送过 → 跳过
        assert_eq!(result.delta_messages.len(), 2);
        assert_eq!(result.skipped_count, 1);
    }

    // ===== IncrementalResult 测试 =====

    #[test]
    fn test_incremental_result_is_full_send() {
        let result = IncrementalResult {
            delta_messages: vec!["a".to_string(), "b".to_string()],
            total_messages: 2,
            skipped_count: 0,
        };
        assert!(result.is_full_send());
        assert_eq!(result.saved_ratio(), 0.0);
    }

    #[test]
    fn test_incremental_result_partial_send() {
        let result = IncrementalResult {
            delta_messages: vec!["c".to_string()],
            total_messages: 3,
            skipped_count: 2,
        };
        assert!(!result.is_full_send());
        assert_eq!(result.saved_ratio(), 2.0 / 3.0);
    }

    // ===== LiveContinuation 测试 =====

    #[test]
    fn test_live_continuation_new() {
        let lc = LiveContinuation::new();
        assert_eq!(lc.sent_count(), 0);
        assert!(!lc.is_reset());
        assert_eq!(lc.reset_count(), 0);
    }

    #[test]
    fn test_live_continuation_first_send() {
        let mut lc = LiveContinuation::new();
        let msgs = vec!["system".to_string(), "task1".to_string()];
        let result = lc.compute_delta(&msgs);
        assert_eq!(result.delta_messages.len(), 2);
        assert!(result.is_full_send());
        lc.mark_sent(&msgs);
        assert_eq!(lc.sent_count(), 2);
    }

    #[test]
    fn test_live_continuation_incremental() {
        let mut lc = LiveContinuation::new();

        let msgs1 = vec![
            "system".to_string(),
            "task1".to_string(),
            "result1".to_string(),
        ];
        lc.mark_sent(&msgs1);

        let msgs2 = vec![
            "system".to_string(),
            "task1".to_string(),
            "result1".to_string(),
            "task2".to_string(),
        ];
        let result = lc.compute_delta(&msgs2);
        assert_eq!(result.delta_messages.len(), 1);
        assert_eq!(result.delta_messages[0], "task2");
        assert_eq!(result.skipped_count, 3);
    }

    #[test]
    fn test_live_continuation_reset() {
        let mut lc = LiveContinuation::new();
        lc.mark_sent(&["msg1".to_string()]);
        assert_eq!(lc.sent_count(), 1);

        lc.reset();
        assert!(lc.is_reset());
        assert_eq!(lc.reset_count(), 1);
        assert_eq!(lc.sent_count(), 0);

        // After reset, all messages are new
        let result = lc.compute_delta(&["msg1".to_string()]);
        assert_eq!(result.delta_messages.len(), 1);
        assert!(result.is_full_send());
    }

    #[test]
    fn test_live_continuation_multiple_resets() {
        let mut lc = LiveContinuation::new();
        lc.reset();
        lc.reset();
        lc.reset();
        assert_eq!(lc.reset_count(), 3);
    }

    #[test]
    fn test_live_continuation_mark_sent_clears_reset() {
        let mut lc = LiveContinuation::new();
        lc.reset();
        assert!(lc.is_reset());
        lc.mark_sent(&["msg".to_string()]);
        assert!(!lc.is_reset());
    }

    #[test]
    fn test_live_continuation_empty_messages() {
        let lc = LiveContinuation::new();
        let result = lc.compute_delta(&[]);
        assert!(result.delta_messages.is_empty());
        assert_eq!(result.total_messages, 0);
    }

    #[test]
    fn test_live_continuation_large_sequence() {
        let mut lc = LiveContinuation::new();

        let msgs1: Vec<String> = (0..100).map(|i| format!("msg_{}", i)).collect();
        lc.mark_sent(&msgs1);

        let mut msgs2 = msgs1.clone();
        msgs2.push("msg_100".to_string());
        msgs2.push("msg_101".to_string());

        let result = lc.compute_delta(&msgs2);
        assert_eq!(result.delta_messages.len(), 2);
        assert_eq!(result.skipped_count, 100);
        assert!(!result.is_full_send());
    }

    #[test]
    fn test_live_continuation_completely_different() {
        let mut lc = LiveContinuation::new();
        lc.mark_sent(&["a".to_string(), "b".to_string()]);

        let result = lc.compute_delta(&["c".to_string(), "d".to_string()]);
        assert_eq!(result.delta_messages.len(), 2);
        assert!(result.is_full_send());
    }

    // ===== 纯函数测试 =====

    #[test]
    fn test_compute_message_ids() {
        let msgs = vec!["hello", "world"];
        let ids = compute_message_ids(&msgs);
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&MessageId::from_text("hello")));
        assert!(ids.contains(&MessageId::from_text("world")));
    }

    #[test]
    fn test_compute_message_ids_with_duplicates() {
        let msgs = vec!["a", "b", "a"];
        let ids = compute_message_ids(&msgs);
        assert_eq!(ids.len(), 2); // 去重后只有2个
    }

    #[test]
    fn test_compute_diff_all_new() {
        let sent: Vec<&str> = vec![];
        let messages = vec!["a", "b", "c"];
        let diff = compute_diff(&sent, &messages);
        assert_eq!(diff.len(), 3);
    }

    #[test]
    fn test_compute_diff_partial() {
        let sent = vec!["a", "b"];
        let messages = vec!["a", "b", "c", "d"];
        let diff = compute_diff(&sent, &messages);
        assert_eq!(diff.len(), 2);
        assert!(diff.contains(&"c".to_string()));
        assert!(diff.contains(&"d".to_string()));
    }

    #[test]
    fn test_compute_diff_none() {
        let sent = vec!["a", "b"];
        let messages = vec!["a", "b"];
        let diff = compute_diff(&sent, &messages);
        assert!(diff.is_empty());
    }

    #[test]
    fn test_find_duplicates_none() {
        let msgs = vec!["a", "b", "c"];
        let dups = find_duplicates(&msgs);
        assert!(dups.is_empty());
    }

    #[test]
    fn test_find_duplicates_some() {
        let msgs = vec!["a", "b", "a", "c", "b"];
        let dups = find_duplicates(&msgs);
        assert_eq!(dups.len(), 2);
        assert_eq!(dups[0].0, 2); // 第2个位置是重复的 "a"
        assert_eq!(dups[0].1, "a");
        assert_eq!(dups[1].0, 4); // 第4个位置是重复的 "b"
        assert_eq!(dups[1].1, "b");
    }

    #[test]
    fn test_deduplicate() {
        let msgs = vec!["a", "b", "a", "c", "b"];
        let deduped = deduplicate(&msgs);
        assert_eq!(deduped, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_deduplicate_empty() {
        let deduped = deduplicate(&[]);
        assert!(deduped.is_empty());
    }

    #[test]
    fn test_deduplicate_all_same() {
        let msgs = vec!["a", "a", "a"];
        let deduped = deduplicate(&msgs);
        assert_eq!(deduped, vec!["a"]);
    }

    // ===== 集成场景测试 =====

    #[test]
    fn test_scenario_context_compaction() {
        // 模拟上下文压缩场景
        let mut lc = LiveContinuation::new();

        // 第一次对话：完整上下文
        let ctx1 = vec![
            "system_prompt".to_string(),
            "task_1".to_string(),
            "result_1".to_string(),
            "task_2".to_string(),
            "result_2".to_string(),
        ];
        let r1 = lc.compute_delta(&ctx1);
        assert_eq!(r1.delta_messages.len(), 5);
        lc.mark_sent(&ctx1);

        // 继续对话：增量
        let ctx2 = vec![
            "system_prompt".to_string(),
            "task_1".to_string(),
            "result_1".to_string(),
            "task_2".to_string(),
            "result_2".to_string(),
            "task_3".to_string(),
            "result_3".to_string(),
        ];
        let r2 = lc.compute_delta(&ctx2);
        assert_eq!(r2.delta_messages.len(), 2); // task_3, result_3
        lc.mark_sent(&ctx2);

        // 对话被压缩 → 重置
        lc.reset();

        // 压缩后：全部需要重新发送
        let ctx3 = vec![
            "compaction_summary".to_string(),
            "task_3".to_string(),
            "result_3".to_string(),
        ];
        let r3 = lc.compute_delta(&ctx3);
        assert_eq!(r3.delta_messages.len(), 3);
        assert!(r3.is_full_send());
    }

    #[test]
    fn test_scenario_error_feedback_loop() {
        // 模拟错误修复循环
        let mut lc = LiveContinuation::new();

        // 发送任务
        let task = vec!["system".to_string(), "write hello world".to_string()];
        let r1 = lc.compute_delta(&task);
        assert_eq!(r1.delta_messages.len(), 2);
        lc.mark_sent(&task);

        // AI 回复错误，发送修复请求
        let fix = vec![
            "system".to_string(),
            "write hello world".to_string(),
            "fix the error: missing semicolon".to_string(),
        ];
        let r2 = lc.compute_delta(&fix);
        assert_eq!(r2.delta_messages.len(), 1);
        assert_eq!(r2.delta_messages[0], "fix the error: missing semicolon");
        lc.mark_sent(&fix);

        // 再次错误修复
        let fix2 = vec![
            "system".to_string(),
            "write hello world".to_string(),
            "fix the error: missing semicolon".to_string(),
            "still broken: fix type mismatch".to_string(),
        ];
        let r3 = lc.compute_delta(&fix2);
        assert_eq!(r3.delta_messages.len(), 1);
        assert_eq!(r3.delta_messages[0], "still broken: fix type mismatch");
    }
}
