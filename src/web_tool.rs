//! Web 工具 — 借鉴 ds4 `ds4_web.c` 的网页交互能力
//!
//! 提供:
//! - 动态页面滚动 (`scroll_dynamic_page`): 智能检测懒加载内容, 滚动到底
//! - 页面内容提取 (`extract_page_content`): 将网页 DOM 转为 Markdown
//! - 搜索结果提取 (`extract_search_results`): 从 Google 搜索结果中提取链接
//! - 页面就绪检测 (`wait_page_ready`): 等待页面加载完成
//!
//! ## 设计理念 (借鉴 ds4)
//!
//! ds4 的 `ds4_web.c` 实现了完整的 CDP 浏览器控制, 包括:
//! - `web_scroll_dynamic_page()`: 智能滚动, 检测懒加载 hooks, 稳定性检测
//! - `web_extract_page_js`: 将网页内容转为结构化 Markdown
//! - `web_extract_search_js`: 从 Google 搜索结果中提取链接
//! - `web_wait_navigated_ready()`: 页面导航后等待就绪
//!
//! Forge 借鉴这些设计, 为未来的 web 搜索/文档查阅能力打下基础。

use crate::cdp::CdpSession;
use anyhow::{bail, Result};
use serde_json::json;
use std::time::Duration;
use tracing::{debug, info};

// ============================================================================
//  纯函数 — JavaScript 代码生成 (可测试, 无 I/O)
// ============================================================================

/// 构建动态页面滚动 JS — 借鉴 ds4 `web_scroll_dynamic_page`
///
/// 该 JS 脚本:
/// 1. 检测页面是否有懒加载 hooks (onscroll, lazy 元素等)
/// 2. 如无可滚动内容或 hooks, 跳过滚动
/// 3. 逐步滚动, 每次等待 900ms 检测内容是否增长
/// 4. 连续 4 次无变化或到底部时停止
///
/// 返回滚动步数和最终文本长度。
pub fn build_scroll_dynamic_page_js() -> &'static str {
    r#"
    (() => new Promise(resolve => {
        const root = () => document.scrollingElement || document.documentElement || document.body;
        const blockSel = 'h1,h2,h3,h4,h5,h6,p,li,pre,blockquote,td,th,[id="content-text"],[class*="comment-body"],[class*="comment-content"],[data-testid*="comment-text"]';
        const lazySel = '[onscroll],[loading="lazy"],[data-src],[data-lazy],[class*="lazy"],[class*="infinite"],[class*="virtual"],[role="feed"],[id*="comment"],[class*="comment"],[data-testid*="comment"]';
        const hookCount = () => {
            let n = 0;
            try { if (window.onscroll) n++; if (document.onscroll) n++; if (document.body && document.body.onscroll) n++; } catch(e) {}
            try { n += document.querySelectorAll(lazySel).length; } catch(e) {}
            return n;
        };
        const metrics = () => {
            const r = root();
            return {
                height: r ? r.scrollHeight : 0,
                view: innerHeight || 900,
                y: scrollY || (r && r.scrollTop) || 0,
                text: ((document.body && document.body.innerText) || '').length,
                links: document.links ? document.links.length : 0,
                blocks: document.body ? document.body.querySelectorAll(blockSel).length : 0,
                hooks: hookCount()
            };
        };
        const sig = m => [m.height, m.text, m.links, m.blocks].join('|');
        const grew = (a, b) => b.height > a.height + 20 || b.text > a.text + 200 || b.links > a.links + 2 || b.blocks > a.blocks + 2;
        const scrollOnce = () => {
            const r = root();
            if (!r) return;
            const h = Math.max(700, Math.floor((innerHeight || 900) * 0.85));
            window.scrollTo(0, Math.min(r.scrollHeight, (scrollY || r.scrollTop || 0) + h));
        };
        let last = metrics(), lastSig = sig(last), same = 0, steps = 0;
        const scrollable = last.height > last.view * 1.35;
        if (!scrollable || last.hooks === 0) {
            resolve('scroll skipped hooks=' + last.hooks + ' text=' + last.text);
            return;
        }
        const tick = () => {
            if (steps >= 28) { resolve('scrolled ' + steps + ' text=' + last.text); return; }
            const before = last;
            scrollOnce();
            steps++;
            setTimeout(() => {
                const now = metrics(), nowSig = sig(now);
                if (nowSig === lastSig) same++;
                else same = 0;
                const loaded = grew(before, now);
                last = now;
                lastSig = nowSig;
                if (steps === 1 && !loaded) { resolve('scroll probe unchanged text=' + now.text); return; }
                const atBottom = now.y + now.view + 20 >= now.height;
                if (same >= 4 || (atBottom && same >= 1)) { resolve('scrolled ' + steps + ' text=' + last.text); return; }
                tick();
            }, 900);
        };
        tick();
    }))()
    "#
}

