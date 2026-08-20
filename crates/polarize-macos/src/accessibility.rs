//! [`AccessibilityInspector`] over the `AXUIElement` C API — see
//! [`crate::ax_ffi`] for why that means hand-written FFI rather than
//! `objc2-accessibility`.
//!
//! Real native calls throughout; see the crate-level "what is and is not
//! verified" note. In particular: no `describe` call has been run against a
//! real app in this environment, so a human on a real macOS session with
//! Accessibility permission granted needs to confirm the returned tree
//! actually mirrors the target app's real UI (right roles, right nesting,
//! right frames) rather than just "some tree came back".

use objc2_core_graphics::{CGDisplayBounds, CGMainDisplayID};
use polarize_core::ax::AxNode;
use polarize_core::coords::PixelSize;
use polarize_core::error::PolarizeError;
use polarize_core::permission::{PermissionError, PermissionKind, PermissionState};
use polarize_core::schema::AppIdentifier;
use polarize_core::traits::{AccessibilityInspector, ResolvedApp};

use crate::ax_ffi::{self, AxElement};
use crate::geometry::safe_normalize_frame;
use crate::window::resolve_running_app;

/// Hard cap on AX-tree recursion depth. Real AX trees are supposed to be
/// finite and shallow, but a misbehaving or adversarial app could in
/// principle expose a very deep or effectively-cyclic tree (some
/// accessibility proxies re-expose a wrapped element under a new node); a
/// depth cap turns "hang or exhaust memory" into "truncate the tree" for
/// that pathological case, which is not itself unit-testable (it needs a
/// real pathological AX tree to exercise) but is documented rather than
/// left as an unexplained magic number.
const MAX_AX_DEPTH: usize = 64;

/// `AccessibilityInspector` implementation over `AXUIElement`.
#[derive(Debug, Default)]
pub struct MacAccessibilityInspector;

impl AccessibilityInspector for MacAccessibilityInspector {
    fn describe(
        &self,
        app: Option<&AppIdentifier>,
    ) -> Result<(ResolvedApp, AxNode), PolarizeError> {
        // `AXIsProcessTrusted` collapses "never asked" and "explicitly
        // denied" into the same `false` — `NotDetermined` is the more
        // conservative of the two to report when we cannot distinguish
        // them (it does not claim the user made an explicit choice).
        // See PINV-10 (preflight before any further native call) and
        // PINV-11 (never falsely report `Denied`) in docs/INVARIANTS.md.
        if !unsafe { ax_ffi::AXIsProcessTrusted() } {
            return Err(PolarizeError::Permission(PermissionError::NotGranted {
                kind: PermissionKind::Accessibility,
                state: PermissionState::NotDetermined,
            }));
        }
        crate::session::ensure_session_usable()?;

        let running = resolve_running_app(app)?;
        let pid = running.processIdentifier();
        let resolved = ResolvedApp {
            name: running
                .localizedName()
                .map(|s| s.to_string())
                .unwrap_or_default(),
            bundle_id: running.bundleIdentifier().map(|s| s.to_string()),
        };

        let element = AxElement::for_application(pid);
        // See `window.rs`'s "known limitation" doc comment: normalizing
        // against the main display's bounds is exactly right for an app
        // whose windows live on the main display, and only approximate
        // for one whose windows live on a secondary display.
        let bounds = CGDisplayBounds(CGMainDisplayID());
        let screen_size = PixelSize {
            width: bounds.size.width,
            height: bounds.size.height,
        };

        let root = build_node(&element, screen_size, 0);
        Ok((resolved, root))
    }
}

/// Builds one node, then its subtree.
///
/// The eleven value attributes a node carries are read in one batch
/// call (PINV-41), with a one-at-a-time fallback. Three round trips
/// stay: the settable-`AXFocused` check, the action list, and
/// `AXChildren`. See also PINV-12 (a single unreadable attribute
/// degrades to a default, never aborts the walk) and PINV-13
/// (recursion stops at [`MAX_AX_DEPTH`], truncating rather than
/// erroring) in `docs/INVARIANTS.md`.
fn build_node(element: &AxElement, screen_size: PixelSize, depth: usize) -> AxNode {
    let attributes = element.node_attributes();
    let frame = safe_normalize_frame(attributes.position, attributes.size, screen_size);

    // Neither of these two reads has a batched form.
    // `AXUIElementIsAttributeSettable` and
    // `AXUIElementCopyActionNames` are their own calls.
    let focusable = element.is_attribute_settable("AXFocused");
    let actions = element.action_names();

    let children = if depth >= MAX_AX_DEPTH {
        Vec::new()
    } else {
        element
            .children()
            .iter()
            .map(|child| build_node(child, screen_size, depth + 1))
            .collect()
    };

    attributes.into_node(frame, focusable, actions, children)
}
