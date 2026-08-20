//! The `set_value` tool: it writes one accessibility attribute
//! directly, instead of typing the value key by key.
//!
//! `keyboard` types a string as synthetic key events. That path has two
//! weaknesses. It needs the target element to hold keyboard focus. A
//! focus change between the two calls sends the text elsewhere. It also
//! needs a keycode for every character, and `polarize-macos`'s `keymap`
//! hardcodes a US-ANSI-like map. A caller on another keyboard layout
//! gets the wrong characters.
//!
//! `AXUIElementSetAttributeValue` avoids both problems. The caller names
//! the element, and the app writes the value itself. `set_value` writes
//! three payloads: a string into `AXValue`, a number into `AXValue`, and
//! a caret or selection into `AXSelectedTextRange`.
//!
//! [`set_element_value`] is pure logic over an in-memory tree. It reads
//! the tree through [`AccessibilityInspector`]. It picks one node with
//! [`crate::selector`]. It checks that node. Then it calls
//! [`ValueSetter`]. `cargo test -p polarize-core` covers all of that.
//! Only the last call needs a real macOS session.
//!
//! ## Risk: web content accepts the write and fires no event
//!
//! A native AppKit control handles an AX write like a user edit. Web
//! content often does not. A `WKWebView`, an Electron app, and a React
//! app each accept the write into the DOM node. The DOM value changes,
//! and no `input` or `keydown` event fires. The page's own JavaScript
//! never learns about the edit. A controlled React input shows the new
//! text, then snaps back to the value in its state. A form stays
//! invalid, and a submit button stays disabled.
//!
//! `polarize` cannot detect this case, and it cannot repair it. The
//! write itself reports success, because the app really did accept it.
//! See PINV-27.
//!
//! ## House rule: which tool to use
//!
//! - A toggle, a button, a checkbox, or a menu item: use
//!   `perform_action` with `AXPress`. The app runs its own handler.
//! - Text and numbers: use `set_value`. It is fast, and it does not
//!   depend on focus or on the keyboard layout.
//! - Keystroke fidelity: use `keyboard`. Type the text when the app
//!   must see every key event, as web content usually must.
//!
//! ## Known limitation: two walks of the same tree
//!
//! [`set_element_value`] reads the tree one time, through `describe`.
//! `polarize-macos` walks the real element hierarchy a second time, to
//! follow the resolved index path. The app can change its interface
//! between the two walks. See PINV-18, and `crate::action`'s note on
//! the same race.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ax::AxNode;
use crate::error::PolarizeError;
use crate::schema::AppIdentifier;
use crate::selector::{self, ElementPath, ElementSelector};
use crate::traits::{AccessibilityInspector, ResolvedApp, ValueSetter};

/// The attribute a text or number payload writes. `AXValue` holds the
/// text of a text field, and the number of a slider or a stepper.
pub const VALUE_ATTRIBUTE: &str = "AXValue";

/// The attribute a range payload writes. It holds the caret position
/// and the selection length of a text element.
pub const SELECTED_TEXT_RANGE_ATTRIBUTE: &str = "AXSelectedTextRange";

/// The roles that accept a string write into `AXValue`.
///
/// The list is deliberately short. Every role on it is a text-entry
/// control of AppKit, of a web view, or of both. `AXStaticText` is
/// absent on purpose: a label reports text, and it does not accept one.
pub const TEXT_VALUE_ROLES: &[&str] = &["AXTextField", "AXTextArea", "AXComboBox", "AXSearchField"];

/// The roles that accept a number write into `AXValue`.
///
/// Each one publishes a numeric `AXValue` with an `AXMinValue` and an
/// `AXMaxValue`. A write moves the control without a drag.
pub const NUMBER_VALUE_ROLES: &[&str] = &[
    "AXSlider",
    "AXStepper",
    "AXIncrementor",
    "AXScrollBar",
    "AXValueIndicator",
];

/// The roles that accept a write into `AXSelectedTextRange`.
///
/// Only a text element carries a caret, so this list matches
/// [`TEXT_VALUE_ROLES`].
pub const SELECTED_TEXT_RANGE_ROLES: &[&str] = TEXT_VALUE_ROLES;

