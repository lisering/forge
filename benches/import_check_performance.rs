#![allow(clippy::useless_vec)]

//! 导入检查大规模性能基准测试 (Session 133 + 137 + 138 + 139 + 141)
//!
//! 测试目标:
//! 1. verify_imports_large — 大规模代码导入检查性能 (10/100/500/1000 行)
//! 2. ensure_external_imports_large — 大规模外部 crate 导入检测性能
//! 3. glob_import_detection — 混合 glob 导入检测性能 (简单/混合/嵌套)
//! 4. trait_method_detection — trait 方法检测性能 (.read()/.send()/.try_read()/.zip()/.chain()/.enumerate()/.flat_map()/.peekable()/.skip()/.take()/.rev()/.step_by()/.cloned()/.copied()/.fuse() 等)
//! 5. edge_cases — 边界情况 (空/单行/全限定路径/已导入/Unicode)

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use forge::extract::{ensure_external_imports, ensure_std_imports, verify_imports};

/// 构建需要多种 std 导入的代码 (n 个使用未导入类型的函数)
fn build_missing_imports_code(n: usize) -> String {
    let mut code = String::new();
    for i in 0..n {
        code.push_str(&format!(
            "fn func_{}() -> HashMap<String, Arc<Mutex<Vec<i32>>>> {{ HashMap::new() }}\n",
            i
        ));
    }
    code
}

