//! 智能错误诊断 — 方向 F
//!
//! 在编译/测试失败时,智能分析错误根因,生成精准修复指令。
//!
//! ## 架构
//!
//! ```text
//! 编译/测试失败 → ErrorDiagnoser::diagnose()
//!                   ├── 1. HeuristicErrorDiagnoser (快速分类)
//!                   │      ├── 按 error_code 分类 (E0308→TypeError, E0382→BorrowError, ...)
//!                   │      ├── 按消息关键词分类 (fallback)
//!                   │      └── 生成基本修复提示
//!                   │
//!                   ├── 2. LlmErrorDiagnoser (LLM 深度分析)
//!                   │      ├── 构建诊断 prompt (错误 + 文件内容)
//!                   │      ├── 调用 LlmClient::generate()
//!                   │      ├── 解析 CATEGORY / ANALYSIS / FIX_GUIDANCE
//!                   │      └── LLM 不可用 → 优雅降级 (返回启发式结果)
//!                   │
//!                   └── 3. HybridErrorDiagnoser (混合)
//!                          ├── 启发式快速分类
//!                          ├── LLM 深度分析 (增强)
//!                          └── 历史相似错误查询 (建议)
//!
//! ErrorHistory 持久化到 .forge/error_history.json
//!   → 记录错误模式 (error_code + 签名) → 出现次数 → 修复成功/失败
//!   → find_similar() 查询相似历史错误 → 提供修复建议
//! ```

use crate::llm_clarify::{classify_llm_failure, should_retry_llm, LlmClient};
use crate::testrunner::CompileError;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

// ============================================================================
//  错误分类
// ============================================================================

/// 错误分类 — 按 Rust error code 或消息关键词分类
///
/// 分类依据:
/// - `from_error_code(code)` — 按 Rust 编译器 error code (如 E0308)
/// - `from_message(msg)` — 按消息关键词 (无 error code 时的 fallback)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCategory {
    /// 语法错误 (E0004 非穷尽匹配, syntax error)
    SyntaxError,
    /// 类型错误 (E0308 类型不匹配, E0271/E0277 trait bound)
    TypeError,
    /// 借用/所有权错误 (E0382 use of moved, E0500-E0599 borrow)
    BorrowError,
    /// 生命周期错误 (E0106, E0495)
    LifetimeError,
    /// 找不到项 (E0425 cannot find, E0426 undeclared)
    MissingItem,
    /// 导入错误 (E0432 unresolved import, E0433 failed to resolve)
    ImportError,
    /// trait 错误 (E0277 trait bound not satisfied, E0404 not a trait)
    TraitError,
    /// 测试失败 (cargo test 失败但编译通过)
    TestFailure,
    /// E2E 测试失败 (程序输出与预期不符)
    E2EFailure,
    /// 未知错误
    Unknown,
}

impl ErrorCategory {
    /// 显示名称
    pub fn display_name(&self) -> &str {
        match self {
            ErrorCategory::SyntaxError => "语法错误",
            ErrorCategory::TypeError => "类型错误",
            ErrorCategory::BorrowError => "借用/所有权错误",
            ErrorCategory::LifetimeError => "生命周期错误",
            ErrorCategory::MissingItem => "找不到定义",
            ErrorCategory::ImportError => "导入错误",
            ErrorCategory::TraitError => "Trait 错误",
            ErrorCategory::TestFailure => "测试失败",
            ErrorCategory::E2EFailure => "E2E 测试失败",
            ErrorCategory::Unknown => "未知错误",
        }
    }

    /// 按 Rust error code 分类
    ///
    /// 常见 error code:
    /// - E0308: mismatched types → TypeError
    /// - E0382: use of moved value → BorrowError
    /// - E0425: cannot find → MissingItem
    /// - E0432/E0433: unresolved import → ImportError
    /// - E0277: trait bound not satisfied → TraitError
    /// - E0106: missing lifetime → LifetimeError
    /// - E0500-E0599: borrow errors → BorrowError
    /// - E0004: non-exhaustive patterns → SyntaxError
    pub fn from_error_code(code: &str) -> Self {
        match code {
            // 语法错误
            "E0004" | "E0001" => ErrorCategory::SyntaxError,

            // 类型错误
            "E0308" | "E0271" | "E0277" | "E0282" | "E0304" | "E0310" | "E0117" | "E0119"
            | "E0133" | "E0136" | "E0138" | "E0144" | "E0146" | "E0152" | "E0154" | "E0158"
            | "E0161" | "E0162" | "E0164" | "E0170" | "E0178" | "E0184" | "E0185" | "E0186"
            | "E0193" | "E0198" | "E0199" | "E0200" | "E0201" | "E0202" | "E0204" | "E0206"
            | "E0207" | "E0210" | "E0211" | "E0212" | "E0214" | "E0216" | "E0220" | "E0221"
            | "E0223" | "E0224" | "E0229" | "E0230" | "E0231" | "E0243" | "E0244" | "E0246"
            | "E0247" | "E0248" | "E0249" | "E0251" | "E0252" | "E0253" | "E0254" | "E0257"
            | "E0259" | "E0260" | "E0261" | "E0262" | "E0263" | "E0264" | "E0265" | "E0266"
            | "E0267" | "E0268" | "E0269" | "E0270" | "E0272" | "E0273" | "E0274" | "E0275"
            | "E0276" | "E0278" | "E0279" | "E0281" | "E0283" | "E0284" | "E0285" | "E0286"
            | "E0287" | "E0288" | "E0289" | "E0290" | "E0291" | "E0292" | "E0293" | "E0294"
            | "E0295" | "E0296" | "E0297" | "E0298" | "E0299" | "E0300" | "E0301" | "E0302"
            | "E0303" | "E0305" | "E0306" | "E0307" | "E0309" | "E0311" | "E0312" | "E0313"
            | "E0314" | "E0315" | "E0316" | "E0317" | "E0318" | "E0319" | "E0320" | "E0321"
            | "E0322" | "E0323" | "E0324" | "E0325" | "E0326" | "E0327" | "E0328" | "E0329"
            | "E0331" | "E0332" | "E0333" | "E0334" | "E0335" | "E0336" | "E0337" | "E0338"
            | "E0339" | "E0340" | "E0341" | "E0342" | "E0343" | "E0344" | "E0345" | "E0346"
            | "E0347" | "E0348" | "E0349" | "E0350" | "E0351" | "E0352" | "E0353" | "E0354"
            | "E0355" | "E0356" | "E0357" | "E0358" | "E0359" | "E0360" | "E0361" | "E0362"
            | "E0363" | "E0364" | "E0365" | "E0366" | "E0367" | "E0368" | "E0369" | "E0370"
            | "E0371" | "E0372" | "E0373" | "E0374" | "E0375" | "E0376" | "E0377" | "E0378"
            | "E0379" | "E0380" | "E0381" => ErrorCategory::TypeError,

            // Borrow/Ownership errors
            "E0382" | "E0500" | "E0501" | "E0502" | "E0503" | "E0504" | "E0505" | "E0506"
            | "E0507" | "E0508" | "E0509" | "E0510" | "E0511" | "E0512" | "E0513" | "E0514"
            | "E0515" | "E0516" | "E0517" | "E0518" | "E0519" | "E0520" | "E0521" | "E0522"
            | "E0523" | "E0524" | "E0525" | "E0526" | "E0527" | "E0528" | "E0529" | "E0530"
            | "E0531" | "E0532" | "E0533" | "E0534" | "E0535" | "E0536" | "E0537" | "E0538"
            | "E0539" | "E0540" | "E0541" | "E0542" | "E0543" | "E0544" | "E0545" | "E0546"
            | "E0547" | "E0548" | "E0549" | "E0550" | "E0551" | "E0552" | "E0553" | "E0554"
            | "E0555" | "E0556" | "E0557" | "E0558" | "E0559" | "E0560" | "E0561" | "E0562"
            | "E0563" | "E0564" | "E0565" | "E0566" | "E0567" | "E0568" | "E0569" | "E0570"
            | "E0571" | "E0572" | "E0573" | "E0574" | "E0575" | "E0576" | "E0577" | "E0578"
            | "E0579" | "E0580" | "E0581" | "E0582" | "E0583" | "E0584" | "E0585" | "E0586"
            | "E0587" | "E0588" | "E0589" | "E0590" | "E0591" | "E0592" | "E0593" | "E0594"
            | "E0595" | "E0596" | "E0597" | "E0598" | "E0599" => ErrorCategory::BorrowError,

            // Lifetime errors
            "E0106" | "E0109" | "E0110" | "E0116" | "E0393" | "E0463" | "E0491" | "E0495"
            | "E0621" | "E0631" | "E0658" | "E0700" | "E0704" | "E0726" | "E0759" | "E0760"
            | "E0761" | "E0762" | "E0767" | "E0770" | "E0781" | "E0803" | "E0804" | "E0805"
            | "E0806" | "E0807" | "E0809" | "E0810" | "E0811" | "E0822" | "E0823" | "E0824"
            | "E0825" | "E0826" | "E0827" | "E0830" | "E0831" | "E0832" | "E0833" | "E0836"
            | "E0837" | "E0838" | "E0716" => ErrorCategory::LifetimeError,

            // Missing item (not found)
            "E0425" | "E0426" | "E0428" | "E0429" | "E0430" | "E0434" | "E0435" | "E0436"
            | "E0437" | "E0438" | "E0439" | "E0445" | "E0446" | "E0447" | "E0448" | "E0449"
            | "E0450" | "E0451" | "E0452" | "E0453" | "E0454" | "E0455" | "E0456" | "E0457"
            | "E0458" | "E0459" | "E0460" | "E0461" | "E0462" | "E0464" | "E0465" | "E0466"
            | "E0467" | "E0468" | "E0469" | "E0470" | "E0471" | "E0472" | "E0473" | "E0474"
            | "E0475" | "E0476" | "E0477" | "E0478" | "E0479" | "E0480" | "E0481" | "E0482"
            | "E0483" | "E0484" | "E0485" | "E0486" | "E0487" | "E0488" | "E0489" | "E0490"
            | "E0492" | "E0493" | "E0494" | "E0496" | "E0497" | "E0498" | "E0499" | "E0600"
            | "E0601" | "E0602" | "E0603" | "E0604" | "E0605" | "E0606" | "E0607" | "E0608"
            | "E0609" | "E0610" | "E0612" | "E0613" | "E0614" | "E0615" | "E0616" | "E0617"
            | "E0618" | "E0619" | "E0620" | "E0622" | "E0623" | "E0624" | "E0625" | "E0626"
            | "E0627" | "E0628" | "E0629" | "E0630" | "E0632" | "E0633" | "E0634" | "E0635"
            | "E0636" | "E0637" | "E0638" | "E0639" | "E0640" | "E0641" | "E0642" | "E0643"
            | "E0644" | "E0645" | "E0646" | "E0647" | "E0648" | "E0649" | "E0650" | "E0651"
            | "E0652" | "E0653" | "E0654" | "E0655" | "E0656" | "E0657" | "E0659" | "E0660"
            | "E0661" | "E0662" | "E0663" | "E0664" | "E0665" | "E0666" | "E0667" | "E0668"
            | "E0669" | "E0670" | "E0671" | "E0672" | "E0673" | "E0674" | "E0675" | "E0676"
            | "E0677" | "E0678" | "E0679" | "E0680" | "E0681" | "E0682" | "E0683" | "E0684"
            | "E0685" | "E0686" | "E0687" | "E0688" | "E0689" | "E0690" | "E0691" | "E0692"
            | "E0693" | "E0694" | "E0695" | "E0696" | "E0697" | "E0698" | "E0699" | "E0701"
            | "E0702" | "E0703" => ErrorCategory::MissingItem,

            // Import errors
            "E0432" | "E0433" => ErrorCategory::ImportError,

            // Trait errors (subset, some overlap with TypeError)
            "E0404" | "E0407" | "E0408" => ErrorCategory::TraitError,

            _ => {
                // Try to parse the number and apply ranges
                if let Ok(num) = code
                    .trim_start_matches('E')
                    .trim_start_matches('e')
                    .parse::<u32>()
                {
                    match num {
                        300..=399 => ErrorCategory::TypeError,
                        500..=599 => ErrorCategory::BorrowError,
                        425..=430 | 600..=699 => ErrorCategory::MissingItem,
                        432 | 433 => ErrorCategory::ImportError,
                        106 | 495 => ErrorCategory::LifetimeError,
                        _ => ErrorCategory::Unknown,
                    }
                } else {
                    ErrorCategory::Unknown
                }
            }
        }
    }

