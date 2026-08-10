//! 取消令牌 — 借鉴 ds4 `web_sleep_ms` 的取消检查模式
//!
//! 为 Forge 长操作提供取消感知能力，支持：
//! - 全局超时取消
//! - 手动取消信号
//! - 层级取消传播
//!
//! ## 设计
//!
//! - [`CancellationToken`][]: 可克隆的取消令牌，用于检查是否应取消操作
//! - [`CancellationTokenSource`][]: 取消令牌源，用于触发取消
//! - 支持与现有 `Deadline` 集成，自动在超时时取消
//!
//! ## 示例
//!
//! ```no_run
//! use forge::cancellation_token::{CancellationTokenSource, CancellationToken};
//! use std::time::Duration;
//!
//! async fn long_running_operation(token: CancellationToken) -> Result<String, anyhow::Error> {
//!     for _ in 0..100 {
//!         // 定期检查取消
//!         if token.is_cancelled() {
//!             return Err(anyhow::anyhow!("操作被取消"));
//!         }
//!         
//!         // 执行一小部分工作，可被取消中断
//!         token.sleep(Duration::from_millis(100)).await
//!             .map_err(|_| anyhow::anyhow!("操作被取消或超时"))?;
//!     }
//!     Ok("完成".to_string())
//! }
//!
//! #[tokio::main]
//! async fn main() {
//!     let source = CancellationTokenSource::with_timeout(Duration::from_secs(5));
//!     let token = source.token();
//!     
//!     let result = long_running_operation(token).await;
//!     match result {
//!         Ok(value) => println!("成功: {}", value),
//!         Err(e) => println!("失败: {}", e),
//!     }
//! }
//! ```

use crate::deadline::Deadline;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// 取消令牌 — 指示操作是否应被取消
///
/// 可安全在线程间共享和克隆。通过 [`CancellationTokenSource`] 创建。
#[derive(Debug, Clone)]
pub struct CancellationToken {
    inner: Arc<CancellationTokenInner>,
}

/// 内部取消状态
#[derive(Debug)]
struct CancellationTokenInner {
    cancelled: AtomicBool,
    deadline: Option<Deadline>,
}

impl CancellationToken {
    /// 检查是否已取消
    pub fn is_cancelled(&self) -> bool {
        if self.inner.cancelled.load(Ordering::Relaxed) {
            return true;
        }

        // 检查 deadline
        if let Some(deadline) = &self.inner.deadline {
            if deadline.expired() {
                return true;
            }
        }

        false
    }

    /// 阻塞直到取消或超时
    ///
    /// 返回 `Ok(())` 如果手动取消，`Err(Duration)` 如果超时而取消
    pub async fn cancelled(&self) -> Result<(), Duration> {
        loop {
            if self.inner.cancelled.load(Ordering::Relaxed) {
                return Ok(());
            }

            if let Some(deadline) = &self.inner.deadline {
                if deadline.expired() {
                    return Err(deadline.overrun());
                }

                // 等待一小段时间后再次检查
                let remaining = deadline.remaining().min(Duration::from_millis(100));
                tokio::time::sleep(remaining).await;
            } else {
                // 没有 deadline，只检查手动取消
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }

    /// 获取剩余超时时间，用于其他异步操作
    ///
    /// 如果有 deadline，返回剩余时间（最小1ms）
    /// 如果没有 deadline，返回 None
    pub fn remaining_timeout(&self) -> Option<Duration> {
        if let Some(deadline) = &self.inner.deadline {
            let remaining = deadline.remaining();
            if remaining.is_zero() {
                Some(Duration::from_millis(1))
            } else {
                Some(remaining)
            }
        } else {
            None
        }
    }

    /// 等待指定的持续时间，但可被取消中断
    pub async fn sleep(&self, duration: Duration) -> Result<(), CancelError> {
        if self.is_cancelled() {
            return Err(CancelError::Cancelled);
        }

        match self.remaining_timeout() {
            Some(_remaining) => {
                // 如果有全局超时，监控取消而不是取最小值
                tokio::select! {
                    _ = tokio::time::sleep(duration) => Ok(()),
                    result = self.cancelled() => {
                        match result {
                            Ok(()) => Err(CancelError::Cancelled),
                            Err(overrun) => Err(CancelError::Timeout(overrun)),
                        }
                    }
                }
            }
            None => {
                // 没有全局超时，使用 select 检查取消
                tokio::select! {
                    _ = tokio::time::sleep(duration) => Ok(()),
                    _ = self.cancelled() => Err(CancelError::Cancelled),
                }
            }
        }
    }
}

/// 取消令牌源 — 用于创建和触发取消
#[derive(Debug)]
pub struct CancellationTokenSource {
    token: CancellationToken,
}

impl Default for CancellationTokenSource {
    fn default() -> Self {
        Self::new()
    }
}

impl CancellationTokenSource {
    /// 创建新的取消令牌源
    pub fn new() -> Self {
        Self::with_deadline(None)
    }

    /// 创建带有超时的取消令牌源
    pub fn with_timeout(timeout: Duration) -> Self {
        Self::with_deadline(Some(Deadline::from_duration(timeout)))
    }

    /// 创建带有截止时间的取消令牌源
    pub fn with_deadline(deadline: Option<Deadline>) -> Self {
        let inner = CancellationTokenInner {
            cancelled: AtomicBool::new(false),
            deadline,
        };

        Self {
            token: CancellationToken {
                inner: Arc::new(inner),
            },
        }
    }

    /// 获取取消令牌
    pub fn token(&self) -> CancellationToken {
        self.token.clone()
    }

    /// 触发取消
    pub fn cancel(&mut self) {
        self.token.inner.cancelled.store(true, Ordering::Relaxed);
    }

    /// 指定时间后自动取消
    pub fn cancel_after(&mut self, delay: Duration) {
        let token = self.token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            token.inner.cancelled.store(true, Ordering::Relaxed);
        });
    }

