//! 回调解耦数据处理 — 借鉴 MediaCrawler callback 模式
//!
//! 将 Orchestrator 中 AI 回复的处理流程解耦为独立的 handler,
//! 每个 handler 只负责一个职责 (SRP), 通过 handler 链顺序执行。
//!
//! ## 设计
//!
//! - [`ResponseHandler`] trait: 处理 AI 回复的接口 (ISP: 小而精)
//! - [`CodeExtractorHandler`][]: 提取代码文件
//! - [`TraceWriterHandler`][]: 记录开发追踪
//! - [`MemoryUpdaterHandler`][]: 更新记忆
//! - [`HandlerChain`][]: 处理器链, 顺序执行所有 handler
//!
//! ## 示例
//!
//! ```
//! use forge::response_handler::*;
//!
//! let mut chain = HandlerChain::new();
//! chain.add(Box::new(CodeExtractorHandler::new()));
//! chain.add(Box::new(TraceWriterHandler::new()));
//! ```

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

// ============================================================================
//  TaskContext — 任务上下文
// ============================================================================

/// 任务上下文 — 包含处理 AI 回复所需的信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskContext {
    /// 当前阶段 (plan / develop / fix / validate)
    pub phase: String,
    /// 当前任务名称
    pub task_name: String,
    /// 对话轮数
    pub turn: usize,
    /// 工作区路径
    pub workspace: String,
    /// 额外元数据
    pub metadata: std::collections::HashMap<String, String>,
}

impl TaskContext {
    /// 创建新的任务上下文
    pub fn new(phase: &str, task_name: &str, workspace: &str) -> Self {
        Self {
            phase: phase.to_string(),
            task_name: task_name.to_string(),
            turn: 0,
            workspace: workspace.to_string(),
            metadata: std::collections::HashMap::new(),
        }
    }

    /// 设置对话轮数
    pub fn with_turn(mut self, turn: usize) -> Self {
        self.turn = turn;
        self
    }

    /// 添加元数据
    pub fn with_metadata(mut self, key: &str, value: &str) -> Self {
        self.metadata.insert(key.to_string(), value.to_string());
        self
    }
}

// ============================================================================
//  HandlerResult — 处理器返回结果
// ============================================================================

/// 处理器返回结果
#[derive(Debug, Clone)]
pub struct HandlerResult {
    /// 是否继续执行后续 handler
    pub continue_chain: bool,
    /// 提取的文件路径列表 (如果有)
    pub extracted_files: Vec<String>,
    /// 处理器产生的消息
    pub message: Option<String>,
}

impl Default for HandlerResult {
    fn default() -> Self {
        Self {
            continue_chain: true,
            extracted_files: vec![],
            message: None,
        }
    }
}

impl HandlerResult {
    /// 创建一个"继续执行"的结果
    pub fn continue_chain() -> Self {
        Self::default()
    }

    /// 创建一个"停止执行"的结果
    pub fn stop_chain() -> Self {
        Self {
            continue_chain: false,
            ..Default::default()
        }
    }

    /// 创建一个带文件的结果
    pub fn with_files(mut self, files: Vec<String>) -> Self {
        self.extracted_files = files;
        self
    }

    /// 创建一个带消息的结果
    pub fn with_message(mut self, msg: &str) -> Self {
        self.message = Some(msg.to_string());
        self
    }
}

// ============================================================================
//  ResponseHandler — 回调 trait (借鉴 MediaCrawler callback 模式)
// ============================================================================

/// AI 回复处理器 trait — 借鉴 MediaCrawler callback 模式
///
/// 每个 handler 只负责一个职责 (SRP):
/// - `CodeExtractorHandler`: 提取代码文件
/// - `TraceWriterHandler`: 记录开发追踪
/// - `MemoryUpdaterHandler`: 更新记忆
///
/// Orchestrator 只负责获取 AI 回复, 处理通过 handler 链解耦 (OCP)。
#[async_trait]
pub trait ResponseHandler: Send + Sync {
    /// 处理 AI 回复
    ///
    /// # 参数
    /// - `response`: AI 回复文本
    /// - `context`: 任务上下文
    ///
    /// # 返回
    /// [`HandlerResult`] — 包含是否继续执行、提取的文件等信息
    async fn handle(&self, response: &str, context: &TaskContext) -> Result<HandlerResult>;

