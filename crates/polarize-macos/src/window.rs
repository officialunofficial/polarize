//! [`WindowManager`] over `NSWorkspace`/`NSRunningApplication`
//! (`objc2-app-kit`) for app enumeration, and `CGDirectDisplay`
//! (`objc2-core-graphics`) for display geometry.
//!
//! Real native calls throughout except [`resolve_running_app`]'s use of
//! [`crate::app_lookup::find_matching_app_index`]; see the crate-level
//! "what is and is not verified" note.
//!
//! [`WindowManager::resolve_target_rect`] returns a [`PixelRect`] — size
//! plus global-space origin — for every [`ScreenshotTarget`] shape, so
//! `polarize-core`'s `perform_tap` (PINV-4) can turn a normalized
//! fraction into a real global pixel point regardless of which display
//! or window it targets. See PINV-4 in `docs/INVARIANTS.md` for why the
//! origin matters: an `App`/`Window` target, or a non-primary display,
//! does not start at the global origin, so dropping it would silently
//! click whatever sits at that pixel offset on the *primary* display
//! instead of the intended element.

use objc2::rc::Retained;
use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication, NSWorkspace};
use objc2_core_graphics::{CGDisplayBounds, CGMainDisplayID};
use polarize_core::coords::{PixelPoint, PixelRect, PixelSize};
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

/// `WindowManager` implementation over `NSWorkspace`/`CGDirectDisplay`.
#[derive(Debug, Default)]
pub struct MacWindowManager;

impl WindowManager for MacWindowManager {
    fn activate_app(&self, app: &AppIdentifier) -> Result<(), PolarizeError> {
        let running = resolve_running_app(Some(app))?;
        // `ActivateIgnoringOtherApps` is deprecated since macOS 14 and has
        // no effect there; the default (empty) options already bring the
        // app's main and key windows forward, which is what a `keyboard`
        // call needs before it can reach that app.
        if running.activateWithOptions(NSApplicationActivationOptions::empty()) {
            Ok(())
        } else {
            Err(PolarizeError::Platform(format!(
                "activateWithOptions returned false for {app:?}"
            )))
        }
    }