    /// 检查是否已取消
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }
}

/// 取消错误类型
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelError {
    /// 手动取消
    Cancelled,
    /// 超时取消，包含超时时间
    Timeout(Duration),
}

impl std::fmt::Display for CancelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CancelError::Cancelled => write!(f, "操作被取消"),
            CancelError::Timeout(duration) => write!(f, "操作超时 ({}ms)", duration.as_millis()),
        }
    }
}

impl std::error::Error for CancelError {}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::timeout;

    #[test]
    fn test_token_not_cancelled_initially() {
        let source = CancellationTokenSource::new();
        let token = source.token();
        assert!(!token.is_cancelled());
    }

    #[test]
    fn test_token_cancelled_after_cancel() {
        let mut source = CancellationTokenSource::new();
        let token = source.token();

        assert!(!token.is_cancelled());
        source.cancel();
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn test_token_cancelled_after_timeout() {
        let source = CancellationTokenSource::with_timeout(Duration::from_millis(50));
        let token = source.token();

        assert!(!token.is_cancelled());

        // 等待超时
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn test_sleep_can_be_cancelled() {
        let mut source = CancellationTokenSource::new();
        let token = source.token();

        // 启动一个长时间 sleep
        let sleep_future = token.sleep(Duration::from_secs(10));

        // 立即取消
        source.cancel();

        let result = timeout(Duration::from_millis(100), sleep_future).await;
        assert!(result.is_ok()); // sleep_future 应该在 100ms 内完成
        assert_eq!(result.unwrap(), Err(CancelError::Cancelled));
    }

    #[tokio::test]
    async fn test_sleep_respects_timeout() {
        let source = CancellationTokenSource::with_timeout(Duration::from_millis(50));
        let token = source.token();

        // 启动一个长时间 sleep
        let sleep_future = token.sleep(Duration::from_secs(10));

        let result = timeout(Duration::from_millis(100), sleep_future).await;
        assert!(result.is_ok()); // sleep_future 应该在 100ms 内完成

        match result.unwrap() {
            Err(CancelError::Timeout(_)) => { /* 预期超时 */ }
            other => panic!("预期超时错误，得到: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_cancelled_awaits_manual_cancel() {
        let mut source = CancellationTokenSource::new();
        let token = source.token();

        let cancel_future = token.cancelled();

        // 取消应该在手动取消时完成
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!token.is_cancelled());

        source.cancel();
        let result = timeout(Duration::from_millis(100), cancel_future).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Ok(()));
    }

    #[tokio::test]
    async fn test_cancelled_awaits_timeout() {
        let source = CancellationTokenSource::with_timeout(Duration::from_millis(50));
        let token = source.token();

        let cancel_future = token.cancelled();

        let result = timeout(Duration::from_millis(100), cancel_future).await;
        assert!(result.is_ok());

        match result.unwrap() {
            Err(overrun) => {
                assert!(overrun.as_millis() > 0);
            }
            Ok(()) => panic!("预期超时错误"),
        }
    }
}