    /// 处理器名称 (用于日志)
    fn name(&self) -> &str;
}

// ============================================================================
//  CodeExtractorHandler — 代码提取处理器
// ============================================================================

/// 代码提取处理器 — 从 AI 回复中提取代码文件
///
/// 借鉴 MediaCrawler 中数据获取与存储解耦的设计,
/// 将代码提取逻辑从 Orchestrator 中分离。
pub struct CodeExtractorHandler {
    /// 上次提取的文件数量
    last_count: std::sync::atomic::AtomicUsize,
}

impl CodeExtractorHandler {
    pub fn new() -> Self {
        Self {
            last_count: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// 获取上次提取的文件数量
    pub fn last_file_count(&self) -> usize {
        self.last_count.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl Default for CodeExtractorHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ResponseHandler for CodeExtractorHandler {
    async fn handle(&self, response: &str, context: &TaskContext) -> Result<HandlerResult> {
        debug!("CodeExtractorHandler: 处理回复 ({}字符)", response.len());

        // 使用 Forge 的 extract_files 提取代码
        let files = crate::extract::extract_files(response);
        let file_paths: Vec<String> = files.iter().map(|f| f.path.clone()).collect();

        self.last_count
            .store(file_paths.len(), std::sync::atomic::Ordering::Relaxed);

        if file_paths.is_empty() {
            debug!("CodeExtractorHandler: 未找到代码文件");
        } else {
            info!(
                "CodeExtractorHandler: 提取 {} 个文件 (阶段: {}, 任务: {})",
                file_paths.len(),
                context.phase,
                context.task_name
            );
        }

        Ok(HandlerResult::continue_chain().with_files(file_paths))
    }

    fn name(&self) -> &str {
        "CodeExtractor"
    }
}

// ============================================================================
//  TraceWriterHandler — 开发追踪处理器
// ============================================================================

/// 开发追踪处理器 — 记录每轮 AI 交互的详细 trace
///
/// 将 trace 记录逻辑从 Orchestrator 中分离, 通过 handler 链自动执行。
pub struct TraceWriterHandler {
    /// 记录的 trace 条目数
    entry_count: std::sync::atomic::AtomicUsize,
}

impl TraceWriterHandler {
    pub fn new() -> Self {
        Self {
            entry_count: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// 获取已记录的条目数
    pub fn entry_count(&self) -> usize {
        self.entry_count.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl Default for TraceWriterHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ResponseHandler for TraceWriterHandler {
    async fn handle(&self, _response: &str, context: &TaskContext) -> Result<HandlerResult> {
        debug!(
            "TraceWriterHandler: 记录 trace (阶段: {}, 任务: {}, 轮: {})",
            context.phase, context.task_name, context.turn
        );

        self.entry_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        // 实际的 trace 写入由 Orchestrator 的 DevTraceWriter 处理
        // 这里只做计数和日志, 保持 handler 轻量
        Ok(HandlerResult::continue_chain())
    }

    fn name(&self) -> &str {
        "TraceWriter"
    }
}

// ============================================================================
//  MemoryUpdaterHandler — 记忆更新处理器
// ============================================================================

/// 记忆更新处理器 — 更新项目记忆 (已完成任务、错误历史等)
pub struct MemoryUpdaterHandler {
    /// 更新次数
    update_count: std::sync::atomic::AtomicUsize,
}

impl MemoryUpdaterHandler {
    pub fn new() -> Self {
        Self {
            update_count: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub fn update_count(&self) -> usize {
        self.update_count.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl Default for MemoryUpdaterHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ResponseHandler for MemoryUpdaterHandler {
    async fn handle(&self, _response: &str, context: &TaskContext) -> Result<HandlerResult> {
        debug!(
            "MemoryUpdaterHandler: 更新记忆 (阶段: {}, 任务: {})",
            context.phase, context.task_name
        );

        self.update_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        Ok(HandlerResult::continue_chain())
    }

    fn name(&self) -> &str {
        "MemoryUpdater"
    }
}

// ============================================================================
//  HandlerChain — 处理器链 (顺序执行所有 handler)
// ============================================================================

/// 处理器链 — 顺序执行所有注册的 handler
///
/// 如果某个 handler 返回 `continue_chain: false`, 则停止执行后续 handler。
///
/// # 示例
///
/// ```
/// use forge::response_handler::*;
///
/// let mut chain = HandlerChain::new();
/// chain.add(Box::new(CodeExtractorHandler::new()));
/// chain.add(Box::new(TraceWriterHandler::new()));
/// chain.add(Box::new(MemoryUpdaterHandler::new()));
///
/// assert_eq!(chain.len(), 3);
/// ```
pub struct HandlerChain {
    handlers: Vec<Box<dyn ResponseHandler>>,
}

impl HandlerChain {
    /// 创建空的处理器链
    pub fn new() -> Self {
        Self { handlers: vec![] }
    }

    /// 添加处理器
    pub fn add(&mut self, handler: Box<dyn ResponseHandler>) -> &mut Self {
        self.handlers.push(handler);
        self
    }

    /// 处理器数量
    pub fn len(&self) -> usize {
        self.handlers.len()
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }

    /// 顺序执行所有处理器
    ///
    /// 如果某个 handler 返回 `continue_chain: false`, 则停止执行。
    /// 返回所有 handler 的合并结果。
    pub async fn execute(&self, response: &str, context: &TaskContext) -> Result<HandlerResult> {
        let mut combined = HandlerResult::continue_chain();
        let mut all_files = Vec::new();

        for handler in &self.handlers {
            debug!("执行处理器: {}", handler.name());
            let result = handler.handle(response, context).await?;

            // 合并提取的文件
            all_files.extend(result.extracted_files);

            if !result.continue_chain {
                warn!("处理器 {} 返回 stop_chain, 中断链", handler.name());
                combined.continue_chain = false;
                break;
            }
        }

        combined.extracted_files = all_files;
        Ok(combined)
    }

    /// 获取所有处理器名称
    pub fn handler_names(&self) -> Vec<&str> {
        self.handlers.iter().map(|h| h.name()).collect()
    }
}

impl Default for HandlerChain {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ===== TaskContext 测试 =====

    #[test]
    fn test_task_context_new() {
        let ctx = TaskContext::new("develop", "task1", "/workspace");
        assert_eq!(ctx.phase, "develop");
        assert_eq!(ctx.task_name, "task1");
        assert_eq!(ctx.workspace, "/workspace");
        assert_eq!(ctx.turn, 0);
        assert!(ctx.metadata.is_empty());
    }

    #[test]
    fn test_task_context_with_turn() {
        let ctx = TaskContext::new("fix", "task2", "/ws").with_turn(5);
        assert_eq!(ctx.turn, 5);
    }

    #[test]
    fn test_task_context_with_metadata() {
        let ctx = TaskContext::new("plan", "task3", "/ws")
            .with_metadata("key1", "value1")
            .with_metadata("key2", "value2");
        assert_eq!(ctx.metadata.get("key1"), Some(&"value1".to_string()));
        assert_eq!(ctx.metadata.get("key2"), Some(&"value2".to_string()));
    }

    // ===== HandlerResult 测试 =====

    #[test]
    fn test_handler_result_default() {
        let result = HandlerResult::default();
        assert!(result.continue_chain);
        assert!(result.extracted_files.is_empty());
        assert!(result.message.is_none());
    }

    #[test]
    fn test_handler_result_continue_chain() {
        let result = HandlerResult::continue_chain();
        assert!(result.continue_chain);
    }

    #[test]
    fn test_handler_result_stop_chain() {
        let result = HandlerResult::stop_chain();
        assert!(!result.continue_chain);
    }

    #[test]
    fn test_handler_result_with_files() {
        let result = HandlerResult::continue_chain()
            .with_files(vec!["file1.rs".to_string(), "file2.rs".to_string()]);
        assert_eq!(result.extracted_files.len(), 2);
    }

    #[test]
    fn test_handler_result_with_message() {
        let result = HandlerResult::continue_chain().with_message("done");
        assert_eq!(result.message, Some("done".to_string()));
    }

    // ===== CodeExtractorHandler 测试 =====

    #[tokio::test]
    async fn test_code_extractor_handler_no_code() {
        let handler = CodeExtractorHandler::new();
        let ctx = TaskContext::new("develop", "task1", "/ws");
        let result = handler
            .handle("这是一个没有代码的回复", &ctx)
            .await
            .unwrap();
        assert!(result.continue_chain);
        assert!(result.extracted_files.is_empty());
        assert_eq!(handler.last_file_count(), 0);
    }

    #[tokio::test]
    async fn test_code_extractor_handler_with_code() {
        let handler = CodeExtractorHandler::new();
        let ctx = TaskContext::new("develop", "task1", "/ws");
        let response = r#"Here's the code:
```file:src/main.rs
fn main() { println!("hello"); }
```
"#;
        let result = handler.handle(response, &ctx).await.unwrap();
        assert!(result.continue_chain);
        assert!(!result.extracted_files.is_empty());
        assert_eq!(handler.last_file_count(), 1);
    }

    #[tokio::test]
    async fn test_code_extractor_handler_name() {
        let handler = CodeExtractorHandler::new();
        assert_eq!(handler.name(), "CodeExtractor");
    }

    // ===== TraceWriterHandler 测试 =====

    #[tokio::test]
    async fn test_trace_writer_handler() {
        let handler = TraceWriterHandler::new();
        let ctx = TaskContext::new("develop", "task1", "/ws").with_turn(3);
        let result = handler.handle("some response", &ctx).await.unwrap();
        assert!(result.continue_chain);
        assert_eq!(handler.entry_count(), 1);
    }

    #[tokio::test]
    async fn test_trace_writer_handler_multiple() {
        let handler = TraceWriterHandler::new();
        let ctx = TaskContext::new("develop", "task1", "/ws");
        for _ in 0..5 {
            handler.handle("response", &ctx).await.unwrap();
        }
        assert_eq!(handler.entry_count(), 5);
    }

    #[tokio::test]
    async fn test_trace_writer_handler_name() {
        let handler = TraceWriterHandler::new();
        assert_eq!(handler.name(), "TraceWriter");
    }

    // ===== MemoryUpdaterHandler 测试 =====

    #[tokio::test]
    async fn test_memory_updater_handler() {
        let handler = MemoryUpdaterHandler::new();
        let ctx = TaskContext::new("fix", "task1", "/ws");
        let result = handler.handle("response", &ctx).await.unwrap();
        assert!(result.continue_chain);
        assert_eq!(handler.update_count(), 1);
    }

    #[tokio::test]
    async fn test_memory_updater_handler_name() {
        let handler = MemoryUpdaterHandler::new();
        assert_eq!(handler.name(), "MemoryUpdater");
    }

    // ===== HandlerChain 测试 =====

    #[tokio::test]
    async fn test_handler_chain_empty() {
        let chain = HandlerChain::new();
        assert!(chain.is_empty());
        assert_eq!(chain.len(), 0);

        let ctx = TaskContext::new("develop", "task1", "/ws");
        let result = chain.execute("response", &ctx).await.unwrap();
        assert!(result.continue_chain);
    }

    #[tokio::test]
    async fn test_handler_chain_single() {
        let mut chain = HandlerChain::new();
        chain.add(Box::new(TraceWriterHandler::new()));
        assert_eq!(chain.len(), 1);

        let ctx = TaskContext::new("develop", "task1", "/ws");
        let result = chain.execute("response", &ctx).await.unwrap();
        assert!(result.continue_chain);
    }

    #[tokio::test]
    async fn test_handler_chain_multiple() {
        let mut chain = HandlerChain::new();
        chain.add(Box::new(CodeExtractorHandler::new()));
        chain.add(Box::new(TraceWriterHandler::new()));
        chain.add(Box::new(MemoryUpdaterHandler::new()));
        assert_eq!(chain.len(), 3);

        let ctx = TaskContext::new("develop", "task1", "/ws");
        let response = "```file:src/main.rs\nfn main() {}\n```";
        let result = chain.execute(response, &ctx).await.unwrap();
        assert!(result.continue_chain);
        assert!(!result.extracted_files.is_empty());
    }

    #[tokio::test]
    async fn test_handler_chain_names() {
        let mut chain = HandlerChain::new();
        chain.add(Box::new(CodeExtractorHandler::new()));
        chain.add(Box::new(TraceWriterHandler::new()));
        chain.add(Box::new(MemoryUpdaterHandler::new()));

        let names = chain.handler_names();
        assert_eq!(names, vec!["CodeExtractor", "TraceWriter", "MemoryUpdater"]);
    }

    #[tokio::test]
    async fn test_handler_chain_add_returns_self() {
        let mut chain = HandlerChain::new();
        chain
            .add(Box::new(CodeExtractorHandler::new()))
            .add(Box::new(TraceWriterHandler::new()));
        assert_eq!(chain.len(), 2);
    }
}
