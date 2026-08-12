#![allow(clippy::useless_vec)]

//! trace_store 性能基准测试
//!
//! 测试目标:
//! 1. storage_backend_from_str - 解析存储后端类型字符串
//! 2. trace_entry_construction - TraceEntry 创建和 builder 链
//! 3. create_trace_store - 工厂函数创建存储后端
//! 4. trace_entry_serde - TraceEntry 序列化/反序列化
//! 5. edge_cases - 边界场景 (空/大对象/Display/Default)

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use forge::trace_store::*;
use std::path::PathBuf;
use std::str::FromStr;

// ============================================================================
//  基准测试 1: storage_backend_from_str
// ============================================================================

fn bench_storage_backend_from_str(c: &mut Criterion) {
    let inputs = vec![
        ("jsonl", "JSONL"),
        ("json", "JSON"),
        ("JSONL", "uppercase_jsonl"),
        ("Json", "mixed_case_json"),
        ("sqlite", "SQLite"),
        ("postgres", "Postgres"),
        ("postgresql", "PostgreSQL_alias"),
    ];

    c.bench_function("storage_backend_from_str", |b| {
        b.iter(|| {
            for (input, _) in black_box(&inputs) {
                let _ = StorageBackend::from_str(input);
            }
        })
    });

    // Display trait
    let backends = vec![
        StorageBackend::Jsonl,
        StorageBackend::Json,
        StorageBackend::Sqlite,
        StorageBackend::Postgres,
    ];

    c.bench_function("storage_backend_display", |b| {
        b.iter(|| {
            for be in black_box(&backends) {
                let s = format!("{}", be);
                black_box(s);
            }
        })
    });
}

// ============================================================================
//  基准测试 2: trace_entry_construction
// ============================================================================

fn bench_trace_entry_construction(c: &mut Criterion) {
    let mut group = c.benchmark_group("trace_entry_construction");

    // 基础创建
    group.bench_function("new_basic", |b| {
        b.iter(|| {
            let entry = TraceEntry::new(
                black_box("send_message"),
                black_box("develop"),
                black_box("task1"),
            );
            black_box(entry);
        })
    });

    // 带 builder 链
    group.bench_function("new_with_builders", |b| {
        b.iter(|| {
            let entry = TraceEntry::new(black_box("compile"), black_box("fix"), black_box("task2"))
                .with_success(false)
                .with_duration(5000)
                .with_detail("error: mismatched types");
            black_box(entry);
        })
    });

    // 批量创建
    for size in [10, 100, 500] {
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter(|| {
                let entries: Vec<TraceEntry> = (0..size)
                    .map(|i| {
                        TraceEntry::new("action", "phase", "task")
                            .with_duration(i * 10)
                            .with_success(i % 2 == 0)
                    })
                    .collect();
                black_box(entries);
            })
        });
    }

    group.finish();
}

// ============================================================================
//  基准测试 3: create_trace_store
// ============================================================================

fn bench_create_trace_store(c: &mut Criterion) {
    let mut group = c.benchmark_group("create_trace_store");

    let configs = vec![
        ("jsonl", StorageBackend::Jsonl),
        ("json", StorageBackend::Json),
        ("sqlite_fallback", StorageBackend::Sqlite),
        ("postgres_fallback", StorageBackend::Postgres),
    ];

    for (name, backend) in &configs {
        let config = StorageConfig {
            backend: *backend,
            path: PathBuf::from("/tmp/forge_bench_trace.jsonl"),
        };
        group.bench_function(*name, |b| {
            b.iter(|| {
                let store = create_trace_store(black_box(&config));
                black_box(store);
            })
        });
    }

    group.finish();
}

// ============================================================================
//  基准测试 4: trace_entry_serde
// ============================================================================

fn bench_trace_entry_serde(c: &mut Criterion) {
    let mut group = c.benchmark_group("trace_entry_serde");

    let entry_simple = TraceEntry::new("action", "phase", "task");
    let entry_full = TraceEntry::new("compile", "fix", "task1")
        .with_success(false)
        .with_duration(12345)
        .with_detail(r#"{"error": "mismatched types", "file": "src/main.rs", "line": 42}"#);

    // 序列化
    group.bench_function("serialize_simple", |b| {
        b.iter(|| {
            let json = serde_json::to_string(black_box(&entry_simple)).unwrap();
            black_box(json);
        })
    });

    group.bench_function("serialize_full", |b| {
        b.iter(|| {
            let json = serde_json::to_string(black_box(&entry_full)).unwrap();
            black_box(json);
        })
    });

    // 反序列化
    let json_simple = serde_json::to_string(&entry_simple).unwrap();
    let json_full = serde_json::to_string(&entry_full).unwrap();

    group.bench_function("deserialize_simple", |b| {
        b.iter(|| {
            let entry: TraceEntry = serde_json::from_str(black_box(&json_simple)).unwrap();
            black_box(entry);
        })
    });

    group.bench_function("deserialize_full", |b| {
        b.iter(|| {
            let entry: TraceEntry = serde_json::from_str(black_box(&json_full)).unwrap();
            black_box(entry);
        })
    });

    // 批量序列化
    let entries: Vec<TraceEntry> = (0..100)
        .map(|i| {
            TraceEntry::new(&format!("action_{i}"), "phase", "task")
                .with_duration(i * 10)
                .with_success(i % 3 != 0)
        })
        .collect();

    group.bench_function("serialize_batch_100", |b| {
        b.iter(|| {
            let json = serde_json::to_string(black_box(&entries)).unwrap();
            black_box(json);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 5: edge_cases
// ============================================================================

fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("trace_store_edge_cases");

    // StorageConfig::default
    group.bench_function("storage_config_default", |b| {
        b.iter(|| {
            let config = StorageConfig::default();
            black_box(config);
        })
    });

    // StorageConfig 克隆
    let config = StorageConfig {
        backend: StorageBackend::Jsonl,
        path: PathBuf::from("/tmp/forge/trace.jsonl"),
    };
    group.bench_function("storage_config_clone", |b| {
        b.iter(|| {
            let cloned = black_box(&config).clone();
            black_box(cloned);
        })
    });

    // from_str 错误处理
    let invalid_inputs = vec!["redis", "mongodb", "", "invalid"];
    group.bench_function("from_str_invalid", |b| {
        b.iter(|| {
            for input in black_box(&invalid_inputs) {
                let _ = StorageBackend::from_str(input);
            }
        })
    });

    // TraceEntry 空详情
    let entry_empty = TraceEntry::new("", "", "");
    group.bench_function("trace_entry_empty_fields", |b| {
        b.iter(|| {
            let json = serde_json::to_string(black_box(&entry_empty)).unwrap();
            let parsed: TraceEntry = serde_json::from_str(&json).unwrap();
            black_box(parsed);
        })
    });

    // Display 全后端
    group.bench_function("display_all_backends", |b| {
        b.iter(|| {
            let backends = [
                StorageBackend::Jsonl,
                StorageBackend::Json,
                StorageBackend::Sqlite,
                StorageBackend::Postgres,
            ];
            for be in &backends {
                let _ = format!("{}", be);
            }
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
        .output_directory(std::path::Path::new("target/criterion/trace_store"))
}

criterion_group! {
    name = trace_store_benches;
    config = configure_criterion();
    targets = bench_storage_backend_from_str,
        bench_trace_entry_construction,
        bench_create_trace_store,
        bench_trace_entry_serde,
        bench_edge_cases,
}

criterion_main!(trace_store_benches);
