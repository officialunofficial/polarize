//! Trait definitions `polarize-macos` implements with real macOS
//! framework calls.
//!
//! These traits carry no logic themselves — implementing them means
//! calling `ScreenCaptureKit`, `AXUIElement`, `CGEvent`, and AppKit APIs
//! that cannot run or be verified without a real macOS session with
//! Screen Recording and Accessibility permission granted (see the
//! "Testing harness" section of `docs/INVARIANTS.md`). What *is* tested
//! here, against fake implementations, is the orchestration logic in
//! [`crate::orchestrate`] that sits in front of them — coordinate
//! normalization, request-to-call dispatch, and response shaping.

use crate::ax::AxNode;
use crate::coords::PixelPoint;
use crate::error::PolarizeError;
use crate::schema::{AppIdentifier, Modifier, NamedKey};

/// A captured image, still in encoded PNG form.
#[derive(Debug, Clone, PartialEq)]
pub struct CapturedImage {
    pub png_bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Captures pixels from a screen or a window. Implemented by
/// `polarize-macos` over `ScreenCaptureKit`.
pub trait ScreenCapture {
    /// Captures a whole display. `display_id` selects a specific display
    /// on a multi-monitor setup; `None` means the main display.
    fn capture_screen(&self, display_id: Option<u32>) -> Result<CapturedImage, PolarizeError>;

    /// Captures one window of `app`. `window_title` selects a specific
    /// window; `None` means the app's frontmost (or only) window.
    fn capture_window(
        &self,
        app: &AppIdentifier,
        window_title: Option<&str>,
    ) -> Result<CapturedImage, PolarizeError>;
}

/// Walks the accessibility tree of an app. Implemented by
/// `polarize-macos` over `AXUIElement` (objc2-accessibility).
pub trait AccessibilityInspector {
    /// Returns the resolved app's display name and its accessibility
    /// tree root. `app` is `None` to inspect the frontmost app.
    fn describe(&self, app: Option<&AppIdentifier>) -> Result<(String, AxNode), PolarizeError>;
}

/// Posts synthetic mouse and keyboard input. Implemented by
/// `polarize-macos` over `CGEvent` (objc2-core-graphics).
pub trait InputSynthesizer {
    /// Posts a mouse click at a pixel point already resolved from a
    /// normalized fraction — see [`crate::orchestrate::perform_tap`]
    /// (PINV-4). Implementations must not re-interpret `point` as
    /// anything other than raw pixels in the global display coordinate
    /// space.
    fn click_at_pixel(&self, point: PixelPoint, click_count: u8) -> Result<(), PolarizeError>;

    /// Types a literal string as a sequence of key-down/key-up events.
    fn type_text(&self, text: &str) -> Result<(), PolarizeError>;

    /// Presses one named key, holding the given modifiers.
    fn press_key(&self, key: NamedKey, modifiers: &[Modifier]) -> Result<(), PolarizeError>;
}

/// Enumerates apps/windows and resolves their pixel geometry.
/// Implemented by `polarize-macos` over AppKit (objc2-app-kit).
pub trait WindowManager {
    /// Brings `app` to the front. [`crate::orchestrate::perform_keyboard`]
    /// calls this first when a request names a `target` app, so typed
    /// text and key presses reach that app even when it did not already
    /// have focus.
    fn activate_app(&self, app: &AppIdentifier) -> Result<(), PolarizeError>;

    /// The pixel geometry — global-space origin plus size — of the
    /// screen or window a [`crate::schema::ScreenshotTarget`] refers to.
    /// [`crate::orchestrate::perform_tap`] normalizes a tap fraction
    /// against `size`, then adds `origin` to get a pixel point in the
    /// global display coordinate space [`InputSynthesizer::click_at_pixel`]
    /// requires (PINV-4 in `docs/INVARIANTS.md`).
    fn resolve_target_rect(
        &self,
        target: &crate::schema::ScreenshotTarget,
    ) -> Result<crate::coords::PixelRect, PolarizeError>;
}
