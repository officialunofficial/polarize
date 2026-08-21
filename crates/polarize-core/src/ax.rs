//! The accessibility-tree data model: a serde-serializable tree of
//! [`AxNode`]s, plus pure flattening/formatting used to build the
//! `describe` MCP tool's response.
//!
//! `polarize-macos` builds an [`AxNode`] tree by walking a real
//! `AXUIElement` hierarchy; everything in this module operates on the
//! resulting in-memory tree and needs no real accessibility session to
//! run or test.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A node's on-screen position and size, normalized to `[0.0, 1.0]`
/// fractions of its containing screen/window — the same convention
/// [`crate::coords`] uses for tap points.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NormalizedFrame {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// One node of an accessibility tree.
///
/// Derives [`JsonSchema`] (in addition to `Serialize`/`Deserialize`)
/// because it appears in [`crate::schema::DescribeResponse`], whose JSON
/// schema `apps/polarize` hands to `rmcp`'s `#[tool]` macro for the
/// `describe` tool's structured output.
///
/// ## Why the extra attributes carry `#[serde(default)]`
///
/// `role`, `label`, `frame`, `focusable`, and `interactive` are the
/// original five fields. The rest came later (see PINV-16). Each new
/// field defaults, so a `describe` response captured by an older client
/// still deserializes into this type. A missing attribute is a normal
/// result, not an error: many AX elements expose none of `AXSubrole`,
/// `AXIdentifier`, or `AXHelp`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AxNode {
    /// The AX role, e.g. `"AXButton"`, `"AXWindow"`, `"AXStaticText"`.
    pub role: String,
    /// The element's title/label/value, when it has one.
    pub label: Option<String>,
    pub frame: NormalizedFrame,
    /// Whether the element can receive keyboard focus.
    pub focusable: bool,
    /// Whether the element accepts a click/press action (a button, a
    /// menu item, a text field — as opposed to a purely decorative or
    /// read-only element). Equivalent to a non-empty [`Self::actions`].
    pub interactive: bool,
    /// `AXEnabled`. A greyed-out control reports `false`. Defaults to
    /// `true`, because an element that does not publish `AXEnabled` is
    /// not a disabled element — see [`AxNode::default`].
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// `AXSubrole`, e.g. `"AXCloseButton"` or `"AXSearchField"`. It
    /// separates elements that share one role.
    #[serde(default)]
    pub subrole: Option<String>,
    /// `AXRoleDescription`: the localized, human-readable role name the
    /// app itself publishes, e.g. `"button"`.
    #[serde(default)]
    pub role_description: Option<String>,
    /// `AXIdentifier`: the stable developer-assigned id. This is the
    /// best selector to address an element by, because it does not
    /// change with locale or layout.
    #[serde(default)]
    pub identifier: Option<String>,
    /// `AXHelp`: the element's tooltip or help text.
    #[serde(default)]
    pub help: Option<String>,
    /// The element's AX action names, e.g. `["AXPress"]`. A caller
    /// passes one of these to the `perform_action` tool.
    #[serde(default)]
    pub actions: Vec<String>,
    pub children: Vec<AxNode>,
}

fn default_true() -> bool {
    true
}

/// A node with no attributes read yet.
///
/// `enabled` starts `true`, not `false`. The rest of this crate treats
/// `enabled` as "the app said this control is off", so an unread or
/// absent `AXEnabled` must not look like a disabled control.
impl Default for AxNode {
    fn default() -> Self {
        Self {
            role: String::new(),
            label: None,
            frame: NormalizedFrame::default(),
            focusable: false,
            interactive: false,
            enabled: true,
            subrole: None,
            role_description: None,
            identifier: None,
            help: None,
            actions: Vec::new(),
            children: Vec::new(),
        }
    }
}

/// One node of an [`AxNode`] tree, flattened to a single pre-order
/// sequence with its depth attached, for building a `describe` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlatAxNode {
    /// Depth from the tree root; the root itself is depth `0`.
    pub depth: usize,
    pub role: String,
    pub label: Option<String>,
    pub frame: NormalizedFrame,
    pub focusable: bool,
    pub interactive: bool,
    pub enabled: bool,
    pub subrole: Option<String>,
    pub role_description: Option<String>,
    pub identifier: Option<String>,
    pub help: Option<String>,
    pub actions: Vec<String>,
}

