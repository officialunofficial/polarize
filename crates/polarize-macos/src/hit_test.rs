//! [`HitTester`] over `AXUIElementCopyElementAtPosition` — see
//! [`crate::ax_ffi`] for why that means hand-written FFI rather than
//! `objc2-accessibility`.
//!
//! Real native calls throughout; see the crate-level "what is and is not
//! verified" note. No hit test has run against a real app in this
//! environment. A human on a real macOS session with Accessibility
//! permission granted needs to confirm two things. First, the reported
//! element is the element the user sees at that point. Second, an
//! element that another window covers does not come back.
//!
//! ## The hit test reads every app, and reports which one answered
//!
//! This implementation asks the **system-wide** accessibility element,
//! so it sees across applications. A sheet, a popover, a second window
//! of the same app, and a window of a completely different app all count
//! as cover. That is what makes the tool a usable `tap` preflight.
//!
//! The app it reports is therefore the app that owns the element it
//! found, read from `AXUIElementGetPid` — not the app the request named.
//! Those differ exactly when something is in the way, which is the case
//! worth knowing about. When macOS reports no element at all, the report
//! falls back to the app the request addressed.

use objc2_app_kit::NSRunningApplication;
use objc2_core_graphics::{CGDisplayBounds, CGMainDisplayID};
use polarize_core::ax::AxNode;
use polarize_core::coords::{PixelPoint, PixelSize};
use polarize_core::error::PolarizeError;
use polarize_core::permission::{PermissionError, PermissionKind, PermissionState};
use polarize_core::schema::AppIdentifier;
use polarize_core::traits::{HitTester, ResolvedApp};

use crate::ax_ffi::{self, AxElement};
use crate::geometry::safe_normalize_frame;
use crate::window::resolve_running_app;

/// `HitTester` implementation over `AXUIElementCopyElementAtPosition`.
#[derive(Debug, Default)]
pub struct MacHitTester;

impl HitTester for MacHitTester {
    fn element_at_pixel(
        &self,
        app: Option<&AppIdentifier>,
        point: PixelPoint,
    ) -> Result<(ResolvedApp, Option<AxNode>), PolarizeError> {
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

        // The app the request named. Only a fallback for the report:
        // the hit test itself addresses the system-wide element.
        let running = resolve_running_app(app)?;
        let resolved = ResolvedApp {
            name: running
                .localizedName()
                .map(|s| s.to_string())
                .unwrap_or_default(),
            bundle_id: running.bundleIdentifier().map(|s| s.to_string()),
        };

        // `point` is already a global display pixel point, which is the
        // space `AXUIElementCopyElementAtPosition` reads (PINV-32).
        // `crate::accessibility` explains why the main display's bounds
        // normalize the frame.
        let bounds = CGDisplayBounds(CGMainDisplayID());
        let screen_size = PixelSize {
            width: bounds.size.width,
            height: bounds.size.height,
        };

        // Hit-test from the system-wide element, never from the app's.
        // `AXUIElementCopyElementAtPosition` searches only inside the
        // element it is called on, so an application element reports
        // its own view under the point even when another app's window
        // covers it. That would make this tool useless as the very
        // occlusion preflight it exists to be. See PINV-32.
        let found = AxElement::system_wide().element_at_position(point.x, point.y);
        let Some(element) = found else {
            return Ok((resolved, None));
        };
        // Report the app that really owns what was found. It differs
        // from the requested app exactly when another app covers the
        // point, and that is the fact a caller preflighting a tap needs.
        let owner = element.pid().and_then(owning_app).unwrap_or(resolved);
        Ok((owner, Some(leaf_node(&element, screen_size))))
    }
}

/// The app that owns `pid`, as a [`ResolvedApp`].
///
/// Returns `None` when the process is gone, or is not an application the
/// window server knows about. The caller then falls back to the app the
/// request named.
fn owning_app(pid: i32) -> Option<ResolvedApp> {
    let running = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)?;
    Some(ResolvedApp {
        name: running
            .localizedName()
            .map(|name| name.to_string())
            .unwrap_or_default(),
        bundle_id: running.bundleIdentifier().map(|id| id.to_string()),
    })
}

/// Reads one element's attributes into an [`AxNode`], with no children.
///
/// This mirrors `crate::accessibility`'s `build_node`, minus the
/// recursion. PINV-33 says a hit test reports one element, so this
/// function never reads `AXChildren`. Every attribute read degrades to a
/// default, exactly as PINV-12 and PINV-16 require.
fn leaf_node(element: &AxElement, screen_size: PixelSize) -> AxNode {
    let role = element
        .string_attribute("AXRole")
        .unwrap_or_else(|| "AXUnknown".to_string());
    let label = ["AXTitle", "AXDescription", "AXValue"]
        .into_iter()
        .find_map(|attr| {
            element
                .string_attribute(attr)
                .filter(|value| !value.is_empty())
        });

    let position = element.point_attribute("AXPosition").unwrap_or_default();
    let size = element.size_attribute("AXSize").unwrap_or_default();
    let frame = safe_normalize_frame(
        PixelPoint {
            x: position.x,
            y: position.y,
        },
        PixelSize {
            width: size.width,
            height: size.height,
        },
        screen_size,
    );

    let focusable = element.is_attribute_settable("AXFocused");
    let actions = element.action_names();
    let interactive = !actions.is_empty();
    // A missing `AXEnabled` means "this element does not publish an
    // enabled state", not "this element is disabled" — see PINV-16.
    let enabled = element.bool_attribute("AXEnabled").unwrap_or(true);
    let non_empty = |attribute: &str| {
        element
            .string_attribute(attribute)
            .filter(|value| !value.is_empty())
    };

    AxNode {
        role,
        label,
        frame,
        focusable,
        interactive,
        enabled,
        subrole: non_empty("AXSubrole"),
        role_description: non_empty("AXRoleDescription"),
        identifier: non_empty("AXIdentifier"),
        help: non_empty("AXHelp"),
        actions,
        children: Vec::new(),
    }
}
