//! [`ActionPerformer`] over `AXUIElementPerformAction` — see
//! [`crate::ax_ffi`] for why that means hand-written FFI rather than
//! `objc2-accessibility`.
//!
//! Real native calls throughout; see the crate-level "what is and is not
//! verified" note. No `perform_action` call has run against a real app in
//! this environment. A human on a real macOS session with Accessibility
//! permission granted needs to confirm two things. First, a resolved
//! path reaches the same element `describe` reported. Second, the app
//! visibly performs the action.
//!
//! ## Why this walks a path instead of holding an element
//!
//! `polarize-core` resolves a selector against the tree `describe`
//! returned. The result is a list of child indices. This module walks
//! the same indices down a live `AXUIElement` hierarchy. Both walks read
//! `AXChildren` in its published order, so both name the same element.
//! See PINV-18. The app can change its interface between the two walks;
//! `crate::action`'s core module documents that race.

use polarize_core::error::PolarizeError;
use polarize_core::permission::{PermissionError, PermissionKind, PermissionState};
use polarize_core::schema::AppIdentifier;
use polarize_core::traits::ActionPerformer;

use crate::ax_ffi::{self, AxElement};
use crate::window::resolve_running_app;

/// `ActionPerformer` implementation over `AXUIElementPerformAction`.
#[derive(Debug, Default)]
pub struct MacActionPerformer;

impl ActionPerformer for MacActionPerformer {
    fn perform_action_at_path(
        &self,
        app: Option<&AppIdentifier>,
        path: &[usize],
        action: &str,
    ) -> Result<(), PolarizeError> {
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

        let running = resolve_running_app(app)?;
        let pid = running.processIdentifier();
        let element = walk_path(AxElement::for_application(pid), path)?;
        element
            .perform_action(action)
            .map_err(PolarizeError::Platform)
    }
}

/// Walks `path` down from `root`, one `AXChildren` index at a time.
///
/// This mirrors `polarize_core::selector::node_at_path`, which walks the
/// same indices over the in-memory tree. See PINV-18.
///
/// An index that no longer names a child is an error, not a silent stop.
/// The tree changed between the two walks, so acting on the parent
/// element instead would press something the caller never named.
fn walk_path(root: AxElement, path: &[usize]) -> Result<AxElement, PolarizeError> {
    let mut element = root;
    for (depth, &index) in path.iter().enumerate() {
        let children = element.children();
        let count = children.len();
        element = children.into_iter().nth(index).ok_or_else(|| {
            PolarizeError::Platform(format!(
                "element path {path:?} does not resolve: the element at depth {depth} \
                 has {count} child element(s), so index {index} is out of range. \
                 The app's interface probably changed after `describe` ran; \
                 call `describe` again."
            ))
        })?;
    }
    Ok(element)
}
