#![allow(clippy::useless_vec)]

//! Loop Detector 模块性能基准测试
//!
//! 测试目标:
//! 1. make_error_signature - 错误签名生成性能
//! 2. has_any_repeated - 重复检测性能 (codes/signatures/files)
//! 3. collect_repeated - 重复收集性能
//! 4. LoopDetector::is_looping - 完整循环检测性能
//! 5. edge_cases - 边界条件性能

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use forge::loop_detector::{self, ErrorRound, LoopDetector};
use forge::testrunner::CompileError;

/// 构建测试用 CompileError
fn make_error(code: Option<&str>, msg: &str, file: &str) -> CompileError {
    CompileError {
        file: file.to_string(),
        line: Some(10),
        column: Some(5),
        message: msg.to_string(),
        error_code: code.map(String::from),
    }
}

/// 构建多轮重复错误
fn build_repeated_rounds(count: usize, errors_per_round: usize) -> Vec<ErrorRound> {
    (0..count)
        .map(|_| {
            let errors: Vec<CompileError> = (0..errors_per_round)
                .map(|i| make_error(Some("E0308"), &format!("error type {i}"), "src/main.rs"))
                .collect();
            ErrorRound::from_errors(&errors)
        })
        .collect()
}

/// 构建多轮不同错误
fn build_distinct_rounds(count: usize, errors_per_round: usize) -> Vec<ErrorRound> {
    (0..count)
        .map(|round_idx| {
            let errors: Vec<CompileError> = (0..errors_per_round)
                .map(|i| {
                    make_error(
                        Some(&format!("E{:04}", round_idx * 100 + i)),
                        &format!("unique error {round_idx}-{i}"),
                        &format!("src/file_{round_idx}_{i}.rs"),
                    )
                })
                .collect();
            ErrorRound::from_errors(&errors)
        })
        .collect()
}

/// 基准测试: make_error_signature
fn bench_make_error_signature(c: &mut Criterion) {
    let mut group = c.benchmark_group("make_error_signature");

    let long_msg = "x".repeat(500);
    let test_cases = vec![
        (
            "short",
            make_error(Some("E0308"), "mismatched types", "src/main.rs"),
        ),
        (
            "no_code",
            make_error(None, "cannot borrow `x`", "src/main.rs"),
        ),
        (
            "long_msg",
            make_error(Some("E0308"), &long_msg, "src/main.rs"),
        ),
    ];

    for (name, error) in &test_cases {
        group.bench_function(*name, |b| {
            b.iter(|| black_box(loop_detector::make_error_signature(black_box(error))))
        });
    }
    group.finish();
}

/// 基准测试: has_any_repeated (codes/signatures/files)
fn bench_has_any_repeated(c: &mut Criterion) {
    let mut group = c.benchmark_group("has_any_repeated");

    let sizes: Vec<usize> = vec![3, 10, 50, 200];

    for &size in &sizes {
        group.throughput(Throughput::Elements(size as u64));
        let repeated_rounds = build_repeated_rounds(size, 3);
        let distinct_rounds = build_distinct_rounds(size, 3);

        // 重复错误检测
        group.bench_with_input(
            BenchmarkId::new("codes_repeated", size),
            &repeated_rounds,
            |b, rounds| {
                b.iter(|| black_box(loop_detector::has_any_repeated_codes(black_box(rounds), 3)))
            },
        );

        // 不同错误检测 (快速返回 false)
        group.bench_with_input(
            BenchmarkId::new("codes_distinct", size),
            &distinct_rounds,
            |b, rounds| {
                b.iter(|| black_box(loop_detector::has_any_repeated_codes(black_box(rounds), 3)))
            },
        );

        // 签名重复检测
        group.bench_with_input(
            BenchmarkId::new("signatures_repeated", size),
            &repeated_rounds,
            |b, rounds| {
                b.iter(|| {
                    black_box(loop_detector::has_any_repeated_signatures(
                        black_box(rounds),
                        3,
                    ))
                })
            },
        );

        // 文件重复检测
        group.bench_with_input(
            BenchmarkId::new("files_repeated", size),
            &repeated_rounds,
            |b, rounds| {
                b.iter(|| black_box(loop_detector::has_any_repeated_files(black_box(rounds), 3)))
            },
        );
    }
    group.finish();
}

