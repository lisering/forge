//! Watchdog 分治架构 — 借鉴 browser-use 的 watchdog 系统
//!
//! browser-use 有 15 个独立 watchdog, 每个 watchdog 监听一类异常并通过
//! EventBus 解耦。Forge 当前是集中式检测 (connection_monitor.rs, auto_recovery.rs,
//! site_health.rs 各自独立检测), 借鉴此模式实现分治式 watchdog。
//!
//! ## 设计
//!
//! - [`Watchdog`][] trait: 每个 watchdog 实现此 trait, 声明监听的事件和发出的事件
//! - [`WatchEvent`][]: 事件类型枚举, 通过 channel 传递
//! - [`WatchdogRegistry`][]: 注册中心, 管理所有 watchdog 的生命周期
//!
//! ## 与现有机制的关系
//!
//! - `ConnectionMonitor` → `ChromeWatchdog` (Chrome 崩溃检测)
//! - `AutoRecovery` → `RecoveryWatchdog` (恢复策略)
//! - `SiteHealthChecker` → `SiteHealthWatchdog` (网站健康)
//! - 新增: `CaptchaWatchdog` (验证码检测), `PopupWatchdog` (弹窗处理)
//!
//! ## 示例
//!
//! ```no_run
//! use forge::watchdog::{WatchdogRegistry, ChromeWatchdog};
//!
//! let mut registry = WatchdogRegistry::new();
//! registry.register("chrome", Box::new(ChromeWatchdog::new()));
//! registry.start_all();
//! ```

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

// ============================================================================
//  WatchEvent — 事件类型
// ============================================================================

/// Watchdog 事件类型 — watchdog 监听和发出的事件
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    /// Chrome 进程崩溃
    ChromeCrashed,
    /// Chrome 调试端口不可达
    ChromeUnreachable,
    /// WebSocket 连接断开
    WebSocketDisconnected(String),
    /// 聊天标签页已关闭
    TabClosed,
    /// 网站健康检查失败
    SiteUnhealthy(String),
    /// 检测到验证码
    CaptchaDetected,
    /// 检测到弹窗
    PopupDetected(String),
    /// 检测到 DOM 变化
    DomChanged,
    /// AI 回复超时
    ResponseTimeout,
    /// 检测到循环
    LoopDetected,
    /// 恢复操作开始
    RecoveryStarted,
    /// 恢复操作完成
    RecoveryCompleted(bool),
    /// 自定义事件
    Custom(String),
}

impl WatchEvent {
    /// 事件的严重级别 (1=info, 2=warning, 3=critical)
    pub fn severity(&self) -> u8 {
        match self {
            WatchEvent::ChromeCrashed => 3,
            WatchEvent::ChromeUnreachable => 3,
            WatchEvent::WebSocketDisconnected(_) => 3,
            WatchEvent::TabClosed => 2,
            WatchEvent::SiteUnhealthy(_) => 2,
            WatchEvent::CaptchaDetected => 2,
            WatchEvent::PopupDetected(_) => 1,
            WatchEvent::DomChanged => 1,
            WatchEvent::ResponseTimeout => 2,
            WatchEvent::LoopDetected => 2,
            WatchEvent::RecoveryStarted => 1,
            WatchEvent::RecoveryCompleted(_) => 1,
            WatchEvent::Custom(_) => 1,
        }
    }

    /// 事件名称 (用于日志和调试)
    pub fn name(&self) -> &str {
        match self {
            WatchEvent::ChromeCrashed => "chrome_crashed",
            WatchEvent::ChromeUnreachable => "chrome_unreachable",
            WatchEvent::WebSocketDisconnected(_) => "websocket_disconnected",
            WatchEvent::TabClosed => "tab_closed",
            WatchEvent::SiteUnhealthy(_) => "site_unhealthy",
            WatchEvent::CaptchaDetected => "captcha_detected",
            WatchEvent::PopupDetected(_) => "popup_detected",
            WatchEvent::DomChanged => "dom_changed",
            WatchEvent::ResponseTimeout => "response_timeout",
            WatchEvent::LoopDetected => "loop_detected",
            WatchEvent::RecoveryStarted => "recovery_started",
            WatchEvent::RecoveryCompleted(_) => "recovery_completed",
            WatchEvent::Custom(name) => name.as_str(),
        }
    }