/// What a `set_value` call writes.
///
/// The payload is a tagged enum, so one call carries exactly one kind of
/// value. A caller cannot ask for a text write and a range write at the
/// same time. The tag field is `kind`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ValuePayload {
    /// A string for `AXValue`, e.g. the contents of a text field.
    Text { text: String },
    /// A number for `AXValue`, e.g. the position of a slider.
    Number { number: f64 },
    /// A caret position, and a selection length, for
    /// `AXSelectedTextRange`. A `length` of `0` places the caret and
    /// selects nothing.
    SelectedTextRange { location: usize, length: usize },
}

impl ValuePayload {
    /// The attribute this payload writes.
    pub fn attribute(&self) -> &'static str {
        match self {
            ValuePayload::Text { .. } | ValuePayload::Number { .. } => VALUE_ATTRIBUTE,
            ValuePayload::SelectedTextRange { .. } => SELECTED_TEXT_RANGE_ATTRIBUTE,
        }
    }

    /// The roles that accept this payload. See PINV-26.
    pub fn accepted_roles(&self) -> &'static [&'static str] {
        match self {
            ValuePayload::Text { .. } => TEXT_VALUE_ROLES,
            ValuePayload::Number { .. } => NUMBER_VALUE_ROLES,
            ValuePayload::SelectedTextRange { .. } => SELECTED_TEXT_RANGE_ROLES,
        }
    }
}

/// A `set_value` tool call.
///
/// The request is a struct, and the payload enum sits inside it. The
/// root of the input schema stays an object. `rmcp` requires that root
/// type (see `apps/polarize`'s `keyboard_input_schema`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SetValueRequest {
    /// Which app to inspect. `None` means the frontmost app.
    #[serde(default)]
    pub app: Option<AppIdentifier>,
    /// Which element to write to. See [`crate::selector`] (PINV-15).
    pub selector: ElementSelector,
    /// What to write.
    pub value: ValuePayload,
}

/// The result of a `set_value` tool call.
///
/// The response repeats the resolved element, because a selector can
/// match more than one node. The caller reads `path`, `role`, and
/// `label` to confirm the tool wrote to the element it meant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SetValueResponse {
    /// Always `true`. A failed write returns an error instead.
    ///
    /// `true` means the app accepted the write. It does not mean the
    /// app ran its own edit handlers. See PINV-27.
    pub set: bool,
    /// The app the write addressed, as `describe` resolved it. A
    /// request that named no app reports the app that was frontmost.
    pub app_name: String,
    /// The attribute the tool wrote, e.g. `"AXValue"`.
    pub attribute: String,
    /// The child indices the selector resolved to, from the tree root.
    pub path: ElementPath,
    /// The resolved element's `AXRole`.
    pub role: String,
    /// The resolved element's label, when it has one.
    pub label: Option<String>,
}

/// Why `set_value` refused to write to the element it resolved.
///
/// Each variant is a refusal before any native call. None of them
/// reports a native failure. `polarize-macos` reports those through
/// [`PolarizeError::Platform`].
///
/// This enum converts into [`PolarizeError::Platform`], because
/// `PolarizeError` carries no `SetValue` variant yet. Every message
/// starts with `set_value refused`, so a reader can still tell a
/// refusal from a native failure.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SetValueError {
    /// The element's role does not accept a write to this attribute.
    #[error(
        "set_value refused: element ({element}) has a role that does not accept a write to \
         {attribute:?}; that write needs one of [{accepted}]"
    )]
    NotSettable {
        element: String,
        attribute: String,
        accepted: String,
    },

    /// The app reports the element as disabled.
    #[error(
        "set_value refused: element ({element}) is disabled, so it cannot accept a write to \
         {attribute:?}"
    )]
    Disabled { element: String, attribute: String },

    /// The caller sent a number that no `CFNumber` can hold.
    #[error("set_value refused: {number} is not a finite number, so it cannot reach {attribute:?}")]
    NotFinite { number: f64, attribute: String },

    /// A resolved path did not read back to a node of the same tree.
    #[error("set_value refused: element path {path:?} does not resolve to a node")]
    PathNotResolved { path: ElementPath },
}

/// A refusal is not a platform failure. `PolarizeError` carries no
/// `SetValue` variant, so the message travels in the `Platform` variant
/// and names itself instead.
impl From<SetValueError> for PolarizeError {
    fn from(error: SetValueError) -> Self {
        PolarizeError::Platform(error.to_string())
    }
}