/// # PINV-3: flatten is pre-order and depth-accurate
///
/// - Always: [`flatten`] visits `root` before any of its descendants. It
///   visits a node's children in their original order. It records each
///   node's depth as the number of ancestors above it, so the root is
///   depth `0`.
/// - Because: [`crate::orchestrate::perform_describe`] embeds
///   [`format_tree`]'s rendering directly in `DescribeResponse::formatted`
///   (see `crate::schema`'s PINV-9). `format_tree` renders through
///   `flatten`, so depth must be correct for its indentation to make
///   sense, and pre-order must hold for a subtree to read as one
///   contiguous run. A traversal bug could reorder children or miscount
///   depth without ever producing an error.
/// - If violated: `describe`'s `formatted` output renders as a flat or
///   mis-indented list. A reader cannot tell which elements nest inside
///   which container.
pub fn flatten(root: &AxNode) -> Vec<FlatAxNode> {
    let mut out = Vec::new();
    flatten_into(root, 0, &mut out);
    out
}

fn flatten_into(node: &AxNode, depth: usize, out: &mut Vec<FlatAxNode>) {
    out.push(FlatAxNode {
        depth,
        role: node.role.clone(),
        label: node.label.clone(),
        frame: node.frame,
        focusable: node.focusable,
        interactive: node.interactive,
        enabled: node.enabled,
        subrole: node.subrole.clone(),
        role_description: node.role_description.clone(),
        identifier: node.identifier.clone(),
        help: node.help.clone(),
        actions: node.actions.clone(),
    });
    for child in &node.children {
        flatten_into(child, depth + 1, out);
    }
}

