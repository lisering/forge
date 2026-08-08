//! Test Runner — 在本地运行 cargo build/test,解析结果
//!
//! 实现 TDD 闭环: 生成代码 → 编译/测试 → 反馈结果给 AI

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;
use tracing::{debug, info, warn};

/// 网络错误最大重试次数 (总尝试次数 = 1 + MAX_NETWORK_RETRIES)
const MAX_NETWORK_RETRIES: u32 = 3;

/// 网络错误重试间隔 (秒)
const NETWORK_RETRY_INTERVAL_SECS: u64 = 5;

/// 测试/编译结果
#[derive(Debug, Clone, Serialize)]
pub struct TestResult {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    /// 解析出的错误 (如果有)
    pub errors: Vec<CompileError>,
    /// 解析出的测试摘要
    pub test_summary: Option<TestSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompileError {
    pub file: String,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub message: String,
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TestSummary {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub ignored: usize,
}

impl TestResult {
    /// 检测是否为网络/代理错误 (而非真正的编译错误)
    ///
    /// 当 cargo 因网络问题 (代理不可用、DNS 解析失败等) 无法下载依赖时,
    /// 输出中会包含 "Couldn't connect to server" / "Failed to connect" 等关键词。
    /// 这种情况下编译失败不是代码问题, 而是 environment 问题。
    ///
    /// 检测模式:
    /// - "Couldn't connect to server"
    /// - "Failed to connect to"
    /// - "unable to update registry"
    /// - "download of config.json failed"
    /// - "spurious network error"
    ///
    /// # 返回
    /// - `true` — stderr 包含网络错误模式, 且没有解析出编译错误
    /// - `false` — 有编译错误或无网络错误模式
    pub fn is_network_error(&self) -> bool {
        // 如果有解析出的编译错误, 不是纯网络错误
        if !self.errors.is_empty() {
            return false;
        }

        // 检测网络错误关键词
        let network_patterns = [
            "Couldn't connect to server",
            "Failed to connect to",
            "unable to update registry",
            "download of config.json failed",
            "spurious network error",
            "Could not connect to server",
        ];

        network_patterns.iter().any(|p| self.stderr.contains(p))
    }

    /// 格式化为反馈给 AI 的文本
    pub fn to_feedback(&self) -> String {
        if self.success {
            return "✅ 编译/测试成功".to_string();
        }

        let mut feedback = String::new();

        // 网络错误检测: 如果是网络错误, 添加明确标记
        if self.is_network_error() {
            feedback.push_str("⚠️ 网络错误 (非代码问题): cargo 无法连接到 crates.io 注册表。\n");
            feedback.push_str("这可能是因为代理未运行或网络不可用。\n");
            feedback.push_str("已自动重试 3 次 (间隔 5s), 仍然失败。\n");
            feedback.push_str("请不要修改代码, 这是环境问题, 稍后会自动恢复。\n\n");
        }

        feedback.push_str("❌ 编译/测试失败\n\n");

        if !self.errors.is_empty() {
            feedback.push_str(&format!("发现 {} 个错误:\n", self.errors.len()));
            for (i, err) in self.errors.iter().enumerate().take(10) {
                feedback.push_str(&format!(
                    "\n{}. {}:{}:{} - {}\n",
                    i + 1,
                    err.file,
                    err.line.unwrap_or(0),
                    err.column.unwrap_or(0),
                    err.message
                ));
                if let Some(code) = &err.error_code {
                    feedback.push_str(&format!("   错误码: {}\n", code));
                }
            }
            if self.errors.len() > 10 {
                feedback.push_str(&format!("\n... 还有 {} 个错误\n", self.errors.len() - 10));
            }
        }

        if let Some(summary) = &self.test_summary {
            feedback.push_str(&format!(
                "\n测试结果: {} 通过, {} 失败, {} 忽略 (共 {})\n",
                summary.passed, summary.failed, summary.ignored, summary.total
            ));
        }

        // 附带 stderr 的最后 2000 字符
        if !self.stderr.is_empty() {
            let stderr = if self.stderr.len() > 2000 {
                format!("...\n{}", &self.stderr[self.stderr.len() - 2000..])
            } else {
                self.stderr.clone()
            };
            feedback.push_str(&format!("\n编译器输出:\n```\n{}\n```\n", stderr));
        }

        feedback
    }
}

// ============================================================================
//  E2E 测试支持 — 运行编译后的程序并验证输出
// ============================================================================

/// E2E 测试用例 — 定义输入和预期输出
///
/// 格式: 输入 (stdin + args) → 预期输出 (stdout + exit_code)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct E2ETestCase {
    /// 测试名称
    pub name: String,
    /// 标准输入 (None 表示不提供 stdin)
    #[serde(default)]
    pub stdin: Option<String>,
    /// 命令行参数
    #[serde(default)]
    pub args: Vec<String>,
    /// 预期 stdout (None 表示不检查)
    #[serde(default)]
    pub expected_stdout: Option<String>,
    /// 预期退出码 (None 表示不检查)
    #[serde(default)]
    pub expected_exit_code: Option<i32>,
}

/// E2E 测试结果
#[derive(Debug, Clone)]
pub struct E2ETestResult {
    /// 对应的测试用例
    pub test_case: E2ETestCase,
    /// 实际 stdout
    pub stdout: String,
    /// 实际 stderr
    pub stderr: String,
    /// 实际退出码
    pub exit_code: i32,
    /// 是否通过
    pub passed: bool,
}

impl E2ETestResult {
    /// 格式化为反馈给 AI 的文本
    pub fn to_feedback(&self) -> String {
        if self.passed {
            return format!("✅ E2E 测试通过: {}", self.test_case.name);
        }

        let mut feedback = format!("❌ E2E 测试失败: {}\n", self.test_case.name);

        if let Some(expected) = &self.test_case.expected_stdout {
            feedback.push_str(&format!("  预期输出: \"{}\"\n", expected));
            feedback.push_str(&format!("  实际输出: \"{}\"\n", self.stdout));
        }

        if let Some(expected) = self.test_case.expected_exit_code {
            feedback.push_str(&format!(
                "  预期退出码: {}, 实际退出码: {}\n",
                expected, self.exit_code
            ));
        }

        if !self.stderr.is_empty() {
            let stderr = if self.stderr.len() > 500 {
                format!("...\n{}", &self.stderr[self.stderr.len() - 500..])
            } else {
                self.stderr.clone()
            };
            feedback.push_str(&format!("  stderr: {}\n", stderr));
        }

        feedback
    }
}

/// E2E 测试结果摘要
#[derive(Debug, Clone)]
pub struct E2ETestSummary {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub results: Vec<E2ETestResult>,
}

impl E2ETestSummary {
    pub fn success(&self) -> bool {
        self.failed == 0
    }

