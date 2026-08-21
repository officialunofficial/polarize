//! Addressing one element of an accessibility tree by its attributes,
//! rather than by a screen coordinate.
//!
//! `tap` needs a normalized point, so a caller has to run `describe`,
//! read a frame, and do coordinate math before it can press a button.
//! An [`ElementSelector`] replaces that with a stable description of the
//! element itself: its `AXIdentifier`, its role, its label. Resolving one
//! yields an [`ElementPath`] — the child indices to walk from the tree
//! root — which `polarize-macos` follows back down a real `AXUIElement`
//! hierarchy.
//!
//! Everything here is pure and runs against an in-memory [`AxNode`]
//! tree, so it is fully covered by `cargo test -p polarize-core`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ax::AxNode;

/// The child indices to walk from a tree root down to one element. An
/// empty path is the root itself.
pub type ElementPath = Vec<usize>;

/// Describes one element of an accessibility tree by its attributes.
///
/// Every field is optional, and a caller may set several. A node matches
/// only when it satisfies **all** the fields that are set. At least one
/// field must be set — see PINV-15.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct ElementSelector {
    /// Exact match against `AxNode::identifier` (`AXIdentifier`). This is
    /// the most stable field, because it does not change with locale.
    #[serde(default)]
    pub identifier: Option<String>,
    /// Exact match against `AxNode::role`, e.g. `"AXButton"`.
    #[serde(default)]
    pub role: Option<String>,
    /// Exact match against `AxNode::subrole`, e.g. `"AXCloseButton"`.
    #[serde(default)]
    pub subrole: Option<String>,
    /// Exact match against `AxNode::label`.
    #[serde(default)]
    pub label: Option<String>,
    /// Substring match against `AxNode::label`. Case-sensitive.
    #[serde(default)]
    pub label_contains: Option<String>,
    /// When `true`, a disabled element never matches.
    #[serde(default)]
    pub enabled_only: bool,
    /// Which match to take when several nodes match, counted in
    /// pre-order from `0`. Defaults to `0`, the first match.
    #[serde(default)]
    pub index: Option<usize>,
}

impl ElementSelector {
    /// Whether the caller set no criteria at all. See PINV-15.
    ///
    /// `enabled_only` and `index` do not count. Neither one names an
    /// element: `enabled_only` narrows a set of matches, and `index`
    /// picks one out of it. A selector carrying only those two describes
    /// no element at all, so it must fail the guard rather than resolve
    /// to the application root.
    pub fn is_empty(&self) -> bool {
        self.identifier.is_none()
            && self.role.is_none()
            && self.subrole.is_none()
            && self.label.is_none()
            && self.label_contains.is_none()
    }

    /// A short, human-readable rendering, for an error message.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(value) = &self.identifier {
            parts.push(format!("identifier={value:?}"));
        }
        if let Some(value) = &self.role {
            parts.push(format!("role={value:?}"));
        }
        if let Some(value) = &self.subrole {
            parts.push(format!("subrole={value:?}"));
        }
        if let Some(value) = &self.label {
            parts.push(format!("label={value:?}"));
        }
        if let Some(value) = &self.label_contains {
            parts.push(format!("label_contains={value:?}"));
        }
        if self.enabled_only {
            parts.push("enabled_only=true".to_string());
        }
        if let Some(index) = self.index {
            parts.push(format!("index={index}"));
        }
        parts.join(", ")
    }
}

/// Why an [`ElementSelector`] did not resolve to exactly one element.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SelectorError {
    /// The selector set no criteria, so it would match every node.
    #[error(
        "element selector is empty: set at least one of identifier, role, subrole, label, label_contains, enabled_only"
    )]
    Empty,

    /// No node in the tree satisfied the selector.
    #[error("no element matches selector ({selector})")]
    NoMatch { selector: String },

    /// The selector matched, but fewer times than its `index` needs.
    #[error("selector ({selector}) matched {matches} element(s), so index {index} is out of range")]
    IndexOutOfRange {
        selector: String,
        index: usize,
        matches: usize,
    },
}

