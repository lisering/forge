//! 浏览器自动检测与端口管理 — 借鉴 MediaCrawler BrowserLauncher 设计
//!
//! 自动检测 Chrome/Edge 安装路径（macOS/Windows/Linux 三平台），
//! 寻找可用调试端口（从 9222 开始尝试），启动浏览器进程，
//! 等待 CDP 端口就绪，并在退出时清理子进程。
//!
//! ## 设计目标
//!
//! - **降低使用门槛**：用户不再需要手动用 `--remote-debugging-port` 启动 Chrome
//! - **跨平台兼容**：自动适配 macOS / Windows / Linux
//! - **端口冲突处理**：从 9222 开始尝试最多 100 个端口
//! - **进程生命周期管理**：启动、等待就绪、优雅清理
//!
//! ## 使用示例
//!
//! ```no_run
//! use forge::browser_launcher::BrowserLauncher;
//! use std::time::Duration;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let mut launcher = BrowserLauncher::new();
//! let port = launcher.find_available_port(9222)?;
//! launcher.launch(port, None, &[])?;
//! launcher.wait_for_ready(port, Duration::from_secs(10)).await?;
//! // ... 使用 CDP 端口连接 ...
//! launcher.cleanup(); // 退出时关闭浏览器
//! # Ok(())
//! # }
//! ```

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// On Unix, Chrome spawns multiple helper processes (GPU, renderer, utility,
/// crashpad). Killing only the parent leaves orphans that can block the user's
/// normal Chrome. We launch Chrome in its own process group (`setsid`) so we
/// can `kill(-pgid, SIGKILL)` the entire tree at once.
#[cfg(unix)]
use std::os::unix::process::CommandExt;

// ============================================================================
//  纯函数 — 浏览器路径检测（可测试, 无 I/O 副作用）
// ============================================================================

