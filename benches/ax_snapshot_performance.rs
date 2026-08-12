#![allow(clippy::useless_vec)]

//! ax_snapshot 性能基准测试
//!
//! 测试目标:
//! 1. role_classification - 角色分类 (is_interactive_role/is_content_role/is_structural_role/is_known_role)
//! 2. ax_node_operations - AxNode 创建和方法 (empty/is_interactive/is_content/has_ref)
//! 3. snapshot_from_cdp - AxSnapshot::from_cdp_response 不同规模
//! 4. snapshot_queries - get_by_ref/interactive_nodes/find_by_role_and_name
//! 5. snapshot_to_text - to_text 不同选项 (interactive_only/compact/max_depth/include_urls)

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use forge::ax_snapshot::*;
use serde_json::{json, Value};

// ============================================================================
//  辅助函数
// ============================================================================

fn make_cdp_response(node_count: usize) -> Value {
    let nodes: Vec<Value> = (0..node_count)
        .map(|i| {
            let role = match i % 6 {
                0 => "button",
                1 => "link",
                2 => "textbox",
                3 => "heading",
                4 => "group",
                _ => "listitem",
            };
            let name = format!("Element {}", i);
            json!({
                "role": {"value": role},
                "name": {"value": name},
                "backendDOMNodeId": i + 1,
                "level": if role == "heading" { json!({"value": 2}) } else { Value::Null },
            })
        })
        .collect();

    json!({"result": {"axTree": nodes}})
}

/// 生成带 nodeId/parentId 的 CDP 响应 (树结构, 用于深度计算基准)
fn make_tree_cdp_response(depth: usize, branching: usize) -> Value {
    let mut nodes = Vec::new();
    let mut next_id = 1usize;

    // BFS 生成树
    let mut current_level: Vec<usize> = vec![]; // node IDs at current level
    for d in 0..depth {
        let mut next_level = Vec::new();
        let count = if d == 0 {
            1 // 根节点
        } else {
            current_level.len() * branching
        };
        for _ in 0..count {
            let node_id = next_id.to_string();
            next_id += 1;
            let parent_id = if d == 0 {
                Value::Null
            } else {
                let parent = current_level[next_level.len() / branching];
                json!(parent.to_string())
            };
            let role = if d == 0 { "WebArea" } else { "group" };
            nodes.push(json!({
                "nodeId": node_id,
                "parentId": parent_id,
                "role": {"value": role},
                "name": {"value": format!("Node {}", next_id - 1)},
                "backendDOMNodeId": next_id - 1,
            }));
            next_level.push(next_id - 1);
        }
        current_level = next_level;
    }

    json!({"result": {"axTree": nodes}})
}

// ============================================================================
//  基准测试 1: role_classification
// ============================================================================

