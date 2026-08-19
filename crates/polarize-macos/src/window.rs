//! [`WindowManager`] over `NSWorkspace`/`NSRunningApplication`
//! (`objc2-app-kit`) for app enumeration, and `CGDirectDisplay`
//! (`objc2-core-graphics`) for display geometry.
//!
//! Real native calls throughout except [`resolve_running_app`]'s use of
//! [`crate::app_lookup::find_matching_app_index`]; see the crate-level
//! "what is and is not verified" note.
//!
//! ## Known limitation: window-scoped `tap` targets
//!
//! [`WindowManager::resolve_target_size`] returns only a [`PixelSize`] (no
//! origin) for an `App`/`Window`-scoped [`ScreenshotTarget`], matching the
//! trait as `polarize-core` defines it. `polarize-core`'s `perform_tap`
//! (PINV-4) then normalizes the request's fraction against that size alone
//! and hands [`crate::input::MacInputSynthesizer::click_at_pixel`] the
//! result — but [`InputSynthesizer::click_at_pixel`]'s contract requires a
//! pixel point in the **global** display coordinate space, and a
//! window-relative size carries no origin to translate into that space.
//! For a `Screen { display_id: None }` target (global origin `(0, 0)`) this
//! is exactly correct. For a non-primary display or an `App`/`Window`
//! target whose origin is not `(0, 0)`, the resulting pixel point is
//! window/display-relative, not global — a real, open gap between this
//! trait shape and `click_at_pixel`'s documented contract. Fixing it needs
//! either a `PixelSize` → `PixelRect` change in `polarize-core` or a
//! trait shape that passes the origin through to `InputSynthesizer`
//! directly; flagged here rather than silently worked around.

use objc2::rc::Retained;
use objc2_app_kit::{NSRunningApplication, NSWorkspace};
use objc2_core_graphics::{CGDisplayBounds, CGMainDisplayID};
use polarize_core::coords::PixelSize;
use polarize_core::error::PolarizeError;
use polarize_core::schema::{AppIdentifier, ScreenshotTarget};
use polarize_core::traits::WindowManager;

use crate::app_lookup::{self, AppCandidate};
use crate::content;

/// Resolves `app` to a live [`NSRunningApplication`]: the frontmost app
/// when `app` is `None`, or the best [`app_lookup`] match among
/// `NSWorkspace`'s currently running apps otherwise.
pub fn resolve_running_app(
    app: Option<&AppIdentifier>,
) -> Result<Retained<NSRunningApplication>, PolarizeError> {
    let workspace = NSWorkspace::sharedWorkspace();
    let Some(identifier) = app else {
        return workspace
            .frontmostApplication()
            .ok_or_else(|| PolarizeError::AppNotFound("no frontmost app".to_string()));
    };

    let running = workspace.runningApplications().to_vec();
    let names: Vec<Option<String>> = running
        .iter()
        .map(|a| a.localizedName().map(|s| s.to_string()))
        .collect();
    let bundle_ids: Vec<Option<String>> = running
        .iter()
        .map(|a| a.bundleIdentifier().map(|s| s.to_string()))
        .collect();
    let candidates: Vec<AppCandidate<'_>> = names
        .iter()
        .zip(bundle_ids.iter())
        .map(|(name, bundle_id)| AppCandidate {
            bundle_id: bundle_id.as_deref(),
            name: name.as_deref(),
        })
        .collect();

    let idx = app_lookup::find_matching_app_index(identifier, &candidates).ok_or_else(|| {
        PolarizeError::AppNotFound(
            identifier
                .bundle_id
                .as_deref()
                .or(identifier.app_name.as_deref())
                .unwrap_or("<empty AppIdentifier>")
                .to_string(),
        )
    })?;
    Ok(running[idx].clone())
}

fn to_app_identifier(app: &NSRunningApplication) -> AppIdentifier {
    AppIdentifier {
        bundle_id: app.bundleIdentifier().map(|s| s.to_string()),
        app_name: app.localizedName().map(|s| s.to_string()),
    }
}

/// `WindowManager` implementation over `NSWorkspace`/`CGDirectDisplay`.
#[derive(Debug, Default)]
pub struct MacWindowManager;

impl WindowManager for MacWindowManager {
    fn frontmost_app(&self) -> Result<AppIdentifier, PolarizeError> {
        let app = NSWorkspace::sharedWorkspace()
            .frontmostApplication()
            .ok_or_else(|| PolarizeError::AppNotFound("no frontmost app".to_string()))?;
        Ok(to_app_identifier(&app))
    }

    fn resolve_target_size(&self, target: &ScreenshotTarget) -> Result<PixelSize, PolarizeError> {
        match target {
            ScreenshotTarget::Screen { display_id } => {
                let id = display_id.unwrap_or_else(|| CGMainDisplayID());
                let bounds = CGDisplayBounds(id);
                Ok(PixelSize {
                    width: bounds.size.width,
                    height: bounds.size.height,
                })
            }
            ScreenshotTarget::App { app } | ScreenshotTarget::Window { app, .. } => {
                let window_title = match target {
                    ScreenshotTarget::Window { window_title, .. } => Some(window_title.as_str()),
                    _ => None,
                };
                let running = resolve_running_app(Some(app))?;
                let pid = running.processIdentifier();
                let content = content::shareable_content()?;
                let window = content::find_window(&content, pid, window_title)?;
                let size = window.frame().size;
                Ok(PixelSize {
                    width: size.width,
                    height: size.height,
                })
            }
        }
    }
}