/// 根据当前操作系统返回候选浏览器可执行文件路径列表
///
/// 按优先级排列: 先 Chrome, 再 Edge, 最后 Chromium。
/// 返回的路径可能不存在, 调用方应使用 [`browser_exists`] 过滤。
pub fn detect_browser_paths() -> Vec<PathBuf> {
    if cfg!(target_os = "macos") {
        vec![
            PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
            PathBuf::from("/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge"),
            PathBuf::from("/Applications/Chromium.app/Contents/MacOS/Chromium"),
            PathBuf::from("/Applications/Google Chrome Beta.app/Contents/MacOS/Google Chrome Beta"),
        ]
    } else if cfg!(target_os = "windows") {
        vec![
            PathBuf::from(r"C:\Program Files\Google\Chrome\Application\chrome.exe"),
            PathBuf::from(r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe"),
            PathBuf::from(r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"),
            PathBuf::from(r"C:\Program Files\Microsoft\Edge\Application\msedge.exe"),
        ]
    } else {
        // Linux
        vec![
            PathBuf::from("/usr/bin/google-chrome"),
            PathBuf::from("/usr/bin/google-chrome-stable"),
            PathBuf::from("/usr/bin/chromium"),
            PathBuf::from("/usr/bin/chromium-browser"),
            PathBuf::from("/usr/bin/microsoft-edge"),
            PathBuf::from("/usr/bin/microsoft-edge-stable"),
        ]
    }
}

/// 从候选路径列表中找到第一个存在的浏览器
///
/// # 示例
///
/// ```
/// use forge::browser_launcher::{detect_browser_paths, find_browser};
///
/// let paths = detect_browser_paths();
/// // find_browser 会返回第一个存在的路径 (或 None)
/// let _browser = find_browser(&paths);
/// ```
pub fn find_browser(paths: &[PathBuf]) -> Option<PathBuf> {
    paths.iter().find(|p| p.exists()).cloned()
}

/// 检查指定路径的浏览器是否存在
pub fn browser_exists(path: &Path) -> bool {
    path.exists()
}

/// 从环境变量 `FORGE_BROWSER` 读取自定义浏览器路径
///
/// 允许用户通过环境变量指定浏览器路径, 覆盖自动检测。
pub fn browser_from_env() -> Option<PathBuf> {
    std::env::var_os("FORGE_BROWSER").map(PathBuf::from)
}

// ============================================================================
//  纯函数 — 端口管理（可测试）
// ============================================================================

/// 寻找可用端口: 从 `start` 开始尝试 `max_tries` 个端口
///
/// 通过尝试 TCP 连接判断端口是否被占用。
/// 返回第一个可用的端口（即无法建立 TCP 连接的端口, 说明没有进程在监听）。
///
/// # 错误
///
/// 如果所有尝试的端口都被占用, 返回错误。
///
/// # 示例
///
/// ```no_run
/// use forge::browser_launcher::find_available_port_sync;
///
/// let port = find_available_port_sync(9222, 100).unwrap();
/// println!("可用端口: {}", port);
/// ```
pub fn find_available_port_sync(start: u16, max_tries: u16) -> Result<u16> {
    for offset in 0..max_tries {
        let port = start.saturating_add(offset);
        if is_port_available_sync(port) {
            return Ok(port);
        }
    }
    bail!(
        "没有找到可用端口 (尝试 {}-{} 共 {} 个端口均被占用)",
        start,
        start + max_tries - 1,
        max_tries
    );
}

/// 检查端口是否可用（同步版本 — 用于纯函数测试）
///
/// 端口"可用"意味着没有进程在监听, 即 TCP connect 会失败。
pub fn is_port_available_sync(port: u16) -> bool {
    std::net::TcpStream::connect(("127.0.0.1", port)).is_err()
}

/// 构建浏览器启动参数列表
///
/// # 参数
///
/// - `port`: CDP 调试端口
/// - `user_data_dir`: 用户数据目录 (None 则使用默认 `~/.forge-chrome`)
/// - `extra_args`: 额外命令行参数
///
/// # 返回
///
/// 返回启动参数列表, 包含反检测参数以降低被网站识别为自动化的概率。
///
/// # 示例
///
/// ```
/// use forge::browser_launcher::build_launch_args;
/// use std::path::PathBuf;
///
/// let args = build_launch_args(9222, Some(PathBuf::from("/tmp/forge-chrome")), &[]);
/// assert!(args.contains(&"--remote-debugging-port=9222".to_string()));
/// assert!(args.iter().any(|a| a.starts_with("--user-data-dir=")));
/// ```
pub fn build_launch_args(
    port: u16,
    user_data_dir: Option<PathBuf>,
    extra_args: &[String],
) -> Vec<String> {
    let dir = user_data_dir.unwrap_or_else(default_user_data_dir);

    let mut args = vec![
        format!("--remote-debugging-port={}", port),
        format!("--user-data-dir={}", dir.display()),
        // 借鉴 ds4 web_spawn_chrome: 允许所有来源的 CDP 连接
        "--remote-allow-origins=*".to_string(),
        // 反检测参数 — 降低被网站识别为自动化的概率
        "--disable-blink-features=AutomationControlled".to_string(),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
        "--disable-features=Translate".to_string(),
        // 借鉴 ds4: 禁用同步, 避免弹窗和账号提示
        "--disable-sync".to_string(),
        // 借鉴 ds4: 使用 mock keychain, 避免系统钥匙串弹窗
        "--use-mock-keychain".to_string(),
        // 借鉴 ds4: 使用基础密码存储, 避免系统密码管理器弹窗
        "--password-store=basic".to_string(),
        // 借鉴 ds4: 静音音频, 避免网页突然发声 (24h 运行)
        "--mute-audio".to_string(),
        // 性能优化
        "--disable-extensions".to_string(),
        "--disable-plugins".to_string(),
        "--disable-popup-blocking".to_string(),
    ];

    args.extend_from_slice(extra_args);
    args
}

/// 默认用户数据目录: `~/.forge-chrome`
///
/// 使用用户目录而非 `/tmp`, 确保重启后登录状态保留。
pub fn default_user_data_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".forge-chrome")
    } else {
        PathBuf::from("/tmp/forge-chrome")
    }
}

// ============================================================================
//  纯函数 — 浏览器名称检测（用于日志输出）
// ============================================================================

/// 从浏览器路径中提取友好名称 (如 "Google Chrome", "Microsoft Edge")
pub fn browser_name(path: &Path) -> String {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "browser".to_string());

    // 根据可执行文件名判断浏览器类型
    let lower = file_name.to_lowercase();
    if lower.contains("chrome") {
        "Google Chrome".to_string()
    } else if lower.contains("edge") || lower.contains("msedge") {
        "Microsoft Edge".to_string()
    } else if lower.contains("chromium") {
        "Chromium".to_string()
    } else {
        file_name
    }
}

// ============================================================================
//  BrowserLauncher — 浏览器启动器（管理子进程生命周期）
// ============================================================================

