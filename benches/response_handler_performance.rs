#![allow(clippy::useless_vec)]

//! response_handler 性能基准测试
//!
//! 测试目标:
//! 1. task_context_construction - TaskContext 创建和 builder 链
//! 2. handler_result_construction - HandlerResult 各种构造方式
//! 3. handler_chain_construction - HandlerChain 创建和添加处理器
//! 4. handler_chain_execute - 处理器链执行 (含 CodeExtractor)
//! 5. edge_cases - 边界场景 (空链/大元数据/handler_names)

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use forge::response_handler::*;

// ============================================================================
//  基准测试 1: task_context_construction
// ============================================================================

fn bench_task_context_construction(c: &mut Criterion) {
    c.bench_function("task_context_new", |b| {
        b.iter(|| {
            let ctx = TaskContext::new(
                black_box("develop"),
                black_box("task1"),
                black_box("/workspace/project"),
            );
            black_box(ctx);
        })
    });

    c.bench_function("task_context_with_builders", |b| {
        b.iter(|| {
            let ctx = TaskContext::new(black_box("fix"), black_box("task2"), black_box("/ws"))
                .with_turn(5)
                .with_metadata("key1", "value1")
                .with_metadata("key2", "value2")
                .with_metadata("phase", "compile");
            black_box(ctx);
        })
    });

    // 大量元数据
    c.bench_function("task_context_large_metadata", |b| {
        b.iter(|| {
            let mut ctx = TaskContext::new("develop", "task", "/ws");
            for i in 0..50 {
                ctx = ctx.with_metadata(&format!("key{i}"), &format!("value{i}"));
            }
            black_box(ctx);
        })
    });
}

// ============================================================================
//  基准测试 2: handler_result_construction
// ============================================================================

fn bench_handler_result_construction(c: &mut Criterion) {
    c.bench_function("result_continue_chain", |b| {
        b.iter(|| {
            let r = HandlerResult::continue_chain();
            black_box(r);
        })
    });

    c.bench_function("result_stop_chain", |b| {
        b.iter(|| {
            let r = HandlerResult::stop_chain();
            black_box(r);
        })
    });

    c.bench_function("result_with_files", |b| {
        b.iter(|| {
            let r = HandlerResult::continue_chain().with_files(vec![
                "src/main.rs".to_string(),
                "src/lib.rs".to_string(),
                "Cargo.toml".to_string(),
            ]);
            black_box(r);
        })
    });

    c.bench_function("result_with_message", |b| {
        b.iter(|| {
            let r = HandlerResult::continue_chain().with_message("处理完成");
            black_box(r);
        })
    });

    c.bench_function("result_default", |b| {
        b.iter(|| {
            let r = HandlerResult::default();
            black_box(r);
        })
    });
}

// ============================================================================
//  基准测试 3: handler_chain_construction
// ============================================================================

fn bench_handler_chain_construction(c: &mut Criterion) {
    c.bench_function("chain_new_empty", |b| {
        b.iter(|| {
            let chain = HandlerChain::new();
            black_box(chain);
        })
    });

    c.bench_function("chain_add_3_handlers", |b| {
        b.iter(|| {
            let mut chain = HandlerChain::new();
            chain.add(Box::new(CodeExtractorHandler::new()));
            chain.add(Box::new(TraceWriterHandler::new()));
            chain.add(Box::new(MemoryUpdaterHandler::new()));
            black_box(chain);
        })
    });

    c.bench_function("chain_default", |b| {
        b.iter(|| {
            let chain = HandlerChain::default();
            black_box(chain);
        })
    });

    c.bench_function("handler_names_3", |b| {
        let mut chain = HandlerChain::new();
        chain.add(Box::new(CodeExtractorHandler::new()));
        chain.add(Box::new(TraceWriterHandler::new()));
        chain.add(Box::new(MemoryUpdaterHandler::new()));
        b.iter(|| {
            let names = black_box(&chain).handler_names();
            black_box(names);
        })
    });
}

