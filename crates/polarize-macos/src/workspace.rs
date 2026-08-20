//! The native half of the four workspace tools: [`WindowLister`],
//! [`AppLifecycle`], and [`DisplayLister`].
//!
//! Every call here only fetches. Nothing in this module decides anything.
//! The join that turns two window lists into one record set (PINV-30),
//! and the policy that quits an app politely and reports what happened
//! (PINV-31), are pure logic in [`polarize_core::workspace`], where real
//! unit tests cover them.
//!
//! Real native calls throughout; see the crate-level "what is and is not
//! verified" note. None of this has run against a real macOS session.
//!
//! ## Which call needs which permission
//!
//! The four tools do not share a permission, and
//! [`polarize_core::permission::workspace_tool_permission`] states the
//! rule. This module follows it:
//!
//! - [`MacWorkspace::accessibility_windows`] reads the accessibility
//!   tree, so it checks `AXIsProcessTrusted` and then the login session,
//!   exactly as `describe` does (PINV-10, PINV-11, PINV-23).
//! - Nothing else here preflights anything. Opening an app, quitting an
//!   app, reading the window-server list, and reading display geometry
//!   capture no pixels, read no accessibility tree, and post no input.
//!   PINV-23 scopes the session check to the tools that do one of those
//!   three things. All four of these calls work while the screen is
//!   locked, so refusing them there would invent a failure the caller
//!   would otherwise never see.
//!
//! ## Two notes about `CGWindowListCopyWindowInfo`
//!
//! macOS hides `kCGWindowName` from a process without Screen Recording
//! permission. `list_windows` still works: the accessibility half
//! supplies the titles, and [`polarize_core::workspace::merge_window_lists`]
//! falls back to frame matching when the window-server half has no title
//! to match on. This is the normal case, not an edge case.
//!
//! `kCGWindowIsOnscreen` is the only supported Spaces signal macOS
//! publishes. It says whether a window sits on the Space the user is
//! looking at now. There is no supported Space id, so `polarize` reports
//! this flag and claims nothing more.

use std::ffi::c_void;
use std::time::Duration;

use objc2::rc::Retained;
use objc2_app_kit::{
    NSApplicationActivationOptions, NSApplicationActivationPolicy, NSRunningApplication,
    NSWorkspace, NSWorkspaceOpenConfiguration,
};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFString, CFType, CGRect};
use objc2_core_graphics::{
    CGDisplayBounds, CGDisplayCopyDisplayMode, CGDisplayMode, CGError, CGGetActiveDisplayList,
    CGMainDisplayID, CGRectMakeWithDictionaryRepresentation, CGWindowListCopyWindowInfo,
    CGWindowListOption, kCGNullWindowID, kCGWindowBounds, kCGWindowIsOnscreen, kCGWindowLayer,
    kCGWindowName, kCGWindowNumber, kCGWindowOwnerName, kCGWindowOwnerPID,
};
use objc2_foundation::NSString;
use polarize_core::error::PolarizeError;
use polarize_core::permission::{PermissionError, PermissionKind, PermissionState};
use polarize_core::schema::AppIdentifier;
use polarize_core::traits::{AppLifecycle, DisplayLister, WindowLister};
use polarize_core::workspace::{
    AxWindow, DisplayInfo, PixelFrame, RunningApp, ServerWindow, describe_identifier,
};

use crate::ax_ffi::{self, AxElement};
use crate::window::resolve_running_app;

/// The largest number of displays [`MacWorkspace::displays`] reads.
///
/// `CGGetActiveDisplayList` fills a caller-sized array. macOS supports
/// far fewer simultaneous displays than this, so the cap only bounds the
/// stack array; it never truncates a real setup.
const MAX_DISPLAYS: u32 = 32;

/// The three workspace traits over `NSWorkspace`, `NSRunningApplication`,
/// `CGWindowListCopyWindowInfo`, and `CGDirectDisplay`.
#[derive(Debug, Default)]
pub struct MacWorkspace;

// ---- WindowLister -------------------------------------------------------

impl WindowLister for MacWorkspace {
    fn resolve_app_pid(&self, app: &AppIdentifier) -> Result<i32, PolarizeError> {
        Ok(resolve_running_app(Some(app))?.processIdentifier())
    }

