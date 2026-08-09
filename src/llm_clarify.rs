//! 本地 LLM 增强自主追问 — 核心中的核心
//!
//! 使用本地 LLM (如 Ollama) 增强 Agent 的自主提问能力:
//!
//! 1. **LlmClarificationChecker** — 用 LLM 判断 AI 回复是否需要追问,
//!    并生成更自然的追问消息 (替代启发式规则匹配)
//! 2. **HybridClarificationChecker** — 启发式优先 + LLM 兜底,
//!    先用快速规则检测, 未检出时用 LLM 深度判断
//! 3. **OllamaClient** — 通过 HTTP API 调用本地 Ollama
//! 4. **LlmClient trait** — DIP: 抽象 LLM 调用, 测试可注入 Mock
//!
//! ## 架构
//!
//! ```text
//! AI 回复 → HybridClarificationChecker::check()
//!              ├── 1. HeuristicClarificationChecker (快速规则)
//!              │      └── YES → 返回启发式结果
//!              │      └── NO  → 继续
//!              └── 2. LlmClarificationChecker (LLM 深度判断)
//!                     ├── 构建判断 prompt
//!                     ├── 调用 LlmClient::generate() (带重试)
//!                     ├── 解析 NEEDS_CLARIFICATION / OK
//!                     ├── 防循环 (重复检测 + 次数上限)
//!                     └── LLM 不可用 → 优雅降级 (返回 NO)
//! ```
//!
//! ## Ollama API
//!
//! - 默认端点: `http://localhost:11434`
//! - 生成: `POST /api/generate` `{"model":"...", "prompt":"...", "stream":false}`
//! - 健康检查: `GET /api/tags`

use crate::clarify::HeuristicClarificationChecker;
use crate::traits::{ClarificationChecker, ClarificationContext, ClarificationResult};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{debug, warn};

// ============================================================================
//  LlmClient trait — DIP: 抽象 LLM 调用
// ============================================================================

/// LLM 客户端 trait — DIP: 抽象本地 LLM 调用能力
///
/// 实现者:
/// - `OllamaClient` (真实 Ollama HTTP API)
/// - `MockLlmClient` (测试版, 预编程回复)
#[async_trait]
pub trait LlmClient: Send + Sync {
    /// 生成文本 (非流式)
    ///
    /// 发送 prompt 给 LLM, 返回生成的文本。
    async fn generate(&self, prompt: &str) -> Result<String>;

    /// 检查 LLM 是否可用
    async fn is_available(&self) -> bool;
}

// ============================================================================
//  纯逻辑函数 — 可独立测试, 无外部依赖
// ============================================================================

/// LLM 调用失败类型分类
///
/// 用于决定是否重试 LLM 调用:
/// - `ConnectionRefused` / `Timeout` → 可重试 (瞬时故障)
/// - `HttpError` / `ParseError` / `Other` → 不重试 (持续性故障)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmFailureType {
    /// 连接被拒绝 (Ollama 未运行)
    ConnectionRefused,
    /// 请求超时
    Timeout,
    /// HTTP 错误状态码
    HttpError,
    /// 响应解析失败
    ParseError,
    /// 其他错误
    Other,
}

/// 从错误消息分类 LLM 失败类型
///
/// 通过关键词匹配判断失败类型, 用于决定是否重试。
///
/// # 示例
///
/// ```
/// # use forge::llm_clarify::{classify_llm_failure, LlmFailureType};
/// assert_eq!(classify_llm_failure("operation timed out"), LlmFailureType::Timeout);
/// assert_eq!(classify_llm_failure("connection refused"), LlmFailureType::ConnectionRefused);
/// assert_eq!(classify_llm_failure("Ollama 返回错误状态: 500"), LlmFailureType::HttpError);
/// assert_eq!(classify_llm_failure("Ollama 响应解析失败"), LlmFailureType::ParseError);
/// ```
pub fn classify_llm_failure(error_msg: &str) -> LlmFailureType {
    let lower = error_msg.to_lowercase();
    if lower.contains("timed out") || lower.contains("timeout") || lower.contains("deadline") {
        LlmFailureType::Timeout
    } else if lower.contains("connection refused")
        || lower.contains("connect error")
        || lower.contains("error sending request")
        || lower.contains("connection reset")
        || lower.contains("broken pipe")
        || lower.contains("请求失败")
    {
        LlmFailureType::ConnectionRefused
    } else if lower.contains("error status")
        || lower.contains("错误状态")
        || lower.contains("404")
        || lower.contains("500")
        || lower.contains("503")
    {
        LlmFailureType::HttpError
    } else if lower.contains("parse") || lower.contains("json") || lower.contains("解析失败") {
        LlmFailureType::ParseError
    } else {
        LlmFailureType::Other
    }
}

/// 判断是否应该重试 LLM 调用
///
/// - `ConnectionRefused` / `Timeout` → 可重试 (瞬时故障, Ollama 可能正在启动)
/// - `HttpError` / `ParseError` / `Other` → 不重试 (持续性故障)
/// - 超过最大重试次数 → 不重试
///
/// # 示例
///
/// ```
/// # use forge::llm_clarify::{should_retry_llm, LlmFailureType};
/// assert!(should_retry_llm(&LlmFailureType::Timeout, 0, 2));
/// assert!(!should_retry_llm(&LlmFailureType::Timeout, 2, 2));
/// assert!(!should_retry_llm(&LlmFailureType::ParseError, 0, 2));
/// ```
pub fn should_retry_llm(failure_type: &LlmFailureType, attempt: u32, max_retries: u32) -> bool {
    if attempt >= max_retries {
        return false;
    }
    matches!(
        failure_type,
        LlmFailureType::ConnectionRefused | LlmFailureType::Timeout
    )
}

/// 截断过长的回复
///
/// 返回 (截断后的字符串, 是否发生了截断)
///
/// # 示例
///
/// ```
/// # use forge::llm_clarify::truncate_response;
/// let (text, truncated) = truncate_response("hello", 10);
/// assert_eq!(text, "hello");
/// assert!(!truncated);
///
/// let (text, truncated) = truncate_response("hello world", 5);
/// assert_eq!(text, "hello");
/// assert!(truncated);
/// ```
pub fn truncate_response(response: &str, max_chars: usize) -> (String, bool) {
    let char_count = response.chars().count();
    if char_count > max_chars {
        let truncated: String = response.chars().take(max_chars).collect();
        (truncated, true)
    } else {
        (response.to_string(), false)
    }
}

/// 构建 LLM 判断 prompt (纯逻辑)
///
/// 将 AI 回复和上下文信息组装成给 LLM 的判断 prompt。
/// 过长的回复会被截断到 `max_chars` 字符。
pub fn build_judge_prompt_text(
    response: &str,
    context: &ClarificationContext,
    max_chars: usize,
) -> String {
    let (response_preview, was_truncated) = truncate_response(response, max_chars);
    let truncated_note = if was_truncated {
        "\n(回复已截断, 仅显示前部分字符)"
    } else {
        ""
    };

    format!(
        "你是一个自主代码生成助手的质量检查器。\n\
         请分析以下 AI 回复，判断是否需要追问。\n\
         \n\
         任务: {task_prompt}\n\
         AI 回复:\n\
         ---\n\
         {response}\n\
         ---{truncated_note}\n\
         是否超时: {timed_out}\n\
         已追问次数: {questions_asked}/{max_questions}\n\
         \n\
         判断标准:\n\
         - AI 在向用户提问 (如\"你希望用什么框架？\")\n\
         - AI 表达不确定 (如\"有两种方案\")\n\
         - 回复不完整或被截断\n\
         - 回复过短, 没有实质内容\n\
         - 回复中没有代码文件 (缺少 ```file: 路径``` 格式的内容)\n\
         \n\
         如果需要追问, 请按以下格式回答:\n\
         NEEDS_CLARIFICATION: <追问原因>\n\
         FOLLOW_UP: <要发送给AI的追问消息, 要求AI自行决策并直接用file:路径格式输出代码>\n\
         \n\
         如果不需要追问, 请回答:\n\
         OK\n\
         \n\
         只回答以上两种格式之一。",
        task_prompt = context.task_prompt,
        response = response_preview,
        truncated_note = truncated_note,
        timed_out = context.timed_out,
        questions_asked = context.questions_asked,
        max_questions = context.max_questions,
    )
}