    /// 是否需要立即恢复
    pub fn needs_immediate_recovery(&self) -> bool {
        self.severity() == 3
    }
}

// ============================================================================
//  Watchdog trait — 分治式监控器
// ============================================================================

/// Watchdog trait — 借鉴 browser-use 的 BaseWatchdog 设计
///
/// 每个 watchdog 独立监听一类异常, 通过事件 channel 通信。
/// `LISTENS_TO` 声明监听的事件类型, `EMITS` 声明可能发出的事件类型。
#[async_trait]
pub trait Watchdog: Send + Sync {
    /// Watchdog 名称 (用于日志和注册)
    fn name(&self) -> &str;

    /// 此 watchdog 监听的事件类型 (用于事件分发)
    fn listens_to(&self) -> Vec<WatchEvent> {
        vec![]
    }

    /// 此 watchdog 可能发出的事件类型 (用于文档)
    fn emits(&self) -> Vec<WatchEvent> {
        vec![]
    }

    /// 检查一次状态, 返回检测到的事件 (如果没有异常返回 None)
    async fn check(&self) -> Option<WatchEvent>;

    /// 处理一个事件 (可选, 默认不做任何事)
    async fn handle_event(&self, _event: &WatchEvent) {}

    /// 检查间隔 (秒), 默认 30 秒
    fn check_interval_secs(&self) -> u64 {
        30
    }

    /// 是否启用 (用于配置开关)
    fn is_enabled(&self) -> bool {
        true
    }
}

// ============================================================================
//  WatchdogRegistry — 注册中心
// ============================================================================

/// Watchdog 注册中心 — 管理所有 watchdog 的注册、启动和事件分发
pub struct WatchdogRegistry {
    watchdogs: HashMap<String, Arc<dyn Watchdog>>,
    event_tx: mpsc::UnboundedSender<WatchEvent>,
    event_rx: Option<mpsc::UnboundedReceiver<WatchEvent>>,
}

impl WatchdogRegistry {
    /// 创建新的注册中心
    pub fn new() -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        Self {
            watchdogs: HashMap::new(),
            event_tx,
            event_rx: Some(event_rx),
        }
    }

    /// 注册一个 watchdog
    pub fn register(&mut self, name: &str, watchdog: Box<dyn Watchdog>) {
        let name = name.to_string();
        info!(
            "注册 Watchdog: {} (检查间隔: {}s)",
            name,
            watchdog.check_interval_secs()
        );
        self.watchdogs.insert(name, Arc::from(watchdog));
    }

    /// 获取事件发送端 (用于外部发送事件)
    pub fn event_sender(&self) -> mpsc::UnboundedSender<WatchEvent> {
        self.event_tx.clone()
    }

    /// 获取事件接收端 (用于消费事件)
    pub fn take_event_receiver(&mut self) -> Option<mpsc::UnboundedReceiver<WatchEvent>> {
        self.event_rx.take()
    }

    /// 启动所有 watchdog 的定期检查循环
    pub fn start_all(&self) {
        for (name, watchdog) in &self.watchdogs {
            if !watchdog.is_enabled() {
                debug!("Watchdog {} 已禁用, 跳过", name);
                continue;
            }

            let name_clone = name.clone();
            let watchdog_clone = Arc::clone(watchdog);
            let event_tx = self.event_tx.clone();

            tokio::spawn(async move {
                let interval_secs = watchdog_clone.check_interval_secs();
                let mut interval =
                    tokio::time::interval(std::time::Duration::from_secs(interval_secs));
                interval.tick().await; // skip first immediate tick

                loop {
                    interval.tick().await;
                    debug!("Watchdog {} 正在检查...", name_clone);

                    if let Some(event) = watchdog_clone.check().await {
                        warn!(
                            "Watchdog {} 检测到异常: {} (严重级别: {})",
                            name_clone,
                            event.name(),
                            event.severity()
                        );
                        if event_tx.send(event).is_err() {
                            debug!("Watchdog {}: 事件通道已关闭, 停止", name_clone);
                            break;
                        }
                    }
                }
            });
        }
    }

    /// 启动事件消费循环 — 分发事件给所有 watchdog 的 handle_event
    pub fn start_event_loop(&mut self) {
        let mut rx = match self.take_event_receiver() {
            Some(rx) => rx,
            None => {
                warn!("事件接收端已被取走, 无法启动事件循环");
                return;
            }
        };

        let watchdogs = self.watchdogs.clone();

        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                info!("处理事件: {} (级别: {})", event.name(), event.severity());

                for (name, watchdog) in &watchdogs {
                    let listens_to = watchdog.listens_to();
                    let should_handle = listens_to.is_empty()
                        || listens_to.iter().any(|e| e.name() == event.name())
                        || listens_to
                            .iter()
                            .any(|e| matches!(e, WatchEvent::Custom(_)));

                    if should_handle {
                        debug!("Watchdog {} 处理事件: {}", name, event.name());
                        watchdog.handle_event(&event).await;
                    }
                }
            }
            debug!("事件循环结束");
        });
    }

    /// 注册的 watchdog 数量
    pub fn len(&self) -> usize {
        self.watchdogs.len()
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.watchdogs.is_empty()
    }
}