/// One resolved attribute write, ready for the platform layer.
///
/// [`set_element_value`] builds this after every check passes.
/// [`ValueSetter`] turns it into one `AXUIElementSetAttributeValue`
/// call. The attribute name and the value type always agree here, so
/// `polarize-macos` picks a setter and makes no further decision.
#[derive(Debug, Clone, PartialEq)]
pub struct AttributeWrite {
    /// The AX attribute name, e.g. `"AXValue"`.
    pub attribute: String,
    /// The value, in the Core Foundation type the attribute takes.
    pub value: AttributeValue,
}

/// The value half of an [`AttributeWrite`].
#[derive(Debug, Clone, PartialEq)]
pub enum AttributeValue {
    /// Writes as a `CFString`.
    Text(String),
    /// Writes as a `CFNumber`.
    Number(f64),
    /// Writes as an `AXValue` that wraps a `CFRange`.
    Range { location: usize, length: usize },
}

impl AttributeWrite {
    /// The write a payload asks for.
    pub fn from_payload(payload: &ValuePayload) -> Self {
        let attribute = payload.attribute().to_string();
        let value = match payload {
            ValuePayload::Text { text } => AttributeValue::Text(text.clone()),
            ValuePayload::Number { number } => AttributeValue::Number(*number),
            ValuePayload::SelectedTextRange { location, length } => AttributeValue::Range {
                location: *location,
                length: *length,
            },
        };
        Self { attribute, value }
    }
}

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

/// # PINV-26: `set_value` checks the element before it writes
///
/// - Always: [`set_element_value`] resolves the element, then refuses in
///   three cases. It refuses when the node's role does not accept the
///   payload. It refuses when the node's `enabled` flag is `false`. It
///   refuses a number that is not finite. A refusal returns a
///   [`SetValueError`] and never calls [`ValueSetter::set_value_at_path`].
/// - Because: an AX write to the wrong element is silent. A write of
///   text into an `AXStaticText` label is one case. A write of a number
///   into a text field is another. Each returns an error code that
///   reads like every other AX error. The caller cannot then tell a
///   wrong target from a missing permission.
///   The tree `describe` already returned carries the role and the
///   enabled flag, so the check costs no extra native call.
/// - If violated: a caller writes to a label, or to a greyed-out
///   control, and reads a bare `kAXErrorIllegalArgument`. The refusal
///   never names the element that caused it.
pub fn set_element_value<A, S>(
    inspector: &A,
    setter: &S,
    request: &SetValueRequest,
) -> Result<SetValueResponse, PolarizeError>
where
    A: AccessibilityInspector,
    S: ValueSetter,
{
    let (resolved, root) = inspector.describe(request.app.as_ref())?;
    let path = selector::find_one(&root, &request.selector)?;
    let node = selector::node_at_path(&root, &path)
        .ok_or_else(|| SetValueError::PathNotResolved { path: path.clone() })?;
    let attribute = request.value.attribute();

    let accepted = request.value.accepted_roles();
    if !accepted.contains(&node.role.as_str()) {
        return Err(SetValueError::NotSettable {
            element: describe_node(node),
            attribute: attribute.to_string(),
            accepted: accepted.join(", "),
        }
        .into());
    }
    if !node.enabled {
        return Err(SetValueError::Disabled {
            element: describe_node(node),
            attribute: attribute.to_string(),
        }
        .into());
    }
    if let ValuePayload::Number { number } = &request.value
        && !number.is_finite()
    {
        return Err(SetValueError::NotFinite {
            number: *number,
            attribute: attribute.to_string(),
        }
        .into());
    }

    // Address the app `describe` actually resolved, not `request.app`.
    // See `crate::action::resolved_target` and PINV-18.
    let target = resolved_target(request.app.as_ref(), &resolved);
    let write = AttributeWrite::from_payload(&request.value);
    setter.set_value_at_path(target.as_ref(), &path, &write)?;

    Ok(SetValueResponse {
        set: true,
        app_name: resolved.name,
        attribute: attribute.to_string(),
        path,
        role: node.role.clone(),
        label: node.label.clone(),
    })
}