/// 构建页面内容提取 JS — 借鉴 ds4 `web_extract_page_js`
///
/// 将网页 DOM 转为结构化 Markdown:
/// - 提取标题 (h1-h6)
/// - 提取段落 (p, li, pre, blockquote)
/// - 提取可见链接
/// - 截断超长内容 (900KB)
pub fn build_extract_page_content_js() -> &'static str {
    r#"
    (() => {
        const clean = s => (s || '').replace(/\s+/g, ' ').trim();
        const esc = s => clean(s).replace(/\\/g, '\\\\').replace(/\[/g, '\\[').replace(/\]/g, '\\]').replace(/\n/g, ' ');
        const visible = el => {
            const r = el.getBoundingClientRect();
            const st = getComputedStyle(el);
            return r.width > 0 && r.height > 0 && st.display !== 'none' && st.visibility !== 'hidden' && st.opacity !== '0';
        };
        const inline = n => {
            if (!n) return '';
            if (n.nodeType === 3) return n.nodeValue;
            if (n.nodeType !== 1) return '';
            const el = n;
            if (el.tagName === 'SCRIPT' || el.tagName === 'STYLE' || el.tagName === 'NOSCRIPT') return '';
            if (el.tagName === 'A') {
                const t = esc(el.innerText || el.textContent);
                const h = el.href || '';
                return t && h ? `[${t}](${h})` : t;
            }
            if (el.tagName === 'CODE') return '`' + clean(el.innerText || el.textContent).replace(/`/g, '\\`') + '`';
            return [...el.childNodes].map(inline).join('');
        };
        const lines = [`# ${clean(document.title) || location.href}`, '', `URL: ${location.href}`, '', '## Content'];
        const blocks = [...document.body.querySelectorAll('h1,h2,h3,h4,h5,h6,p,li,pre,blockquote,td,th,[id="content-text"],[class*="comment-body"],[class*="comment-content"],[data-testid*="comment-text"]')];
        const seen = new Set();
        for (const el of blocks) {
            if (!visible(el)) continue;
            let s = '';
            const tag = el.tagName;
            if (/^H[1-6]$/.test(tag)) { s = '#'.repeat(Number(tag[1])) + ' ' + inline(el); }
            else if (tag === 'LI') { s = '- ' + inline(el); }
            else if (tag === 'PRE') { s = '```\n' + (el.innerText || el.textContent || '').trimEnd() + '\n```'; }
            else if (tag === 'BLOCKQUOTE') { s = '> ' + clean(el.innerText || el.textContent); }
            else { s = inline(el); }
            s = s.trim();
            if (!s || seen.has(s)) continue;
            seen.add(s);
            lines.push('', s);
            if (lines.join('\n').length > 900000) { lines.push('', '[Content truncated by browser extractor.]'); break; }
        }
        lines.push('', '## Visible links');
        let n = 0;
        const linkSeen = new Set();
        for (const a of document.querySelectorAll('a[href]')) {
            if (!visible(a)) continue;
            const t = esc(a.innerText || a.textContent);
            if (t.length < 3) continue;
            let u;
            try { u = new URL(a.href); } catch { continue; }
            if (!/^https?:$/.test(u.protocol) || linkSeen.has(u.href)) continue;
            linkSeen.add(u.href);
            lines.push(`- [${t.slice(0, 160)}](${u.href})`);
            if (++n >= 80) break;
        }
        return lines.join('\n');
    })()
    "#
}

/// 构建 Google 搜索结果提取 JS — 借鉴 ds4 `web_extract_search_js`
pub fn build_extract_search_results_js() -> &'static str {
    r#"
    (() => {
        const clean = s => (s || '').replace(/\s+/g, ' ').trim();
        const esc = s => clean(s).replace(/\\/g, '\\\\').replace(/\[/g, '\\[').replace(/\]/g, '\\]').replace(/\n/g, ' ');
        const visible = el => {
            const r = el.getBoundingClientRect();
            const st = getComputedStyle(el);
            return r.width > 0 && r.height > 0 && st.display !== 'none' && st.visibility !== 'hidden' && st.opacity !== '0';
        };
        const bad = h => /(^|\.)google\./.test(h) || /(^|\.)gstatic\./.test(h) || /(^|\.)googleusercontent\./.test(h);
        const lines = ['# Google search results', '', `URL: ${location.href}`, '', '## Visible links'];
        const seen = new Set();
        for (const a of document.querySelectorAll('a[href]')) {
            if (!visible(a)) continue;
            let href = a.href || '';
            try { const u = new URL(href); if (u.pathname === '/url' && u.searchParams.get('q')) href = u.searchParams.get('q'); } catch {}
            let u;
            try { u = new URL(href); } catch { continue; }
            if (!/^https?:$/.test(u.protocol)) continue;
            if (bad(u.hostname)) continue;
            const text = esc(a.innerText || a.textContent);
            if (text.length < 3) continue;
            if (seen.has(u.href)) continue;
            seen.add(u.href);
            lines.push(`- [${text.slice(0, 180)}](${u.href})`);
            if (seen.size >= 20) break;
        }
        lines.push('', '## Text snapshot', clean(document.body.innerText).slice(0, 1200));
        return lines.join('\n');
    })()
    "#
}