/// 基准测试: collect_repeated (signatures/files)
fn bench_collect_repeated(c: &mut Criterion) {
    let mut group = c.benchmark_group("collect_repeated");

    let sizes: Vec<usize> = vec![3, 10, 50, 200];

    for &size in &sizes {
        group.throughput(Throughput::Elements(size as u64));
        let repeated_rounds = build_repeated_rounds(size, 5);

        group.bench_with_input(
            BenchmarkId::new("signatures", size),
            &repeated_rounds,
            |b, rounds| {
                b.iter(|| {
                    black_box(loop_detector::collect_repeated_signatures(
                        black_box(rounds),
                        3,
                    ))
                })
            },
        );

        group.bench_with_input(
            BenchmarkId::new("files", size),
            &repeated_rounds,
            |b, rounds| {
                b.iter(|| black_box(loop_detector::collect_repeated_files(black_box(rounds), 3)))
            },
        );
    }
    group.finish();
}

/// 基准测试: LoopDetector::is_looping (完整循环检测流程)
fn bench_is_looping(c: &mut Criterion) {
    let mut group = c.benchmark_group("loop_detector_is_looping");

    let sizes: Vec<usize> = vec![3, 10, 50, 200];

    for &size in &sizes {
        group.throughput(Throughput::Elements(size as u64));
        let errors = vec![make_error(Some("E0308"), "mismatched types", "src/main.rs")];

        // 预构建 detector
        let mut detector = LoopDetector::new(3);
        for _ in 0..size {
            detector.record_errors(&errors);
        }

        group.bench_function(format!("looping_{size}"), |b| {
            b.iter(|| black_box(detector.is_looping()))
        });

        // 非循环 detector (不同错误)
        let distinct_errors: Vec<CompileError> = (0..size)
            .map(|i| {
                make_error(
                    Some(&format!("E{:04}", i)),
                    &format!("error {i}"),
                    "src/main.rs",
                )
            })
            .collect();
        let mut detector_clean = LoopDetector::new(3);
        for err in &distinct_errors {
            detector_clean.record_errors(std::slice::from_ref(err));
        }

        group.bench_function(format!("not_looping_{size}"), |b| {
            b.iter(|| black_box(detector_clean.is_looping()))
        });
    }
    group.finish();
}

/// 边界条件基准测试
fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_cases");

    // 空轮次
    let empty_rounds: Vec<ErrorRound> = vec![];
    group.bench_function("empty_rounds", |b| {
        b.iter(|| {
            black_box(loop_detector::has_any_repeated_codes(
                black_box(&empty_rounds),
                3,
            ))
        })
    });

    // 单轮次
    let single_round = build_repeated_rounds(1, 1);
    group.bench_function("single_round", |b| {
        b.iter(|| {
            black_box(loop_detector::has_any_repeated_codes(
                black_box(&single_round),
                3,
            ))
        })
    });

    // truncate_text 边界
    group.bench_function("truncate_empty", |b| {
        b.iter(|| black_box(loop_detector::truncate_text(black_box(""), 100)))
    });

    let long_text = "x".repeat(10000);
    group.bench_function("truncate_long", |b| {
        b.iter(|| black_box(loop_detector::truncate_text(black_box(&long_text), 100)))
    });

    // should_detect_loop 边界
    group.bench_function("should_detect_disabled", |b| {
        b.iter(|| {
            black_box(loop_detector::should_detect_loop(
                black_box(0),
                black_box(100),
            ))
        })
    });

    // format_repeated_summary 边界
    let empty_sigs: Vec<(String, usize)> = vec![];
    let empty_files: Vec<(String, usize)> = vec![];
    group.bench_function("format_empty_summary", |b| {
        b.iter(|| {
            black_box(loop_detector::format_repeated_summary(
                black_box(&empty_sigs),
                black_box(&empty_files),
            ))
        })
    });

    group.finish();
}

/// 配置基准测试参数
fn configure_criterion() -> Criterion {
    Criterion::default()
        .sample_size(50)
        .measurement_time(std::time::Duration::from_secs(5))
        .warm_up_time(std::time::Duration::from_secs(2))
        .nresamples(50_000)
        .noise_threshold(0.05)
        .output_directory(std::path::Path::new("target/criterion/loop_detector"))
}

criterion_group! {
    name = loop_detector_benches;
    config = configure_criterion();
    targets =
        bench_make_error_signature,
        bench_has_any_repeated,
        bench_collect_repeated,
        bench_is_looping,
        bench_edge_cases,
}

criterion_main!(loop_detector_benches);
