//! [`ScreenCapture`] over `ScreenCaptureKit`'s `SCScreenshotManager`.
//!
//! Real native calls throughout; see the crate-level "what is and is not
//! verified" note. In particular: this has never captured a real frame in
//! this environment. There is no display here, and no Screen Recording
//! permission to grant. A human on a real macOS session must confirm the
//! returned PNG bytes decode to a recognizable screenshot, not just that
//! a `Result` came back `Ok`.

use objc2_core_graphics::{CGMainDisplayID, CGPreflightScreenCaptureAccess};
use polarize_core::error::PolarizeError;
use polarize_core::permission::{PermissionError, PermissionKind, PermissionState};
use polarize_core::schema::AppIdentifier;
use polarize_core::traits::{CapturedImage, ScreenCapture};
use screencapturekit::screenshot_manager::SCScreenshotManager;
use screencapturekit::stream::configuration::SCStreamConfiguration;
use screencapturekit::stream::content_filter::SCContentFilter;

use crate::content;
use crate::window::resolve_running_app;

/// Checks Screen Recording permission before any `ScreenCaptureKit` call
/// — including `content::shareable_content`, so `crate::window`'s
/// raise-free activation path (PINV-48) calls this too, not only the
/// two [`ScreenCapture`] methods below. See PINV-10 in
/// `docs/INVARIANTS.md`.
///
/// `CGPreflightScreenCaptureAccess` collapses "never asked" and
/// "explicitly denied" into the same `false`, the same caveat
/// `accessibility.rs` and `input.rs` document for their own preflight
/// checks. `NotDetermined` is the more conservative of the two to report
/// when this method cannot tell them apart.
///
/// Calls `CGMainDisplayID` first, purely for its side effect of
/// establishing this process's connection to the WindowServer.
/// `polarize` has no `NSApplication` run loop. Without that connection
/// already open, `SCShareableContent::get()` crashes the whole process
/// — `Assertion failed: (did_initialize), function CGS_REQUIRE_INIT` —
/// instead of returning an `Err`. Calling this function first is what
/// guarantees that connection exists, for every caller, not only
/// `screenshot`.
pub(crate) fn ensure_screen_recording_permission() -> Result<(), PolarizeError> {
    let _ = CGMainDisplayID();
    if CGPreflightScreenCaptureAccess() {
        crate::session::ensure_session_usable()
    } else {
        Err(PolarizeError::Permission(PermissionError::NotGranted {
            kind: PermissionKind::ScreenRecording,
            state: PermissionState::NotDetermined,
        }))
    }
}

/// `ScreenCapture` implementation over `ScreenCaptureKit`.
#[derive(Debug, Default)]
pub struct MacScreenCapture;

impl ScreenCapture for MacScreenCapture {
    fn capture_screen(&self, display_id: Option<u32>) -> Result<CapturedImage, PolarizeError> {
        ensure_screen_recording_permission()?;
        let content = content::shareable_content()?;
        let display = content::find_display(&content, display_id)?;
        let filter = SCContentFilter::create()
            .with_display(&display)
            .with_excluding_windows(&[])
            .build();
        let config = SCStreamConfiguration::new()
            .with_width(display.width())
            .with_height(display.height());
        capture_and_encode(filter, config)
    }

    fn capture_window(
        &self,
        app: &AppIdentifier,
        window_title: Option<&str>,
    ) -> Result<CapturedImage, PolarizeError> {
        ensure_screen_recording_permission()?;
        let running = resolve_running_app(Some(app))?;
        let pid = running.processIdentifier();
        let content = content::shareable_content()?;
        let window = content::find_window(&content, pid, window_title)?;
        let size = window.frame().size;
        let filter = SCContentFilter::create().with_window(&window).build();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let config = SCStreamConfiguration::new()
            .with_width(size.width.round() as u32)
            .with_height(size.height.round() as u32);
        capture_and_encode(filter, config)
    }
}

/// Captures via `SCScreenshotManager`, then round-trips the resulting
/// `CGImage` through `ImageIO`'s real PNG encoder (`save_png`, backed by
/// `CGImageDestination`) to get PNG bytes. `screencapturekit`/`apple-cf`
/// expose PNG export only as "write to a file", not "encode to an
/// in-memory buffer". A uniquely named temp file is the bridge between
/// them. This is an internal implementation detail: it stays invisible to
/// `polarize-core`'s base64-in-response schema (see `schema.rs`'s
/// design-decision doc comment).
///
/// Takes `filter`/`config` by value, not by reference: the
/// `SCScreenshotManager::capture_image` call below blocks on a
/// `Condvar` with no timeout of its own (see issue #50 and
/// [`content::SCREENCAPTUREKIT_CALL_TIMEOUT`]'s doc), so it runs inside
/// [`polarize_core::timeout::with_timeout`] on a dedicated thread —
/// which needs `'static` ownership of everything it captures.
/// `SCContentFilter`, `SCStreamConfiguration`, and `CGImage` are all
/// `Send` (the crates mark each so), so only the timeout wait is added
/// here — nothing about the real capture call changes.
fn capture_and_encode(
    filter: SCContentFilter,
    config: SCStreamConfiguration,
) -> Result<CapturedImage, PolarizeError> {
    let image =
        polarize_core::timeout::with_timeout(content::SCREENCAPTUREKIT_CALL_TIMEOUT, move || {
            SCScreenshotManager::capture_image(&filter, &config)
                .map_err(|err| PolarizeError::Platform(format!("screen capture failed: {err}")))
        })?;
    let width = u32::try_from(image.width())
        .map_err(|_| PolarizeError::Platform("captured image width overflows u32".to_string()))?;
    let height = u32::try_from(image.height())
        .map_err(|_| PolarizeError::Platform("captured image height overflows u32".to_string()))?;

    let path = std::env::temp_dir().join(format!(
        "polarize-screenshot-{}-{}.png",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ));
    image
        .save_png(&path)
        .map_err(|err| PolarizeError::Platform(format!("PNG encode failed: {err}")))?;
    // Read, then always clean up the temp file — even when the read
    // itself fails, so a read error does not also leak the file.
    let read_result = std::fs::read(&path);
    let _ = std::fs::remove_file(&path);
    let png_bytes = read_result
        .map_err(|err| PolarizeError::Platform(format!("reading encoded PNG failed: {err}")))?;

    Ok(CapturedImage {
        png_bytes,
        width,
        height,
    })
}