    fn accessibility_windows(
        &self,
        app: Option<&AppIdentifier>,
    ) -> Result<Vec<AxWindow>, PolarizeError> {
        // This is the half that reads the accessibility tree, so it
        // preflights Accessibility and then the login session, exactly
        // as `describe` does. `AXIsProcessTrusted` collapses "never
        // asked" and "explicitly denied" into one `false`, so report the
        // conservative `NotDetermined` (PINV-10, PINV-11).
        if !unsafe { ax_ffi::AXIsProcessTrusted() } {
            return Err(PolarizeError::Permission(PermissionError::NotGranted {
                kind: PermissionKind::Accessibility,
                state: PermissionState::NotDetermined,
            }));
        }
        crate::session::ensure_session_usable()?;

        let pids = match app {
            Some(app) => vec![resolve_running_app(Some(app))?.processIdentifier()],
            None => regular_app_pids(),
        };
        let mut windows = Vec::new();
        for pid in pids {
            let element = AxElement::for_application(pid);
            for window in element.element_array_attribute("AXWindows") {
                windows.push(read_ax_window(pid, &window));
            }
        }
        Ok(windows)
    }

    fn window_server_windows(&self) -> Result<Vec<ServerWindow>, PolarizeError> {
        // `ExcludeDesktopElements` drops the desktop picture and the
        // desktop icon layer. `OptionAll` keeps windows on other Spaces,
        // which is the point: `kCGWindowIsOnscreen` then tells a caller
        // which ones are on the Space in front of the user.
        let options = CGWindowListOption::OptionAll | CGWindowListOption::ExcludeDesktopElements;
        let Some(list) = CGWindowListCopyWindowInfo(options, kCGNullWindowID) else {
            return Err(PolarizeError::Platform(
                "CGWindowListCopyWindowInfo returned null".to_string(),
            ));
        };
        let mut windows = Vec::with_capacity(list.count() as usize);
        for index in 0..list.count() {
            let borrowed = unsafe { list.value_at_index(index) };
            if borrowed.is_null() {
                continue;
            }
            let value: &CFType = match unsafe { borrowed.cast::<CFType>().as_ref() } {
                Some(value) => value,
                None => continue,
            };
            let Some(entry) = value.downcast_ref::<CFDictionary>() else {
                continue;
            };
            if let Some(window) = read_server_window(entry) {
                windows.push(window);
            }
        }
        Ok(windows)
    }

    fn running_apps(&self) -> Result<Vec<RunningApp>, PolarizeError> {
        Ok(NSWorkspace::sharedWorkspace()
            .runningApplications()
            .iter()
            .map(|app| read_running_app(&app))
            .collect())
    }
}

/// The process ids of every app that shows up in the Dock.
///
/// An `Accessory` or `Prohibited` app publishes no ordinary window, so
/// walking its accessibility tree only costs time.
fn regular_app_pids() -> Vec<i32> {
    NSWorkspace::sharedWorkspace()
        .runningApplications()
        .iter()
        .filter(|app| app.activationPolicy() == NSApplicationActivationPolicy::Regular)
        .map(|app| app.processIdentifier())
        .collect()
}

/// Reads one window element out of an app's `AXWindows` list.
///
/// An unreadable attribute degrades to a default rather than dropping
/// the window, for the same reason PINV-12 gives for the tree walk: one
/// missing attribute must not cost a caller the whole record.
fn read_ax_window(pid: i32, element: &AxElement) -> AxWindow {
    let position = element.point_attribute("AXPosition").unwrap_or_default();
    let size = element.size_attribute("AXSize").unwrap_or_default();
    AxWindow {
        owner_pid: pid,
        title: element
            .string_attribute("AXTitle")
            .filter(|title| !title.is_empty()),
        frame: PixelFrame::new(position.x, position.y, size.width, size.height),
        main: element.bool_attribute("AXMain").unwrap_or(false),
        minimized: element.bool_attribute("AXMinimized").unwrap_or(false),
    }
}

/// Reads one entry of the `CGWindowListCopyWindowInfo` array.
///
/// Returns `None` when the entry carries no window number or no owner
/// process id. Without those two, the record can neither be addressed
/// again nor joined to an app.
fn read_server_window(entry: &CFDictionary) -> Option<ServerWindow> {
    let window_id = number_value(entry, unsafe { kCGWindowNumber })?;
    let owner_pid = number_value(entry, unsafe { kCGWindowOwnerPID })?;
    Some(ServerWindow {
        window_id: window_id as u32,
        owner_pid: owner_pid as i32,
        owner_name: string_value(entry, unsafe { kCGWindowOwnerName }),
        title: string_value(entry, unsafe { kCGWindowName }).filter(|name| !name.is_empty()),
        frame: rect_value(entry, unsafe { kCGWindowBounds }).unwrap_or_default(),
        // macOS adds `kCGWindowIsOnscreen` only for a window on the
        // Space in front of the user, so an absent key means "not on
        // this Space".
        on_screen: bool_value(entry, unsafe { kCGWindowIsOnscreen }).unwrap_or(false),
        layer: number_value(entry, unsafe { kCGWindowLayer }).unwrap_or(0) as i32,
    })
}

