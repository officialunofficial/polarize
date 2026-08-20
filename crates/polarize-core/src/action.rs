//! The `perform_action` tool: it presses an element through the
//! element's own accessibility action, not through a synthetic click.
//!
//! `tap` posts a click at a coordinate. A click fails when another
//! window covers the element. A click fails when the element is smaller
//! than a click target. A click also makes the caller run `describe`
//! first, to turn an element into a point. `AXUIElementPerformAction`
//! has none of those problems. The caller names the element, and the
//! app performs the action itself.
//!
//! [`perform_element_action`] is pure logic over an in-memory tree. It
//! reads the tree through [`AccessibilityInspector`]. It picks one node
//! with [`crate::selector`]. It checks that node. Then it calls
//! [`ActionPerformer`]. `cargo test -p polarize-core` covers all of
//! that. Only the last call needs a real macOS session.
//!
//! ## Known limitation: two walks of the same tree
//!
//! [`perform_element_action`] reads the tree one time, through
//! `describe`. `polarize-macos` walks the real element hierarchy a
//! second time, to follow the resolved index path. The app can change
//! its interface between the two walks. The path then points at a
//! different element, or at no element. `polarize` does not solve this
//! race. This design holds no live element handle between the two
//! walks. A caller that sees the wrong element must call the tool
//! again. See PINV-18.
//!
//! ## Known limitation: an unread action list reads as "no actions"
//!
//! `describe` reports a failed action-name read as an empty list. See
//! PINV-12 and PINV-16. [`perform_element_action`] refuses an element
//! with an empty list. So a failed read looks the same as an element
//! that offers no action. The error message prints the list the tree
//! carries. The caller can then see which case it hit.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ax::AxNode;
use crate::error::PolarizeError;
use crate::schema::AppIdentifier;
use crate::selector::{self, ElementPath, ElementSelector};
use crate::traits::{AccessibilityInspector, ActionPerformer};

/// The action [`perform_element_action`] uses when a request names
/// none. `AXPress` is the action a button, a menu item, and a checkbox
/// all publish.
pub const DEFAULT_ACTION: &str = "AXPress";

/// A `perform_action` tool call.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct PerformActionRequest {
    /// Which app to inspect. `None` means the frontmost app.
    #[serde(default)]
    pub app: Option<AppIdentifier>,
    /// Which element to act on. See [`crate::selector`] (PINV-15).
    pub selector: ElementSelector,
    /// Which AX action to perform, e.g. `"AXPress"` or `"AXShowMenu"`.
    /// `None` means [`DEFAULT_ACTION`].
    #[serde(default)]
    pub action: Option<String>,
}

/// The result of a `perform_action` tool call.
///
/// The response repeats the resolved element, because a selector can
/// match more than one node. The caller reads `path`, `role`, and
/// `label` to confirm the tool acted on the element it meant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PerformActionResponse {
    /// Always `true`. A failed action returns an error instead.
    pub performed: bool,
    /// The action the tool performed, after the default was applied.
    pub action: String,
    /// The child indices the selector resolved to, from the tree root.
    pub path: ElementPath,
    /// The resolved element's `AXRole`.
    pub role: String,
    /// The resolved element's label, when it has one.
    pub label: Option<String>,
}

/// Why `perform_action` refused to act on the element it resolved.
///
/// Each variant is a refusal before any native call. None of them
/// reports a native failure. `polarize-macos` reports those through
/// [`PolarizeError::Platform`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ActionError {
    /// The element does not publish the action the caller asked for.
    #[error("element ({element}) does not offer action {action:?}; it offers [{offered}]")]
    UnsupportedAction {
        element: String,
        action: String,
        offered: String,
    },

    /// The app reports the element as disabled.
    #[error("element ({element}) is disabled, so it cannot perform action {action:?}")]
    Disabled { element: String, action: String },

    /// A resolved path did not read back to a node of the same tree.
    #[error("element path {path:?} does not resolve to a node")]
    PathNotResolved { path: ElementPath },
}

