//! 多语言项目支持 — 方向 B
//!
//! 通过 `LanguageAdapter` trait 抽象不同语言的构建/测试/运行能力,
//! `MultiLanguageTestRunner` 自动检测项目语言并委托给对应的适配器。
//!
//! 支持的语言:
//! - Rust: cargo check / cargo test / cargo build
//! - Python: python -m py_compile / python -m pytest / python main.py
//! - Go: go build / go test / go run
//! - Node: npx tsc --noEmit / npm test / node index.js

use crate::testrunner::{E2ETestCase, E2ETestResult, TestResult};
use crate::traits::{Language, LanguageAdapter, TestRunner};
use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;
use tracing::{debug, info, warn};

// ============================================================================
//  语言检测
// ============================================================================

/// 检测项目目录的语言
///
/// 通过检查项目文件自动识别:
/// - `Cargo.toml` → Rust
/// - `pyproject.toml` / `setup.py` / `requirements.txt` → Python
/// - `go.mod` → Go
/// - `package.json` → Node
/// - 都没有 → Unknown
pub fn detect_language(dir: &Path) -> Language {
    if dir.join("Cargo.toml").exists() {
        Language::Rust
    } else if dir.join("pyproject.toml").exists()
        || dir.join("setup.py").exists()
        || dir.join("requirements.txt").exists()
    {
        Language::Python
    } else if dir.join("go.mod").exists() {
        Language::Go
    } else if dir.join("package.json").exists() {
        Language::Node
    } else {
        Language::Unknown
    }
}

/// 根据项目目录获取对应的语言适配器
pub fn get_adapter(dir: &Path) -> Box<dyn LanguageAdapter> {
    let lang = detect_language(dir);
    match lang {
        Language::Rust => Box::new(RustAdapter),
        Language::Python => Box::new(PythonAdapter),
        Language::Go => Box::new(GoAdapter),
        Language::Node => Box::new(NodeAdapter),
        Language::Unknown => {
            warn!("无法检测项目语言, 默认使用 Rust 适配器");
            Box::new(RustAdapter)
        }
    }
}

// ============================================================================
//  RustAdapter — cargo 工具链
// ============================================================================

/// Rust 语言适配器 — 使用 cargo 进行构建/测试
///
/// 复用现有的 `cargo_check` / `cargo_test` / `run_e2e_tests` 函数。
pub struct RustAdapter;

impl LanguageAdapter for RustAdapter {
    fn language(&self) -> Language {
        Language::Rust
    }

    fn check(&self, dir: &Path) -> Result<TestResult> {
        crate::testrunner::cargo_check(dir)
    }

    fn test(&self, dir: &Path) -> Result<TestResult> {
        crate::testrunner::cargo_test(dir)
    }

    fn run_binary(&self, dir: &Path, test_cases: &[E2ETestCase]) -> Result<Vec<E2ETestResult>> {
        crate::testrunner::run_e2e_tests(dir, test_cases)
    }
}

// ============================================================================
//  PythonAdapter — python 工具链
// ============================================================================

/// Python 语言适配器 — 使用 python/pytest 进行构建/测试
///
/// - `check`: `python -m py_compile` 编译所有 .py 文件
/// - `test`: `python -m pytest` 运行测试
/// - `run_binary`: `python main.py <args>` 运行主程序
pub struct PythonAdapter;

/// Python 解释器命令名
const PYTHON_CMD: &str = "python3";

impl PythonAdapter {
    /// 查找项目中的所有 .py 文件
    pub fn find_python_files(dir: &Path) -> Vec<std::path::PathBuf> {
        let mut files = Vec::new();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    if let Some(ext) = path.extension() {
                        if ext == "py" {
                            files.push(path);
                        }
                    }
                } else if path.is_dir() {
                    let dir_name = path.file_name().map(|n| n.to_string_lossy().to_string());
                    // 跳过隐藏目录和常见忽略目录
                    if let Some(name) = &dir_name {
                        if name.starts_with('.')
                            || name == "__pycache__"
                            || name == "venv"
                            || name == "env"
                        {
                            continue;
                        }
                    }
                    // 递归查找
                    let sub_files = Self::find_python_files(&path);
                    files.extend(sub_files);
                }
            }
        }
        files
    }

    /// 查找主入口文件 (main.py, app.py, __main__.py)
    pub fn find_entry_point(dir: &Path) -> Option<std::path::PathBuf> {
        for name in &["main.py", "app.py", "__main__.py", "run.py"] {
            let path = dir.join(name);
            if path.exists() {
                return Some(path);
            }
        }
        // 查找任何 .py 文件
        let py_files = Self::find_python_files(dir);
        py_files.into_iter().next()
    }
}

