//! [`ScreenCapture`] over `ScreenCaptureKit`'s `SCScreenshotManager`.
//!
//! Real native calls throughout; see the crate-level "what is and is not
//! verified" note. In particular: this has never captured a real frame in
//! this environment (no display, no Screen Recording permission to grant),
//! so a human on a real macOS session needs to confirm the returned PNG
//! bytes actually decode to a recognizable screenshot, not just that a
//! `Result` came back `Ok`.

use polarize_core::error::PolarizeError;
use polarize_core::schema::AppIdentifier;
use polarize_core::traits::{CapturedImage, ScreenCapture};
use screencapturekit::screenshot_manager::SCScreenshotManager;
use screencapturekit::stream::configuration::SCStreamConfiguration;
use screencapturekit::stream::content_filter::SCContentFilter;

use crate::content;
use crate::window::resolve_running_app;

/// `ScreenCapture` implementation over `ScreenCaptureKit`.
#[derive(Debug, Default)]
pub struct MacScreenCapture;

impl ScreenCapture for MacScreenCapture {
    fn capture_screen(&self, display_id: Option<u32>) -> Result<CapturedImage, PolarizeError> {
        let content = content::shareable_content()?;
        let display = content::find_display(&content, display_id)?;
        let filter = SCContentFilter::create()
            .with_display(&display)
            .with_excluding_windows(&[])
            .build();
        let config = SCStreamConfiguration::new()
            .with_width(display.width())
            .with_height(display.height());
        capture_and_encode(&filter, &config)
    }

    fn capture_window(
        &self,
        app: &AppIdentifier,
        window_title: Option<&str>,
    ) -> Result<CapturedImage, PolarizeError> {
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
        capture_and_encode(&filter, &config)
    }
}

/// Captures via `SCScreenshotManager`, then round-trips the resulting
/// `CGImage` through `ImageIO`'s real PNG encoder (`save_png`, backed by
/// `CGImageDestination`) to get PNG bytes: `screencapturekit`/`apple-cf`
/// expose PNG export only as "write to a file", not "encode to an in-memory
/// buffer", so a uniquely-named temp file is the bridge — an internal
/// implementation detail, invisible to `polarize-core`'s base64-in-response
/// schema (see `schema.rs`'s design-decision doc comment).
fn capture_and_encode(
    filter: &SCContentFilter,
    config: &SCStreamConfiguration,
) -> Result<CapturedImage, PolarizeError> {
    let image = SCScreenshotManager::capture_image(filter, config)
        .map_err(|err| PolarizeError::Platform(format!("screen capture failed: {err}")))?;
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
    let png_bytes = std::fs::read(&path)
        .map_err(|err| PolarizeError::Platform(format!("reading encoded PNG failed: {err}")))?;
    let _ = std::fs::remove_file(&path);

    Ok(CapturedImage {
        png_bytes,
        width,
        height,
    })
}
