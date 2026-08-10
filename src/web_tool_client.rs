//! WebTool 实现 — 基于 CDP 的网页搜索客户端
//!
//! 实现 traits::WebTool trait，使用现有的 web_tool 模块功能
//! 为 Orchestrator 提供 AI 自主网页搜索/文档查阅能力。

use crate::cdp::CdpSession;
use crate::traits::{WebSearchResult, WebTool};
use crate::web_tool;
use anyhow::Result;
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, info, warn};

/// 基于 CDP 的网页工具客户端
///
/// 包装 CdpSession，实现 WebTool trait，
/// 使 Orchestrator 能够在开发流程中自主搜索文档/查阅网页。
#[derive(Clone)]
pub struct CdpWebTool {
    /// CDP 会话 (Arc 包装以支持 Clone)
    session: Arc<CdpSession>,
}

impl CdpWebTool {
    /// 创建新的 CDP WebTool 客户端
    pub fn new(session: CdpSession) -> Self {
        Self {
            session: Arc::new(session),
        }
    }
}

#[async_trait::async_trait]
impl WebTool for CdpWebTool {
    /// 搜索网页内容
    ///
    /// 如果提供了 URL，直接导航到该 URL 并提取内容；
    /// 如果未提供 URL，构造 Google 搜索 URL 并提取搜索结果。
    async fn search_web(&self, query: &str, url: Option<&str>) -> Result<WebSearchResult> {
        let start = Instant::now();
        info!("开始网页搜索: query='{}', url={:?}", query, url);

        let (final_url, content) = if let Some(specific_url) = url {
            // 直接导航到指定 URL
            debug!("导航到指定 URL: {}", specific_url);
            self.session.navigate_and_wait(specific_url, 30_000).await?;
            
            // 动态滚动以加载所有内容
            let _scroll_result = self.session.scroll_dynamic_page().await?;
            
            // 提取页面内容
            let content = self.session.extract_page_content().await?;
            (specific_url.to_string(), content)
        } else {
            // 构造 Google 搜索 URL
            let search_url = format!(
                "https://www.google.com/search?q={}&hl=en",
                url_encode(query)
            );
            debug!("执行 Google 搜索: {}", search_url);
            
            // 导航到搜索结果页
            self.session.navigate_and_wait(&search_url, 30_000).await?;
            
            // 动态滚动加载更多结果
            let _scroll_result = self.session.scroll_dynamic_page().await?;
            
            // 提取搜索结果
            let content = self.session.extract_search_results().await?;
            (search_url, content)
        };

        let duration_ms = start.elapsed().as_millis() as u64;
        info!("网页搜索完成: {}ms, {} 字符", duration_ms, content.len());

        // 记录到 DevTrace (如果启用)
        debug!("网页搜索: query='{}', url='{}', duration={}ms", query, final_url, duration_ms);

        Ok(WebSearchResult {
            content,
            query: query.to_string(),
            duration_ms,
        })
    }

    /// 导航到指定 URL 并提取页面内容
    async fn navigate_and_extract(&self, url: &str) -> Result<String> {
        info!("导航并提取页面内容: {}", url);
        
        // 导航到页面
        self.session.navigate_and_wait(url, 30_000).await?;
        
        // 动态滚动
        let _scroll_result = self.session.scroll_dynamic_page().await?;
        
        // 提取内容
        let content = self.session.extract_page_content().await?;
        
        info!("页面内容提取完成: {} 字符", content.len());
        Ok(content)
    }
}

/// URL 编码辅助函数
fn url_encode(s: &str) -> String {
    s.chars().map(|c| {
        match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            ' ' => "+".to_string(),
            _ => format!("%{:02X}", c as u8),
        }
    }).collect()
}

/// Mock WebTool 实现 — 用于测试
#[derive(Clone, Default)]
pub struct MockWebTool {
    /// 预编程的搜索结果
    search_responses: std::collections::HashMap<String, String>,
}

impl MockWebTool {
    /// 创建新的 Mock WebTool
    pub fn new() -> Self {
        Self {
            search_responses: std::collections::HashMap::new(),
        }
    }

    /// 设置预编程的搜索结果
    pub fn with_response(mut self, query: &str, response: &str) -> Self {
        self.search_responses.insert(query.to_string(), response.to_string());
        self
    }
}

#[async_trait::async_trait]
impl WebTool for MockWebTool {
    async fn search_web(&self, query: &str, url: Option<&str>) -> Result<WebSearchResult> {
        // 模拟网络延迟
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let content = if let Some(specific_url) = url {
            format!(
                "# Mock page content\n\nURL: {}\nQuery: {}\n\nThis is mock content for testing purposes.",
                specific_url, query
            )
        } else {
            self.search_responses
                .get(query)
                .cloned()
                .unwrap_or_else(|| {
                    format!(
                        "# Mock search results\n\nQuery: {}\n\n- [Result 1](https://example.com/1)\n- [Result 2](https://example.com/2)",
                        query
                    )
                })
        };

        Ok(WebSearchResult {
            content,
            query: query.to_string(),
            duration_ms: 100,
        })
    }

    async fn navigate_and_extract(&self, url: &str) -> Result<String> {
        // 模拟网络延迟
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        
        Ok(format!(
            "# Mock page content\n\nURL: {}\n\nThis is mock content for testing purposes.",
            url
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_url_encode() {
        assert_eq!(url_encode("hello world"), "hello+world");
        assert_eq!(url_encode("rust-lang"), "rust-lang");
        assert_eq!(url_encode("c++"), "c%2B%2B");
        assert_eq!(url_encode("测试"), "%E6%B5%8B%E8%AF%95");
    }

    #[tokio::test]
    async fn test_mock_web_tool_search() {
        let tool = MockWebTool::new()
            .with_response("rust", "# Rust documentation\n\nRust is a systems programming language.");

        let result = tool.search_web("rust", None).await.unwrap();
        assert_eq!(result.query, "rust");
        assert!(result.content.contains("Rust documentation"));
        assert_eq!(result.duration_ms, 100);

        let result = tool.search_web("unknown", None).await.unwrap();
        assert!(result.content.contains("Mock search results"));
        assert!(result.content.contains("unknown"));
    }

    #[tokio::test]
    async fn test_mock_web_tool_navigate() {
        let tool = MockWebTool::new();

        let result = tool.navigate_and_extract("https://example.com").await.unwrap();
        assert!(result.contains("Mock page content"));
        assert!(result.contains("https://example.com"));
    }
}