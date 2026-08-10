//! 反检测 JS 补丁注入 — 借鉴 zendriver-rs stealth patches
//!
//! AI 聊天网站可能检测到自动化控制 (如 `navigator.webdriver`)。
//! 本模块提供 JS 补丁, 通过 `Page.addScriptToEvaluateOnNewDocument`
//! 在页面加载前注入, 隐藏自动化标记。
//!
//! ## 借鉴来源
//!
//! - `zendriver-rs/patches/webdriver.js` — 隐藏 navigator.webdriver
//! - `zendriver-rs/patches/chrome.js` — 伪造 window.chrome 对象
//! - `zendriver-rs/patches/plugins.js` — 伪造插件列表
//! - `zendriver-rs/patches/permissions.js` — 权限 API 伪装
//! - `zendriver-rs/patches/webrtc.js` — WebRTC IP 泄露防护
//! - `zendriver-rs/patches/screen.js` — 窗口/屏幕几何一致性
//!
//! ## 使用方式
//!
//! ```no_run
//! use forge::stealth_patches::build_bootstrap_script;
//!
//! let script = build_bootstrap_script();
//! // cdp_session.send_command("Page.addScriptToEvaluateOnNewDocument",
//! //     serde_json::json!({"source": script})).await?;
//! ```

/// 构建完整的反检测 bootstrap 脚本
///
/// 将所有补丁合并为单个 IIFE (立即执行函数表达式),
/// 通过一次 `Page.addScriptToEvaluateOnNewDocument` 注入。
pub fn build_bootstrap_script() -> String {
    let mut script = String::with_capacity(8192);

    // 1. 隐藏 navigator.webdriver (最常被检测的标记)
    script.push_str(WEBDRIVER_PATCH);
    script.push('\n');

    // 2. 伪造 window.chrome 对象
    script.push_str(CHROME_OBJECT_PATCH);
    script.push('\n');

    // 3. 伪造插件列表
    script.push_str(PLUGINS_PATCH);
    script.push('\n');

    // 4. 权限 API 伪装
    script.push_str(PERMISSIONS_PATCH);
    script.push('\n');

    // 5. WebRTC IP 泄露防护
    script.push_str(WEBRTC_PATCH);
    script.push('\n');

    // 6. 窗口/屏幕几何一致性
    script.push_str(SCREEN_PATCH);

    script
}

/// 隐藏 `navigator.webdriver` 标记 — 最常被检测的自动化标记
pub const WEBDRIVER_PATCH: &str = r#"
// Hide navigator.webdriver — the most common automation detection
(() => {
    'use strict';
    Object.defineProperty(navigator, 'webdriver', {
        get: () => undefined,
        configurable: true,
    });
})();
"#;

/// 伪造 `window.chrome` 对象 — headless Chrome 可能缺少此对象
pub const CHROME_OBJECT_PATCH: &str = r#"
// Ensure window.chrome exists (headless mode may lack it)
(() => {
    'use strict';
    if (!window.chrome) {
        window.chrome = {
            runtime: {},
            loadTimes: () => {},
            csi: () => {},
            app: {},
        };
    }
})();
"#;

/// 伪造插件列表 — 自动化浏览器通常没有插件
pub const PLUGINS_PATCH: &str = r#"
// Fake plugin list (automated browsers typically have none)
(() => {
    'use strict';
    Object.defineProperty(navigator, 'plugins', {
        get: () => {
            const plugins = [
                { name: 'Chrome PDF Plugin', filename: 'internal-pdf-viewer', description: 'Portable Document Format' },
                { name: 'Chrome PDF Viewer', filename: 'mhjfbmdgcfjbbpaeojofohoefgiehjai', description: '' },
                { name: 'Native Client', filename: 'internal-nacl-plugin', description: '' },
            ];
            plugins.length = 3;
            return plugins;
        },
        configurable: true,
    });
})();
"#;

/// 权限 API 伪装 — 自动化浏览器的 `navigator.permissions.query` 行为异常
pub const PERMISSIONS_PATCH: &str = r#"
// Fix navigator.permissions.query behavior
(() => {
    'use strict';
    const originalQuery = navigator.permissions?.query;
    if (originalQuery) {
        navigator.permissions.query = (parameters) => (
            parameters.name === 'notifications' ?
                Promise.resolve({ state: Notification.permission }) :
                originalQuery(parameters)
        );
    }
})();
"#;

/// WebRTC IP 泄露防护 — 防止通过 WebRTC 获取真实 IP 地址
pub const WEBRTC_PATCH: &str = r#"
// Prevent WebRTC IP leak
(() => {
    'use strict';
    if (window.RTCPeerConnection) {
        const OriginalRTC = window.RTCPeerConnection;
        window.RTCPeerConnection = function(...args) {
            if (args[0]?.iceServers) {
                args[0].iceServers = [];
            }
            return new OriginalRTC(...args);
        };
        window.RTCPeerConnection.prototype = OriginalRTC.prototype;
    }
})();
"#;

