//! 多语言项目支持集成测试 — 方向 B
//!
//! 验证 MultiLanguageTestRunner 和 LanguageAdapter 的核心行为:
//! 1. 语言检测 — Rust/Python/Go/Node 项目文件检测
//! 2. MultiLanguageTestRunner 实现 TestRunner trait
//! 3. RustAdapter — 对 Rust 项目使用 cargo (与 CargoTestRunner 行为一致)
//! 4. 各适配器的 language() 返回正确值
//! 5. Python 文件查找和入口点检测
//! 6. Node.js 入口点检测 (含 package.json main 字段)
//! 7. TypeScript 检测

use forge::language::{
    detect_language, get_adapter, GoAdapter, MultiLanguageTestRunner, NodeAdapter, PythonAdapter,
    RustAdapter,
};
use forge::traits::{Language, LanguageAdapter, TestRunner};
use tempfile::tempdir;

// ============================================================================
//  语言检测测试
// ============================================================================

#[test]
fn test_detect_rust_project() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"test\"\nversion = \"0.1.0\"",
    )
    .unwrap();
    assert_eq!(detect_language(dir.path()), Language::Rust);
}

#[test]
fn test_detect_python_pyproject() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("pyproject.toml"), "[project]").unwrap();
    std::fs::write(dir.path().join("main.py"), "print('hello')").unwrap();
    assert_eq!(detect_language(dir.path()), Language::Python);
}

#[test]
fn test_detect_python_setup_py() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("setup.py"), "from setuptools import setup").unwrap();
    assert_eq!(detect_language(dir.path()), Language::Python);
}

#[test]
fn test_detect_python_requirements_txt() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("requirements.txt"), "flask\nrequests").unwrap();
    assert_eq!(detect_language(dir.path()), Language::Python);
}

#[test]
fn test_detect_go_project() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("go.mod"), "module test").unwrap();
    std::fs::write(dir.path().join("main.go"), "package main").unwrap();
    assert_eq!(detect_language(dir.path()), Language::Go);
}

#[test]
fn test_detect_node_project() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("package.json"),
        r#"{"name":"test","main":"index.js"}"#,
    )
    .unwrap();
    assert_eq!(detect_language(dir.path()), Language::Node);
}

#[test]
fn test_detect_unknown_project() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("README.md"), "# Test").unwrap();
    assert_eq!(detect_language(dir.path()), Language::Unknown);
}

#[test]
fn test_detect_priority_rust_over_node() {
    let dir = tempdir().unwrap();
    // 同时有 Cargo.toml 和 package.json → 应检测为 Rust
    std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
    std::fs::write(dir.path().join("package.json"), "{}").unwrap();
    assert_eq!(detect_language(dir.path()), Language::Rust);
}

// ============================================================================
//  get_adapter 测试
// ============================================================================

#[test]
fn test_get_adapter_for_rust() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
    let adapter = get_adapter(dir.path());
    assert_eq!(adapter.language(), Language::Rust);
}

#[test]
fn test_get_adapter_for_python() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("pyproject.toml"), "[project]").unwrap();
    let adapter = get_adapter(dir.path());
    assert_eq!(adapter.language(), Language::Python);
}

#[test]
fn test_get_adapter_for_go() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("go.mod"), "module test").unwrap();
    let adapter = get_adapter(dir.path());
    assert_eq!(adapter.language(), Language::Go);
}

#[test]
fn test_get_adapter_for_node() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}").unwrap();
    let adapter = get_adapter(dir.path());
    assert_eq!(adapter.language(), Language::Node);
}

#[test]
fn test_get_adapter_unknown_defaults_to_rust() {
    let dir = tempdir().unwrap();
    let adapter = get_adapter(dir.path());
    assert_eq!(
        adapter.language(),
        Language::Rust,
        "未知语言应默认使用 Rust 适配器"
    );
}

// ============================================================================
//  LanguageAdapter 实现测试
// ============================================================================

#[test]
fn test_rust_adapter_language() {
    assert_eq!(RustAdapter.language(), Language::Rust);
}

#[test]
fn test_python_adapter_language() {
    assert_eq!(PythonAdapter.language(), Language::Python);
}

#[test]
fn test_go_adapter_language() {
    assert_eq!(GoAdapter.language(), Language::Go);
}

#[test]
fn test_node_adapter_language() {
    assert_eq!(NodeAdapter.language(), Language::Node);
}

// ============================================================================
//  MultiLanguageTestRunner 测试 (TestRunner trait)
// ============================================================================

#[test]
fn test_multi_language_runner_implements_test_runner() {
    // 验证 MultiLanguageTestRunner 实现了 TestRunner trait
    let runner = MultiLanguageTestRunner::new();
    let dir = tempdir().unwrap();
    // 对空目录执行 check — 应返回某种结果 (可能成功或失败, 取决于 cargo)
    let _ = runner.check(dir.path());
}

