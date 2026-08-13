//! Import Report 集成测试 (Session 135)
//!
//! 验证 verify_imports_to_json / verify_imports_to_markdown / verify_imports_report
//! 在各种代码模式下的正确性, 包括:
//! - Session 135 新增外部 crate (warp/actix-web/sea-orm/diesel)
//! - Session 135 新增 trait 方法检测 (.collect() / .into_iter())
//! - 多文件聚合报告
//! - 空报告 / 无问题报告
//! - JSON / Markdown 双格式

use forge::extract::{
    ensure_external_imports, verify_imports, verify_imports_report, verify_imports_to_json,
    verify_imports_to_markdown,
};

// ===== Session 135: JSON 报告 — 新增外部 crate 测试 =====

#[test]
fn test_import_report_json_warp_missing() {
    let code = "fn foo() -> Filter { unimplemented!() }";
    let json = verify_imports_to_json(code);
    assert!(json.contains("Filter"), "JSON 报告应包含 Filter: {}", json);
    assert!(json.contains("warp"), "JSON 报告应包含 warp 模块: {}", json);
}

#[test]
fn test_import_report_json_actix_missing() {
    let code = "fn foo() -> HttpResponse { HttpResponse::Ok().finish() }";
    let json = verify_imports_to_json(code);
    assert!(
        json.contains("HttpResponse"),
        "JSON 报告应包含 HttpResponse: {}",
        json
    );
    assert!(
        json.contains("actix_web"),
        "JSON 报告应包含 actix_web 模块: {}",
        json
    );
}

#[test]
fn test_import_report_json_sea_orm_missing() {
    let code = "fn foo() -> EntityTrait { unimplemented!() }";
    let json = verify_imports_to_json(code);
    assert!(
        json.contains("EntityTrait"),
        "JSON 报告应包含 EntityTrait: {}",
        json
    );
    assert!(
        json.contains("sea_orm"),
        "JSON 报告应包含 sea_orm 模块: {}",
        json
    );
}

#[test]
fn test_import_report_json_diesel_missing() {
    let code = "fn foo() -> PgConnection { unimplemented!() }";
    let json = verify_imports_to_json(code);
    assert!(
        json.contains("PgConnection"),
        "JSON 报告应包含 PgConnection: {}",
        json
    );
    assert!(
        json.contains("diesel"),
        "JSON 报告应包含 diesel 模块: {}",
        json
    );
}

// ===== Session 135: JSON 报告 — trait 方法检测测试 =====

#[test]
fn test_import_report_json_collect_method() {
    let code = "fn foo(v: Vec<i32>) { let x: Vec<i32> = v.iter().collect(); }";
    let json = verify_imports_to_json(code);
    assert!(
        json.contains("FromIterator"),
        "JSON 报告应包含 FromIterator: {}",
        json
    );
    assert!(
        json.contains("std::iter"),
        "JSON 报告应包含 std::iter 模块: {}",
        json
    );
}

#[test]
fn test_import_report_json_into_iter_method() {
    let code = "fn foo(v: Vec<i32>) { for x in v.into_iter() { println!(\"{}\", x); } }";
    let json = verify_imports_to_json(code);
    assert!(
        json.contains("IntoIterator"),
        "JSON 报告应包含 IntoIterator: {}",
        json
    );
}

// ===== Session 135: Markdown 报告 — 新增外部 crate 测试 =====

#[test]
fn test_import_report_markdown_warp_missing() {
    let code = "fn foo() -> Filter { unimplemented!() }";
    let md = verify_imports_to_markdown(code);
    assert!(md.contains("Filter"), "Markdown 报告应包含 Filter: {}", md);
    assert!(md.contains("warp"), "Markdown 报告应包含 warp: {}", md);
}

#[test]
fn test_import_report_markdown_actix_missing() {
    let code = "fn foo() -> HttpResponse { HttpResponse::Ok().finish() }";
    let md = verify_imports_to_markdown(code);
    assert!(
        md.contains("HttpResponse"),
        "Markdown 报告应包含 HttpResponse: {}",
        md
    );
    assert!(
        md.contains("actix_web"),
        "Markdown 报告应包含 actix_web: {}",
        md
    );
}

#[test]
fn test_import_report_markdown_diesel_missing() {
    let code = "fn foo() -> PgConnection { unimplemented!() }";
    let md = verify_imports_to_markdown(code);
    assert!(
        md.contains("PgConnection"),
        "Markdown 报告应包含 PgConnection: {}",
        md
    );
    assert!(md.contains("diesel"), "Markdown 报告应包含 diesel: {}", md);
}

// ===== Session 135: Markdown 报告 — trait 方法检测测试 =====

#[test]
fn test_import_report_markdown_collect_method() {
    let code = "fn foo(v: Vec<i32>) { let x: Vec<i32> = v.iter().collect(); }";
    let md = verify_imports_to_markdown(code);
    assert!(
        md.contains("FromIterator"),
        "Markdown 报告应包含 FromIterator: {}",
        md
    );
}