    /// 格式化为反馈给 AI 的文本
    pub fn to_feedback(&self) -> String {
        if self.success() {
            return format!("✅ 所有 E2E 测试通过 ({}/{})", self.passed, self.total);
        }

        let mut feedback = format!(
            "❌ E2E 测试: {}/{} 通过, {} 失败\n\n",
            self.passed, self.total, self.failed
        );

        for result in &self.results {
            if !result.passed {
                feedback.push_str(&result.to_feedback());
                feedback.push('\n');
            }
        }

        feedback
    }
}

/// Cargo 测试运行器 — 真实实现
///
/// 实现 `TestRunner` trait (DIP),通过 `cargo check` / `cargo test` 运行编译和测试。
pub struct CargoTestRunner;

impl crate::traits::TestRunner for CargoTestRunner {
    fn check(&self, dir: &Path) -> Result<TestResult> {
        cargo_check(dir)
    }
    fn test(&self, dir: &Path) -> Result<TestResult> {
        cargo_test(dir)
    }

    fn run_binary(&self, dir: &Path, test_cases: &[E2ETestCase]) -> Result<Vec<E2ETestResult>> {
        run_e2e_tests(dir, test_cases)
    }
}

/// 运行 cargo build
pub fn cargo_build(project_dir: &Path) -> Result<TestResult> {
    run_cargo(project_dir, &["build"])
}

/// 运行 cargo test
pub fn cargo_test(project_dir: &Path) -> Result<TestResult> {
    run_cargo(project_dir, &["test"])
}

/// 运行 cargo check (快速检查,不生成二进制)
pub fn cargo_check(project_dir: &Path) -> Result<TestResult> {
    run_cargo(project_dir, &["check"])
}

fn run_cargo(project_dir: &Path, args: &[&str]) -> Result<TestResult> {
    let cargo_cmd = args.join(" ");
    info!("运行 cargo {} (在 {})", cargo_cmd, project_dir.display());

    let mut last_result: Option<TestResult> = None;

    for attempt in 0..=MAX_NETWORK_RETRIES {
        let output = Command::new("cargo")
            .args(args)
            .current_dir(project_dir)
            .output()
            .with_context(|| format!("无法执行 cargo {}", cargo_cmd))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(-1);
        let success = output.status.success();

        // 解析编译错误
        let errors = parse_compile_errors(&stderr);
        let test_summary = parse_test_summary(&stdout);

        let result = TestResult {
            success,
            stdout,
            stderr,
            exit_code,
            errors,
            test_summary,
        };

        if success {
            info!("cargo {} 成功", cargo_cmd);
            return Ok(result);
        }

        // 检测网络错误: 如果是网络错误且有重试次数剩余, 等待后重试
        if result.is_network_error() && attempt < MAX_NETWORK_RETRIES {
            warn!(
                "cargo {} 失败 (网络错误), {}s 后重试 ({}/{})",
                cargo_cmd,
                NETWORK_RETRY_INTERVAL_SECS,
                attempt + 1,
                MAX_NETWORK_RETRIES
            );
            last_result = Some(result);
            thread::sleep(Duration::from_secs(NETWORK_RETRY_INTERVAL_SECS));
            continue;
        }

        // 非网络错误或重试次数耗尽
        if !result.is_network_error() {
            warn!(
                "cargo {} 失败 (exit {}), {} 个错误",
                cargo_cmd,
                exit_code,
                result.errors.len()
            );
        } else if attempt == MAX_NETWORK_RETRIES {
            // 网络错误且重试耗尽
            warn!(
                "cargo {} 网络错误重试耗尽 ({}/{}), 返回失败结果",
                cargo_cmd, MAX_NETWORK_RETRIES, MAX_NETWORK_RETRIES
            );
            // 在 stderr 中追加重试摘要, 便于 AI 理解
            let mut result = result;
            result.stderr.push_str(&format!(
                "\n--- Forge 网络错误重试摘要 ---\n已自动重试 {} 次 (间隔 {}s), 仍然失败。\n这是环境问题, 非代码问题。\n",
                MAX_NETWORK_RETRIES,
                NETWORK_RETRY_INTERVAL_SECS
            ));
            return Ok(result);
        }

        return Ok(result);
    }

    // 理论上不会执行到这里 (循环已覆盖所有路径), 但为编译器安全返回
    Ok(last_result.unwrap_or_else(|| TestResult {
        success: false,
        stdout: String::new(),
        stderr: "unexpected: run_cargo loop exhausted".to_string(),
        exit_code: -1,
        errors: vec![],
        test_summary: None,
    }))
}

/// 从 cargo 输出中解析编译错误
fn parse_compile_errors(stderr: &str) -> Vec<CompileError> {
    let mut errors = Vec::new();

    // 匹配: error[E0308]: mismatched types
    //   --> src/main.rs:10:5
    let re = regex::Regex::new(r"error(?:\[([^\]]+)\])?:\s*(.+?)\n\s*-->\s*([^:]+):(\d+):(\d+)")
        .unwrap();

    for cap in re.captures_iter(stderr) {
        errors.push(CompileError {
            error_code: cap.get(1).map(|m| m.as_str().to_string()),
            message: cap
                .get(2)
                .map(|m| m.as_str().trim().to_string())
                .unwrap_or_default(),
            file: cap
                .get(3)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default(),
            line: cap.get(4).and_then(|m| m.as_str().parse().ok()),
            column: cap.get(5).and_then(|m| m.as_str().parse().ok()),
        });
    }

    // 也匹配 warning,但不算错误
    // 匹配: error: could not compile ... due to N previous errors
    let re_fatal =
        regex::Regex::new(r"error:\s*could not compile.*due to\s+(\d+)\s+previous error").unwrap();
    if let Some(cap) = re_fatal.captures(stderr) {
        // 已经被上面的规则捕获了
        debug!("致命错误: {} 个", cap.get(1).unwrap().as_str());
    }

    errors
}

/// 从 cargo test 输出中解析测试摘要
fn parse_test_summary(stdout: &str) -> Option<TestSummary> {
    // 匹配: test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
    // 或: running 3 tests
    let re =
        regex::Regex::new(r"test result:.*?(\d+)\s+passed;.*?(\d+)\s+failed;.*?(\d+)\s+ignored")
            .ok()?;

    let caps = re.captures(stdout)?;
    let passed = caps
        .get(1)
        .and_then(|m| m.as_str().parse().ok())
        .unwrap_or(0);
    let failed = caps
        .get(2)
        .and_then(|m| m.as_str().parse().ok())
        .unwrap_or(0);
    let ignored = caps
        .get(3)
        .and_then(|m| m.as_str().parse().ok())
        .unwrap_or(0);

    Some(TestSummary {
        total: passed + failed + ignored,
        passed,
        failed,
        ignored,
    })
}

// ============================================================================
//  E2E 测试: 构建并运行二进制
// ============================================================================

/// 从 Cargo.toml 中解析包名
fn parse_package_name(project_dir: &Path) -> Option<String> {
    let cargo_toml = project_dir.join("Cargo.toml");
    let content = std::fs::read_to_string(&cargo_toml).ok()?;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("name") {
            // name = "package_name"
            let value = trimmed.split('=').nth(1)?.trim();
            return Some(value.trim_matches('"').to_string());
        }
    }
    None
}