    /// 按消息关键词分类 (无 error code 时的 fallback)
    ///
    /// 支持多语言错误消息:
    /// - Rust: "mismatched types", "cannot borrow", "cannot find", "expected one of"
    /// - Python: "SyntaxError", "ImportError", "NameError", "AttributeError", "ValueError"
    /// - Go: "undefined", "cannot find", "too many arguments", "not enough arguments"
    /// - Node: "is not defined", "Cannot find module", "is not a function"
    pub fn from_message(msg: &str) -> Self {
        let lower = msg.to_lowercase();

        if lower.contains("syntax")
            || lower.contains("unexpected token")
            || lower.contains("parse error")
        {
            return ErrorCategory::SyntaxError;
        }

        if lower.contains("mismatched types")
            || lower.contains("type mismatch")
            || (lower.contains("expected") && lower.contains("found"))
            || lower.contains("typeerror")
            || lower.contains("cannot be applied to type")
            || lower.contains("is not a function")
            || lower.contains("is not iterable")
            || lower.contains("attributeerror")
            || lower.contains("valueerror")
        {
            return ErrorCategory::TypeError;
        }

        if lower.contains("borrow")
            || lower.contains("ownership")
            || lower.contains("moved")
            || lower.contains("use of moved")
            || lower.contains("borrowed")
        {
            return ErrorCategory::BorrowError;
        }

        if lower.contains("lifetime") {
            return ErrorCategory::LifetimeError;
        }

        if lower.contains("importerror")
            || lower.contains("modulenotfound")
            || lower.contains("unresolved import")
            || lower.contains("cannot find module")
            || lower.contains("no module named")
        {
            return ErrorCategory::ImportError;
        }

        if lower.contains("cannot find")
            || lower.contains("not found")
            || lower.contains("undefined")
            || lower.contains("not defined")
            || lower.contains("nameerror")
            || lower.contains("undeclared")
            || lower.contains("keyerror")
            || lower.contains("indexerror")
        {
            return ErrorCategory::MissingItem;
        }

        if lower.contains("trait") && lower.contains("not satisfied")
            || lower.contains("trait bound")
        {
            return ErrorCategory::TraitError;
        }

        if lower.contains("e2e") || lower.contains("expected_stdout") {
            return ErrorCategory::E2EFailure;
        }

        if lower.contains("test") && lower.contains("fail") {
            return ErrorCategory::TestFailure;
        }

        ErrorCategory::Unknown
    }

    /// 从 CompileError 推断分类
    pub fn from_compile_error(error: &CompileError) -> Self {
        if let Some(code) = &error.error_code {
            let cat = Self::from_error_code(code);
            if cat != ErrorCategory::Unknown {
                return cat;
            }
        }
        Self::from_message(&error.message)
    }
}

impl std::fmt::Display for ErrorCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

// ============================================================================
//  纯逻辑函数 — 可独立测试, 无外部依赖
// ============================================================================

/// 从文本中提取 Rust error code (如 "error[E0308]: ..." → "E0308")
///
/// 支持格式:
/// - `error[E0308]: mismatched types`
/// - `[E0308] mismatched types`
/// - `E0308: mismatched types`
///
/// 返回 None 如果文本中没有 error code。
pub fn extract_error_code_from_text(msg: &str) -> Option<String> {
    // 匹配 error[EXXXX] 格式
    if let Some(start) = msg.find('[') {
        if let Some(end) = msg[start..].find(']') {
            let code = &msg[start + 1..start + end];
            if code.len() >= 2
                && (code.starts_with('E') || code.starts_with('e'))
                && code[1..].chars().all(|c| c.is_ascii_digit())
            {
                return Some(code.to_uppercase());
            }
        }
    }
    // 匹配 EXXXX: 格式 (行首)
    for line in msg.lines() {
        let trimmed = line.trim();
        if trimmed.len() >= 2 && (trimmed.starts_with('E') || trimmed.starts_with('e')) {
            let potential_code: String = trimmed
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            if potential_code.len() >= 2
                && potential_code[1..].chars().all(|c| c.is_ascii_digit())
                && trimmed
                    .chars()
                    .nth(potential_code.len())
                    .is_some_and(|c| c == ':' || c == ' ' || c == ']')
            {
                return Some(potential_code.to_uppercase());
            }
        }
    }
    None
}

/// 格式化错误位置为 `file:line:col` 字符串
///
/// # 示例
/// ```ignore
/// let err = CompileError { file: "src/main.rs".into(), line: Some(10), column: Some(5), ... };
/// assert_eq!(format_error_location(&err), "src/main.rs:10:5");
/// ```
pub fn format_error_location(error: &CompileError) -> String {
    match (error.line, error.column) {
        (Some(line), Some(col)) => format!("{}:{}:{}", error.file, line, col),
        (Some(line), None) => format!("{}:{}", error.file, line),
        _ => error.file.clone(),
    }
}

/// 按分类生成通用修复建议 (纯逻辑)
///
/// 从 `HeuristicErrorDiagnoser::generate_guidance` 提取。
/// 未知错误返回空字符串 (不提供指导)。
pub fn generate_guidance_for_category(category: ErrorCategory) -> String {
    match category {
        ErrorCategory::TypeError => {
            "类型不匹配。请检查函数签名和变量类型声明，确保赋值的两端类型一致。\n\
             常见原因: 整数类型不匹配 (usize vs i32)、函数返回类型错误、泛型参数不正确。"
                .to_string()
        }
        ErrorCategory::BorrowError => {
            "借用/所有权错误。请检查变量的所有权是否被转移、是否在借用后修改了原值。\n\
             常见修复: 使用 .clone() 复制值、使用 & 或 &mut 引用传递、重构数据流避免 move。"
                .to_string()
        }
        ErrorCategory::LifetimeError => "生命周期错误。请检查引用的生命周期标注是否正确。\n\
             常见修复: 添加生命周期参数标注、使用 'static、重构以减少引用嵌套。"
            .to_string(),
        ErrorCategory::MissingItem => "找不到定义。请检查变量名/函数名是否正确、是否需要导入。\n\
             常见原因: 拼写错误、缺少 use 语句、函数/结构体未定义。"
            .to_string(),
        ErrorCategory::ImportError => {
            "导入错误。请检查 use 语句的路径是否正确、依赖是否在 Cargo.toml 中声明。\n\
             常见修复: 修正导入路径、添加 [dependencies] 到 Cargo.toml、使用正确的模块路径。"
                .to_string()
        }
        ErrorCategory::TraitError => {
            "Trait 错误。请检查 trait 是否被正确实现、trait bound 是否满足。\n\
             常见修复: 实现 missing trait、添加 trait bound 到泛型参数、使用 derive 宏。"
                .to_string()
        }
        ErrorCategory::SyntaxError => "语法错误。请检查代码结构是否完整、括号/分号是否匹配。\n\
             常见原因: 缺少分号、括号不匹配、模式匹配不完整。"
            .to_string(),
        ErrorCategory::TestFailure => {
            "测试失败。请检查测试逻辑是否正确、被测试函数的行为是否符合预期。\n\
             建议: 运行 `cargo test -- --nocapture` 查看详细输出，修正被测函数或测试用例。"
                .to_string()
        }
        ErrorCategory::E2EFailure => {
            "E2E 测试失败。程序输出与预期不符。请检查程序的输出格式和退出码。\n\
             建议: 在本地运行程序检查实际输出，修正输出逻辑或调整预期值。"
                .to_string()
        }
        ErrorCategory::Unknown => String::new(),
    }
}

/// 生成诊断分析文本 (纯逻辑)
///
/// 从 `HeuristicErrorDiagnoser::generate_analysis` 提取。
/// 空错误列表返回 "无编译错误，可能是测试失败"。
pub fn generate_analysis_text(errors: &[CompileError], category: ErrorCategory) -> String {
    if errors.is_empty() {
        return "无编译错误，可能是测试失败".to_string();
    }
    let main_error = &errors[0];
    let location = format_error_location(main_error);
    let code_info = main_error
        .error_code
        .as_ref()
        .map(|c| format!(" [{}]", c))
        .unwrap_or_default();
    format!(
        "错误分类: {}\n位置: {}\n消息:{} {}",
        category, location, code_info, main_error.message
    )
}