    /// See PINV-48. Builds the same raw SkyLight event record `yabai`'s
    /// `window_manager_make_key_window` posts (window_manager.c:1269).
    /// Then calls `_SLPSSetFrontProcessWithOptions`. `yabai` pairs
    /// exactly these two steps in both
    /// `window_manager_focus_window_with_raise` and
    /// `window_manager_focus_window_without_raise`
    /// (window_manager.c:1293) — they are shared machinery, not the
    /// raise/no-raise difference itself.
    ///
    /// The real difference is one call this function never makes:
    /// `window_manager_focus_window_with_raise` (window_manager.c:1324)
    /// ends with `AXUIElementPerformAction(window_ref, kAXRaiseAction)`.
    /// The without-raise variant does not. Skipping that AX raise
    /// action, not anything about the event records above, is what
    /// makes this "without raise."
    ///
    /// This also deliberately skips
    /// `window_manager_focus_window_without_raise`'s leading
    /// conditional block: two more event records, gated on whether
    /// `window_psn` equals `yabai`'s own tracked previously-focused
    /// window. That gate exists to replay `yabai`'s own per-Space
    /// focus history across a Space switch. `polarize` keeps no such
    /// history. One `keyboard` call is a one-shot request, not a step
    /// in a tracked focus sequence. There is no equivalent state to
    /// gate on here.
    fn activate_app_without_raise(&self, app: &AppIdentifier) -> Result<bool, PolarizeError> {
        let symbols = crate::skylight_ffi::symbols();
        let (Some(post_event_record_to), Some(set_front_process_with_options)) = (
            symbols.post_event_record_to,
            symbols.set_front_process_with_options,
        ) else {
            return Ok(false);
        };

        let running = resolve_running_app(Some(app))?;
        let pid = running.processIdentifier();
        let Some(psn) = crate::carbon_process::find_psn_for_pid(pid) else {
            return Ok(false);
        };
        // The window id comes from AX, not `ScreenCaptureKit` — see
        // issue #33 and PINV-48. This path only needs Accessibility
        // permission, the same as every other `keyboard` path. No
        // on-screen window (or `_AXUIElementGetWindow` not resolving)
        // is the third availability gate PINV-48 states, not a hard
        // failure: the caller falls back to `activate_app` the same
        // as a missing symbol or pid.
        //
        // `AXFocusedWindow` first, `AXMainWindow` only as a fallback: a
        // live session found `AXMainWindow` reports the app's primary
        // window even while a separate floating panel — one with no
        // `AXMainWindow` bit set — actually holds keyboard focus. Keying
        // the wrong window this way makes `keyboard` post text nobody
        // reads. `AXFocusedWindow` names whichever window really has
        // focus right now; `AXMainWindow` only stands in when the app
        // reports no focused window at all.
        let app_element = crate::ax_ffi::AxElement::for_application(pid);
        let Some(window_id) = app_element
            .element_attribute("AXFocusedWindow")
            .or_else(|| app_element.element_attribute("AXMainWindow"))
            .and_then(|window| window.window_id())
        else {
            return Ok(false);
        };

        // `yabai`'s `window_manager_make_key_window`: a `0xf8`-byte
        // record, byte `0x3a` set, the window id at `0x3c`, and bytes
        // `0x20..0x30` filled with `0xff`. Only byte `0x08` changes
        // between the two posts.
        let mut record = [0u8; 0xf8];
        record[0x04] = 0xf8;
        record[0x3a] = 0x10;
        record[0x3c..0x40].copy_from_slice(&window_id.to_ne_bytes());
        record[0x20..0x30].fill(0xff);

        record[0x08] = 0x01;
        unsafe { post_event_record_to(&psn, record.as_ptr()) };
        record[0x08] = 0x02;
        unsafe { post_event_record_to(&psn, record.as_ptr()) };

        // `kCPSUserGenerated` (`window_manager.h`): the mode `yabai`
        // pairs with the event records above for a non-raising focus
        // change. Whether this mode alone avoids raising and Space
        // switching here too is still an open question. See PINV-48
        // for the real-session check this needs.
        const K_CPS_USER_GENERATED: u32 = 0x200;
        unsafe { set_front_process_with_options(&psn, window_id, K_CPS_USER_GENERATED) };

        Ok(true)
    }

    fn resolve_app_pid(&self, app: &AppIdentifier) -> Result<Option<i32>, PolarizeError> {
        let running = resolve_running_app(Some(app))?;
        Ok(Some(running.processIdentifier()))
    }

    fn resolve_target_pid(&self, target: &ScreenshotTarget) -> Result<Option<i32>, PolarizeError> {
        match target {
            ScreenshotTarget::Screen { .. } => Ok(None),
            ScreenshotTarget::App { app } | ScreenshotTarget::Window { app, .. } => {
                let running = resolve_running_app(Some(app))?;
                Ok(Some(running.processIdentifier()))
            }
        }
    }

    fn resolve_target_rect(&self, target: &ScreenshotTarget) -> Result<PixelRect, PolarizeError> {
        match target {
            ScreenshotTarget::Screen { display_id } => {
                let id = display_id.unwrap_or_else(|| CGMainDisplayID());
                let bounds = CGDisplayBounds(id);
                Ok(PixelRect {
                    origin: PixelPoint {
                        x: bounds.origin.x,
                        y: bounds.origin.y,
                    },
                    size: PixelSize {
                        width: bounds.size.width,
                        height: bounds.size.height,
                    },
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
                let frame = window.frame();
                Ok(PixelRect {
                    origin: PixelPoint {
                        x: frame.origin.x,
                        y: frame.origin.y,
                    },
                    size: PixelSize {
                        width: frame.size.width,
                        height: frame.size.height,
                    },
                })
            }
        }
    }
}