/// 解析 LLM 判断结果 (纯逻辑)
///
/// 返回 `Option<(needs_clarification, reason, Option<follow_up_question>)>`
///
/// 支持的格式:
/// - `NEEDS_CLARIFICATION: <原因>\nFOLLOW_UP: <追问消息>` → `Some((true, reason, Some(follow_up)))`
/// - `NEEDS_CLARIFICATION: <原因>` → `Some((true, reason, None))`
/// - `OK` → `Some((false, "", None))`
/// - 其他 → `None` (无法解析)
pub fn parse_llm_judge_result(llm_response: &str) -> Option<(bool, String, Option<String>)> {
    let trimmed = llm_response.trim();
    if trimmed.is_empty() {
        return None;
    }

    // 检查是否以 "NEEDS_CLARIFICATION" 开头 (大小写不敏感)
    if trimmed.to_uppercase().starts_with("NEEDS_CLARIFICATION") {
        // 提取原因
        let after_marker = &trimmed["NEEDS_CLARIFICATION".len()..];
        let reason = after_marker.trim_start_matches(&[':', ' ', '：'][..]);

        // 查找 FOLLOW_UP 行
        let follow_up = trimmed
            .lines()
            .find(|line| line.trim().to_uppercase().starts_with("FOLLOW_UP"))
            .map(|line| {
                let after = &line["FOLLOW_UP".len()..];
                after
                    .trim_start_matches(&[':', ' ', '：'][..])
                    .trim()
                    .to_string()
            })
            .filter(|s| !s.is_empty());

        let reason_str = if reason.is_empty() {
            "LLM 判断需要追问".to_string()
        } else {
            // 只取第一行作为原因 (避免包含 FOLLOW_UP)
            reason.lines().next().unwrap_or(reason).trim().to_string()
        };

        return Some((true, reason_str, follow_up));
    }

    // 检查是否以 "OK" 开头
    if trimmed.to_uppercase().starts_with("OK") {
        return Some((false, String::new(), None));
    }

    // 无法解析的格式
    None
}

/// 构建默认追问消息 (纯逻辑)
///
/// 当 LLM 判断需要追问但未提供 FOLLOW_UP 消息时使用。
pub fn build_default_follow_up_message(reason: &str) -> String {
    format!(
        "请根据项目需求自行选择最合适的方案，并直接用 ```file:路径``` 格式输出所有代码文件。\n\
         不要再提问，直接做出最佳决策并开始编码。\n\
         （LLM 检测到需要澄清的原因: {}）",
        reason
    )
}

/// 检测追问是否与之前的问题重复 (纯逻辑)
///
/// 通过比较前 `prefix_len` 个字符来判断是否重复。
pub fn is_duplicate_question(question: &str, previous: &[String], prefix_len: usize) -> bool {
    let prefix: String = question.chars().take(prefix_len).collect();
    for prev in previous {
        let prev_prefix: String = prev.chars().take(prefix_len).collect();
        if prefix == prev_prefix {
            return true;
        }
    }
    false
}

// ============================================================================
//  OllamaClient — 真实 Ollama HTTP API 客户端
// ============================================================================

/// Ollama API 请求体
#[derive(Serialize)]
struct OllamaGenerateRequest {
    model: String,
    prompt: String,
    stream: bool,
    options: OllamaOptions,
}

/// Ollama 生成参数
#[derive(Serialize)]
struct OllamaOptions {
    /// 温度 (0 = 确定性, 1 = 创造性)
    temperature: f64,
    /// 最大 token 数
    num_predict: i32,
}

/// Ollama API 响应体
#[derive(Deserialize)]
struct OllamaGenerateResponse {
    response: String,
    #[allow(dead_code)]
    done: bool,
}

/// Ollama HTTP 客户端 — 通过本地 HTTP API 调用 Ollama
///
/// 默认端点: `http://localhost:11434`
/// 推荐模型: `qwen2.5:3b` (小模型, 快速判断)
pub struct OllamaClient {
    endpoint: String,
    model: String,
    client: reqwest::Client,
    timeout: Duration,
}

impl OllamaClient {
    /// 创建 Ollama 客户端
    ///
    /// - `endpoint`: Ollama API 地址 (如 `http://localhost:11434`)
    /// - `model`: 模型名称 (如 `qwen2.5:3b`)
    pub fn new(endpoint: &str, model: &str) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        Self {
            endpoint: endpoint.trim_end_matches('/').to_string(),
            model: model.to_string(),
            client,
            timeout: Duration::from_secs(30),
        }
    }

    /// 设置超时时间
    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout = Duration::from_secs(secs);
        self.client = reqwest::Client::builder()
            .timeout(self.timeout)
            .build()
            .unwrap_or_default();
        self
    }

    /// 默认配置: localhost:11434, qwen2.5:3b
    pub fn default_local() -> Self {
        Self::new("http://localhost:11434", "qwen2.5:3b")
    }
}

#[async_trait]
impl LlmClient for OllamaClient {
    async fn generate(&self, prompt: &str) -> Result<String> {
        let url = format!("{}/api/generate", self.endpoint);
        let req = OllamaGenerateRequest {
            model: self.model.clone(),
            prompt: prompt.to_string(),
            stream: false,
            options: OllamaOptions {
                temperature: 0.1, // 低温度 = 更确定性
                num_predict: 256, // 短回复足够判断
            },
        };

        debug!("Ollama 请求: {} (模型: {})", url, self.model);

        let resp = self
            .client
            .post(&url)
            .json(&req)
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|e| anyhow!("Ollama 请求失败: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Ollama 返回错误状态: {} - {}", status, body));
        }

        let result: OllamaGenerateResponse = resp
            .json()
            .await
            .map_err(|e| anyhow!("Ollama 响应解析失败: {}", e))?;

        debug!("Ollama 响应: {}字符", result.response.len());
        Ok(result.response)
    }

    async fn is_available(&self) -> bool {
        let url = format!("{}/api/tags", self.endpoint);
        match self.client.get(&url).timeout(self.timeout).send().await {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        }
    }
}

// ============================================================================
//  LlmClarificationChecker — LLM 增强版澄清检查器
// ============================================================================

/// LLM 增强澄清检查器 — 使用本地 LLM 判断 AI 回复是否需要追问
///
/// **核心中的核心** — 用 LLM 替代启发式规则:
/// - 更准确地检测隐晦的提问/不确定表达
/// - 生成更自然的追问消息 (而非模板)
/// - LLM 不可用时优雅降级 (返回"不需要追问")
/// - 瞬时故障 (连接拒绝/超时) 自动重试
///
/// 泛型参数 `C: LlmClient` 支持 DIP — 测试时可注入 MockLlmClient。
pub struct LlmClarificationChecker<C: LlmClient> {
    /// LLM 客户端 (OllamaClient / MockLlmClient)
    client: C,
    /// 最大重试次数 (对瞬时故障: 连接拒绝/超时), 默认 2
    max_retries: u32,
}

impl<C: LlmClient> LlmClarificationChecker<C> {
    /// 创建 LLM 澄清检查器
    ///
    /// 默认最大重试次数为 2 (对瞬时故障)。
    pub fn new(client: C) -> Self {
        Self {
            client,
            max_retries: 2,
        }
    }