/// 构建需要多种外部 crate 导入的代码
fn build_missing_external_code(n: usize) -> String {
    let types = [
        ("Serialize", "#[derive(Serialize)]\nstruct S{}"),
        ("Regex", "fn f() -> Regex { Regex::new(\"\").unwrap() }"),
        ("DateTime", "fn f() -> DateTime<Utc> { Utc::now() }"),
        ("Client", "fn f() -> Client { Client::new() }"),
        ("Router", "fn f() -> Router { Router::new() }"),
        ("Tera", "fn f() -> Tera { Tera::new(\"x\").unwrap() }"),
        ("Pool", "fn f() -> Pool<Sqlite> { unimplemented!() }"),
        ("Message", "fn f() -> Message { unimplemented!() }"),
        ("Config", "fn f() -> Config { unimplemented() }"),
        ("Cmd", "fn f() -> Cmd { Cmd::new(\"GET\") }"),
        // Session 136+137 types
        ("Mailgun", "fn f() -> Mailgun { unimplemented!() }"),
        ("Charge", "fn f() -> Charge { unimplemented!() }"),
        (
            "PutObjectOutput",
            "fn f() -> PutObjectOutput { unimplemented!() }",
        ),
        ("SdkConfig", "fn f() -> SdkConfig { unimplemented!() }"),
        ("BasicClient", "fn f() -> BasicClient { unimplemented!() }"),
        ("AccessToken", "fn f() -> AccessToken { unimplemented!() }"),
        ("Pattern", "fn f() -> Pattern { unimplemented!() }"),
        ("Cookie", "fn f() -> Cookie { unimplemented!() }"),
        ("CookieJar", "fn f() -> CookieJar { unimplemented!() }"),
        // Session 138 types
        ("EnvError", "fn f() -> EnvError { unimplemented!() }"),
        ("AppBuilder", "fn f() -> AppBuilder { unimplemented!() }"),
        ("Device", "fn f() -> Device { unimplemented!() }"),
        ("Queue", "fn f() -> Queue { unimplemented!() }"),
        ("Surface", "fn f() -> Surface { unimplemented!() }"),
        // Session 139 types
        ("Builder", "fn f() -> Builder { Builder::new() }"),
        ("Target", "fn f(t: &Target) { t.set_level(); }"),
        ("Filter", "fn f() -> Filter { unimplemented!() }"),
        ("Watcher", "fn f(w: &Watcher) { w.watch(); }"),
        ("EventKind", "fn f() -> EventKind { unimplemented!() }"),
        ("Event", "fn f(e: &Event) { }"),
        (
            "ShadowBuilder",
            "fn f() -> ShadowBuilder { unimplemented!() }",
        ),
        // Session 141 types
        ("System", "fn f() -> System { System::new() }"),
        ("CpuCore", "fn f() -> CpuCore { unimplemented!() }"),
        ("Disk", "fn f() -> Disk { unimplemented!() }"),
        ("SerialPort", "fn f() -> SerialPort { unimplemented!() }"),
        // Session 142 types
        ("EnvLoader", "fn f() -> EnvLoader { unimplemented!() }"),
        ("FdLock", "fn f() -> FdLock { unimplemented!() }"),
        ("NixPath", "fn f() -> NixPath { unimplemented!() }"),
        ("Utf8PathBuf", "fn f() -> Utf8PathBuf { unimplemented!() }"),
        // Session 144 types
        ("Enigo", "fn f() -> Enigo { unimplemented!() }"),
        ("HMODULE", "fn f() -> HMODULE { unimplemented!() }"),
        ("CGContext", "fn f() -> CGContext { unimplemented!() }"),
        // Session 145 types
        ("ImageBuffer", "fn f() -> ImageBuffer { unimplemented!() }"),
        ("Rgba", "fn f() -> Rgba<u8> { unimplemented!() }"),
        ("Drawing", "fn f() -> Drawing { unimplemented!() }"),
        ("Font", "fn f() -> Font { unimplemented!() }"),
        (
            "ChartContext",
            "fn f() -> ChartContext { unimplemented!() }",
        ),
        // Session 146 types
        ("Line", "fn f() -> Line { unimplemented!() }"),
        ("Layout", "fn f() -> Layout { unimplemented!() }"),
        ("Block", "fn f() -> Block { unimplemented!() }"),
        ("Frame", "fn f(f: &mut Frame) { unimplemented!() }"),
        ("Terminal", "fn f() -> Terminal { unimplemented!() }"),
        ("Display", "fn f() -> Display { unimplemented!() }"),
        ("Surface", "fn f() -> Surface { unimplemented!() }"),
        ("VkHandle", "fn f() -> VkHandle { unimplemented!() }"),
        (
            "VulkanObject",
            "fn f() -> VulkanObject { unimplemented!() }",
        ),
        // Session 147 types
        ("App", "fn f(a: &mut App) { unimplemented!() }"),
        ("Frame", "fn f(f: &mut Frame) { unimplemented!() }"),
        ("Context", "fn f(ctx: &Context) { unimplemented!() }"),
        ("Ui", "fn f(ui: &mut Ui) { unimplemented!() }"),
        ("Application", "fn f() -> Application { unimplemented!() }"),
        ("Command", "fn f() -> Command { unimplemented!() }"),
        ("AppDelegate", "fn f(d: &AppDelegate) { unimplemented!() }"),
        (
            "ComponentHandle",
            "fn f() -> ComponentHandle { unimplemented!() }",
        ),
        ("Model", "fn f() -> Model { unimplemented!() }"),
    ];
    let mut code = String::new();
    for i in 0..n {
        let (_, snippet) = types[i % types.len()];
        code.push_str(snippet);
        code.push('\n');
    }
    code
}