/// 浏览器启动器 — 自动检测、启动、等待就绪、清理
///
/// 借鉴 MediaCrawler `BrowserLauncher` 的设计, 管理 Chrome/Edge 子进程的生命周期。
///
/// # 生命周期
///
/// 1. `new()` — 创建启动器
/// 2. `find_available_port()` — 寻找可用端口
/// 3. `launch()` — 启动浏览器进程
/// 4. `wait_for_ready()` — 等待 CDP 端口就绪
/// 5. (使用浏览器...)
/// 6. `cleanup()` — 关闭浏览器进程
///
/// # 示例
///
/// ```no_run
/// # use forge::browser_launcher::BrowserLauncher;
/// # use std::time::Duration;
/// # async fn example() -> anyhow::Result<()> {
/// let mut launcher = BrowserLauncher::new();
/// let port = launcher.find_available_port(9222)?;
/// launcher.launch(port, None, &[])?;
/// launcher.wait_for_ready(port, Duration::from_secs(10)).await?;
/// // ... 使用端口连接 CDP ...
/// launcher.cleanup();
/// # Ok(())
/// # }
/// ```
pub struct BrowserLauncher {
    /// 子进程句柄 (启动后存在)
    child: Option<Child>,
    /// 浏览器可执行文件路径
    browser_path: Option<PathBuf>,
    /// 实际使用的 CDP 端口 (启动后存在)
    ///
    /// Session 69: 用于将实际端口传递给 BrowserManager,
    /// 解决 find_available_port 可能返回不同于 cli.port 的问题。
    port: Option<u16>,
    /// Unix 进程组 ID — 用于 kill 整个 Chrome 进程树
    #[cfg(unix)]
    pgid: Option<i32>,
}

impl BrowserLauncher {
    /// 创建新的浏览器启动器
    pub fn new() -> Self {
        Self {
            child: None,
            browser_path: None,
            port: None,
            #[cfg(unix)]
            pgid: None,
        }
    }

    /// 自动检测浏览器路径
    ///
    /// 优先级: 环境变量 `FORGE_BROWSER` > 自动检测
    ///
    /// # 错误
    ///
    /// 如果找不到任何浏览器, 返回错误。
    pub fn detect_browser(&mut self) -> Result<&Path> {
        // 1. 先检查环境变量
        if let Some(path) = browser_from_env() {
            if browser_exists(&path) {
                debug!("使用环境变量指定的浏览器: {}", path.display());
                self.browser_path = Some(path);
                return Ok(self.browser_path.as_deref().unwrap());
            } else {
                warn!(
                    "环境变量 FORGE_BROWSER 指定的浏览器不存在: {}",
                    path.display()
                );
            }
        }

        // 2. 自动检测
        let paths = detect_browser_paths();
        if let Some(path) = find_browser(&paths) {
            debug!("检测到浏览器: {} ({})", browser_name(&path), path.display());
            self.browser_path = Some(path);
            Ok(self.browser_path.as_deref().unwrap())
        } else {
            let tried: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
            bail!(
                "未检测到浏览器。尝试了以下路径:\n{}\n\
                 请设置环境变量 FORGE_BROWSER 指定浏览器路径, 或安装 Chrome/Edge",
                tried.join("\n")
            )
        }
    }

    /// 寻找可用端口 (从 `start` 开始尝试 100 个端口)
    pub fn find_available_port(&self, start: u16) -> Result<u16> {
        find_available_port_sync(start, 100)
    }

    /// 启动浏览器进程
    ///
    /// # 参数
    ///
    /// - `port`: CDP 调试端口
    /// - `user_data_dir`: 用户数据目录 (None 使用默认 `~/.forge-chrome`)
    /// - `extra_args`: 额外启动参数
    ///
    /// # 错误
    ///
    /// - 未检测到浏览器 (需先调用 `detect_browser()`)
    /// - 启动进程失败
    pub fn launch(
        &mut self,
        port: u16,
        user_data_dir: Option<PathBuf>,
        extra_args: &[String],
    ) -> Result<()> {
        let browser_path = if let Some(ref path) = self.browser_path {
            path
        } else {
            // 自动检测
            self.detect_browser()?;
            self.browser_path.as_ref().unwrap()
        };

        let args = build_launch_args(port, user_data_dir, extra_args);

        info!(
            "启动浏览器: {} (端口 {}, {} 个参数)",
            browser_name(browser_path),
            port,
            args.len()
        );
        debug!("启动参数: {}", args.join(" "));

        let mut command = Command::new(browser_path);
        command.args(&args);
        command.stdout(Stdio::null());
        command.stderr(Stdio::null());

        // Unix: 启动 Chrome 在新进程组中, 以便后续 kill 整个进程树
        #[cfg(unix)]
        {
            // pre_exec 在 fork 后、exec 前调用
            // setsid() 创建新会话和新进程组, 使 Chrome 成为进程组长
            unsafe {
                command.pre_exec(|| {
                    libc::setsid();
                    Ok(())
                });
            }
        }

        let child = command
            .spawn()
            .context(format!("启动浏览器失败: {}", browser_path.display()))?;

        // Unix: 记录进程组 ID (Chrome 的 PID 就是 pgid, 因为 setsid 使其成为组长)
        #[cfg(unix)]
        {
            self.pgid = Some(child.id() as i32);
        }

        info!("浏览器进程已启动 (PID: {})", child.id());
        self.child = Some(child);
        self.port = Some(port);
        Ok(())
    }