/// 格式化错误列表用于 LLM prompt (纯逻辑)
///
/// 返回 (格式化文本, 截断的错误数量)。
/// 最多显示 5 个错误, 超出的部分用 "... 还有 N 个错误" 标注。
pub fn format_errors_for_prompt(errors: &[CompileError]) -> (String, Option<usize>) {
    let errors_list: String = errors
        .iter()
        .take(5)
        .enumerate()
        .map(|(i, e)| {
            let code = e
                .error_code
                .as_ref()
                .map(|c| format!("[{}]", c))
                .unwrap_or_default();
            let line = e.line.unwrap_or(0);
            let col = e.column.unwrap_or(0);
            format!(
                "{}. {}:{}:{} {} {}\n   {}",
                i + 1,
                e.file,
                line,
                col,
                code,
                e.message,
                e.message
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let truncated = if errors.len() > 5 {
        Some(errors.len() - 5)
    } else {
        None
    };

    (errors_list, truncated)
}

/// 从 LLM 响应中提取字段值 (纯逻辑)
///
/// 从 `LlmErrorDiagnoser::extract_field` 提取。
/// 支持大小写不敏感的字段名匹配, 支持中英文冒号。
pub fn extract_field_value(text: &str, field_name: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.to_uppercase().starts_with(field_name) {
            let after = &trimmed[field_name.len()..];
            let value = after.trim_start_matches(&[':', ' ', '：'][..]).trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// 解析 LLM 诊断响应 (纯逻辑)
///
/// 从 `LlmErrorDiagnoser::parse_diagnosis` 提取。
/// 格式:
/// ```text
/// CATEGORY: TypeError
/// ANALYSIS: 根因分析...
/// FIX_GUIDANCE: 修复建议...
/// ```
///
/// 返回 None 如果无法解析 (至少需要 CATEGORY 字段)。
pub fn parse_llm_diagnosis(response: &str) -> Option<(ErrorCategory, String, String)> {
    let trimmed = response.trim();

    let category_str = extract_field_value(trimmed, "CATEGORY");
    if category_str.is_none() {
        debug!(
            "LLM 诊断结果无法解析: {}",
            &trimmed[..trimmed.len().min(100)]
        );
        return None;
    }

    let category = category_str
        .map(|s| ErrorCategory::from_message(&s))
        .unwrap_or(ErrorCategory::Unknown);

    let analysis = extract_field_value(trimmed, "ANALYSIS").unwrap_or_default();
    let guidance = extract_field_value(trimmed, "FIX_GUIDANCE").unwrap_or_default();

    Some((category, analysis, guidance))
}

/// 格式化历史建议文本 (纯逻辑)
///
/// 从 `HybridErrorDiagnoser::build_history_suggestion` 提取。
/// 空列表返回空字符串。
pub fn format_history_suggestion(similar: &[ErrorPattern]) -> String {
    if similar.is_empty() {
        return String::new();
    }

    let mut suggestions = Vec::new();
    for p in similar.iter().take(3) {
        let status = if p.last_fix_succeeded {
            "已修复"
        } else {
            "未修复"
        };
        let approach = p
            .suggested_approach
            .as_ref()
            .map(|s| format!("  建议: {}\n", s.chars().take(100).collect::<String>()))
            .unwrap_or_default();
        suggestions.push(format!(
            "- [{}] {} (出现 {} 次, {})\n{}",
            p.category,
            p.error_code.as_deref().unwrap_or("N/A"),
            p.occurrences,
            status,
            approach
        ));
    }
    format!("相似历史错误:\n{}", suggestions.join(""))
}

/// 计算诊断置信度 (纯逻辑)
///
/// 规则:
/// - hybrid: 0.9
/// - llm: 0.85
/// - heuristic: 已知分类 0.7, 未知 0.3
/// - heuristic_fallback: 已知分类 0.6, 未知 0.25
/// - mock: 1.0
/// - none/其他: 0.0
pub fn compute_diagnosis_confidence(category: ErrorCategory, source: &str) -> f64 {
    let is_known = category != ErrorCategory::Unknown;
    match source {
        "hybrid" => 0.9,
        "llm" => 0.85,
        "heuristic" => {
            if is_known {
                0.7
            } else {
                0.3
            }
        }
        "heuristic_fallback" => {
            if is_known {
                0.6
            } else {
                0.25
            }
        }
        "mock" => 1.0,
        _ => 0.0,
    }
}

/// 合并 LLM 修复指导和历史建议 (纯逻辑)
///
/// 两者都有内容时用双换行分隔, 否则返回有内容的那一方 (都空则返回空)。
pub fn merge_guidance(llm_guidance: &str, history_suggestion: &str) -> String {
    let mut result = llm_guidance.to_string();
    if !result.is_empty() && !history_suggestion.is_empty() {
        result.push_str("\n\n");
    }
    result.push_str(history_suggestion);
    result
}

/// 合并 LLM 分析和启发式分析 (纯逻辑)
///
/// 两者都有内容时用单换行分隔, 否则返回有内容的那一方 (都空则返回空)。
pub fn merge_analysis(llm_analysis: &str, heuristic_analysis: &str) -> String {
    let mut result = llm_analysis.to_string();
    if !result.is_empty() && !heuristic_analysis.is_empty() {
        result.push('\n');
    }
    if !heuristic_analysis.is_empty() {
        result.push_str(heuristic_analysis);
    }
    result
}

// ============================================================================
//  错误模式 / 诊断结果 / 诊断上下文
// ============================================================================

/// 错误模式 — 记录重复出现的错误模式 (用于历史学习)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorPattern {
    /// Rust error code (如 "E0308"), None 表示无 error code
    pub error_code: Option<String>,
    /// 消息签名 (前 100 字符, 用于去重)
    pub message_signature: String,
    /// 错误分类
    pub category: ErrorCategory,
    /// 出现次数
    pub occurrences: usize,
    /// 首次出现时间
    pub first_seen: DateTime<Utc>,
    /// 最后出现时间
    pub last_seen: DateTime<Utc>,
    /// 最近一次修复是否成功
    pub last_fix_succeeded: bool,
    /// LLM 建议的修复方法 (如有)
    #[serde(default)]
    pub suggested_approach: Option<String>,
}

/// 诊断结果 — 错误诊断的输出
#[derive(Debug, Clone)]
pub struct DiagnosisResult {
    /// 错误分类
    pub category: ErrorCategory,
    /// 根因分析 (LLM 分析或启发式描述)
    pub analysis: String,
    /// 修复指导 (追加到修复 prompt 前面的具体建议)
    pub fix_guidance: String,
    /// 相似的历史错误模式 (如有)
    pub similar_patterns: Vec<ErrorPattern>,
    /// 置信度 (0.0-1.0)
    pub confidence: f64,
    /// 诊断来源 ("heuristic" / "llm" / "hybrid")
    pub source: String,
}

impl DiagnosisResult {
    /// 创建一个空的诊断结果 (无诊断)
    pub fn empty() -> Self {
        Self {
            category: ErrorCategory::Unknown,
            analysis: String::new(),
            fix_guidance: String::new(),
            similar_patterns: vec![],
            confidence: 0.0,
            source: "none".to_string(),
        }
    }

    /// 是否有实质性的修复指导
    pub fn has_guidance(&self) -> bool {
        !self.fix_guidance.is_empty()
    }
}

/// 诊断上下文 — 提供给诊断器的判断依据
#[derive(Debug, Clone)]
pub struct DiagnosisContext {
    /// 当前任务的原始 prompt
    pub task_prompt: String,
    /// 当前修复轮次 (从 1 开始)
    pub attempt: u32,
    /// 最大修复轮次
    pub max_attempts: u32,
    /// 本次任务写入的文件列表
    pub files_written: Vec<String>,
}

// ============================================================================
//  错误历史 — 持久化到 .forge/error_history.json
// ============================================================================

/// 错误历史 — 记录错误模式,支持持久化和查询
///
/// 存储在 `.forge/error_history.json`,在任务执行过程中积累:
/// - 每次编译/测试失败 → 记录错误模式
/// - 修复成功 → 标记 last_fix_succeeded=true
/// - 修复失败 → 标记 last_fix_succeeded=false
/// - 下次遇到相似错误 → 查询历史,提供修复建议
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ErrorHistory {
    /// 错误模式列表
    pub patterns: Vec<ErrorPattern>,
    /// 历史文件路径 (运行时设置,不序列化)
    #[serde(skip)]
    pub history_path: Option<PathBuf>,
}

impl ErrorHistory {
    /// 创建空的历史
    pub fn new() -> Self {
        Self::default()
    }

    /// 生成错误签名的工具函数
    fn make_signature(error: &CompileError) -> String {
        let msg_part: String = error.message.chars().take(100).collect();
        match &error.error_code {
            Some(code) => format!("[{}] {}", code, msg_part),
            None => msg_part,
        }
    }

    /// 记录一个错误出现 (如已存在则增加计数)
    ///
    /// `fixed` 表示本次是否最终修复成功
    pub fn record(&mut self, error: &CompileError, category: ErrorCategory, fixed: bool) {
        let signature = Self::make_signature(error);
        let now = Utc::now();

        if let Some(pattern) = self
            .patterns
            .iter_mut()
            .find(|p| p.message_signature == signature)
        {
            pattern.occurrences += 1;
            pattern.last_seen = now;
            pattern.last_fix_succeeded = fixed;
            if fixed {
                pattern.suggested_approach = Some(error.message.clone());
            }
        } else {
            self.patterns.push(ErrorPattern {
                error_code: error.error_code.clone(),
                message_signature: signature,
                category,
                occurrences: 1,
                first_seen: now,
                last_seen: now,
                last_fix_succeeded: fixed,
                suggested_approach: if fixed {
                    Some(error.message.clone())
                } else {
                    None
                },
            });
        }
    }

    /// 查找与给定错误相似的历史模式
    ///
    /// 匹配规则:
    /// 1. 相同 error_code → 匹配
    /// 2. 相同消息签名 → 匹配
    /// 3. 相同分类 + 消息部分匹配 → 可能匹配
    pub fn find_similar(&self, error: &CompileError) -> Vec<&ErrorPattern> {
        let signature = Self::make_signature(error);
        let category = ErrorCategory::from_compile_error(error);

        self.patterns
            .iter()
            .filter(|p| {
                // 精确匹配 error_code
                if let (Some(c1), Some(c2)) = (&p.error_code, &error.error_code) {
                    if c1 == c2 {
                        return true;
                    }
                }
                // 精确匹配签名
                if p.message_signature == signature {
                    return true;
                }
                // 相同分类 + error_code 匹配
                if p.category == category && p.error_code.as_ref() == error.error_code.as_ref() {
                    return true;
                }
                false
            })
            .collect()
    }

    /// 查找已成功修复的相似模式 (用于获取修复建议)
    pub fn find_successful_patterns(&self, error: &CompileError) -> Vec<&ErrorPattern> {
        self.find_similar(error)
            .into_iter()
            .filter(|p| p.last_fix_succeeded)
            .collect()
    }

    /// 获取所有错误模式统计摘要
    pub fn summary(&self) -> String {
        if self.patterns.is_empty() {
            return "(无历史错误)".to_string();
        }
        let total: usize = self.patterns.iter().map(|p| p.occurrences).sum();
        let fixed = self
            .patterns
            .iter()
            .filter(|p| p.last_fix_succeeded)
            .count();
        format!(
            "{} 个模式, {} 次出现, {} 个已修复",
            self.patterns.len(),
            total,
            fixed
        )
    }

    /// 从文件加载错误历史
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            let mut h = Self::new();
            h.history_path = Some(path.to_path_buf());
            return Ok(h);
        }
        let content =
            std::fs::read_to_string(path).map_err(|e| anyhow!("读取错误历史失败: {}", e))?;
        let mut history: ErrorHistory =
            serde_json::from_str(&content).map_err(|e| anyhow!("解析错误历史 JSON 失败: {}", e))?;
        history.history_path = Some(path.to_path_buf());
        Ok(history)
    }

    /// 保存错误历史到文件
    pub fn save(&self) -> Result<()> {
        let path = self
            .history_path
            .as_ref()
            .ok_or_else(|| anyhow!("错误历史路径未设置"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content =
            serde_json::to_string_pretty(self).map_err(|e| anyhow!("序列化错误历史失败: {}", e))?;
        std::fs::write(path, content).map_err(|e| anyhow!("写入错误历史失败: {}", e))?;
        Ok(())
    }

    /// 从工作区加载 (查找 .forge/error_history.json)
    pub fn load_from_workspace(workspace_root: &Path) -> Self {
        let path = workspace_root.join(".forge").join("error_history.json");
        match Self::load(&path) {
            Ok(h) => h,
            Err(e) => {
                warn!("加载错误历史失败, 使用空历史: {}", e);
                let mut h = Self::new();
                h.history_path = Some(path);
                h
            }
        }
    }

    /// 保存到工作区
    pub fn save_to_workspace(&self) -> Result<()> {
        if self.history_path.is_none() {
            return Ok(());
        }
        self.save()
    }

    /// 清除所有历史 (用于测试)
    pub fn clear(&mut self) {
        self.patterns.clear();
    }
}

// ============================================================================
//  ErrorDiagnoser trait — DIP: 抽象错误诊断能力
// ============================================================================

/// 错误诊断器 trait — DIP: 在编译/测试失败时智能分析错误根因
///
/// **方向 F: 智能错误诊断**
///
/// 实现者:
/// - `HeuristicErrorDiagnoser` (启发式快速分类)
/// - `LlmErrorDiagnoser<C: LlmClient>` (LLM 深度分析)
/// - `HybridErrorDiagnoser<C: LlmClient>` (混合: 启发式 + LLM + 历史)
/// - `MockErrorDiagnoser` (测试版)
#[async_trait]
pub trait ErrorDiagnoser: Send + Sync {
    /// 诊断编译/测试错误
    ///
    /// 返回 `DiagnosisResult`:
    /// - `category` — 错误分类
    /// - `analysis` — 根因分析
    /// - `fix_guidance` — 修复指导 (追加到修复 prompt)
    /// - `similar_patterns` — 相似历史错误
    /// - `confidence` — 置信度 (0.0-1.0)
    async fn diagnose(
        &self,
        errors: &[CompileError],
        feedback: &str,
        context: &DiagnosisContext,
        history: &ErrorHistory,
    ) -> DiagnosisResult;
}

// ============================================================================
//  HeuristicErrorDiagnoser — 启发式快速分类
// ============================================================================

/// 启发式错误诊断器 — 基于 error code 和消息关键词的快速分类
///
/// 无 LLM 依赖, 纯规则匹配, 速度快。
/// 生成的修复指导基于错误分类的通用建议。
pub struct HeuristicErrorDiagnoser;

impl HeuristicErrorDiagnoser {
    pub fn new() -> Self {
        Self
    }

    /// 按分类生成通用修复建议 (委托纯函数)
    fn generate_guidance(category: ErrorCategory) -> String {
        generate_guidance_for_category(category)
    }

    /// 生成诊断分析文本 (委托纯函数)
    fn generate_analysis(errors: &[CompileError], category: ErrorCategory) -> String {
        generate_analysis_text(errors, category)
    }
}

impl Default for HeuristicErrorDiagnoser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ErrorDiagnoser for HeuristicErrorDiagnoser {
    async fn diagnose(
        &self,
        errors: &[CompileError],
        _feedback: &str,
        _context: &DiagnosisContext,
        history: &ErrorHistory,
    ) -> DiagnosisResult {
        if errors.is_empty() {
            return DiagnosisResult {
                category: ErrorCategory::TestFailure,
                analysis: "无编译错误，可能是测试运行时失败".to_string(),
                fix_guidance: Self::generate_guidance(ErrorCategory::TestFailure),
                similar_patterns: vec![],
                confidence: compute_diagnosis_confidence(ErrorCategory::TestFailure, "heuristic"),
                source: "heuristic".to_string(),
            };
        }

        let category = ErrorCategory::from_compile_error(&errors[0]);
        let analysis = Self::generate_analysis(errors, category);
        let guidance = Self::generate_guidance(category);
        let similar = history.find_similar(&errors[0]);

        DiagnosisResult {
            category,
            analysis,
            fix_guidance: guidance,
            similar_patterns: similar.into_iter().cloned().collect(),
            confidence: compute_diagnosis_confidence(category, "heuristic"),
            source: "heuristic".to_string(),
        }
    }
}

// ============================================================================
//  LlmErrorDiagnoser — LLM 深度分析
// ============================================================================

/// LLM 增强错误诊断器 — 使用本地 LLM 分析错误根因和修复建议
///
/// 复用 `LlmClient` trait (OllamaClient),与 LLM 自主追问共享 LLM 基础设施。
/// LLM 不可用时优雅降级到启发式分类。
pub struct LlmErrorDiagnoser<C: LlmClient> {
    /// LLM 客户端
    client: C,
    /// 最大重试次数 (默认 2, 设为 0 表示不重试)
    max_retries: u32,
}

impl<C: LlmClient> LlmErrorDiagnoser<C> {
    pub fn new(client: C) -> Self {
        Self {
            client,
            max_retries: 2,
        }
    }

    /// 设置最大重试次数 (builder 模式)
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
                            "LLM 诊断调用失败 (不重试): attempt={}, type={:?}, error={}",
                            attempt, failure_type, e
                        );
                        return Err(e);
                    }

                    debug!(
                        "LLM 诊断调用失败 (将重试): attempt={}/{}, type={:?}, error={}",
                        attempt, self.max_retries, failure_type, e
                    );
                }
            }
        }

        Err(anyhow!(last_error_msg))
    }

    /// 构建给 LLM 的诊断 prompt (委托纯函数)
    fn build_diagnosis_prompt(
        &self,
        errors: &[CompileError],
        context: &DiagnosisContext,
    ) -> String {
        let (errors_list, truncated) = format_errors_for_prompt(errors);
        let more = truncated
            .map(|n| format!("\n... 还有 {} 个错误", n))
            .unwrap_or_default();

        format!(
            "你是一个编译错误分析专家。\n\
             请分析以下编译错误，给出诊断和修复建议。\n\
             \n\
             任务: {task_prompt}\n\
             修复轮次: {attempt}/{max_attempts}\n\
             已写入文件: {files}\n\
             \n\
             编译错误:\n\
             {errors}{more}\n\
             \n\
             请按以下格式输出:\n\
             CATEGORY: <分类>\n\
             ANALYSIS: <根因分析,1-2句话说明为什么出错>\n\
             FIX_GUIDANCE: <具体修复建议,告诉AI应该怎么改>\n\
             \n\
             分类选项: SyntaxError, TypeError, BorrowError, LifetimeError, MissingItem, ImportError, TraitError, TestFailure, Unknown\n\
             \n\
             只输出以上格式,不要多余内容。",
            task_prompt = context.task_prompt.chars().take(200).collect::<String>(),
            attempt = context.attempt,
            max_attempts = context.max_attempts,
            files = context.files_written.join(", "),
            errors = errors_list,
            more = more,
        )
    }

    /// 解析 LLM 返回的诊断结果 (委托纯函数)
    fn parse_diagnosis(&self, llm_response: &str) -> Option<(ErrorCategory, String, String)> {
        parse_llm_diagnosis(llm_response)
    }

    /// 从 LLM 响应中提取字段值 (委托纯函数)
    #[allow(dead_code)]
    fn extract_field(text: &str, field_name: &str) -> Option<String> {
        extract_field_value(text, field_name)
    }
}