/// 构建已有所有导入的代码 (幂等性测试)
fn build_complete_imports_code(n: usize) -> String {
    let imports = [
        "use std::collections::HashMap;",
        "use std::sync::{Arc, Mutex, RwLock};",
        "use serde::Serialize;",
        "use regex::Regex;",
        "use chrono::{DateTime, Utc};",
        "use reqwest::Client;",
        "use axum::Router;",
        "use tera::Tera;",
        "use sqlx::Pool;",
        "use lettre::Message;",
        "use config::Config;",
        "use redis::Cmd;",
        // Session 136+137 imports
        "use mailgun::Mailgun;",
        "use stripe::Charge;",
        "use aws_sdk_s3::PutObjectOutput;",
        "use aws_config::SdkConfig;",
        "use oauth2::BasicClient;",
        "use glob::Pattern;",
        "use cookie::Cookie;",
        // Session 138 imports
        "use dotenv::EnvError;",
        "use tauri::AppBuilder;",
        "use wgpu::Device;",
        // Session 139 imports
        "use env_logger::Builder;",
        "use notify::Watcher;",
        "use shadow_rs::ShadowBuilder;",
        // Session 141 imports
        "use sysinfo::System;",
        "use serialport::SerialPort;",
        // Session 142 imports
        "use dotenvy::EnvLoader;",
        "use fd_lock::FdLock;",
        "use nix::path::NixPath;",
        "use camino::Utf8PathBuf;",
        // Session 144 imports
        "use enigo::Enigo;",
        "use winapi::HMODULE;",
        "use core_graphics::CGContext;",
        // Session 145 imports
        "use image::ImageBuffer;",
        "use rusttype::Font;",
        "use plotters::chart::ChartContext;",
        // Session 146 imports
        "use ratatui::text::Line;",
        "use crossterm::execute;",
        "use tui::Frame;",
        "use glium::Display;",
        "use vulkano::VkHandle;",
        "use ndarray_npy::open_npz;",
        // Session 147 imports
        "use eframe::App;",
        "use egui::Context;",
        "use iced::Application;",
        "use druid::AppDelegate;",
        "use slint::ComponentHandle;",
    ];
    let mut code = String::new();
    for imp in &imports {
        code.push_str(imp);
        code.push('\n');
    }
    for i in 0..n {
        code.push_str(&format!("fn func_{}() -> i32 {{ {} }}\n", i, i));
    }
    code
}

/// 构建大量 glob 导入的代码
fn build_glob_heavy_code(n: usize) -> String {
    let mut code = String::new();
    code.push_str("use std::collections::*;\n");
    code.push_str("use std::sync::{*, atomic::*};\n");
    code.push_str("use std::{io::*, fmt::*};\n");
    code.push_str("use serde::{Serialize, *};\n");
    for i in 0..n {
        code.push_str(&format!(
            "fn func_{}() -> HashMap<String, AtomicBool> {{ HashMap::new() }}\n",
            i
        ));
    }
    code
}

/// 构建 trait 方法调用密集的代码
fn build_trait_method_heavy_code(n: usize) -> String {
    let methods = [
        ".lock()",
        ".read()",
        ".write()",
        ".try_read()",
        ".try_write()",
        ".send(42)",
        ".recv()",
        ".try_send(42)",
        ".gen_range(0..10)",
        ".par_iter()",
        // Session 136+137 Iterator trait methods
        ".map(|x| x)",
        ".filter(|x| true)",
        ".zip(other.iter())",
        ".chain(other.iter())",
        ".enumerate()",
        // Session 138 Iterator trait methods
        ".flat_map(|x| Some(x))",
        ".peekable()",
        ".skip(1)",
        // Session 139 Iterator trait methods
        ".take(2)",
        ".rev()",
        ".step_by(3)",
        // Session 141 Iterator trait methods
        ".cloned()",
        ".copied()",
        ".fuse()",
        // Session 142 Iterator trait methods
        ".flatten()",
        ".max(Ord)",
        ".min(Ord)",
        ".sum()",
        ".product()",
        // Session 144 Iterator trait methods
        ".any(|x| true)",
        ".all(|x| true)",
        ".find(|x| true)",
        ".count()",
        ".fold(0, |a, b| a + b)",
        ".for_each(|x| {})",
        // Session 145 Iterator trait methods
        ".scan(0, |acc, x| Some(x))",
        ".unzip()",
        ".cycle()",
        // Session 146 Iterator trait methods
        ".chunks(2)",
        ".windows(3)",
        ".rchunks(4)",
        // Session 147 Iterator trait methods
        ".first()",
        ".last()",
        ".nth(0)",
        ".next_back()",
        ".rposition(|x| true)",
        ".rfold(0, |a, b| a + b)",
        ".rfind(|x| true)",
    ];
    let mut code = String::new();
    for i in 0..n {
        let method = methods[i % methods.len()];
        if method.contains("other") {
            code.push_str(&format!(
                "fn func_{}(v: Vec<i32>, other: Vec<i32>) {{ let x = v.iter(){}; }}\n",
                i, method
            ));
        } else {
            code.push_str(&format!("fn func_{}() {{ let x = unit{}; }}\n", i, method));
        }
    }
    code
}