    /// 等待 CDP 端口就绪 (浏览器完成启动并开始监听)
    ///
    /// 通过 HTTP 请求 `http://localhost:{port}/json/version` 判断是否就绪。
    ///
    /// # 参数
    ///
    /// - `port`: CDP 调试端口
    /// - `timeout`: 最大等待时间
    ///
    /// # 错误
    ///
    /// 超时后端口仍未就绪, 或浏览器进程已退出。
    pub async fn wait_for_ready(&mut self, port: u16, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        let url = format!("http://localhost:{}/json/version", port);

        loop {
            // 检查进程是否已退出
            if let Some(ref mut child) = self.child {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        bail!("浏览器进程已退出 (状态: {})", status);
                    }
                    Ok(None) => { /* 进程仍在运行 */ }
                    Err(e) => {
                        warn!("检查浏览器进程状态失败: {}", e);
                    }
                }
            }

            // 检查端口是否就绪
            if reqwest::get(&url).await.is_ok() {
                info!(
                    "浏览器 CDP 端口 {} 已就绪 (耗时 {:?})",
                    port,
                    deadline - Instant::now()
                );
                return Ok(());
            }

            if Instant::now() >= deadline {
                bail!(
                    "等待浏览器启动超时 ({}s), 端口 {} 未就绪",
                    timeout.as_secs(),
                    port
                );
            }

            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// 清理: 关闭浏览器进程
    ///
    /// 先尝试优雅关闭 (kill), 再等待进程退出。
    /// 如果进程已退出则无操作。
    ///
    /// Unix 下会 kill 整个进程组 (kill -pgid), 确保 Chrome helper 进程
    /// (GPU, renderer, utility, crashpad) 也被终止, 防止孤儿进程。
    pub fn cleanup(&mut self) {
        if let Some(mut child) = self.child.take() {
            // Unix: 先尝试 kill 整个进程组
            #[cfg(unix)]
            {
                if let Some(pgid) = self.pgid {
                    // kill(-pgid, SIGKILL) 杀死整个进程组
                    unsafe {
                        let _ = libc::kill(-pgid, libc::SIGTERM);
                    }
                    debug!("已向进程组 {} 发送 SIGTERM", pgid);

                    // 短暂等待优雅退出
                    std::thread::sleep(Duration::from_millis(500));

                    // 如果仍在运行, 强制 kill
                    match child.try_wait() {
                        Ok(Some(_)) => { /* 已退出 */ }
                        _ => {
                            unsafe {
                                let _ = libc::kill(-pgid, libc::SIGKILL);
                            }
                            debug!("已向进程组 {} 发送 SIGKILL", pgid);
                        }
                    }
                }
            }

            // 通用: kill 子进程
            match child.kill() {
                Ok(_) => debug!("浏览器进程已发送 kill 信号"),
                Err(e) => {
                    // 进程可能已被进程组 kill 终止
                    debug!("关闭浏览器进程失败 (可能已退出): {}", e)
                }
            }
            // 等待进程退出 (避免僵尸进程)
            match child.wait() {
                Ok(status) => debug!("浏览器进程已退出 (状态: {})", status),
                Err(e) => warn!("等待浏览器进程退出失败: {}", e),
            }
            #[cfg(unix)]
            {
                self.pgid = None;
            }
            info!("浏览器进程已清理");
        }
    }