/// 构建页面就绪检测 JS — 借鉴 ds4 `web_page_probe`
///
/// 返回 `href\nreadyState\ntextLength` 三行数据
pub fn build_page_probe_js() -> &'static str {
    r#"
    location.href + '\n' + document.readyState + '\n' +
    ((document.body && document.body.innerText) || '').length
    "#
}

/// 构建页面就绪等待条件 JS — 借鉴 ds4 `web_wait_ready`
pub fn build_page_ready_condition_js() -> &'static str {
    r#"
    document.readyState === 'complete' || document.readyState === 'interactive'
    "#
}

// ============================================================================
//  纯函数 — 滚动结果解析 (可测试)
// ============================================================================

/// 解析滚动结果字符串, 提取步数和文本长度
///
/// ds4 的滚动 JS 返回类似 `"scrolled 15 text=12345"` 或 `"scroll skipped hooks=0 text=0"` 的字符串。
/// 本函数解析出步数和文本长度。
///
/// # 示例
///
/// ```
/// use forge::web_tool::parse_scroll_result;
///
/// let (steps, text_len) = parse_scroll_result("scrolled 15 text=12345");
/// assert_eq!(steps, 15);
/// assert_eq!(text_len, 12345);
///
/// let (steps, text_len) = parse_scroll_result("scroll skipped hooks=0 text=0");
/// assert_eq!(steps, 0);
/// assert_eq!(text_len, 0);
/// ```
pub fn parse_scroll_result(result: &str) -> (u32, u64) {
    let steps = result
        .split_whitespace()
        .find_map(|w| w.parse::<u32>().ok())
        .unwrap_or(0);

    let text_len = result
        .split("text=")
        .nth(1)
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0);

    (steps, text_len)
}

/// 判断页面探测结果是否表示页面已就绪 — 借鉴 ds4 `web_wait_navigated_ready`
///
/// 条件:
/// - readyState 为 "complete" 或 "interactive"
/// - textLength > 0
pub fn is_page_ready_from_probe(probe_result: &str) -> bool {
    let lines: Vec<&str> = probe_result.lines().collect();
    if lines.len() < 3 {
        return false;
    }
    let ready = lines[1].trim();
    let text_len: u64 = lines[2].trim().parse().unwrap_or(0);
    (ready == "complete" || ready == "interactive") && text_len > 0
}

// ============================================================================
//  CdpSession 扩展 — Web 工具方法
// ============================================================================

impl CdpSession {
    /// 动态滚动页面 — 借鉴 ds4 `web_scroll_dynamic_page`
    ///
    /// 在当前标签页执行智能滚动, 加载所有懒加载内容。
    /// 适用于需要完整页面内容的场景 (如搜索结果页、文档页)。
    ///
    /// # 行为
    ///
    /// 1. 检测页面是否有可滚动内容和懒加载 hooks
    /// 2. 逐步滚动, 每步等待 900ms 让内容加载
    /// 3. 连续 4 次无变化或到达底部时停止
    /// 4. 最多滚动 28 步
    ///
    /// # 返回
    ///
    /// 返回滚动步数和最终文本长度。
    pub async fn scroll_dynamic_page(&self) -> Result<(u32, u64)> {
        let js = build_scroll_dynamic_page_js();
        let result = self.evaluate_string(js).await?;
        let (steps, text_len) = parse_scroll_result(&result);
        info!("动态滚动完成: {} 步, 文本 {} 字符", steps, text_len);
        Ok((steps, text_len))
    }