/// 构建 sqlx 宏密集的代码
fn build_sqlx_macro_heavy_code(n: usize) -> String {
    let mut code = String::new();
    for i in 0..n {
        match i % 3 {
            0 => code.push_str(&format!(
                "fn func_{}() {{ let r = query!(\"SELECT {} FROM t\"); }}\n",
                i, i
            )),
            1 => code.push_str(&format!(
                "fn func_{}() {{ let r = query_as!(User, \"SELECT * FROM t WHERE id = {}\"); }}\n",
                i, i
            )),
            _ => code.push_str(&format!(
                "fn func_{}() -> Pool<Sqlite> {{ unimplemented!() }}\n",
                i
            )),
        }
    }
    code
}

/// 1. verify_imports 大规模性能
fn verify_imports_large(c: &mut Criterion) {
    let mut group = c.benchmark_group("verify_imports_large");
    let sizes = [10, 100, 500, 1000];

    for &size in &sizes {
        let code = build_missing_imports_code(size);
        group.bench_with_input(
            BenchmarkId::new("missing_imports", size),
            &code,
            |b, code| {
                b.iter(|| {
                    let issues = verify_imports(black_box(code));
                    black_box(issues);
                });
            },
        );
    }

    for &size in &sizes {
        let code = build_complete_imports_code(size);
        group.bench_with_input(
            BenchmarkId::new("complete_imports", size),
            &code,
            |b, code| {
                b.iter(|| {
                    let issues = verify_imports(black_box(code));
                    black_box(issues);
                });
            },
        );
    }

    group.finish();
}

/// 2. ensure_external_imports 大规模性能
fn ensure_external_imports_large(c: &mut Criterion) {
    let mut group = c.benchmark_group("ensure_external_imports_large");
    let sizes = [10, 100, 500, 1000];

    for &size in &sizes {
        let code = build_missing_external_code(size);
        group.bench_with_input(
            BenchmarkId::new("missing_external", size),
            &code,
            |b, code| {
                b.iter(|| {
                    let result = ensure_external_imports(black_box(code));
                    black_box(result);
                });
            },
        );
    }

    for &size in &sizes {
        let code = build_complete_imports_code(size);
        group.bench_with_input(BenchmarkId::new("idempotent", size), &code, |b, code| {
            b.iter(|| {
                let result = ensure_external_imports(black_box(code));
                black_box(result);
            });
        });
    }

    group.finish();
}

/// 3. glob 导入检测性能
fn glob_import_detection(c: &mut Criterion) {
    let mut group = c.benchmark_group("glob_import_detection");
    let sizes = [10, 100, 500, 1000];

    for &size in &sizes {
        let code = build_glob_heavy_code(size);
        group.bench_with_input(BenchmarkId::new("glob_heavy", size), &code, |b, code| {
            b.iter(|| {
                let result = ensure_std_imports(black_box(code));
                black_box(result);
            });
        });
    }

    // 混合 glob + 外部导入
    for &size in &sizes {
        let mut code = String::new();
        code.push_str("use std::sync::{*, atomic::*};\n");
        code.push_str("use serde::{Serialize, *};\n");
        for i in 0..size {
            code.push_str(&format!(
                "#[derive(Serialize)]\nstruct S{} {{ x: AtomicBool }}\n",
                i
            ));
        }
        group.bench_with_input(
            BenchmarkId::new("mixed_glob_external", size),
            &code,
            |b, code| {
                b.iter(|| {
                    let result = ensure_external_imports(black_box(code));
                    black_box(result);
                });
            },
        );
    }

    group.finish();
}