#[async_trait]
impl<C: LlmClient> ErrorDiagnoser for LlmErrorDiagnoser<C> {
    async fn diagnose(
        &self,
        errors: &[CompileError],
        feedback: &str,
        context: &DiagnosisContext,
        history: &ErrorHistory,
    ) -> DiagnosisResult {
        if errors.is_empty() {
            // 无编译错误, 可能是测试失败 — 交给启发式
            let heuristic = HeuristicErrorDiagnoser::new();
            return heuristic.diagnose(errors, feedback, context, history).await;
        }

        // 构建诊断 prompt
        let prompt = self.build_diagnosis_prompt(errors, context);

        // 调用 LLM (带重试)
        match self.generate_with_retry(&prompt).await {
            Ok(llm_response) => {
                match self.parse_diagnosis(&llm_response) {
                    Some((category, analysis, guidance)) => {
                        let similar = history.find_similar(&errors[0]);
                        DiagnosisResult {
                            category,
                            analysis,
                            fix_guidance: guidance,
                            similar_patterns: similar.into_iter().cloned().collect(),
                            confidence: compute_diagnosis_confidence(category, "llm"),
                            source: "llm".to_string(),
                        }
                    }
                    None => {
                        // LLM 输出无法解析, 降级到启发式
                        warn!("LLM 诊断输出无法解析, 降级到启发式");
                        let heuristic = HeuristicErrorDiagnoser::new();
                        let mut result =
                            heuristic.diagnose(errors, feedback, context, history).await;
                        result.source = "heuristic_fallback".to_string();
                        result
                    }
                }
            }
            Err(e) => {
                // LLM 不可用, 降级到启发式
                warn!("LLM 诊断失败, 降级到启发式: {}", e);
                let heuristic = HeuristicErrorDiagnoser::new();
                let mut result = heuristic.diagnose(errors, feedback, context, history).await;
                result.source = "heuristic_fallback".to_string();
                result
            }
        }
    }
}

// ============================================================================
//  HybridErrorDiagnoser — 启发式 + LLM + 历史学习
// ============================================================================

/// 混合错误诊断器 — 启发式分类 + LLM 深度分析 + 历史学习
///
/// 策略:
/// 1. 启发式快速分类 (确定错误分类)
/// 2. LLM 深度分析 (增强根因分析和修复建议)
/// 3. 历史查询 (查找相似错误模式)
/// 4. 综合三者生成最终诊断
///
/// LLM 不可用时降级到纯启发式。
pub struct HybridErrorDiagnoser<C: LlmClient> {
    heuristic: HeuristicErrorDiagnoser,
    llm: LlmErrorDiagnoser<C>,
    /// 最大重试次数 (传递给内部 LlmErrorDiagnoser)
    max_retries: u32,
}

impl<C: LlmClient> HybridErrorDiagnoser<C> {
    pub fn new(client: C) -> Self {
        Self {
            heuristic: HeuristicErrorDiagnoser::new(),
            llm: LlmErrorDiagnoser::new(client),
            max_retries: 2,
        }
    }

    /// 设置最大重试次数 (builder 模式)
    ///
    /// 同时设置内部 LlmErrorDiagnoser 的重试次数。
    pub fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self.llm = self.llm.with_max_retries(max_retries);
        self
    }

    /// 构建历史建议文本 (委托纯函数)
    fn build_history_suggestion(similar: &[ErrorPattern]) -> String {
        format_history_suggestion(similar)
    }
}

#[async_trait]
impl<C: LlmClient> ErrorDiagnoser for HybridErrorDiagnoser<C> {
    async fn diagnose(
        &self,
        errors: &[CompileError],
        feedback: &str,
        context: &DiagnosisContext,
        history: &ErrorHistory,
    ) -> DiagnosisResult {
        // 1. 启发式快速分类
        let heuristic_result = self
            .heuristic
            .diagnose(errors, feedback, context, history)
            .await;

        // 2. 历史查询
        let similar = if !errors.is_empty() {
            history.find_similar(&errors[0])
        } else {
            vec![]
        };

        // 3. LLM 深度分析 (增强)
        let llm_result = self.llm.diagnose(errors, feedback, context, history).await;

        // 4. 综合结果
        // 使用 LLM 的分析和指导 (更精准), 启发式的分类 (更稳定)
        let category = if llm_result.category != ErrorCategory::Unknown {
            llm_result.category
        } else {
            heuristic_result.category
        };

        let history_suggestion = Self::build_history_suggestion(
            &similar.iter().map(|p| (*p).clone()).collect::<Vec<_>>(),
        );

        // 合并指导和分析 (委托纯函数)
        let fix_guidance = merge_guidance(&llm_result.fix_guidance, &history_suggestion);
        let analysis = merge_analysis(&llm_result.analysis, &heuristic_result.analysis);

        DiagnosisResult {
            category,
            analysis,
            fix_guidance,
            similar_patterns: similar.into_iter().cloned().collect(),
            confidence: compute_diagnosis_confidence(category, "hybrid"),
            source: "hybrid".to_string(),
        }
    }
}

// ============================================================================
//  MockErrorDiagnoser — 测试用
// ============================================================================

/// Mock 错误诊断器 — 预编程诊断结果, 用于测试
pub struct MockErrorDiagnoser {
    /// 预设的诊断结果
    pub result: DiagnosisResult,
}

impl MockErrorDiagnoser {
    pub fn new(result: DiagnosisResult) -> Self {
        Self { result }
    }

    /// 创建一个简单的 mock (返回指定分类)
    pub fn with_category(category: ErrorCategory) -> Self {
        Self::new(DiagnosisResult {
            category,
            analysis: format!("Mock 分析: {}", category),
            fix_guidance: "Mock 修复建议".to_string(),
            similar_patterns: vec![],
            confidence: 1.0,
            source: "mock".to_string(),
        })
    }

    /// 创建一个空结果的 mock (模拟无诊断)
    pub fn empty() -> Self {
        Self::new(DiagnosisResult::empty())
    }
}