    /// 提取页面内容为 Markdown — 借鉴 ds4 `web_extract_page_js`
    ///
    /// 将当前页面的可见内容提取为结构化 Markdown 文本。
    /// 包含标题、段落、代码块、引用和可见链接。
    ///
    /// # 截断
    ///
    /// 内容超过 900KB 时自动截断。
    pub async fn extract_page_content(&self) -> Result<String> {
        let js = build_extract_page_content_js();
        let content = self.evaluate_string(js).await?;
        debug!("页面内容提取: {} 字符", content.len());
        Ok(content)
    }

    /// 提取 Google 搜索结果 — 借鉴 ds4 `web_extract_search_js`
    ///
    /// 从 Google 搜索结果页提取可见链接和文本快照。
    /// 返回 Markdown 格式的搜索结果摘要。
    pub async fn extract_search_results(&self) -> Result<String> {
        let js = build_extract_search_results_js();
        let content = self.evaluate_string(js).await?;
        debug!("搜索结果提取: {} 字符", content.len());
        Ok(content)
    }

    /// 等待页面就绪 — 借鉴 ds4 `web_wait_navigated_ready`
    ///
    /// 在页面导航后等待页面加载完成。
    /// 检测 `readyState` 和文本内容稳定性。
    ///
    /// # 参数
    ///
    /// - `timeout_ms`: 总超时时间 (毫秒)
    /// - `stable_count`: 文本内容连续不变的次数 (默认 2)
    ///
    /// # 返回
    ///
    /// 页面就绪返回 `Ok(())`, 超时返回错误。
    pub async fn wait_page_ready(&self, timeout_ms: u64, stable_count: u32) -> Result<()> {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        let probe_js = build_page_probe_js();
        let mut last_text_len: i64 = -1;
        let mut stable = 0u32;

        for _ in 0..100 {
            if tokio::time::Instant::now() > deadline {
                bail!("页面就绪等待超时 ({}ms)", timeout_ms);
            }

            let probe = self.evaluate_string(probe_js).await;
            match probe {
                Ok(result) if is_page_ready_from_probe(&result) => {
                    let current_len: i64 = result
                        .lines()
                        .nth(2)
                        .and_then(|s| s.trim().parse().ok())
                        .unwrap_or(0);
                    if current_len == last_text_len {
                        stable += 1;
                        if stable >= stable_count {
                            debug!("页面已就绪 (stable={})", stable);
                            return Ok(());
                        }
                    } else {
                        stable = 0;
                    }
                    last_text_len = current_len;
                }
                Ok(_) => {
                    // 页面未就绪, 继续等待
                    stable = 0;
                }
                Err(e) => {
                    debug!("页面探测失败 (非致命): {}", e);
                }
            }

            tokio::time::sleep(Duration::from_millis(250)).await;
        }

        bail!("页面就绪等待失败: 达到最大重试次数")
    }

    /// 导航到 URL 并等待页面就绪 — 借鉴 ds4 `web_cdp_navigate` + `web_wait_navigated_ready`
    ///
    /// 封装了 Page.navigate + wait_page_ready 两步操作。
    pub async fn navigate_and_wait(&self, url: &str, timeout_ms: u64) -> Result<()> {
        info!("CDP 导航到: {}", url);
        self.send_command("Page.navigate", json!({ "url": url }))
            .await?;
        self.wait_page_ready(timeout_ms, 2).await
    }
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // ===== build_scroll_dynamic_page_js 测试 =====

    #[test]
    fn test_build_scroll_dynamic_page_js_not_empty() {
        let js = build_scroll_dynamic_page_js();
        assert!(!js.is_empty());
        assert!(js.contains("scrollTo"));
        assert!(js.contains("Promise"));
    }

    #[test]
    fn test_build_scroll_dynamic_page_js_has_stability_check() {
        let js = build_scroll_dynamic_page_js();
        // 应包含稳定性检测逻辑
        assert!(js.contains("same"));
        assert!(js.contains("atBottom"));
    }

    #[test]
    fn test_build_scroll_dynamic_page_js_has_hook_detection() {
        let js = build_scroll_dynamic_page_js();
        // 应包含懒加载 hook 检测
        assert!(js.contains("hookCount"));
        assert!(js.contains("lazy"));
    }

    // ===== build_extract_page_content_js 测试 =====

    #[test]
    fn test_build_extract_page_content_js_not_empty() {
        let js = build_extract_page_content_js();
        assert!(!js.is_empty());
        assert!(js.contains("Content"));
    }