impl Default for WatchdogRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
//  内置 Watchdog 实现
// ============================================================================

/// Chrome 崩溃/不可达 Watchdog — 借鉴 browser-use crash_watchdog
pub struct ChromeWatchdog {
    /// CDP 端口
    port: u16,
}

impl ChromeWatchdog {
    pub fn new(port: u16) -> Self {
        Self { port }
    }
}

#[async_trait]
impl Watchdog for ChromeWatchdog {
    fn name(&self) -> &str {
        "chrome_crash"
    }

    fn listens_to(&self) -> Vec<WatchEvent> {
        vec![
            WatchEvent::RecoveryStarted,
            WatchEvent::RecoveryCompleted(false),
        ]
    }

    fn emits(&self) -> Vec<WatchEvent> {
        vec![WatchEvent::ChromeCrashed, WatchEvent::ChromeUnreachable]
    }

    async fn check(&self) -> Option<WatchEvent> {
        let url = format!("http://localhost:{}/json/version", self.port);
        match reqwest::get(&url).await {
            Ok(_) => None, // Chrome 可达
            Err(_) => {
                // Chrome 不可达 — 可能已崩溃
                Some(WatchEvent::ChromeUnreachable)
            }
        }
    }

    fn check_interval_secs(&self) -> u64 {
        15 // Chrome 崩溃检测需要更快
    }
}

/// 验证码 Watchdog — 借鉴 browser-use captcha_watchdog
pub struct CaptchaWatchdog {
    /// CDP 端口 (预留, 未来实现用)
    #[allow(dead_code)]
    port: u16,
}

impl CaptchaWatchdog {
    pub fn new(port: u16) -> Self {
        Self { port }
    }
}

#[async_trait]
impl Watchdog for CaptchaWatchdog {
    fn name(&self) -> &str {
        "captcha"
    }

    fn listens_to(&self) -> Vec<WatchEvent> {
        vec![]
    }

    fn emits(&self) -> Vec<WatchEvent> {
        vec![WatchEvent::CaptchaDetected]
    }

    async fn check(&self) -> Option<WatchEvent> {
        // TODO: 通过 CDP 检测页面上的验证码元素
        // 当前为占位实现, 实际需要检查 iframe 或特定 DOM 元素
        None
    }

    fn check_interval_secs(&self) -> u64 {
        60 // 验证码不需要频繁检查
    }
}

/// 弹窗 Watchdog — 借鉴 browser-use popups_watchdog
pub struct PopupWatchdog {
    /// CDP 端口 (预留, 未来实现用)
    #[allow(dead_code)]
    port: u16,
}

impl PopupWatchdog {
    pub fn new(port: u16) -> Self {
        Self { port }
    }
}

#[async_trait]
impl Watchdog for PopupWatchdog {
    fn name(&self) -> &str {
        "popup"
    }

    fn listens_to(&self) -> Vec<WatchEvent> {
        vec![WatchEvent::PopupDetected("".to_string())]
    }

    fn emits(&self) -> Vec<WatchEvent> {
        vec![WatchEvent::PopupDetected("unknown".to_string())]
    }

    async fn check(&self) -> Option<WatchEvent> {
        // TODO: 通过 CDP 检测 JavaScript dialog (alert/confirm/prompt)
        None
    }