/// 查找编译后的二进制文件路径
///
/// 尝试在 `target/debug/` 目录中查找与包名匹配的二进制文件。
fn find_binary(project_dir: &Path, package_name: &str) -> Option<std::path::PathBuf> {
    let debug_dir = project_dir.join("target").join("debug");

    // 直接用包名查找
    let binary_path = debug_dir.join(package_name);
    if binary_path.exists() && binary_path.is_file() {
        return Some(binary_path);
    }

    // Windows: 加 .exe 后缀
    #[cfg(target_os = "windows")]
    {
        let exe_path = debug_dir.join(format!("{}.exe", package_name));
        if exe_path.exists() && exe_path.is_file() {
            return Some(exe_path);
        }
    }

    // 遍历 target/debug 查找 (可能包名与二进制名不同)
    if let Ok(entries) = std::fs::read_dir(&debug_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                let file_name = path.file_name()?.to_string_lossy().to_string();
                if file_name == package_name || file_name == format!("{}.exe", package_name) {
                    return Some(path);
                }
            }
        }
    }

    None
}

/// 运行 E2E 测试 — 构建项目并对每个测试用例运行二进制
///
/// 流程:
/// 1. `cargo build` 编译项目
/// 2. 查找编译后的二进制文件
/// 3. 对每个 E2ETestCase: 运行二进制 → 捕获 stdout/stderr/exit_code → 比较预期
pub fn run_e2e_tests(project_dir: &Path, test_cases: &[E2ETestCase]) -> Result<Vec<E2ETestResult>> {
    if test_cases.is_empty() {
        return Ok(vec![]);
    }

    info!("运行 E2E 测试 ({} 个用例)", test_cases.len());

    // 步骤 1: cargo build
    let build_result = cargo_build(project_dir)?;
    if !build_result.success {
        warn!("E2E 测试: cargo build 失败, 跳过");
        // build 失败时, 所有测试用例标记为失败
        return Ok(test_cases
            .iter()
            .map(|tc| E2ETestResult {
                test_case: tc.clone(),
                stdout: String::new(),
                stderr: format!("cargo build 失败:\n{}", build_result.stderr),
                exit_code: -1,
                passed: false,
            })
            .collect());
    }

    // 步骤 2: 查找二进制文件
    let package_name = parse_package_name(project_dir)
        .ok_or_else(|| anyhow::anyhow!("无法从 Cargo.toml 解析包名"))?;

    let binary_path = find_binary(project_dir, &package_name)
        .ok_or_else(|| anyhow::anyhow!("找不到编译后的二进制文件 (包名: {})", package_name))?;

    debug!("E2E: 二进制路径: {}", binary_path.display());

    // 步骤 3: 对每个测试用例运行二进制
    let mut results = Vec::new();
    for tc in test_cases {
        debug!("E2E: 运行测试用例 '{}'", tc.name);

        let mut cmd = Command::new(&binary_path);
        cmd.args(&tc.args);

        // 设置 stdin
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow::anyhow!("启动二进制失败: {}", e))?;

        // 写入 stdin (如果有)
        if let Some(stdin_input) = &tc.stdin {
            use std::io::Write;
            if let Some(stdin) = child.stdin.as_mut() {
                let _ = stdin.write_all(stdin_input.as_bytes());
            }
        }

        // 关闭 stdin (发送 EOF)
        drop(child.stdin.take());

        let output = child.wait_with_output()?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(-1);

        // 比较预期
        let stdout_ok = tc
            .expected_stdout
            .as_ref()
            .is_none_or(|expected| stdout.trim() == expected.trim());
        let exit_code_ok = tc
            .expected_exit_code
            .is_none_or(|expected| exit_code == expected);
        let passed = stdout_ok && exit_code_ok;

        info!(
            "E2E 测试 '{}': {} (exit={}, stdout={}字符)",
            tc.name,
            if passed { "✅ 通过" } else { "❌ 失败" },
            exit_code,
            stdout.len()
        );

        results.push(E2ETestResult {
            test_case: tc.clone(),
            stdout,
            stderr,
            exit_code,
            passed,
        });
    }

    Ok(results)
}