#[test]
fn test_import_report_markdown_into_iter_method() {
    let code = "fn foo(v: Vec<i32>) { for x in v.into_iter() { println!(\"{}\", x); } }";
    let md = verify_imports_to_markdown(code);
    assert!(
        md.contains("IntoIterator"),
        "Markdown 报告应包含 IntoIterator: {}",
        md
    );
}

// ===== Session 135: ImportReport 结构体 — 综合测试 =====

#[test]
fn test_import_report_struct_multiple_s135_types() {
    let code = "fn foo() -> (Filter, HttpResponse, EntityTrait, PgConnection) { unimplemented!() }";
    let report = verify_imports_report(code);
    assert!(report.has_issues, "应有问题");
    assert!(report.total_issues >= 4, "应至少有 4 个问题: {:?}", report);

    let module_strings: Vec<&str> = report.modules_affected.iter().map(|s| s.as_str()).collect();
    assert!(
        module_strings.contains(&"warp"),
        "受影响模块应包含 warp: {:?}",
        report.modules_affected
    );
    assert!(
        module_strings.contains(&"actix_web"),
        "受影响模块应包含 actix_web: {:?}",
        report.modules_affected
    );
    assert!(
        module_strings.contains(&"sea_orm"),
        "受影响模块应包含 sea_orm: {:?}",
        report.modules_affected
    );
    assert!(
        module_strings.contains(&"diesel"),
        "受影响模块应包含 diesel: {:?}",
        report.modules_affected
    );
}

#[test]
fn test_import_report_struct_with_collect_and_into_iter() {
    let code = "fn foo(v: Vec<i32>) { let x: Vec<i32> = v.into_iter().collect(); }";
    let report = verify_imports_report(code);
    assert!(report.has_issues, "应有问题");
    assert!(
        report.issues.iter().any(|i| i.type_name == "FromIterator"),
        "应包含 FromIterator 问题: {:?}",
        report.issues
    );
    assert!(
        report.issues.iter().any(|i| i.type_name == "IntoIterator"),
        "应包含 IntoIterator 问题: {:?}",
        report.issues
    );
}

#[test]
fn test_import_report_no_issues_clean_code() {
    let code = "fn foo() -> i32 { 42 }";
    let report = verify_imports_report(code);
    assert!(!report.has_issues, "无问题代码不应有问题");
    assert_eq!(report.total_issues, 0, "问题数应为 0");
    assert!(report.issues.is_empty(), "问题列表应为空");
}

// ===== Session 135: ensure_external_imports 后无问题验证 =====

#[test]
fn test_import_report_after_ensure_external_no_s135_issues() {
    let code = "fn foo() -> (Filter, HttpResponse) { unimplemented!() }";
    let fixed = ensure_external_imports(code);
    let issues = verify_imports(&fixed);

    // ensure_external_imports 应修复所有 Session 135 外部 crate 导入
    let s135_issues: Vec<_> = issues
        .iter()
        .filter(|i| {
            i.module_path == "warp"
                || i.module_path == "actix_web"
                || i.module_path == "sea_orm"
                || i.module_path == "diesel"
        })
        .collect();

    assert!(
        s135_issues.is_empty(),
        "ensure_external_imports 后不应有 Session 135 外部 crate 导入问题: {:?}",
        s135_issues
    );
}

// ===== Session 135: JSON / Markdown 双格式一致性 =====

#[test]
fn test_import_report_json_markdown_consistency() {
    let code = "fn foo() -> (Filter, PgConnection) { unimplemented!() }\nfn bar(v: Vec<i32>) { v.iter().collect(); }";
    let json = verify_imports_to_json(code);
    let md = verify_imports_to_markdown(code);

    // 两种格式都应报告相同的问题
    for type_name in &["Filter", "PgConnection", "FromIterator"] {
        assert!(
            json.contains(type_name),
            "JSON 应包含 {}: {}",
            type_name,
            json
        );
        assert!(
            md.contains(type_name),
            "Markdown 应包含 {}: {}",
            type_name,
            md
        );
    }
}

#[test]
fn test_import_report_json_valid_structure() {
    let code = "fn foo() -> HttpResponse { unimplemented!() }";
    let json = verify_imports_to_json(code);

    // 验证 JSON 结构
    assert!(
        json.contains("\"total_issues\""),
        "应包含 total_issues 字段"
    );
    assert!(json.contains("\"has_issues\""), "应包含 has_issues 字段");
    assert!(json.contains("\"issues\""), "应包含 issues 字段");
    assert!(
        json.contains("\"modules_affected\""),
        "应包含 modules_affected 字段"
    );
}

#[test]
fn test_import_report_markdown_has_table() {
    let code = "fn foo() -> (Filter, PgConnection) { unimplemented!() }";
    let md = verify_imports_to_markdown(code);

    // Markdown 应包含表格
    assert!(
        md.contains("|") || md.contains("-"),
        "Markdown 应包含表格格式: {}",
        md
    );
    assert!(
        md.contains("**Modules Affected:**"),
        "Markdown 应包含 Modules Affected: {}",
        md
    );
}

