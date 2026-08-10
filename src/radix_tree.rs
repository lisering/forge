//! Radix Tree 对话状态跟踪 — 借鉴 ds4 `rax.h` 的基数树设计
//!
//! 用基数树 (Radix Tree / Patricia Trie) 存储对话前缀，
//! 避免在上下文压缩后重复发送相同的上下文。
//!
//! ## 核心思路
//!
//! 当对话被压缩并新开后，新对话的上下文摘要可能与之前存储的对话有公共前缀。
//! 用 Radix Tree 存储所有对话的指纹序列，可以高效找到最长公共前缀，
//! 只发送增量部分而非完整上下文。
//!
//! ## 流程
//!
//! ```text
//! 对话消息序列 → 计算每条消息的指纹 → 在 RadixTree 中查找最长公共前缀
//!   → 公共前缀长度 = 已发送的消息数 → 增量 = 序列[公共前缀长度..]
//!   → 发送增量 → 将完整序列存入 RadixTree
//! ```
//!
//! ## Radix Tree vs 普通 Trie
//!
//! Radix Tree 是压缩前缀树：每条边存储一个字符串段而非单个字符，
//! 内部节点至少有 2 个子节点。对于长公共前缀的对话序列，
//! 比普通 Trie 更节省内存且查找更快。

use std::collections::BTreeMap;
use std::fmt;

// ============================================================================
//  消息指纹 — 将消息内容映射为定长哈希
// ============================================================================

/// 消息指纹 — 用 FNV-1a 哈希将消息内容映射为 u64
///
/// FNV-1a 简单快速，无碰撞风险对短消息足够。
/// 对于碰撞敏感场景可换成 SipHash (std 默认)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct MessageFingerprint(u64);

impl MessageFingerprint {
    /// 计算消息内容的指纹
    ///
    /// 使用 FNV-1a 64-bit 哈希算法。
    ///
    /// # 示例
    ///
    /// ```
    /// # use forge::radix_tree::MessageFingerprint;
    /// let fp1 = MessageFingerprint::from_text("hello");
    /// let fp2 = MessageFingerprint::from_text("hello");
    /// let fp3 = MessageFingerprint::from_text("world");
    /// assert_eq!(fp1, fp2);      // 相同内容 → 相同指纹
    /// assert_ne!(fp1, fp3);      // 不同内容 → 不同指纹
    /// ```
    pub fn from_text(text: &str) -> Self {
        // FNV-1a 64-bit
        const FNV_OFFSET: u64 = 0xcbf29ce484222325;
        const FNV_PRIME: u64 = 0x00000100000001B3;

        let mut hash = FNV_OFFSET;
        for byte in text.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        MessageFingerprint(hash)
    }

    /// 从已计算的哈希值创建指纹 (用于反序列化等)
    pub fn from_hash(hash: u64) -> Self {
        MessageFingerprint(hash)
    }

    /// 获取原始哈希值
    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

impl fmt::Display for MessageFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

// ============================================================================
//  RadixNode — 基数树节点
// ============================================================================

/// Radix Tree 节点
///
/// 每个节点存储一个键段 (而非单个字符)，子节点按首字符排序。
/// `is_terminal` 标记此节点是否代表一个完整存储的键。
#[derive(Debug, Clone)]
struct RadixNode {
    /// 此节点代表的键段
    segment: Vec<MessageFingerprint>,
    /// 子节点 (按子节点段的首元素排序)
    children: BTreeMap<MessageFingerprint, RadixNode>,
    /// 是否为终端节点 (代表一个完整存储的键)
    is_terminal: bool,
    /// 存储的值 (仅终端节点有)
    value: Option<u64>,
}

impl RadixNode {
    fn new(segment: Vec<MessageFingerprint>) -> Self {
        Self {
            segment,
            children: BTreeMap::new(),
            is_terminal: false,
            value: None,
        }
    }

    fn root() -> Self {
        Self {
            segment: vec![],
            children: BTreeMap::new(),
            is_terminal: false,
            value: None,
        }
    }

    /// 计算两个键段的最长公共前缀长度
    fn common_prefix_len(a: &[MessageFingerprint], b: &[MessageFingerprint]) -> usize {
        let min_len = a.len().min(b.len());
        let mut i = 0;
        while i < min_len && a[i] == b[i] {
            i += 1;
        }
        i
    }