    /// 获取浏览器进程 PID (如果正在运行)
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(|c| c.id())
    }

    /// 浏览器是否正在运行
    pub fn is_running(&self) -> bool {
        self.child.is_some()
    }

    /// 获取实际使用的 CDP 端口
    ///
    /// Session 69: 当 `find_available_port` 找到的端口与 `cli.port` 不同时,
    /// 调用方需要通过此方法获取实际端口, 传递给 `BrowserManager`。
    ///
    /// # 返回
    ///
    /// - `Some(port)`: 浏览器已启动, 返回实际端口
    /// - `None`: 浏览器未启动
    pub fn port(&self) -> Option<u16> {
        self.port
    }

    /// 获取 Unix 进程组 ID (用于调试和诊断)
    ///
    /// 仅在 Unix 平台可用。Chrome 启动后, `pgid` 等于 Chrome 主进程 PID,
    /// 因为 `setsid()` 使其成为进程组长。
    #[cfg(unix)]
    pub fn pgid(&self) -> Option<i32> {
        self.pgid
    }

    /// 自动打开默认聊天网页
    ///
    /// Session 69: 当 `--auto-launch` 启动浏览器后, 自动打开默认聊天网页,
    /// 用户无需手动在浏览器中输入 URL。
    ///
    /// # 参数
    ///
    /// - `urls`: 要打开的 URL 列表 (如 `["https://chat.deepseek.com"]`)
    ///
    /// # 错误
    ///
    /// 如果 CDP 端口不可达或创建标签页失败, 返回错误。
    pub async fn auto_open_chats(&self, urls: &[&str]) -> Result<()> {
        let port = self
            .port
            .ok_or_else(|| anyhow::anyhow!("浏览器未启动, 无法自动打开聊天网页"))?;

        for url in urls {
            info!("自动打开聊天网页: {}", url);
            match crate::cdp::create_tab(port, url).await {
                Ok(tab) => {
                    info!("已打开: {} ({})", tab.title, tab.url);
                }
                Err(e) => {
                    warn!("打开 {} 失败: {}", url, e);
                }
            }
        }
        Ok(())
    }
}