    /// 设置最大重试次数 (对瞬时故障: 连接拒绝/超时)
    ///
    /// 设为 0 表示不重试。
    pub fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    /// 带 LLM 重试的生成调用
    ///
    /// 对瞬时故障 (连接拒绝/超时) 进行重试, 最多 `max_retries` 次。
    /// 持续性故障 (HTTP错误/解析错误) 不重试。
    async fn generate_with_retry(&self, prompt: &str) -> Result<String> {
        let mut last_error_msg = "未知错误".to_string();

        for attempt in 0..=self.max_retries {
            match self.client.generate(prompt).await {
                Ok(text) => return Ok(text),
                Err(e) => {
                    last_error_msg = e.to_string();
                    let failure_type = classify_llm_failure(&last_error_msg);

                    if !should_retry_llm(&failure_type, attempt, self.max_retries) {
                        debug!(
                            "LLM 调用失败 (不重试): attempt={}, type={:?}, error={}",
                            attempt, failure_type, e
                        );
                        return Err(e);
                    }

                    debug!(
                        "LLM 调用失败 (将重试): attempt={}/{}, type={:?}, error={}",
                        attempt, self.max_retries, failure_type, e
                    );
                }
            }
        }

        Err(anyhow!(last_error_msg))
    }

    /// 构建给 LLM 的判断 prompt
    ///
    /// 要求 LLM 分析 AI 回复, 判断是否需要追问,
    /// 并在需要时生成追问消息。
    fn build_judge_prompt(&self, response: &str, context: &ClarificationContext) -> String {
        build_judge_prompt_text(response, context, 2000)
    }

    /// 解析 LLM 的判断结果
    ///
    /// 返回 `Option<(needs_clarification, reason, Option<follow_up_question>)>`
    fn parse_judge_result(&self, llm_response: &str) -> Option<(bool, String, Option<String>)> {
        parse_llm_judge_result(llm_response)
    }

    /// 构建默认追问消息 (当 LLM 未提供 FOLLOW_UP 时使用)
    fn build_default_follow_up(&self, reason: &str) -> String {
        build_default_follow_up_message(reason)
    }

    /// 检测追问是否与之前的问题重复
    fn is_duplicate(&self, question: &str, previous: &[String]) -> bool {
        is_duplicate_question(question, previous, 30)
    }
}

#[async_trait]
impl<C: LlmClient> ClarificationChecker for LlmClarificationChecker<C> {
    async fn check(&self, response: &str, context: &ClarificationContext) -> ClarificationResult {
        // 防循环: 超过最大追问次数
        if !context.can_ask_more() {
            return ClarificationResult::no();
        }

        // 构建 LLM 判断 prompt
        let judge_prompt = self.build_judge_prompt(response, context);

        // 调用 LLM (带重试)
        let llm_response = match self.generate_with_retry(&judge_prompt).await {
            Ok(text) => text,
            Err(e) => {
                warn!("LLM 判断失败 (重试耗尽), 优雅降级 (不追问): {}", e);
                return ClarificationResult::no();
            }
        };

        // 解析 LLM 判断结果
        match self.parse_judge_result(&llm_response) {
            Some((true, reason, follow_up_opt)) => {
                let question =
                    follow_up_opt.unwrap_or_else(|| self.build_default_follow_up(&reason));

                // 防循环: 重复检测
                if self.is_duplicate(&question, &context.previous_questions) {
                    debug!("LLM 追问与之前重复, 跳过");
                    return ClarificationResult::no();
                }

                ClarificationResult::yes(question, format!("LLM 判断: {}", reason))
            }
            Some((false, _, _)) => ClarificationResult::no(),
            None => {
                // 无法解析 LLM 输出, 优雅降级
                warn!("LLM 输出无法解析, 优雅降级 (不追问)");
                ClarificationResult::no()
            }
        }
    }
}

// ============================================================================
//  HybridClarificationChecker — 启发式优先 + LLM 兜底
// ============================================================================

/// 混合澄清检查器 — 启发式优先 + LLM 兜底
///
/// 策略:
/// 1. 先运行启发式规则 (快速, 无网络开销)
/// 2. 启发式检测到问题 → 直接返回 (快速路径)
/// 3. 启发式未检测到 → 运行 LLM 深度判断 (慢速路径)
/// 4. LLM 不可用 → 优雅降级 (返回"不需要追问")
///
/// 这种策略兼顾速度和准确性:
/// - 明显的提问/超时/过短 → 启发式秒判
/// - 隐晦的疑问/不确定 → LLM 深度分析
pub struct HybridClarificationChecker<C: LlmClient> {
    /// 启发式检查器 (快速路径)
    heuristic: HeuristicClarificationChecker,
    /// LLM 检查器 (慢速路径)
    llm: LlmClarificationChecker<C>,
}

impl<C: LlmClient> HybridClarificationChecker<C> {
    /// 创建混合检查器
    pub fn new(client: C) -> Self {
        Self {
            heuristic: HeuristicClarificationChecker::new(),
            llm: LlmClarificationChecker::new(client),
        }
    }

    /// 设置启发式的最小回复长度阈值
    pub fn with_min_response_len(mut self, len: usize) -> Self {
        self.heuristic = self.heuristic.with_min_response_len(len);
        self
    }

    /// 设置 LLM 的最大重试次数
    pub fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.llm = self.llm.with_max_retries(max_retries);
        self
    }
}

#[async_trait]
impl<C: LlmClient> ClarificationChecker for HybridClarificationChecker<C> {
    async fn check(&self, response: &str, context: &ClarificationContext) -> ClarificationResult {
        // 防循环: 超过最大追问次数
        if !context.can_ask_more() {
            return ClarificationResult::no();
        }

        // 1. 启发式快速检查
        let heuristic_result = self.heuristic.check(response, context).await;
        if heuristic_result.needs_clarification {
            debug!("启发式检测到需要追问 (快速路径)");
            return heuristic_result;
        }

        // 2. LLM 深度检查 (启发式未检测到问题时)
        debug!("启发式未检测到问题, 启动 LLM 深度判断");
        self.llm.check(response, context).await
    }
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Mock LLM 客户端 — 按顺序返回预编程回复
    struct MockLlmClient {
        responses: Arc<Mutex<Vec<String>>>,
        available: bool,
    }