#[test]
fn test_multi_language_runner_new() {
    let runner = MultiLanguageTestRunner::new();
    let _ = runner;
}

#[test]
fn test_multi_language_runner_default() {
    let runner = MultiLanguageTestRunner::new();
    let _ = runner;
}

// ============================================================================
//  Python 文件查找测试
// ============================================================================

#[test]
fn test_python_find_py_files_in_root() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("main.py"), "print('hello')").unwrap();
    std::fs::write(dir.path().join("utils.py"), "def foo(): pass").unwrap();
    let files = PythonAdapter::find_python_files(dir.path());
    assert_eq!(files.len(), 2);
}

#[test]
fn test_python_find_py_files_in_subdirs() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("pkg")).unwrap();
    std::fs::write(dir.path().join("main.py"), "print('hello')").unwrap();
    std::fs::write(dir.path().join("pkg").join("mod.py"), "x = 1").unwrap();
    let files = PythonAdapter::find_python_files(dir.path());
    assert_eq!(files.len(), 2);
}

#[test]
fn test_python_find_py_files_skips_pycache() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("main.py"), "print('hello')").unwrap();
    std::fs::create_dir_all(dir.path().join("__pycache__")).unwrap();
    std::fs::write(dir.path().join("__pycache__").join("cached.py"), "y = 2").unwrap();
    let files = PythonAdapter::find_python_files(dir.path());
    assert_eq!(files.len(), 1, "__pycache__ 中的文件应被跳过");
}

#[test]
fn test_python_find_py_files_skips_venv() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("main.py"), "print('hello')").unwrap();
    std::fs::create_dir_all(dir.path().join("venv").join("lib")).unwrap();
    std::fs::write(dir.path().join("venv").join("lib").join("site.py"), "x = 2").unwrap();
    let files = PythonAdapter::find_python_files(dir.path());
    assert_eq!(files.len(), 1, "venv 中的文件应被跳过");
}

#[test]
fn test_python_find_entry_point_main_py() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("main.py"), "print('hello')").unwrap();
    let entry = PythonAdapter::find_entry_point(dir.path());
    assert!(entry.is_some());
    assert!(entry.unwrap().to_string_lossy().ends_with("main.py"));
}

#[test]
fn test_python_find_entry_point_app_py() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("app.py"), "print('hello')").unwrap();
    let entry = PythonAdapter::find_entry_point(dir.path());
    assert!(entry.is_some());
    assert!(entry.unwrap().to_string_lossy().ends_with("app.py"));
}

#[test]
fn test_python_find_entry_point_fallback_any_py() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("script.py"), "print('hello')").unwrap();
    let entry = PythonAdapter::find_entry_point(dir.path());
    assert!(entry.is_some(), "无 main.py/app.py 时应回退到任意 .py 文件");
}

#[test]
fn test_python_find_entry_point_none() {
    let dir = tempdir().unwrap();
    assert!(PythonAdapter::find_entry_point(dir.path()).is_none());
}

// ============================================================================
//  Node.js 入口点检测测试
// ============================================================================

#[test]
fn test_node_find_entry_point_index_js() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("index.js"), "console.log('hello')").unwrap();
    let entry = NodeAdapter::find_entry_point(dir.path());
    assert!(entry.is_some());
    assert!(entry.unwrap().to_string_lossy().ends_with("index.js"));
}

#[test]
fn test_node_find_entry_point_main_js() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("main.js"), "console.log('hello')").unwrap();
    let entry = NodeAdapter::find_entry_point(dir.path());
    assert!(entry.is_some());
}

#[test]
fn test_node_find_entry_point_from_package_json_main() {
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
    assert!(entry.unwrap().to_string_lossy().ends_with("app.js"));
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

// ============================================================================
//  Language 枚举测试
// ============================================================================

#[test]
fn test_language_display() {
    assert_eq!(Language::Rust.to_string(), "Rust");
    assert_eq!(Language::Python.to_string(), "Python");
    assert_eq!(Language::Go.to_string(), "Go");
    assert_eq!(Language::Node.to_string(), "Node.js");
    assert_eq!(Language::Unknown.to_string(), "Unknown");
}

#[test]
fn test_language_equality() {
    assert_eq!(Language::Rust, Language::Rust);
    assert_ne!(Language::Rust, Language::Python);
}

#[test]
fn test_language_display_name() {
    assert_eq!(Language::Rust.display_name(), "Rust");
    assert_eq!(Language::Python.display_name(), "Python");
    assert_eq!(Language::Go.display_name(), "Go");
    assert_eq!(Language::Node.display_name(), "Node.js");
    assert_eq!(Language::Unknown.display_name(), "Unknown");
}
