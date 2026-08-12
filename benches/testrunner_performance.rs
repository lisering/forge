#![allow(clippy::useless_vec)]

//! testrunner 性能基准测试
//!
//! 测试目标:
//! 1. test_result_construction - TestResult 构造和字段
//! 2. network_error_detection - is_network_error 各种模式
//! 3. feedback_formatting - to_feedback (TestResult/E2ETestResult/E2ETestSummary)
//! 4. e2e_test_case_serde - E2ETestCase 序列化/反序列化
//! 5. edge_cases - 边界场景 (空/大输出/大量错误)

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use forge::testrunner::*;

// ============================================================================
//  基准测试 1: test_result_construction
// ============================================================================

fn bench_test_result_construction(c: &mut Criterion) {
    let mut group = c.benchmark_group("test_result_construction");

    // 成功结果
    group.bench_function("success", |b| {
        b.iter(|| {
            let result = TestResult {
                success: true,
                stdout: "Compiling...\nFinished".to_string(),
                stderr: String::new(),
                exit_code: 0,
                errors: vec![],
                test_summary: None,
            };
            black_box(result);
        })
    });

    // 失败结果 (有错误)
    group.bench_function("failure_with_errors", |b| {
        b.iter(|| {
            let result = TestResult {
                success: false,
                stdout: String::new(),
                stderr: "error[E0308]: mismatched types\n --> src/main.rs:10:5".to_string(),
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
            black_box(result);
        })
    });

    // 带测试摘要
    group.bench_function("with_test_summary", |b| {
        b.iter(|| {
            let result = TestResult {
                success: true,
                stdout: "running 5 tests...\ntest result: ok. 5 passed; 0 failed".to_string(),
                stderr: String::new(),
                exit_code: 0,
                errors: vec![],
                test_summary: Some(TestSummary {
                    total: 5,
                    passed: 5,
                    failed: 0,
                    ignored: 0,
                }),
            };
            black_box(result);
        })
    });

    // clone
    let result = TestResult {
        success: true,
        stdout: "ok".to_string(),
        stderr: String::new(),
        exit_code: 0,
        errors: vec![],
        test_summary: None,
    };
    group.bench_function("clone", |b| {
        b.iter(|| {
            let cloned = black_box(&result).clone();
            black_box(cloned);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 2: network_error_detection
// ============================================================================

fn bench_network_error_detection(c: &mut Criterion) {
    let mut group = c.benchmark_group("network_error_detection");

    // 各种网络错误模式
    let network_errors = vec![
        (
            "couldnt_connect",
            "error: Couldn't connect to server\nFailed to connect to 127.0.0.1 port 7890",
        ),
        ("failed_to_connect", "error: Failed to connect to crates.io"),
        (
            "unable_update_registry",
            "error: unable to update registry `crates-io`",
        ),
        ("download_failed", "error: download of config.json failed"),
        (
            "spurious_network",
            "warning: spurious network error (2 tries remaining)",
        ),
        ("could_not_connect", "error: Could not connect to server"),
    ];

    for (name, stderr) in &network_errors {
        let result = TestResult {
            success: false,
            stdout: String::new(),
            stderr: stderr.to_string(),
            exit_code: 101,
            errors: vec![],
            test_summary: None,
        };
        group.bench_function(format!("detect_{}", name), |b| {
            b.iter(|| {
                let is_net = black_box(&result).is_network_error();
                assert!(is_net);
                black_box(is_net);
            })
        });
    }

    // 非网络错误 (编译错误)
    let compile_error_result = TestResult {
        success: false,
        stdout: String::new(),
        stderr: "error[E0308]: mismatched types\n --> src/main.rs:10:5".to_string(),
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
    group.bench_function("not_network_error_with_compile_errors", |b| {
        b.iter(|| {
            let is_net = black_box(&compile_error_result).is_network_error();
            assert!(!is_net);
            black_box(is_net);
        })
    });

    // 无错误无网络模式
    let clean_result = TestResult {
        success: true,
        stdout: String::new(),
        stderr: String::new(),
        exit_code: 0,
        errors: vec![],
        test_summary: None,
    };
    group.bench_function("not_network_error_clean", |b| {
        b.iter(|| {
            let is_net = black_box(&clean_result).is_network_error();
            assert!(!is_net);
            black_box(is_net);
        })
    });

    // 批量检测 6 种
    let results: Vec<TestResult> = network_errors
        .iter()
        .map(|(_, stderr)| TestResult {
            success: false,
            stdout: String::new(),
            stderr: stderr.to_string(),
            exit_code: 101,
            errors: vec![],
            test_summary: None,
        })
        .collect();
    group.bench_function("batch_6", |b| {
        b.iter(|| {
            let flags: Vec<bool> = black_box(&results)
                .iter()
                .map(|r| r.is_network_error())
                .collect();
            black_box(flags);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 3: feedback_formatting
// ============================================================================

fn bench_feedback_formatting(c: &mut Criterion) {
    let mut group = c.benchmark_group("feedback_formatting");

    // TestResult::to_feedback 成功
    let success_result = TestResult {
        success: true,
        stdout: String::new(),
        stderr: String::new(),
        exit_code: 0,
        errors: vec![],
        test_summary: None,
    };
    group.bench_function("test_result_success_feedback", |b| {
        b.iter(|| {
            let feedback = success_result.to_feedback();
            black_box(feedback);
        })
    });

    // TestResult::to_feedback 失败 (有错误)
    let error_result = TestResult {
        success: false,
        stdout: String::new(),
        stderr: "error[E0308]: mismatched types\n --> src/main.rs:10:5".to_string(),
        exit_code: 101,
        errors: vec![
            CompileError {
                file: "src/main.rs".to_string(),
                line: Some(10),
                column: Some(5),
                message: "mismatched types".to_string(),
                error_code: Some("E0308".to_string()),
            },
            CompileError {
                file: "src/lib.rs".to_string(),
                line: Some(20),
                column: Some(10),
                message: "expected `()`, found integer".to_string(),
                error_code: None,
            },
        ],
        test_summary: Some(TestSummary {
            total: 3,
            passed: 1,
            failed: 2,
            ignored: 0,
        }),
    };
    group.bench_function("test_result_error_feedback", |b| {
        b.iter(|| {
            let feedback = error_result.to_feedback();
            black_box(feedback);
        })
    });

    // TestResult::to_feedback 网络错误
    let network_result = TestResult {
        success: false,
        stdout: String::new(),
        stderr: "error: spurious network error (2 tries remaining)".to_string(),
        exit_code: 101,
        errors: vec![],
        test_summary: None,
    };
    group.bench_function("test_result_network_error_feedback", |b| {
        b.iter(|| {
            let feedback = network_result.to_feedback();
            black_box(feedback);
        })
    });

    // E2ETestResult::to_feedback 通过
    let e2e_pass = E2ETestResult {
        test_case: E2ETestCase {
            name: "test1".to_string(),
            stdin: None,
            args: vec![],
            expected_stdout: Some("hello".to_string()),
            expected_exit_code: Some(0),
        },
        stdout: "hello".to_string(),
        stderr: String::new(),
        exit_code: 0,
        passed: true,
    };
    group.bench_function("e2e_result_pass_feedback", |b| {
        b.iter(|| {
            let feedback = e2e_pass.to_feedback();
            black_box(feedback);
        })
    });

    // E2ETestResult::to_feedback 失败
    let e2e_fail = E2ETestResult {
        test_case: E2ETestCase {
            name: "test2".to_string(),
            stdin: Some("input".to_string()),
            args: vec!["--flag".to_string()],
            expected_stdout: Some("expected".to_string()),
            expected_exit_code: Some(0),
        },
        stdout: "actual".to_string(),
        stderr: "some error".to_string(),
        exit_code: 1,
        passed: false,
    };
    group.bench_function("e2e_result_fail_feedback", |b| {
        b.iter(|| {
            let feedback = e2e_fail.to_feedback();
            black_box(feedback);
        })
    });

    // E2ETestSummary::to_feedback
    let summary = E2ETestSummary {
        total: 3,
        passed: 2,
        failed: 1,
        results: vec![e2e_fail.clone()],
    };
    group.bench_function("e2e_summary_feedback", |b| {
        b.iter(|| {
            let feedback = summary.to_feedback();
            black_box(feedback);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 4: e2e_test_case_serde
// ============================================================================

fn bench_e2e_test_case_serde(c: &mut Criterion) {
    let mut group = c.benchmark_group("e2e_test_case_serde");

    let case = E2ETestCase {
        name: "test_full".to_string(),
        stdin: Some("hello world".to_string()),
        args: vec!["--flag".to_string(), "--verbose".to_string()],
        expected_stdout: Some("expected output".to_string()),
        expected_exit_code: Some(0),
    };

    // serialize
    group.bench_function("serialize", |b| {
        b.iter(|| {
            let json = serde_json::to_string(black_box(&case)).unwrap();
            black_box(json);
        })
    });

    // deserialize
    let json_str = serde_json::to_string(&case).unwrap();
    group.bench_function("deserialize", |b| {
        b.iter(|| {
            let case: E2ETestCase = serde_json::from_str(black_box(&json_str)).unwrap();
            black_box(case);
        })
    });

    // roundtrip
    group.bench_function("roundtrip", |b| {
        b.iter(|| {
            let json = serde_json::to_string(&case).unwrap();
            let back: E2ETestCase = serde_json::from_str(&json).unwrap();
            black_box(back);
        })
    });

    // 最小用例
    let minimal = E2ETestCase {
        name: "minimal".to_string(),
        stdin: None,
        args: vec![],
        expected_stdout: None,
        expected_exit_code: None,
    };
    group.bench_function("serialize_minimal", |b| {
        b.iter(|| {
            let json = serde_json::to_string(&minimal).unwrap();
            black_box(json);
        })
    });

    // 批量序列化 100 个
    let cases: Vec<E2ETestCase> = (0..100)
        .map(|i| E2ETestCase {
            name: format!("test_{}", i),
            stdin: Some(format!("input_{}", i)),
            args: vec![format!("--arg{}", i)],
            expected_stdout: Some(format!("output_{}", i)),
            expected_exit_code: Some(0),
        })
        .collect();
    group.bench_function("serialize_batch_100", |b| {
        b.iter(|| {
            let json = serde_json::to_string(black_box(&cases)).unwrap();
            black_box(json);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 5: edge_cases
// ============================================================================

fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("testrunner_edge_cases");

    // 空 stderr 的 to_feedback
    let empty_result = TestResult {
        success: false,
        stdout: String::new(),
        stderr: String::new(),
        exit_code: 1,
        errors: vec![],
        test_summary: None,
    };
    group.bench_function("empty_stderr_feedback", |b| {
        b.iter(|| {
            let feedback = empty_result.to_feedback();
            black_box(feedback);
        })
    });

    // 大 stderr (10000 字符) 的 to_feedback
    let large_result = TestResult {
        success: false,
        stdout: String::new(),
        stderr: "x".repeat(10000),
        exit_code: 1,
        errors: vec![],
        test_summary: None,
    };
    group.bench_function("large_stderr_feedback", |b| {
        b.iter(|| {
            let feedback = large_result.to_feedback();
            black_box(feedback);
        })
    });

    // 大量错误 (20个)
    let many_errors: Vec<CompileError> = (0..20)
        .map(|i| CompileError {
            file: format!("src/file_{}.rs", i),
            line: Some(i * 10),
            column: Some(i),
            message: format!("Error number {}", i),
            error_code: Some(format!("E{:04}", i)),
        })
        .collect();
    let many_error_result = TestResult {
        success: false,
        stdout: String::new(),
        stderr: "multiple errors".to_string(),
        exit_code: 101,
        errors: many_errors,
        test_summary: None,
    };
    group.bench_function("many_errors_feedback", |b| {
        b.iter(|| {
            let feedback = many_error_result.to_feedback();
            black_box(feedback);
        })
    });

    // E2ETestSummary success
    let success_summary = E2ETestSummary {
        total: 10,
        passed: 10,
        failed: 0,
        results: vec![],
    };
    group.bench_function("e2e_summary_success", |b| {
        b.iter(|| {
            let feedback = success_summary.to_feedback();
            black_box(feedback);
        })
    });

    // E2ETestSummary with 10 failed results
    let failed_results: Vec<E2ETestResult> = (0..10)
        .map(|i| E2ETestResult {
            test_case: E2ETestCase {
                name: format!("test_{}", i),
                stdin: None,
                args: vec![],
                expected_stdout: Some("expected".to_string()),
                expected_exit_code: Some(0),
            },
            stdout: "actual".to_string(),
            stderr: String::new(),
            exit_code: 1,
            passed: false,
        })
        .collect();
    let failed_summary = E2ETestSummary {
        total: 10,
        passed: 0,
        failed: 10,
        results: failed_results,
    };
    group.bench_function("e2e_summary_10_failures", |b| {
        b.iter(|| {
            let feedback = failed_summary.to_feedback();
            black_box(feedback);
        })
    });

    group.finish();
}

// ============================================================================
//  配置 & 入口
// ============================================================================

fn configure_criterion() -> Criterion {
    Criterion::default()
        .sample_size(50)
        .measurement_time(std::time::Duration::from_secs(5))
        .warm_up_time(std::time::Duration::from_secs(2))
        .nresamples(50_000)
        .noise_threshold(0.05)
        .output_directory(std::path::Path::new("target/criterion/testrunner"))
}

criterion_group! {
    name = testrunner_benches;
    config = configure_criterion();
    targets = bench_test_result_construction,
        bench_network_error_detection,
        bench_feedback_formatting,
        bench_e2e_test_case_serde,
        bench_edge_cases,
}

criterion_main!(testrunner_benches);