    /// 是否为叶子节点 (无子节点)
    #[allow(dead_code)]
    fn is_leaf(&self) -> bool {
        self.children.is_empty()
    }
}

// ============================================================================
//  RadixTree — 基数树主结构
// ============================================================================

/// Radix Tree (压缩前缀树 / Patricia Trie)
///
/// 存储消息指纹序列，支持高效的最长公共前缀查找。
/// 用于跟踪已发送的对话上下文，避免重复发送。
///
/// # 设计
///
/// - 每条边存储一个 `Vec<MessageFingerprint>` 段 (而非单个字符)
/// - 内部节点至少有 2 个子节点 (压缩保证)
/// - 查找/插入/删除时间复杂度 O(k)，k 为键长度
///
/// # 示例
///
/// ```
/// # use forge::radix_tree::{RadixTree, MessageFingerprint};
/// let mut tree = RadixTree::new();
/// let key = vec![
///     MessageFingerprint::from_text("msg1"),
///     MessageFingerprint::from_text("msg2"),
/// ];
/// tree.insert(&key, 42);
/// assert!(tree.contains(&key));
/// assert_eq!(tree.lookup(&key), Some(42));
/// assert_eq!(tree.len(), 1);
/// ```
#[derive(Debug, Clone)]
pub struct RadixTree {
    root: RadixNode,
    size: usize,
}

impl RadixTree {
    /// 创建空的 Radix Tree
    pub fn new() -> Self {
        Self {
            root: RadixNode::root(),
            size: 0,
        }
    }

    /// 返回存储的条目数
    pub fn len(&self) -> usize {
        self.size
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.size == 0
    }

    /// 插入键值对
    ///
    /// 如果键已存在，更新其值并返回旧值。
    pub fn insert(&mut self, key: &[MessageFingerprint], value: u64) -> Option<u64> {
        let result = Self::insert_recursive(&mut self.root, key, value);
        if result.is_none() {
            self.size += 1;
        }
        result
    }

    fn insert_recursive(
        node: &mut RadixNode,
        key: &[MessageFingerprint],
        value: u64,
    ) -> Option<u64> {
        if key.is_empty() {
            let old = if node.is_terminal { node.value } else { None };
            node.is_terminal = true;
            node.value = Some(value);
            return old;
        }

        let first = key[0];

        // 查找匹配的子节点
        if let Some(child) = node.children.get_mut(&first) {
            let cp_len = RadixNode::common_prefix_len(&child.segment, key);

            if cp_len == child.segment.len() {
                // 子节点段是 key 的前缀，递归插入剩余部分
                return Self::insert_recursive(child, &key[cp_len..], value);
            }

            // 需要分裂子节点
            let child_segment = std::mem::take(&mut child.segment);
            let prefix = child_segment[..cp_len].to_vec();
            let child_suffix = child_segment[cp_len..].to_vec();
            let key_suffix = key[cp_len..].to_vec();

            // 保存子节点的其他状态
            let child_children = std::mem::take(&mut child.children);
            let child_is_terminal = child.is_terminal;
            let child_value = child.value.take();

            // 更新子节点为分裂后的前缀节点
            child.segment = prefix;

            // 创建旧子节点的后缀节点
            let mut old_suffix_node = RadixNode::new(child_suffix);
            old_suffix_node.children = child_children;
            old_suffix_node.is_terminal = child_is_terminal;
            old_suffix_node.value = child_value;

            if key_suffix.is_empty() {
                // 新键在分裂点终止
                child.is_terminal = true;
                child.value = Some(value);
                // 添加旧后缀子节点
                if !old_suffix_node.segment.is_empty() {
                    let old_first = old_suffix_node.segment[0];
                    child.children.insert(old_first, old_suffix_node);
                }
            } else {
                // 创建新键的后缀节点
                let mut new_suffix_node = RadixNode::new(key_suffix);
                new_suffix_node.is_terminal = true;
                new_suffix_node.value = Some(value);

                // 添加两个子节点
                if !old_suffix_node.segment.is_empty() {
                    let old_first = old_suffix_node.segment[0];
                    child.children.insert(old_first, old_suffix_node);
                }
                let new_first = new_suffix_node.segment[0];
                child.children.insert(new_first, new_suffix_node);
            }

            return None;
        }

        // 没有匹配的子节点，创建新子节点
        let mut new_child = RadixNode::new(key.to_vec());
        new_child.is_terminal = true;
        new_child.value = Some(value);
        node.children.insert(first, new_child);
        None
    }

    /// 查找键对应的值
    pub fn lookup(&self, key: &[MessageFingerprint]) -> Option<u64> {
        Self::lookup_recursive(&self.root, key)
    }

    fn lookup_recursive(node: &RadixNode, key: &[MessageFingerprint]) -> Option<u64> {
        if key.is_empty() {
            return if node.is_terminal { node.value } else { None };
        }

        let first = key[0];
        if let Some(child) = node.children.get(&first) {
            let cp_len = RadixNode::common_prefix_len(&child.segment, key);
            if cp_len == child.segment.len() {
                return Self::lookup_recursive(child, &key[cp_len..]);
            }
        }
        None
    }

    /// 是否包含某个键
    pub fn contains(&self, key: &[MessageFingerprint]) -> bool {
        self.lookup(key).is_some()
    }