/// 从 JSON 文件加载 E2E 测试用例
///
/// JSON 格式: 数组 of E2ETestCase
pub fn load_e2e_tests_from_file(path: &Path) -> Result<Vec<E2ETestCase>> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("读取 E2E 测试文件失败: {}", e))?;
    let cases: Vec<E2ETestCase> = serde_json::from_str(&content)
        .map_err(|e| anyhow::anyhow!("解析 E2E 测试 JSON 失败: {}", e))?;
    Ok(cases)
}

/// 从工作区加载 E2E 测试用例
///
/// 查找 `<workspace>/.forge/e2e_tests.json` 文件
pub fn load_e2e_tests_from_workspace(project_dir: &Path) -> Vec<E2ETestCase> {
    let e2e_path = project_dir.join(".forge").join("e2e_tests.json");
    if !e2e_path.exists() {
        return vec![];
    }
    match load_e2e_tests_from_file(&e2e_path) {
        Ok(cases) => {
            debug!(
                "从 {} 加载了 {} 个 E2E 测试用例",
                e2e_path.display(),
                cases.len()
            );
            cases
        }
        Err(e) => {
            warn!("加载 E2E 测试用例失败: {}", e);
            vec![]
        }
    }
}

// ============================================================================
//  单元测试: E2E 测试支持
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // ===== E2ETestCase / E2ETestResult / E2ETestSummary =====

    #[test]
    fn test_e2e_test_case_serde() {
        let case = E2ETestCase {
            name: "test1".to_string(),
            stdin: Some("hello".to_string()),
            args: vec!["--flag".to_string()],
            expected_stdout: Some("world".to_string()),
            expected_exit_code: Some(0),
        };
        let json = serde_json::to_string(&case).unwrap();
        let decoded: E2ETestCase = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.name, "test1");
        assert_eq!(decoded.stdin, Some("hello".to_string()));
        assert_eq!(decoded.args, vec!["--flag"]);
        assert_eq!(decoded.expected_stdout, Some("world".to_string()));
        assert_eq!(decoded.expected_exit_code, Some(0));
    }

    #[test]
    fn test_e2e_test_case_with_defaults() {
        let json = r#"{"name":"minimal"}"#;
        let case: E2ETestCase = serde_json::from_str(json).unwrap();
        assert_eq!(case.name, "minimal");
        assert!(case.stdin.is_none());
        assert!(case.args.is_empty());
        assert!(case.expected_stdout.is_none());
        assert!(case.expected_exit_code.is_none());
    }

    #[test]
    fn test_e2e_result_passed_feedback() {
        let case = E2ETestCase {
            name: "ok_test".to_string(),
            stdin: None,
            args: vec![],
            expected_stdout: Some("hello".to_string()),
            expected_exit_code: Some(0),
        };
        let result = E2ETestResult {
            test_case: case,
            stdout: "hello".to_string(),
            stderr: String::new(),
            exit_code: 0,
            passed: true,
        };
        let feedback = result.to_feedback();
        assert!(feedback.contains("✅"));
        assert!(feedback.contains("ok_test"));
    }

    #[test]
    fn test_e2e_result_failed_feedback() {
        let case = E2ETestCase {
            name: "fail_test".to_string(),
            stdin: None,
            args: vec![],
            expected_stdout: Some("expected".to_string()),
            expected_exit_code: Some(0),
        };
        let result = E2ETestResult {
            test_case: case,
            stdout: "actual".to_string(),
            stderr: "error occurred".to_string(),
            exit_code: 1,
            passed: false,
        };
        let feedback = result.to_feedback();
        assert!(feedback.contains("❌"));
        assert!(feedback.contains("fail_test"));
        assert!(feedback.contains("expected"));
        assert!(feedback.contains("actual"));
        assert!(feedback.contains("error occurred"));
    }

    #[test]
    fn test_e2e_summary_success() {
        let summary = E2ETestSummary {
            total: 3,
            passed: 3,
            failed: 0,
            results: vec![],
        };
        assert!(summary.success());
        assert!(summary.to_feedback().contains("✅"));
    }

    #[test]
    fn test_e2e_summary_failure() {
        let case = E2ETestCase {
            name: "fail".to_string(),
            stdin: None,
            args: vec![],
            expected_stdout: Some("x".to_string()),
            expected_exit_code: None,
        };
        let result = E2ETestResult {
            test_case: case,
            stdout: "y".to_string(),
            stderr: String::new(),
            exit_code: 0,
            passed: false,
        };
        let summary = E2ETestSummary {
            total: 2,
            passed: 1,
            failed: 1,
            results: vec![result],
        };
        assert!(!summary.success());
        let feedback = summary.to_feedback();
        assert!(feedback.contains("❌"));
        assert!(feedback.contains("1/2"));
        assert!(feedback.contains("fail"));
    }

    // ===== load_e2e_tests_from_file / from_workspace =====

    #[test]
    fn test_load_e2e_tests_from_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("e2e.json");
        let json = r#"[
            {"name":"test1","stdin":"hello","expected_stdout":"world","expected_exit_code":0},
            {"name":"test2","args":["--version"],"expected_stdout":"1.0.0"}
        ]"#;
        std::fs::write(&path, json).unwrap();
        let cases = load_e2e_tests_from_file(&path).unwrap();
        assert_eq!(cases.len(), 2);
        assert_eq!(cases[0].name, "test1");
        assert_eq!(cases[0].stdin, Some("hello".to_string()));
        assert_eq!(cases[1].name, "test2");
        assert_eq!(cases[1].args, vec!["--version"]);
    }

    #[test]
    fn test_load_e2e_tests_nonexistent_file() {
        let result = load_e2e_tests_from_file(std::path::Path::new("/nonexistent/e2e.json"));
        assert!(result.is_err());
    }

    #[test]
    fn test_load_e2e_tests_invalid_json() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, "not json").unwrap();
        assert!(load_e2e_tests_from_file(&path).is_err());
    }

    #[test]
    fn test_load_e2e_tests_from_workspace_nonexistent() {
        let dir = tempdir().unwrap();
        let cases = load_e2e_tests_from_workspace(dir.path());
        assert!(cases.is_empty());
    }

    #[test]
    fn test_load_e2e_tests_from_workspace_existing() {
        let dir = tempdir().unwrap();
        let forge_dir = dir.path().join(".forge");
        std::fs::create_dir_all(&forge_dir).unwrap();
        let e2e_path = forge_dir.join("e2e_tests.json");
        let json = r#"[{"name":"test1","expected_stdout":"hello"}]"#;
        std::fs::write(&e2e_path, json).unwrap();

        let cases = load_e2e_tests_from_workspace(dir.path());
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].name, "test1");
    }

    // ===== parse_package_name / find_binary =====

    #[test]
    fn test_parse_package_name() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            r#"[package]
