//! Accessibility Tree 快照 — 借鉴 agent-browser snapshot.rs
//!
//! 通过 CDP `Accessibility.getFullAXTree` 获取页面的无障碍树,
//! 提供比 CSS 选择器更稳健的元素定位方式。
//!
//! ## 设计
//!
//! - [`AxNode`][]: 无障碍树节点
//! - [`AxSnapshot`][]: 快照结果, 包含所有节点和 ref 映射
//! - [`build_snapshot_js`][]: 构建快照提取 JS
//!
//! ## 与现有机制的关系
//!
//! Forge 当前通过 CSS 选择器 (`document.querySelector`) 定位元素。
//! AX Tree 提供了另一种定位方式 — 通过 role + name 定位,
//! 对动态页面更稳健 (选择器可能变化, 但 role + name 通常不变)。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 交互式角色 — 可以点击/输入的元素
pub const INTERACTIVE_ROLES: &[&str] = &[
    "button",
    "link",
    "textbox",
    "checkbox",
    "radio",
    "combobox",
    "listbox",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "option",
    "searchbox",
    "slider",
    "spinbutton",
    "switch",
    "tab",
    "treeitem",
];

/// 内容角色 — 显示文本的元素
pub const CONTENT_ROLES: &[&str] = &[
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

/// 结构角色 — 容器元素
pub const STRUCTURAL_ROLES: &[&str] = &[
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

/// 无障碍树节点 — 借鉴 agent-browser TreeNode
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AxNode {
    /// 节点角色 (button, link, textbox 等)
    pub role: String,
    /// 节点名称 (按钮文本、链接文本等)
    pub name: String,
    /// 层级 (heading 级别等)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<i64>,
    /// 是否选中
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked: Option<String>,
    /// 是否展开
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expanded: Option<bool>,
    /// 是否禁用
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    /// 是否必填
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    /// 值文本
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_text: Option<String>,
    /// 后端节点 ID (CDP 内部)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend_node_id: Option<i64>,
    /// 引用 ID (用于 click/fill 操作)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ref_id: Option<String>,
    /// 子节点索引
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub children: Vec<usize>,
    /// 父节点索引
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_idx: Option<usize>,
    /// 节点深度
    pub depth: usize,
    /// URL (link 角色时)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl AxNode {
    /// 创建空节点
    pub fn empty() -> Self {
        Self {
            role: String::new(),
            name: String::new(),
            level: None,
            checked: None,
            expanded: None,
            disabled: None,
            required: None,
            value_text: None,
            backend_node_id: None,
            ref_id: None,
            children: Vec::new(),
            parent_idx: None,
            depth: 0,
            url: None,
        }
    }

    /// 是否为交互式元素
    pub fn is_interactive(&self) -> bool {
        INTERACTIVE_ROLES
            .iter()
            .any(|r| r.eq_ignore_ascii_case(&self.role))
    }

    /// 是否为内容元素
    pub fn is_content(&self) -> bool {
        CONTENT_ROLES
            .iter()
            .any(|r| r.eq_ignore_ascii_case(&self.role))
    }

    /// 是否为结构元素
    pub fn is_structural(&self) -> bool {
        STRUCTURAL_ROLES
            .iter()
            .any(|r| r.eq_ignore_ascii_case(&self.role))
    }

    /// 是否有引用 ID (可操作)
    pub fn has_ref(&self) -> bool {
        self.ref_id.is_some()
    }
}

/// AX Tree 快照选项
#[derive(Debug, Clone, Default)]
pub struct SnapshotOptions {
    /// CSS 选择器过滤 (仅返回匹配元素的子树)
    pub selector: Option<String>,
    /// 仅返回交互式元素
    pub interactive_only: bool,
    /// 紧凑模式 (省略结构元素)
    pub compact: bool,
    /// 最大深度
    pub max_depth: Option<usize>,
    /// 包含 URL (link 角色)
    pub include_urls: bool,
}

/// AX Tree 快照结果
#[derive(Debug, Clone)]
pub struct AxSnapshot {
    /// 所有节点
    pub nodes: Vec<AxNode>,
    /// ref ID → 节点索引 映射
    pub ref_map: std::collections::HashMap<String, usize>,
}

impl AxSnapshot {
    /// 从 CDP `Accessibility.getFullAXTree` 响应构建快照
    pub fn from_cdp_response(response: &Value) -> Self {
        let mut nodes = Vec::new();
        let mut ref_map = std::collections::HashMap::new();

        if let Some(axes) = response
            .get("result")
            .and_then(|r| r.get("axTree"))
            .and_then(|t| t.as_array())
        {
            for (idx, ax_value) in axes.iter().enumerate() {
                let node = parse_ax_node(ax_value, idx);
                if let Some(ref_id) = &node.ref_id {
                    ref_map.insert(ref_id.clone(), idx);
                }
                nodes.push(node);
            }
        }

        Self { nodes, ref_map }
    }

    /// 根据 ref ID 获取节点
    pub fn get_by_ref(&self, ref_id: &str) -> Option<&AxNode> {
        self.ref_map
            .get(ref_id)
            .and_then(|&idx| self.nodes.get(idx))
    }

    /// 获取所有交互式元素
    pub fn interactive_nodes(&self) -> Vec<&AxNode> {
        self.nodes.iter().filter(|n| n.is_interactive()).collect()
    }

    /// 根据 role + name 查找节点
    pub fn find_by_role_and_name(&self, role: &str, name: &str) -> Option<&AxNode> {
        self.nodes
            .iter()
            .find(|n| n.role.eq_ignore_ascii_case(role) && n.name.contains(name))
    }

    /// 将快照格式化为可读文本 (用于 AI 消费)
    pub fn to_text(&self, options: &SnapshotOptions) -> String {
        let mut result = String::with_capacity(4096);

        for node in &self.nodes {
            if options.interactive_only && !node.is_interactive() {
                continue;
            }
            if options.compact && node.is_structural() {
                continue;
            }
            if let Some(max_depth) = options.max_depth {
                if node.depth > max_depth {
                    continue;
                }
            }

            let indent = "  ".repeat(node.depth);
            let ref_str = node
                .ref_id
                .as_ref()
                .map(|r| format!("[@{}] ", r))
                .unwrap_or_default();

            result.push_str(&format!(
                "{}{}{}: {}",
                indent, ref_str, node.role, node.name
            ));

            if let Some(level) = node.level {
                result.push_str(&format!(" (h{})", level));
            }
            if let Some(url) = &node.url {
                if options.include_urls {
                    result.push_str(&format!(" → {}", url));
                }
            }
            if let Some(value) = &node.value_text {
                if !value.is_empty() {
                    result.push_str(&format!(" = \"{}\"", value));
                }
            }

            result.push('\n');
        }

        result
    }
}

/// 从 CDP AX 节点 JSON 解析为 AxNode
fn parse_ax_node(value: &Value, idx: usize) -> AxNode {
    let mut node = AxNode::empty();
    node.depth = 0; // 实际深度需要后续计算

    if let Some(role) = value
        .get("role")
        .and_then(|r| r.get("value"))
        .and_then(|v| v.as_str())
    {
        node.role = role.to_string();
    }
    if let Some(name) = value
        .get("name")
        .and_then(|n| n.get("value"))
        .and_then(|v| v.as_str())
    {
        node.name = name.to_string();
    }
    if let Some(level) = value
        .get("level")
        .and_then(|l| l.get("value"))
        .and_then(|v| v.as_i64())
    {
        node.level = Some(level);
    }
    if let Some(checked) = value
        .get("checked")
        .and_then(|c| c.get("value"))
        .and_then(|v| v.as_str())
    {
        node.checked = Some(checked.to_string());
    }
    if let Some(expanded) = value
        .get("expanded")
        .and_then(|e| e.get("value"))
        .and_then(|v| v.as_bool())
    {
        node.expanded = Some(expanded);
    }
    if let Some(disabled) = value
        .get("disabled")
        .and_then(|d| d.get("value"))
        .and_then(|v| v.as_bool())
    {
        node.disabled = Some(disabled);
    }
    if let Some(required) = value
        .get("required")
        .and_then(|r| r.get("value"))
        .and_then(|v| v.as_bool())
    {
        node.required = Some(required);
    }
    if let Some(value_text) = value
        .get("value")
        .and_then(|v| v.get("value"))
        .and_then(|v| v.as_str())
    {
        node.value_text = Some(value_text.to_string());
    }
    if let Some(backend_id) = value.get("backendDOMNodeId").and_then(|v| v.as_i64()) {
        node.backend_node_id = Some(backend_id);
    }

    // ref_id 可以从 frameId 或自定义生成
    // 在实际实现中, agent-browser 通过 session ID + backendNodeId 生成
    if let Some(backend_id) = node.backend_node_id {
        node.ref_id = Some(format!("e{}", backend_id));
    }

    let _ = idx;
    node
}

/// 构建 AX Tree 快照提取 JS (当 CDP Accessibility 域不可用时的 fallback)
pub fn build_snapshot_js() -> String {
    r#"
(() => {
    const result = [];
    const walker = document.createTreeWalker(
        document.body,
        NodeFilter.SHOW_ELEMENT,
        null
    );
    let depth = 0;
    let node;
    while (node = walker.nextNode()) {
        const el = node;
        const role = el.getAttribute('role') || el.tagName.toLowerCase();
        const name = el.getAttribute('aria-label') || el.textContent?.trim()?.substring(0, 100) || '';
        if (role && name) {
            result.push({
                role: role,
                name: name,
                depth: depth,
                tagName: el.tagName.toLowerCase(),
                id: el.id || '',
                className: el.className || '',
            });
        }
    }
    return JSON.stringify(result);
})()
"#.to_string()
}

// ============================================================================
//  纯函数 — 角色分类和匹配
// ============================================================================

/// 判断角色是否为交互式
pub fn is_interactive_role(role: &str) -> bool {
    INTERACTIVE_ROLES
        .iter()
        .any(|r| r.eq_ignore_ascii_case(role))
}

/// 判断角色是否为内容
pub fn is_content_role(role: &str) -> bool {
    CONTENT_ROLES.iter().any(|r| r.eq_ignore_ascii_case(role))
}

/// 判断角色是否为结构
pub fn is_structural_role(role: &str) -> bool {
    STRUCTURAL_ROLES
        .iter()
        .any(|r| r.eq_ignore_ascii_case(role))
}

/// 判断角色是否已知 (在三个列表之一中)
pub fn is_known_role(role: &str) -> bool {
    is_interactive_role(role) || is_content_role(role) || is_structural_role(role)
}

// ============================================================================
//  单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ===== AxNode 测试 =====

    #[test]
    fn test_ax_node_empty() {
        let node = AxNode::empty();
        assert!(node.role.is_empty());
        assert!(node.name.is_empty());
        assert!(!node.has_ref());
    }

    #[test]
    fn test_ax_node_is_interactive_button() {
        let mut node = AxNode::empty();
        node.role = "button".to_string();
        assert!(node.is_interactive());
        assert!(!node.is_content());
        assert!(!node.is_structural());
    }

    #[test]
    fn test_ax_node_is_interactive_link() {
        let mut node = AxNode::empty();
        node.role = "link".to_string();
        assert!(node.is_interactive());
    }

    #[test]
    fn test_ax_node_is_content_heading() {
        let mut node = AxNode::empty();
        node.role = "heading".to_string();
        assert!(node.is_content());
        assert!(!node.is_interactive());
    }

    #[test]
    fn test_ax_node_is_structural_group() {
        let mut node = AxNode::empty();
        node.role = "group".to_string();
        assert!(node.is_structural());
    }

    #[test]
    fn test_ax_node_has_ref() {
        let mut node = AxNode::empty();
        assert!(!node.has_ref());
        node.ref_id = Some("e1".to_string());
        assert!(node.has_ref());
    }

    // ===== 角色分类函数 =====

    #[test]
    fn test_is_interactive_role() {
        assert!(is_interactive_role("button"));
        assert!(is_interactive_role("link"));
        assert!(is_interactive_role("textbox"));
        assert!(is_interactive_role("BUTTON")); // case insensitive
    }

    #[test]
    fn test_is_interactive_role_negative() {
        assert!(!is_interactive_role("heading"));
        assert!(!is_interactive_role("group"));
        assert!(!is_interactive_role("unknown"));
    }

    #[test]
    fn test_is_content_role() {
        assert!(is_content_role("heading"));
        assert!(is_content_role("listitem"));
        assert!(is_content_role("article"));
    }

    #[test]
    fn test_is_structural_role() {
        assert!(is_structural_role("group"));
        assert!(is_structural_role("list"));
        assert!(is_structural_role("table"));
    }

    #[test]
    fn test_is_known_role() {
        assert!(is_known_role("button"));
        assert!(is_known_role("heading"));
        assert!(is_known_role("group"));
        assert!(!is_known_role("unknown_role"));
    }

    // ===== AxSnapshot 测试 =====

    #[test]
    fn test_snapshot_from_cdp_response() {
        let response = json!({
            "result": {
                "axTree": [
                    {
                        "role": {"value": "WebArea"},
                        "name": {"value": "Chat"},
                        "backendDOMNodeId": 1
                    },
                    {
                        "role": {"value": "textbox"},
                        "name": {"value": "Message"},
                        "backendDOMNodeId": 5
                    },
                    {
                        "role": {"value": "button"},
                        "name": {"value": "Send"},
                        "backendDOMNodeId": 10
                    }
                ]
            }
        });

        let snapshot = AxSnapshot::from_cdp_response(&response);
        assert_eq!(snapshot.nodes.len(), 3);
        assert_eq!(snapshot.nodes[0].role, "WebArea");
        assert_eq!(snapshot.nodes[1].role, "textbox");
        assert_eq!(snapshot.nodes[2].role, "button");
        assert_eq!(snapshot.nodes[2].name, "Send");
    }

    #[test]
    fn test_snapshot_from_empty_response() {
        let response = json!({});
        let snapshot = AxSnapshot::from_cdp_response(&response);
        assert!(snapshot.nodes.is_empty());
    }

    #[test]
    fn test_snapshot_get_by_ref() {
        let response = json!({
            "result": {
                "axTree": [
                    {
                        "role": {"value": "button"},
                        "name": {"value": "Submit"},
                        "backendDOMNodeId": 42
                    }
                ]
            }
        });

        let snapshot = AxSnapshot::from_cdp_response(&response);
        let node = snapshot.get_by_ref("e42");
        assert!(node.is_some());
        assert_eq!(node.unwrap().role, "button");
    }

    #[test]
    fn test_snapshot_interactive_nodes() {
        let response = json!({
            "result": {
                "axTree": [
                    {"role": {"value": "heading"}, "name": {"value": "Title"}, "backendDOMNodeId": 1},
                    {"role": {"value": "button"}, "name": {"value": "Click"}, "backendDOMNodeId": 2},
                    {"role": {"value": "textbox"}, "name": {"value": "Input"}, "backendDOMNodeId": 3},
                    {"role": {"value": "article"}, "name": {"value": "Content"}, "backendDOMNodeId": 4}
                ]
            }
        });

        let snapshot = AxSnapshot::from_cdp_response(&response);
        let interactive = snapshot.interactive_nodes();
        assert_eq!(interactive.len(), 2);
    }

    #[test]
    fn test_snapshot_find_by_role_and_name() {
        let response = json!({
            "result": {
                "axTree": [
                    {"role": {"value": "button"}, "name": {"value": "Send Message"}, "backendDOMNodeId": 1}
                ]
            }
        });

        let snapshot = AxSnapshot::from_cdp_response(&response);
        let node = snapshot.find_by_role_and_name("button", "Send");
        assert!(node.is_some());
        assert_eq!(node.unwrap().name, "Send Message");
    }

    #[test]
    fn test_snapshot_find_by_role_and_name_not_found() {
        let response = json!({
            "result": {
                "axTree": [
                    {"role": {"value": "button"}, "name": {"value": "Send"}, "backendDOMNodeId": 1}
                ]
            }
        });

        let snapshot = AxSnapshot::from_cdp_response(&response);
        assert!(snapshot.find_by_role_and_name("button", "Cancel").is_none());
    }

    #[test]
    fn test_snapshot_to_text() {
        let response = json!({
            "result": {
                "axTree": [
                    {"role": {"value": "button"}, "name": {"value": "Send"}, "backendDOMNodeId": 1},
                    {"role": {"value": "textbox"}, "name": {"value": "Message"}, "backendDOMNodeId": 2}
                ]
            }
        });

        let snapshot = AxSnapshot::from_cdp_response(&response);
        let text = snapshot.to_text(&SnapshotOptions::default());
        assert!(text.contains("button"));
        assert!(text.contains("Send"));
        assert!(text.contains("textbox"));
        assert!(text.contains("Message"));
    }

    #[test]
    fn test_snapshot_to_text_interactive_only() {
        let response = json!({
            "result": {
                "axTree": [
                    {"role": {"value": "heading"}, "name": {"value": "Title"}, "backendDOMNodeId": 1},
                    {"role": {"value": "button"}, "name": {"value": "Click"}, "backendDOMNodeId": 2}
                ]
            }
        });

        let snapshot = AxSnapshot::from_cdp_response(&response);
        let options = SnapshotOptions {
            interactive_only: true,
            ..Default::default()
        };
        let text = snapshot.to_text(&options);
        assert!(!text.contains("heading"));
        assert!(text.contains("button"));
    }

    // ===== build_snapshot_js 测试 =====

    #[test]
    fn test_build_snapshot_js_not_empty() {
        let js = build_snapshot_js();
        assert!(!js.is_empty());
    }

    #[test]
    fn test_build_snapshot_js_contains_tree_walker() {
        let js = build_snapshot_js();
        assert!(js.contains("createTreeWalker"));
    }

    #[test]
    fn test_build_snapshot_js_returns_json() {
        let js = build_snapshot_js();
        assert!(js.contains("JSON.stringify"));
    }
}