    /// 查找 key 在树中的最长公共前缀长度
    ///
    /// 返回树中存储的键与 `key` 共享的最长前缀的长度。
    /// 用于确定多少条消息已经发送过（不需要重发）。
    ///
    /// # 示例
    ///
    /// ```
    /// # use forge::radix_tree::{RadixTree, MessageFingerprint};
    /// let mut tree = RadixTree::new();
    /// let stored = vec![
    ///     MessageFingerprint::from_text("a"),
    ///     MessageFingerprint::from_text("b"),
    ///     MessageFingerprint::from_text("c"),
    /// ];
    /// tree.insert(&stored, 1);
    ///
    /// let query = vec![
    ///     MessageFingerprint::from_text("a"),
    ///     MessageFingerprint::from_text("b"),
    ///     MessageFingerprint::from_text("d"),  // 第3个不同
    /// ];
    /// // 最长公共前缀 = 2 (a, b)
    /// assert_eq!(tree.longest_prefix_len(&query), 2);
    /// ```
    pub fn longest_prefix_len(&self, key: &[MessageFingerprint]) -> usize {
        Self::longest_prefix_recursive(&self.root, key, 0)
    }

    fn longest_prefix_recursive(
        node: &RadixNode,
        key: &[MessageFingerprint],
        accumulated: usize,
    ) -> usize {
        // 路径匹配模式：任何累积的路径匹配都算作有效前缀
        // 用于确定有多少消息已发送过（不需要重发）
        let mut best = accumulated;

        if key.is_empty() {
            return best;
        }

        let first = key[0];
        if let Some(child) = node.children.get(&first) {
            let cp_len = RadixNode::common_prefix_len(&child.segment, key);
            if cp_len > 0 {
                let new_accumulated = accumulated + cp_len;

                if cp_len == child.segment.len() {
                    // 完全匹配子节点段，继续递归查找更深匹配
                    let result =
                        Self::longest_prefix_recursive(child, &key[cp_len..], new_accumulated);
                    best = best.max(result);
                } else {
                    // 部分匹配子节点段 — 此路径的最大匹配到此为止
                    best = best.max(new_accumulated);
                }
            }
        }

        best
    }

    /// 删除键
    ///
    /// 返回被删除的值（如果键存在）。
    pub fn remove(&mut self, key: &[MessageFingerprint]) -> Option<u64> {
        let result = Self::remove_recursive(&mut self.root, key);
        if result.is_some() {
            self.size -= 1;
        }
        result
    }

    fn remove_recursive(node: &mut RadixNode, key: &[MessageFingerprint]) -> Option<u64> {
        if key.is_empty() {
            if node.is_terminal {
                node.is_terminal = false;
                return node.value.take();
            }
            return None;
        }

        let first = key[0];
        if let Some(child) = node.children.get_mut(&first) {
            let cp_len = RadixNode::common_prefix_len(&child.segment, key);
            if cp_len == child.segment.len() {
                let result = Self::remove_recursive(child, &key[cp_len..]);
                if result.is_some() {
                    // 清理：如果子节点变为空，移除它
                    if !child.is_terminal && child.children.is_empty() {
                        node.children.remove(&first);
                    }
                }
                return result;
            }
        }
        None
    }