/// The app identifier the write should address.
///
/// A request that named an app keeps that identifier. A request that
/// named none falls back to what `describe` resolved. Both calls then
/// address one app, which is the race PINV-18 closes.
///
/// `crate::action` holds a private copy of this rule for
/// `perform_action`. Both copies are four lines, and neither module owns
/// the other, so each keeps its own.
fn resolved_target(
    requested: Option<&AppIdentifier>,
    resolved: &ResolvedApp,
) -> Option<AppIdentifier> {
    match requested {
        Some(app) => Some(app.clone()),
        None => resolved.identifier(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    // ---- fakes -------------------------------------------------------

    struct FakeInspector {
        root: AxNode,
        bundle_id: Option<String>,
        seen: RefCell<Vec<Option<AppIdentifier>>>,
    }

    impl FakeInspector {
        fn new(root: AxNode) -> Self {
            Self {
                root,
                bundle_id: None,
                seen: RefCell::new(Vec::new()),
            }
        }

        fn with_bundle_id(mut self, bundle_id: &str) -> Self {
            self.bundle_id = Some(bundle_id.to_string());
            self
        }
    }

    impl AccessibilityInspector for FakeInspector {
        fn describe(
            &self,
            app: Option<&AppIdentifier>,
        ) -> Result<(ResolvedApp, AxNode), PolarizeError> {
            self.seen.borrow_mut().push(app.cloned());
            Ok((
                ResolvedApp {
                    name: "TestApp".to_string(),
                    bundle_id: self.bundle_id.clone(),
                },
                self.root.clone(),
            ))
        }
    }

    /// Records every call, so a test can prove the exact path and the
    /// exact write reached the platform layer.
    #[derive(Default)]
    struct RecordingSetter {
        calls: RefCell<Vec<(Option<AppIdentifier>, ElementPath, AttributeWrite)>>,
        fail_with: Option<String>,
    }

    impl RecordingSetter {
        fn failing(message: &str) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                fail_with: Some(message.to_string()),
            }
        }
    }

    impl ValueSetter for RecordingSetter {
        fn set_value_at_path(
            &self,
            app: Option<&AppIdentifier>,
            path: &[usize],
            write: &AttributeWrite,
        ) -> Result<(), PolarizeError> {
            self.calls
                .borrow_mut()
                .push((app.cloned(), path.to_vec(), write.clone()));
            match &self.fail_with {
                Some(message) => Err(PolarizeError::Platform(message.clone())),
                None => Ok(()),
            }
        }
    }

    // ---- test tree ---------------------------------------------------

    fn node(role: &str, label: &str) -> AxNode {
        AxNode {
            role: role.to_string(),
            label: Some(label.to_string()),
            focusable: true,
            ..AxNode::default()
        }
    }

    /// AXWindow "Main"
    ///   AXTextField "Name"     #name
    ///   AXGroup
    ///     AXTextField "Locked" (disabled)
    ///     AXTextArea  "Body"
    ///     AXSlider    "Volume"
    ///   AXStaticText "Ready"
    ///   AXButton     "Save"
    fn tree() -> AxNode {
        AxNode {
            role: "AXWindow".to_string(),
            label: Some("Main".to_string()),
            children: vec![
                AxNode {
                    identifier: Some("name".to_string()),
                    ..node("AXTextField", "Name")
                },
                AxNode {
                    role: "AXGroup".to_string(),
                    children: vec![
                        AxNode {
                            enabled: false,
                            ..node("AXTextField", "Locked")
                        },
                        node("AXTextArea", "Body"),
                        node("AXSlider", "Volume"),
                    ],
                    ..AxNode::default()
                },
                node("AXStaticText", "Ready"),
                AxNode {
                    actions: vec!["AXPress".to_string()],
                    ..node("AXButton", "Save")
                },
            ],
            ..AxNode::default()
        }
    }

    fn by_label(label: &str) -> ElementSelector {
        ElementSelector {
            label: Some(label.to_string()),
            ..ElementSelector::default()
        }
    }

    fn text_request(label: &str, text: &str) -> SetValueRequest {
        SetValueRequest {
            app: None,
            selector: by_label(label),
            value: ValuePayload::Text {
                text: text.to_string(),
            },
        }
    }

    // ---- happy path ---------------------------------------------------

    #[test]
    fn a_text_write_reports_the_resolved_element() {
        let inspector = FakeInspector::new(tree());
        let setter = RecordingSetter::default();

        let response =
            set_element_value(&inspector, &setter, &text_request("Name", "Ada")).unwrap();

        assert_eq!(
            response,
            SetValueResponse {
                set: true,
                app_name: "TestApp".to_string(),
                attribute: "AXValue".to_string(),
                path: vec![0],
                role: "AXTextField".to_string(),
                label: Some("Name".to_string()),
            }
        );
    }

    #[test]
    fn the_setter_receives_the_exact_path_and_write_that_were_resolved() {
        // PINV-18: the index path core resolves is the index path the
        // platform layer walks. Nothing may rewrite it in between.
        let inspector = FakeInspector::new(tree());
        let setter = RecordingSetter::default();

        set_element_value(&inspector, &setter, &text_request("Body", "hello")).unwrap();

        assert_eq!(
            setter.calls.borrow().as_slice(),
            &[(
                Some(AppIdentifier {
                    bundle_id: None,
                    app_name: Some("TestApp".to_string()),
                }),
                vec![1, 1],
                AttributeWrite {
                    attribute: "AXValue".to_string(),
                    value: AttributeValue::Text("hello".to_string()),
                }
            )]
        );
    }

    #[test]
    fn a_number_write_reaches_a_slider_as_a_number() {
        let inspector = FakeInspector::new(tree());
        let setter = RecordingSetter::default();
        let request = SetValueRequest {
            app: None,
            selector: by_label("Volume"),
            value: ValuePayload::Number { number: 0.75 },
        };

        let response = set_element_value(&inspector, &setter, &request).unwrap();

        assert_eq!(response.attribute, "AXValue");
        assert_eq!(response.path, vec![1, 2]);
        assert_eq!(
            setter.calls.borrow()[0].2,
            AttributeWrite {
                attribute: "AXValue".to_string(),
                value: AttributeValue::Number(0.75),
            }
        );
    }

    #[test]
    fn a_range_write_reaches_a_text_field_as_a_range() {
        let inspector = FakeInspector::new(tree());
        let setter = RecordingSetter::default();
        let request = SetValueRequest {
            app: None,
            selector: by_label("Name"),
            value: ValuePayload::SelectedTextRange {
                location: 3,
                length: 5,
            },
        };

        let response = set_element_value(&inspector, &setter, &request).unwrap();

        assert_eq!(response.attribute, "AXSelectedTextRange");
        assert_eq!(
            setter.calls.borrow()[0].2,
            AttributeWrite {
                attribute: "AXSelectedTextRange".to_string(),
                value: AttributeValue::Range {
                    location: 3,
                    length: 5,
                },
            }
        );
    }

    #[test]
    fn a_zero_length_range_places_the_caret_and_selects_nothing() {
        let inspector = FakeInspector::new(tree());
        let setter = RecordingSetter::default();
        let request = SetValueRequest {
            app: None,
            selector: by_label("Body"),
            value: ValuePayload::SelectedTextRange {
                location: 0,
                length: 0,
            },
        };

        set_element_value(&inspector, &setter, &request).unwrap();

        assert_eq!(
            setter.calls.borrow()[0].2.value,
            AttributeValue::Range {
                location: 0,
                length: 0
            }
        );
    }

    #[test]
    fn an_empty_string_write_clears_a_text_field() {
        // Clearing a field is a normal call, not a mistake. It must not
        // read as "the caller sent nothing".
        let inspector = FakeInspector::new(tree());
        let setter = RecordingSetter::default();

        set_element_value(&inspector, &setter, &text_request("Name", "")).unwrap();

        assert_eq!(
            setter.calls.borrow()[0].2.value,
            AttributeValue::Text(String::new())
        );
    }

    #[test]
    fn the_resolved_path_reads_back_to_the_element_the_response_names() {
        let root = tree();
        let inspector = FakeInspector::new(root.clone());
        let setter = RecordingSetter::default();

        let response = set_element_value(&inspector, &setter, &text_request("Body", "x")).unwrap();

        let node = selector::node_at_path(&root, &response.path).unwrap();
        assert_eq!(node.role, response.role);
        assert_eq!(node.label, response.label);
    }

    #[test]
    fn the_app_identifier_reaches_both_the_inspector_and_the_setter() {
        let inspector = FakeInspector::new(tree());
        let setter = RecordingSetter::default();
        let app = AppIdentifier {
            bundle_id: Some("com.apple.TextEdit".to_string()),
            app_name: None,
        };
        let mut request = text_request("Name", "Ada");
        request.app = Some(app.clone());

        set_element_value(&inspector, &setter, &request).unwrap();

        assert_eq!(inspector.seen.borrow().as_slice(), &[Some(app.clone())]);
        assert_eq!(setter.calls.borrow()[0].0, Some(app));
    }

    #[test]
    fn a_request_naming_no_app_pins_the_write_to_the_app_describe_resolved() {
        let inspector = FakeInspector::new(tree());
        let setter = RecordingSetter::default();

        set_element_value(&inspector, &setter, &text_request("Name", "Ada")).unwrap();

        assert_eq!(
            setter.calls.borrow()[0].0,
            Some(AppIdentifier {
                bundle_id: None,
                app_name: Some("TestApp".to_string()),
            })
        );
    }

    #[test]
    fn a_request_naming_no_app_prefers_the_resolved_bundle_id() {
        let inspector = FakeInspector::new(tree()).with_bundle_id("com.apple.TextEdit");
        let setter = RecordingSetter::default();

        set_element_value(&inspector, &setter, &text_request("Name", "Ada")).unwrap();

        assert_eq!(
            setter.calls.borrow()[0].0,
            Some(AppIdentifier {
                bundle_id: Some("com.apple.TextEdit".to_string()),
                app_name: None,
            })
        );
    }

    // ---- PINV-26 refusals ---------------------------------------------

    #[test]
    fn a_text_write_to_a_static_label_is_refused_before_the_platform_runs() {
        let inspector = FakeInspector::new(tree());
        let setter = RecordingSetter::default();

        let err = set_element_value(&inspector, &setter, &text_request("Ready", "x")).unwrap_err();

        let message = err.to_string();
        assert!(message.contains("AXStaticText"), "{message}");
        assert!(message.contains("Ready"), "names the element: {message}");
        assert!(
            message.contains("AXValue"),
            "names the attribute: {message}"
        );
        assert!(
            message.contains("AXTextField"),
            "names an accepted role: {message}"
        );
        assert!(
            setter.calls.borrow().is_empty(),
            "the platform must not be called on a refusal"
        );
    }

    #[test]
    fn a_text_write_to_a_button_is_refused() {
        let inspector = FakeInspector::new(tree());
        let setter = RecordingSetter::default();

        let err = set_element_value(&inspector, &setter, &text_request("Save", "x")).unwrap_err();

        assert!(err.to_string().contains("AXButton"), "{err}");
        assert!(setter.calls.borrow().is_empty());
    }

    #[test]
    fn a_number_write_to_a_text_field_is_refused() {
        // A text field holds a string. A number write there produces a
        // bare AX error code with no element named.
        let inspector = FakeInspector::new(tree());
        let setter = RecordingSetter::default();
        let request = SetValueRequest {
            app: None,
            selector: by_label("Name"),
            value: ValuePayload::Number { number: 1.0 },
        };

        let err = set_element_value(&inspector, &setter, &request).unwrap_err();

        assert!(err.to_string().contains("AXSlider"), "{err}");
        assert!(setter.calls.borrow().is_empty());
    }

    #[test]
    fn a_range_write_to_a_slider_is_refused() {
        // A slider carries no caret.
        let inspector = FakeInspector::new(tree());
        let setter = RecordingSetter::default();
        let request = SetValueRequest {
            app: None,
            selector: by_label("Volume"),
            value: ValuePayload::SelectedTextRange {
                location: 0,
                length: 1,
            },
        };

        let err = set_element_value(&inspector, &setter, &request).unwrap_err();

        assert!(err.to_string().contains("AXSelectedTextRange"), "{err}");
        assert!(setter.calls.borrow().is_empty());
    }

    #[test]
    fn a_disabled_element_is_refused_before_the_platform_runs() {
        let inspector = FakeInspector::new(tree());
        let setter = RecordingSetter::default();

        let err = set_element_value(&inspector, &setter, &text_request("Locked", "x")).unwrap_err();

        let message = err.to_string();
        assert!(message.contains("disabled"), "{message}");
        assert!(message.contains("Locked"), "names the element: {message}");
        assert!(setter.calls.borrow().is_empty());
    }

    #[test]
    fn a_number_that_is_not_finite_is_refused() {
        let inspector = FakeInspector::new(tree());
        let setter = RecordingSetter::default();
        for number in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let request = SetValueRequest {
                app: None,
                selector: by_label("Volume"),
                value: ValuePayload::Number { number },
            };

            let err = set_element_value(&inspector, &setter, &request).unwrap_err();

            assert!(
                err.to_string().contains("not a finite number"),
                "{number}: {err}"
            );
        }
        assert!(setter.calls.borrow().is_empty());
    }

    #[test]
    fn the_refusal_errors_render_the_element_and_the_attribute() {
        let not_settable = SetValueError::NotSettable {
            element: "role=\"AXStaticText\", label=\"Ready\"".to_string(),
            attribute: "AXValue".to_string(),
            accepted: "AXTextField, AXTextArea".to_string(),
        };
        assert_eq!(
            not_settable.to_string(),
            "set_value refused: element (role=\"AXStaticText\", label=\"Ready\") has a role that \
             does not accept a write to \"AXValue\"; that write needs one of [AXTextField, AXTextArea]"
        );

        let disabled = SetValueError::Disabled {
            element: "role=\"AXTextField\", label=\"Locked\"".to_string(),
            attribute: "AXValue".to_string(),
        };
        assert_eq!(
            disabled.to_string(),
            "set_value refused: element (role=\"AXTextField\", label=\"Locked\") is disabled, so \
             it cannot accept a write to \"AXValue\""
        );
    }

    #[test]
    fn a_refusal_names_itself_inside_the_platform_variant() {
        // `PolarizeError` carries no `SetValue` variant, so a refusal
        // travels as `Platform`. The message must still say which it is.
        let err: PolarizeError = SetValueError::Disabled {
            element: "role=\"AXTextField\"".to_string(),
            attribute: "AXValue".to_string(),
        }
        .into();
        assert!(matches!(err, PolarizeError::Platform(_)), "{err}");
        assert!(
            err.to_string()
                .starts_with("platform error: set_value refused")
        );
    }

    // ---- payload mapping -------------------------------------------------

    #[test]
    fn each_payload_names_its_own_attribute() {
        assert_eq!(
            ValuePayload::Text {
                text: String::new()
            }
            .attribute(),
            "AXValue"
        );
        assert_eq!(ValuePayload::Number { number: 0.0 }.attribute(), "AXValue");
        assert_eq!(
            ValuePayload::SelectedTextRange {
                location: 0,
                length: 0
            }
            .attribute(),
            "AXSelectedTextRange"
        );
    }

    #[test]
    fn a_range_payload_accepts_only_text_roles() {
        let roles = ValuePayload::SelectedTextRange {
            location: 0,
            length: 0,
        }
        .accepted_roles();
        assert!(roles.contains(&"AXTextField"));
        assert!(!roles.contains(&"AXSlider"));
    }

    #[test]
    fn a_static_text_role_accepts_no_payload_at_all() {
        // A label is the most common wrong target, so state it plainly.
        for payload in [
            ValuePayload::Text {
                text: String::new(),
            },
            ValuePayload::Number { number: 0.0 },
            ValuePayload::SelectedTextRange {
                location: 0,
                length: 0,
            },
        ] {
            assert!(!payload.accepted_roles().contains(&"AXStaticText"));
        }
    }

    // ---- selector failures ---------------------------------------------

    #[test]
    fn a_selector_that_matches_nothing_reports_the_selector_error() {
        let inspector = FakeInspector::new(tree());
        let setter = RecordingSetter::default();

        let err = set_element_value(&inspector, &setter, &text_request("Nope", "x")).unwrap_err();

        assert!(matches!(err, PolarizeError::Selector(_)), "{err}");
        assert!(err.to_string().contains("Nope"), "{err}");
        assert!(setter.calls.borrow().is_empty());
    }

    #[test]
    fn an_empty_selector_is_refused_by_the_selector_module() {
        let inspector = FakeInspector::new(tree());
        let setter = RecordingSetter::default();
        let request = SetValueRequest {
            app: None,
            selector: ElementSelector::default(),
            value: ValuePayload::Text {
                text: "x".to_string(),
            },
        };

        let err = set_element_value(&inspector, &setter, &request).unwrap_err();

        assert!(matches!(err, PolarizeError::Selector(_)), "{err}");
        assert!(setter.calls.borrow().is_empty());
    }

    #[test]
    fn an_index_picks_among_several_matching_elements() {
        let inspector = FakeInspector::new(tree());
        let setter = RecordingSetter::default();
        let request = SetValueRequest {
            app: None,
            selector: ElementSelector {
                role: Some("AXTextField".to_string()),
                enabled_only: true,
                index: Some(0),
                ..ElementSelector::default()
            },
            value: ValuePayload::Text {
                text: "x".to_string(),
            },
        };

        let response = set_element_value(&inspector, &setter, &request).unwrap();

        assert_eq!(response.path, vec![0]);
        assert_eq!(response.label.as_deref(), Some("Name"));
    }

    // ---- error propagation ----------------------------------------------

    #[test]
    fn a_platform_failure_from_the_setter_reaches_the_caller() {
        let inspector = FakeInspector::new(tree());
        let setter = RecordingSetter::failing("AXUIElementSetAttributeValue failed");

        let err = set_element_value(&inspector, &setter, &text_request("Name", "Ada")).unwrap_err();

        assert!(
            err.to_string()
                .contains("AXUIElementSetAttributeValue failed"),
            "{err}"
        );
    }

    #[test]
    fn a_describe_failure_stops_the_call_before_any_write() {
        struct FailingInspector;
        impl AccessibilityInspector for FailingInspector {
            fn describe(
                &self,
                _app: Option<&AppIdentifier>,
            ) -> Result<(ResolvedApp, AxNode), PolarizeError> {
                Err(PolarizeError::AppNotFound("com.example.Nope".to_string()))
            }
        }
        let setter = RecordingSetter::default();

        let err = set_element_value(&FailingInspector, &setter, &text_request("Name", "Ada"))
            .unwrap_err();

        assert!(matches!(err, PolarizeError::AppNotFound(_)), "{err}");
        assert!(setter.calls.borrow().is_empty());
    }

    // ---- wire contract ----------------------------------------------------

    #[test]
    fn the_request_round_trips_through_json() {
        let request = SetValueRequest {
            app: Some(AppIdentifier {
                bundle_id: Some("com.apple.TextEdit".to_string()),
                app_name: None,
            }),
            selector: by_label("Name"),
            value: ValuePayload::Text {
                text: "Ada".to_string(),
            },
        };
        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(
            serde_json::from_str::<SetValueRequest>(&json).unwrap(),
            request
        );
    }

    #[test]
    fn the_payload_is_tagged_by_kind_on_the_wire() {
        let json = serde_json::to_string(&ValuePayload::SelectedTextRange {
            location: 2,
            length: 4,
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"kind":"selected_text_range","location":2,"length":4}"#
        );
    }

    #[test]
    fn a_request_with_only_a_selector_and_a_value_deserializes() {
        let request: SetValueRequest = serde_json::from_str(
            r#"{"selector":{"identifier":"name"},"value":{"kind":"text","text":"Ada"}}"#,
        )
        .unwrap();
        assert_eq!(request.app, None);
        assert_eq!(request.selector.identifier.as_deref(), Some("name"));
        assert_eq!(
            request.value,
            ValuePayload::Text {
                text: "Ada".to_string()
            }
        );
    }

    #[test]
    fn a_payload_carrying_two_kinds_at_once_does_not_deserialize() {
        // The tag decides. A caller cannot mean text and a range in one
        // call, which is why the payload is an enum.
        let json = r#"{"selector":{"identifier":"name"},
            "value":{"kind":"text","location":1,"length":2}}"#;
        assert!(serde_json::from_str::<SetValueRequest>(json).is_err());
    }

    #[test]
    fn an_unknown_payload_kind_does_not_deserialize() {
        let json = r#"{"selector":{"identifier":"name"},"value":{"kind":"colour","text":"red"}}"#;
        assert!(serde_json::from_str::<SetValueRequest>(json).is_err());
    }

    #[test]
    fn a_negative_range_does_not_deserialize() {
        // `usize` rejects it, so no negative range reaches `CFRange`.
        let json = r#"{"selector":{"identifier":"name"},
            "value":{"kind":"selected_text_range","location":-1,"length":0}}"#;
        assert!(serde_json::from_str::<SetValueRequest>(json).is_err());
    }

    #[test]
    fn the_response_round_trips_through_json() {
        let response = SetValueResponse {
            set: true,
            app_name: "TestApp".to_string(),
            attribute: "AXValue".to_string(),
            path: vec![1, 0],
            role: "AXTextField".to_string(),
            label: Some("Name".to_string()),
        };
        let json = serde_json::to_string(&response).unwrap();
        assert_eq!(
            serde_json::from_str::<SetValueResponse>(&json).unwrap(),
            response
        );
    }

    #[test]
    fn the_request_renders_a_json_schema_with_an_object_root() {
        // `rmcp` rejects a tool input schema whose root is not an
        // object. The payload enum sits inside the request struct, so
        // the root stays an object and needs no patch in the server.
        let schema = serde_json::to_value(schemars::schema_for!(SetValueRequest)).unwrap();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["value"].is_object());
    }
}
