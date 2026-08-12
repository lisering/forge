#![allow(clippy::useless_vec)]

//! browser_launcher 性能基准测试
//!
//! 测试目标:
//! 1. browser_detection - 浏览器路径检测 (detect_browser_paths/find_browser/browser_exists)
//! 2. port_management - 端口管理 (find_available_port_sync/is_port_available_sync)
//! 3. launch_args_building - 启动参数构建 (build_launch_args)
//! 4. browser_name_detection - 浏览器名称提取 (browser_name)
//! 5. edge_cases - 边界场景 (空路径/不存在路径/默认目录/BrowserLauncher::new)

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use forge::browser_launcher::*;
use std::path::{Path, PathBuf};

// ============================================================================
//  基准测试 1: browser_detection
// ============================================================================

fn bench_browser_detection(c: &mut Criterion) {
    let mut group = c.benchmark_group("browser_detection");

    // detect_browser_paths
    group.bench_function("detect_browser_paths", |b| {
        b.iter(|| {
            let paths = detect_browser_paths();
            black_box(paths);
        })
    });

    // find_browser (在系统路径中查找)
    let paths = detect_browser_paths();
    group.bench_function("find_browser", |b| {
        b.iter(|| {
            let browser = find_browser(black_box(&paths));
            black_box(browser);
        })
    });

    // browser_exists (存在)
    if let Some(ref browser_path) = find_browser(&paths) {
        group.bench_function("browser_exists_true", |b| {
            b.iter(|| {
                let exists = browser_exists(black_box(browser_path));
                black_box(exists);
            })
        });
    }

    // browser_exists (不存在)
    let nonexistent = Path::new("/nonexistent/browser/path");
    group.bench_function("browser_exists_false", |b| {
        b.iter(|| {
            let exists = browser_exists(black_box(nonexistent));
            black_box(exists);
        })
    });

    // browser_from_env (未设置)
    group.bench_function("browser_from_env_unset", |b| {
        b.iter(|| {
            let result = browser_from_env();
            black_box(result);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 2: port_management
// ============================================================================

fn bench_port_management(c: &mut Criterion) {
    let mut group = c.benchmark_group("port_management");

    // is_port_available_sync (空闲端口)
    group.bench_function("is_port_available_free", |b| {
        b.iter(|| {
            let available = is_port_available_sync(black_box(1));
            black_box(available);
        })
    });

    // is_port_available_sync (被占用端口 — 当前调试端口)
    // 使用一个很可能不存在的端口测试 "可用" 路径
    let test_ports = vec![1u16, 8080, 9222, 9999, 65535];
    for &port in &test_ports {
        group.bench_function(format!("is_port_available_{}", port), |b| {
            b.iter(|| {
                let available = is_port_available_sync(black_box(port));
                black_box(available);
            })
        });
    }

    // find_available_port_sync 不同起始端口
    let starts = vec![9222u16, 9300, 10000];
    for &start in &starts {
        group.bench_with_input(
            BenchmarkId::new("find_available_port", start),
            &start,
            |b, &start| {
                b.iter(|| {
                    let port = find_available_port_sync(black_box(start), black_box(100)).unwrap();
                    black_box(port);
                })
            },
        );
    }

    // find_available_port_sync 少量尝试
    group.bench_function("find_available_port_1_try", |b| {
        b.iter(|| {
            let port = find_available_port_sync(black_box(1), black_box(1)).unwrap();
            black_box(port);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 3: launch_args_building
// ============================================================================

fn bench_launch_args_building(c: &mut Criterion) {
    let mut group = c.benchmark_group("launch_args_building");

    // 基本参数 (无自定义目录)
    group.bench_function("basic_no_dir", |b| {
        b.iter(|| {
            let args = build_launch_args(black_box(9222), None, &[]);
            black_box(args);
        })
    });

    // 带自定义目录
    group.bench_function("with_custom_dir", |b| {
        b.iter(|| {
            let args = build_launch_args(
                black_box(9222),
                Some(PathBuf::from("/tmp/forge-chrome")),
                &[],
            );
            black_box(args);
        })
    });

    // 带额外参数
    let extra_args: Vec<String> = vec!["--headless".to_string(), "--no-sandbox".to_string()];
    group.bench_function("with_extra_args", |b| {
        b.iter(|| {
            let args = build_launch_args(black_box(9222), None, black_box(&extra_args));
            black_box(args);
        })
    });

    // 不同端口
    let ports = vec![9222u16, 9300, 10000, 65535];
    for &port in &ports {
        group.bench_function(format!("port_{}", port), |b| {
            b.iter(|| {
                let args = build_launch_args(black_box(port), None, &[]);
                black_box(args);
            })
        });
    }

    // 大量额外参数 (10个)
    let many_args: Vec<String> = (0..10).map(|i| format!("--arg{}", i)).collect();
    group.bench_function("with_10_extra_args", |b| {
        b.iter(|| {
            let args = build_launch_args(
                black_box(9222),
                Some(PathBuf::from("/tmp/forge-chrome")),
                black_box(&many_args),
            );
            black_box(args);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 4: browser_name_detection
// ============================================================================

fn bench_browser_name_detection(c: &mut Criterion) {
    let mut group = c.benchmark_group("browser_name_detection");

    let test_cases = vec![
        (
            "chrome",
            Path::new("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
        ),
        ("edge", Path::new("/usr/bin/microsoft-edge")),
        ("chromium", Path::new("/usr/bin/chromium")),
        ("unknown", Path::new("/usr/bin/firefox")),
        ("empty", Path::new("")),
    ];

    for (name, path) in &test_cases {
        group.bench_function(format!("browser_name_{}", name), |b| {
            b.iter(|| {
                let name = browser_name(black_box(path));
                black_box(name);
            })
        });
    }

    // 批量检测
    group.bench_function("batch_5_paths", |b| {
        b.iter(|| {
            for (_, path) in black_box(&test_cases) {
                let _ = browser_name(path);
            }
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 5: edge_cases
// ============================================================================

fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("browser_launcher_edge_cases");

    // default_user_data_dir
    group.bench_function("default_user_data_dir", |b| {
        b.iter(|| {
            let dir = default_user_data_dir();
            black_box(dir);
        })
    });

    // BrowserLauncher::new
    group.bench_function("launcher_new", |b| {
        b.iter(|| {
            let launcher = BrowserLauncher::new();
            black_box(launcher);
        })
    });

    // BrowserLauncher::default
    group.bench_function("launcher_default", |b| {
        b.iter(|| {
            let launcher = BrowserLauncher::default();
            black_box(launcher);
        })
    });

    // find_browser 空列表
    let empty_paths: Vec<PathBuf> = vec![];
    group.bench_function("find_browser_empty", |b| {
        b.iter(|| {
            let result = find_browser(black_box(&empty_paths));
            assert!(result.is_none());
            black_box(result);
        })
    });

    // find_browser 全不存在
    let bad_paths: Vec<PathBuf> = vec![
        PathBuf::from("/nonexistent/1"),
        PathBuf::from("/nonexistent/2"),
        PathBuf::from("/nonexistent/3"),
    ];
    group.bench_function("find_browser_all_nonexistent", |b| {
        b.iter(|| {
            let result = find_browser(black_box(&bad_paths));
            assert!(result.is_none());
            black_box(result);
        })
    });

    // build_launch_args 端口 0
    group.bench_function("build_launch_args_port_0", |b| {
        b.iter(|| {
            let args = build_launch_args(black_box(0), None, &[]);
            black_box(args);
        })
    });

    // build_launch_args 端口 max
    group.bench_function("build_launch_args_port_max", |b| {
        b.iter(|| {
            let args = build_launch_args(black_box(u16::MAX), None, &[]);
            black_box(args);
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
        .output_directory(std::path::Path::new("target/criterion/browser_launcher"))
}

criterion_group! {
    name = browser_launcher_benches;
    config = configure_criterion();
    targets = bench_browser_detection,
        bench_port_management,
        bench_launch_args_building,
        bench_browser_name_detection,
        bench_edge_cases,
}

criterion_main!(browser_launcher_benches);
