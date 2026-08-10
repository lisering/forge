//! 全局 Deadline 超时传播 — 借鉴 us/crw `deadline.rs` 设计
//!
//! 在请求管线的每一层（CDP 命令、聊天等待、Orchestrator 轮次等）中，
//! 各自独立设置 timeout 无法保证全局超时控制。一个 `Deadline` 从请求
//! 入口构建，贯穿每一层，每层 clamp 自己的 timeout 到 `remaining()`，
//! 确保绝对返回时间有界。
//!
//! ## 设计
//!
//! - [`Deadline`][]: 绝对截止时间，Copy 语义，线程安全
//! - 从 `Duration` 或毫秒数构建
//! - 每层通过 `remaining()` 获取剩余时间，`expired()` 检查是否已过期
//!
//! ## 示例
//!
//! ```no_run
//! use forge::deadline::Deadline;
//! use std::time::Duration;
//!
//! // 从 30 秒构建
//! let deadline = Deadline::from_millis(30_000);
//! assert!(!deadline.expired());
//!
//! // 各层 clamp 自己的 timeout
//! let layer_timeout = deadline.remaining().min(Duration::from_secs(10));
//! // tokio::time::timeout(layer_timeout, some_async_op()).await?;
//! ```

use std::time::{Duration, Instant};

/// 绝对截止时间 — 贯穿整个请求管线
///
/// Cheap to copy. 使用 [`Self::remaining`] 计算剩余时间;
/// 永远不要调度比该值更长的等待。
#[derive(Debug, Clone, Copy)]
pub struct Deadline {
    absolute: Instant,
}

impl Deadline {
    /// 从毫秒数构建截止时间 (`ms` 毫秒后过期)
    ///
    /// `ms = 0` 产生立即过期的 deadline (用于测试)
    pub fn from_millis(ms: u64) -> Self {
        Self {
            absolute: Instant::now() + Duration::from_millis(ms),
        }
    }

    /// 从 `Duration` 构建截止时间
    pub fn from_duration(d: Duration) -> Self {
        Self {
            absolute: Instant::now() + d,
        }
    }

    /// 从一个 `Instant` 绝对时间点构建
    pub fn from_instant(instant: Instant) -> Self {
        Self { absolute: instant }
    }

    /// 剩余时间。如果已过期返回 `Duration::ZERO`
    pub fn remaining(&self) -> Duration {
        self.absolute.saturating_duration_since(Instant::now())
    }

    /// 是否已过期
    pub fn expired(&self) -> bool {
        Instant::now() >= self.absolute
    }

    /// 超过截止时间多久了。如果未过期返回 `Duration::ZERO`
    ///
    /// 用于生成有意义的超时错误消息 (而非报告 0ms)
    pub fn overrun(&self) -> Duration {
        Instant::now().saturating_duration_since(self.absolute)
    }

    /// 绝对截止 Instant
    pub fn absolute(&self) -> Instant {
        self.absolute
    }

    /// Clamp 一个 timeout 到剩余时间内
    ///
    /// 如果 timeout > remaining, 返回 remaining;
    /// 如果已过期, 返回 Duration::ZERO
    pub fn clamp_timeout(&self, timeout: Duration) -> Duration {
        let remaining = self.remaining();
        if timeout < remaining {
            timeout
        } else {
            remaining
        }
    }

    /// 从截止时间中扣除已用时间, 生成子 deadline
    ///
    /// 子 deadline 的截止时间不会超过父 deadline
    pub fn sub_deadline(&self, additional: Duration) -> Self {
        let now = Instant::now();
        let sub_absolute = now + additional;
        Self {
            absolute: if sub_absolute < self.absolute {
                sub_absolute
            } else {
                self.absolute
            },
        }
    }
}

impl PartialEq for Deadline {
    fn eq(&self, other: &Self) -> bool {
        self.absolute == other.absolute
    }
}

impl Eq for Deadline {}