// ===== Session 135: 多类型混合报告 =====

#[test]
fn test_import_report_mixed_s135_and_earlier_types() {
    // 混合 Session 135 和早期 Session 的类型
    let code =
        "fn foo() -> (Filter, Serialize, HashMap<String, HttpResponse>) { unimplemented!() }";
    let report = verify_imports_report(code);

    assert!(report.has_issues, "应有问题");
    // 应包含 warp (Session 135)
    assert!(
        report.issues.iter().any(|i| i.module_path == "warp"),
        "应包含 warp 问题"
    );
    // 应包含 serde (Session 127)
    assert!(
        report.issues.iter().any(|i| i.module_path == "serde"),
        "应包含 serde 问题"
    );
    // 应包含 std::collections (Session 124)
    assert!(
        report
            .issues
            .iter()
            .any(|i| i.module_path == "std::collections"),
        "应包含 std::collections 问题"
    );
    // 应包含 actix_web (Session 135)
    assert!(
        report.issues.iter().any(|i| i.module_path == "actix_web"),
        "应包含 actix_web 问题"
    );
}

// ===== Session 137: JSON 报告 — oauth2/glob/cookie 测试 =====

#[test]
fn test_import_report_json_oauth2_missing() {
    let code = "fn foo() -> BasicClient { unimplemented!() }";
    let json = verify_imports_to_json(code);
    assert!(
        json.contains("BasicClient"),
        "JSON 报告应包含 BasicClient: {}",
        json
    );
    assert!(
        json.contains("oauth2"),
        "JSON 报告应包含 oauth2 模块: {}",
        json
    );
}

#[test]
fn test_import_report_json_glob_missing() {
    let code = "fn foo() -> Pattern { unimplemented!() }";
    let json = verify_imports_to_json(code);
    assert!(
        json.contains("Pattern"),
        "JSON 报告应包含 Pattern: {}",
        json
    );
    assert!(json.contains("glob"), "JSON 报告应包含 glob 模块: {}", json);
}

#[test]
fn test_import_report_json_cookie_missing() {
    let code = "fn foo() -> Cookie { unimplemented!() }";
    let json = verify_imports_to_json(code);
    assert!(json.contains("Cookie"), "JSON 报告应包含 Cookie: {}", json);
    assert!(
        json.contains("cookie"),
        "JSON 报告应包含 cookie 模块: {}",
        json
    );
}

// ===== Session 137: Markdown 报告 — oauth2/glob/cookie 测试 =====

#[test]
fn test_import_report_markdown_oauth2_missing() {
    let code = "fn foo() -> BasicClient { unimplemented!() }";
    let md = verify_imports_to_markdown(code);
    assert!(
        md.contains("BasicClient"),
        "Markdown 报告应包含 BasicClient: {}",
        md
    );
    assert!(md.contains("oauth2"), "Markdown 报告应包含 oauth2: {}", md);
}

#[test]
fn test_import_report_markdown_glob_cookie_missing() {
    let code = "fn foo() -> (Pattern, Cookie) { unimplemented!() }";
    let md = verify_imports_to_markdown(code);
    assert!(
        md.contains("Pattern"),
        "Markdown 报告应包含 Pattern: {}",
        md
    );
    assert!(md.contains("glob"), "Markdown 报告应包含 glob: {}", md);
    assert!(md.contains("Cookie"), "Markdown 报告应包含 Cookie: {}", md);
    assert!(md.contains("cookie"), "Markdown 报告应包含 cookie: {}", md);
}

// ===== Session 137: ensure_external_imports 后无问题验证 =====

#[test]
fn test_import_report_after_ensure_external_no_s137_issues() {
    let code = "fn foo() -> (BasicClient, Pattern, Cookie) { unimplemented!() }\nfn bar() -> (AccessToken, GlobBuilder, CookieJar) { unimplemented!() }";
    let fixed = ensure_external_imports(code);
    let issues = verify_imports(&fixed);

    let s137_issues: Vec<_> = issues
        .iter()
        .filter(|i| {
            i.module_path == "oauth2" || i.module_path == "glob" || i.module_path == "cookie"
        })
        .collect();

    assert!(
        s137_issues.is_empty(),
        "ensure_external_imports 后不应有 Session 137 外部 crate 导入问题: {:?}",
        s137_issues
    );
}

// ===== Session 137: .zip()/.chain()/.enumerate() trait 方法报告 =====

#[test]
fn test_import_report_json_zip_chain_enumerate() {
    let code = "fn foo(v: Vec<i32>) { v.iter().zip(v.iter()).chain(v.iter()).enumerate(); }";
    let json = verify_imports_to_json(code);
    assert!(
        json.contains("Iterator"),
        "JSON 报告应包含 Iterator (.zip/.chain/.enumerate): {}",
        json
    );
}

#[test]
fn test_import_report_markdown_zip_chain_enumerate() {
    let code = "fn foo(v: Vec<i32>) { v.iter().zip(v.iter()).chain(v.iter()).enumerate(); }";
    let md = verify_imports_to_markdown(code);
    assert!(
        md.contains("Iterator"),
        "Markdown 报告应包含 Iterator (.zip/.chain/.enumerate): {}",
        md
    );
}