/// Reduces one `NSRunningApplication` to the fields a window record
/// names.
fn read_running_app(app: &NSRunningApplication) -> RunningApp {
    RunningApp {
        pid: app.processIdentifier(),
        name: app
            .localizedName()
            .map(|name| name.to_string())
            .unwrap_or_default(),
        bundle_id: app.bundleIdentifier().map(|id| id.to_string()),
    }
}

// ---- Core Foundation dictionary readers ---------------------------------

/// Reads one borrowed value out of a `CFDictionary`.
///
/// `CFDictionaryGetValue` hands back a borrowed (+0) value that lives as
/// long as the dictionary does, so no retain is needed. A missing key
/// reads as `None`.
fn value_for_key<'a>(dictionary: &'a CFDictionary, key: &CFString) -> Option<&'a CFType> {
    let key_ptr: *const c_void = (key as *const CFString).cast();
    let value: *const c_void = unsafe { dictionary.value(key_ptr) };
    unsafe { value.cast::<CFType>().as_ref() }
}

/// Reads a `CFString` value, or `None` when the key is absent or holds
/// another type.
fn string_value(dictionary: &CFDictionary, key: &CFString) -> Option<String> {
    value_for_key(dictionary, key)?
        .downcast_ref::<CFString>()
        .map(ToString::to_string)
}

/// Reads a `CFNumber` value as an `i64`.
fn number_value(dictionary: &CFDictionary, key: &CFString) -> Option<i64> {
    value_for_key(dictionary, key)?
        .downcast_ref::<CFNumber>()?
        .as_i64()
}

/// Reads a `CFBoolean` value.
fn bool_value(dictionary: &CFDictionary, key: &CFString) -> Option<bool> {
    value_for_key(dictionary, key)?
        .downcast_ref::<CFBoolean>()
        .map(CFBoolean::value)
}

/// Reads a rectangle value. `kCGWindowBounds` holds a `CFDictionary`
/// with `X`, `Y`, `Width`, and `Height` keys, which
/// `CGRectMakeWithDictionaryRepresentation` unpacks.
fn rect_value(dictionary: &CFDictionary, key: &CFString) -> Option<PixelFrame> {
    let bounds = value_for_key(dictionary, key)?.downcast_ref::<CFDictionary>()?;
    let mut rect = CGRect::ZERO;
    let ok = unsafe { CGRectMakeWithDictionaryRepresentation(Some(bounds), &mut rect) };
    ok.then(|| {
        PixelFrame::new(
            rect.origin.x,
            rect.origin.y,
            rect.size.width,
            rect.size.height,
        )
    })
}

// ---- AppLifecycle -------------------------------------------------------

impl AppLifecycle for MacWorkspace {
    fn find_running_app(&self, app: &AppIdentifier) -> Result<Option<RunningApp>, PolarizeError> {
        match resolve_running_app(Some(app)) {
            Ok(running) => Ok(Some(read_running_app(&running))),
            // "Not running" is the answer this call exists to give, not
            // a failure. Every other error still travels.
            Err(PolarizeError::AppNotFound(_)) => Ok(None),
            Err(err) => Err(err),
        }
    }

    fn open_app(&self, app: &AppIdentifier) -> Result<Option<RunningApp>, PolarizeError> {
        let workspace = NSWorkspace::sharedWorkspace();

        // PINV-5: the bundle id is tried first, and a mismatch falls
        // through to the name.
        if let Some(bundle_id) = app.bundle_id.as_deref() {
            let key = NSString::from_str(bundle_id);
            if let Some(url) = workspace.URLForApplicationWithBundleIdentifier(&key) {
                let configuration = NSWorkspaceOpenConfiguration::configuration();
                // The completion handler is `None` on purpose. This call
                // is asynchronous either way, and
                // `polarize_core::workspace::perform_app_launch` polls
                // `find_running_app` until the app appears. That keeps
                // the launch-wait policy — the deadline, the poll
                // interval — in tested pure logic instead of in a block
                // this crate cannot test.
                workspace.openApplicationAtURL_configuration_completionHandler(
                    &url,
                    &configuration,
                    None,
                );
                return Ok(None);
            }
        }

        let Some(name) = app.app_name.as_deref() else {
            return Err(PolarizeError::AppNotFound(describe_identifier(app)));
        };
        // `launchApplication:` is the only `NSWorkspace` call that opens
        // an app by name. Its replacement,
        // `openApplicationAtURL:configuration:completionHandler:`, needs
        // a bundle URL, and `NSWorkspace` publishes no supported way to
        // turn a display name into one. Deprecated is not removed: the
        // call still works, and the alternative would be guessing at
        // `/Applications/<name>.app`, which is wrong for any app
        // installed elsewhere.
        #[allow(deprecated)]
        let started = workspace.launchApplication(&NSString::from_str(name));
        if !started {
            return Err(PolarizeError::AppNotFound(describe_identifier(app)));
        }
        Ok(None)
    }