    fn check_interval_secs(&self) -> u64 {
        30
    }
}

// ============================================================================
//  纯函数 — 事件匹配逻辑 (可测试)
// ============================================================================

/// 判断一个 watchdog 是否应该处理一个事件
///
/// 规则:
/// 1. 如果 watchdog 的 listens_to 为空, 不处理任何事件
/// 2. 如果 watchdog 的 listens_to 包含精确匹配的事件, 处理
/// 3. Custom 事件匹配所有 Custom 事件
pub fn should_handle_event(listens_to: &[WatchEvent], event: &WatchEvent) -> bool {
    if listens_to.is_empty() {
        return false;
    }

    listens_to.iter().any(|e| {
        // 精确匹配 (忽略 Custom 的内部字符串)
        e.name() == event.name()
    })
}
pub fn should_trigger_auto_recovery(event: &WatchEvent) -> bool {
    event.severity() >= 2
}

/// 获取事件优先级 (用于排序事件队列)
pub fn event_priority(event: &WatchEvent) -> u32 {
    // 严重级别越高, 优先级越高
    // 同级别内, 恢复相关事件优先
    let base = event.severity() as u32 * 100;
    let bonus = match event {
        WatchEvent::ChromeCrashed => 50,
        WatchEvent::ChromeUnreachable => 50,
        WatchEvent::RecoveryStarted => 10,
        WatchEvent::RecoveryCompleted(true) => 5,
        _ => 0,
    };
    base + bonus
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ===== WatchEvent 测试 =====

    #[test]
    fn test_event_severity_chrome_crashed() {
        assert_eq!(WatchEvent::ChromeCrashed.severity(), 3);
    }

    #[test]
    fn test_event_severity_tab_closed() {
        assert_eq!(WatchEvent::TabClosed.severity(), 2);
    }

    #[test]
    fn test_event_severity_dom_changed() {
        assert_eq!(WatchEvent::DomChanged.severity(), 1);
    }

    #[test]
    fn test_event_needs_immediate_recovery_critical() {
        assert!(WatchEvent::ChromeCrashed.needs_immediate_recovery());
        assert!(WatchEvent::ChromeUnreachable.needs_immediate_recovery());
    }

    #[test]
    fn test_event_needs_immediate_recovery_non_critical() {
        assert!(!WatchEvent::DomChanged.needs_immediate_recovery());
        assert!(!WatchEvent::PopupDetected("test".to_string()).needs_immediate_recovery());
    }

    #[test]
    fn test_event_name() {
        assert_eq!(WatchEvent::ChromeCrashed.name(), "chrome_crashed");
        assert_eq!(WatchEvent::TabClosed.name(), "tab_closed");
        assert_eq!(
            WatchEvent::Custom("custom_event".to_string()).name(),
            "custom_event"
        );
    }

    // ===== should_handle_event 测试 =====

    #[test]
    fn test_should_handle_event_exact_match() {
        let listens_to = vec![WatchEvent::ChromeCrashed, WatchEvent::TabClosed];
        assert!(should_handle_event(&listens_to, &WatchEvent::ChromeCrashed));
        assert!(should_handle_event(&listens_to, &WatchEvent::TabClosed));
    }

    #[test]
    fn test_should_handle_event_no_match() {
        let listens_to = vec![WatchEvent::ChromeCrashed];
        assert!(!should_handle_event(&listens_to, &WatchEvent::DomChanged));
    }

    #[test]
    fn test_should_handle_event_empty_listens_to() {
        let listens_to: Vec<WatchEvent> = vec![];
        assert!(!should_handle_event(
            &listens_to,
            &WatchEvent::ChromeCrashed
        ));
    }

    // ===== should_trigger_auto_recovery 测试 =====

    #[test]
    fn test_should_trigger_auto_recovery_critical() {
        assert!(should_trigger_auto_recovery(&WatchEvent::ChromeCrashed));
    }

    #[test]
    fn test_should_trigger_auto_recovery_warning() {
        assert!(should_trigger_auto_recovery(&WatchEvent::TabClosed));
        assert!(should_trigger_auto_recovery(&WatchEvent::ResponseTimeout));
    }

    #[test]
    fn test_should_trigger_auto_recovery_info() {
        assert!(!should_trigger_auto_recovery(&WatchEvent::DomChanged));
        assert!(!should_trigger_auto_recovery(&WatchEvent::PopupDetected(
            "test".to_string()
        )));
    }

    // ===== event_priority 测试 =====

    #[test]
    fn test_event_priority_critical_higher_than_info() {
        let critical = event_priority(&WatchEvent::ChromeCrashed);
        let info = event_priority(&WatchEvent::DomChanged);
        assert!(critical > info);
    }

    #[test]
    fn test_event_priority_recovery_bonus() {
        let crash_priority = event_priority(&WatchEvent::ChromeUnreachable);
        let recovery_start_priority = event_priority(&WatchEvent::RecoveryStarted);
        // ChromeUnreachable (severity=3 + 50 bonus = 350)
        // RecoveryStarted (severity=1 + 10 bonus = 110)
        assert!(crash_priority > recovery_start_priority);
    }

    #[test]
    fn test_event_priority_completed_true_vs_false() {
        let completed_true = event_priority(&WatchEvent::RecoveryCompleted(true));
        let completed_false = event_priority(&WatchEvent::RecoveryCompleted(false));
        // both severity=1, true has +5 bonus
        assert!(completed_true > completed_false);
    }

    // ===== WatchdogRegistry 测试 =====

    #[test]
    fn test_registry_empty() {
        let registry = WatchdogRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn test_registry_register() {
        let mut registry = WatchdogRegistry::new();
        registry.register("chrome", Box::new(ChromeWatchdog::new(9222)));
        assert_eq!(registry.len(), 1);
        assert!(!registry.is_empty());
    }

    #[test]
    fn test_registry_register_multiple() {
        let mut registry = WatchdogRegistry::new();
        registry.register("chrome", Box::new(ChromeWatchdog::new(9222)));
        registry.register("captcha", Box::new(CaptchaWatchdog::new(9222)));
        registry.register("popup", Box::new(PopupWatchdog::new(9222)));
        assert_eq!(registry.len(), 3);
    }

    #[test]
    fn test_registry_event_sender() {
        let registry = WatchdogRegistry::new();
        let sender = registry.event_sender();
        // 应该能发送事件
        assert!(sender.send(WatchEvent::ChromeCrashed).is_ok());
    }

    // ===== ChromeWatchdog 测试 =====

    #[test]
    fn test_chrome_watchdog_name() {
        let wd = ChromeWatchdog::new(9222);
        assert_eq!(wd.name(), "chrome_crash");
    }

    #[test]
    fn test_chrome_watchdog_listens_to() {
        let wd = ChromeWatchdog::new(9222);
        assert!(!wd.listens_to().is_empty());
    }

    #[test]
    fn test_chrome_watchdog_emits() {
        let wd = ChromeWatchdog::new(9222);
        let emits = wd.emits();
        assert!(emits.iter().any(|e| matches!(e, WatchEvent::ChromeCrashed)));
        assert!(emits
            .iter()
            .any(|e| matches!(e, WatchEvent::ChromeUnreachable)));
    }

    #[test]
    fn test_chrome_watchdog_check_interval() {
        let wd = ChromeWatchdog::new(9222);
        assert_eq!(wd.check_interval_secs(), 15);
    }

    #[test]
    fn test_chrome_watchdog_is_enabled() {
        let wd = ChromeWatchdog::new(9222);
        assert!(wd.is_enabled());
    }

    // ===== CaptchaWatchdog 测试 =====

    #[test]
    fn test_captcha_watchdog_name() {
        let wd = CaptchaWatchdog::new(9222);
        assert_eq!(wd.name(), "captcha");
    }

    #[test]
    fn test_captcha_watchdog_check_interval() {
        let wd = CaptchaWatchdog::new(9222);
        assert_eq!(wd.check_interval_secs(), 60);
    }

    // ===== PopupWatchdog 测试 =====

    #[test]
    fn test_popup_watchdog_name() {
        let wd = PopupWatchdog::new(9222);
        assert_eq!(wd.name(), "popup");
    }

    #[test]
    fn test_popup_watchdog_check_interval() {
        let wd = PopupWatchdog::new(9222);
        assert_eq!(wd.check_interval_secs(), 30);
    }
}