    impl MockLlmClient {
        fn new(responses: Vec<String>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses)),
                available: true,
            }
        }

        fn single(response: &str) -> Self {
            Self::new(vec![response.to_string()])
        }

        fn unavailable() -> Self {
            Self {
                responses: Arc::new(Mutex::new(vec![])),
                available: false,
            }
        }
    }

    #[async_trait]
    impl LlmClient for MockLlmClient {
        async fn generate(&self, _prompt: &str) -> Result<String> {
            if !self.available {
                return Err(anyhow!("Mock LLM 不可用"));
            }
            let mut queue = self.responses.lock().unwrap();
            if queue.is_empty() {
                return Ok("OK".to_string());
            }
            Ok(queue.remove(0))
        }

        async fn is_available(&self) -> bool {
            self.available
        }
    }

    /// Mock LLM 客户端 — 按顺序返回结果 (成功或失败), 用于测试重试逻辑
    struct SequenceLlmClient {
        results: Arc<Mutex<Vec<Result<String, String>>>>,
        call_count: Arc<Mutex<u32>>,
    }

    impl SequenceLlmClient {
        fn new(results: Vec<Result<String, String>>) -> Self {
            Self {
                results: Arc::new(Mutex::new(results)),
                call_count: Arc::new(Mutex::new(0)),
            }
        }

        #[allow(dead_code)]
        fn call_count(&self) -> u32 {
            *self.call_count.lock().unwrap()
        }
    }

    #[async_trait]
    impl LlmClient for SequenceLlmClient {
        async fn generate(&self, _prompt: &str) -> Result<String> {
            let mut count = self.call_count.lock().unwrap();
            *count += 1;
            drop(count);

            let mut queue = self.results.lock().unwrap();
            if queue.is_empty() {
                return Ok("OK".to_string());
            }
            let result = queue.remove(0);
            match result {
                Ok(text) => Ok(text),
                Err(e) => Err(anyhow!(e)),
            }
        }

        async fn is_available(&self) -> bool {
            true
        }
    }

    /// 创建默认上下文 (0 次追问, 最大 2 次)
    fn ctx() -> ClarificationContext {
        ClarificationContext {
            task_prompt: "创建一个 Rust CLI 工具".to_string(),
            timed_out: false,
            questions_asked: 0,
            max_questions: 2,
            previous_questions: vec![],
        }
    }

    /// 创建已追问 2 次的上下文 (达到上限)
    fn ctx_maxed() -> ClarificationContext {
        ClarificationContext {
            task_prompt: "test".to_string(),
            timed_out: false,
            questions_asked: 2,
            max_questions: 2,
            previous_questions: vec!["q1".to_string(), "q2".to_string()],
        }
    }

    // ===== 纯逻辑函数测试: truncate_response =====

    #[test]
    fn test_truncate_response_short_no_truncation() {
        let (text, truncated) = truncate_response("hello", 10);
        assert_eq!(text, "hello");
        assert!(!truncated);
    }

    #[test]
    fn test_truncate_response_exact_boundary() {
        let (text, truncated) = truncate_response("hello", 5);
        assert_eq!(text, "hello");
        assert!(!truncated, "恰好等于 max_chars 不应截断");
    }

    #[test]
    fn test_truncate_response_one_over() {
        let (text, truncated) = truncate_response("hello!", 5);
        assert_eq!(text, "hello");
        assert!(truncated, "超过 max_chars 应截断");
    }

    #[test]
    fn test_truncate_response_empty_string() {
        let (text, truncated) = truncate_response("", 10);
        assert_eq!(text, "");
        assert!(!truncated);
    }

    #[test]
    fn test_truncate_response_unicode() {
        // 中文字符: 每个字符是一个 char, 但多个字节
        let (text, truncated) = truncate_response("你好世界测试", 4);
        assert_eq!(text, "你好世界");
        assert!(truncated);
    }

    #[test]
    fn test_truncate_response_zero_max() {
        let (text, truncated) = truncate_response("hello", 0);
        assert_eq!(text, "");
        assert!(truncated, "max_chars=0 应截断非空字符串");
    }

    #[test]
    fn test_truncate_response_zero_max_empty() {
        let (text, truncated) = truncate_response("", 0);
        assert_eq!(text, "");
        assert!(!truncated, "空字符串 + max_chars=0 不应截断");
    }

    // ===== 纯逻辑函数测试: classify_llm_failure =====

    #[test]
    fn test_classify_connection_refused() {
        assert_eq!(
            classify_llm_failure("Ollama 请求失败: error sending request"),
            LlmFailureType::ConnectionRefused
        );
        assert_eq!(
            classify_llm_failure("connection refused"),
            LlmFailureType::ConnectionRefused
        );
        assert_eq!(
            classify_llm_failure("connection reset by peer"),
            LlmFailureType::ConnectionRefused
        );
    }

    #[test]
    fn test_classify_timeout() {
        assert_eq!(
            classify_llm_failure("operation timed out"),
            LlmFailureType::Timeout
        );
        assert_eq!(
            classify_llm_failure("request timeout"),
            LlmFailureType::Timeout
        );
        assert_eq!(
            classify_llm_failure("deadline exceeded"),
            LlmFailureType::Timeout
        );
    }

    #[test]
    fn test_classify_http_error() {
        assert_eq!(
            classify_llm_failure("Ollama 返回错误状态: 500"),
            LlmFailureType::HttpError
        );
        assert_eq!(
            classify_llm_failure("error status 404"),
            LlmFailureType::HttpError
        );
        assert_eq!(
            classify_llm_failure("Service Unavailable 503"),
            LlmFailureType::HttpError
        );
    }

    #[test]
    fn test_classify_parse_error() {
        assert_eq!(
            classify_llm_failure("Ollama 响应解析失败"),
            LlmFailureType::ParseError
        );
        assert_eq!(
            classify_llm_failure("json parse error"),
            LlmFailureType::ParseError
        );
    }

    #[test]
    fn test_classify_other() {
        assert_eq!(
            classify_llm_failure("some unknown error"),
            LlmFailureType::Other
        );
    }

    #[test]
    fn test_classify_empty_string() {
        assert_eq!(classify_llm_failure(""), LlmFailureType::Other);
    }

    #[test]
    fn test_classify_case_insensitive() {
        assert_eq!(classify_llm_failure("TIMED OUT"), LlmFailureType::Timeout);
        assert_eq!(
            classify_llm_failure("CONNECTION REFUSED"),
            LlmFailureType::ConnectionRefused
        );
    }

    // ===== 纯逻辑函数测试: should_retry_llm =====

    #[test]
    fn test_retry_timeout_within_limit() {
        assert!(should_retry_llm(&LlmFailureType::Timeout, 0, 2));
        assert!(should_retry_llm(&LlmFailureType::Timeout, 1, 2));
    }

    #[test]
    fn test_retry_timeout_at_limit() {
        assert!(!should_retry_llm(&LlmFailureType::Timeout, 2, 2));
        assert!(!should_retry_llm(&LlmFailureType::Timeout, 3, 2));
    }

    #[test]
    fn test_retry_connection_refused_within_limit() {
        assert!(should_retry_llm(&LlmFailureType::ConnectionRefused, 0, 3));
    }

    #[test]
    fn test_retry_connection_refused_at_limit() {
        assert!(!should_retry_llm(&LlmFailureType::ConnectionRefused, 3, 3));
    }

    #[test]
    fn test_no_retry_http_error() {
        assert!(!should_retry_llm(&LlmFailureType::HttpError, 0, 5));
    }

    #[test]
    fn test_no_retry_parse_error() {
        assert!(!should_retry_llm(&LlmFailureType::ParseError, 0, 5));
    }

    #[test]
    fn test_no_retry_other() {
        assert!(!should_retry_llm(&LlmFailureType::Other, 0, 5));
    }

    #[test]
    fn test_retry_zero_max_retries() {
        // max_retries = 0 → 不重试任何类型
        assert!(!should_retry_llm(&LlmFailureType::Timeout, 0, 0));
        assert!(!should_retry_llm(&LlmFailureType::ConnectionRefused, 0, 0));
    }

    // ===== 纯逻辑函数测试: parse_llm_judge_result =====

    #[test]
    fn test_parse_empty_string() {
        assert!(parse_llm_judge_result("").is_none());
    }

    #[test]
    fn test_parse_only_whitespace() {
        assert!(parse_llm_judge_result("   \n\t  ").is_none());
    }

    #[test]
    fn test_parse_needs_clarification_with_follow_up() {
        let result =
            parse_llm_judge_result("NEEDS_CLARIFICATION: AI 在提问\nFOLLOW_UP: 请直接输出代码。");

        assert!(result.is_some());
        let (needs, reason, follow_up) = result.unwrap();
        assert!(needs);
        assert_eq!(reason, "AI 在提问");
        assert_eq!(follow_up, Some("请直接输出代码。".to_string()));
    }

    #[test]
    fn test_parse_ok() {
        let result = parse_llm_judge_result("OK");

        assert!(result.is_some());
        let (needs, _, _) = result.unwrap();
        assert!(!needs);
    }

    #[test]
    fn test_parse_unparsable() {
        assert!(parse_llm_judge_result("随便说点什么").is_none());
    }

    #[test]
    fn test_parse_case_insensitive_marker() {
        let result = parse_llm_judge_result("needs_clarification: test\nfollow_up: test");

        assert!(result.is_some());
        let (needs, _, _) = result.unwrap();
        assert!(needs);
    }

    #[test]
    fn test_parse_needs_clarification_without_follow_up() {
        let result = parse_llm_judge_result("NEEDS_CLARIFICATION: 回复不完整");

        assert!(result.is_some());
        let (needs, reason, follow_up) = result.unwrap();
        assert!(needs);
        assert_eq!(reason, "回复不完整");
        assert!(follow_up.is_none());
    }

    #[test]
    fn test_parse_needs_clarification_no_colon() {
        // 没有冒号也能匹配, reason 为默认值
        let result = parse_llm_judge_result("NEEDS_CLARIFICATION");

        assert!(result.is_some());
        let (needs, reason, follow_up) = result.unwrap();
        assert!(needs);
        assert_eq!(reason, "LLM 判断需要追问");
        assert!(follow_up.is_none());
    }

    #[test]
    fn test_parse_needs_clarification_empty_reason() {
        // 冒号后为空
        let result = parse_llm_judge_result("NEEDS_CLARIFICATION: ");

        assert!(result.is_some());
        let (needs, reason, _) = result.unwrap();
        assert!(needs);
        assert_eq!(reason, "LLM 判断需要追问");
    }

    #[test]
    fn test_parse_follow_up_empty_value_filtered() {
        // FOLLOW_UP 后为空 → 应被过滤为 None
        let result = parse_llm_judge_result("NEEDS_CLARIFICATION: test\nFOLLOW_UP: ");

        assert!(result.is_some());
        let (needs, _, follow_up) = result.unwrap();
        assert!(needs);
        assert!(follow_up.is_none(), "空的 FOLLOW_UP 应被过滤");
    }

    #[test]
    fn test_parse_multiple_follow_ups_takes_first() {
        let result = parse_llm_judge_result(
            "NEEDS_CLARIFICATION: test\n\
             FOLLOW_UP: 第一个追问\n\
             FOLLOW_UP: 第二个追问",
        );

        assert!(result.is_some());
        let (_, _, follow_up) = result.unwrap();
        assert_eq!(follow_up, Some("第一个追问".to_string()));
    }

    #[test]
    fn test_parse_chinese_colon() {
        // 中文冒号也应被正确处理
        let result =
            parse_llm_judge_result("NEEDS_CLARIFICATION：中文冒号测试\nFOLLOW_UP：追问消息");

        assert!(result.is_some());
        let (needs, reason, follow_up) = result.unwrap();
        assert!(needs);
        assert_eq!(reason, "中文冒号测试");
        assert_eq!(follow_up, Some("追问消息".to_string()));
    }

    #[test]
    fn test_parse_ok_with_trailing_text() {
        // "OK" 开头即可, 后面的文本被忽略
        let result = parse_llm_judge_result("OK, 一切正常");

        assert!(result.is_some());
        let (needs, _, _) = result.unwrap();
        assert!(!needs);
    }

    #[test]
    fn test_parse_multiline_reason() {
        // 多行原因, 只取第一行
        let result = parse_llm_judge_result(
            "NEEDS_CLARIFICATION: 第一行原因\n第二行不该出现\nFOLLOW_UP: test",
        );

        assert!(result.is_some());
        let (_, reason, _) = result.unwrap();
        assert_eq!(reason, "第一行原因");
    }

    // ===== 纯逻辑函数测试: build_default_follow_up_message =====

    #[test]
    fn test_default_follow_up_normal() {
        let msg = build_default_follow_up_message("AI 在提问");
        assert!(msg.contains("file:路径"));
        assert!(msg.contains("AI 在提问"));
        assert!(msg.contains("不要再提问"));
    }

    #[test]
    fn test_default_follow_up_empty_reason() {
        let msg = build_default_follow_up_message("");
        assert!(msg.contains("file:路径"), "空原因也应包含代码格式要求");
    }

    #[test]
    fn test_default_follow_up_long_reason() {
        let long_reason = "这是一个非常长的原因".repeat(50);
        let msg = build_default_follow_up_message(&long_reason);
        assert!(msg.contains(&long_reason), "长原因应完整包含");
    }

    #[test]
    fn test_default_follow_up_contains_code_instruction() {
        let msg = build_default_follow_up_message("test");
        assert!(
            msg.contains("file:路径"),
            "默认追问应包含 file:路径 格式要求"
        );
        assert!(msg.contains("直接做出最佳决策"), "默认追问应包含决策要求");
    }

    // ===== 纯逻辑函数测试: is_duplicate_question =====

    #[test]
    fn test_duplicate_empty_previous() {
        assert!(!is_duplicate_question("test", &[], 30));
    }

    #[test]
    fn test_duplicate_exact_match() {
        let previous = vec!["请直接选择最适合的框架并输出代码。".to_string()];
        assert!(is_duplicate_question(
            "请直接选择最适合的框架并输出代码。",
            &previous,
            30
        ));
    }

    #[test]
    fn test_duplicate_prefix_match() {
        // 前 10 字符相同即视为重复
        let q1 = "请直接选择最适合的框架并输出代码，不要犹豫。";
        let q2 = "请直接选择最适合的框架并输出代码，马上开始。";
        let previous = vec![q1.to_string()];
        assert!(is_duplicate_question(q2, &previous, 10));
    }

    #[test]
    fn test_duplicate_different_prefix() {
        let q1 = "请选择方案A并输出代码。";
        let q2 = "请选择方案B并输出代码。";
        let previous = vec![q1.to_string()];
        // 前 30 字符不同 (方案A vs 方案B 在第 5 个字符就不同了)
        assert!(!is_duplicate_question(q2, &previous, 30));
    }

    #[test]
    fn test_duplicate_empty_question() {
        let previous = vec!["".to_string()];
        // 空问题 + 空前缀 → 重复
        assert!(is_duplicate_question("", &previous, 30));
    }

    #[test]
    fn test_duplicate_short_question() {
        // 问题比 prefix_len 短
        let previous = vec!["ab".to_string()];
        assert!(is_duplicate_question("ab", &previous, 30));
        assert!(!is_duplicate_question("cd", &previous, 30));
    }

    #[test]
    fn test_duplicate_multiple_previous() {
        let previous = vec![
            "第一个问题".to_string(),
            "第二个问题".to_string(),
            "第三个问题".to_string(),
        ];
        // 匹配第二个
        assert!(is_duplicate_question("第二个问题xyz", &previous, 5));
        // 都不匹配
        assert!(!is_duplicate_question("第四个问题", &previous, 5));
    }

    #[test]
    fn test_duplicate_unicode_prefix() {
        let q1 = "你好世界这是第一个问题";
        let q2 = "你好世界这是第二个问题";
        let previous = vec![q1.to_string()];
        // 前 7 个字符 "你好世界这是第" 相同
        assert!(is_duplicate_question(q2, &previous, 7));
        // 前 8 个字符不同 (第一 vs 第二)
        assert!(!is_duplicate_question(q2, &previous, 8));
    }

    // ===== 纯逻辑函数测试: build_judge_prompt_text =====

    #[test]
    fn test_prompt_contains_task_and_response() {
        let context = ctx();
        let prompt = build_judge_prompt_text("这是 AI 回复", &context, 2000);

        assert!(
            prompt.contains("创建一个 Rust CLI 工具"),
            "prompt 应包含任务"
        );
        assert!(prompt.contains("这是 AI 回复"), "prompt 应包含回复");
        assert!(prompt.contains("NEEDS_CLARIFICATION"), "prompt 应说明格式");
        assert!(prompt.contains("FOLLOW_UP"), "prompt 应要求 FOLLOW_UP");
    }

    #[test]
    fn test_prompt_truncation_marker() {
        let long_response = "a".repeat(3000);
        let context = ctx();
        let prompt = build_judge_prompt_text(&long_response, &context, 2000);

        assert!(prompt.contains("已截断"), "长回复应标记为已截断");
    }

    #[test]
    fn test_prompt_no_truncation_marker() {
        let context = ctx();
        let prompt = build_judge_prompt_text("短回复", &context, 2000);

        assert!(!prompt.contains("已截断"), "短回复不应有截断标记");
    }

    #[test]
    fn test_prompt_contains_context_fields() {
        let context = ClarificationContext {
            task_prompt: "测试任务".to_string(),
            timed_out: true,
            questions_asked: 1,
            max_questions: 3,
            previous_questions: vec![],
        };
        let prompt = build_judge_prompt_text("response", &context, 2000);

        assert!(prompt.contains("测试任务"), "应包含 task_prompt");
        assert!(prompt.contains("true"), "应包含 timed_out");
        assert!(
            prompt.contains("1/3"),
            "应包含 questions_asked/max_questions"
        );
    }

    #[test]
    fn test_prompt_empty_response() {
        let context = ctx();
        let prompt = build_judge_prompt_text("", &context, 2000);

        assert!(
            prompt.contains("NEEDS_CLARIFICATION"),
            "空回复也应构建有效 prompt"
        );
        assert!(!prompt.contains("已截断"), "空回复不应有截断标记");
    }

    // ===== LlmClarificationChecker: 基本 LLM 判断 =====

    #[tokio::test]
    async fn test_llm_detects_needs_clarification() {
        let client = MockLlmClient::single(
            "NEEDS_CLARIFICATION: AI 在询问框架选择\n\
             FOLLOW_UP: 请直接选择最适合的框架并输出代码。",
        );
        let checker = LlmClarificationChecker::new(client);

        let result = checker.check("你希望用哪种框架？", &ctx()).await;

        assert!(result.needs_clarification, "LLM 判断需要追问");
        assert!(result.reason.contains("框架选择"));
        assert!(result.question.contains("直接选择"));
    }

    #[tokio::test]
    async fn test_llm_says_ok_no_clarification() {
        let client = MockLlmClient::single("OK");
        let checker = LlmClarificationChecker::new(client);

        let response = "好的，我来创建项目。\n```file:src/main.rs\nfn main() {}\n```";
        let result = checker.check(response, &ctx()).await;

        assert!(!result.needs_clarification, "LLM 判断不需要追问");
    }

    #[tokio::test]
    async fn test_llm_detects_subtle_question_heuristic_missed() {
        let client = MockLlmClient::single(
            "NEEDS_CLARIFICATION: AI 对技术方案表达了犹豫\n\
             FOLLOW_UP: 请选择最佳方案并输出完整代码。",
        );
        let checker = LlmClarificationChecker::new(client);

        let response = "我认为可以考虑使用 tokio，但也不排除 async-std 的可能性。";
        let result = checker.check(response, &ctx()).await;

        assert!(result.needs_clarification, "LLM 应检测到隐晦的不确定");
        assert!(result.reason.contains("犹豫"));
    }

    // ===== LlmClarificationChecker: 优雅降级 =====

    #[tokio::test]
    async fn test_llm_unavailable_returns_no_clarification() {
        let client = MockLlmClient::unavailable();
        let checker = LlmClarificationChecker::new(client);

        let result = checker.check("你希望用什么框架？", &ctx()).await;

        assert!(!result.needs_clarification, "LLM 不可用时应优雅降级");
    }

    #[tokio::test]
    async fn test_llm_unparsable_output_returns_no_clarification() {
        let client = MockLlmClient::single("我不确定你在说什么，随便吧。");
        let checker = LlmClarificationChecker::new(client);

        let result = checker.check("some response", &ctx()).await;

        assert!(!result.needs_clarification, "无法解析的 LLM 输出应优雅降级");
    }

    #[tokio::test]
    async fn test_llm_empty_response_returns_no_clarification() {
        let client = MockLlmClient::single("");
        let checker = LlmClarificationChecker::new(client);

        let result = checker.check("some response", &ctx()).await;

        assert!(!result.needs_clarification, "空 LLM 响应应优雅降级");
    }

    #[tokio::test]
    async fn test_llm_whitespace_only_response_returns_no_clarification() {
        let client = MockLlmClient::single("   \n\t  ");
        let checker = LlmClarificationChecker::new(client);

        let result = checker.check("some response", &ctx()).await;

        assert!(!result.needs_clarification, "纯空白 LLM 响应应优雅降级");
    }

    // ===== LlmClarificationChecker: 防循环 =====

    #[tokio::test]
    async fn test_llm_max_questions_reached() {
        let client = MockLlmClient::single("NEEDS_CLARIFICATION: test\nFOLLOW_UP: test");
        let checker = LlmClarificationChecker::new(client);

        let result = checker.check("some response", &ctx_maxed()).await;

        assert!(!result.needs_clarification, "达到最大追问次数后不应再追问");
    }

    #[tokio::test]
    async fn test_llm_duplicate_question_not_asked() {
        let client = MockLlmClient::single(
            "NEEDS_CLARIFICATION: test\n\
             FOLLOW_UP: 请直接选择最适合的框架并输出代码。",
        );
        let checker = LlmClarificationChecker::new(client);

        let context = ClarificationContext {
            task_prompt: "test".to_string(),
            timed_out: false,
            questions_asked: 1,
            max_questions: 2,
            previous_questions: vec!["请直接选择最适合的框架并输出代码。".to_string()],
        };

        let result = checker.check("some response", &context).await;

        assert!(!result.needs_clarification, "重复的追问不应再次发送");
    }

    // ===== LlmClarificationChecker: 追问消息质量 =====

    #[tokio::test]
    async fn test_llm_follow_up_contains_code_format_instruction() {
        let client = MockLlmClient::single(
            "NEEDS_CLARIFICATION: test\n\
             FOLLOW_UP: 请选择方案并输出代码。",
        );
        let checker = LlmClarificationChecker::new(client);

        let result = checker.check("?", &ctx()).await;

        assert!(result.needs_clarification);
        assert!(result.question.contains("代码"), "追问应包含代码要求");
    }

    #[tokio::test]
    async fn test_llm_default_follow_up_when_no_follow_up_provided() {
        // LLM 只返回 NEEDS_CLARIFICATION 但没有 FOLLOW_UP 行
        let client = MockLlmClient::single("NEEDS_CLARIFICATION: AI 回复不完整");
        let checker = LlmClarificationChecker::new(client);

        let result = checker.check("?", &ctx()).await;

        assert!(result.needs_clarification);
        assert!(
            result.question.contains("file:路径"),
            "默认追问应包含 file:路径 格式要求"
        );
        assert!(result.reason.contains("不完整"));
    }

    #[tokio::test]
    async fn test_llm_reason_prefixed_with_llm_marker() {
        let client = MockLlmClient::single("NEEDS_CLARIFICATION: 检测到提问\nFOLLOW_UP: test");
        let checker = LlmClarificationChecker::new(client);

        let result = checker.check("?", &ctx()).await;

        assert!(result.reason.contains("LLM 判断"), "原因应标记为 LLM 判断");
    }

    // ===== LlmClarificationChecker: 超时检测 =====

    #[tokio::test]
    async fn test_llm_with_timeout_context() {
        let client = MockLlmClient::single(
            "NEEDS_CLARIFICATION: 回复超时可能未完成\n\
             FOLLOW_UP: 请继续完成回复并输出所有代码文件。",
        );
        let checker = LlmClarificationChecker::new(client);

        let mut context = ctx();
        context.timed_out = true;

        let result = checker.check("partial response", &context).await;

        assert!(result.needs_clarification);
        assert!(result.reason.contains("超时"));
    }

    // ===== LlmClarificationChecker: 重试逻辑 =====

    #[tokio::test]
    async fn test_llm_retry_succeeds_on_second_attempt() {
        // 第一次超时失败, 第二次成功
        let client = SequenceLlmClient::new(vec![
            Err("operation timed out".to_string()),
            Ok("NEEDS_CLARIFICATION: test\nFOLLOW_UP: test".to_string()),
        ]);
        let checker = LlmClarificationChecker::new(client).with_max_retries(2);

        let result = checker.check("response", &ctx()).await;

        assert!(result.needs_clarification, "重试后应成功");
    }

    #[tokio::test]
    async fn test_llm_retry_succeeds_on_third_attempt() {
        // 两次失败, 第三次成功
        let client = SequenceLlmClient::new(vec![
            Err("connection refused".to_string()),
            Err("operation timed out".to_string()),
            Ok("OK".to_string()),
        ]);
        let checker = LlmClarificationChecker::new(client).with_max_retries(2);

        let result = checker.check("response", &ctx()).await;

        assert!(!result.needs_clarification, "第三次重试后应成功 (返回 OK)");
    }

    #[tokio::test]
    async fn test_llm_retry_exhausted_degrades() {
        // 所有重试都失败 (超时)
        let client = SequenceLlmClient::new(vec![
            Err("operation timed out".to_string()),
            Err("operation timed out".to_string()),
            Err("operation timed out".to_string()),
        ]);
        let checker = LlmClarificationChecker::new(client).with_max_retries(2);

        let result = checker.check("response", &ctx()).await;

        assert!(!result.needs_clarification, "重试耗尽后应优雅降级");
    }

    #[tokio::test]
    async fn test_llm_no_retry_on_parse_error() {
        // 解析错误不应重试
        let client = SequenceLlmClient::new(vec![
            Err("Ollama 响应解析失败".to_string()),
            Ok("OK".to_string()), // 这不应该被调用
        ]);
        let checker = LlmClarificationChecker::new(client).with_max_retries(3);

        let result = checker.check("response", &ctx()).await;

        assert!(!result.needs_clarification, "解析错误应立即降级");
        // 验证只调用了 1 次 (没有重试)
        // Note: call_count 验证需要保存 client 引用
    }

    #[tokio::test]
    async fn test_llm_no_retry_on_http_error() {
        // HTTP 错误不应重试
        let client = SequenceLlmClient::new(vec![
            Err("Ollama 返回错误状态: 500".to_string()),
            Ok("OK".to_string()), // 这不应该被调用
        ]);
        let checker = LlmClarificationChecker::new(client).with_max_retries(3);

        let result = checker.check("response", &ctx()).await;

        assert!(!result.needs_clarification, "HTTP 错误应立即降级");
    }

    #[tokio::test]
    async fn test_llm_retry_with_max_retries_zero() {
        // max_retries = 0 → 不重试, 第一次失败就降级
        let client = SequenceLlmClient::new(vec![
            Err("operation timed out".to_string()),
            Ok("OK".to_string()), // 这不应该被调用
        ]);
        let checker = LlmClarificationChecker::new(client).with_max_retries(0);

        let result = checker.check("response", &ctx()).await;

        assert!(!result.needs_clarification, "max_retries=0 不应重试");
    }

    #[tokio::test]
    async fn test_llm_retry_call_count_on_success() {
        // 验证重试后成功: 第一次连接拒绝, 第二次返回 OK
        let inner = SequenceLlmClient::new(vec![
            Err("connection refused".to_string()),
            Ok("OK".to_string()),
        ]);

        // 如果第一次成功就不会重试, 如果重试了说明第一次失败了
        let checker = LlmClarificationChecker::new(inner).with_max_retries(2);
        let result = checker.check("response", &ctx()).await;
        assert!(!result.needs_clarification, "重试后 OK → 不需要追问");
    }

    #[tokio::test]
    async fn test_llm_retry_connection_refused_then_success() {
        // 连接拒绝 → 重试 → 成功
        let client = SequenceLlmClient::new(vec![
            Err("error sending request: connection refused".to_string()),
            Ok("NEEDS_CLARIFICATION: AI 提问\nFOLLOW_UP: 请直接输出代码。".to_string()),
        ]);
        let checker = LlmClarificationChecker::new(client).with_max_retries(3);

        let result = checker.check("response", &ctx()).await;

        assert!(result.needs_clarification, "连接拒绝重试后应成功");
        assert!(result.question.contains("直接输出代码"));
    }

    // ===== LlmClarificationChecker: 方法委托测试 =====

    #[test]
    fn test_method_build_judge_prompt_delegates() {
        let client = MockLlmClient::unavailable();
        let checker = LlmClarificationChecker::new(client);

        let context = ctx();
        let prompt = checker.build_judge_prompt("AI 回复", &context);

        assert!(prompt.contains("创建一个 Rust CLI 工具"));
        assert!(prompt.contains("AI 回复"));
        assert!(prompt.contains("NEEDS_CLARIFICATION"));
    }

    #[test]
    fn test_method_parse_judge_result_delegates() {
        let client = MockLlmClient::unavailable();
        let checker = LlmClarificationChecker::new(client);

        let result = checker.parse_judge_result("OK");
        assert!(result.is_some());
        let (needs, _, _) = result.unwrap();
        assert!(!needs);
    }

    #[test]
    fn test_method_build_default_follow_up_delegates() {
        let client = MockLlmClient::unavailable();
        let checker = LlmClarificationChecker::new(client);

        let msg = checker.build_default_follow_up("test reason");
        assert!(msg.contains("file:路径"));
        assert!(msg.contains("test reason"));
    }

    #[test]
    fn test_method_is_duplicate_delegates() {
        let client = MockLlmClient::unavailable();
        let checker = LlmClarificationChecker::new(client);

        // is_duplicate 使用 prefix_len=30, 短字符串整体作为前缀比较
        let previous = vec!["完全相同的问题".to_string()];
        assert!(checker.is_duplicate("完全相同的问题", &previous));
        assert!(!checker.is_duplicate("完全不同的问题", &previous));
    }

    #[test]
    fn test_with_max_retries_builder() {
        let client = MockLlmClient::unavailable();
        let checker = LlmClarificationChecker::new(client).with_max_retries(5);
        assert_eq!(checker.max_retries, 5);
    }

    #[test]
    fn test_default_max_retries_is_two() {
        let client = MockLlmClient::unavailable();
        let checker = LlmClarificationChecker::new(client);
        assert_eq!(checker.max_retries, 2);
    }

    // ===== parse_judge_result 解析测试 (方法版, 保留兼容) =====

    #[test]
    fn test_parse_needs_clarification_with_follow_up_method() {
        let client = MockLlmClient::unavailable();
        let checker = LlmClarificationChecker::new(client);

        let result = checker
            .parse_judge_result("NEEDS_CLARIFICATION: AI 在提问\nFOLLOW_UP: 请直接输出代码。");

        assert!(result.is_some());
        let (needs, reason, follow_up) = result.unwrap();
        assert!(needs);
        assert_eq!(reason, "AI 在提问");
        assert_eq!(follow_up, Some("请直接输出代码。".to_string()));
    }

    #[test]
    fn test_parse_ok_method() {
        let client = MockLlmClient::unavailable();
        let checker = LlmClarificationChecker::new(client);

        let result = checker.parse_judge_result("OK");

        assert!(result.is_some());
        let (needs, _, _) = result.unwrap();
        assert!(!needs);
    }

    #[test]
    fn test_parse_unparsable_method() {
        let client = MockLlmClient::unavailable();
        let checker = LlmClarificationChecker::new(client);

        let result = checker.parse_judge_result("随便说点什么");

        assert!(result.is_none());
    }

    #[test]
    fn test_parse_case_insensitive_marker_method() {
        let client = MockLlmClient::unavailable();
        let checker = LlmClarificationChecker::new(client);

        let result = checker.parse_judge_result("needs_clarification: test\nfollow_up: test");

        assert!(result.is_some());
        let (needs, _, _) = result.unwrap();
        assert!(needs);
    }

    #[test]
    fn test_parse_needs_clarification_without_follow_up_method() {
        let client = MockLlmClient::unavailable();
        let checker = LlmClarificationChecker::new(client);

        let result = checker.parse_judge_result("NEEDS_CLARIFICATION: 回复不完整");

        assert!(result.is_some());
        let (needs, reason, follow_up) = result.unwrap();
        assert!(needs);
        assert_eq!(reason, "回复不完整");
        assert!(follow_up.is_none());
    }

    // ===== build_judge_prompt 测试 (方法版, 保留兼容) =====

    #[test]
    fn test_judge_prompt_contains_task_and_response_method() {
        let client = MockLlmClient::unavailable();
        let checker = LlmClarificationChecker::new(client);

        let context = ctx();
        let prompt = checker.build_judge_prompt("这是 AI 回复", &context);

        assert!(
            prompt.contains("创建一个 Rust CLI 工具"),
            "prompt 应包含任务"
        );
        assert!(prompt.contains("这是 AI 回复"), "prompt 应包含回复");
        assert!(prompt.contains("NEEDS_CLARIFICATION"), "prompt 应说明格式");
        assert!(prompt.contains("FOLLOW_UP"), "prompt 应要求 FOLLOW_UP");
    }

    #[test]
    fn test_judge_prompt_truncates_long_response_method() {
        let client = MockLlmClient::unavailable();
        let checker = LlmClarificationChecker::new(client);

        let long_response = "a".repeat(3000);
        let context = ctx();
        let prompt = checker.build_judge_prompt(&long_response, &context);

        assert!(prompt.contains("已截断"), "长回复应标记为已截断");
    }

    // ===== HybridClarificationChecker 测试 =====

    #[tokio::test]
    async fn test_hybrid_heuristic_catches_question_fast() {
        // 启发式能检测到的提问 → 不调用 LLM
        let client = MockLlmClient::unavailable(); // LLM 不可用, 但不应被调用
        let checker = HybridClarificationChecker::new(client);

        let result = checker.check("你希望用哪种框架？", &ctx()).await;

        assert!(result.needs_clarification, "启发式应检测到提问");
        // 原因来自启发式 (包含"提问标记")
        assert!(
            result.reason.contains("提问标记") || result.reason.contains("问号"),
            "应为启发式结果: {}",
            result.reason
        );
    }

    #[tokio::test]
    async fn test_hybrid_llm_catches_subtle_question() {
        // 启发式检测不到, 但 LLM 能检测到
        let client = MockLlmClient::single(
            "NEEDS_CLARIFICATION: AI 表达了对方案的犹豫\n\
             FOLLOW_UP: 请选择最佳方案并输出代码。",
        );
        let checker = HybridClarificationChecker::new(client);

        // 这个回复没有明显的提问标记, 启发式不会触发
        let response = "我觉得可以用 tokio, 也可以考虑 async-std, 各有优劣。";
        let result = checker.check(response, &ctx()).await;

        assert!(result.needs_clarification, "LLM 应检测到隐晦的不确定");
        assert!(result.reason.contains("LLM 判断"), "应为 LLM 结果");
    }

    #[tokio::test]
    async fn test_hybrid_both_say_no() {
        let client = MockLlmClient::single("OK");
        let checker = HybridClarificationChecker::new(client);

        let response = "好的，我来创建项目。\n```file:src/main.rs\nfn main() {}\n```";
        let result = checker.check(response, &ctx()).await;

        assert!(!result.needs_clarification, "两边都不需要追问");
    }

    #[tokio::test]
    async fn test_hybrid_llm_unavailable_fallback() {
        // LLM 不可用, 但启发式也没检测到 → 不追问
        let client = MockLlmClient::unavailable();
        let checker = HybridClarificationChecker::new(client);

        let response = "这是一个正常的回复，但没有代码。";
        let result = checker.check(response, &ctx()).await;

        // 启发式可能因为过短而触发, 但如果回复够长就不触发
        // 这里回复 > 20 字符, 启发式不会触发, LLM 不可用 → 不追问
        assert!(!result.needs_clarification, "LLM 不可用时应优雅降级");
    }

    #[tokio::test]
    async fn test_hybrid_timeout_triggers_heuristic_first() {
        // 超时 → 启发式优先检测 → 不调用 LLM
        let client = MockLlmClient::unavailable();
        let checker = HybridClarificationChecker::new(client);

        let mut context = ctx();
        context.timed_out = true;

        let result = checker.check("partial", &context).await;

        assert!(result.needs_clarification, "超时应触发启发式追问");
        assert!(result.reason.contains("超时"), "应为启发式的超时原因");
    }

    #[tokio::test]
    async fn test_hybrid_max_questions_reached() {
        let client = MockLlmClient::single("NEEDS_CLARIFICATION: test\nFOLLOW_UP: test");
        let checker = HybridClarificationChecker::new(client);

        let result = checker.check("你希望用什么？", &ctx_maxed()).await;

        assert!(!result.needs_clarification, "达到上限后不应追问");
    }

    #[tokio::test]
    async fn test_hybrid_with_min_response_len() {
        let client = MockLlmClient::single("OK");
        let checker = HybridClarificationChecker::new(client).with_min_response_len(10);

        let result = checker.check("短回复", &ctx()).await;

        // 启发式检测到过短 → 触发追问
        assert!(result.needs_clarification, "过短回复应触发启发式追问");
        assert!(result.reason.contains("过短"));
    }

    #[tokio::test]
    async fn test_hybrid_heuristic_yes_skips_llm() {
        // 验证启发式检测到问题时, 不会调用 LLM
        // 使用一个只会返回错误响应的 MockLlmClient
        struct FailingLlmClient;
        #[async_trait]
        impl LlmClient for FailingLlmClient {
            async fn generate(&self, _prompt: &str) -> Result<String> {
                panic!("启发式检测到问题时不应调用 LLM");
            }
            async fn is_available(&self) -> bool {
                false
            }
        }

        let checker = HybridClarificationChecker::new(FailingLlmClient);
        let result = checker.check("你希望用什么框架？", &ctx()).await;

        // 启发式应检测到提问, 不会调用 LLM (不会 panic)
        assert!(result.needs_clarification);
    }

    #[tokio::test]
    async fn test_hybrid_with_max_retries() {
        // Hybrid 也能设置 max_retries
        let client = SequenceLlmClient::new(vec![
            Err("operation timed out".to_string()),
            Ok("OK".to_string()),
        ]);
        let checker = HybridClarificationChecker::new(client)
            .with_max_retries(2)
            .with_min_response_len(5);

        let result = checker
            .check("这是一个正常的长回复，没有提问标记。", &ctx())
            .await;

        // 启发式不触发 (回复够长), LLM 重试后返回 OK
        assert!(!result.needs_clarification);
    }

    #[tokio::test]
    async fn test_hybrid_llm_retry_then_detects_clarification() {
        // 启发式不触发, LLM 第一次超时, 第二次检测到需要追问
        let client = SequenceLlmClient::new(vec![
            Err("connection refused".to_string()),
            Ok("NEEDS_CLARIFICATION: AI 犹豫不决\nFOLLOW_UP: 请选择方案并输出代码。".to_string()),
        ]);
        let checker = HybridClarificationChecker::new(client).with_max_retries(3);

        let response = "我觉得可以用 tokio, 也可以考虑 async-std, 各有优劣。";
        let result = checker.check(response, &ctx()).await;

        assert!(result.needs_clarification, "重试后 LLM 应检测到需要追问");
        assert!(result.reason.contains("LLM 判断"));
    }

    // ===== OllamaClient 构造测试 (不涉及网络) =====

    #[test]
    fn test_ollama_client_creation() {
        let client = OllamaClient::new("http://localhost:11434/", "llama2");
        // 端点末尾的 / 应被去除
        assert_eq!(client.endpoint, "http://localhost:11434");
        assert_eq!(client.model, "llama2");
    }

    #[test]
    fn test_ollama_client_default_local() {
        let client = OllamaClient::default_local();
        assert_eq!(client.endpoint, "http://localhost:11434");
        assert_eq!(client.model, "qwen2.5:3b");
    }

    #[test]
    fn test_ollama_client_with_timeout() {
        let client = OllamaClient::new("http://localhost:11434", "llama2").with_timeout(60);
        assert_eq!(client.timeout, Duration::from_secs(60));
    }

    #[test]
    fn test_ollama_client_trims_trailing_slash() {
        let client = OllamaClient::new("http://localhost:11434///", "llama2");
        assert_eq!(client.endpoint, "http://localhost:11434");
    }

    #[test]
    fn test_ollama_client_no_trailing_slash() {
        let client = OllamaClient::new("http://localhost:11434", "llama2");
        assert_eq!(client.endpoint, "http://localhost:11434");
    }
}