    fn activate_app_by_pid(&self, pid: i32) -> Result<bool, PolarizeError> {
        let running = running_app_for_pid(pid)?;
        // `ActivateIgnoringOtherApps` is deprecated since macOS 14 and
        // has no effect there. The default options already bring the
        // app's main and key windows forward.
        Ok(running.activateWithOptions(NSApplicationActivationOptions::empty()))
    }

    fn request_terminate(&self, pid: i32, force: bool) -> Result<bool, PolarizeError> {
        let running = running_app_for_pid(pid)?;
        // PINV-31: `force` comes from the caller, and this code never
        // escalates on its own. `terminate` sends a quit Apple Event, so
        // the app can save its documents. `forceTerminate` does not.
        Ok(if force {
            running.forceTerminate()
        } else {
            running.terminate()
        })
    }

    fn sleep_until_exit(&self, pid: Option<i32>, budget: Duration) -> Result<bool, PolarizeError> {
        let Some(pid) = pid else {
            std::thread::sleep(budget);
            return Ok(false);
        };
        // A pid macOS no longer lists belongs to a process that is gone.
        let Some(running) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
        else {
            return Ok(true);
        };
        if running.isTerminated() {
            return Ok(true);
        }
        std::thread::sleep(budget);
        Ok(running.isTerminated())
    }
}

/// Looks one running app up by process id.
fn running_app_for_pid(pid: i32) -> Result<Retained<NSRunningApplication>, PolarizeError> {
    NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
        .ok_or_else(|| PolarizeError::AppNotFound(format!("pid {pid}")))
}

// ---- DisplayLister ------------------------------------------------------

impl DisplayLister for MacWorkspace {
    fn displays(&self) -> Result<Vec<DisplayInfo>, PolarizeError> {
        // `CGDisplayBounds` is what `WindowManager::resolve_target_rect`
        // already returns for a `Screen` target, so a caller can compare
        // the two directly. `NSScreen.screens` would report the same
        // displays in AppKit's own bottom-left-origin space, and it
        // needs the main thread, which an MCP tool call does not run on.
        let mut ids = [0u32; MAX_DISPLAYS as usize];
        let mut count: u32 = 0;
        let err = unsafe { CGGetActiveDisplayList(MAX_DISPLAYS, ids.as_mut_ptr(), &mut count) };
        if err != CGError::Success {
            return Err(PolarizeError::Platform(format!(
                "CGGetActiveDisplayList failed with CGError {}",
                err.0
            )));
        }
        let main_id = CGMainDisplayID();
        Ok(ids[..count as usize]
            .iter()
            .map(|id| {
                let bounds = CGDisplayBounds(*id);
                DisplayInfo {
                    display_id: *id,
                    frame: PixelFrame::new(
                        bounds.origin.x,
                        bounds.origin.y,
                        bounds.size.width,
                        bounds.size.height,
                    ),
                    scale_factor: display_scale_factor(*id),
                    is_main: *id == main_id,
                }
            })
            .collect())
    }
}

/// The backing scale factor of one display.
///
/// The display mode reports two widths: the point width the desktop uses,
/// and the pixel width the backing store holds. Their ratio is the scale
/// factor — 2.0 on a Retina display. `NSScreen.backingScaleFactor` says
/// the same thing, but it needs the main thread.
///
/// Returns `1.0` when the mode is unreadable.
/// [`polarize_core::workspace::normalize_scale_factor`] repairs any other
/// useless value.
fn display_scale_factor(display_id: u32) -> f64 {
    let Some(mode) = CGDisplayCopyDisplayMode(display_id) else {
        return 1.0;
    };
    let points = CGDisplayMode::width(Some(&mode));
    let pixels = CGDisplayMode::pixel_width(Some(&mode));
    if points == 0 {
        return 1.0;
    }
    pixels as f64 / points as f64
}