/// Renders an [`AxNode`] tree as one indented line per node, mirroring
/// argent's own `describe` text rendering: role, label, frame, and
/// interactivity flags, indented two spaces per depth level.
///
/// ## What a line leaves out
///
/// A line shows `subrole`, `identifier`, and `actions`, because a caller
/// selects an element by those. It omits `role_description` and `help`,
/// which are prose and make a line too long to scan. Both stay available
/// in `DescribeResponse::root`.
pub fn format_tree(root: &AxNode) -> String {
    flatten(root)
        .iter()
        .map(format_flat_node)
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_flat_node(node: &FlatAxNode) -> String {
    let indent = "  ".repeat(node.depth);
    let label = node.label.as_deref().unwrap_or("");
    let mut flags = Vec::new();
    if node.focusable {
        flags.push("focusable");
    }
    if node.interactive {
        flags.push("interactive");
    }
    if !node.enabled {
        flags.push("disabled");
    }
    let flags = if flags.is_empty() {
        String::new()
    } else {
        format!(" [{}]", flags.join(","))
    };
    let subrole = match &node.subrole {
        Some(subrole) => format!(" <{subrole}>"),
        None => String::new(),
    };
    let identifier = match &node.identifier {
        Some(identifier) => format!(" #{identifier}"),
        None => String::new(),
    };
    let actions = if node.actions.is_empty() {
        String::new()
    } else {
        format!(" {{{}}}", node.actions.join(","))
    };
    format!(
        "{indent}{role}{subrole} \"{label}\"{identifier} ({x:.2},{y:.2},{w:.2},{h:.2}){flags}{actions}",
        role = node.role,
        x = node.frame.x,
        y = node.frame.y,
        w = node.frame.width,
        h = node.frame.height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(role: &str, label: &str) -> AxNode {
        AxNode {
            role: role.to_string(),
            label: Some(label.to_string()),
            frame: NormalizedFrame {
                x: 0.0,
                y: 0.0,
                width: 0.1,
                height: 0.05,
            },
            ..AxNode::default()
        }
    }

    fn sample_tree() -> AxNode {
        AxNode {
            role: "AXWindow".to_string(),
            label: Some("Untitled".to_string()),
            frame: NormalizedFrame {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
            children: vec![
                AxNode {
                    role: "AXButton".to_string(),
                    label: Some("Save".to_string()),
                    frame: NormalizedFrame {
                        x: 0.1,
                        y: 0.2,
                        width: 0.15,
                        height: 0.05,
                    },
                    focusable: true,
                    interactive: true,
                    actions: vec!["AXPress".to_string()],
                    ..AxNode::default()
                },
                AxNode {
                    role: "AXGroup".to_string(),
                    label: None,
                    frame: NormalizedFrame {
                        x: 0.0,
                        y: 0.3,
                        width: 1.0,
                        height: 0.6,
                    },
                    children: vec![leaf("AXStaticText", "Hello")],
                    ..AxNode::default()
                },
            ],
            ..AxNode::default()
        }
    }

    #[test]
    fn flatten_single_node_has_depth_zero() {
        let node = leaf("AXButton", "OK");
        let flat = flatten(&node);
        assert_eq!(flat.len(), 1);
        assert_eq!(flat[0].depth, 0);
        assert_eq!(flat[0].role, "AXButton");
    }

    #[test]
    fn flatten_is_pre_order_with_correct_depths() {
        let flat = flatten(&sample_tree());
        let roles_and_depths: Vec<(&str, usize)> =
            flat.iter().map(|n| (n.role.as_str(), n.depth)).collect();
        assert_eq!(
            roles_and_depths,
            vec![
                ("AXWindow", 0),
                ("AXButton", 1),
                ("AXGroup", 1),
                ("AXStaticText", 2),
            ]
        );
    }

    #[test]
    fn flatten_preserves_children_order() {
        let mut tree = sample_tree();
        // Swap the two top-level children and confirm flatten follows.
        tree.children.swap(0, 1);
        let flat = flatten(&tree);
        assert_eq!(flat[1].role, "AXGroup");
        assert_eq!(flat[2].role, "AXStaticText");
        assert_eq!(flat[3].role, "AXButton");
    }

    #[test]
    fn flatten_carries_frame_and_flags_through() {
        let flat = flatten(&sample_tree());
        let save_button = &flat[1];
        assert_eq!(save_button.label.as_deref(), Some("Save"));
        assert!(save_button.focusable);
        assert!(save_button.interactive);
        assert_eq!(
            save_button.frame,
            NormalizedFrame {
                x: 0.1,
                y: 0.2,
                width: 0.15,
                height: 0.05
            }
        );
    }

    #[test]
    fn flatten_carries_the_enriched_attributes_through() {
        let node = AxNode {
            role: "AXButton".to_string(),
            enabled: false,
            subrole: Some("AXCloseButton".to_string()),
            role_description: Some("close button".to_string()),
            identifier: Some("close".to_string()),
            help: Some("Close this window".to_string()),
            actions: vec!["AXPress".to_string(), "AXShowMenu".to_string()],
            ..AxNode::default()
        };
        let flat = flatten(&node);
        assert!(!flat[0].enabled);
        assert_eq!(flat[0].subrole.as_deref(), Some("AXCloseButton"));
        assert_eq!(flat[0].role_description.as_deref(), Some("close button"));
        assert_eq!(flat[0].identifier.as_deref(), Some("close"));
        assert_eq!(flat[0].help.as_deref(), Some("Close this window"));
        assert_eq!(flat[0].actions, vec!["AXPress", "AXShowMenu"]);
    }

    #[test]
    fn format_tree_indents_by_depth_and_shows_flags() {
        let text = format_tree(&sample_tree());
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0], "AXWindow \"Untitled\" (0.00,0.00,1.00,1.00)");
        assert_eq!(
            lines[1],
            "  AXButton \"Save\" (0.10,0.20,0.15,0.05) [focusable,interactive] {AXPress}"
        );
        assert_eq!(lines[2], "  AXGroup \"\" (0.00,0.30,1.00,0.60)");
        assert_eq!(lines[3], "    AXStaticText \"Hello\" (0.00,0.00,0.10,0.05)");
    }

    #[test]
    fn format_tree_shows_subrole_identifier_and_disabled_flag() {
        let node = AxNode {
            role: "AXButton".to_string(),
            label: Some("Close".to_string()),
            enabled: false,
            subrole: Some("AXCloseButton".to_string()),
            identifier: Some("close-btn".to_string()),
            actions: vec!["AXPress".to_string()],
            ..AxNode::default()
        };
        assert_eq!(
            format_tree(&node),
            "AXButton <AXCloseButton> \"Close\" #close-btn (0.00,0.00,0.00,0.00) [disabled] {AXPress}"
        );
    }

    #[test]
    fn format_tree_omits_role_description_and_help() {
        let node = AxNode {
            role: "AXButton".to_string(),
            label: Some("Save".to_string()),
            role_description: Some("button".to_string()),
            help: Some("Saves the document".to_string()),
            ..AxNode::default()
        };
        let line = format_tree(&node);
        assert!(!line.contains("button"));
        assert!(!line.contains("Saves the document"));
    }

    #[test]
    fn default_node_is_enabled() {
        assert!(AxNode::default().enabled);
    }

    #[test]
    fn a_response_without_the_enriched_attributes_still_deserializes() {
        // The exact shape `describe` returned before PINV-16 landed.
        let json = r#"{
            "role": "AXButton",
            "label": "Save",
            "frame": {"x": 0.0, "y": 0.0, "width": 0.1, "height": 0.1},
            "focusable": true,
            "interactive": true,
            "children": []
        }"#;
        let node: AxNode = serde_json::from_str(json).expect("deserialize");
        assert_eq!(node.role, "AXButton");
        assert!(
            node.enabled,
            "a missing AXEnabled must not read as disabled"
        );
        assert_eq!(node.subrole, None);
        assert!(node.actions.is_empty());
    }
}