// ===== Session 138: JSON 报告 — dotenv/tauri/wgpu 测试 =====

#[test]
fn test_import_report_json_dotenv_missing() {
    let code = "fn foo() -> EnvError { unimplemented!() }";
    let json = verify_imports_to_json(code);
    assert!(
        json.contains("EnvError"),
        "JSON 报告应包含 EnvError: {}",
        json
    );
    assert!(
        json.contains("dotenv"),
        "JSON 报告应包含 dotenv 模块: {}",
        json
    );
}

#[test]
fn test_import_report_json_tauri_missing() {
    let code = "fn foo() -> AppBuilder { unimplemented!() }";
    let json = verify_imports_to_json(code);
    assert!(
        json.contains("AppBuilder"),
        "JSON 报告应包含 AppBuilder: {}",
        json
    );
    assert!(
        json.contains("tauri"),
        "JSON 报告应包含 tauri 模块: {}",
        json
    );
}

#[test]
fn test_import_report_json_wgpu_missing() {
    let code = "fn foo() -> Device { unimplemented!() }";
    let json = verify_imports_to_json(code);
    assert!(json.contains("Device"), "JSON 报告应包含 Device: {}", json);
    assert!(json.contains("wgpu"), "JSON 报告应包含 wgpu 模块: {}", json);
}

// ===== Session 138: Markdown 报告 — dotenv/tauri/wgpu 测试 =====

#[test]
fn test_import_report_markdown_dotenv_tauri_wgpu_missing() {
    let code = "fn foo() -> (EnvError, AppBuilder, Device) { unimplemented!() }";
    let md = verify_imports_to_markdown(code);
    assert!(
        md.contains("EnvError"),
        "Markdown 报告应包含 EnvError: {}",
        md
    );
    assert!(md.contains("dotenv"), "Markdown 报告应包含 dotenv: {}", md);
    assert!(
        md.contains("AppBuilder"),
        "Markdown 报告应包含 AppBuilder: {}",
        md
    );
    assert!(md.contains("tauri"), "Markdown 报告应包含 tauri: {}", md);
    assert!(md.contains("Device"), "Markdown 报告应包含 Device: {}", md);
    assert!(md.contains("wgpu"), "Markdown 报告应包含 wgpu: {}", md);
}

// ===== Session 138: ensure_external_imports 后无问题验证 =====

#[test]
fn test_import_report_after_ensure_external_no_s138_issues() {
    let code = "fn foo() -> (EnvError, AppBuilder, Device) { unimplemented!() }\nfn bar() -> (AppHandle, Queue, Surface) { unimplemented!() }\nfn baz() -> (Manager, Invoke, SurfaceConfiguration, ShaderModule) { unimplemented!() }";
    let fixed = ensure_external_imports(code);
    let issues = verify_imports(&fixed);

    let s138_issues: Vec<_> = issues
        .iter()
        .filter(|i| {
            i.module_path == "dotenv" || i.module_path == "tauri" || i.module_path == "wgpu"
        })
        .collect();

    assert!(
        s138_issues.is_empty(),
        "ensure_external_imports 后不应有 Session 138 外部 crate 导入问题: {:?}",
        s138_issues
    );
}

// ===== Session 138: .flat_map()/.peekable()/.skip() trait 方法报告 =====

#[test]
fn test_import_report_json_flat_map_peekable_skip() {
    let code = "fn foo(v: Vec<i32>) { v.iter().flat_map(|x| Some(x)).peekable().skip(1); }";
    let json = verify_imports_to_json(code);
    assert!(
        json.contains("Iterator"),
        "JSON 报告应包含 Iterator (.flat_map/.peekable/.skip): {}",
        json
    );
}

#[test]
fn test_import_report_markdown_flat_map_peekable_skip() {
    let code = "fn foo(v: Vec<i32>) { v.iter().flat_map(|x| Some(x)).peekable().skip(1); }";
    let md = verify_imports_to_markdown(code);
    assert!(
        md.contains("Iterator"),
        "Markdown 报告应包含 Iterator (.flat_map/.peekable/.skip): {}",
        md
    );
}

// ===== Session 138: 多类型混合报告 (S137 + S138) =====

#[test]
fn test_import_report_mixed_s137_s138_types() {
    let code = "fn foo() -> (BasicClient, Pattern, Cookie, EnvError, AppBuilder, Device) { unimplemented!() }";
    let report = verify_imports_report(code);

    assert!(report.has_issues, "应有问题");
    // Session 137 types
    assert!(
        report.issues.iter().any(|i| i.module_path == "oauth2"),
        "应包含 oauth2 问题"
    );
    assert!(
        report.issues.iter().any(|i| i.module_path == "glob"),
        "应包含 glob 问题"
    );
    assert!(
        report.issues.iter().any(|i| i.module_path == "cookie"),
        "应包含 cookie 问题"
    );
    // Session 138 types
    assert!(
        report.issues.iter().any(|i| i.module_path == "dotenv"),
        "应包含 dotenv 问题"
    );
    assert!(
        report.issues.iter().any(|i| i.module_path == "tauri"),
        "应包含 tauri 问题"
    );
    assert!(
        report.issues.iter().any(|i| i.module_path == "wgpu"),
        "应包含 wgpu 问题"
    );
}