impl PartialOrd for Deadline {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Deadline {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.absolute.cmp(&other.absolute)
    }
}

/// 无限截止时间 — 永不过期 (用于不需要超时控制的场景)
pub fn no_deadline() -> Deadline {
    Deadline::from_millis(u64::MAX / 2)
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fresh_deadline_has_remaining() {
        let d = Deadline::from_millis(1000);
        assert!(d.remaining() > Duration::from_millis(900));
        assert!(!d.expired());
    }

    #[test]
    fn test_zero_ms_is_expired() {
        let d = Deadline::from_millis(0);
        assert!(d.expired());
        assert_eq!(d.remaining(), Duration::ZERO);
    }

    #[test]
    fn test_from_duration() {
        let d = Deadline::from_duration(Duration::from_millis(500));
        assert!(d.remaining() > Duration::from_millis(400));
        assert!(!d.expired());
    }

    #[test]
    fn test_remaining_decreases() {
        let d = Deadline::from_millis(100);
        let r1 = d.remaining();
        std::thread::sleep(Duration::from_millis(20));
        let r2 = d.remaining();
        assert!(r2 < r1);
    }

    #[test]
    fn test_expired_after_timeout() {
        let d = Deadline::from_millis(10);
        std::thread::sleep(Duration::from_millis(20));
        assert!(d.expired());
        assert_eq!(d.remaining(), Duration::ZERO);
    }

    #[test]
    fn test_overrun_not_expired() {
        let d = Deadline::from_millis(1000);
        assert_eq!(d.overrun(), Duration::ZERO);
    }

    #[test]
    fn test_overrun_expired() {
        let d = Deadline::from_millis(10);
        std::thread::sleep(Duration::from_millis(30));
        assert!(d.overrun() > Duration::ZERO);
    }

    #[test]
    fn test_clamp_smaller_than_remaining() {
        let d = Deadline::from_millis(1000);
        let clamped = d.clamp_timeout(Duration::from_millis(100));
        assert_eq!(clamped, Duration::from_millis(100));
    }

    #[test]
    fn test_clamp_larger_than_remaining() {
        let d = Deadline::from_millis(100);
        let clamped = d.clamp_timeout(Duration::from_secs(10));
        assert!(clamped <= Duration::from_millis(100));
    }

    #[test]
    fn test_clamp_expired() {
        let d = Deadline::from_millis(0);
        let clamped = d.clamp_timeout(Duration::from_secs(10));
        assert_eq!(clamped, Duration::ZERO);
    }

    #[test]
    fn test_sub_deadline_within_parent() {
        let parent = Deadline::from_millis(1000);
        let child = parent.sub_deadline(Duration::from_millis(500));
        assert!(child.remaining() <= Duration::from_millis(500));
        assert!(child.absolute() <= parent.absolute());
    }

    #[test]
    fn test_sub_deadline_exceeds_parent() {
        let parent = Deadline::from_millis(100);
        let child = parent.sub_deadline(Duration::from_secs(10));
        // 子 deadline 不应超过父 deadline
        assert!(child.absolute() <= parent.absolute());
    }

    #[test]
    fn test_no_deadline_not_expired() {
        let d = no_deadline();
        assert!(!d.expired());
        assert!(d.remaining() > Duration::from_secs(3600));
    }

    #[test]
    fn test_deadline_ordering() {
        let d1 = Deadline::from_millis(100);
        let d2 = Deadline::from_millis(200);
        // d1 先过期, 所以 d1 < d2
        assert!(d1 < d2);
    }

    #[test]
    fn test_deadline_equality() {
        let now = Instant::now();
        let d1 = Deadline::from_instant(now);
        let d2 = Deadline::from_instant(now);
        assert_eq!(d1, d2);
    }

    #[test]
    fn test_absolute_returns_correct_instant() {
        let now = Instant::now();
        let d = Deadline::from_instant(now + Duration::from_secs(5));
        assert!(d.absolute() > now);
    }
}
