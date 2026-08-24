//! Shared `ScreenCaptureKit` content lookups used by both
//! [`crate::capture`] (pixels) and [`crate::window`] (window/app geometry
//! for `tap` normalization) — kept in one place so a screenshot and a
//! `resolve_target_rect` call for the same target agree on which window
//! they mean.
//!
//! Real native calls throughout; see the crate-level "what is and is not
//! verified" note.

use std::time::Duration;

use polarize_core::error::PolarizeError;
use screencapturekit::shareable_content::{SCDisplay, SCShareableContent, SCWindow};

/// How long [`crate::capture`] and [`crate::window`] wait for a
/// `ScreenCaptureKit` completion callback before giving up. See issue
/// #50: `screencapturekit`'s `SyncCompletion::wait()` blocks a `Condvar`
/// with no timeout of its own — a callback that never fires (observed
/// under stale Screen Recording TCC state) hangs the calling thread
/// forever without this bound. Ten seconds is generous against this
/// module's own doc note that a real capture normally completes in
/// milliseconds.
pub(crate) const SCREENCAPTUREKIT_CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// Fetches a fresh content snapshot. `ScreenCaptureKit` scopes this to
/// whatever the caller currently has Screen Recording permission to see;
/// a permission failure surfaces here as [`PolarizeError::Platform`].
///
/// `SCShareableContent` is `Send` (the crate marks it so), so this call
/// itself runs inside [`polarize_core::timeout::with_timeout`] here,
/// bounding the one blocking wait every caller of this function shares.
/// A caller does not need its own timeout around this specific call —
/// though a caller whose own further work can also block (a
/// `SCScreenshotManager::capture_image` call, say) still needs its own
/// bound around that separate wait. See [`SCREENCAPTUREKIT_CALL_TIMEOUT`].
pub fn shareable_content() -> Result<SCShareableContent, PolarizeError> {
    polarize_core::timeout::with_timeout(SCREENCAPTUREKIT_CALL_TIMEOUT, || {
        SCShareableContent::get().map_err(|err| {
            PolarizeError::Platform(format!("SCShareableContent::get failed: {err}"))
        })
    })
}

/// Finds the display matching `display_id`, or the first display when
/// `display_id` is `None` (macOS does not guarantee display enumeration
/// order matches "main display first", but `resolve_target_rect`'s
/// `CGMainDisplayID`-based path is what actually decides "main" for
/// sizing — this lookup only needs *a* display to build a capture filter
/// from when the caller did not ask for a specific one).
pub fn find_display(
    content: &SCShareableContent,
    display_id: Option<u32>,
) -> Result<SCDisplay, PolarizeError> {
    let displays = content.displays();
    match display_id {
        Some(id) => displays
            .into_iter()
            .find(|d| d.display_id() == id)
            .ok_or_else(|| PolarizeError::Platform(format!("no display with id {id}"))),
        None => displays
            .into_iter()
            .next()
            .ok_or_else(|| PolarizeError::Platform("no displays available".to_string())),
    }
}

/// Finds a window owned by the app with process id `pid`, optionally
/// matching `window_title` exactly. When `window_title` is `None`, the
/// first on-screen window found for that app is returned — `polarize`'s
/// `describe`/`screenshot` targets do not track window z-order today, so
/// "first" is the best available proxy for "frontmost".
pub fn find_window(
    content: &SCShareableContent,
    pid: i32,
    window_title: Option<&str>,
) -> Result<SCWindow, PolarizeError> {
    content
        .windows()
        .into_iter()
        .find(|w| {
            let owned_by_pid = w
                .owning_application()
                .is_some_and(|app| app.process_id() == pid);
            if !owned_by_pid {
                return false;
            }
            match window_title {
                Some(title) => w.title().as_deref() == Some(title),
                None => w.is_on_screen(),
            }
        })
        .ok_or_else(|| {
            let what = match window_title {
                Some(title) => format!("window titled {title:?} for pid {pid}"),
                None => format!("any on-screen window for pid {pid}"),
            };
            PolarizeError::WindowNotFound(what)
        })
}