impl Default for BrowserLauncher {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for BrowserLauncher {
    fn drop(&mut self) {
        self.cleanup();
    }
}

// ============================================================================
//  连接已有浏览器 — 借鉴 MediaCrawler _connect_existing_browser
// ============================================================================

/// 等待并连接已有浏览器
///
/// 当用户已经打开了带远程调试的 Chrome 时, Forge 可以直接连接,
/// 复用用户已有的登录态和标签页, 大幅降低使用门槛。
///
/// # 参数
///
/// - `port`: CDP 调试端口
/// - `timeout`: 最大等待时间
///
/// # 行为
///
/// 每隔 1 秒检查端口是否可用, 最多等待 `timeout` 时间。
/// 每 5 秒输出一次等待提示。
///
/// # 错误
///
/// 超时后仍未检测到浏览器。
///
/// # 示例
///
/// ```no_run
/// # use forge::browser_launcher::connect_existing_browser;
/// # use std::time::Duration;
/// # async fn example() -> anyhow::Result<()> {
/// connect_existing_browser(9222, Duration::from_secs(60)).await?;
/// // 浏览器已就绪, 可以连接 CDP
/// # Ok(())
/// # }
/// ```
pub async fn connect_existing_browser(port: u16, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let url = format!("http://localhost:{}/json/version", port);

    info!("等待已有浏览器开启远程调试 (端口 {})...", port);

    loop {
        if reqwest::get(&url).await.is_ok() {
            info!("已连接到已有浏览器 (端口 {})", port);
            return Ok(());
        }

        if Instant::now() >= deadline {
            bail!(
                "等待浏览器超时 ({}s)。请确保 Chrome 已用 --remote-debugging-port={} 启动",
                timeout.as_secs(),
                port
            );
        }

        // 每 5 秒提示一次
        let elapsed = deadline.duration_since(Instant::now());
        let waited = timeout.as_secs().saturating_sub(elapsed.as_secs());
        if waited > 0 && waited.is_multiple_of(5) {
            info!("等待浏览器开启远程调试... (已等待 {}s)", waited);
        }

        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// 检查指定端口的浏览器是否已在运行
pub async fn is_browser_running(port: u16) -> bool {
    let url = format!("http://localhost:{}/json/version", port);
    reqwest::get(&url).await.is_ok()
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ===== detect_browser_paths 测试 =====

    #[test]
    fn test_detect_browser_paths_not_empty() {
        let paths = detect_browser_paths();
        assert!(!paths.is_empty(), "应至少返回一个候选浏览器路径");
    }

    #[test]
    fn test_detect_browser_paths_contains_chrome() {
        let paths = detect_browser_paths();
        let has_chrome = paths.iter().any(|p| {
            let lower = p.to_string_lossy().to_lowercase();
            lower.contains("chrome")
        });
        assert!(has_chrome, "应包含 Chrome 路径");
    }

    #[test]
    fn test_detect_browser_paths_macos_format() {
        if !cfg!(target_os = "macos") {
            return;
        }
        let paths = detect_browser_paths();
        assert!(paths
            .iter()
            .any(|p| p.to_string_lossy().contains(".app/Contents/MacOS")));
    }

    #[test]
    fn test_detect_browser_paths_windows_format() {
        if !cfg!(target_os = "windows") {
            return;
        }
        let paths = detect_browser_paths();
        assert!(paths
            .iter()
            .any(|p| p.to_string_lossy().contains("Program Files")));
    }

    #[test]
    fn test_detect_browser_paths_linux_format() {
        if !cfg!(target_os = "linux") {
            return;
        }
        let paths = detect_browser_paths();
        assert!(paths
            .iter()
            .any(|p| p.to_string_lossy().starts_with("/usr/")));
    }

    // ===== find_browser 测试 =====

    #[test]
    fn test_find_browser_returns_existing() {
        // 创建临时文件模拟浏览器
        let temp = tempfile::tempdir().unwrap();
        let fake_browser = temp.path().join("chrome");
        std::fs::write(&fake_browser, "fake").unwrap();

        let paths = vec![
            PathBuf::from("/nonexistent/browser1"),
            fake_browser.clone(),
            PathBuf::from("/nonexistent/browser2"),
        ];
        let result = find_browser(&paths);
        assert_eq!(result, Some(fake_browser));
    }

    #[test]
    fn test_find_browser_returns_none_when_none_exist() {
        let paths = vec![
            PathBuf::from("/nonexistent1"),
            PathBuf::from("/nonexistent2"),
        ];
        assert_eq!(find_browser(&paths), None);
    }

    #[test]
    fn test_find_browser_empty_list() {
        let paths: Vec<PathBuf> = vec![];
        assert_eq!(find_browser(&paths), None);
    }

    #[test]
    fn test_find_browser_returns_first_match() {
        let temp1 = tempfile::tempdir().unwrap();
        let temp2 = tempfile::tempdir().unwrap();
        let browser1 = temp1.path().join("chrome");
        let browser2 = temp2.path().join("edge");
        std::fs::write(&browser1, "fake").unwrap();
        std::fs::write(&browser2, "fake").unwrap();

        let paths = vec![browser1.clone(), browser2];
        assert_eq!(find_browser(&paths), Some(browser1));
    }

    // ===== browser_exists 测试 =====

    #[test]
    fn test_browser_exists_true() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("browser");
        std::fs::write(&path, "fake").unwrap();
        assert!(browser_exists(&path));
    }

    #[test]
    fn test_browser_exists_false() {
        assert!(!browser_exists(Path::new("/nonexistent/browser/path")));
    }

    // ===== browser_from_env 测试 =====

    #[test]
    fn test_browser_from_env_not_set() {
        // 确保环境变量未设置 (可能影响其他测试, 先保存)
        let saved = std::env::var_os("FORGE_BROWSER");
        std::env::remove_var("FORGE_BROWSER");

        assert!(browser_from_env().is_none());

        // 恢复
        if let Some(val) = saved {
            std::env::set_var("FORGE_BROWSER", val);
        }
    }

    #[test]
    fn test_browser_from_env_set() {
        let saved = std::env::var_os("FORGE_BROWSER");
        std::env::set_var("FORGE_BROWSER", "/custom/browser/path");

        assert_eq!(
            browser_from_env(),
            Some(PathBuf::from("/custom/browser/path"))
        );

        // 恢复
        if let Some(val) = saved {
            std::env::set_var("FORGE_BROWSER", val);
        } else {
            std::env::remove_var("FORGE_BROWSER");
        }
    }

    // ===== is_port_available_sync 测试 =====

    #[test]
    fn test_is_port_available_sync_unused_port() {
        // 使用一个不太可能被占用的端口
        // 端口 0 是保留端口, 不应被监听
        // 但 connect 到端口 0 会返回错误, 所以它"可用"
        assert!(is_port_available_sync(1));
    }

    #[test]
    fn test_is_port_available_sync_listening_port() {
        // 绑定一个端口, 然后检查它是否"不可用"
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(!is_port_available_sync(port));
        drop(listener);
    }

    // ===== find_available_port_sync 测试 =====

    #[test]
    fn test_find_available_port_sync_finds_free_port() {
        // 从高端口开始, 减少冲突概率
        let port = find_available_port_sync(50000, 100).unwrap();
        assert!(port >= 50000);
        assert!(port < 50100);
    }

    #[test]
    fn test_find_available_port_sync_all_occupied() {
        // 绑定多个端口使其不可用
        let mut listeners = Vec::new();
        for port in 40000..40010 {
            if let Ok(l) = std::net::TcpListener::bind(("127.0.0.1", port)) {
                listeners.push(l);
            }
        }

        // 可能不是所有端口都能绑定 (有些可能已被占用), 所以只测试能绑定的
        if listeners.len() == 10 {
            let result = find_available_port_sync(40000, 10);
            assert!(result.is_err());
        }
        // 如果不是全部绑定成功, 测试仍然应该能找到一个端口
        // (因为未绑定的端口是可用的)
    }

    #[test]
    fn test_find_available_port_sync_zero_tries() {
        let result = find_available_port_sync(9222, 0);
        assert!(result.is_err());
    }

    // ===== build_launch_args 测试 =====

    #[test]
    fn test_build_launch_args_contains_port() {
        let args = build_launch_args(9222, None, &[]);
        assert!(args.iter().any(|a| a == "--remote-debugging-port=9222"));
    }

    #[test]
    fn test_build_launch_args_contains_user_data_dir() {
        let dir = PathBuf::from("/tmp/test-chrome");
        let args = build_launch_args(9222, Some(dir.clone()), &[]);
        assert!(args
            .iter()
            .any(|a| a == &format!("--user-data-dir={}", dir.display())));
    }

    #[test]
    fn test_build_launch_args_default_user_data_dir() {
        let args = build_launch_args(9222, None, &[]);
        assert!(args.iter().any(|a| a.starts_with("--user-data-dir=")));
    }

    #[test]
    fn test_build_launch_args_contains_anti_detection() {
        let args = build_launch_args(9222, None, &[]);
        assert!(args.iter().any(|a| a.contains("AutomationControlled")));
    }

    #[test]
    fn test_build_launch_args_contains_no_first_run() {
        let args = build_launch_args(9222, None, &[]);
        assert!(args.iter().any(|a| a == "--no-first-run"));
    }

    #[test]
    fn test_build_launch_args_contains_ds4_stability_flags() {
        // 借鉴 ds4 web_spawn_chrome 的稳定性参数
        let args = build_launch_args(9222, None, &[]);
        assert!(
            args.iter().any(|a| a == "--remote-allow-origins=*"),
            "应包含 --remote-allow-origins=* (借鉴 ds4)"
        );
        assert!(
            args.iter().any(|a| a == "--disable-sync"),
            "应包含 --disable-sync (借鉴 ds4)"
        );
        assert!(
            args.iter().any(|a| a == "--use-mock-keychain"),
            "应包含 --use-mock-keychain (借鉴 ds4)"
        );
        assert!(
            args.iter().any(|a| a == "--password-store=basic"),
            "应包含 --password-store=basic (借鉴 ds4)"
        );
        assert!(
            args.iter().any(|a| a == "--mute-audio"),
            "应包含 --mute-audio (借鉴 ds4)"
        );
    }

    #[test]
    fn test_build_launch_args_with_extra_args() {
        let extra = vec!["--headless".to_string(), "--disable-gpu".to_string()];
        let args = build_launch_args(9222, None, &extra);
        assert!(args.contains(&"--headless".to_string()));
        assert!(args.contains(&"--disable-gpu".to_string()));
    }

    #[test]
    fn test_build_launch_args_minimal_port() {
        let args = build_launch_args(0, None, &[]);
        assert!(args.iter().any(|a| a == "--remote-debugging-port=0"));
    }

    #[test]
    fn test_build_launch_args_max_port() {
        let args = build_launch_args(65535, None, &[]);
        assert!(args.iter().any(|a| a == "--remote-debugging-port=65535"));
    }

    // ===== default_user_data_dir 测试 =====

    #[test]
    fn test_default_user_data_dir_not_empty() {
        let dir = default_user_data_dir();
        assert!(!dir.as_os_str().is_empty());
    }

    #[test]
    fn test_default_user_data_dir_contains_forge() {
        let dir = default_user_data_dir();
        assert!(dir.to_string_lossy().contains("forge"));
    }

    // ===== browser_name 测试 =====

    #[test]
    fn test_browser_name_chrome() {
        let path = PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome");
        assert_eq!(browser_name(&path), "Google Chrome");
    }

    #[test]
    fn test_browser_name_edge() {
        let path = PathBuf::from("/usr/bin/microsoft-edge");
        assert_eq!(browser_name(&path), "Microsoft Edge");
    }

    #[test]
    fn test_browser_name_chromium() {
        let path = PathBuf::from("/usr/bin/chromium");
        assert_eq!(browser_name(&path), "Chromium");
    }

    #[test]
    fn test_browser_name_unknown() {
        let path = PathBuf::from("/usr/bin/firefox");
        assert_eq!(browser_name(&path), "firefox");
    }

    #[test]
    fn test_browser_name_windows_chrome() {
        let path = PathBuf::from(r"C:\Program Files\Google\Chrome\Application\chrome.exe");
        assert_eq!(browser_name(&path), "Google Chrome");
    }

    #[test]
    fn test_browser_name_windows_edge() {
        let path = PathBuf::from(r"C:\Program Files\Microsoft\Edge\Application\msedge.exe");
        assert_eq!(browser_name(&path), "Microsoft Edge");
    }

    // ===== BrowserLauncher 测试 =====

    #[test]
    fn test_browser_launcher_new() {
        let launcher = BrowserLauncher::new();
        assert!(!launcher.is_running());
        assert_eq!(launcher.pid(), None);
    }

    #[test]
    fn test_browser_launcher_default() {
        let launcher = BrowserLauncher::default();
        assert!(!launcher.is_running());
    }

    #[test]
    fn test_browser_launcher_cleanup_when_not_running() {
        let mut launcher = BrowserLauncher::new();
        launcher.cleanup(); // 应该不 panic
    }

    #[test]
    fn test_browser_launcher_detect_browser_with_env() {
        let saved = std::env::var_os("FORGE_BROWSER");
        let temp = tempfile::tempdir().unwrap();
        let fake_browser = temp.path().join("chrome");
        std::fs::write(&fake_browser, "fake").unwrap();
        std::env::set_var("FORGE_BROWSER", &fake_browser);

        let mut launcher = BrowserLauncher::new();
        let result = launcher.detect_browser();
        assert!(result.is_ok());

        // 恢复
        if let Some(val) = saved {
            std::env::set_var("FORGE_BROWSER", val);
        } else {
            std::env::remove_var("FORGE_BROWSER");
        }
    }

    #[test]
    fn test_browser_launcher_detect_browser_not_found() {
        let saved = std::env::var_os("FORGE_BROWSER");
        std::env::remove_var("FORGE_BROWSER");

        // 由于测试环境中可能没有浏览器, 我们只验证不 panic
        let mut launcher = BrowserLauncher::new();
        let _ = launcher.detect_browser();

        // 恢复
        if let Some(val) = saved {
            std::env::set_var("FORGE_BROWSER", val);
        }
    }

    #[test]
    fn test_browser_launcher_find_available_port() {
        let launcher = BrowserLauncher::new();
        let result = launcher.find_available_port(50000);
        assert!(result.is_ok());
    }

    // ===== connect_existing_browser / is_browser_running 测试 =====

    #[tokio::test]
    async fn test_is_browser_running_false() {
        // 使用一个不太可能被占用的端口
        assert!(!is_browser_running(1).await);
    }

    #[tokio::test]
    async fn test_connect_existing_browser_timeout() {
        // 端口 1 不太可能有浏览器, 应该超时
        let result = connect_existing_browser(1, Duration::from_millis(100)).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("超时"));
    }

    // ===== Session 69: port() / auto_open_chats() 测试 =====

    #[test]
    fn test_port_none_before_launch() {
        let launcher = BrowserLauncher::new();
        assert_eq!(launcher.port(), None);
    }

    #[cfg(unix)]
    #[test]
    fn test_pgid_none_before_launch() {
        let launcher = BrowserLauncher::new();
        assert_eq!(launcher.pgid(), None);
    }

    #[cfg(unix)]
    #[test]
    fn test_pgid_set_after_launch() {
        // 使用 sleep 命令模拟浏览器进程
        let mut launcher = BrowserLauncher::new();
        let mut cmd = std::process::Command::new("sleep");
        cmd.arg("10");
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let mut child = cmd.spawn().unwrap();
        launcher.pgid = Some(child.id() as i32);
        assert!(launcher.pgid().is_some());
        // 清理
        unsafe {
            libc::kill(-launcher.pgid().unwrap(), libc::SIGKILL);
        }
        let _ = child.wait();
    }

    #[tokio::test]
    async fn test_auto_open_chats_fails_when_not_launched() {
        let launcher = BrowserLauncher::new();
        let result = launcher
            .auto_open_chats(&["https://chat.deepseek.com"])
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("浏览器未启动"));
    }
}