name = "my_app"
version = "0.1.0""#,
        )
        .unwrap();
        let name = parse_package_name(dir.path());
        assert_eq!(name, Some("my_app".to_string()));
    }

    #[test]
    fn test_parse_package_name_missing() {
        let dir = tempdir().unwrap();
        assert!(parse_package_name(dir.path()).is_none());
    }

    #[test]
    fn test_find_binary_nonexistent() {
        let dir = tempdir().unwrap();
        assert!(find_binary(dir.path(), "nonexistent").is_none());
    }

    // ===== run_e2e_tests with empty cases =====

    #[test]
    fn test_run_e2e_tests_empty_cases() {
        let dir = tempdir().unwrap();
        let results = run_e2e_tests(dir.path(), &[]).unwrap();
        assert!(results.is_empty());
    }

    // ===== is_network_error 测试 =====

    #[test]
    fn test_is_network_error_proxy_connection() {
        // 模拟压测中出现的代理连接错误
        let result = TestResult {
            success: false,
            stdout: String::new(),
            stderr: r#"warning: spurious network error (2 tries remaining): [7] Couldn't connect to server (Failed to connect to 127.0.0.1 port 7890 after 0 ms: Couldn't connect to server)
error: failed to get `anyhow` as a dependency of package `calculator v0.1.0`
Caused by:
  unable to update registry `crates-io`
"#.to_string(),
            exit_code: 101,
            errors: vec![],
            test_summary: None,
        };
        assert!(result.is_network_error(), "代理连接错误应被检测为网络错误");
    }

    #[test]
    fn test_is_network_error_download_failed() {
        let result = TestResult {
            success: false,
            stdout: String::new(),
            stderr: r#"error: failed to get `anyhow` as a dependency
Caused by:
  download of config.json failed
"#
            .to_string(),
            exit_code: 101,
            errors: vec![],
            test_summary: None,
        };
        assert!(result.is_network_error(), "下载失败应被检测为网络错误");
    }

    #[test]
    fn test_is_network_error_not_when_compile_errors_exist() {
        // 如果有编译错误, 不应判定为网络错误
        let result = TestResult {
            success: false,
            stdout: String::new(),
            stderr: r#"error[E0308]: mismatched types
  --> src/main.rs:10:5
"#
            .to_string(),
            exit_code: 101,
            errors: vec![CompileError {
                file: "src/main.rs".to_string(),
                line: Some(10),
                column: Some(5),
                message: "mismatched types".to_string(),
                error_code: Some("E0308".to_string()),
            }],
            test_summary: None,
        };
        assert!(!result.is_network_error(), "有编译错误时不应判定为网络错误");
    }

    #[test]
    fn test_is_network_error_false_on_success() {
        let result = TestResult {
            success: true,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
            errors: vec![],
            test_summary: None,
        };
        assert!(!result.is_network_error(), "成功时不应判定为网络错误");
    }

    #[test]
    fn test_is_network_error_false_on_no_match() {
        let result = TestResult {
            success: false,
            stdout: String::new(),
            stderr: "some other error".to_string(),
            exit_code: 1,
            errors: vec![],
            test_summary: None,
        };
        assert!(
            !result.is_network_error(),
            "不匹配网络错误模式时应返回 false"
        );
    }

    #[test]
    fn test_to_feedback_includes_network_error_warning() {
        // 网络错误时, feedback 应包含网络错误警告
        let result = TestResult {
            success: false,
            stdout: String::new(),
            stderr: r#"Couldn't connect to server (Failed to connect to 127.0.0.1 port 7890)"#
                .to_string(),
            exit_code: 101,
            errors: vec![],
            test_summary: None,
        };
        let feedback = result.to_feedback();
        assert!(feedback.contains("网络错误"), "feedback 应包含网络错误标记");
        assert!(feedback.contains("非代码问题"), "feedback 应说明非代码问题");
    }

    #[test]
    fn test_to_feedback_no_network_warning_on_compile_error() {
        // 编译错误时, feedback 不应包含网络错误警告
        let result = TestResult {
            success: false,
            stdout: String::new(),
            stderr: r#"error[E0308]: mismatched types
  --> src/main.rs:10:5"#
                .to_string(),
            exit_code: 101,
            errors: vec![CompileError {
                file: "src/main.rs".to_string(),
                line: Some(10),
                column: Some(5),
                message: "mismatched types".to_string(),
                error_code: Some("E0308".to_string()),
            }],
            test_summary: None,
        };
        let feedback = result.to_feedback();
        assert!(
            !feedback.contains("网络错误"),
            "编译错误时不应有网络错误标记"
        );
    }

    // ===== 网络错误重试逻辑测试 =====

    #[test]
    fn test_to_feedback_network_error_includes_retry_info() {
        // 网络错误时, feedback 应包含重试信息
        let result = TestResult {
            success: false,
            stdout: String::new(),
            stderr: r#"Couldn't connect to server (Failed to connect to 127.0.0.1 port 7890)"#
                .to_string(),
            exit_code: 101,
            errors: vec![],
            test_summary: None,
        };
        let feedback = result.to_feedback();
        assert!(feedback.contains("网络错误"), "feedback 应包含网络错误标记");
        assert!(feedback.contains("非代码问题"), "feedback 应说明非代码问题");
        assert!(feedback.contains("重试"), "feedback 应包含重试信息");
        assert!(
            feedback.contains("请不要修改代码"),
            "feedback 应告知 AI 不要修改代码"
        );
    }

    #[test]
    fn test_to_feedback_network_error_includes_env_issue() {
        // 网络错误 feedback 应明确告知 AI 这是环境问题
        let result = TestResult {
            success: false,
            stdout: String::new(),
            stderr: "spurious network error".to_string(),
            exit_code: 101,
            errors: vec![],
            test_summary: None,
        };
        let feedback = result.to_feedback();
        assert!(feedback.contains("环境问题"), "feedback 应说明是环境问题");
        assert!(feedback.contains("代理"), "feedback 应提到代理");
    }

    #[test]
    fn test_network_error_with_retry_summary_in_stderr() {
        // 模拟 run_cargo 重试耗尽后, stderr 中追加重试摘要
        let stderr_with_retry = format!(
            r#"warning: spurious network error (2 tries remaining): [7] Couldn't connect to server
error: failed to get `anyhow` as a dependency

--- Forge 网络错误重试摘要 ---
已自动重试 {} 次 (间隔 {}s), 仍然失败。
这是环境问题, 非代码问题。
"#,
            MAX_NETWORK_RETRIES, NETWORK_RETRY_INTERVAL_SECS
        );

        let result = TestResult {
            success: false,
            stdout: String::new(),
            stderr: stderr_with_retry,
            exit_code: 101,
            errors: vec![],
            test_summary: None,
        };

        assert!(
            result.is_network_error(),
            "含重试摘要的 stderr 仍应被检测为网络错误"
        );
        let feedback = result.to_feedback();
        assert!(feedback.contains("网络错误"), "feedback 应包含网络错误标记");
        assert!(feedback.contains("重试"), "feedback 应包含重试信息");
    }

    #[test]
    fn test_network_retry_constants() {
        // 验证重试常量值
        assert_eq!(MAX_NETWORK_RETRIES, 3, "最大重试次数应为 3");
        assert_eq!(NETWORK_RETRY_INTERVAL_SECS, 5, "重试间隔应为 5 秒");
    }

    #[test]
    fn test_to_feedback_network_error_not_on_empty_stderr() {
        // 空 stderr 不应判定为网络错误
        let result = TestResult {
            success: false,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 1,
            errors: vec![],
            test_summary: None,
        };
        assert!(!result.is_network_error(), "空 stderr 不应判定为网络错误");
        let feedback = result.to_feedback();
        assert!(
            !feedback.contains("网络错误"),
            "空 stderr 时不应有网络错误标记"
        );
    }
}