/// Reports a refusal as a [`PolarizeError`].
///
/// `polarize-core` owns no variant for a refusal yet, so a refusal
/// travels as [`PolarizeError::Platform`]. Replace this impl with an
/// `#[error(transparent)] Action(#[from] ActionError)` variant on
/// `PolarizeError` when `error.rs` can take one.
/// A short rendering of one node, for an error message.
fn describe_node(node: &AxNode) -> String {
    let mut parts = vec![format!("role={:?}", node.role)];
    if let Some(label) = &node.label {
        parts.push(format!("label={label:?}"));
    }
    if let Some(identifier) = &node.identifier {
        parts.push(format!("identifier={identifier:?}"));
    }
    parts.join(", ")
}

/// # PINV-17: `perform_action` checks the element before it acts
///
/// - Always: [`perform_element_action`] resolves the element, then
///   refuses in two cases. It refuses when the element's `actions` list
///   does not hold the chosen action. It refuses when the element's
///   `enabled` flag is `false`. A refusal returns [`ActionError`] and
///   never calls [`ActionPerformer::perform_action_at_path`].
/// - Because: `AXUIElementPerformAction` has no timeout of its own.
///   Some apps block the caller for the full AX timeout on an action
///   the element does not handle. A greyed-out control is the common
///   case: it still publishes `AXPress`, and it still answers, but it
///   does nothing. Both cases look like a hang, or like a silent
///   success, to an agent that cannot see the screen. The tree already
///   carries both facts, so this check costs no extra native call.
/// - If violated: a `perform_action` call blocks the whole MCP server
///   until the AX timeout expires. Or it reports success for a control
///   the app never let the user press.
pub fn perform_element_action<A, P>(
    inspector: &A,
    performer: &P,
    request: &PerformActionRequest,
) -> Result<PerformActionResponse, PolarizeError>
where
    A: AccessibilityInspector,
    P: ActionPerformer,
{
    let (_app_name, root) = inspector.describe(request.app.as_ref())?;
    let path = selector::find_one(&root, &request.selector)?;
    let node = selector::node_at_path(&root, &path)
        .ok_or_else(|| ActionError::PathNotResolved { path: path.clone() })?;
    let action = request.action.as_deref().unwrap_or(DEFAULT_ACTION);

    if !node.actions.iter().any(|offered| offered == action) {
        return Err(ActionError::UnsupportedAction {
            element: describe_node(node),
            action: action.to_string(),
            offered: node.actions.join(", "),
        }
        .into());
    }
    if !node.enabled {
        return Err(ActionError::Disabled {
            element: describe_node(node),
            action: action.to_string(),
        }
        .into());
    }

    performer.perform_action_at_path(request.app.as_ref(), &path, action)?;

    Ok(PerformActionResponse {
        performed: true,
        action: action.to_string(),
        path,
        role: node.role.clone(),
        label: node.label.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    // ---- fakes -------------------------------------------------------

    struct FakeInspector {
        root: AxNode,
        seen: RefCell<Vec<Option<AppIdentifier>>>,
    }

    impl FakeInspector {
        fn new(root: AxNode) -> Self {
            Self {
                root,
                seen: RefCell::new(Vec::new()),
            }
        }
    }

    impl AccessibilityInspector for FakeInspector {
        fn describe(&self, app: Option<&AppIdentifier>) -> Result<(String, AxNode), PolarizeError> {
            self.seen.borrow_mut().push(app.cloned());
            Ok(("TestApp".to_string(), self.root.clone()))
        }
    }

    /// Records every call, so a test can prove the exact path and the
    /// exact action string reached the platform layer.
    #[derive(Default)]
    struct RecordingPerformer {
        calls: RefCell<Vec<(Option<AppIdentifier>, ElementPath, String)>>,
        fail_with: Option<String>,
    }

    impl RecordingPerformer {
        fn failing(message: &str) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                fail_with: Some(message.to_string()),
            }
        }
    }

    impl ActionPerformer for RecordingPerformer {
        fn perform_action_at_path(
            &self,
            app: Option<&AppIdentifier>,
            path: &[usize],
            action: &str,
        ) -> Result<(), PolarizeError> {
            self.calls
                .borrow_mut()
                .push((app.cloned(), path.to_vec(), action.to_string()));
            match &self.fail_with {
                Some(message) => Err(PolarizeError::Platform(message.clone())),
                None => Ok(()),
            }
        }
    }

    // ---- test tree ---------------------------------------------------

    fn button(label: &str, actions: &[&str], enabled: bool) -> AxNode {
        AxNode {
            role: "AXButton".to_string(),
            label: Some(label.to_string()),
            actions: actions.iter().map(|a| a.to_string()).collect(),
            interactive: !actions.is_empty(),
            enabled,
            ..AxNode::default()
        }
    }

    /// AXWindow "Main"                       {AXRaise}
    ///   AXButton "Save"        #save        {AXPress}
    ///   AXGroup
    ///     AXButton "Cancel"    (disabled)   {AXPress}
    ///     AXButton "Options"                {AXPress,AXShowMenu}
    ///   AXStaticText "Ready"                {}
    fn tree() -> AxNode {
        AxNode {
            role: "AXWindow".to_string(),
            label: Some("Main".to_string()),
            actions: vec!["AXRaise".to_string()],
            children: vec![
                AxNode {
                    identifier: Some("save".to_string()),
                    ..button("Save", &["AXPress"], true)
                },
                AxNode {
                    role: "AXGroup".to_string(),
                    children: vec![
                        button("Cancel", &["AXPress"], false),
                        button("Options", &["AXPress", "AXShowMenu"], true),
                    ],
                    ..AxNode::default()
                },
                AxNode {
                    role: "AXStaticText".to_string(),
                    label: Some("Ready".to_string()),
                    ..AxNode::default()
                },
            ],
            ..AxNode::default()
        }
    }

    fn request(selector: ElementSelector) -> PerformActionRequest {
        PerformActionRequest {
            app: None,
            selector,
            action: None,
        }
    }

    fn by_label(label: &str) -> ElementSelector {
        ElementSelector {
            label: Some(label.to_string()),
            ..ElementSelector::default()
        }
    }

    // ---- happy path ---------------------------------------------------

    #[test]
    fn performs_the_named_action_and_reports_the_resolved_element() {
        let inspector = FakeInspector::new(tree());
        let performer = RecordingPerformer::default();
        let request = PerformActionRequest {
            app: None,
            selector: by_label("Options"),
            action: Some("AXShowMenu".to_string()),
        };

        let response = perform_element_action(&inspector, &performer, &request).unwrap();

        assert_eq!(
            response,
            PerformActionResponse {
                performed: true,
                action: "AXShowMenu".to_string(),
                path: vec![1, 1],
                role: "AXButton".to_string(),
                label: Some("Options".to_string()),
            }
        );
    }

    #[test]
    fn the_performer_receives_the_exact_path_and_action_that_were_resolved() {
        // PINV-18: the index path core resolves is the index path the
        // platform layer walks. Nothing may rewrite it in between.
        let inspector = FakeInspector::new(tree());
        let performer = RecordingPerformer::default();
        let request = PerformActionRequest {
            app: None,
            selector: by_label("Options"),
            action: Some("AXShowMenu".to_string()),
        };

        perform_element_action(&inspector, &performer, &request).unwrap();

        assert_eq!(
            performer.calls.borrow().as_slice(),
            &[(None, vec![1, 1], "AXShowMenu".to_string())]
        );
    }

    #[test]
    fn the_resolved_path_reads_back_to_the_element_the_response_names() {
        let root = tree();
        let inspector = FakeInspector::new(root.clone());
        let performer = RecordingPerformer::default();

        let response =
            perform_element_action(&inspector, &performer, &request(by_label("Options"))).unwrap();
        let node = selector::node_at_path(&root, &response.path).unwrap();
        assert_eq!(node.role, response.role);
        assert_eq!(node.label, response.label);
    }

    #[test]
    fn an_omitted_action_defaults_to_ax_press() {
        let inspector = FakeInspector::new(tree());
        let performer = RecordingPerformer::default();

        let response =
            perform_element_action(&inspector, &performer, &request(by_label("Save"))).unwrap();

        assert_eq!(response.action, DEFAULT_ACTION);
        assert_eq!(performer.calls.borrow()[0].2, "AXPress");
    }

    #[test]
    fn the_root_element_resolves_to_an_empty_path() {
        let inspector = FakeInspector::new(tree());
        let performer = RecordingPerformer::default();
        let request = PerformActionRequest {
            app: None,
            selector: ElementSelector {
                role: Some("AXWindow".to_string()),
                ..ElementSelector::default()
            },
            action: Some("AXRaise".to_string()),
        };

        let response = perform_element_action(&inspector, &performer, &request).unwrap();

        assert_eq!(response.path, Vec::<usize>::new());
        assert_eq!(performer.calls.borrow()[0].1, Vec::<usize>::new());
    }

    #[test]
    fn the_app_identifier_reaches_both_the_inspector_and_the_performer() {
        let inspector = FakeInspector::new(tree());
        let performer = RecordingPerformer::default();
        let app = AppIdentifier {
            bundle_id: Some("com.apple.TextEdit".to_string()),
            app_name: None,
        };
        let request = PerformActionRequest {
            app: Some(app.clone()),
            selector: by_label("Save"),
            action: None,
        };

        perform_element_action(&inspector, &performer, &request).unwrap();

        assert_eq!(inspector.seen.borrow().as_slice(), &[Some(app.clone())]);
        assert_eq!(performer.calls.borrow()[0].0, Some(app));
    }

    // ---- PINV-17 refusals ---------------------------------------------

    #[test]
    fn an_action_the_element_does_not_offer_is_refused_before_the_platform_runs() {
        let inspector = FakeInspector::new(tree());
        let performer = RecordingPerformer::default();
        let request = PerformActionRequest {
            app: None,
            selector: by_label("Save"),
            action: Some("AXShowMenu".to_string()),
        };

        let err = perform_element_action(&inspector, &performer, &request).unwrap_err();

        let message = err.to_string();
        assert!(message.contains("AXShowMenu"), "{message}");
        assert!(
            message.contains("AXPress"),
            "names the real list: {message}"
        );
        assert!(message.contains("Save"), "names the element: {message}");
        assert!(
            performer.calls.borrow().is_empty(),
            "the platform must not be called on a refusal"
        );
    }

    #[test]
    fn an_element_that_offers_no_action_at_all_is_refused() {
        let inspector = FakeInspector::new(tree());
        let performer = RecordingPerformer::default();

        let err = perform_element_action(&inspector, &performer, &request(by_label("Ready")))
            .unwrap_err();

        assert!(err.to_string().contains("it offers []"), "{err}");
        assert!(performer.calls.borrow().is_empty());
    }

    #[test]
    fn a_disabled_element_is_refused_before_the_platform_runs() {
        let inspector = FakeInspector::new(tree());
        let performer = RecordingPerformer::default();

        let err = perform_element_action(&inspector, &performer, &request(by_label("Cancel")))
            .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("disabled"), "{message}");
        assert!(message.contains("Cancel"), "names the element: {message}");
        assert!(
            performer.calls.borrow().is_empty(),
            "the platform must not be called on a disabled element"
        );
    }

    #[test]
    fn the_refusal_errors_render_the_element_and_the_action() {
        let unsupported = ActionError::UnsupportedAction {
            element: "role=\"AXButton\", label=\"Save\"".to_string(),
            action: "AXShowMenu".to_string(),
            offered: "AXPress".to_string(),
        };
        assert_eq!(
            unsupported.to_string(),
            "element (role=\"AXButton\", label=\"Save\") does not offer action \"AXShowMenu\"; it offers [AXPress]"
        );

        let disabled = ActionError::Disabled {
            element: "role=\"AXButton\", label=\"Cancel\"".to_string(),
            action: "AXPress".to_string(),
        };
        assert_eq!(
            disabled.to_string(),
            "element (role=\"AXButton\", label=\"Cancel\") is disabled, so it cannot perform action \"AXPress\""
        );
    }

    #[test]
    fn a_refusal_travels_as_its_own_error_variant() {
        // A refusal is neither a caller mistake nor a platform failure.
        // It reports what the element itself does not allow.
        let err: PolarizeError = ActionError::Disabled {
            element: "role=\"AXButton\"".to_string(),
            action: "AXPress".to_string(),
        }
        .into();
        assert!(matches!(err, PolarizeError::Action(_)), "{err}");
        assert!(err.to_string().contains("AXPress"));
    }

    // ---- selector failures ---------------------------------------------

    #[test]
    fn a_selector_that_matches_nothing_reports_the_selector_error() {
        let inspector = FakeInspector::new(tree());
        let performer = RecordingPerformer::default();

        let err =
            perform_element_action(&inspector, &performer, &request(by_label("Nope"))).unwrap_err();

        assert!(matches!(err, PolarizeError::Selector(_)), "{err}");
        assert!(err.to_string().contains("Nope"), "{err}");
        assert!(performer.calls.borrow().is_empty());
    }

    #[test]
    fn an_empty_selector_is_refused_by_the_selector_module() {
        let inspector = FakeInspector::new(tree());
        let performer = RecordingPerformer::default();

        let err =
            perform_element_action(&inspector, &performer, &request(ElementSelector::default()))
                .unwrap_err();

        assert!(matches!(err, PolarizeError::Selector(_)), "{err}");
        assert!(performer.calls.borrow().is_empty());
    }

    #[test]
    fn an_index_picks_among_several_matching_elements() {
        let inspector = FakeInspector::new(tree());
        let performer = RecordingPerformer::default();
        let request = PerformActionRequest {
            app: None,
            selector: ElementSelector {
                role: Some("AXButton".to_string()),
                index: Some(2),
                ..ElementSelector::default()
            },
            action: None,
        };

        let response = perform_element_action(&inspector, &performer, &request).unwrap();

        assert_eq!(response.path, vec![1, 1]);
        assert_eq!(response.label.as_deref(), Some("Options"));
    }

    // ---- error propagation ----------------------------------------------

    #[test]
    fn a_platform_failure_from_the_performer_reaches_the_caller() {
        let inspector = FakeInspector::new(tree());
        let performer = RecordingPerformer::failing("AXUIElementPerformAction failed");

        let err =
            perform_element_action(&inspector, &performer, &request(by_label("Save"))).unwrap_err();

        assert!(
            err.to_string().contains("AXUIElementPerformAction failed"),
            "{err}"
        );
    }

    #[test]
    fn a_describe_failure_stops_the_call_before_any_action() {
        struct FailingInspector;
        impl AccessibilityInspector for FailingInspector {
            fn describe(
                &self,
                _app: Option<&AppIdentifier>,
            ) -> Result<(String, AxNode), PolarizeError> {
                Err(PolarizeError::AppNotFound("com.example.Nope".to_string()))
            }
        }
        let performer = RecordingPerformer::default();

        let err = perform_element_action(&FailingInspector, &performer, &request(by_label("Save")))
            .unwrap_err();

        assert!(matches!(err, PolarizeError::AppNotFound(_)), "{err}");
        assert!(performer.calls.borrow().is_empty());
    }

    // ---- wire contract ----------------------------------------------------

    #[test]
    fn the_request_round_trips_through_json() {
        let request = PerformActionRequest {
            app: Some(AppIdentifier {
                bundle_id: Some("com.apple.TextEdit".to_string()),
                app_name: None,
            }),
            selector: by_label("Save"),
            action: Some("AXPress".to_string()),
        };
        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(
            serde_json::from_str::<PerformActionRequest>(&json).unwrap(),
            request
        );
    }

    #[test]
    fn a_request_with_only_a_selector_deserializes() {
        let request: PerformActionRequest =
            serde_json::from_str(r#"{"selector":{"identifier":"save"}}"#).unwrap();
        assert_eq!(request.app, None);
        assert_eq!(request.action, None);
        assert_eq!(request.selector.identifier.as_deref(), Some("save"));
    }

    #[test]
    fn the_response_round_trips_through_json() {
        let response = PerformActionResponse {
            performed: true,
            action: "AXPress".to_string(),
            path: vec![1, 0],
            role: "AXButton".to_string(),
            label: Some("Save".to_string()),
        };
        let json = serde_json::to_string(&response).unwrap();
        assert_eq!(
            serde_json::from_str::<PerformActionResponse>(&json).unwrap(),
            response
        );
    }
}
