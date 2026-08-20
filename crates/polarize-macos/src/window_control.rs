//! [`WindowController`] over the `AXUIElement` C API — see
//! [`crate::ax_ffi`] for why that means hand-written FFI rather than
//! `objc2-accessibility`.
//!
//! Real native calls throughout; see the crate-level "what is and is not
//! verified" note. No window write has run against a real app in this
//! environment.
//!
//! This module decides nothing. [`polarize_core::window_control`] picks
//! the window, builds the ordered list of writes, and re-reads the
//! result. This module reads `AXWindows` and applies one write at a
//! time. That split is what lets `cargo test -p polarize-core` cover
//! PINV-28 and PINV-29 without a macOS session.
//!
//! ## Why an index, and not a live element handle
//!
//! `polarize-core` addresses a window by its index in the `AXWindows`
//! list. Every call re-reads that list. The app can open or close a
//! window between two calls, and the index then names another window.
//! `polarize` does not solve that race, for the same reason
//! [`crate::action`] does not: no element handle survives between calls.
//! An index that names no window is an error, never a silent fallback to
//! another window.
//!
//! ## Risk: `AXFullScreen` is undocumented
//!
//! `kAXFullScreenAttribute` has no entry in Apple's published attribute
//! list. The literal `"AXFullScreen"` is what real AppKit windows
//! publish. [`read_window_info`] reads it as an `Option<bool>`, so a
//! window that does not publish it reports `None` rather than `false`.
//! [`set_full_screen`] refuses such a window with a message that names
//! the attribute. `polarize-core` refuses it earlier still (PINV-28).

use objc2_core_foundation::{CGPoint, CGSize};
use polarize_core::coords::{PixelPoint, PixelRect, PixelSize};
use polarize_core::error::PolarizeError;
use polarize_core::permission::{PermissionError, PermissionKind, PermissionState};
use polarize_core::schema::AppIdentifier;
use polarize_core::traits::{ResolvedApp, WindowController, WindowInfo, WindowWrite};

use crate::ax_ffi::{self, AxElement};
use crate::window::resolve_running_app;

/// The undocumented attribute that toggles full screen. See the module
/// docs on the risk this carries.
const FULL_SCREEN_ATTRIBUTE: &str = "AXFullScreen";

/// `WindowController` implementation over `AXUIElement`.
#[derive(Debug, Default)]
pub struct MacWindowController;

impl WindowController for MacWindowController {
    fn list_windows(
        &self,
        app: Option<&AppIdentifier>,
    ) -> Result<(ResolvedApp, Vec<WindowInfo>), PolarizeError> {
        preflight()?;
        let (resolved, windows) = app_windows(app)?;
        let infos = windows.iter().map(read_window_info).collect();
        Ok((resolved, infos))
    }

    fn apply_window_write(
        &self,
        app: Option<&AppIdentifier>,
        index: usize,
        write: &WindowWrite,
    ) -> Result<(), PolarizeError> {
        preflight()?;
        let (_, windows) = app_windows(app)?;
        let count = windows.len();
        let window = windows.into_iter().nth(index).ok_or_else(|| {
            PolarizeError::WindowNotFound(format!(
                "window index {index} no longer names a window: the app now publishes \
                 {count} window(s). The app opened or closed a window after the list \
                 was read; call the tool again."
            ))
        })?;

        match write {
            WindowWrite::SetPosition(point) => window
                .set_point_attribute(
                    "AXPosition",
                    CGPoint {
                        x: point.x,
                        y: point.y,
                    },
                )
                .map_err(PolarizeError::Platform),
            WindowWrite::SetSize(size) => window
                .set_size_attribute(
                    "AXSize",
                    CGSize {
                        width: size.width,
                        height: size.height,
                    },
                )
                .map_err(PolarizeError::Platform),
            WindowWrite::SetMinimized(value) => window
                .set_bool_attribute("AXMinimized", *value)
                .map_err(PolarizeError::Platform),
            WindowWrite::SetMain(value) => window
                .set_bool_attribute("AXMain", *value)
                .map_err(PolarizeError::Platform),
            WindowWrite::SetFullScreen(value) => set_full_screen(&window, *value),
            WindowWrite::Raise => window
                .perform_action("AXRaise")
                .map_err(PolarizeError::Platform),
            WindowWrite::Close => press_close_button(&window),
        }
    }
}