impl LanguageAdapter for PythonAdapter {
    fn language(&self) -> Language {
        Language::Python
    }

    fn check(&self, dir: &Path) -> Result<TestResult> {
        let py_files = Self::find_python_files(dir);
        if py_files.is_empty() {
            return Ok(TestResult {
                success: true,
                stdout: String::new(),
                stderr: "无 .py 文件, 跳过检查".to_string(),
                exit_code: 0,
                errors: vec![],
                test_summary: None,
            });
        }

        info!("Python check: 编译 {} 个 .py 文件", py_files.len());

        let mut cmd = Command::new(PYTHON_CMD);
        cmd.arg("-m").arg("py_compile");
        for f in &py_files {
            cmd.arg(f);
        }
        cmd.current_dir(dir);

        let output = cmd
            .output()
            .context("无法执行 python3 -m py_compile (请确认 python3 已安装)")?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(-1);
        let success = output.status.success();

        if success {
            info!("Python check 成功");
        } else {
            warn!("Python check 失败 (exit {})", exit_code);
        }

        Ok(TestResult {
            success,
            stdout,
            stderr,
            exit_code,
            errors: vec![],
            test_summary: None,
        })
    }

    fn test(&self, dir: &Path) -> Result<TestResult> {
        // 优先使用 pytest, 回退到 unittest
        let mut cmd = Command::new(PYTHON_CMD);
        cmd.arg("-m").arg("pytest").arg("-v").current_dir(dir);

        let output = match cmd.output() {
            Ok(o) => o,
            Err(_) => {
                // pytest 不可用, 回退到 unittest
                warn!("pytest 不可用, 回退到 unittest");
                let mut cmd2 = Command::new(PYTHON_CMD);
                cmd2.arg("-m")
                    .arg("unittest")
                    .arg("discover")
                    .current_dir(dir);
                let output2 = cmd2.output().context("无法执行 python3 -m unittest")?;
                let stdout = String::from_utf8_lossy(&output2.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output2.stderr).to_string();
                let exit_code = output2.status.code().unwrap_or(-1);
                let success = output2.status.success();
                return Ok(TestResult {
                    success,
                    stdout,
                    stderr,
                    exit_code,
                    errors: vec![],
                    test_summary: None,
                });
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(-1);
        let success = output.status.success();

        if success {
            info!("Python test 成功");
        } else {
            warn!("Python test 失败 (exit {})", exit_code);
        }

        Ok(TestResult {
            success,
            stdout,
            stderr,
            exit_code,
            errors: vec![],
            test_summary: None,
        })
    }

    fn run_binary(&self, dir: &Path, test_cases: &[E2ETestCase]) -> Result<Vec<E2ETestResult>> {
        if test_cases.is_empty() {
            return Ok(vec![]);
        }

        let entry = Self::find_entry_point(dir)
            .ok_or_else(|| anyhow::anyhow!("找不到 Python 入口文件 (main.py/app.py)"))?;

        debug!("Python E2E: 入口文件: {}", entry.display());

        let mut results = Vec::new();
        for tc in test_cases {
            debug!("Python E2E: 运行测试用例 '{}'", tc.name);

            let mut cmd = Command::new(PYTHON_CMD);
            cmd.arg(&entry).args(&tc.args);
            cmd.current_dir(dir);
            cmd.stdin(std::process::Stdio::piped());
            cmd.stdout(std::process::Stdio::piped());
            cmd.stderr(std::process::Stdio::piped());

            let mut child = cmd
                .spawn()
                .map_err(|e| anyhow::anyhow!("启动 python 失败: {}", e))?;

            if let Some(stdin_input) = &tc.stdin {
                use std::io::Write;
                if let Some(stdin) = child.stdin.as_mut() {
                    let _ = stdin.write_all(stdin_input.as_bytes());
                }
            }
            drop(child.stdin.take());

            let output = child.wait_with_output()?;

            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let exit_code = output.status.code().unwrap_or(-1);

            let stdout_ok = tc
                .expected_stdout
                .as_ref()
                .is_none_or(|expected| stdout.trim() == expected.trim());
            let exit_code_ok = tc
                .expected_exit_code
                .is_none_or(|expected| exit_code == expected);
            let passed = stdout_ok && exit_code_ok;

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
}

// ============================================================================
//  GoAdapter — go 工具链
// ============================================================================

/// Go 语言适配器 — 使用 go 进行构建/测试
///
/// - `check`: `go build ./...` 编译检查
/// - `test`: `go test ./...` 运行测试
/// - `run_binary`: `go build -o /tmp/xxx` 然后 运行二进制
pub struct GoAdapter;

impl LanguageAdapter for GoAdapter {
    fn language(&self) -> Language {
        Language::Go
    }

    fn check(&self, dir: &Path) -> Result<TestResult> {
        info!("Go check: go build ./...");

        let output = Command::new("go")
            .arg("build")
            .arg("./...")
            .current_dir(dir)
            .output()
            .context("无法执行 go build (请确认 go 已安装)")?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(-1);
        let success = output.status.success();

        if success {
            info!("Go check 成功");
        } else {
            warn!("Go check 失败 (exit {})", exit_code);
        }

        Ok(TestResult {
            success,
            stdout,
            stderr,
            exit_code,
            errors: vec![],
            test_summary: None,
        })
    }

    fn test(&self, dir: &Path) -> Result<TestResult> {
        info!("Go test: go test ./...");

        let output = Command::new("go")
            .arg("test")
            .arg("./...")
            .arg("-v")
            .current_dir(dir)
            .output()
            .context("无法执行 go test")?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(-1);
        let success = output.status.success();

        if success {
            info!("Go test 成功");
        } else {
            warn!("Go test 失败 (exit {})", exit_code);
        }

        Ok(TestResult {
            success,
            stdout,
            stderr,
            exit_code,
            errors: vec![],
            test_summary: None,
        })
    }

    fn run_binary(&self, dir: &Path, test_cases: &[E2ETestCase]) -> Result<Vec<E2ETestResult>> {
        if test_cases.is_empty() {
            return Ok(vec![]);
        }

        // 构建 Go 程序
        let tmpdir = tempfile::tempdir()?;
        let binary_path = tmpdir.path().join("forge_go_binary");

        let build_output = Command::new("go")
            .arg("build")
            .arg("-o")
            .arg(&binary_path)
            .arg(".")
            .current_dir(dir)
            .output()
            .context("无法执行 go build")?;

        if !build_output.status.success() {
            let stderr = String::from_utf8_lossy(&build_output.stderr).to_string();
            return Ok(test_cases
                .iter()
                .map(|tc| E2ETestResult {
                    test_case: tc.clone(),
                    stdout: String::new(),
                    stderr: format!("go build 失败:\n{}", stderr),
                    exit_code: -1,
                    passed: false,
                })
                .collect());
        }

        let mut results = Vec::new();
        for tc in test_cases {
            debug!("Go E2E: 运行测试用例 '{}'", tc.name);

            let mut cmd = Command::new(&binary_path);
            cmd.args(&tc.args);
            cmd.stdin(std::process::Stdio::piped());
            cmd.stdout(std::process::Stdio::piped());
            cmd.stderr(std::process::Stdio::piped());

            let mut child = cmd
                .spawn()
                .map_err(|e| anyhow::anyhow!("启动 Go 二进制失败: {}", e))?;

            if let Some(stdin_input) = &tc.stdin {
                use std::io::Write;
                if let Some(stdin) = child.stdin.as_mut() {
                    let _ = stdin.write_all(stdin_input.as_bytes());
                }
            }
            drop(child.stdin.take());

            let output = child.wait_with_output()?;

            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let exit_code = output.status.code().unwrap_or(-1);

            let stdout_ok = tc
                .expected_stdout
                .as_ref()
                .is_none_or(|expected| stdout.trim() == expected.trim());
            let exit_code_ok = tc
                .expected_exit_code
                .is_none_or(|expected| exit_code == expected);
            let passed = stdout_ok && exit_code_ok;

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
}

// ============================================================================
//  NodeAdapter — npm/node 工具链
// ============================================================================

/// Node.js 语言适配器 — 使用 npm/node 进行构建/测试
///
/// - `check`: `npx tsc --noEmit` (TypeScript) 或跳过 (JavaScript)
/// - `test`: `npm test`
/// - `run_binary`: `node index.js <args>` 运行主程序
pub struct NodeAdapter;

impl NodeAdapter {
    /// 查找主入口文件 (index.js, main.js, app.js)
    pub fn find_entry_point(dir: &Path) -> Option<std::path::PathBuf> {
        for name in &[
            "index.js",
            "main.js",
            "app.js",
            "src/index.js",
            "src/main.js",
        ] {
            let path = dir.join(name);
            if path.exists() {
                return Some(path);
            }
        }
        // 检查 package.json 的 main 字段
        let pkg_path = dir.join("package.json");
        if let Ok(content) = std::fs::read_to_string(&pkg_path) {
            if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(main) = pkg.get("main").and_then(|m| m.as_str()) {
                    let path = dir.join(main);
                    if path.exists() {
                        return Some(path);
                    }
                }
            }
        }
        None
    }

    /// 检查是否是 TypeScript 项目 (有 tsconfig.json)
    pub fn is_typescript(dir: &Path) -> bool {
        dir.join("tsconfig.json").exists()
    }
}

impl LanguageAdapter for NodeAdapter {
    fn language(&self) -> Language {
        Language::Node
    }

    fn check(&self, dir: &Path) -> Result<TestResult> {
        if !Self::is_typescript(dir) {
            // 纯 JavaScript 项目, 跳过类型检查
            return Ok(TestResult {
                success: true,
                stdout: "JavaScript 项目, 跳过类型检查".to_string(),
                stderr: String::new(),
                exit_code: 0,
                errors: vec![],
                test_summary: None,
            });
        }

        info!("Node check: npx tsc --noEmit");

        let output = Command::new("npx")
            .arg("tsc")
            .arg("--noEmit")
            .current_dir(dir)
            .output()
            .context("无法执行 npx tsc (请确认 node/npm 已安装)")?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(-1);
        let success = output.status.success();

        if success {
            info!("Node check 成功");
        } else {
            warn!("Node check 失败 (exit {})", exit_code);
        }

        Ok(TestResult {
            success,
            stdout,
            stderr,
            exit_code,
            errors: vec![],
            test_summary: None,
        })
    }

    fn test(&self, dir: &Path) -> Result<TestResult> {
        info!("Node test: npm test");

        let output = Command::new("npm")
            .arg("test")
            .current_dir(dir)
            .output()
            .context("无法执行 npm test")?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(-1);
        let success = output.status.success();

        if success {
            info!("Node test 成功");
        } else {
            warn!("Node test 失败 (exit {})", exit_code);
        }

        Ok(TestResult {
            success,
            stdout,
            stderr,
            exit_code,
            errors: vec![],
            test_summary: None,
        })
    }

    fn run_binary(&self, dir: &Path, test_cases: &[E2ETestCase]) -> Result<Vec<E2ETestResult>> {
        if test_cases.is_empty() {
            return Ok(vec![]);
        }

        let entry = Self::find_entry_point(dir)
            .ok_or_else(|| anyhow::anyhow!("找不到 Node.js 入口文件 (index.js/main.js)"))?;

        debug!("Node E2E: 入口文件: {}", entry.display());

        let mut results = Vec::new();
        for tc in test_cases {
            debug!("Node E2E: 运行测试用例 '{}'", tc.name);

            let mut cmd = Command::new("node");
            cmd.arg(&entry).args(&tc.args);
            cmd.current_dir(dir);
            cmd.stdin(std::process::Stdio::piped());
            cmd.stdout(std::process::Stdio::piped());
            cmd.stderr(std::process::Stdio::piped());

            let mut child = cmd
                .spawn()
                .map_err(|e| anyhow::anyhow!("启动 node 失败: {}", e))?;

            if let Some(stdin_input) = &tc.stdin {
                use std::io::Write;
                if let Some(stdin) = child.stdin.as_mut() {
                    let _ = stdin.write_all(stdin_input.as_bytes());
                }
            }
            drop(child.stdin.take());

            let output = child.wait_with_output()?;

            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let exit_code = output.status.code().unwrap_or(-1);

            let stdout_ok = tc
                .expected_stdout
                .as_ref()
                .is_none_or(|expected| stdout.trim() == expected.trim());
            let exit_code_ok = tc
                .expected_exit_code
                .is_none_or(|expected| exit_code == expected);
            let passed = stdout_ok && exit_code_ok;

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
}

// ============================================================================
//  MultiLanguageTestRunner — 自动检测语言并委托
// ============================================================================

/// 多语言测试运行器 — 自动检测项目语言并委托给对应的适配器
///
/// 实现 `TestRunner` trait, 与现有的 `CargoTestRunner` 接口兼容。
///
/// 检测规则:
/// - `Cargo.toml` → Rust (cargo)
/// - `pyproject.toml` / `setup.py` / `requirements.txt` → Python (python/pytest)
/// - `go.mod` → Go (go)
/// - `package.json` → Node (npm/node)
///
/// 使用方式:
/// ```no_run
/// use forge::language::MultiLanguageTestRunner;
/// // 替换 CargoTestRunner, 自动适配不同语言
/// ```
pub struct MultiLanguageTestRunner;

impl MultiLanguageTestRunner {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MultiLanguageTestRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl TestRunner for MultiLanguageTestRunner {
    fn check(&self, dir: &Path) -> Result<TestResult> {
        let lang = detect_language(dir);
        info!("MultiLanguage: 检测到语言: {}", lang);
        get_adapter(dir).check(dir)
    }

    fn test(&self, dir: &Path) -> Result<TestResult> {
        let lang = detect_language(dir);
        info!("MultiLanguage: 检测到语言: {}", lang);
        get_adapter(dir).test(dir)
    }

    fn run_binary(&self, dir: &Path, test_cases: &[E2ETestCase]) -> Result<Vec<E2ETestResult>> {
        let lang = detect_language(dir);
        info!("MultiLanguage: 检测到语言: {}", lang);
        get_adapter(dir).run_binary(dir, test_cases)
    }
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // ===== detect_language =====

    #[test]
    fn test_detect_rust() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();
        assert_eq!(detect_language(dir.path()), Language::Rust);
    }

    #[test]
    fn test_detect_python_pyproject() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), "[project]").unwrap();
        assert_eq!(detect_language(dir.path()), Language::Python);
    }

    #[test]
    fn test_detect_python_setup_py() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("setup.py"), "from setuptools import setup").unwrap();
        assert_eq!(detect_language(dir.path()), Language::Python);
    }

    #[test]
    fn test_detect_python_requirements() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("requirements.txt"), "flask\nrequests").unwrap();
        assert_eq!(detect_language(dir.path()), Language::Python);
    }

