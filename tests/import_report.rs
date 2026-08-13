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
