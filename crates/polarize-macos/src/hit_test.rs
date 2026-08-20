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
//! ## Known limitation: the hit test reads one app
//!
//! This implementation asks one application element for the element at
//! the point. So it respects the window order inside that app. A sheet,
//! a popover, or a second window of the same app hides what is below
//! it, and the hit test reports the cover. It does **not** see a window
//! of *another* app over the point. `AXUIElementCopyElementAtPosition`
//! answers that wider question only from the system-wide element, and
//! `crate::ax_ffi` publishes no constructor for that element today. The
//! trait already carries `app: None` for "let the platform pick", so a
//! system-wide implementation needs no change in `polarize-core`.
//! Until then, `app: None` reads the frontmost app.

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

        let running = resolve_running_app(app)?;
        let pid = running.processIdentifier();
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

        let application = AxElement::for_application(pid);
        let node = application
            .element_at_position(point.x, point.y)
            .map(|element| leaf_node(&element, screen_size));
        Ok((resolved, node))
    }
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