    #[test]
    fn test_detect_go() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module test").unwrap();
        assert_eq!(detect_language(dir.path()), Language::Go);
    }

    #[test]
    fn test_detect_node() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), r#"{"name":"test"}"#).unwrap();
        assert_eq!(detect_language(dir.path()), Language::Node);
    }

    #[test]
    fn test_detect_unknown() {
        let dir = tempdir().unwrap();
        assert_eq!(detect_language(dir.path()), Language::Unknown);
    }

    #[test]
    fn test_detect_rust_takes_priority() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        // Cargo.toml 优先检测
        assert_eq!(detect_language(dir.path()), Language::Rust);
    }

    // ===== Language =====

    #[test]
    fn test_language_display() {
        assert_eq!(Language::Rust.to_string(), "Rust");
        assert_eq!(Language::Python.to_string(), "Python");
        assert_eq!(Language::Go.to_string(), "Go");
        assert_eq!(Language::Node.to_string(), "Node.js");
        assert_eq!(Language::Unknown.to_string(), "Unknown");
    }

    #[test]
    fn test_language_display_name() {
        assert_eq!(Language::Rust.display_name(), "Rust");
        assert_eq!(Language::Python.display_name(), "Python");
    }

    // ===== get_adapter =====

    #[test]
    fn test_get_adapter_rust() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        let adapter = get_adapter(dir.path());
        assert_eq!(adapter.language(), Language::Rust);
    }

    #[test]
    fn test_get_adapter_python() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), "[project]").unwrap();
        let adapter = get_adapter(dir.path());
        assert_eq!(adapter.language(), Language::Python);
    }

    #[test]
    fn test_get_adapter_go() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module test").unwrap();
        let adapter = get_adapter(dir.path());
        assert_eq!(adapter.language(), Language::Go);
    }

    #[test]
    fn test_get_adapter_node() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        let adapter = get_adapter(dir.path());
        assert_eq!(adapter.language(), Language::Node);
    }

    #[test]
    fn test_get_adapter_unknown_defaults_rust() {
        let dir = tempdir().unwrap();
        let adapter = get_adapter(dir.path());
        assert_eq!(adapter.language(), Language::Rust);
    }

    // ===== RustAdapter =====

    #[test]
    fn test_rust_adapter_language() {
        let adapter = RustAdapter;
        assert_eq!(adapter.language(), Language::Rust);
    }

    // ===== PythonAdapter =====

    #[test]
    fn test_python_adapter_language() {
        let adapter = PythonAdapter;
        assert_eq!(adapter.language(), Language::Python);
    }

    #[test]
    fn test_python_find_py_files() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("main.py"), "print('hello')").unwrap();
        std::fs::write(dir.path().join("utils.py"), "def foo(): pass").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub").join("mod.py"), "x = 1").unwrap();
        // 应跳过 __pycache__
        std::fs::create_dir(dir.path().join("__pycache__")).unwrap();
        std::fs::write(dir.path().join("__pycache__").join("cached.py"), "y = 2").unwrap();

        let files = PythonAdapter::find_python_files(dir.path());
        assert_eq!(files.len(), 3);
    }

    #[test]
    fn test_python_find_entry_point() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("main.py"), "print('hello')").unwrap();
        let entry = PythonAdapter::find_entry_point(dir.path());
        assert!(entry.is_some());
        assert!(entry.unwrap().to_string_lossy().contains("main.py"));
    }

    #[test]
    fn test_python_find_entry_point_app() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("app.py"), "print('hello')").unwrap();
        let entry = PythonAdapter::find_entry_point(dir.path());
        assert!(entry.is_some());
        assert!(entry.unwrap().to_string_lossy().contains("app.py"));
    }

    #[test]
    fn test_python_find_entry_point_fallback() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("script.py"), "print('hello')").unwrap();
        let entry = PythonAdapter::find_entry_point(dir.path());
        assert!(entry.is_some());
    }

    #[test]
    fn test_python_find_entry_point_none() {
        let dir = tempdir().unwrap();
        assert!(PythonAdapter::find_entry_point(dir.path()).is_none());
    }

    // ===== GoAdapter =====

    #[test]
    fn test_go_adapter_language() {
        let adapter = GoAdapter;
        assert_eq!(adapter.language(), Language::Go);
    }

    // ===== NodeAdapter =====

    #[test]
    fn test_node_adapter_language() {
        let adapter = NodeAdapter;
        assert_eq!(adapter.language(), Language::Node);
    }

    #[test]
    fn test_node_find_entry_point_index() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("index.js"), "console.log('hello')").unwrap();
        let entry = NodeAdapter::find_entry_point(dir.path());
        assert!(entry.is_some());
        assert!(entry.unwrap().to_string_lossy().contains("index.js"));
    }

    #[test]
    fn test_node_find_entry_point_main() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("main.js"), "console.log('hello')").unwrap();
        let entry = NodeAdapter::find_entry_point(dir.path());
        assert!(entry.is_some());
    }

    #[test]
    fn test_node_find_entry_point_from_package_json() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"test","main":"src/app.js"}"#,
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src").join("app.js"),
            "console.log('hello')",
        )
        .unwrap();
        let entry = NodeAdapter::find_entry_point(dir.path());
        assert!(entry.is_some());
        assert!(entry.unwrap().to_string_lossy().contains("app.js"));
    }

    #[test]
    fn test_node_find_entry_point_none() {
        let dir = tempdir().unwrap();
        assert!(NodeAdapter::find_entry_point(dir.path()).is_none());
    }

    #[test]
    fn test_node_is_typescript() {
        let dir = tempdir().unwrap();
        assert!(!NodeAdapter::is_typescript(dir.path()));

        std::fs::write(dir.path().join("tsconfig.json"), "{}").unwrap();
        assert!(NodeAdapter::is_typescript(dir.path()));
    }

    // ===== MultiLanguageTestRunner =====

    #[test]
    fn test_multi_language_runner_detects_rust() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        // 检测应该工作
        assert_eq!(detect_language(dir.path()), Language::Rust);
    }

    #[test]
    fn test_multi_language_runner_default() {
        let runner = MultiLanguageTestRunner::new();
        let _ = runner; // 只验证可以创建
    }
}