#[async_trait]
impl ErrorDiagnoser for MockErrorDiagnoser {
    async fn diagnose(
        &self,
        _errors: &[CompileError],
        _feedback: &str,
        _context: &DiagnosisContext,
        _history: &ErrorHistory,
    ) -> DiagnosisResult {
        self.result.clone()
    }
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    // ===== MockLlmClient (复用 llm_clarify.rs 的模式) =====

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
                return Ok("CATEGORY: Unknown\nANALYSIS: \nFIX_GUIDANCE: ".to_string());
            }
            Ok(queue.remove(0))
        }

        async fn is_available(&self) -> bool {
            self.available
        }
    }

    // ===== 辅助函数 =====

    fn make_error(code: Option<&str>, msg: &str, file: &str) -> CompileError {
        CompileError {
            file: file.to_string(),
            line: Some(10),
            column: Some(5),
            message: msg.to_string(),
            error_code: code.map(String::from),
        }
    }

    fn make_ctx() -> DiagnosisContext {
        DiagnosisContext {
            task_prompt: "创建一个 CLI 工具".to_string(),
            attempt: 2,
            max_attempts: 3,
            files_written: vec!["src/main.rs".to_string()],
        }
    }

    // ===== ErrorCategory::from_error_code =====

    #[test]
    fn test_from_error_code_type_mismatch() {
        assert_eq!(
            ErrorCategory::from_error_code("E0308"),
            ErrorCategory::TypeError
        );
    }

    #[test]
    fn test_from_error_code_use_of_moved() {
        assert_eq!(
            ErrorCategory::from_error_code("E0382"),
            ErrorCategory::BorrowError
        );
    }

    #[test]
    fn test_from_error_code_cannot_find() {
        assert_eq!(
            ErrorCategory::from_error_code("E0425"),
            ErrorCategory::MissingItem
        );
    }

    #[test]
    fn test_from_error_code_unresolved_import() {
        assert_eq!(
            ErrorCategory::from_error_code("E0432"),
            ErrorCategory::ImportError
        );
    }

    #[test]
    fn test_from_error_code_trait_bound() {
        assert_eq!(
            ErrorCategory::from_error_code("E0277"),
            ErrorCategory::TypeError
        );
    }

    #[test]
    fn test_from_error_code_missing_lifetime() {
        assert_eq!(
            ErrorCategory::from_error_code("E0106"),
            ErrorCategory::LifetimeError
        );
    }

    #[test]
    fn test_from_error_code_borrow_range() {
        assert_eq!(
            ErrorCategory::from_error_code("E0502"),
            ErrorCategory::BorrowError
        );
        assert_eq!(
            ErrorCategory::from_error_code("E0596"),
            ErrorCategory::BorrowError
        );
    }

    #[test]
    fn test_from_error_code_non_exhaustive() {
        assert_eq!(
            ErrorCategory::from_error_code("E0004"),
            ErrorCategory::SyntaxError
        );
    }

    #[test]
    fn test_from_error_code_unknown_range() {
        assert_eq!(
            ErrorCategory::from_error_code("E9999"),
            ErrorCategory::Unknown
        );
    }

    #[test]
    fn test_from_error_code_unknown_prefix() {
        assert_eq!(
            ErrorCategory::from_error_code("XYZ123"),
            ErrorCategory::Unknown
        );
    }

    // ===== ErrorCategory::from_message =====

    #[test]
    fn test_from_message_syntax_error() {
        assert_eq!(
            ErrorCategory::from_message("syntax error: unexpected token"),
            ErrorCategory::SyntaxError
        );
    }

    #[test]
    fn test_from_message_mismatched_types() {
        assert_eq!(
            ErrorCategory::from_message("mismatched types: expected usize, found i32"),
            ErrorCategory::TypeError
        );
    }

    #[test]
    fn test_from_message_cannot_borrow() {
        assert_eq!(
            ErrorCategory::from_message("cannot borrow `x` as mutable"),
            ErrorCategory::BorrowError
        );
    }

    #[test]
    fn test_from_message_use_of_moved() {
        assert_eq!(
            ErrorCategory::from_message("use of moved value: `x`"),
            ErrorCategory::BorrowError
        );
    }

    #[test]
    fn test_from_message_lifetime() {
        assert_eq!(
            ErrorCategory::from_message("missing lifetime specifier"),
            ErrorCategory::LifetimeError
        );
    }

    #[test]
    fn test_from_message_python_import_error() {
        assert_eq!(
            ErrorCategory::from_message("ImportError: No module named 'foo'"),
            ErrorCategory::ImportError
        );
    }

    #[test]
    fn test_from_message_python_name_error() {
        assert_eq!(
            ErrorCategory::from_message("NameError: name 'x' is not defined"),
            ErrorCategory::MissingItem
        );
    }

    #[test]
    fn test_from_message_node_undefined() {
        assert_eq!(
            ErrorCategory::from_message("x is not defined"),
            ErrorCategory::MissingItem
        );
    }

    #[test]
    fn test_from_message_unknown() {
        assert_eq!(
            ErrorCategory::from_message("some random error"),
            ErrorCategory::Unknown
        );
    }

    // ===== ErrorCategory::from_compile_error =====

    #[test]
    fn test_from_compile_error_with_code() {
        let err = make_error(Some("E0308"), "mismatched types", "src/main.rs");
        assert_eq!(
            ErrorCategory::from_compile_error(&err),
            ErrorCategory::TypeError
        );
    }

    #[test]
    fn test_from_compile_error_without_code() {
        let err = make_error(None, "cannot borrow `x` as mutable", "src/main.rs");
        assert_eq!(
            ErrorCategory::from_compile_error(&err),
            ErrorCategory::BorrowError
        );
    }

    #[test]
    fn test_from_compile_error_code_overrides_message() {
        // error_code 和 message 不匹配时, 优先用 error_code
        let err = make_error(Some("E0308"), "cannot borrow something", "src/main.rs");
        assert_eq!(
            ErrorCategory::from_compile_error(&err),
            ErrorCategory::TypeError
        );
    }

    // ===== ErrorCategory::display_name =====

    #[test]
    fn test_display_name() {
        assert_eq!(ErrorCategory::TypeError.display_name(), "类型错误");
        assert_eq!(ErrorCategory::BorrowError.display_name(), "借用/所有权错误");
        assert_eq!(ErrorCategory::Unknown.display_name(), "未知错误");
    }

    // ===== ErrorCategory Display =====

    #[test]
    fn test_error_category_display() {
        assert_eq!(format!("{}", ErrorCategory::TypeError), "类型错误");
        assert_eq!(format!("{}", ErrorCategory::ImportError), "导入错误");
    }

    // ===== ErrorHistory: record =====

    #[test]
    fn test_history_record_new() {
        let mut h = ErrorHistory::new();
        let err = make_error(Some("E0308"), "mismatched types", "src/main.rs");
        h.record(&err, ErrorCategory::TypeError, false);

        assert_eq!(h.patterns.len(), 1);
        assert_eq!(h.patterns[0].occurrences, 1);
        assert!(!h.patterns[0].last_fix_succeeded);
        assert_eq!(h.patterns[0].category, ErrorCategory::TypeError);
    }

    #[test]
    fn test_history_record_duplicate_increments() {
        let mut h = ErrorHistory::new();
        let err = make_error(Some("E0308"), "mismatched types", "src/main.rs");
        h.record(&err, ErrorCategory::TypeError, false);
        h.record(&err, ErrorCategory::TypeError, false);
        h.record(&err, ErrorCategory::TypeError, true);

        assert_eq!(h.patterns.len(), 1);
        assert_eq!(h.patterns[0].occurrences, 3);
        assert!(h.patterns[0].last_fix_succeeded);
    }

    #[test]
    fn test_history_record_different_errors() {
        let mut h = ErrorHistory::new();
        let err1 = make_error(Some("E0308"), "mismatched types", "src/main.rs");
        let err2 = make_error(Some("E0382"), "use of moved value", "src/main.rs");
        h.record(&err1, ErrorCategory::TypeError, false);
        h.record(&err2, ErrorCategory::BorrowError, true);

        assert_eq!(h.patterns.len(), 2);
    }

    #[test]
    fn test_history_record_no_code_uses_message() {
        let mut h = ErrorHistory::new();
        let err = make_error(None, "cannot borrow `x` as mutable", "src/main.rs");
        h.record(&err, ErrorCategory::BorrowError, false);

        assert_eq!(h.patterns.len(), 1);
        assert!(h.patterns[0].error_code.is_none());
        assert!(h.patterns[0].message_signature.contains("cannot borrow"));
    }

    // ===== ErrorHistory: find_similar =====

    #[test]
    fn test_history_find_similar_by_code() {
        let mut h = ErrorHistory::new();
        let err1 = make_error(
            Some("E0308"),
            "mismatched types: usize vs i32",
            "src/main.rs",
        );
        h.record(&err1, ErrorCategory::TypeError, true);

        // 查找相同 error_code 但不同消息的错误
        let err2 = make_error(
            Some("E0308"),
            "mismatched types: String vs &str",
            "src/main.rs",
        );
        let found = h.find_similar(&err2);
        assert_eq!(found.len(), 1);
        assert!(found[0].last_fix_succeeded);
    }

    #[test]
    fn test_history_find_similar_by_signature() {
        let mut h = ErrorHistory::new();
        let err1 = make_error(None, "cannot borrow `x` as mutable", "src/main.rs");
        h.record(&err1, ErrorCategory::BorrowError, false);

        let err2 = make_error(None, "cannot borrow `x` as mutable", "src/main.rs");
        let found = h.find_similar(&err2);
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn test_history_find_similar_none() {
        let h = ErrorHistory::new();
        let err = make_error(Some("E0308"), "mismatched types", "src/main.rs");
        let found = h.find_similar(&err);
        assert!(found.is_empty());
    }

    // ===== ErrorHistory: find_successful_patterns =====

    #[test]
    fn test_history_find_successful() {
        let mut h = ErrorHistory::new();
        let err1 = make_error(Some("E0308"), "mismatched types", "src/main.rs");
        h.record(&err1, ErrorCategory::TypeError, true);

        let err2 = make_error(Some("E0382"), "use of moved", "src/main.rs");
        h.record(&err2, ErrorCategory::BorrowError, false);

        let query = make_error(Some("E0308"), "mismatched types", "src/main.rs");
        let successful = h.find_successful_patterns(&query);
        assert_eq!(successful.len(), 1);
        assert!(successful[0].last_fix_succeeded);
    }

    // ===== ErrorHistory: summary =====

    #[test]
    fn test_history_summary_empty() {
        let h = ErrorHistory::new();
        assert_eq!(h.summary(), "(无历史错误)");
    }

    #[test]
    fn test_history_summary_with_patterns() {
        let mut h = ErrorHistory::new();
        let err = make_error(Some("E0308"), "mismatched types", "src/main.rs");
        h.record(&err, ErrorCategory::TypeError, true);
        h.record(&err, ErrorCategory::TypeError, true);

        let summary = h.summary();
        assert!(summary.contains("1 个模式"));
        assert!(summary.contains("2 次出现"));
        assert!(summary.contains("1 个已修复"));
    }

    // ===== ErrorHistory: load/save =====

    #[test]
    fn test_history_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("error_history.json");

        let mut h = ErrorHistory::new();
        h.history_path = Some(path.clone());
        let err = make_error(Some("E0308"), "mismatched types", "src/main.rs");
        h.record(&err, ErrorCategory::TypeError, true);
        h.save().unwrap();

        let loaded = ErrorHistory::load(&path).unwrap();
        assert_eq!(loaded.patterns.len(), 1);
        assert_eq!(loaded.patterns[0].category, ErrorCategory::TypeError);
    }

    #[test]
    fn test_history_load_nonexistent() {
        let path = std::path::Path::new("/nonexistent/error_history.json");
        let h = ErrorHistory::load(path).unwrap();
        assert!(h.patterns.is_empty());
    }

    #[test]
    fn test_history_load_from_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let forge_dir = dir.path().join(".forge");
        std::fs::create_dir_all(&forge_dir).unwrap();
        let path = forge_dir.join("error_history.json");

        let mut h = ErrorHistory::new();
        h.history_path = Some(path);
        let err = make_error(Some("E0308"), "test error", "src/main.rs");
        h.record(&err, ErrorCategory::TypeError, false);
        h.save().unwrap();

        let loaded = ErrorHistory::load_from_workspace(dir.path());
        assert_eq!(loaded.patterns.len(), 1);
    }

    #[test]
    fn test_history_load_from_workspace_nonexistent() {
        let dir = tempfile::tempdir().unwrap();
        let h = ErrorHistory::load_from_workspace(dir.path());
        assert!(h.patterns.is_empty());
    }

    #[test]
    fn test_history_clear() {
        let mut h = ErrorHistory::new();
        let err = make_error(Some("E0308"), "test", "src/main.rs");
        h.record(&err, ErrorCategory::TypeError, false);
        assert_eq!(h.patterns.len(), 1);
        h.clear();
        assert!(h.patterns.is_empty());
    }

    // ===== HeuristicErrorDiagnoser =====

    #[tokio::test]
    async fn test_heuristic_diagnose_type_error() {
        let diagnoser = HeuristicErrorDiagnoser::new();
        let errors = vec![make_error(Some("E0308"), "mismatched types", "src/main.rs")];
        let result = diagnoser
            .diagnose(&errors, "feedback", &make_ctx(), &ErrorHistory::new())
            .await;

        assert_eq!(result.category, ErrorCategory::TypeError);
        assert!(!result.analysis.is_empty());
        assert!(result.fix_guidance.contains("类型"));
        assert_eq!(result.source, "heuristic");
        assert!(result.confidence > 0.0);
    }

    #[tokio::test]
    async fn test_heuristic_diagnose_borrow_error() {
        let diagnoser = HeuristicErrorDiagnoser::new();
        let errors = vec![make_error(
            Some("E0382"),
            "use of moved value",
            "src/main.rs",
        )];
        let result = diagnoser
            .diagnose(&errors, "feedback", &make_ctx(), &ErrorHistory::new())
            .await;

        assert_eq!(result.category, ErrorCategory::BorrowError);
        assert!(result.fix_guidance.contains("借用"));
    }

    #[tokio::test]
    async fn test_heuristic_diagnose_import_error() {
        let diagnoser = HeuristicErrorDiagnoser::new();
        let errors = vec![make_error(
            Some("E0432"),
            "unresolved import",
            "src/main.rs",
        )];
        let result = diagnoser
            .diagnose(&errors, "feedback", &make_ctx(), &ErrorHistory::new())
            .await;

        assert_eq!(result.category, ErrorCategory::ImportError);
        assert!(result.fix_guidance.contains("导入"));
    }

    #[tokio::test]
    async fn test_heuristic_diagnose_empty_errors() {
        let diagnoser = HeuristicErrorDiagnoser::new();
        let result = diagnoser
            .diagnose(
                &[],
                "test failure feedback",
                &make_ctx(),
                &ErrorHistory::new(),
            )
            .await;

        assert_eq!(result.category, ErrorCategory::TestFailure);
    }

    #[tokio::test]
    async fn test_heuristic_diagnose_unknown_error() {
        let diagnoser = HeuristicErrorDiagnoser::new();
        let errors = vec![make_error(Some("E9999"), "unknown error", "src/main.rs")];
        let result = diagnoser
            .diagnose(&errors, "feedback", &make_ctx(), &ErrorHistory::new())
            .await;

        assert_eq!(result.category, ErrorCategory::Unknown);
        assert!(result.fix_guidance.is_empty(), "未知错误不应提供指导");
        assert!(result.confidence < 0.5);
    }

    #[tokio::test]
    async fn test_heuristic_diagnose_with_history() {
        let mut history = ErrorHistory::new();
        let err = make_error(Some("E0308"), "mismatched types", "src/main.rs");
        history.record(&err, ErrorCategory::TypeError, true);

        let diagnoser = HeuristicErrorDiagnoser::new();
        let errors = vec![make_error(Some("E0308"), "mismatched types", "src/main.rs")];
        let result = diagnoser
            .diagnose(&errors, "feedback", &make_ctx(), &history)
            .await;

        assert!(!result.similar_patterns.is_empty(), "应找到历史相似错误");
    }

    // ===== LlmErrorDiagnoser =====

    #[tokio::test]
    async fn test_llm_diagnose_success() {
        let client = MockLlmClient::single(
            "CATEGORY: TypeError\n\
             ANALYSIS: 变量类型声明为 usize 但赋值为 i32\n\
             FIX_GUIDANCE: 将 42i32 改为 42usize",
        );
        let diagnoser = LlmErrorDiagnoser::new(client);
        let errors = vec![make_error(Some("E0308"), "mismatched types", "src/main.rs")];
        let result = diagnoser
            .diagnose(&errors, "feedback", &make_ctx(), &ErrorHistory::new())
            .await;

        assert_eq!(result.category, ErrorCategory::TypeError);
        assert!(result.analysis.contains("变量类型"));
        assert!(result.fix_guidance.contains("42usize"));
        assert_eq!(result.source, "llm");
        assert!(result.confidence > 0.8);
    }

    #[tokio::test]
    async fn test_llm_diagnose_unavailable_fallback() {
        let client = MockLlmClient::unavailable();
        let diagnoser = LlmErrorDiagnoser::new(client);
        let errors = vec![make_error(Some("E0308"), "mismatched types", "src/main.rs")];
        let result = diagnoser
            .diagnose(&errors, "feedback", &make_ctx(), &ErrorHistory::new())
            .await;

        // LLM 不可用时应降级到启发式
        assert_eq!(result.source, "heuristic_fallback");
        assert_eq!(result.category, ErrorCategory::TypeError);
    }

    #[tokio::test]
    async fn test_llm_diagnose_unparsable_fallback() {
        let client = MockLlmClient::single("这是一段无法解析的文本");
        let diagnoser = LlmErrorDiagnoser::new(client);
        let errors = vec![make_error(Some("E0308"), "mismatched types", "src/main.rs")];
        let result = diagnoser
            .diagnose(&errors, "feedback", &make_ctx(), &ErrorHistory::new())
            .await;

        assert_eq!(result.source, "heuristic_fallback");
    }

    #[tokio::test]
    async fn test_llm_diagnose_empty_errors() {
        let client = MockLlmClient::unavailable();
        let diagnoser = LlmErrorDiagnoser::new(client);
        let result = diagnoser
            .diagnose(&[], "test failure", &make_ctx(), &ErrorHistory::new())
            .await;

        // 无编译错误时交给启发式
        assert_eq!(result.category, ErrorCategory::TestFailure);
    }

    // ===== LlmErrorDiagnoser: parse_diagnosis =====

    #[test]
    fn test_parse_diagnosis_full() {
        let client = MockLlmClient::unavailable();
        let diagnoser = LlmErrorDiagnoser::new(client);
        let result = diagnoser.parse_diagnosis(
            "CATEGORY: BorrowError\n\
             ANALYSIS: 变量所有权已转移\n\
             FIX_GUIDANCE: 使用 clone() 复制值",
        );

        assert!(result.is_some());
        let (cat, analysis, guidance) = result.unwrap();
        assert_eq!(cat, ErrorCategory::BorrowError);
        assert!(analysis.contains("所有权"));
        assert!(guidance.contains("clone"));
    }

    #[test]
    fn test_parse_diagnosis_case_insensitive() {
        let client = MockLlmClient::unavailable();
        let diagnoser = LlmErrorDiagnoser::new(client);
        let result = diagnoser.parse_diagnosis(
            "category: ImportError\n\
             analysis: 模块未找到\n\
             fix_guidance: 检查 use 路径",
        );

        assert!(result.is_some());
        let (cat, _, _) = result.unwrap();
        assert_eq!(cat, ErrorCategory::ImportError);
    }

    #[test]
    fn test_parse_diagnosis_missing_fields() {
        let client = MockLlmClient::unavailable();
        let diagnoser = LlmErrorDiagnoser::new(client);
        let result = diagnoser.parse_diagnosis("CATEGORY: Unknown");

        // 只有 CATEGORY, 没有其他字段
        assert!(result.is_some());
        let (cat, analysis, guidance) = result.unwrap();
        assert_eq!(cat, ErrorCategory::Unknown);
        assert!(analysis.is_empty());
        assert!(guidance.is_empty());
    }

    #[test]
    fn test_parse_diagnosis_unparsable() {
        let client = MockLlmClient::unavailable();
        let diagnoser = LlmErrorDiagnoser::new(client);
        let result = diagnoser.parse_diagnosis("完全无法解析的文本");
        assert!(result.is_none());
    }

    // ===== LlmErrorDiagnoser: build_diagnosis_prompt =====

    #[test]
    fn test_build_diagnosis_prompt_contains_errors() {
        let client = MockLlmClient::unavailable();
        let diagnoser = LlmErrorDiagnoser::new(client);
        let errors = vec![make_error(Some("E0308"), "mismatched types", "src/main.rs")];
        let prompt = diagnoser.build_diagnosis_prompt(&errors, &make_ctx());

        assert!(prompt.contains("E0308"));
        assert!(prompt.contains("mismatched types"));
        assert!(prompt.contains("src/main.rs"));
        assert!(prompt.contains("CATEGORY"));
        assert!(prompt.contains("ANALYSIS"));
        assert!(prompt.contains("FIX_GUIDANCE"));
    }

    #[test]
    fn test_build_diagnosis_prompt_truncates_many_errors() {
        let client = MockLlmClient::unavailable();
        let diagnoser = LlmErrorDiagnoser::new(client);
        let errors: Vec<CompileError> = (0..10)
            .map(|i| make_error(Some("E0308"), &format!("error {}", i), "src/main.rs"))
            .collect();
        let prompt = diagnoser.build_diagnosis_prompt(&errors, &make_ctx());
        assert!(prompt.contains("还有 5 个错误"));
    }

    // ===== HybridErrorDiagnoser =====

    #[tokio::test]
    async fn test_hybrid_diagnose_with_llm() {
        let client = MockLlmClient::single(
            "CATEGORY: TypeError\n\
             ANALYSIS: 类型不匹配\n\
             FIX_GUIDANCE: 改为 usize 类型",
        );
        let diagnoser = HybridErrorDiagnoser::new(client);
        let errors = vec![make_error(Some("E0308"), "mismatched types", "src/main.rs")];
        let result = diagnoser
            .diagnose(&errors, "feedback", &make_ctx(), &ErrorHistory::new())
            .await;

        assert_eq!(result.category, ErrorCategory::TypeError);
        assert!(result.fix_guidance.contains("usize"));
        assert_eq!(result.source, "hybrid");
        assert!(result.confidence > 0.85);
    }

    #[tokio::test]
    async fn test_hybrid_diagnose_llm_unavailable() {
        let client = MockLlmClient::unavailable();
        let diagnoser = HybridErrorDiagnoser::new(client);
        let errors = vec![make_error(Some("E0308"), "mismatched types", "src/main.rs")];
        let result = diagnoser
            .diagnose(&errors, "feedback", &make_ctx(), &ErrorHistory::new())
            .await;

        // LLM 不可用时, hybrid 仍应返回分类 (来自启发式)
        assert_eq!(result.category, ErrorCategory::TypeError);
        assert!(!result.analysis.is_empty());
    }

    #[tokio::test]
    async fn test_hybrid_diagnose_with_history() {
        let mut history = ErrorHistory::new();
        let err1 = make_error(Some("E0308"), "mismatched types", "src/main.rs");
        history.record(&err1, ErrorCategory::TypeError, true);

        let client = MockLlmClient::single(
            "CATEGORY: TypeError\n\
             ANALYSIS: 类型不匹配\n\
             FIX_GUIDANCE: 修改类型",
        );
        let diagnoser = HybridErrorDiagnoser::new(client);
        let errors = vec![make_error(Some("E0308"), "mismatched types", "src/main.rs")];
        let result = diagnoser
            .diagnose(&errors, "feedback", &make_ctx(), &history)
            .await;

        assert!(!result.similar_patterns.is_empty(), "应包含历史相似错误");
        assert!(
            result.fix_guidance.contains("相似历史错误") || result.fix_guidance.contains("usize"),
            "指导应包含历史建议或 LLM 建议"
        );
    }

    // ===== MockErrorDiagnoser =====

    #[tokio::test]
    async fn test_mock_diagnoser() {
        let diagnoser = MockErrorDiagnoser::with_category(ErrorCategory::BorrowError);
        let result = diagnoser
            .diagnose(&[], "feedback", &make_ctx(), &ErrorHistory::new())
            .await;

        assert_eq!(result.category, ErrorCategory::BorrowError);
        assert_eq!(result.source, "mock");
        assert!(!result.fix_guidance.is_empty());
    }

    #[tokio::test]
    async fn test_mock_diagnoser_empty() {
        let diagnoser = MockErrorDiagnoser::empty();
        let result = diagnoser
            .diagnose(&[], "feedback", &make_ctx(), &ErrorHistory::new())
            .await;

        assert_eq!(result.category, ErrorCategory::Unknown);
        assert!(!result.has_guidance());
    }

    // ===== DiagnosisResult =====

    #[test]
    fn test_diagnosis_result_empty() {
        let r = DiagnosisResult::empty();
        assert!(!r.has_guidance());
        assert_eq!(r.confidence, 0.0);
        assert_eq!(r.source, "none");
    }

    #[test]
    fn test_diagnosis_result_has_guidance() {
        let r = DiagnosisResult {
            category: ErrorCategory::TypeError,
            analysis: "test".to_string(),
            fix_guidance: "fix it".to_string(),
            similar_patterns: vec![],
            confidence: 0.8,
            source: "test".to_string(),
        };
        assert!(r.has_guidance());
    }

    // ===== extract_error_code_from_text =====

    #[test]
    fn test_extract_error_code_bracket_format() {
        assert_eq!(
            extract_error_code_from_text("error[E0308]: mismatched types"),
            Some("E0308".to_string())
        );
    }

    #[test]
    fn test_extract_error_code_bracket_lowercase() {
        assert_eq!(
            extract_error_code_from_text("error[e0308]: mismatched types"),
            Some("E0308".to_string())
        );
    }

    #[test]
    fn test_extract_error_code_line_start() {
        assert_eq!(
            extract_error_code_from_text("E0308: mismatched types"),
            Some("E0308".to_string())
        );
    }

    #[test]
    fn test_extract_error_code_no_code() {
        assert_eq!(extract_error_code_from_text("some random error"), None);
    }

    #[test]
    fn test_extract_error_code_empty() {
        assert_eq!(extract_error_code_from_text(""), None);
    }

    #[test]
    fn test_extract_error_code_short_code() {
        // E1 是有效的短 error code 格式 (E + 数字)
        assert_eq!(
            extract_error_code_from_text("error[E1]: test"),
            Some("E1".to_string())
        );
    }

    #[test]
    fn test_extract_error_code_non_digit_suffix() {
        assert_eq!(extract_error_code_from_text("error[EABCD]: test"), None);
    }

    #[test]
    fn test_extract_error_code_multiline() {
        let msg = "some preamble\nE0308: mismatched types\nmore text";
        assert_eq!(extract_error_code_from_text(msg), Some("E0308".to_string()));
    }

    #[test]
    fn test_extract_error_code_no_bracket_but_line_start() {
        assert_eq!(
            extract_error_code_from_text("E0432 unresolved import"),
            Some("E0432".to_string())
        );
    }

    // ===== format_error_location =====

    #[test]
    fn test_format_error_location_full() {
        let err = make_error(Some("E0308"), "test", "src/main.rs");
        assert_eq!(format_error_location(&err), "src/main.rs:10:5");
    }

    #[test]
    fn test_format_error_location_line_only() {
        let err = CompileError {
            file: "src/lib.rs".to_string(),
            line: Some(42),
            column: None,
            message: "test".to_string(),
            error_code: None,
        };
        assert_eq!(format_error_location(&err), "src/lib.rs:42");
    }

    #[test]
    fn test_format_error_location_no_line_col() {
        let err = CompileError {
            file: "src/lib.rs".to_string(),
            line: None,
            column: None,
            message: "test".to_string(),
            error_code: None,
        };
        assert_eq!(format_error_location(&err), "src/lib.rs");
    }

    #[test]
    fn test_format_error_location_col_only() {
        let err = CompileError {
            file: "src/lib.rs".to_string(),
            line: None,
            column: Some(3),
            message: "test".to_string(),
            error_code: None,
        };
        assert_eq!(format_error_location(&err), "src/lib.rs");
    }

    // ===== generate_guidance_for_category =====

    #[test]
    fn test_generate_guidance_type_error() {
        let g = generate_guidance_for_category(ErrorCategory::TypeError);
        assert!(g.contains("类型"));
        assert!(!g.is_empty());
    }

    #[test]
    fn test_generate_guidance_borrow_error() {
        let g = generate_guidance_for_category(ErrorCategory::BorrowError);
        assert!(g.contains("借用"));
    }

    #[test]
    fn test_generate_guidance_lifetime_error() {
        let g = generate_guidance_for_category(ErrorCategory::LifetimeError);
        assert!(g.contains("生命周期"));
    }

    #[test]
    fn test_generate_guidance_missing_item() {
        let g = generate_guidance_for_category(ErrorCategory::MissingItem);
        assert!(g.contains("找不到"));
    }

    #[test]
    fn test_generate_guidance_import_error() {
        let g = generate_guidance_for_category(ErrorCategory::ImportError);
        assert!(g.contains("导入"));
    }

    #[test]
    fn test_generate_guidance_trait_error() {
        let g = generate_guidance_for_category(ErrorCategory::TraitError);
        assert!(g.contains("Trait"));
    }

    #[test]
    fn test_generate_guidance_syntax_error() {
        let g = generate_guidance_for_category(ErrorCategory::SyntaxError);
        assert!(g.contains("语法"));
    }

    #[test]
    fn test_generate_guidance_test_failure() {
        let g = generate_guidance_for_category(ErrorCategory::TestFailure);
        assert!(g.contains("测试"));
    }

    #[test]
    fn test_generate_guidance_e2e_failure() {
        let g = generate_guidance_for_category(ErrorCategory::E2EFailure);
        assert!(g.contains("E2E"));
    }

    #[test]
    fn test_generate_guidance_unknown_empty() {
        let g = generate_guidance_for_category(ErrorCategory::Unknown);
        assert!(g.is_empty());
    }

    // ===== generate_analysis_text =====

    #[test]
    fn test_generate_analysis_text_empty_errors() {
        let result = generate_analysis_text(&[], ErrorCategory::TestFailure);
        assert!(result.contains("无编译错误"));
    }

    #[test]
    fn test_generate_analysis_text_with_errors() {
        let errors = vec![make_error(Some("E0308"), "mismatched types", "src/main.rs")];
        let result = generate_analysis_text(&errors, ErrorCategory::TypeError);
        assert!(result.contains("类型错误"));
        assert!(result.contains("src/main.rs:10:5"));
        assert!(result.contains("[E0308]"));
        assert!(result.contains("mismatched types"));
    }

    #[test]
    fn test_generate_analysis_text_no_code() {
        let errors = vec![make_error(None, "syntax error", "src/lib.rs")];
        let result = generate_analysis_text(&errors, ErrorCategory::SyntaxError);
        assert!(result.contains("语法错误"));
        assert!(!result.contains("[]"));
    }

    // ===== format_errors_for_prompt =====

    #[test]
    fn test_format_errors_for_prompt_single() {
        let errors = vec![make_error(Some("E0308"), "test error", "src/main.rs")];
        let (text, truncated) = format_errors_for_prompt(&errors);
        assert!(text.contains("E0308"));
        assert!(text.contains("src/main.rs"));
        assert!(text.contains("test error"));
        assert!(truncated.is_none());
    }

    #[test]
    fn test_format_errors_for_prompt_truncation() {
        let errors: Vec<CompileError> = (0..8)
            .map(|i| make_error(Some("E0308"), &format!("error {}", i), "src/main.rs"))
            .collect();
        let (text, truncated) = format_errors_for_prompt(&errors);
        assert!(text.contains("error 0"));
        assert!(text.contains("error 4"));
        assert!(!text.contains("error 5"));
        assert_eq!(truncated, Some(3));
    }

    #[test]
    fn test_format_errors_for_prompt_empty() {
        let (text, truncated) = format_errors_for_prompt(&[]);
        assert!(text.is_empty());
        assert!(truncated.is_none());
    }

    #[test]
    fn test_format_errors_for_prompt_no_code() {
        let errors = vec![make_error(None, "some error", "src/lib.rs")];
        let (text, truncated) = format_errors_for_prompt(&errors);
        assert!(!text.contains("[]"));
        assert!(text.contains("some error"));
        assert!(truncated.is_none());
    }

    #[test]
    fn test_format_errors_for_prompt_exactly_five() {
        let errors: Vec<CompileError> = (0..5)
            .map(|i| make_error(Some("E0308"), &format!("error {}", i), "src/main.rs"))
            .collect();
        let (_, truncated) = format_errors_for_prompt(&errors);
        assert_eq!(truncated, None);
    }

    #[test]
    fn test_format_errors_for_prompt_six() {
        let errors: Vec<CompileError> = (0..6)
            .map(|i| make_error(Some("E0308"), &format!("error {}", i), "src/main.rs"))
            .collect();
        let (_, truncated) = format_errors_for_prompt(&errors);
        assert_eq!(truncated, Some(1));
    }

    // ===== extract_field_value =====

    #[test]
    fn test_extract_field_value_found() {
        let text = "CATEGORY: TypeError\nANALYSIS: test";
        assert_eq!(
            extract_field_value(text, "CATEGORY"),
            Some("TypeError".to_string())
        );
    }

    #[test]
    fn test_extract_field_value_case_insensitive() {
        let text = "category: BorrowError";
        assert_eq!(
            extract_field_value(text, "CATEGORY"),
            Some("BorrowError".to_string())
        );
    }

    #[test]
    fn test_extract_field_value_chinese_colon() {
        let text = "CATEGORY： TypeError";
        assert_eq!(
            extract_field_value(text, "CATEGORY"),
            Some("TypeError".to_string())
        );
    }

    #[test]
    fn test_extract_field_value_not_found() {
        let text = "some random text";
        assert_eq!(extract_field_value(text, "CATEGORY"), None);
    }

    #[test]
    fn test_extract_field_value_empty_value() {
        let text = "CATEGORY: \nANALYSIS: test";
        assert_eq!(extract_field_value(text, "CATEGORY"), None);
    }

    #[test]
    fn test_extract_field_value_multiline() {
        let text = "CATEGORY: TypeError\nANALYSIS: root cause\nFIX_GUIDANCE: fix it";
        assert_eq!(
            extract_field_value(text, "ANALYSIS"),
            Some("root cause".to_string())
        );
        assert_eq!(
            extract_field_value(text, "FIX_GUIDANCE"),
            Some("fix it".to_string())
        );
    }

    // ===== parse_llm_diagnosis =====

    #[test]
    fn test_parse_llm_diagnosis_full() {
        let result = parse_llm_diagnosis(
            "CATEGORY: BorrowError\n\
             ANALYSIS: 变量所有权已转移\n\
             FIX_GUIDANCE: 使用 clone() 复制值",
        );
        assert!(result.is_some());
        let (cat, analysis, guidance) = result.unwrap();
        assert_eq!(cat, ErrorCategory::BorrowError);
        assert!(analysis.contains("所有权"));
        assert!(guidance.contains("clone"));
    }

    #[test]
    fn test_parse_llm_diagnosis_case_insensitive() {
        let result = parse_llm_diagnosis(
            "category: ImportError\n\
             analysis: 模块未找到\n\
             fix_guidance: 检查 use 路径",
        );
        assert!(result.is_some());
        let (cat, _, _) = result.unwrap();
        assert_eq!(cat, ErrorCategory::ImportError);
    }

    #[test]
    fn test_parse_llm_diagnosis_missing_fields() {
        let result = parse_llm_diagnosis("CATEGORY: Unknown");
        assert!(result.is_some());
        let (cat, analysis, guidance) = result.unwrap();
        assert_eq!(cat, ErrorCategory::Unknown);
        assert!(analysis.is_empty());
        assert!(guidance.is_empty());
    }

    #[test]
    fn test_parse_llm_diagnosis_unparsable() {
        let result = parse_llm_diagnosis("完全无法解析的文本");
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_llm_diagnosis_empty() {
        let result = parse_llm_diagnosis("");
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_llm_diagnosis_whitespace_only() {
        let result = parse_llm_diagnosis("   \n  \n  ");
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_llm_diagnosis_chinese_colon() {
        let result = parse_llm_diagnosis("CATEGORY： TypeError\nANALYSIS： test");
        assert!(result.is_some());
        let (cat, _, _) = result.unwrap();
        assert_eq!(cat, ErrorCategory::TypeError);
    }

    #[test]
    fn test_parse_llm_diagnosis_extra_whitespace() {
        let result = parse_llm_diagnosis(
            "CATEGORY:    TypeError\n\
             ANALYSIS:    extra spaces\n\
             FIX_GUIDANCE:    fix it",
        );
        assert!(result.is_some());
        let (cat, analysis, guidance) = result.unwrap();
        assert_eq!(cat, ErrorCategory::TypeError);
        assert_eq!(analysis, "extra spaces");
        assert_eq!(guidance, "fix it");
    }

    // ===== format_history_suggestion =====

    #[test]
    fn test_format_history_suggestion_empty() {
        let result = format_history_suggestion(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_format_history_suggestion_single() {
        let now = Utc::now();
        let pattern = ErrorPattern {
            error_code: Some("E0308".to_string()),
            message_signature: "[E0308] mismatched".to_string(),
            category: ErrorCategory::TypeError,
            occurrences: 3,
            first_seen: now,
            last_seen: now,
            last_fix_succeeded: true,
            suggested_approach: Some("改为 usize".to_string()),
        };
        let result = format_history_suggestion(&[pattern]);
        assert!(result.contains("相似历史错误"));
        assert!(result.contains("E0308"));
        assert!(result.contains("已修复"));
        assert!(result.contains("3"));
    }

    #[test]
    fn test_format_history_suggestion_unfixed() {
        let now = Utc::now();
        let pattern = ErrorPattern {
            error_code: None,
            message_signature: "test sig".to_string(),
            category: ErrorCategory::BorrowError,
            occurrences: 1,
            first_seen: now,
            last_seen: now,
            last_fix_succeeded: false,
            suggested_approach: None,
        };
        let result = format_history_suggestion(&[pattern]);
        assert!(result.contains("未修复"));
        assert!(result.contains("N/A"));
    }

    #[test]
    fn test_format_history_suggestion_max_three() {
        let now = Utc::now();
        let patterns: Vec<ErrorPattern> = (0..5)
            .map(|i| ErrorPattern {
                error_code: Some(format!("E{}", 100 + i)),
                message_signature: format!("sig {}", i),
                category: ErrorCategory::TypeError,
                occurrences: 1,
                first_seen: now,
                last_seen: now,
                last_fix_succeeded: true,
                suggested_approach: None,
            })
            .collect();
        let result = format_history_suggestion(&patterns);
        // 只取前 3 个
        assert!(result.contains("E100"));
        assert!(result.contains("E101"));
        assert!(result.contains("E102"));
        assert!(!result.contains("E103"));
    }

    #[test]
    fn test_format_history_suggestion_with_approach() {
        let now = Utc::now();
        let long_approach = "a".repeat(200);
        let pattern = ErrorPattern {
            error_code: Some("E0308".to_string()),
            message_signature: "sig".to_string(),
            category: ErrorCategory::TypeError,
            occurrences: 1,
            first_seen: now,
            last_seen: now,
            last_fix_succeeded: true,
            suggested_approach: Some(long_approach),
        };
        let result = format_history_suggestion(&[pattern]);
        // 建议应截断到 100 字符
        let approach_line = result.lines().find(|l| l.contains("建议")).unwrap();
        assert!(approach_line.chars().filter(|c| *c == 'a').count() <= 100);
    }

    // ===== compute_diagnosis_confidence =====

    #[test]
    fn test_confidence_hybrid() {
        assert_eq!(
            compute_diagnosis_confidence(ErrorCategory::TypeError, "hybrid"),
            0.9
        );
    }

    #[test]
    fn test_confidence_llm() {
        assert_eq!(
            compute_diagnosis_confidence(ErrorCategory::TypeError, "llm"),
            0.85
        );
    }

    #[test]
    fn test_confidence_heuristic_known() {
        assert_eq!(
            compute_diagnosis_confidence(ErrorCategory::TypeError, "heuristic"),
            0.7
        );
    }

    #[test]
    fn test_confidence_heuristic_unknown() {
        assert_eq!(
            compute_diagnosis_confidence(ErrorCategory::Unknown, "heuristic"),
            0.3
        );
    }

    #[test]
    fn test_confidence_heuristic_fallback_known() {
        assert_eq!(
            compute_diagnosis_confidence(ErrorCategory::TypeError, "heuristic_fallback"),
            0.6
        );
    }

    #[test]
    fn test_confidence_heuristic_fallback_unknown() {
        assert_eq!(
            compute_diagnosis_confidence(ErrorCategory::Unknown, "heuristic_fallback"),
            0.25
        );
    }

    #[test]
    fn test_confidence_mock() {
        assert_eq!(
            compute_diagnosis_confidence(ErrorCategory::TypeError, "mock"),
            1.0
        );
    }

    #[test]
    fn test_confidence_none() {
        assert_eq!(
            compute_diagnosis_confidence(ErrorCategory::TypeError, "none"),
            0.0
        );
    }

    #[test]
    fn test_confidence_unknown_source() {
        assert_eq!(
            compute_diagnosis_confidence(ErrorCategory::TypeError, "random"),
            0.0
        );
    }

    // ===== merge_guidance =====

    #[test]
    fn test_merge_guidance_both_present() {
        let result = merge_guidance("LLM 建议", "历史建议");
        assert!(result.contains("LLM 建议"));
        assert!(result.contains("历史建议"));
        assert!(result.contains("\n\n"));
    }

    #[test]
    fn test_merge_guidance_llm_only() {
        let result = merge_guidance("LLM 建议", "");
        assert_eq!(result, "LLM 建议");
    }

    #[test]
    fn test_merge_guidance_history_only() {
        let result = merge_guidance("", "历史建议");
        assert_eq!(result, "历史建议");
    }

    #[test]
    fn test_merge_guidance_both_empty() {
        let result = merge_guidance("", "");
        assert!(result.is_empty());
    }

    // ===== merge_analysis =====

    #[test]
    fn test_merge_analysis_both_present() {
        let result = merge_analysis("LLM 分析", "启发式分析");
        assert!(result.contains("LLM 分析"));
        assert!(result.contains("启发式分析"));
        assert!(result.contains('\n'));
    }

    #[test]
    fn test_merge_analysis_llm_only() {
        let result = merge_analysis("LLM 分析", "");
        assert_eq!(result, "LLM 分析");
    }

    #[test]
    fn test_merge_analysis_heuristic_only() {
        let result = merge_analysis("", "启发式分析");
        assert_eq!(result, "启发式分析");
    }

    #[test]
    fn test_merge_analysis_both_empty() {
        let result = merge_analysis("", "");
        assert!(result.is_empty());
    }

    // ===== from_message: 新增多语言错误模式 =====

    #[test]
    fn test_from_message_python_attribute_error() {
        assert_eq!(
            ErrorCategory::from_message("AttributeError: 'str' object has no attribute 'foo'"),
            ErrorCategory::TypeError
        );
    }

    #[test]
    fn test_from_message_python_value_error() {
        assert_eq!(
            ErrorCategory::from_message("ValueError: invalid literal for int()"),
            ErrorCategory::TypeError
        );
    }

    #[test]
    fn test_from_message_python_key_error() {
        assert_eq!(
            ErrorCategory::from_message("KeyError: 'nonexistent'"),
            ErrorCategory::MissingItem
        );
    }

    #[test]
    fn test_from_message_python_index_error() {
        assert_eq!(
            ErrorCategory::from_message("IndexError: list index out of range"),
            ErrorCategory::MissingItem
        );
    }

    #[test]
    fn test_from_message_node_not_a_function() {
        assert_eq!(
            ErrorCategory::from_message("TypeError: x is not a function"),
            ErrorCategory::TypeError
        );
    }

    #[test]
    fn test_from_message_node_not_iterable() {
        assert_eq!(
            ErrorCategory::from_message("x is not iterable"),
            ErrorCategory::TypeError
        );
    }

    // ===== LlmErrorDiagnoser: with_max_retries builder =====

    #[test]
    fn test_llm_diagnoser_with_max_retries() {
        let client = MockLlmClient::unavailable();
        let diagnoser = LlmErrorDiagnoser::new(client).with_max_retries(5);
        assert_eq!(diagnoser.max_retries, 5);
    }

    #[test]
    fn test_llm_diagnoser_default_max_retries() {
        let client = MockLlmClient::unavailable();
        let diagnoser = LlmErrorDiagnoser::new(client);
        assert_eq!(diagnoser.max_retries, 2);
    }

    #[test]
    fn test_llm_diagnoser_zero_retries() {
        let client = MockLlmClient::unavailable();
        let diagnoser = LlmErrorDiagnoser::new(client).with_max_retries(0);
        assert_eq!(diagnoser.max_retries, 0);
    }

    // ===== LlmErrorDiagnoser: retry mechanism =====

    #[tokio::test]
    async fn test_llm_diagnose_retry_success() {
        // 第1次失败 (连接拒绝), 第2次成功
        let client = SequenceLlmClient::new(vec![
            Err(anyhow!("Connection refused")),
            Ok("CATEGORY: TypeError\nANALYSIS: type issue\nFIX_GUIDANCE: use usize".to_string()),
        ]);
        let diagnoser = LlmErrorDiagnoser::new(client).with_max_retries(2);
        let errors = vec![make_error(Some("E0308"), "mismatched types", "src/main.rs")];
        let result = diagnoser
            .diagnose(&errors, "feedback", &make_ctx(), &ErrorHistory::new())
            .await;

        assert_eq!(result.category, ErrorCategory::TypeError);
        assert_eq!(result.source, "llm");
        assert!(result.fix_guidance.contains("usize"));
    }

    #[tokio::test]
    async fn test_llm_diagnose_retry_exhausted() {
        // 全部失败 (连接拒绝)
        let client = SequenceLlmClient::new(vec![
            Err(anyhow!("Connection refused")),
            Err(anyhow!("Connection refused")),
            Err(anyhow!("Connection refused")),
        ]);
        let diagnoser = LlmErrorDiagnoser::new(client).with_max_retries(2);
        let errors = vec![make_error(Some("E0308"), "mismatched types", "src/main.rs")];
        let result = diagnoser
            .diagnose(&errors, "feedback", &make_ctx(), &ErrorHistory::new())
            .await;

        // 重试耗尽后降级到启发式
        assert_eq!(result.source, "heuristic_fallback");
        assert_eq!(result.category, ErrorCategory::TypeError);
    }

    #[tokio::test]
    async fn test_llm_diagnose_no_retry_on_parse_error() {
        // ParseError 不应重试 — 只提供1个失败, 确认不重试
        let client = SequenceLlmClient::new(vec![Err(anyhow!("HTTP 404 error"))]);
        let diagnoser = LlmErrorDiagnoser::new(client).with_max_retries(3);
        let errors = vec![make_error(Some("E0308"), "mismatched types", "src/main.rs")];
        let result = diagnoser
            .diagnose(&errors, "feedback", &make_ctx(), &ErrorHistory::new())
            .await;

        // HTTP错误不重试, 直接降级
        assert_eq!(result.source, "heuristic_fallback");
    }

    #[tokio::test]
    async fn test_llm_diagnose_zero_retries_no_retry() {
        // max_retries=0 时不重试
        let client = SequenceLlmClient::new(vec![Err(anyhow!("Connection refused"))]);
        let diagnoser = LlmErrorDiagnoser::new(client).with_max_retries(0);
        let errors = vec![make_error(Some("E0308"), "mismatched types", "src/main.rs")];
        let result = diagnoser
            .diagnose(&errors, "feedback", &make_ctx(), &ErrorHistory::new())
            .await;

        assert_eq!(result.source, "heuristic_fallback");
    }

    // ===== HybridErrorDiagnoser: with_max_retries =====

    #[test]
    fn test_hybrid_diagnoser_with_max_retries() {
        let client = MockLlmClient::unavailable();
        let diagnoser = HybridErrorDiagnoser::new(client).with_max_retries(5);
        assert_eq!(diagnoser.max_retries, 5);
        assert_eq!(diagnoser.llm.max_retries, 5);
    }

    #[test]
    fn test_hybrid_diagnoser_default_max_retries() {
        let client = MockLlmClient::unavailable();
        let diagnoser = HybridErrorDiagnoser::new(client);
        assert_eq!(diagnoser.max_retries, 2);
        assert_eq!(diagnoser.llm.max_retries, 2);
    }

    #[tokio::test]
    async fn test_hybrid_diagnose_retry_success() {
        let client = SequenceLlmClient::new(vec![
            Err(anyhow!("Connection refused")),
            Ok("CATEGORY: TypeError\nANALYSIS: type issue\nFIX_GUIDANCE: use usize".to_string()),
        ]);
        let diagnoser = HybridErrorDiagnoser::new(client).with_max_retries(2);
        let errors = vec![make_error(Some("E0308"), "mismatched types", "src/main.rs")];
        let result = diagnoser
            .diagnose(&errors, "feedback", &make_ctx(), &ErrorHistory::new())
            .await;

        assert_eq!(result.category, ErrorCategory::TypeError);
        assert_eq!(result.source, "hybrid");
        assert!(result.fix_guidance.contains("usize"));
    }

    // ===== 方法委托测试 =====

    #[test]
    fn test_heuristic_generate_guidance_delegates() {
        // 验证 generate_guidance 方法正确委托纯函数
        let g1 = HeuristicErrorDiagnoser::generate_guidance(ErrorCategory::TypeError);
        let g2 = generate_guidance_for_category(ErrorCategory::TypeError);
        assert_eq!(g1, g2);
    }

    #[test]
    fn test_heuristic_generate_analysis_delegates() {
        let errors = vec![make_error(Some("E0308"), "test", "src/main.rs")];
        let a1 = HeuristicErrorDiagnoser::generate_analysis(&errors, ErrorCategory::TypeError);
        let a2 = generate_analysis_text(&errors, ErrorCategory::TypeError);
        assert_eq!(a1, a2);
    }

    #[test]
    fn test_llm_extract_field_delegates() {
        let text = "CATEGORY: TypeError";
        let r1 = LlmErrorDiagnoser::<MockLlmClient>::extract_field(text, "CATEGORY");
        let r2 = extract_field_value(text, "CATEGORY");
        assert_eq!(r1, r2);
    }

    #[test]
    fn test_llm_parse_diagnosis_delegates() {
        let client = MockLlmClient::unavailable();
        let diagnoser = LlmErrorDiagnoser::new(client);
        let response = "CATEGORY: TypeError\nANALYSIS: test\nFIX_GUIDANCE: fix";
        let r1 = diagnoser.parse_diagnosis(response);
        let r2 = parse_llm_diagnosis(response);
        assert_eq!(r1, r2);
    }

    #[test]
    fn test_hybrid_build_history_suggestion_delegates() {
        let now = Utc::now();
        let patterns = vec![ErrorPattern {
            error_code: Some("E0308".to_string()),
            message_signature: "sig".to_string(),
            category: ErrorCategory::TypeError,
            occurrences: 1,
            first_seen: now,
            last_seen: now,
            last_fix_succeeded: true,
            suggested_approach: None,
        }];
        let r1 = HybridErrorDiagnoser::<MockLlmClient>::build_history_suggestion(&patterns);
        let r2 = format_history_suggestion(&patterns);
        assert_eq!(r1, r2);
    }

    // ===== SequenceLlmClient 测试工具 =====

    /// 按顺序返回结果 (成功或失败) 的 Mock LlmClient, 用于测试重试逻辑
    struct SequenceLlmClient {
        responses: Arc<Mutex<Vec<Result<String>>>>,
        call_count: Arc<Mutex<usize>>,
    }

    impl SequenceLlmClient {
        fn new(responses: Vec<Result<String>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses)),
                call_count: Arc::new(Mutex::new(0)),
            }
        }
    }

    #[async_trait]
    impl LlmClient for SequenceLlmClient {
        async fn generate(&self, _prompt: &str) -> Result<String> {
            let mut count = self.call_count.lock().unwrap();
            *count += 1;
            let mut queue = self.responses.lock().unwrap();
            if queue.is_empty() {
                return Err(anyhow!("No more responses"));
            }
            queue.remove(0)
        }

        async fn is_available(&self) -> bool {
            true
        }
    }
}