// ============================================================================
//  基准测试 4: handler_chain_execute
// ============================================================================

fn bench_handler_chain_execute(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let mut group = c.benchmark_group("handler_chain_execute");

    // 空链
    let empty_chain = HandlerChain::new();
    let ctx = TaskContext::new("develop", "task1", "/ws");
    group.bench_function("empty_chain", |b| {
        b.iter(|| {
            rt.block_on(async {
                let result = empty_chain
                    .execute(black_box("response text"), black_box(&ctx))
                    .await
                    .unwrap();
                black_box(result);
            })
        })
    });

    // 单处理器 (TraceWriter)
    let mut single_chain = HandlerChain::new();
    single_chain.add(Box::new(TraceWriterHandler::new()));
    group.bench_function("single_handler", |b| {
        b.iter(|| {
            rt.block_on(async {
                let result = single_chain
                    .execute(black_box("response text"), black_box(&ctx))
                    .await
                    .unwrap();
                black_box(result);
            })
        })
    });

    // 3 处理器链 + 含代码的回复
    let mut full_chain = HandlerChain::new();
    full_chain.add(Box::new(CodeExtractorHandler::new()));
    full_chain.add(Box::new(TraceWriterHandler::new()));
    full_chain.add(Box::new(MemoryUpdaterHandler::new()));
    let code_response = r#"Here is the code:
```file:src/main.rs
fn main() {
    println!("Hello, World!");
}
```
"#;
    group.bench_function("full_chain_with_code", |b| {
        b.iter(|| {
            rt.block_on(async {
                let result = full_chain
                    .execute(black_box(code_response), black_box(&ctx))
                    .await
                    .unwrap();
                black_box(result);
            })
        })
    });

    // 3 处理器链 + 无代码回复
    group.bench_function("full_chain_no_code", |b| {
        b.iter(|| {
            rt.block_on(async {
                let result = full_chain
                    .execute(
                        black_box("This is a plain text response without code."),
                        black_box(&ctx),
                    )
                    .await
                    .unwrap();
                black_box(result);
            })
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 5: edge_cases
// ============================================================================

fn bench_edge_cases(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("response_handler_edge_cases");

    // HandlerResult::default + with_files + with_message 组合
    group.bench_function("result_combined", |b| {
        b.iter(|| {
            let r = HandlerResult::default()
                .with_files(vec!["a.rs".to_string(), "b.rs".to_string()])
                .with_message("done");
            black_box(r);
        })
    });

    // 大量文件路径
    group.bench_function("result_many_files", |b| {
        b.iter(|| {
            let files: Vec<String> = (0..100).map(|i| format!("src/file_{i}.rs")).collect();
            let r = HandlerResult::continue_chain().with_files(files);
            black_box(r);
        })
    });

    // TaskContext 克隆
    let ctx = TaskContext::new("develop", "task1", "/ws")
        .with_turn(3)
        .with_metadata("k", "v");
    group.bench_function("task_context_clone", |b| {
        b.iter(|| {
            let cloned = black_box(&ctx).clone();
            black_box(cloned);
        })
    });

    // CodeExtractorHandler 多次调用
    let handler = CodeExtractorHandler::new();
    let ctx2 = TaskContext::new("develop", "task", "/ws");
    group.bench_function("code_extractor_repeated", |b| {
        b.iter(|| {
            rt.block_on(async {
                for _ in 0..10 {
                    let _ = handler
                        .handle(black_box("no code here"), black_box(&ctx2))
                        .await
                        .unwrap();
                }
            })
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
        .output_directory(std::path::Path::new("target/criterion/response_handler"))
}

criterion_group! {
    name = response_handler_benches;
    config = configure_criterion();
    targets = bench_task_context_construction,
        bench_handler_result_construction,
        bench_handler_chain_construction,
        bench_handler_chain_execute,
        bench_edge_cases,
}

criterion_main!(response_handler_benches);