/// The two checks every tool runs before any other native call.
///
/// `AXIsProcessTrusted` collapses "never asked" and "explicitly denied"
/// into the same `false` — `NotDetermined` is the more conservative of
/// the two to report when we cannot distinguish them. See PINV-10 and
/// PINV-11 in `docs/INVARIANTS.md`. The login-session check is PINV-23.
fn preflight() -> Result<(), PolarizeError> {
    if !unsafe { ax_ffi::AXIsProcessTrusted() } {
        return Err(PolarizeError::Permission(PermissionError::NotGranted {
            kind: PermissionKind::Accessibility,
            state: PermissionState::NotDetermined,
        }));
    }
    crate::session::ensure_session_usable()
}

/// Resolves the app, then reads its `AXWindows` list.
///
/// macOS publishes `AXWindows` front to back, so index `0` is the app's
/// frontmost window. `polarize_core::window_control` relies on that
/// order (PINV-28).
fn app_windows(
    app: Option<&AppIdentifier>,
) -> Result<(ResolvedApp, Vec<AxElement>), PolarizeError> {
    let running = resolve_running_app(app)?;
    let resolved = ResolvedApp {
        name: running
            .localizedName()
            .map(|name| name.to_string())
            .unwrap_or_default(),
        bundle_id: running.bundleIdentifier().map(|id| id.to_string()),
    };
    let pid = running.processIdentifier();
    let element = AxElement::for_application(pid);
    Ok((resolved, element.element_array_attribute("AXWindows")))
}

/// Reads one window's attributes into the shape `polarize-core` reasons
/// about.
///
/// An unreadable attribute degrades to a default rather than failing the
/// whole list (PINV-12), with one exception. `AXFullScreen` keeps its
/// `None`, because "this window does not publish full screen" and "this
/// window is not full screen" are different facts, and only the first
/// one must block a full-screen write (PINV-16, PINV-28).
fn read_window_info(window: &AxElement) -> WindowInfo {
    let position = window.point_attribute("AXPosition").unwrap_or_default();
    let size = window.size_attribute("AXSize").unwrap_or_default();
    WindowInfo {
        title: window
            .string_attribute("AXTitle")
            .filter(|title| !title.is_empty()),
        rect: PixelRect {
            origin: PixelPoint {
                x: position.x,
                y: position.y,
            },
            size: PixelSize {
                width: size.width,
                height: size.height,
            },
        },
        minimized: window.bool_attribute("AXMinimized").unwrap_or(false),
        main: window.bool_attribute("AXMain").unwrap_or(false),
        focused: window.bool_attribute("AXFocused").unwrap_or(false),
        full_screen: window.bool_attribute(FULL_SCREEN_ATTRIBUTE),
    }
}

/// Writes `AXFullScreen`, after confirming the window publishes it.
///
/// The read is not redundant with `polarize-core`'s check. The window
/// list can change between the two calls, and a window that publishes no
/// `AXFullScreen` accepts the write and does nothing. That looks exactly
/// like success. This turns it into an error.
fn set_full_screen(window: &AxElement, value: bool) -> Result<(), PolarizeError> {
    if window.bool_attribute(FULL_SCREEN_ATTRIBUTE).is_none() {
        return Err(PolarizeError::Platform(format!(
            "this window publishes no {FULL_SCREEN_ATTRIBUTE} attribute, so polarize \
             cannot toggle full screen on it. That attribute is undocumented, and a \
             window without a full-screen button does not publish it."
        )));
    }
    window
        .set_bool_attribute(FULL_SCREEN_ATTRIBUTE, value)
        .map_err(PolarizeError::Platform)
}

/// Closes the window by pressing its own close button.
///
/// There is no "close" attribute. `AXCloseButton` is the button in the
/// title bar, and `AXPress` on it is what a user's click does. A window
/// with no close button — a panel, a sheet, a full-screen window — has
/// no such attribute, and that is an error rather than a silent no-op.
fn press_close_button(window: &AxElement) -> Result<(), PolarizeError> {
    let button = window.element_attribute("AXCloseButton").ok_or_else(|| {
        PolarizeError::Platform(
            "this window publishes no AXCloseButton, so polarize cannot close it. \
             A panel, a sheet, and a full-screen window all lack one."
                .to_string(),
        )
    })?;
    button
        .perform_action("AXPress")
        .map_err(PolarizeError::Platform)
}