/// Whether one node satisfies every criterion the selector sets.
/// [`ElementSelector::index`] is not a per-node criterion, so this
/// ignores it.
pub fn matches(node: &AxNode, selector: &ElementSelector) -> bool {
    fn same(actual: Option<&String>, wanted: &Option<String>) -> bool {
        match wanted {
            Some(wanted) => actual.is_some_and(|actual| actual == wanted),
            None => true,
        }
    }

    if !same(node.identifier.as_ref(), &selector.identifier) {
        return false;
    }
    if !same(Some(&node.role), &selector.role) {
        return false;
    }
    if !same(node.subrole.as_ref(), &selector.subrole) {
        return false;
    }
    if !same(node.label.as_ref(), &selector.label) {
        return false;
    }
    if let Some(needle) = &selector.label_contains
        && !node.label.as_ref().is_some_and(|l| l.contains(needle))
    {
        return false;
    }
    if selector.enabled_only && !node.enabled {
        return false;
    }
    true
}

/// # PINV-15: a selector must name a criterion, and resolves in pre-order
///
/// - Always: [`find_all`] and [`find_one`] reject an
///   [`ElementSelector`] that sets no criterion, with
///   [`SelectorError::Empty`]. They return matches in the same
///   pre-order [`crate::ax::flatten`] uses (PINV-3), so
///   [`ElementSelector::index`] always names the same element for the
///   same tree.
/// - Because: a selector resolves to a real press, a real text write, or
///   a real wait. An empty selector would match the application root and
///   silently press whatever sits first in the tree. An unstable order
///   would make `index` name a different element on each call, which is
///   worse than an error, because the caller cannot see it happen.
/// - If violated: a `perform_action` call presses an element the caller
///   never named, and the same request presses a different element on the
///   next run.
pub fn find_all(
    root: &AxNode,
    selector: &ElementSelector,
) -> Result<Vec<ElementPath>, SelectorError> {
    if selector.is_empty() {
        return Err(SelectorError::Empty);
    }
    let mut found = Vec::new();
    let mut path = Vec::new();
    collect(root, selector, &mut path, &mut found);
    Ok(found)
}

fn collect(
    node: &AxNode,
    selector: &ElementSelector,
    path: &mut ElementPath,
    found: &mut Vec<ElementPath>,
) {
    if matches(node, selector) {
        found.push(path.clone());
    }
    for (index, child) in node.children.iter().enumerate() {
        path.push(index);
        collect(child, selector, path, found);
        path.pop();
    }
}

/// The one element `selector` addresses: its `index`-th match in
/// pre-order, or the first match when `index` is `None`. See PINV-15.
pub fn find_one(root: &AxNode, selector: &ElementSelector) -> Result<ElementPath, SelectorError> {
    let found = find_all(root, selector)?;
    if found.is_empty() {
        return Err(SelectorError::NoMatch {
            selector: selector.describe(),
        });
    }
    let index = selector.index.unwrap_or(0);
    found
        .get(index)
        .cloned()
        .ok_or_else(|| SelectorError::IndexOutOfRange {
            selector: selector.describe(),
            index,
            matches: found.len(),
        })
}