// ===== Session 138: JSON / Markdown 双格式一致性 (S137 + S138) =====

#[test]
fn test_import_report_json_markdown_consistency_s138() {
    let code = "fn foo() -> (BasicClient, Device, EnvError) { unimplemented!() }\nfn bar(v: Vec<i32>) { v.iter().flat_map(|x| Some(x)).skip(1); }";
    let json = verify_imports_to_json(code);
    let md = verify_imports_to_markdown(code);

    for type_name in &["BasicClient", "Device", "EnvError", "Iterator"] {
        assert!(
            json.contains(type_name),
            "JSON 应包含 {}: {}",
            type_name,
            json
        );
        assert!(
            md.contains(type_name),
            "Markdown 应包含 {}: {}",
            type_name,
            md
        );
    }
}

// ===== Session 139: JSON 报告 — env_logger/notify/shadow-rs 测试 =====

#[test]
fn test_import_report_json_env_logger_missing() {
    let code = "fn foo() -> Builder { Builder::new() }";
    let json = verify_imports_to_json(code);
    assert!(
        json.contains("Builder"),
        "JSON 报告应包含 Builder: {}",
        json
    );
    assert!(
        json.contains("env_logger"),
        "JSON 报告应包含 env_logger 模块: {}",
        json
    );
}

#[test]
fn test_import_report_json_notify_missing() {
    let code = "fn foo(w: &Watcher) { w.watch(); }";
    let json = verify_imports_to_json(code);
    assert!(
        json.contains("Watcher"),
        "JSON 报告应包含 Watcher: {}",
        json
    );
    assert!(
        json.contains("notify"),
        "JSON 报告应包含 notify 模块: {}",
        json
    );
}

#[test]
fn test_import_report_json_shadow_rs_missing() {
    let code = "fn foo() -> ShadowBuilder { ShadowBuilder::new() }";
    let json = verify_imports_to_json(code);
    assert!(
        json.contains("ShadowBuilder"),
        "JSON 报告应包含 ShadowBuilder: {}",
        json
    );
    assert!(
        json.contains("shadow_rs"),
        "JSON 报告应包含 shadow_rs 模块: {}",
        json
    );
}

// ===== Session 139: Markdown 报告 — env_logger/notify/shadow-rs 测试 =====

#[test]
fn test_import_report_markdown_env_logger_notify_shadow_rs() {
    let code = "fn foo() -> (Builder, Watcher, ShadowBuilder) { unimplemented!() }";
    let md = verify_imports_to_markdown(code);
    assert!(
        md.contains("Builder"),
        "Markdown 报告应包含 Builder: {}",
        md
    );
    assert!(
        md.contains("env_logger"),
        "Markdown 报告应包含 env_logger: {}",
        md
    );
    assert!(
        md.contains("Watcher"),
        "Markdown 报告应包含 Watcher: {}",
        md
    );
    assert!(md.contains("notify"), "Markdown 报告应包含 notify: {}", md);
    assert!(
        md.contains("ShadowBuilder"),
        "Markdown 报告应包含 ShadowBuilder: {}",
        md
    );
    assert!(
        md.contains("shadow_rs"),
        "Markdown 报告应包含 shadow_rs: {}",
        md
    );
}

// ===== Session 139: ensure_external_imports 后无问题验证 =====

#[test]
fn test_import_report_after_ensure_external_no_s139_issues() {
    let code = "fn foo() -> (Builder, Watcher, ShadowBuilder) { unimplemented!() }\nfn bar() -> (Target, EventKind) { unimplemented!() }\nfn baz() -> (Filter, Event) { unimplemented!() }";
    let fixed = ensure_external_imports(code);
    let issues = verify_imports(&fixed);

    let s139_issues: Vec<_> = issues
        .iter()
        .filter(|i| {
            i.module_path == "env_logger"
                || i.module_path == "notify"
                || i.module_path == "shadow_rs"
        })
        .collect();

    assert!(
        s139_issues.is_empty(),
        "ensure_external_imports 后不应有 Session 139 外部 crate 导入问题: {:?}",
        s139_issues
    );
}

// ===== Session 139: .take()/.rev()/.step_by() trait 方法报告 =====

#[test]
fn test_import_report_json_take_rev_step_by() {
    let code = "fn foo(v: Vec<i32>) { v.iter().take(3).rev().step_by(2); }";
    let json = verify_imports_to_json(code);
    assert!(
        json.contains("Iterator"),
        "JSON 报告应包含 Iterator (.take/.rev/.step_by): {}",
        json
    );
}

#[test]
fn test_import_report_markdown_take_rev_step_by() {
    let code = "fn foo(v: Vec<i32>) { v.iter().take(3).rev().step_by(2); }";
    let md = verify_imports_to_markdown(code);
    assert!(
        md.contains("Iterator"),
        "Markdown 报告应包含 Iterator (.take/.rev/.step_by): {}",
        md
    );
}