    #[test]
    fn test_build_extract_page_content_js_has_visible_check() {
        let js = build_extract_page_content_js();
        assert!(js.contains("visible"));
        assert!(js.contains("getBoundingClientRect"));
    }

    #[test]
    fn test_build_extract_page_content_js_has_truncation() {
        let js = build_extract_page_content_js();
        assert!(js.contains("900000"));
        assert!(js.contains("truncated"));
    }

    #[test]
    fn test_build_extract_page_content_js_has_markdown_format() {
        let js = build_extract_page_content_js();
        // 应包含 Markdown 格式化
        assert!(js.contains("```"));
        assert!(js.contains("## "));
    }

    // ===== build_extract_search_results_js 测试 =====

    #[test]
    fn test_build_extract_search_results_js_not_empty() {
        let js = build_extract_search_results_js();
        assert!(!js.is_empty());
        assert!(js.contains("Google search"));
    }

    #[test]
    fn test_build_extract_search_results_js_filters_google_domains() {
        let js = build_extract_search_results_js();
        // 应过滤 google.com 等域名 (JS 中为正则 google\.)
        assert!(js.contains("google") || js.contains("google\\."));
        assert!(js.contains("gstatic"));
    }

    #[test]
    fn test_build_extract_search_results_js_has_link_limit() {
        let js = build_extract_search_results_js();
        assert!(js.contains("20"));
    }

    // ===== build_page_probe_js 测试 =====

    #[test]
    fn test_build_page_probe_js_returns_three_fields() {
        let js = build_page_probe_js();
        assert!(js.contains("location.href"));
        assert!(js.contains("readyState"));
        assert!(js.contains("innerText"));
    }

    // ===== parse_scroll_result 测试 =====

    #[test]
    fn test_parse_scroll_result_scrolled() {
        let (steps, text) = parse_scroll_result("scrolled 15 text=12345");
        assert_eq!(steps, 15);
        assert_eq!(text, 12345);
    }

    #[test]
    fn test_parse_scroll_result_skipped() {
        let (steps, text) = parse_scroll_result("scroll skipped hooks=0 text=0");
        assert_eq!(steps, 0);
        assert_eq!(text, 0);
    }

    #[test]
    fn test_parse_scroll_result_unchanged() {
        let (steps, text) = parse_scroll_result("scroll probe unchanged text=500");
        assert_eq!(steps, 0);
        assert_eq!(text, 500);
    }

    #[test]
    fn test_parse_scroll_result_empty() {
        let (steps, text) = parse_scroll_result("");
        assert_eq!(steps, 0);
        assert_eq!(text, 0);
    }

    #[test]
    fn test_parse_scroll_result_large_numbers() {
        let (steps, text) = parse_scroll_result("scrolled 28 text=999999");
        assert_eq!(steps, 28);
        assert_eq!(text, 999999);
    }

    // ===== is_page_ready_from_probe 测试 =====

    #[test]
    fn test_is_page_ready_complete_with_text() {
        let probe = "https://example.com/page\ncomplete\n12345";
        assert!(is_page_ready_from_probe(probe));
    }

    #[test]
    fn test_is_page_ready_interactive_with_text() {
        let probe = "https://example.com/page\ninteractive\n500";
        assert!(is_page_ready_from_probe(probe));
    }

    #[test]
    fn test_is_page_ready_loading_state() {
        let probe = "https://example.com/page\nloading\n0";
        assert!(!is_page_ready_from_probe(probe));
    }

    #[test]
    fn test_is_page_ready_complete_no_text() {
        let probe = "https://example.com/page\ncomplete\n0";
        assert!(!is_page_ready_from_probe(probe));
    }

    #[test]
    fn test_is_page_ready_malformed() {
        assert!(!is_page_ready_from_probe("only one line"));
        assert!(!is_page_ready_from_probe(""));
    }

    #[test]
    fn test_is_page_ready_about_blank() {
        let probe = "about:blank\ncomplete\n0";
        // about:blank with 0 text is not ready
        assert!(!is_page_ready_from_probe(probe));
    }

    // ===== proptest 属性测试 =====

    #[test]
    fn prop_parse_scroll_result_always_non_negative() {
        proptest!(|(s in r"(scrolled|scroll) \w+ text=\d+")| {
            let (steps, text) = parse_scroll_result(&s);
            prop_assert!(steps <= 28);
            prop_assert!(text <= 999999);
        });
    }
}