/// Walks `path` down from `root`. Returns `None` when any index in
/// `path` is out of range.
pub fn node_at_path<'a>(root: &'a AxNode, path: &[usize]) -> Option<&'a AxNode> {
    let mut node = root;
    for &index in path {
        node = node.children.get(index)?;
    }
    Some(node)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ax::NormalizedFrame;

    fn node(role: &str, label: Option<&str>) -> AxNode {
        AxNode {
            role: role.to_string(),
            label: label.map(str::to_string),
            ..AxNode::default()
        }
    }

    /// AXWindow "Main"
    ///   AXButton "Save"           #save
    ///   AXGroup
    ///     AXButton "Cancel"       (disabled)
    ///     AXButton "Save"
    ///   AXTextField "Name"        <AXSearchField>
    fn tree() -> AxNode {
        AxNode {
            role: "AXWindow".to_string(),
            label: Some("Main".to_string()),
            frame: NormalizedFrame {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
            children: vec![
                AxNode {
                    identifier: Some("save".to_string()),
                    actions: vec!["AXPress".to_string()],
                    ..node("AXButton", Some("Save"))
                },
                AxNode {
                    children: vec![
                        AxNode {
                            enabled: false,
                            ..node("AXButton", Some("Cancel"))
                        },
                        node("AXButton", Some("Save")),
                    ],
                    ..node("AXGroup", None)
                },
                AxNode {
                    subrole: Some("AXSearchField".to_string()),
                    ..node("AXTextField", Some("Name"))
                },
            ],
            ..AxNode::default()
        }
    }

    #[test]
    fn an_empty_selector_is_rejected_rather_than_matching_everything() {
        let selector = ElementSelector::default();
        assert!(selector.is_empty());
        assert_eq!(find_all(&tree(), &selector), Err(SelectorError::Empty));
        assert_eq!(find_one(&tree(), &selector), Err(SelectorError::Empty));
    }

    #[test]
    fn an_index_only_selector_is_still_empty() {
        // `index` picks among matches; it is not itself a criterion.
        let selector = ElementSelector {
            index: Some(2),
            ..ElementSelector::default()
        };
        assert!(selector.is_empty());
        assert_eq!(find_one(&tree(), &selector), Err(SelectorError::Empty));
    }

    #[test]
    fn an_enabled_only_selector_is_still_empty() {
        // `enabled_only` filters matches; it names no element, exactly
        // like `index`. Counting it as a criterion let
        // `{"enabled_only": true}` through the PINV-15 guard and
        // resolved it to the application root.
        let selector = ElementSelector {
            enabled_only: true,
            ..ElementSelector::default()
        };
        assert!(selector.is_empty());
        assert_eq!(find_all(&tree(), &selector), Err(SelectorError::Empty));
        assert_eq!(find_one(&tree(), &selector), Err(SelectorError::Empty));
    }

    #[test]
    fn every_filter_together_is_still_empty() {
        let selector = ElementSelector {
            enabled_only: true,
            index: Some(0),
            ..ElementSelector::default()
        };
        assert!(selector.is_empty());
        assert_eq!(find_one(&tree(), &selector), Err(SelectorError::Empty));
    }

    #[test]
    fn enabled_only_still_filters_once_a_real_criterion_is_named() {
        // The guard must not disable the filter itself.
        let selector = ElementSelector {
            role: Some("AXButton".to_string()),
            enabled_only: true,
            ..ElementSelector::default()
        };
        assert!(!selector.is_empty());
        let with_filter = find_all(&tree(), &selector).unwrap();

        let without = ElementSelector {
            role: Some("AXButton".to_string()),
            ..ElementSelector::default()
        };
        let no_filter = find_all(&tree(), &without).unwrap();
        assert!(
            with_filter.len() < no_filter.len(),
            "the tree needs a disabled AXButton for this test to mean anything"
        );
    }

    #[test]
    fn identifier_resolves_to_one_path() {
        let selector = ElementSelector {
            identifier: Some("save".to_string()),
            ..ElementSelector::default()
        };
        assert_eq!(find_one(&tree(), &selector), Ok(vec![0]));
    }

    #[test]
    fn find_all_returns_paths_in_pre_order() {
        let selector = ElementSelector {
            role: Some("AXButton".to_string()),
            ..ElementSelector::default()
        };
        assert_eq!(
            find_all(&tree(), &selector),
            Ok(vec![vec![0], vec![1, 0], vec![1, 1]])
        );
    }

    #[test]
    fn index_picks_the_nth_match_in_pre_order() {
        let selector = ElementSelector {
            label: Some("Save".to_string()),
            index: Some(1),
            ..ElementSelector::default()
        };
        assert_eq!(find_one(&tree(), &selector), Ok(vec![1, 1]));
    }

    #[test]
    fn omitted_index_means_the_first_match() {
        let selector = ElementSelector {
            label: Some("Save".to_string()),
            ..ElementSelector::default()
        };
        assert_eq!(find_one(&tree(), &selector), Ok(vec![0]));
    }

    #[test]
    fn several_criteria_must_all_hold() {
        let both = ElementSelector {
            role: Some("AXButton".to_string()),
            label: Some("Cancel".to_string()),
            ..ElementSelector::default()
        };
        assert_eq!(find_one(&tree(), &both), Ok(vec![1, 0]));

        let mismatched = ElementSelector {
            role: Some("AXTextField".to_string()),
            label: Some("Cancel".to_string()),
            ..ElementSelector::default()
        };
        assert!(matches!(
            find_one(&tree(), &mismatched),
            Err(SelectorError::NoMatch { .. })
        ));
    }

    #[test]
    fn enabled_only_skips_a_disabled_element() {
        let selector = ElementSelector {
            label: Some("Cancel".to_string()),
            enabled_only: true,
            ..ElementSelector::default()
        };
        assert!(matches!(
            find_one(&tree(), &selector),
            Err(SelectorError::NoMatch { .. })
        ));
    }

    #[test]
    fn label_contains_is_a_substring_match() {
        let selector = ElementSelector {
            label_contains: Some("anc".to_string()),
            ..ElementSelector::default()
        };
        assert_eq!(find_one(&tree(), &selector), Ok(vec![1, 0]));
    }

    #[test]
    fn subrole_matches() {
        let selector = ElementSelector {
            subrole: Some("AXSearchField".to_string()),
            ..ElementSelector::default()
        };
        assert_eq!(find_one(&tree(), &selector), Ok(vec![2]));
    }

    #[test]
    fn the_root_itself_can_match_and_resolves_to_an_empty_path() {
        let selector = ElementSelector {
            role: Some("AXWindow".to_string()),
            ..ElementSelector::default()
        };
        assert_eq!(find_one(&tree(), &selector), Ok(Vec::new()));
    }

    #[test]
    fn an_out_of_range_index_names_how_many_matched() {
        let selector = ElementSelector {
            role: Some("AXButton".to_string()),
            index: Some(9),
            ..ElementSelector::default()
        };
        assert_eq!(
            find_one(&tree(), &selector),
            Err(SelectorError::IndexOutOfRange {
                selector: selector.describe(),
                index: 9,
                matches: 3,
            })
        );
    }

    #[test]
    fn no_match_reports_the_selector_in_its_message() {
        let selector = ElementSelector {
            identifier: Some("nope".to_string()),
            ..ElementSelector::default()
        };
        let err = find_one(&tree(), &selector).unwrap_err();
        assert!(err.to_string().contains("nope"));
    }

    #[test]
    fn node_at_path_walks_child_indices() {
        let tree = tree();
        assert_eq!(node_at_path(&tree, &[]).unwrap().role, "AXWindow");
        assert_eq!(
            node_at_path(&tree, &[1, 0]).unwrap().label.as_deref(),
            Some("Cancel")
        );
        assert!(node_at_path(&tree, &[9]).is_none());
        assert!(node_at_path(&tree, &[0, 0]).is_none());
    }

    #[test]
    fn every_found_path_resolves_back_to_a_matching_node() {
        let tree = tree();
        let selector = ElementSelector {
            role: Some("AXButton".to_string()),
            ..ElementSelector::default()
        };
        for path in find_all(&tree, &selector).unwrap() {
            let node = node_at_path(&tree, &path).expect("path resolves");
            assert!(matches(node, &selector));
        }
    }

    #[test]
    fn selector_round_trips_through_json() {
        let selector = ElementSelector {
            identifier: Some("save".to_string()),
            index: Some(1),
            enabled_only: true,
            ..ElementSelector::default()
        };
        let json = serde_json::to_string(&selector).unwrap();
        assert_eq!(
            serde_json::from_str::<ElementSelector>(&json).unwrap(),
            selector
        );
    }

    #[test]
    fn an_absent_field_deserializes_to_none() {
        let selector: ElementSelector = serde_json::from_str(r#"{"role":"AXButton"}"#).unwrap();
        assert_eq!(selector.role.as_deref(), Some("AXButton"));
        assert_eq!(selector.identifier, None);
        assert_eq!(selector.index, None);
        assert!(!selector.enabled_only);
    }
}