    /// 清空树
    pub fn clear(&mut self) {
        self.root = RadixNode::root();
        self.size = 0;
    }
}

impl Default for RadixTree {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
//  纯函数 — 消息序列指纹计算
// ============================================================================

/// 将消息序列转换为指纹序列
///
/// 纯函数，不依赖外部状态。
///
/// # 示例
///
/// ```
/// # use forge::radix_tree::{compute_fingerprints, MessageFingerprint};
/// let msgs = vec!["hello", "world"];
/// let fps = compute_fingerprints(&msgs);
/// assert_eq!(fps.len(), 2);
/// assert_eq!(fps[0], MessageFingerprint::from_text("hello"));
/// ```
pub fn compute_fingerprints(messages: &[&str]) -> Vec<MessageFingerprint> {
    messages
        .iter()
        .map(|msg| MessageFingerprint::from_text(msg))
        .collect()
}

/// 将消息序列转换为指纹序列 (String 版本)
pub fn compute_fingerprints_owned(messages: &[String]) -> Vec<MessageFingerprint> {
    messages
        .iter()
        .map(|msg| MessageFingerprint::from_text(msg))
        .collect()
}

/// 计算两个序列的最长公共前缀长度 (纯函数)
///
/// # 示例
///
/// ```
/// # use forge::radix_tree::common_prefix_length;
/// let a = vec!["a", "b", "c", "d"];
/// let b = vec!["a", "b", "e", "f"];
/// assert_eq!(common_prefix_length(&a, &b), 2);
/// ```
pub fn common_prefix_length(a: &[&str], b: &[&str]) -> usize {
    let min_len = a.len().min(b.len());
    let mut i = 0;
    while i < min_len && a[i] == b[i] {
        i += 1;
    }
    i
}

// ============================================================================
//  ConversationTracker — 对话状态跟踪器
// ============================================================================

/// 对话状态跟踪器 — 使用 Radix Tree 跟踪已发送的对话上下文
///
/// 核心功能：
/// - `add_sent_context()` — 记录已发送的对话上下文
/// - `compute_delta()` — 计算需要发送的增量消息
/// - `mark_sent()` — 标记消息为已发送
///
/// # 设计
///
/// 当对话被压缩并新开后，新对话的上下文摘要可能与之前有公共前缀。
/// `compute_delta()` 利用 Radix Tree 的最长前缀查找，
/// 只返回需要发送的增量消息，避免重复发送已有上下文。
///
/// # 示例
///
/// ```
/// # use forge::radix_tree::ConversationTracker;
/// let mut tracker = ConversationTracker::new();
///
/// // 第一次发送完整上下文
/// let ctx1 = vec!["system_prompt".to_string(), "task_desc".to_string()];
/// let delta1 = tracker.compute_delta(&ctx1);
/// assert_eq!(delta1.len(), 2);  // 全部需要发送
/// tracker.mark_sent(&ctx1);
///
/// // 第二次有公共前缀
/// let ctx2 = vec![
///     "system_prompt".to_string(),
///     "task_desc".to_string(),
///     "new_message".to_string(),
/// ];
/// let delta2 = tracker.compute_delta(&ctx2);
/// assert_eq!(delta2.len(), 1);  // 只有 "new_message" 需要发送
/// assert_eq!(delta2[0], "new_message");
/// ```
#[derive(Debug, Clone)]
pub struct ConversationTracker {
    tree: RadixTree,
    /// 已存储的对话序列 (用于调试和统计)
    stored_sequences: usize,
    /// 通过复用前缀节省的消息数 (统计)
    saved_messages: usize,
}

impl ConversationTracker {
    /// 创建新的对话跟踪器
    pub fn new() -> Self {
        Self {
            tree: RadixTree::new(),
            stored_sequences: 0,
            saved_messages: 0,
        }
    }

    /// 计算需要发送的增量消息
    ///
    /// 查找 `messages` 在 Radix Tree 中的最长公共前缀，
    /// 返回前缀之后的消息 (即需要新发送的部分)。
    ///
    /// 如果树为空或没有公共前缀，返回全部消息。
    pub fn compute_delta(&self, messages: &[String]) -> Vec<String> {
        if messages.is_empty() {
            return vec![];
        }

        if self.tree.is_empty() {
            return messages.to_vec();
        }

        let fingerprints = compute_fingerprints_owned(messages);
        let prefix_len = self.tree.longest_prefix_len(&fingerprints);

        messages[prefix_len..].to_vec()
    }

    /// 标记消息序列为已发送 (存入 Radix Tree)
    pub fn mark_sent(&mut self, messages: &[String]) {
        if messages.is_empty() {
            return;
        }

        let fingerprints = compute_fingerprints_owned(messages);
        let prefix_len = if self.tree.is_empty() {
            0
        } else {
            self.tree.longest_prefix_len(&fingerprints)
        };

        self.saved_messages += prefix_len;
        self.tree
            .insert(&fingerprints, self.stored_sequences as u64);
        self.stored_sequences += 1;
    }

    /// 添加已发送的对话上下文 (mark_sent 的别名)
    pub fn add_sent_context(&mut self, messages: &[String]) {
        self.mark_sent(messages);
    }

    /// 已存储的对话序列数
    pub fn stored_count(&self) -> usize {
        self.stored_sequences
    }

    /// 通过复用前缀节省的消息总数
    pub fn saved_message_count(&self) -> usize {
        self.saved_messages
    }

    /// 清空跟踪状态
    pub fn clear(&mut self) {
        self.tree.clear();
        self.stored_sequences = 0;
        self.saved_messages = 0;
    }

    /// 检查消息序列是否已完全存储
    pub fn is_fully_sent(&self, messages: &[String]) -> bool {
        if messages.is_empty() {
            return true;
        }
        let fingerprints = compute_fingerprints_owned(messages);
        self.tree.contains(&fingerprints)
    }
}

impl Default for ConversationTracker {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
//  DeltaResult — 增量计算结果
// ============================================================================

/// 增量计算结果 — 包含增量消息和统计信息
#[derive(Debug, Clone)]
pub struct DeltaResult {
    /// 需要发送的增量消息
    pub delta_messages: Vec<String>,
    /// 公共前缀长度 (已发送的消息数)
    pub common_prefix_len: usize,
    /// 总消息数
    pub total_messages: usize,
    /// 是否全部为增量 (无公共前缀)
    pub is_full_send: bool,
}

impl DeltaResult {
    /// 节省的消息数 (total - delta)
    pub fn saved_count(&self) -> usize {
        self.total_messages - self.delta_messages.len()
    }

