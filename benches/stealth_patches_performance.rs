#![allow(clippy::useless_vec)]

//! stealth_patches 性能基准测试
//!
//! 测试目标:
//! 1. bootstrap_construction - 构建反检测 bootstrap 脚本
//! 2. patch_validation - 验证脚本完整性
//! 3. patch_constants - 补丁常量访问与内容检查
//! 4. utility_functions - needs_stealth_patches/patch_names
//! 5. edge_cases - 边界场景 (空/部分/超长/Unicode)

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use forge::stealth_patches::*;

// ============================================================================
//  基准测试 1: bootstrap_construction
// ============================================================================

fn bench_bootstrap_construction(c: &mut Criterion) {
    let mut group = c.benchmark_group("bootstrap_construction");

    // 单次构建
    group.bench_function("build_single", |b| {
        b.iter(|| {
            let script = build_bootstrap_script();
            black_box(script);
        })
    });

    // 验证构建结果长度
    group.bench_function("build_and_length", |b| {
        b.iter(|| {
            let script = build_bootstrap_script();
            let len = script.len();
            black_box(len);
        })
    });

    // 重复构建 10 次 (模拟多次标签页注入)
    group.bench_function("build_10x", |b| {
        b.iter(|| {
            let mut total = 0usize;
            for _ in 0..10 {
                let script = build_bootstrap_script();
                total += script.len();
            }
            black_box(total);
        })
    });

    // 重复构建 100 次 (模拟批量初始化)
    group.bench_function("build_100x", |b| {
        b.iter(|| {
            let mut total = 0usize;
            for _ in 0..100 {
                let script = build_bootstrap_script();
                total += script.len();
            }
            black_box(total);
        })
    });

    // 构建并验证 (组合操作)
    group.bench_function("build_and_validate", |b| {
        b.iter(|| {
            let script = build_bootstrap_script();
            let valid = validate_bootstrap_script(&script);
            black_box((script.len(), valid));
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 2: patch_validation
// ============================================================================

fn bench_patch_validation(c: &mut Criterion) {
    let mut group = c.benchmark_group("patch_validation");

    let valid_script = build_bootstrap_script();

    // 验证完整脚本
    group.bench_function("validate_full", |b| {
        b.iter(|| {
            let result = validate_bootstrap_script(black_box(&valid_script));
            black_box(result);
        })
    });

    // 验证空字符串
    group.bench_function("validate_empty", |b| {
        b.iter(|| {
            let result = validate_bootstrap_script(black_box(""));
            black_box(result);
        })
    });

    // 验证部分脚本 (只有 2 个补丁关键字)
    let partial_script = "navigator, 'webdriver' window.chrome";
    group.bench_function("validate_partial", |b| {
        b.iter(|| {
            let result = validate_bootstrap_script(black_box(partial_script));
            black_box(result);
        })
    });

    // 验证接近完整但缺少一个的脚本
    let almost_complete: String = valid_script.replace("outerWidth", "");
    group.bench_function("validate_missing_one", |b| {
        b.iter(|| {
            let result = validate_bootstrap_script(black_box(&almost_complete));
            black_box(result);
        })
    });

    // 批量验证 100 次
    group.bench_function("validate_batch_100", |b| {
        b.iter(|| {
            let mut count = 0u32;
            for _ in 0..100 {
                if validate_bootstrap_script(&valid_script) {
                    count += 1;
                }
            }
            black_box(count);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 3: patch_constants
// ============================================================================

fn bench_patch_constants(c: &mut Criterion) {
    let mut group = c.benchmark_group("patch_constants");

    // 访问单个常量并检查长度
    group.bench_function("webdriver_patch_len", |b| {
        b.iter(|| {
            let len = WEBDRIVER_PATCH.len();
            black_box(len);
        })
    });

    group.bench_function("chrome_object_patch_len", |b| {
        b.iter(|| {
            let len = CHROME_OBJECT_PATCH.len();
            black_box(len);
        })
    });

    group.bench_function("plugins_patch_len", |b| {
        b.iter(|| {
            let len = PLUGINS_PATCH.len();
            black_box(len);
        })
    });

    group.bench_function("permissions_patch_len", |b| {
        b.iter(|| {
            let len = PERMISSIONS_PATCH.len();
            black_box(len);
        })
    });

    group.bench_function("webrtc_patch_len", |b| {
        b.iter(|| {
            let len = WEBRTC_PATCH.len();
            black_box(len);
        })
    });

    group.bench_function("screen_patch_len", |b| {
        b.iter(|| {
            let len = SCREEN_PATCH.len();
            black_box(len);
        })
    });

    // 所有常量长度总和
    group.bench_function("all_patches_total_len", |b| {
        b.iter(|| {
            let total = WEBDRIVER_PATCH.len()
                + CHROME_OBJECT_PATCH.len()
                + PLUGINS_PATCH.len()
                + PERMISSIONS_PATCH.len()
                + WEBRTC_PATCH.len()
                + SCREEN_PATCH.len();
            black_box(total);
        })
    });

    // 内容验证 (contains 检查)
    group.bench_function("content_contains_check", |b| {
        b.iter(|| {
            let has_navigator = WEBDRIVER_PATCH.contains("navigator");
            let has_chrome = CHROME_OBJECT_PATCH.contains("window.chrome");
            let has_plugins = PLUGINS_PATCH.contains("Chrome PDF Plugin");
            let has_permissions = PERMISSIONS_PATCH.contains("permissions.query");
            let has_rtc = WEBRTC_PATCH.contains("RTCPeerConnection");
            let has_screen = SCREEN_PATCH.contains("outerWidth");
            black_box((
                has_navigator,
                has_chrome,
                has_plugins,
                has_permissions,
                has_rtc,
                has_screen,
            ));
        })
    });

    // IIFE 模式验证
    group.bench_function("iife_pattern_check", |b| {
        b.iter(|| {
            let patches = [
                WEBDRIVER_PATCH,
                CHROME_OBJECT_PATCH,
                PLUGINS_PATCH,
                PERMISSIONS_PATCH,
                WEBRTC_PATCH,
                SCREEN_PATCH,
            ];
            let count = patches
                .iter()
                .filter(|p| p.contains("(() => {") && p.contains("})();"))
                .count();
            black_box(count);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 4: utility_functions
// ============================================================================

fn bench_utility_functions(c: &mut Criterion) {
    let mut group = c.benchmark_group("utility_functions");

    // needs_stealth_patches
    group.bench_function("needs_patches_true", |b| {
        b.iter(|| {
            let result = needs_stealth_patches(black_box(true));
            black_box(result);
        })
    });

    group.bench_function("needs_patches_false", |b| {
        b.iter(|| {
            let result = needs_stealth_patches(black_box(false));
            black_box(result);
        })
    });

    group.bench_function("needs_patches_batch", |b| {
        b.iter(|| {
            let mut count = 0u32;
            for i in 0..100u32 {
                if needs_stealth_patches(i % 2 == 0) {
                    count += 1;
                }
            }
            black_box(count);
        })
    });

    // patch_names
    group.bench_function("patch_names_single", |b| {
        b.iter(|| {
            let names = patch_names();
            black_box(names);
        })
    });

    group.bench_function("patch_names_count", |b| {
        b.iter(|| {
            let names = patch_names();
            let count = names.len();
            black_box(count);
        })
    });

    group.bench_function("patch_names_contains_all", |b| {
        b.iter(|| {
            let names = patch_names();
            let has_webdriver = names.contains(&"webdriver");
            let has_chrome = names.contains(&"chrome_object");
            let has_plugins = names.contains(&"plugins");
            let has_permissions = names.contains(&"permissions");
            let has_webrtc = names.contains(&"webrtc");
            let has_screen = names.contains(&"screen");
            black_box((
                has_webdriver,
                has_chrome,
                has_plugins,
                has_permissions,
                has_webrtc,
                has_screen,
            ))
        })
    });

    // patch_names 批量调用
    group.bench_function("patch_names_batch_100", |b| {
        b.iter(|| {
            let mut total = 0usize;
            for _ in 0..100 {
                let names = patch_names();
                total += names.len();
            }
            black_box(total);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 5: edge_cases
// ============================================================================

fn bench_edge_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("stealth_patches_edge_cases");

    // 空字符串验证
    group.bench_function("validate_empty_string", |b| {
        b.iter(|| {
            let result = validate_bootstrap_script("");
            black_box(result);
        })
    });

    // 单字符字符串
    group.bench_function("validate_single_char", |b| {
        b.iter(|| {
            let result = validate_bootstrap_script("x");
            black_box(result);
        })
    });

    // 只有空格的字符串
    group.bench_function("validate_whitespace_only", |b| {
        b.iter(|| {
            let result = validate_bootstrap_script("     \n\n\n\t\t  ");
            black_box(result);
        })
    });

    // 超长字符串 (无补丁内容, 10KB)
    let long_random: String = "abcdef ".repeat(1280);
    group.bench_function("validate_long_no_patches", |b| {
        b.iter(|| {
            let result = validate_bootstrap_script(&long_random);
            black_box(result);
        })
    });

    // 超长字符串 (包含补丁内容, 10KB + 补丁)
    let long_with_patches = format!(
        "{}\n{}\n{}\n{}\n{}\n{}\n{}",
        long_random,
        "navigator, 'webdriver'",
        "window.chrome",
        "navigator, 'plugins'",
        "permissions",
        "RTCPeerConnection",
        "outerWidth"
    );
    group.bench_function("validate_long_with_patches", |b| {
        b.iter(|| {
            let result = validate_bootstrap_script(&long_with_patches);
            black_box(result);
        })
    });

    // Unicode 内容验证
    let unicode_script =
        "navigator, 'webdriver'\nwindow.chrome\nnavigator, 'plugins'\npermissions\nRTCPeerConnection\nouterWidth\n测试中文内容"
            .to_string();
    group.bench_function("validate_unicode_content", |b| {
        b.iter(|| {
            let result = validate_bootstrap_script(&unicode_script);
            black_box(result);
        })
    });

    // 混合大小写验证 (不应通过, 因为 contains 区分大小写)
    let mixed_case =
        "NAVIGATOR, 'WEBDRIVER'\nWINDOW.CHROME\nNAVIGATOR, 'PLUGINS'\nPERMISSIONS\nRTCPEERCONNECTION\nOUTERWIDTH"
            .to_string();
    group.bench_function("validate_mixed_case", |b| {
        b.iter(|| {
            let result = validate_bootstrap_script(&mixed_case);
            black_box(result);
        })
    });

    // 构建脚本 + 验证 + patch_names (完整流程)
    group.bench_function("full_workflow", |b| {
        b.iter(|| {
            let script = build_bootstrap_script();
            let valid = validate_bootstrap_script(&script);
            let names = patch_names();
            let needed = needs_stealth_patches(true);
            black_box((script.len(), valid, names.len(), needed));
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
        .output_directory(std::path::Path::new("target/criterion/stealth_patches"))
}

criterion_group! {
    name = stealth_patches_benches;
    config = configure_criterion();
    targets = bench_bootstrap_construction,
        bench_patch_validation,
        bench_patch_constants,
        bench_utility_functions,
        bench_edge_cases,
}

criterion_main!(stealth_patches_benches);