// ===== Session 139: 多类型混合报告 (S138 + S139) =====

#[test]
fn test_import_report_mixed_s138_s139_types() {
    let code = "fn foo() -> (EnvError, AppBuilder, Device, Builder, Watcher, ShadowBuilder) { unimplemented!() }";
    let report = verify_imports_report(code);

    assert!(report.has_issues, "应有问题");
    // Session 138 types
    assert!(
        report.issues.iter().any(|i| i.module_path == "dotenv"),
        "应包含 dotenv 问题"
    );
    assert!(
        report.issues.iter().any(|i| i.module_path == "tauri"),
        "应包含 tauri 问题"
    );
    assert!(
        report.issues.iter().any(|i| i.module_path == "wgpu"),
        "应包含 wgpu 问题"
    );
    // Session 139 types
    assert!(
        report.issues.iter().any(|i| i.module_path == "env_logger"),
        "应包含 env_logger 问题"
    );
    assert!(
        report.issues.iter().any(|i| i.module_path == "notify"),
        "应包含 notify 问题"
    );
    assert!(
        report.issues.iter().any(|i| i.module_path == "shadow_rs"),
        "应包含 shadow_rs 问题"
    );
}

// ===== Session 139: JSON / Markdown 双格式一致性 (S138 + S139) =====

#[test]
fn test_import_report_json_markdown_consistency_s139() {
    let code = "fn foo() -> (Builder, Device, EnvError, ShadowBuilder) { unimplemented!() }\nfn bar(v: Vec<i32>) { v.iter().take(3).rev().step_by(1); }";
    let json = verify_imports_to_json(code);
    let md = verify_imports_to_markdown(code);

    for type_name in &["Builder", "Device", "EnvError", "ShadowBuilder", "Iterator"] {
        assert!(
            json.contains(type_name),
            "JSON 应包含 {}: {}",
            type_name,
            json
        );
        assert!(
            md.contains(type_name),
            "Markdown 应包含 {}: {}",
            type_name,
            md
        );
    }
}

// ===== Session 141: JSON 报告 — sysinfo/serialport/machine-uid 测试 =====

#[test]
fn test_import_report_json_sysinfo_missing() {
    let code = "fn foo() -> System { System::new() }";
    let json = verify_imports_to_json(code);
    assert!(json.contains("System"), "JSON 报告应包含 System: {}", json);
    assert!(
        json.contains("sysinfo"),
        "JSON 报告应包含 sysinfo 模块: {}",
        json
    );
}

#[test]
fn test_import_report_json_sysinfo_cpu_core_missing() {
    let code = "fn foo() -> CpuCore { unimplemented!() }";
    let json = verify_imports_to_json(code);
    assert!(
        json.contains("CpuCore"),
        "JSON 报告应包含 CpuCore: {}",
        json
    );
    assert!(
        json.contains("sysinfo"),
        "JSON 报告应包含 sysinfo 模块: {}",
        json
    );
}

#[test]
fn test_import_report_json_sysinfo_disk_missing() {
    let code = "fn foo() -> Disk { unimplemented!() }";
    let json = verify_imports_to_json(code);
    assert!(json.contains("Disk"), "JSON 报告应包含 Disk: {}", json);
    assert!(
        json.contains("sysinfo"),
        "JSON 报告应包含 sysinfo 模块: {}",
        json
    );
}

#[test]
fn test_import_report_json_serialport_missing() {
    let code = "fn foo() -> SerialPort { unimplemented!() }";
    let json = verify_imports_to_json(code);
    assert!(
        json.contains("SerialPort"),
        "JSON 报告应包含 SerialPort: {}",
        json
    );
    assert!(
        json.contains("serialport"),
        "JSON 报告应包含 serialport 模块: {}",
        json
    );
}

// ===== Session 141: Markdown 报告 — sysinfo/serialport 测试 =====

#[test]
fn test_import_report_markdown_sysinfo_serialport() {
    let code =
        "fn foo() -> (System, CpuCore, Disk) { unimplemented!() }\nfn bar() -> SerialPort { unimplemented!() }";
    let md = verify_imports_to_markdown(code);
    assert!(
        md.contains("sysinfo"),
        "Markdown 报告应包含 sysinfo: {}",
        md
    );
    assert!(
        md.contains("serialport"),
        "Markdown 报告应包含 serialport: {}",
        md
    );
    assert!(md.contains("System"), "Markdown 报告应包含 System: {}", md);
    assert!(
        md.contains("SerialPort"),
        "Markdown 报告应包含 SerialPort: {}",
        md
    );
}

// ===== Session 141: ensure_external_imports 后无问题验证 =====