/// 窗口/屏幕几何一致性 — 修复 headless 模式下的不可能的窗口尺寸
pub const SCREEN_PATCH: &str = r#"
// Fix window/screen geometry coherence (headless mode artifacts)
(() => {
    'use strict';
    // Ensure outerWidth >= innerWidth (headless may have outerWidth=0)
    if (window.outerWidth === 0) {
        Object.defineProperty(window, 'outerWidth', {
            get: () => window.innerWidth,
            configurable: true,
        });
    }
    if (window.outerHeight === 0) {
        Object.defineProperty(window, 'outerHeight', {
            get: () => window.innerHeight,
            configurable: true,
        });
    }
})();
"#;

// ============================================================================
//  纯函数 — 补丁构建和验证
// ============================================================================

/// 判断是否需要反检测补丁
///
/// 当使用 CDP 连接浏览器时, `navigator.webdriver` 会被设为 true,
/// 需要注入补丁隐藏此标记。
pub fn needs_stealth_patches(using_cdp: bool) -> bool {
    using_cdp
}

/// 获取所有补丁名称列表 (用于日志和调试)
pub fn patch_names() -> Vec<&'static str> {
    vec![
        "webdriver",
        "chrome_object",
        "plugins",
        "permissions",
        "webrtc",
        "screen",
    ]
}

/// 验证 bootstrap 脚本是否包含所有必要的补丁
pub fn validate_bootstrap_script(script: &str) -> bool {
    script.contains("navigator, 'webdriver'")
        && script.contains("window.chrome")
        && script.contains("navigator, 'plugins'")
        && script.contains("permissions")
        && script.contains("RTCPeerConnection")
        && script.contains("outerWidth")
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_bootstrap_script_not_empty() {
        let script = build_bootstrap_script();
        assert!(!script.is_empty());
    }

    #[test]
    fn test_build_bootstrap_script_contains_all_patches() {
        let script = build_bootstrap_script();
        assert!(script.contains("webdriver"), "Missing webdriver patch");
        assert!(script.contains("chrome"), "Missing chrome patch");
        assert!(script.contains("plugins"), "Missing plugins patch");
        assert!(script.contains("permissions"), "Missing permissions patch");
        assert!(script.contains("RTCPeerConnection"), "Missing webrtc patch");
        assert!(script.contains("outerWidth"), "Missing screen patch");
    }

    #[test]
    fn test_needs_stealth_patches_cdp() {
        assert!(needs_stealth_patches(true));
    }

    #[test]
    fn test_needs_stealth_patches_no_cdp() {
        assert!(!needs_stealth_patches(false));
    }

    #[test]
    fn test_patch_names_complete() {
        let names = patch_names();
        assert_eq!(names.len(), 6);
        assert!(names.contains(&"webdriver"));
        assert!(names.contains(&"chrome_object"));
        assert!(names.contains(&"plugins"));
        assert!(names.contains(&"permissions"));
        assert!(names.contains(&"webrtc"));
        assert!(names.contains(&"screen"));
    }

    #[test]
    fn test_validate_bootstrap_script_valid() {
        let script = build_bootstrap_script();
        assert!(validate_bootstrap_script(&script));
    }

    #[test]
    fn test_validate_bootstrap_script_empty() {
        assert!(!validate_bootstrap_script(""));
    }

    #[test]
    fn test_validate_bootstrap_script_partial() {
        let partial = "navigator, 'webdriver' window.chrome";
        assert!(!validate_bootstrap_script(partial));
    }

    #[test]
    fn test_webdriver_patch_hides_navigator() {
        assert!(WEBDRIVER_PATCH.contains("navigator"));
        assert!(WEBDRIVER_PATCH.contains("webdriver"));
        assert!(WEBDRIVER_PATCH.contains("undefined"));
    }

    #[test]
    fn test_chrome_object_patch_creates_window_chrome() {
        assert!(CHROME_OBJECT_PATCH.contains("window.chrome"));
        assert!(CHROME_OBJECT_PATCH.contains("runtime"));
    }

    #[test]
    fn test_plugins_patch_fake_plugins() {
        assert!(PLUGINS_PATCH.contains("Chrome PDF Plugin"));
        assert!(PLUGINS_PATCH.contains("navigator, 'plugins'"));
    }

    #[test]
    fn test_permissions_patch_fixes_query() {
        assert!(PERMISSIONS_PATCH.contains("permissions.query"));
        assert!(PERMISSIONS_PATCH.contains("notifications"));
    }

    #[test]
    fn test_webrtc_patch_blocks_ice_servers() {
        assert!(WEBRTC_PATCH.contains("RTCPeerConnection"));
        assert!(WEBRTC_PATCH.contains("iceServers"));
    }

    #[test]
    fn test_screen_patch_fixes_geometry() {
        assert!(SCREEN_PATCH.contains("outerWidth"));
        assert!(SCREEN_PATCH.contains("outerHeight"));
        assert!(SCREEN_PATCH.contains("innerWidth"));
    }

    #[test]
    fn test_bootstrap_script_is_iife() {
        let script = build_bootstrap_script();
        // 每个补丁应包含 IIFE 模式
        assert!(script.contains("(() => {"));
        assert!(script.contains("})();"));
    }
}