    /// 节省比例 (0.0 ~ 1.0)
    pub fn saved_ratio(&self) -> f64 {
        if self.total_messages == 0 {
            return 0.0;
        }
        self.saved_count() as f64 / self.total_messages as f64
    }
}

/// 计算增量并发送结果 (带统计)
///
/// 纯函数版本，不修改 tracker 状态。
pub fn compute_delta_with_stats(tracker: &ConversationTracker, messages: &[String]) -> DeltaResult {
    let delta = tracker.compute_delta(messages);
    let common_prefix_len = messages.len() - delta.len();
    let is_full_send = common_prefix_len == 0;

    DeltaResult {
        delta_messages: delta,
        common_prefix_len,
        total_messages: messages.len(),
        is_full_send,
    }
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ===== MessageFingerprint 测试 =====

    #[test]
    fn test_fingerprint_same_text() {
        let fp1 = MessageFingerprint::from_text("hello world");
        let fp2 = MessageFingerprint::from_text("hello world");
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn test_fingerprint_different_text() {
        let fp1 = MessageFingerprint::from_text("hello");
        let fp2 = MessageFingerprint::from_text("world");
        assert_ne!(fp1, fp2);
    }

    #[test]
    fn test_fingerprint_empty_text() {
        let fp = MessageFingerprint::from_text("");
        // FNV-1a of empty string = offset basis
        assert_eq!(fp.as_u64(), 0xcbf29ce484222325);
    }

    #[test]
    fn test_fingerprint_from_hash() {
        let fp = MessageFingerprint::from_hash(12345);
        assert_eq!(fp.as_u64(), 12345);
    }

    #[test]
    fn test_fingerprint_display() {
        let fp = MessageFingerprint::from_hash(0xdeadbeef);
        assert_eq!(format!("{}", fp), "00000000deadbeef");
    }

    #[test]
    fn test_fingerprint_ordering() {
        let fp1 = MessageFingerprint::from_hash(1);
        let fp2 = MessageFingerprint::from_hash(2);
        assert!(fp1 < fp2);
    }

    // ===== RadixTree 基本操作测试 =====

    #[test]
    fn test_tree_new_is_empty() {
        let tree = RadixTree::new();
        assert!(tree.is_empty());
        assert_eq!(tree.len(), 0);
    }

    #[test]
    fn test_tree_insert_and_lookup() {
        let mut tree = RadixTree::new();
        let key = vec![
            MessageFingerprint::from_text("a"),
            MessageFingerprint::from_text("b"),
        ];
        tree.insert(&key, 42);
        assert_eq!(tree.len(), 1);
        assert_eq!(tree.lookup(&key), Some(42));
    }

    #[test]
    fn test_tree_insert_multiple() {
        let mut tree = RadixTree::new();
        let key1 = vec![MessageFingerprint::from_text("a")];
        let key2 = vec![MessageFingerprint::from_text("b")];
        let key3 = vec![MessageFingerprint::from_text("c")];

        tree.insert(&key1, 1);
        tree.insert(&key2, 2);
        tree.insert(&key3, 3);

        assert_eq!(tree.len(), 3);
        assert_eq!(tree.lookup(&key1), Some(1));
        assert_eq!(tree.lookup(&key2), Some(2));
        assert_eq!(tree.lookup(&key3), Some(3));
    }

    #[test]
    fn test_tree_insert_overwrite() {
        let mut tree = RadixTree::new();
        let key = vec![MessageFingerprint::from_text("a")];
        assert_eq!(tree.insert(&key, 1), None);
        assert_eq!(tree.insert(&key, 2), Some(1)); // 返回旧值
        assert_eq!(tree.len(), 1); // 大小不变
        assert_eq!(tree.lookup(&key), Some(2));
    }

    #[test]
    fn test_tree_contains() {
        let mut tree = RadixTree::new();
        let key = vec![MessageFingerprint::from_text("hello")];
        tree.insert(&key, 1);
        assert!(tree.contains(&key));
        let missing = vec![MessageFingerprint::from_text("world")];
        assert!(!tree.contains(&missing));
    }

    #[test]
    fn test_tree_lookup_missing() {
        let tree = RadixTree::new();
        let key = vec![MessageFingerprint::from_text("a")];
        assert_eq!(tree.lookup(&key), None);
    }

    #[test]
    fn test_tree_insert_with_common_prefix() {
        let mut tree = RadixTree::new();
        let key1 = vec![
            MessageFingerprint::from_text("common"),
            MessageFingerprint::from_text("a"),
        ];
        let key2 = vec![
            MessageFingerprint::from_text("common"),
            MessageFingerprint::from_text("b"),
        ];

        tree.insert(&key1, 1);
        tree.insert(&key2, 2);

        assert_eq!(tree.len(), 2);
        assert_eq!(tree.lookup(&key1), Some(1));
        assert_eq!(tree.lookup(&key2), Some(2));
    }

    #[test]
    fn test_tree_insert_prefix_key() {
        let mut tree = RadixTree::new();
        let short = vec![MessageFingerprint::from_text("a")];
        let long = vec![
            MessageFingerprint::from_text("a"),
            MessageFingerprint::from_text("b"),
        ];

        tree.insert(&short, 1);
        tree.insert(&long, 2);

        assert_eq!(tree.lookup(&short), Some(1));
        assert_eq!(tree.lookup(&long), Some(2));
        assert_eq!(tree.len(), 2);
    }

    // ===== longest_prefix_len 测试 =====

    #[test]
    fn test_longest_prefix_empty_tree() {
        let tree = RadixTree::new();
        let key = vec![MessageFingerprint::from_text("a")];
        assert_eq!(tree.longest_prefix_len(&key), 0);
    }

    #[test]
    fn test_longest_prefix_exact_match() {
        let mut tree = RadixTree::new();
        let key = vec![
            MessageFingerprint::from_text("a"),
            MessageFingerprint::from_text("b"),
            MessageFingerprint::from_text("c"),
        ];
        tree.insert(&key, 1);
        assert_eq!(tree.longest_prefix_len(&key), 3);
    }

    #[test]
    fn test_longest_prefix_partial_match() {
        let mut tree = RadixTree::new();
        let stored = vec![
            MessageFingerprint::from_text("a"),
            MessageFingerprint::from_text("b"),
            MessageFingerprint::from_text("c"),
        ];
        tree.insert(&stored, 1);

        let query = vec![
            MessageFingerprint::from_text("a"),
            MessageFingerprint::from_text("b"),
            MessageFingerprint::from_text("d"),
        ];
        assert_eq!(tree.longest_prefix_len(&query), 2);
    }

    #[test]
    fn test_longest_prefix_no_match() {
        let mut tree = RadixTree::new();
        let stored = vec![MessageFingerprint::from_text("a")];
        tree.insert(&stored, 1);

        let query = vec![MessageFingerprint::from_text("b")];
        assert_eq!(tree.longest_prefix_len(&query), 0);
    }

    #[test]
    fn test_longest_prefix_query_is_prefix_of_stored() {
        let mut tree = RadixTree::new();
        let stored = vec![
            MessageFingerprint::from_text("a"),
            MessageFingerprint::from_text("b"),
            MessageFingerprint::from_text("c"),
        ];
        tree.insert(&stored, 1);

        let query = vec![MessageFingerprint::from_text("a")];
        // query is a prefix of stored — path match finds 1 (the "a" segment matches)
        // 路径匹配模式："a" 匹配了已存储路径的第一段
        assert_eq!(tree.longest_prefix_len(&query), 1);
    }

    #[test]
    fn test_longest_prefix_stored_is_prefix_of_query() {
        let mut tree = RadixTree::new();
        let stored = vec![
            MessageFingerprint::from_text("a"),
            MessageFingerprint::from_text("b"),
        ];
        tree.insert(&stored, 1);

        let query = vec![
            MessageFingerprint::from_text("a"),
            MessageFingerprint::from_text("b"),
            MessageFingerprint::from_text("c"),
        ];
        // stored is a prefix of query
        assert_eq!(tree.longest_prefix_len(&query), 2);
    }

    #[test]
    fn test_longest_prefix_multiple_stored() {
        let mut tree = RadixTree::new();
        let s1 = vec![
            MessageFingerprint::from_text("a"),
            MessageFingerprint::from_text("b"),
            MessageFingerprint::from_text("c"),
        ];
        let s2 = vec![
            MessageFingerprint::from_text("a"),
            MessageFingerprint::from_text("b"),
            MessageFingerprint::from_text("d"),
            MessageFingerprint::from_text("e"),
        ];
        tree.insert(&s1, 1);
        tree.insert(&s2, 2);

        let query = vec![
            MessageFingerprint::from_text("a"),
            MessageFingerprint::from_text("b"),
            MessageFingerprint::from_text("d"),
            MessageFingerprint::from_text("f"),
        ];
        // s2 shares prefix of length 3 (a, b, d) with query
        assert_eq!(tree.longest_prefix_len(&query), 3);
    }

    // ===== remove 测试 =====

    #[test]
    fn test_remove_existing() {
        let mut tree = RadixTree::new();
        let key = vec![MessageFingerprint::from_text("a")];
        tree.insert(&key, 42);
        assert_eq!(tree.remove(&key), Some(42));
        assert_eq!(tree.len(), 0);
        assert!(!tree.contains(&key));
    }

    #[test]
    fn test_remove_non_existing() {
        let mut tree = RadixTree::new();
        let key = vec![MessageFingerprint::from_text("a")];
        assert_eq!(tree.remove(&key), None);
    }

    #[test]
    fn test_remove_one_from_multiple() {
        let mut tree = RadixTree::new();
        let key1 = vec![MessageFingerprint::from_text("a")];
        let key2 = vec![MessageFingerprint::from_text("b")];
        tree.insert(&key1, 1);
        tree.insert(&key2, 2);

        tree.remove(&key1);
        assert!(!tree.contains(&key1));
        assert!(tree.contains(&key2));
        assert_eq!(tree.len(), 1);
    }

    // ===== clear 测试 =====

    #[test]
    fn test_clear() {
        let mut tree = RadixTree::new();
        let key = vec![MessageFingerprint::from_text("a")];
        tree.insert(&key, 1);
        tree.clear();
        assert!(tree.is_empty());
        assert_eq!(tree.len(), 0);
    }

    // ===== 纯函数测试 =====

    #[test]
    fn test_compute_fingerprints() {
        let msgs = vec!["hello", "world"];
        let fps = compute_fingerprints(&msgs);
        assert_eq!(fps.len(), 2);
        assert_eq!(fps[0], MessageFingerprint::from_text("hello"));
        assert_eq!(fps[1], MessageFingerprint::from_text("world"));
    }

    #[test]
    fn test_compute_fingerprints_empty() {
        let fps = compute_fingerprints(&[]);
        assert!(fps.is_empty());
    }

    #[test]
    fn test_common_prefix_length_full() {
        let a = vec!["a", "b", "c"];
        let b = vec!["a", "b", "c"];
        assert_eq!(common_prefix_length(&a, &b), 3);
    }

    #[test]
    fn test_common_prefix_length_partial() {
        let a = vec!["a", "b", "c"];
        let b = vec!["a", "b", "d"];
        assert_eq!(common_prefix_length(&a, &b), 2);
    }

    #[test]
    fn test_common_prefix_length_none() {
        let a = vec!["a"];
        let b = vec!["b"];
        assert_eq!(common_prefix_length(&a, &b), 0);
    }

    #[test]
    fn test_common_prefix_length_empty() {
        assert_eq!(common_prefix_length(&[], &[]), 0);
        assert_eq!(common_prefix_length(&[], &["a"]), 0);
    }

    // ===== ConversationTracker 测试 =====

    #[test]
    fn test_tracker_new() {
        let tracker = ConversationTracker::new();
        assert_eq!(tracker.stored_count(), 0);
        assert_eq!(tracker.saved_message_count(), 0);
    }

    #[test]
    fn test_tracker_first_send_full_delta() {
        let tracker = ConversationTracker::new();
        let messages = vec!["msg1".to_string(), "msg2".to_string(), "msg3".to_string()];
        let delta = tracker.compute_delta(&messages);
        // 第一次发送，无公共前缀，全部需要发送
        assert_eq!(delta.len(), 3);
    }

    #[test]
    fn test_tracker_second_send_with_common_prefix() {
        let mut tracker = ConversationTracker::new();

        let ctx1 = vec![
            "system".to_string(),
            "task".to_string(),
            "context".to_string(),
        ];
        tracker.mark_sent(&ctx1);

        let ctx2 = vec![
            "system".to_string(),
            "task".to_string(),
            "context".to_string(),
            "new_msg".to_string(),
        ];
        let delta = tracker.compute_delta(&ctx2);
        // 公共前缀 = 3，只需要发送 "new_msg"
        assert_eq!(delta.len(), 1);
        assert_eq!(delta[0], "new_msg");
    }

    #[test]
    fn test_tracker_no_common_prefix() {
        let mut tracker = ConversationTracker::new();

        let ctx1 = vec!["aaa".to_string(), "bbb".to_string()];
        tracker.mark_sent(&ctx1);

        let ctx2 = vec!["ccc".to_string(), "ddd".to_string()];
        let delta = tracker.compute_delta(&ctx2);
        // 无公共前缀，全部需要发送
        assert_eq!(delta.len(), 2);
    }

    #[test]
    fn test_tracker_empty_messages() {
        let tracker = ConversationTracker::new();
        let delta = tracker.compute_delta(&[]);
        assert!(delta.is_empty());
    }

    #[test]
    fn test_tracker_saved_messages_count() {
        let mut tracker = ConversationTracker::new();

        let ctx1 = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        tracker.mark_sent(&ctx1);

        let ctx2 = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        tracker.mark_sent(&ctx2);

        // 第二次发送节省了 3 条消息 (公共前缀)
        assert_eq!(tracker.saved_message_count(), 3);
    }

    #[test]
    fn test_tracker_is_fully_sent() {
        let mut tracker = ConversationTracker::new();
        let messages = vec!["msg1".to_string(), "msg2".to_string()];
        assert!(!tracker.is_fully_sent(&messages));
        tracker.mark_sent(&messages);
        assert!(tracker.is_fully_sent(&messages));
    }

    #[test]
    fn test_tracker_clear() {
        let mut tracker = ConversationTracker::new();
        tracker.mark_sent(&["msg1".to_string()]);
        tracker.clear();
        assert_eq!(tracker.stored_count(), 0);
        assert_eq!(tracker.saved_message_count(), 0);
    }

    #[test]
    fn test_tracker_multiple_contexts() {
        let mut tracker = ConversationTracker::new();

        // 第一次：系统提示 + 任务1
        let ctx1 = vec!["system".to_string(), "task1".to_string()];
        tracker.mark_sent(&ctx1);

        // 第二次：系统提示 + 任务1 + 任务1结果 + 任务2
        let ctx2 = vec![
            "system".to_string(),
            "task1".to_string(),
            "task1_result".to_string(),
            "task2".to_string(),
        ];
        let delta2 = tracker.compute_delta(&ctx2);
        assert_eq!(delta2.len(), 2); // task1_result + task2
        tracker.mark_sent(&ctx2);

        // 第三次：系统提示 + 任务1 + 任务1结果 + 任务2 + 任务2结果 + 任务3
        let ctx3 = vec![
            "system".to_string(),
            "task1".to_string(),
            "task1_result".to_string(),
            "task2".to_string(),
            "task2_result".to_string(),
            "task3".to_string(),
        ];
        let delta3 = tracker.compute_delta(&ctx3);
        assert_eq!(delta3.len(), 2); // task2_result + task3
    }

    // ===== DeltaResult 测试 =====

    #[test]
    fn test_delta_result_saved_count() {
        let result = DeltaResult {
            delta_messages: vec!["new".to_string()],
            common_prefix_len: 3,
            total_messages: 4,
            is_full_send: false,
        };
        assert_eq!(result.saved_count(), 3);
    }

    #[test]
    fn test_delta_result_saved_ratio() {
        let result = DeltaResult {
            delta_messages: vec!["new".to_string()],
            common_prefix_len: 3,
            total_messages: 4,
            is_full_send: false,
        };
        assert_eq!(result.saved_ratio(), 0.75);
    }

    #[test]
    fn test_delta_result_full_send() {
        let result = DeltaResult {
            delta_messages: vec!["a".to_string(), "b".to_string()],
            common_prefix_len: 0,
            total_messages: 2,
            is_full_send: true,
        };
        assert_eq!(result.saved_count(), 0);
        assert_eq!(result.saved_ratio(), 0.0);
    }

    #[test]
    fn test_compute_delta_with_stats() {
        let mut tracker = ConversationTracker::new();
        let ctx1 = vec!["system".to_string(), "task".to_string()];
        tracker.mark_sent(&ctx1);

        let ctx2 = vec!["system".to_string(), "task".to_string(), "new".to_string()];
        let result = compute_delta_with_stats(&tracker, &ctx2);
        assert_eq!(result.common_prefix_len, 2);
        assert_eq!(result.total_messages, 3);
        assert!(!result.is_full_send);
        assert_eq!(result.delta_messages.len(), 1);
    }

    // ===== 边界测试 =====

    #[test]
    fn test_tree_insert_empty_key() {
        let mut tree = RadixTree::new();
        let key: Vec<MessageFingerprint> = vec![];
        tree.insert(&key, 42);
        assert_eq!(tree.len(), 1);
        assert_eq!(tree.lookup(&key), Some(42));
    }

    #[test]
    fn test_tree_insert_single_element() {
        let mut tree = RadixTree::new();
        let key = vec![MessageFingerprint::from_text("only")];
        tree.insert(&key, 1);
        assert_eq!(tree.lookup(&key), Some(1));
        assert_eq!(tree.longest_prefix_len(&key), 1);
    }

    #[test]
    fn test_tree_clone() {
        let mut tree = RadixTree::new();
        let key = vec![MessageFingerprint::from_text("a")];
        tree.insert(&key, 1);
        let cloned = tree.clone();
        assert_eq!(cloned.len(), 1);
        assert_eq!(cloned.lookup(&key), Some(1));
    }

    #[test]
    fn test_tracker_large_sequence() {
        let mut tracker = ConversationTracker::new();

        // 模拟长对话序列
        let ctx1: Vec<String> = (0..100).map(|i| format!("msg_{}", i)).collect();
        tracker.mark_sent(&ctx1);

        // 第二次只多一条消息
        let mut ctx2 = ctx1.clone();
        ctx2.push("msg_100".to_string());
        let delta = tracker.compute_delta(&ctx2);
        assert_eq!(delta.len(), 1);
        assert_eq!(delta[0], "msg_100");
    }

    #[test]
    fn test_tracker_diverged_sequences() {
        let mut tracker = ConversationTracker::new();

        let ctx1 = vec![
            "system".to_string(),
            "task_a".to_string(),
            "result_a".to_string(),
        ];
        tracker.mark_sent(&ctx1);

        // 分叉：从第二条消息开始不同
        let ctx2 = vec![
            "system".to_string(),
            "task_b".to_string(),
            "result_b".to_string(),
        ];
        let delta = tracker.compute_delta(&ctx2);
        // 只有 "system" 是公共前缀
        assert_eq!(delta.len(), 2);
    }
}