/// 4. trait 方法检测性能
fn trait_method_detection(c: &mut Criterion) {
    let mut group = c.benchmark_group("trait_method_detection");
    let sizes = [10, 100, 500, 1000];

    for &size in &sizes {
        let code = build_trait_method_heavy_code(size);
        group.bench_with_input(BenchmarkId::new("trait_methods", size), &code, |b, code| {
            b.iter(|| {
                let issues = verify_imports(black_box(code));
                black_box(issues);
            });
        });
    }

    // sqlx 宏检测
    for &size in &sizes {
        let code = build_sqlx_macro_heavy_code(size);
        group.bench_with_input(BenchmarkId::new("sqlx_macros", size), &code, |b, code| {
            b.iter(|| {
                let issues = verify_imports(black_box(code));
                black_box(issues);
            });
        });
    }

    group.finish();
}

/// 5. 边界情况
fn edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_cases");

    // 空代码
    group.bench_function("empty", |b| {
        b.iter(|| {
            let issues = verify_imports(black_box(""));
            black_box(issues);
        });
    });

    // 单行代码
    group.bench_function("single_line", |b| {
        b.iter(|| {
            let issues = verify_imports(black_box("fn foo() -> i32 { 42 }"));
            black_box(issues);
        });
    });

    // 全限定路径 (不需要导入)
    group.bench_function("full_paths", |b| {
        b.iter(|| {
            let code = "fn foo() -> std::collections::HashMap<i32, i32> { std::collections::HashMap::new() }";
            let issues = verify_imports(black_box(code));
            black_box(issues);
        });
    });

    // 已全部导入
    group.bench_function("all_imported", |b| {
        b.iter(|| {
            let code = "use std::collections::HashMap;\nuse std::sync::{Arc, Mutex, RwLock};\nuse serde::Serialize;\nfn foo() -> HashMap<String, Arc<Mutex<i32>>> { HashMap::new() }";
            let issues = verify_imports(black_box(code));
            black_box(issues);
        });
    });

    // Unicode 标识符
    group.bench_function("unicode", |b| {
        b.iter(|| {
            let code = "fn 你好() -> HashMap<String, i32> { HashMap::new() }\nfn func() -> RwLock<i32> { RwLock::new(0) }";
            let issues = verify_imports(black_box(code));
            black_box(issues);
        });
    });

    // 超大代码 (5000 行)
    group.bench_function("very_large_5000", |b| {
        b.iter(|| {
            let code = build_missing_imports_code(5000);
            let issues = verify_imports(black_box(&code));
            black_box(issues);
        });
    });

    // 大量外部 crate 类型 (ensure_external_imports)
    group.bench_function("many_external_types", |b| {
        b.iter(|| {
            let code = build_missing_external_code(500);
            let result = ensure_external_imports(black_box(&code));
            black_box(result);
        });
    });

    // Session 137: mailgun/stripe/aws-sdk/oauth2/glob/cookie 类型检测
    group.bench_function("s136_s137_external_types", |b| {
        b.iter(|| {
            let code =
                "fn foo() -> (Mailgun, Charge, PutObjectOutput, SdkConfig) { unimplemented!() }\n\
                 fn bar() -> (BasicClient, Pattern, Cookie) { unimplemented!() }\n\
                 fn baz() -> (Recipient, Customer, GetObjectOutput, BehaviorVersion) { unimplemented!() }\n\
                 fn qux() -> (AccessToken, GlobBuilder, CookieJar) { unimplemented!() }";
            let result = ensure_external_imports(black_box(code));
            black_box(result);
        });
    });

    // Session 137: .zip()/.chain()/.enumerate() trait 方法检测
    group.bench_function("s137_iterator_methods", |b| {
        b.iter(|| {
            let code = "fn foo(v: Vec<i32>) { v.iter().map(|x| x).filter(|&x| x > 0).zip(v.iter()).chain(v.iter()).enumerate(); }";
            let issues = verify_imports(black_box(code));
            black_box(issues);
        });
    });

    // Session 138: dotenv/tauri/wgpu 类型检测
    group.bench_function("s138_external_types", |b| {
        b.iter(|| {
            let code =
                "fn foo() -> (EnvError, AppBuilder, Device) { unimplemented!() }\n\
                 fn bar() -> (AppHandle, Queue, Surface) { unimplemented!() }\n\
                 fn baz() -> (Manager, Invoke, SurfaceConfiguration, ShaderModule) { unimplemented!() }";
            let result = ensure_external_imports(black_box(code));
            black_box(result);
        });
    });

    // Session 138: .flat_map()/.peekable()/.skip() trait 方法检测
    group.bench_function("s138_iterator_methods", |b| {
        b.iter(|| {
            let code = "fn foo(v: Vec<i32>) { v.iter().flat_map(|x| Some(x)).peekable().skip(1); }";
            let issues = verify_imports(black_box(code));
            black_box(issues);
        });
    });

    // Session 139: env_logger/notify/shadow-rs 类型检测
    group.bench_function("s139_external_types", |b| {
        b.iter(|| {
            let code = "fn foo() -> (Builder, Watcher, ShadowBuilder) { unimplemented!() }\n\
                 fn bar() -> (Target, EventKind) { unimplemented!() }\n\
                 fn baz() -> (Filter, Event) { unimplemented!() }";
            let result = ensure_external_imports(black_box(code));
            black_box(result);
        });
    });

    // Session 139: .take()/.rev()/.step_by() trait 方法检测
    group.bench_function("s139_iterator_methods", |b| {
        b.iter(|| {
            let code = "fn foo(v: Vec<i32>) { v.iter().take(3).rev().step_by(2); }";
            let issues = verify_imports(black_box(code));
            black_box(issues);
        });
    });

    // Session 141: sysinfo/serialport 类型检测
    group.bench_function("s141_external_types", |b| {
        b.iter(|| {
            let code = "fn foo() -> (System, CpuCore, Disk) { unimplemented!() }\n\
                 fn bar() -> SerialPort { unimplemented!() }";
            let result = ensure_external_imports(black_box(code));
            black_box(result);
        });
    });

    // Session 141: .cloned()/.copied()/.fuse() trait 方法检测
    group.bench_function("s141_iterator_methods", |b| {
        b.iter(|| {
            let code = "fn foo(v: Vec<&i32>) { v.iter().cloned().copied().fuse(); }";
            let issues = verify_imports(black_box(code));
            black_box(issues);
        });
    });

    // Session 142: dotenvy/fd-lock/nix/camino 类型检测
    group.bench_function("s142_external_types", |b| {
        b.iter(|| {
            let code = "fn foo() -> (EnvLoader, FdLock, NixPath) { unimplemented!() }\n\
                 fn bar() -> (Errno, Utf8PathBuf, Utf8Path) { unimplemented!() }";
            let result = ensure_external_imports(black_box(code));
            black_box(result);
        });
    });

    // Session 142: .flatten()/.max()/.min()/.sum()/.product() trait 方法检测
    group.bench_function("s142_iterator_methods", |b| {
        b.iter(|| {
            let code = "fn foo(v: Vec<i32>) { v.iter().flatten().max(Ord).min(Ord); let s = v.iter().sum(); let p = v.iter().product(); }";
            let issues = verify_imports(black_box(code));
            black_box(issues);
        });
    });

    // Session 145: image/imageproc/rusttype/plotters 类型检测
    group.bench_function("s145_external_types", |b| {
        b.iter(|| {
            let code = "fn foo() -> (ImageBuffer, Rgba<u8>, Drawing) { unimplemented!() }\n\
                 fn bar() -> (Font, PositionedGlyph, ChartContext) { unimplemented!() }";
            let result = ensure_external_imports(black_box(code));
            black_box(result);
        });
    });

    // Session 145: .scan()/.unzip()/.cycle() trait 方法检测
    group.bench_function("s145_iterator_methods", |b| {
        b.iter(|| {
            let code =
                "fn foo(v: Vec<i32>) { v.iter().scan(0, |acc, &x| Some(x)).unzip().cycle(); }";
            let issues = verify_imports(black_box(code));
            black_box(issues);
        });
    });

    // Session 146: ratatui/crossterm/tui/glium/vulkano/ndarray-npy 类型检测
    group.bench_function("s146_external_types", |b| {
        b.iter(|| {
            let code = "fn foo() -> (Line, Layout, Block) { unimplemented!() }\n\
                 fn bar(f: &mut Frame) { unimplemented!() }\n\
                 fn baz() -> (Display, Surface, VkHandle, VulkanObject) { unimplemented!() }\n\
                 fn qux() { open_npz(\"f.npz\"); save_npz(\"f.npz\"); }";
            let result = ensure_external_imports(black_box(code));
            black_box(result);
        });
    });

    // Session 146: .chunks()/.windows()/.rchunks() trait 方法检测
    group.bench_function("s146_iterator_methods", |b| {
        b.iter(|| {
            let code = "fn foo(v: Vec<i32>) { v.chunks(2); v.windows(3); v.rchunks(4); }";
            let issues = verify_imports(black_box(code));
            black_box(issues);
        });
    });

    // Session 147: eframe/egui/iced/druid/slint 类型检测
    group.bench_function("s147_external_types", |b| {
        b.iter(|| {
            let code = "fn foo(a: &mut App, ctx: &Context) { unimplemented!() }\n\
                 fn bar() -> (Application, Command) { unimplemented!() }\n\
                 fn baz(d: &AppDelegate) { unimplemented!() }\n\
                 fn qux() -> ComponentHandle { unimplemented!() }";
            let result = ensure_external_imports(black_box(code));
            black_box(result);
        });
    });

    // Session 147: .first()/.last()/.nth()/.next_back()/.rposition()/.rfold()/.rfind() trait 方法检测
    group.bench_function("s147_iterator_methods", |b| {
        b.iter(|| {
            let code = "fn foo(v: Vec<i32>) { v.first(); v.last(); v.nth(0); v.next_back(); v.rposition(|x| true); v.rfold(0, |a, b| a + b); v.rfind(|x| true); }";
            let issues = verify_imports(black_box(code));
            black_box(issues);
        });
    });

    // Session 148: num_cpus/once_cell/flate2/walkdir/tempfile/indicatif 类型检测
    group.bench_function("s148_external_types", |b| {
        b.iter(|| {
            let code = "fn foo() -> (WalkDir, TempDir, ProgressBar) { unimplemented!() }\n\
                 fn bar() -> Lazy<i32> { unimplemented!() }\n\
                 fn baz() -> usize { num_cpus() }\n\
                 fn qux() -> GzEncoder { unimplemented!() }";
            let result = ensure_external_imports(black_box(code));
            black_box(result);
        });
    });

    // Session 148: .iter_mut()/.split()/.lines()/.chars()/.bytes() trait 方法检测
    group.bench_function("s148_iterator_methods", |b| {
        b.iter(|| {
            let code = "fn foo(s: &str) { s.split(','); s.lines(); s.chars(); s.bytes(); }\nfn bar(v: &mut Vec<i32>) { v.iter_mut(); }";
            let issues = verify_imports(black_box(code));
            black_box(issues);
        });
    });

    // Session 149: .iter()/.try_fold()/.try_for_each()/.is_sorted()/.collect_into() trait 方法检测
    group.bench_function("s149_iterator_methods", |b| {
        b.iter(|| {
            let code = "fn foo(v: Vec<i32>) { v.iter(); v.try_fold(0, |a, b| a + b); v.try_for_each(|x| Ok(())); v.is_sorted(); v.collect_into(&mut Vec::new()); }";
            let issues = verify_imports(black_box(code));
            black_box(issues);
        });
    });

    // 嵌套 glob 导入
    group.bench_function("nested_glob", |b| {
        b.iter(|| {
            let code = "use std::{sync::{*, atomic::*}, io::*, fmt::{Display, Debug}};\nfn foo() -> AtomicBool { AtomicBool::new(true) }\nfn bar() -> BufReader<File> { unimplemented!() }";
            let result = ensure_std_imports(black_box(code));
            black_box(result);
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    verify_imports_large,
    ensure_external_imports_large,
    glob_import_detection,
    trait_method_detection,
    edge_cases,
);
criterion_main!(benches);