#[test]
fn test_import_report_after_ensure_external_no_s141_issues() {
    let code = "fn foo() -> (System, CpuCore, Disk, SerialPort) { unimplemented!() }";
    let fixed = ensure_external_imports(code);
    let issues = verify_imports(&fixed);
    let s141_issues: Vec<_> = issues
        .iter()
        .filter(|i| {
            i.module_path == "sysinfo"
                || i.module_path == "serialport"
                || i.module_path == "machine_uid"
        })
        .collect();

    assert!(
        s141_issues.is_empty(),
        "ensure_external_imports 后不应有 Session 141 外部 crate 导入问题: {:?}",
        s141_issues
    );
}

// ===== Session 141: .cloned()/.copied()/.fuse() trait 方法报告 =====

#[test]
fn test_import_report_json_cloned_copied_fuse() {
    let code = "fn foo(v: Vec<&i32>) { v.iter().cloned().copied().fuse(); }";
    let json = verify_imports_to_json(code);
    assert!(
        json.contains("Iterator"),
        "JSON 报告应包含 Iterator (.cloned/.copied/.fuse): {}",
        json
    );
}

#[test]
fn test_import_report_markdown_cloned_copied_fuse() {
    let code = "fn foo(v: Vec<&i32>) { v.iter().cloned().copied().fuse(); }";
    let md = verify_imports_to_markdown(code);
    assert!(
        md.contains("Iterator"),
        "Markdown 报告应包含 Iterator (.cloned/.copied/.fuse): {}",
        md
    );
}

// ===== Session 141: 多类型混合报告 (S139 + S141) =====

#[test]
fn test_import_report_mixed_s139_s141_types() {
    let code = "fn foo() -> (Builder, Watcher, ShadowBuilder, System, CpuCore, Disk, SerialPort) { unimplemented!() }";
    let report = verify_imports_report(code);

    assert!(report.has_issues, "应有问题");
    // Session 139 types
    assert!(
        report.issues.iter().any(|i| i.module_path == "env_logger"),
        "应包含 env_logger 问题"
    );
    assert!(
        report.issues.iter().any(|i| i.module_path == "notify"),
        "应包含 notify 问题"
    );
    assert!(
        report.issues.iter().any(|i| i.module_path == "shadow_rs"),
        "应包含 shadow_rs 问题"
    );
    // Session 141 types
    assert!(
        report.issues.iter().any(|i| i.module_path == "sysinfo"),
        "应包含 sysinfo 问题"
    );
    assert!(
        report.issues.iter().any(|i| i.module_path == "serialport"),
        "应包含 serialport 问题"
    );
}

// ===== Session 141: JSON / Markdown 双格式一致性 (S139 + S141) =====

#[test]
fn test_import_report_json_markdown_consistency_s141() {
    let code = "fn foo() -> (Builder, System, SerialPort, ShadowBuilder) { unimplemented!() }\nfn bar(v: Vec<&i32>) { v.iter().cloned().copied().fuse(); }";
    let json = verify_imports_to_json(code);
    let md = verify_imports_to_markdown(code);

    for type_name in &[
        "Builder",
        "System",
        "SerialPort",
        "ShadowBuilder",
        "Iterator",
    ] {
        assert!(
            json.contains(type_name),
            "JSON 应包含 {}: {}",
            type_name,
            json
        );
        assert!(
            md.contains(type_name),
            "Markdown 应包含 {}: {}",
            type_name,
            md
        );
    }
}

// ===== Session 142: JSON 报告 — 新增外部 crate 测试 =====

#[test]
fn test_import_report_json_dotenvy_missing() {
    let code = "fn foo() -> EnvLoader { unimplemented!() }";
    let json = verify_imports_to_json(code);
    assert!(
        json.contains("EnvLoader"),
        "JSON 报告应包含 EnvLoader: {}",
        json
    );
    assert!(
        json.contains("dotenvy"),
        "JSON 报告应包含 dotenvy 模块: {}",
        json
    );
}

#[test]
fn test_import_report_json_fd_lock_missing() {
    let code = "fn foo() -> FdLock { unimplemented!() }";
    let json = verify_imports_to_json(code);
    assert!(json.contains("FdLock"), "JSON 报告应包含 FdLock: {}", json);
    assert!(
        json.contains("fd_lock"),
        "JSON 报告应包含 fd_lock 模块: {}",
        json
    );
}

#[test]
fn test_import_report_json_nix_missing() {
    let code = "fn foo() -> (NixPath, Errno) { unimplemented!() }";
    let json = verify_imports_to_json(code);
    assert!(
        json.contains("NixPath"),
        "JSON 报告应包含 NixPath: {}",
        json
    );
    assert!(json.contains("nix"), "JSON 报告应包含 nix 模块: {}", json);
}

#[test]
fn test_import_report_json_camino_missing() {
    let code = "fn foo() -> Utf8PathBuf { unimplemented!() }";
    let json = verify_imports_to_json(code);
    assert!(
        json.contains("Utf8PathBuf"),
        "JSON 报告应包含 Utf8PathBuf: {}",
        json
    );
    assert!(
        json.contains("camino"),
        "JSON 报告应包含 camino 模块: {}",
        json
    );
}

// ===== Session 142: Markdown 报告 — 新增外部 crate 测试 =====