fn bench_role_classification(c: &mut Criterion) {
    let mut group = c.benchmark_group("role_classification");

    // 所有交互式角色
    let interactive_roles = vec![
        "button",
        "link",
        "textbox",
        "checkbox",
        "radio",
        "combobox",
        "listbox",
        "menuitem",
        "option",
        "searchbox",
        "slider",
        "spinbutton",
        "switch",
        "tab",
        "treeitem",
    ];
    group.bench_function("is_interactive_all_15", |b| {
        b.iter(|| {
            let results: Vec<bool> = black_box(&interactive_roles)
                .iter()
                .map(|r| is_interactive_role(r))
                .collect();
            black_box(results);
        })
    });

    // 所有内容角色
    let content_roles = vec![
        "heading",
        "cell",
        "gridcell",
        "columnheader",
        "rowheader",
        "listitem",
        "article",
        "region",
        "main",
        "navigation",
    ];
    group.bench_function("is_content_all_10", |b| {
        b.iter(|| {
            let results: Vec<bool> = black_box(&content_roles)
                .iter()
                .map(|r| is_content_role(r))
                .collect();
            black_box(results);
        })
    });

    // 所有结构角色
    let structural_roles = vec![
        "generic",
        "group",
        "list",
        "table",
        "row",
        "rowgroup",
        "grid",
        "treegrid",
        "menu",
        "menubar",
        "toolbar",
        "tablist",
        "tree",
        "directory",
        "document",
        "application",
        "presentation",
        "none",
        "WebArea",
        "RootWebArea",
    ];
    group.bench_function("is_structural_all_20", |b| {
        b.iter(|| {
            let results: Vec<bool> = black_box(&structural_roles)
                .iter()
                .map(|r| is_structural_role(r))
                .collect();
            black_box(results);
        })
    });

    // 单个角色判断
    for role in &["button", "heading", "group", "unknown"] {
        group.bench_function(format!("is_interactive_{}", role), |b| {
            b.iter(|| {
                let result = is_interactive_role(black_box(role));
                black_box(result);
            })
        });
    }

    // is_known_role (组合判断)
    let all_roles: Vec<&str> = interactive_roles
        .iter()
        .chain(content_roles.iter())
        .chain(structural_roles.iter())
        .copied()
        .collect();
    group.bench_function("is_known_all_45", |b| {
        b.iter(|| {
            let results: Vec<bool> = black_box(&all_roles)
                .iter()
                .map(|r| is_known_role(r))
                .collect();
            black_box(results);
        })
    });

    // case insensitive
    group.bench_function("case_insensitive", |b| {
        b.iter(|| {
            let upper = is_interactive_role(black_box("BUTTON"));
            let mixed = is_interactive_role(black_box("Link"));
            let lower = is_interactive_role(black_box("textbox"));
            black_box((upper, mixed, lower));
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 2: ax_node_operations
// ============================================================================

fn bench_ax_node_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("ax_node_operations");

    // AxNode::empty
    group.bench_function("empty", |b| {
        b.iter(|| {
            let node = AxNode::empty();
            black_box(node);
        })
    });

    // is_interactive
    let mut button_node = AxNode::empty();
    button_node.role = "button".to_string();
    group.bench_function("is_interactive_button", |b| {
        b.iter(|| {
            let result = button_node.is_interactive();
            black_box(result);
        })
    });

    // is_content
    let mut heading_node = AxNode::empty();
    heading_node.role = "heading".to_string();
    group.bench_function("is_content_heading", |b| {
        b.iter(|| {
            let result = heading_node.is_content();
            black_box(result);
        })
    });

    // is_structural
    let mut group_node = AxNode::empty();
    group_node.role = "group".to_string();
    group.bench_function("is_structural_group", |b| {
        b.iter(|| {
            let result = group_node.is_structural();
            black_box(result);
        })
    });

    // has_ref (无 ref)
    let no_ref_node = AxNode::empty();
    group.bench_function("has_ref_false", |b| {
        b.iter(|| {
            let result = no_ref_node.has_ref();
            black_box(result);
        })
    });

    // has_ref (有 ref)
    let mut ref_node = AxNode::empty();
    ref_node.ref_id = Some("e42".to_string());
    group.bench_function("has_ref_true", |b| {
        b.iter(|| {
            let result = ref_node.has_ref();
            black_box(result);
        })
    });

    // 全方法一次调用
    group.bench_function("all_methods", |b| {
        b.iter(|| {
            let node = AxNode::empty();
            let _ = node.is_interactive();
            let _ = node.is_content();
            let _ = node.is_structural();
            let _ = node.has_ref();
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 3: snapshot_from_cdp
// ============================================================================

fn bench_snapshot_from_cdp(c: &mut Criterion) {
    let mut group = c.benchmark_group("snapshot_from_cdp");

    // 不同规模
    for &size in &[3usize, 10, 50, 100, 500] {
        let response = make_cdp_response(size);
        group.bench_with_input(
            BenchmarkId::new("from_cdp_response", size),
            &response,
            |b, response| {
                b.iter(|| {
                    let snapshot = AxSnapshot::from_cdp_response(black_box(response));
                    black_box(snapshot);
                })
            },
        );
    }

    // 空响应
    let empty_response = json!({});
    group.bench_function("from_empty_response", |b| {
        b.iter(|| {
            let snapshot = AxSnapshot::from_cdp_response(black_box(&empty_response));
            black_box(snapshot);
        })
    });

    // 响应无 axTree
    let no_tree_response = json!({"result": {}});
    group.bench_function("from_no_tree", |b| {
        b.iter(|| {
            let snapshot = AxSnapshot::from_cdp_response(black_box(&no_tree_response));
            black_box(snapshot);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 4: snapshot_queries
// ============================================================================

fn bench_snapshot_queries(c: &mut Criterion) {
    let mut group = c.benchmark_group("snapshot_queries");

    // 准备 100 节点的快照
    let response = make_cdp_response(100);
    let snapshot = AxSnapshot::from_cdp_response(&response);

    // get_by_ref (存在)
    group.bench_function("get_by_ref_found", |b| {
        b.iter(|| {
            let node = snapshot.get_by_ref(black_box("e1"));
            black_box(node);
        })
    });

    // get_by_ref (不存在)
    group.bench_function("get_by_ref_not_found", |b| {
        b.iter(|| {
            let node = snapshot.get_by_ref(black_box("e999"));
            black_box(node);
        })
    });

    // interactive_nodes
    group.bench_function("interactive_nodes_100", |b| {
        b.iter(|| {
            let nodes = snapshot.interactive_nodes();
            black_box(nodes);
        })
    });

    // find_by_role_and_name (存在)
    group.bench_function("find_by_role_and_name_found", |b| {
        b.iter(|| {
            let node = snapshot.find_by_role_and_name(black_box("button"), black_box("Element 0"));
            black_box(node);
        })
    });

    // find_by_role_and_name (不存在)
    group.bench_function("find_by_role_and_name_not_found", |b| {
        b.iter(|| {
            let node =
                snapshot.find_by_role_and_name(black_box("button"), black_box("Nonexistent"));
            black_box(node);
        })
    });

    // 批量 get_by_ref 50 次
    let ref_ids: Vec<String> = (1..=50).map(|i| format!("e{}", i)).collect();
    group.bench_function("batch_get_by_ref_50", |b| {
        b.iter(|| {
            let nodes: Vec<Option<&AxNode>> = black_box(&ref_ids)
                .iter()
                .map(|r| snapshot.get_by_ref(r))
                .collect();
            black_box(nodes);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 5: snapshot_to_text
// ============================================================================

fn bench_snapshot_to_text(c: &mut Criterion) {
    let mut group = c.benchmark_group("snapshot_to_text");

    // 准备快照
    let response_10 = make_cdp_response(10);
    let snapshot_10 = AxSnapshot::from_cdp_response(&response_10);
    let response_100 = make_cdp_response(100);
    let snapshot_100 = AxSnapshot::from_cdp_response(&response_100);

    // 默认选项 10 节点
    group.bench_function("default_10", |b| {
        b.iter(|| {
            let text = snapshot_10.to_text(black_box(&SnapshotOptions::default()));
            black_box(text);
        })
    });

    // 默认选项 100 节点
    group.bench_function("default_100", |b| {
        b.iter(|| {
            let text = snapshot_100.to_text(black_box(&SnapshotOptions::default()));
            black_box(text);
        })
    });

    // interactive_only
    let interactive_opts = SnapshotOptions {
        interactive_only: true,
        ..Default::default()
    };
    group.bench_function("interactive_only_100", |b| {
        b.iter(|| {
            let text = snapshot_100.to_text(black_box(&interactive_opts));
            black_box(text);
        })
    });

    // compact 模式
    let compact_opts = SnapshotOptions {
        compact: true,
        ..Default::default()
    };
    group.bench_function("compact_100", |b| {
        b.iter(|| {
            let text = snapshot_100.to_text(black_box(&compact_opts));
            black_box(text);
        })
    });

    // max_depth 限制
    let depth_opts = SnapshotOptions {
        max_depth: Some(2),
        ..Default::default()
    };
    group.bench_function("max_depth_2_100", |b| {
        b.iter(|| {
            let text = snapshot_100.to_text(black_box(&depth_opts));
            black_box(text);
        })
    });

    // include_urls
    let url_opts = SnapshotOptions {
        include_urls: true,
        ..Default::default()
    };
    group.bench_function("include_urls_10", |b| {
        b.iter(|| {
            let text = snapshot_10.to_text(black_box(&url_opts));
            black_box(text);
        })
    });

    // build_snapshot_js
    group.bench_function("build_snapshot_js", |b| {
        b.iter(|| {
            let js = build_snapshot_js();
            black_box(js);
        })
    });

    group.finish();
}

// ============================================================================
//  基准测试 6: depth_computation (Session 110)
// ============================================================================

fn bench_depth_computation(c: &mut Criterion) {
    let mut group = c.benchmark_group("depth_computation");

    // 不同深度的线性链 (每层 1 个子节点)
    for &depth in &[3usize, 5, 10, 50, 100] {
        let response = make_tree_cdp_response(depth, 1);
        group.bench_with_input(
            BenchmarkId::new("linear_chain", depth),
            &response,
            |b, response| {
                b.iter(|| {
                    let snapshot = AxSnapshot::from_cdp_response(black_box(response));
                    if !snapshot.nodes.is_empty() {
                        let max_depth = snapshot.nodes.iter().map(|n| n.depth).max().unwrap();
                        black_box(max_depth);
                    }
                    black_box(snapshot);
                })
            },
        );
    }

    // 分支树 (3 层 × 3 分支 = 40 节点)
    let tree_response = make_tree_cdp_response(3, 3);
    group.bench_function("tree_3x3", |b| {
        b.iter(|| {
            let snapshot = AxSnapshot::from_cdp_response(black_box(&tree_response));
            black_box(snapshot);
        })
    });

    // 分支树 (4 层 × 2 分支 = 31 节点)
    let tree_4x2 = make_tree_cdp_response(4, 2);
    group.bench_function("tree_4x2", |b| {
        b.iter(|| {
            let snapshot = AxSnapshot::from_cdp_response(black_box(&tree_4x2));
            black_box(snapshot);
        })
    });

    // 无 nodeId/parentId 的响应 (向后兼容)
    let flat_response = make_cdp_response(100);
    group.bench_function("flat_no_parent_ids_100", |b| {
        b.iter(|| {
            let snapshot = AxSnapshot::from_cdp_response(black_box(&flat_response));
            let all_zero = snapshot.nodes.iter().all(|n| n.depth == 0);
            black_box(all_zero);
            black_box(snapshot);
        })
    });

    // 孤儿节点 (parentId 指向不存在的节点)
    let orphan_response = json!({
        "result": {
            "axTree": [
                {"nodeId": "1", "parentId": "999", "role": {"value": "button"}, "name": {"value": "Orphan"}, "backendDOMNodeId": 1}
            ]
        }
    });
    group.bench_function("orphan_parent", |b| {
        b.iter(|| {
            let snapshot = AxSnapshot::from_cdp_response(black_box(&orphan_response));
            black_box(snapshot);
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
        .output_directory(std::path::Path::new("target/criterion/ax_snapshot"))
}

criterion_group! {
    name = ax_snapshot_benches;
    config = configure_criterion();
    targets = bench_role_classification,
        bench_ax_node_operations,
        bench_snapshot_from_cdp,
        bench_snapshot_queries,
        bench_snapshot_to_text,
        bench_depth_computation,
}

criterion_main!(ax_snapshot_benches);