#[test]
fn test_import_report_markdown_dotenvy_missing() {
    let code = "fn foo() -> EnvLoader { unimplemented!() }";
    let md = verify_imports_to_markdown(code);
    assert!(
        md.contains("EnvLoader"),
        "Markdown 报告应包含 EnvLoader: {}",
        md
    );
    assert!(
        md.contains("dotenvy"),
        "Markdown 报告应包含 dotenvy 模块: {}",
        md
    );
}

#[test]
fn test_import_report_markdown_fd_lock_missing() {
    let code = "fn foo() -> FdLock { unimplemented!() }";
    let md = verify_imports_to_markdown(code);
    assert!(md.contains("FdLock"), "Markdown 报告应包含 FdLock: {}", md);
    assert!(
        md.contains("fd_lock"),
        "Markdown 报告应包含 fd_lock 模块: {}",
        md
    );
}

#[test]
fn test_import_report_markdown_camino_missing() {
    let code = "fn foo() -> Utf8PathBuf { unimplemented!() }";
    let md = verify_imports_to_markdown(code);
    assert!(
        md.contains("Utf8PathBuf"),
        "Markdown 报告应包含 Utf8PathBuf: {}",
        md
    );
    assert!(
        md.contains("camino"),
        "Markdown 报告应包含 camino 模块: {}",
        md
    );
}

// ===== Session 142: ensure_external_imports 后无问题验证 =====

#[test]
fn test_import_report_after_ensure_external_no_s142_issues() {
    let code = "fn foo() -> (EnvLoader, FdLock, NixPath, Utf8PathBuf) { unimplemented!() }";
    let fixed = ensure_external_imports(code);
    let issues = verify_imports(&fixed);
    let s142_issues: Vec<_> = issues
        .iter()
        .filter(|i| {
            i.module_path == "dotenvy"
                || i.module_path == "fd_lock"
                || i.module_path == "nix::path"
                || i.module_path == "nix::errno"
                || i.module_path == "camino"
        })
        .collect();

    assert!(
        s142_issues.is_empty(),
        "ensure_external_imports 后不应有 Session 142 外部 crate 导入问题: {:?}",
        s142_issues
    );
}

// ===== Session 142: .flatten()/.max()/.min()/.sum()/.product() trait 方法报告 =====

#[test]
fn test_import_report_json_flatten_max_min_sum_product() {
    let code = "fn foo(v: Vec<i32>) { v.iter().flatten().max(Ord).min(Ord); let s = v.iter().sum(); let p = v.iter().product(); }";
    let json = verify_imports_to_json(code);
    assert!(
        json.contains("Iterator"),
        "JSON 报告应包含 Iterator (.flatten/.max/.min/.sum/.product): {}",
        json
    );
}

#[test]
fn test_import_report_markdown_flatten_max_min_sum_product() {
    let code = "fn foo(v: Vec<i32>) { v.iter().flatten().max(Ord).min(Ord); let s = v.iter().sum(); let p = v.iter().product(); }";
    let md = verify_imports_to_markdown(code);
    assert!(
        md.contains("Iterator"),
        "Markdown 报告应包含 Iterator (.flatten/.max/.min/.sum/.product): {}",
        md
    );
}

// ===== Session 142: 多类型混合报告 (S141 + S142) =====

#[test]
fn test_import_report_mixed_s141_s142_types() {
    let code =
        "fn foo() -> (System, SerialPort, EnvLoader, FdLock, Utf8PathBuf) { unimplemented!() }";
    let report = verify_imports_report(code);

    assert!(report.has_issues, "应有问题");
    // Session 141 types
    assert!(
        report.issues.iter().any(|i| i.module_path == "sysinfo"),
        "应包含 sysinfo 问题"
    );
    assert!(
        report.issues.iter().any(|i| i.module_path == "serialport"),
        "应包含 serialport 问题"
    );
    // Session 142 types
    assert!(
        report.issues.iter().any(|i| i.module_path == "dotenvy"),
        "应包含 dotenvy 问题"
    );
    assert!(
        report.issues.iter().any(|i| i.module_path == "fd_lock"),
        "应包含 fd_lock 问题"
    );
    assert!(
        report.issues.iter().any(|i| i.module_path == "camino"),
        "应包含 camino 问题"
    );
}

// ===== Session 142: JSON / Markdown 双格式一致性 (S141 + S142) =====

#[test]
fn test_import_report_json_markdown_consistency_s142() {
    let code = "fn foo() -> (System, EnvLoader, FdLock, Utf8PathBuf) { unimplemented!() }\nfn bar(v: Vec<i32>) { v.iter().flatten().max(Ord).sum(); }";
    let json = verify_imports_to_json(code);
    let md = verify_imports_to_markdown(code);

    for type_name in &["System", "EnvLoader", "FdLock", "Utf8PathBuf", "Iterator"] {
        assert!(
            json.contains(type_name),
            "JSON 应包含 {}: {}",
            type_name,
            json
        );
        assert!(
            md.contains(type_name),
            "Markdown 应包含 {}: {}",
            type_name,
            md
        );
    }
}
