//! The window management tools: `set_window_frame` and `window_action`.
//!
//! `polarize` could inspect and click a window before this module. It
//! could not move one. These two tools close that gap. They move and
//! resize a window through `AXPosition` and `AXSize`. They minimize,
//! restore, focus, close, and full-screen a window through
//! `AXMinimized`, `AXMain`, `AXRaise`, `AXCloseButton`, and
//! `AXFullScreen`.
//!
//! Everything a test can decide lives here. [`WindowController`] holds
//! the two calls that need a real macOS session: read the app's window
//! list, and write one attribute. `cargo test -p polarize-core` covers
//! the rest.
//!
//! ## The coordinate contract, and why it has two units
//!
//! Every other `polarize` tool takes normalized `[0.0, 1.0]` fractions
//! of a target (PINV-4, PINV-8). A window tool has two real callers, and
//! they want different units.
//!
//! One caller wants a layout: "put this window on the left half of the
//! display". That is `position {x: 0.0, y: 0.0}` and `size {width: 0.5,
//! height: 1.0}`. It stays correct on every display size. Fractions are
//! the right unit for it, and they match the rest of the tool surface.
//!
//! The other caller wants an exact pixel frame: a screenshot comparison,
//! a bug report that names a size, a regression test. A fraction cannot
//! express `1280 x 720` on an unknown display.
//!
//! So [`SetWindowFrameRequest`] carries a `units` field. `fraction` is
//! the default, because it is the contract the rest of `polarize` uses.
//! `pixels` is the opt-in. The two are never mixed in one call, so a
//! request is never ambiguous.
//!
//! The fraction-to-pixel conversion is pure logic. It lives in
//! [`frame_to_pixels`] and it is unit-tested. A fraction resolves
//! against the display's [`PixelRect`], the same rect `tap` normalizes
//! against, and the display's origin is added. A window then lands in
//! the global display coordinate space, the space `AXPosition` reads and
//! writes.
//!
//! ## Reported frames are exact, not clamped
//!
//! A response reports the window's frame two times. It reports global
//! pixels, and it reports fractions of the display. The fractions are
//! exact. They are not clamped into `[0.0, 1.0]`.
//!
//! This differs from PINV-8 on purpose. PINV-8 clamps an element frame
//! because that frame feeds a `tap` point, and a tap point must be on
//! the display to be clickable. A window frame here is a report. A
//! window can sit partly off the display, or on a second display. A
//! clamped report would hide that. See PINV-29.
//!
//! ## Risk: `AXFullScreen` is undocumented
//!
//! `kAXFullScreenAttribute` is not in Apple's published attribute list.
//! The string `"AXFullScreen"` is what AppKit windows really publish,
//! and it has been stable for many releases. It is still not a promise.
//!
//! This module never assumes it. [`WindowInfo::full_screen`] is an
//! `Option<bool>`. `None` means the window publishes no such attribute.
//! A full-screen request against such a window is refused before any
//! write, with an error that names the attribute (PINV-28). It never
//! panics, and it never reports a silent success.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ax::NormalizedFrame;
use crate::coords::{self, CoordError, Fraction, PixelPoint, PixelRect, PixelSize};
use crate::error::PolarizeError;
use crate::schema::{AppIdentifier, ScreenshotTarget};
use crate::traits::{ResolvedApp, WindowController, WindowInfo, WindowManager, WindowWrite};

/// How close the real frame must be to the requested frame for
/// [`SetWindowFrameResponse::applied_exactly`] to report `true`.
///
/// One pixel. A window server rounds a frame to the backing store's
/// pixel grid, so an exact float match never holds on a Retina display.
/// A real clamp — an app that refuses to go below its minimum width —
/// misses by far more than one pixel.
pub const FRAME_TOLERANCE_PIXELS: f64 = 1.0;

/// Which unit a [`SetWindowFrameRequest`]'s `position` and `size` use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FrameUnits {
    /// Fractions of the display, in `0.0..=1.0`. The default, and the
    /// contract every other `polarize` tool uses.
    #[default]
    Fraction,
    /// Raw pixels in the global display coordinate space.
    Pixels,
}

/// A window's top-left corner, in whichever [`FrameUnits`] the request
/// names.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FramePoint {
    pub x: f64,
    pub y: f64,
}

/// A window's width and height, in whichever [`FrameUnits`] the request
/// names.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FrameSize {
    pub width: f64,
    pub height: f64,
}

/// A `set_window_frame` tool call.
///
/// A call must name a `position`, a `size`, or both. A call that names
/// neither is refused, because it would write nothing.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct SetWindowFrameRequest {
    /// Which app owns the window. `None` means the frontmost app.
    #[serde(default)]
    pub app: Option<AppIdentifier>,
    /// The exact `AXTitle` of the window. `None` means the app's
    /// frontmost window. See PINV-28.
    #[serde(default)]
    pub window_title: Option<String>,
    /// Picks one window when the addressing is otherwise ambiguous. See
    /// PINV-28.
    #[serde(default)]
    pub window_index: Option<usize>,
    /// The unit `position` and `size` use. Defaults to `fraction`.
    #[serde(default)]
    pub units: FrameUnits,
    /// The new top-left corner. `None` leaves the window where it is.
    #[serde(default)]
    pub position: Option<FramePoint>,
    /// The new size. `None` leaves the window's size alone.
    #[serde(default)]
    pub size: Option<FrameSize>,
    /// Which display a `fraction` request resolves against, and which
    /// display the reported fractions are relative to. `None` means the
    /// main display.
    #[serde(default)]
    pub display_id: Option<u32>,
}

/// What a `window_action` call does to the window it addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WindowAction {
    /// Sends the window to the Dock, through `AXMinimized`.
    Minimize,
    /// Brings the window back out of the Dock.
    Restore,
    /// Brings the app forward, then makes this window the main window
    /// and raises it.
    Focus,
    /// Presses the window's close button.
    Close,
    /// Enters full screen, through `AXFullScreen`.
    EnterFullScreen,
    /// Leaves full screen.
    ExitFullScreen,
}

/// A `window_action` tool call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WindowActionRequest {
    /// Which app owns the window. `None` means the frontmost app.
    #[serde(default)]
    pub app: Option<AppIdentifier>,
    /// The exact `AXTitle` of the window. `None` means the app's
    /// frontmost window. See PINV-28.
    #[serde(default)]
    pub window_title: Option<String>,
    /// Picks one window when the addressing is otherwise ambiguous. See
    /// PINV-28.
    #[serde(default)]
    pub window_index: Option<usize>,
    /// What to do to that window.
    pub action: WindowAction,
    /// Which display the reported fractions are relative to. `None`
    /// means the main display.
    #[serde(default)]
    pub display_id: Option<u32>,
}

/// One window's state, as the tool read it back after its write.
///
/// Every field here comes from a re-read. None of it repeats what the
/// request asked for. See PINV-29.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WindowState {
    /// The window's index in the app's `AXWindows` list, at the time of
    /// the re-read.
    pub index: usize,
    /// The window's `AXTitle`. `None` when it publishes none.
    pub title: Option<String>,
    /// The window's real frame, in global display pixels.
    pub pixel_x: f64,
    pub pixel_y: f64,
    pub pixel_width: f64,
    pub pixel_height: f64,
    /// The same frame, as exact fractions of the display. `None` when
    /// the display reports a non-positive size. These fractions are not
    /// clamped: a window off the display's edge reports a component
    /// outside `0.0..=1.0`.
    pub frame: Option<NormalizedFrame>,
    /// `AXMinimized`.
    pub minimized: bool,
    /// `AXMain`: whether this is the app's main window.
    pub main: bool,
    /// `AXFocused`: whether this window takes keyboard input.
    pub focused: bool,
    /// `AXFullScreen`. `None` when the window publishes no such
    /// attribute — see the module docs on that risk.
    pub full_screen: Option<bool>,
}

/// The result of a `set_window_frame` call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SetWindowFrameResponse {
    /// The app the call addressed, as the platform resolved it.
    pub app_name: String,
    /// The window, re-read after the write (PINV-29).
    pub window: WindowState,
    /// The position the tool asked the app for, in global pixels. It is
    /// in pixels whatever `units` the request used.
    pub requested_position: Option<FramePoint>,
    /// The size the tool asked the app for, in pixels.
    pub requested_size: Option<FrameSize>,
    /// `false` when the app did not honor the request exactly. An app
    /// clamps a window to its own minimum and maximum size. An app can
    /// also refuse a move. Read `window` for what really happened.
    pub applied_exactly: bool,
}

/// The result of a `window_action` call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WindowActionResponse {
    /// The app the call addressed, as the platform resolved it.
    pub app_name: String,
    /// The action the tool performed.
    pub action: WindowAction,
    /// The window, re-read after the write (PINV-29).
    ///
    /// `None` only after a `close` that really removed the window. A
    /// `close` that reports `Some` means the app kept the window. That
    /// happens when the app shows a "save changes?" sheet.
    pub window: Option<WindowState>,
    /// How many windows the app has, re-read after the write.
    pub window_count: usize,
}

/// Why a window tool refused to write.
///
/// Every variant is a refusal before any native write. None of them
/// reports a native failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WindowControlError {
    /// The app publishes no windows at all.
    #[error("app {app:?} publishes no windows")]
    NoWindows { app: String },

    /// No window carries the title the request named.
    #[error("no window titled {title:?}; the app publishes [{available}]")]
    NoMatch { title: String, available: String },

    /// Several windows carry that title, and the request named no
    /// `window_index` to pick between them.
    #[error(
        "{count} windows are titled {title:?}; name a window_index of 0..{count} to pick one; the app publishes [{available}]"
    )]
    Ambiguous {
        title: String,
        count: usize,
        available: String,
    },

    /// The `window_index` names no window.
    #[error("window_index {index} is out of range; {count} window(s) match")]
    IndexOutOfRange { index: usize, count: usize },

    /// A `set_window_frame` call named neither a position nor a size.
    #[error("set_window_frame needs a position, a size, or both; this call named neither")]
    NothingToDo,

    /// The window publishes no `AXFullScreen` attribute.
    #[error(
        "window ({window}) publishes no AXFullScreen attribute, so polarize cannot toggle full screen on it"
    )]
    FullScreenUnsupported { window: String },
}

impl From<WindowControlError> for PolarizeError {
    /// Maps a refusal onto the error variants `crate::error` already
    /// publishes.
    ///
    /// The four addressing failures are window lookups that found
    /// nothing usable, so they travel as
    /// [`PolarizeError::WindowNotFound`]. The other two report what this
    /// tool refuses to do, and `crate::error` has no variant for that,
    /// so they travel as [`PolarizeError::Platform`].
    fn from(error: WindowControlError) -> Self {
        match error {
            WindowControlError::NoWindows { .. }
            | WindowControlError::NoMatch { .. }
            | WindowControlError::Ambiguous { .. }
            | WindowControlError::IndexOutOfRange { .. } => {
                PolarizeError::WindowNotFound(error.to_string())
            }
            WindowControlError::NothingToDo | WindowControlError::FullScreenUnsupported { .. } => {
                PolarizeError::Platform(error.to_string())
            }
        }
    }
}

/// Renders a window list's titles for an error message.
fn available_titles(windows: &[WindowInfo]) -> String {
    windows
        .iter()
        .map(|window| match &window.title {
            Some(title) => format!("{title:?}"),
            None => "<untitled>".to_string(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// # PINV-28: a window tool checks its target before it writes
///
/// - Always: [`select_window`] resolves a request to exactly one window,
///   or it returns a [`WindowControlError`]. A title that matches no
///   window, a title that matches several with no `window_index`, an
///   out-of-range `window_index`, and an app with no windows are all
///   refusals. A full-screen request against a window that publishes no
///   `AXFullScreen` is a refusal too, in [`plan_action_writes`]. No
///   refusal ever reaches [`WindowController::apply_window_writes`].
/// - Because: these writes are destructive and they are not reversible
///   by the caller. `close` throws away unsaved work. A move loses the
///   window's old frame, which nothing recorded. Guessing which of two
///   equally titled windows the caller meant is therefore worse than
///   refusing. `AXFullScreen` needs its own check, because it is an
///   undocumented attribute: a window that does not publish it accepts
///   the write and does nothing, which looks exactly like success.
/// - If violated: `window_action` closes the wrong document window, or
///   reports that a window went full screen while nothing on screen
///   changed.
///
/// The matching rule itself: a `title` must equal `AXTitle` exactly.
/// This is the rule `polarize-macos`'s `content::find_window` already
/// uses for `screenshot`, so one title names one window across both
/// tools. A `title` of `None` means "the app's frontmost window", which
/// is index `0`: macOS publishes `AXWindows` in front-to-back order.
pub fn select_window(
    windows: &[WindowInfo],
    title: Option<&str>,
    index: Option<usize>,
    app: &str,
) -> Result<usize, WindowControlError> {
    if windows.is_empty() {
        return Err(WindowControlError::NoWindows {
            app: app.to_string(),
        });
    }
    let Some(title) = title else {
        // No title names no specific window, so an index is a plain
        // index into the whole list, and no index means the frontmost.
        return match index {
            None => Ok(0),
            Some(index) if index < windows.len() => Ok(index),
            Some(index) => Err(WindowControlError::IndexOutOfRange {
                index,
                count: windows.len(),
            }),
        };
    };

    let matches: Vec<usize> = windows
        .iter()
        .enumerate()
        .filter(|(_, window)| window.title.as_deref() == Some(title))
        .map(|(index, _)| index)
        .collect();

    match (matches.len(), index) {
        (0, _) => Err(WindowControlError::NoMatch {
            title: title.to_string(),
            available: available_titles(windows),
        }),
        (_, Some(index)) => {
            matches
                .get(index)
                .copied()
                .ok_or(WindowControlError::IndexOutOfRange {
                    index,
                    count: matches.len(),
                })
        }
        (1, None) => Ok(matches[0]),
        (count, None) => Err(WindowControlError::Ambiguous {
            title: title.to_string(),
            count,
            available: available_titles(windows),
        }),
    }
}

/// Finds the same window again after a write.
///
/// It applies the addressing the write used. A caller that named a title
/// gets the window with that title, even when the app reordered
/// `AXWindows` — `AXRaise` does exactly that. A caller that named no
/// title falls back to the index the write used.
///
/// Returns `None` when neither rule finds a window. The window is then
/// gone, which is the normal result of a `close`.
pub fn reselect_window(
    windows: &[WindowInfo],
    title: Option<&str>,
    index: Option<usize>,
    previous: usize,
    was: &WindowInfo,
    app: &str,
) -> Option<usize> {
    // A request that named a title can just look it up again. That is
    // the identity the caller chose, and a reorder does not change it.
    if title.is_some() {
        return match select_window(windows, title, index, app) {
            Ok(found) => Some(found),
            Err(_) if previous < windows.len() => Some(previous),
            Err(_) => None,
        };
    }

    // A request that named only an index cannot re-use it. The writes
    // just ran, and `AXRaise` or an un-minimize moves the window to the
    // front — so that index now names whatever took its place. Follow
    // the window's own title instead, when it has a unique one.
    if let Some(was_title) = was.title.as_deref() {
        let mut matches = windows
            .iter()
            .enumerate()
            .filter(|(_, window)| window.title.as_deref() == Some(was_title));
        if let Some((found, _)) = matches.next()
            && matches.next().is_none()
        {
            return Some(found);
        }
    }

    // An untitled window, or one of several sharing a title, has no
    // identity to follow across a reorder. The index is the only thing
    // left. See PINV-29's note on what that cannot promise.
    (previous < windows.len()).then_some(previous)
}

/// Converts a request's `position` and `size` into global pixels.
///
/// A `fraction` request resolves against `display`, through
/// [`crate::coords::fraction_to_pixel`]. That call rejects a fraction
/// outside `0.0..=1.0` (PINV-1), so a caller that passed pixels by
/// mistake gets a clear error rather than a window in the top-left
/// corner. The display's origin is then added, so the result is in the
/// global display coordinate space `AXPosition` uses.
///
/// A `pixels` request passes through. Its position is not range-checked:
/// a display above or left of the main display holds negative global
/// coordinates, and those are valid.
///
/// A size of zero or less is rejected in both units. A window with no
/// area is not a window.
pub fn frame_to_pixels(
    position: Option<FramePoint>,
    size: Option<FrameSize>,
    units: FrameUnits,
    display: PixelRect,
) -> Result<(Option<PixelPoint>, Option<PixelSize>), PolarizeError> {
    let position = match (position, units) {
        (None, _) => None,
        (Some(point), FrameUnits::Pixels) => Some(PixelPoint {
            x: point.x,
            y: point.y,
        }),
        (Some(point), FrameUnits::Fraction) => {
            let local = coords::fraction_to_pixel(
                Fraction {
                    x: point.x,
                    y: point.y,
                },
                display.size,
            )?;
            Some(PixelPoint {
                x: display.origin.x + local.x,
                y: display.origin.y + local.y,
            })
        }
    };

    let size = match (size, units) {
        (None, _) => None,
        (Some(size), FrameUnits::Pixels) => Some(PixelSize {
            width: size.width,
            height: size.height,
        }),
        (Some(size), FrameUnits::Fraction) => {
            // `fraction_to_pixel` applies PINV-1's range check to both
            // components and scales them. A width and a height scale the
            // same way a point's x and y do.
            let scaled = coords::fraction_to_pixel(
                Fraction {
                    x: size.width,
                    y: size.height,
                },
                display.size,
            )?;
            Some(PixelSize {
                width: scaled.x,
                height: scaled.y,
            })
        }
    };

    if let Some(size) = size
        && (size.width <= 0.0 || size.height <= 0.0)
    {
        return Err(CoordError::NonPositiveSize {
            width: size.width,
            height: size.height,
        }
        .into());
    }
    Ok((position, size))
}

/// The ordered writes that move and resize one window.
///
/// A request that names both a position and a size writes the position
/// two times, around the size. This is not redundant. An app clamps a
/// size against the space left on the display from the window's current
/// position, and it clamps a position against the window's current size.
/// Writing the position first frees the room the new size needs. Writing
/// it again puts the window back where the caller asked, now that the
/// size is settled.
///
/// The order is decided here, not in `polarize-macos`, so a test can
/// prove it. Whether the workaround really defeats a given app's
/// clamping is native behavior, and PINV-29's re-read is what reports
/// the truth either way.
pub fn plan_frame_writes(
    position: Option<PixelPoint>,
    size: Option<PixelSize>,
) -> Vec<WindowWrite> {
    match (position, size) {
        (Some(position), Some(size)) => vec![
            WindowWrite::SetPosition(position),
            WindowWrite::SetSize(size),
            WindowWrite::SetPosition(position),
        ],
        (Some(position), None) => vec![WindowWrite::SetPosition(position)],
        (None, Some(size)) => vec![WindowWrite::SetSize(size)],
        (None, None) => Vec::new(),
    }
}

/// A short rendering of one window, for an error message.
fn describe_window(index: usize, window: &WindowInfo) -> String {
    match &window.title {
        Some(title) => format!("index={index}, title={title:?}"),
        None => format!("index={index}, untitled"),
    }
}

/// The ordered writes that carry out one [`WindowAction`].
///
/// `Focus` un-minimizes first when the window is in the Dock. `AXRaise`
/// cannot bring a minimized window forward, so a focus call on a
/// minimized window would otherwise report success and change nothing.
///
/// Both full-screen actions refuse a window that publishes no
/// `AXFullScreen` (PINV-28).
pub fn plan_action_writes(
    action: WindowAction,
    index: usize,
    window: &WindowInfo,
) -> Result<Vec<WindowWrite>, WindowControlError> {
    let full_screen = |value: bool| match window.full_screen {
        Some(_) => Ok(vec![WindowWrite::SetFullScreen(value)]),
        None => Err(WindowControlError::FullScreenUnsupported {
            window: describe_window(index, window),
        }),
    };
    match action {
        WindowAction::Minimize => Ok(vec![WindowWrite::SetMinimized(true)]),
        WindowAction::Restore => Ok(vec![WindowWrite::SetMinimized(false)]),
        WindowAction::Focus if window.minimized => Ok(vec![
            WindowWrite::SetMinimized(false),
            WindowWrite::SetMain(true),
            WindowWrite::Raise,
        ]),
        WindowAction::Focus => Ok(vec![WindowWrite::SetMain(true), WindowWrite::Raise]),
        WindowAction::Close => Ok(vec![WindowWrite::Close]),
        WindowAction::EnterFullScreen => full_screen(true),
        WindowAction::ExitFullScreen => full_screen(false),
    }
}

/// The window's frame as exact fractions of `display`.
///
/// Returns `None` when the display reports a non-positive size, which
/// would divide by zero. The fractions are not clamped — see the module
/// docs.
pub fn normalized_frame(window: PixelRect, display: PixelRect) -> Option<NormalizedFrame> {
    if display.size.width <= 0.0 || display.size.height <= 0.0 {
        return None;
    }
    Some(NormalizedFrame {
        x: (window.origin.x - display.origin.x) / display.size.width,
        y: (window.origin.y - display.origin.y) / display.size.height,
        width: window.size.width / display.size.width,
        height: window.size.height / display.size.height,
    })
}

/// Shapes one re-read window into the response's [`WindowState`].
fn window_state(index: usize, window: &WindowInfo, display: PixelRect) -> WindowState {
    WindowState {
        index,
        title: window.title.clone(),
        pixel_x: window.rect.origin.x,
        pixel_y: window.rect.origin.y,
        pixel_width: window.rect.size.width,
        pixel_height: window.rect.size.height,
        frame: normalized_frame(window.rect, display),
        minimized: window.minimized,
        main: window.main,
        focused: window.focused,
        full_screen: window.full_screen,
    }
}

/// Whether the app honored the requested frame, within
/// [`FRAME_TOLERANCE_PIXELS`].
///
/// It compares only what the request named. A call that named no size
/// says nothing about the size it got.
pub fn frame_applied_exactly(
    actual: PixelRect,
    position: Option<PixelPoint>,
    size: Option<PixelSize>,
) -> bool {
    let close = |left: f64, right: f64| (left - right).abs() <= FRAME_TOLERANCE_PIXELS;
    let position_ok = match position {
        None => true,
        Some(position) => close(actual.origin.x, position.x) && close(actual.origin.y, position.y),
    };
    let size_ok = match size {
        None => true,
        Some(size) => {
            close(actual.size.width, size.width) && close(actual.size.height, size.height)
        }
    };
    position_ok && size_ok
}

/// The app identifier a follow-up call should address.
///
/// Same rule as `crate::action`'s: a caller's own identifier wins,
/// because it may carry a bundle id. A request that named no app falls
/// back to what the platform resolved, so the write and the re-read
/// address one app rather than resolving "frontmost" three times. See
/// PINV-18.
fn resolved_target(
    requested: Option<&AppIdentifier>,
    resolved: &ResolvedApp,
) -> Option<AppIdentifier> {
    match requested {
        Some(app) => Some(app.clone()),
        None => resolved.identifier(),
    }
}

/// Reads the display rect a request's fractions resolve against.
fn display_rect<W>(window_manager: &W, display_id: Option<u32>) -> Result<PixelRect, PolarizeError>
where
    W: WindowManager,
{
    window_manager.resolve_target_rect(&ScreenshotTarget::Screen { display_id })
}

/// # PINV-29: a window tool reports the frame it re-read, never the frame it requested
///
/// - Always: [`perform_set_window_frame`] and [`perform_window_action`]
///   call [`WindowController::list_windows`] again after their last
///   write. Every geometric and boolean field of the response comes from
///   that second read. `set_window_frame` reports the requested frame in
///   its own separate fields, and compares the two into
///   `applied_exactly`.
/// - Because: an app is free to ignore a write. Every AppKit window has
///   a minimum size, many have a maximum, and a document window can
///   refuse a move that would put its title bar under the menu bar.
///   `AXUIElementSetAttributeValue` returns `kAXErrorSuccess` for all of
///   those: the app took the message and applied its own policy. A tool
///   that echoed the request would report a 200-pixel-wide window that
///   is really 480 pixels wide. An agent cannot see the screen, so it
///   has no way to catch that.
/// - If violated: an agent lays out three windows, believes the layout
///   succeeded, and every later coordinate it computes from that belief
///   is wrong.
pub fn perform_set_window_frame<W, C>(
    window_manager: &W,
    control: &C,
    request: &SetWindowFrameRequest,
) -> Result<SetWindowFrameResponse, PolarizeError>
where
    W: WindowManager,
    C: WindowController,
{
    if request.position.is_none() && request.size.is_none() {
        return Err(WindowControlError::NothingToDo.into());
    }
    // Resolve the geometry before touching the app. A bad display id or
    // an out-of-range fraction then fails with nothing written.
    let display = display_rect(window_manager, request.display_id)?;
    let (position, size) = frame_to_pixels(request.position, request.size, request.units, display)?;

    let (resolved, windows) = control.list_windows(request.app.as_ref())?;
    let title = request.window_title.as_deref();
    let index = select_window(&windows, title, request.window_index, &resolved.name)?;
    let target = resolved_target(request.app.as_ref(), &resolved);

    control.apply_window_writes(target.as_ref(), index, &plan_frame_writes(position, size))?;

    // PINV-29: read the app again, and report what it really did.
    let (_, after) = control.list_windows(target.as_ref())?;
    let found = reselect_window(
        &after,
        title,
        request.window_index,
        index,
        &windows[index],
        &resolved.name,
    )
    .ok_or_else(|| WindowControlError::NoWindows {
        app: resolved.name.clone(),
    })?;
    let window = &after[found];

    Ok(SetWindowFrameResponse {
        app_name: resolved.name,
        window: window_state(found, window, display),
        requested_position: position.map(|point| FramePoint {
            x: point.x,
            y: point.y,
        }),
        requested_size: size.map(|size| FrameSize {
            width: size.width,
            height: size.height,
        }),
        applied_exactly: frame_applied_exactly(window.rect, position, size),
    })
}

/// Carries out one [`WindowAction`], then re-reads the window (PINV-29).
///
/// A `focus` call activates the app first, through
/// [`WindowManager::activate_app`]. Raising a window inside a background
/// app leaves the app in the background, so the window still takes no
/// keyboard input. This is the same rule PINV-14 applies to `keyboard`.
pub fn perform_window_action<W, C>(
    window_manager: &W,
    control: &C,
    request: &WindowActionRequest,
) -> Result<WindowActionResponse, PolarizeError>
where
    W: WindowManager,
    C: WindowController,
{
    let display = display_rect(window_manager, request.display_id)?;
    let (resolved, before) = control.list_windows(request.app.as_ref())?;
    let title = request.window_title.as_deref();
    let index = select_window(&before, title, request.window_index, &resolved.name)?;
    let plan = plan_action_writes(request.action, index, &before[index])?;
    let target = resolved_target(request.app.as_ref(), &resolved);

    if request.action == WindowAction::Focus
        && let Some(app) = target.as_ref()
    {
        window_manager.activate_app(app)?;
    }
    control.apply_window_writes(target.as_ref(), index, &plan)?;

    // PINV-29: read the app again, and report what it really did.
    //
    // A close can take the whole app with it. Preview, TextEdit and many
    // other document apps exit when their last window closes, so this
    // read finds no app at all. That is the close succeeding. Any other
    // action reaching the same error is a real failure, because nothing
    // it does should end the app.
    let after = match control.list_windows(target.as_ref()) {
        Ok((_, after)) => after,
        Err(PolarizeError::AppNotFound(_)) if request.action == WindowAction::Close => Vec::new(),
        Err(err) => return Err(err),
    };
    let closed = request.action == WindowAction::Close && after.len() < before.len();
    let window = if closed {
        None
    } else {
        reselect_window(
            &after,
            title,
            request.window_index,
            index,
            &before[index],
            &resolved.name,
        )
        .map(|found| window_state(found, &after[found], display))
    };

    Ok(WindowActionResponse {
        app_name: resolved.name,
        action: request.action,
        window,
        window_count: after.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coords::PixelRect;
    use std::cell::RefCell;

    // ---- fixtures -----------------------------------------------------
    #[test]
    fn closing_the_last_window_of_an_app_that_quits_is_not_an_error() {
        // Preview, TextEdit and many document apps exit with their last
        // window. The PINV-29 re-read then cannot find the app at all.
        // That is the close succeeding, not failing.
        struct QuitsOnClose;
        impl WindowController for QuitsOnClose {
            fn list_windows(
                &self,
                _app: Option<&AppIdentifier>,
            ) -> Result<(ResolvedApp, Vec<WindowInfo>), PolarizeError> {
                thread_local! {
                    static CALLS: RefCell<usize> = const { RefCell::new(0) };
                }
                let first = CALLS.with(|c| {
                    let mut c = c.borrow_mut();
                    *c += 1;
                    *c == 1
                });
                if first {
                    Ok((
                        ResolvedApp {
                            name: "Preview".to_string(),
                            bundle_id: None,
                        },
                        vec![window(Some("Doc"))],
                    ))
                } else {
                    Err(PolarizeError::AppNotFound("Preview".to_string()))
                }
            }

            fn apply_window_writes(
                &self,
                _app: Option<&AppIdentifier>,
                _index: usize,
                _writes: &[WindowWrite],
            ) -> Result<(), PolarizeError> {
                Ok(())
            }
        }

        let request = WindowActionRequest {
            app: None,
            window_title: None,
            window_index: None,
            action: WindowAction::Close,
            display_id: None,
        };

        let response = perform_window_action(&FakeWindows::new(), &QuitsOnClose, &request).unwrap();

        assert_eq!(response.action, WindowAction::Close);
        assert!(response.window.is_none(), "the window is gone");
        assert_eq!(response.window_count, 0);
    }

    #[test]
    fn every_write_of_one_action_lands_on_one_window() {
        // `focus` on a minimized window plans three writes. Un-minimizing
        // reorders `AXWindows`, so re-resolving the index between writes
        // would send the last two to a different window.
        let windows = vec![
            window(Some("Front")),
            WindowInfo {
                minimized: true,
                ..window(Some("Target"))
            },
        ];
        let control = ReorderingController::new(windows);
        let request = WindowActionRequest {
            app: None,
            window_title: Some("Target".to_string()),
            window_index: None,
            action: WindowAction::Focus,
            display_id: None,
        };

        perform_window_action(&FakeWindows::new(), &control, &request).unwrap();

        assert_eq!(
            control.hits(),
            vec!["Target", "Target", "Target"],
            "a reorder mid-action moved the target"
        );
    }

    #[test]
    fn a_reorder_does_not_make_the_response_describe_another_window() {
        // Addressed by index, with no title. After the raise, index 1
        // names a different window, so re-reading by index alone would
        // report the wrong one.
        let windows = vec![
            window(Some("Front")),
            WindowInfo {
                rect: rect(10.0, 10.0, 200.0, 200.0),
                ..window(Some("Target"))
            },
        ];
        let control = ReorderingController::new(windows);
        let request = WindowActionRequest {
            app: None,
            window_title: None,
            window_index: Some(1),
            action: WindowAction::Focus,
            display_id: None,
        };

        let response = perform_window_action(&FakeWindows::new(), &control, &request).unwrap();

        let window = response.window.expect("the window still exists");
        assert_eq!(
            window.title.as_deref(),
            Some("Target"),
            "the response named the window that moved into the old index"
        );
    }

    const DISPLAY: PixelRect = PixelRect {
        origin: PixelPoint { x: 0.0, y: 0.0 },
        size: PixelSize {
            width: 1000.0,
            height: 800.0,
        },
    };

    fn rect(x: f64, y: f64, width: f64, height: f64) -> PixelRect {
        PixelRect {
            origin: PixelPoint { x, y },
            size: PixelSize { width, height },
        }
    }

    fn window(title: Option<&str>) -> WindowInfo {
        WindowInfo {
            title: title.map(ToString::to_string),
            rect: rect(0.0, 0.0, 400.0, 300.0),
            minimized: false,
            main: false,
            focused: false,
            full_screen: Some(false),
        }
    }

    /// A fake [`WindowController`]. It records every call, and it swaps
    /// in a second window list after the first write, so a test can tell
    /// a re-read apart from an echo of the request.
    /// A controller whose window list REORDERS after the first write,
    /// the way a real `AXRaise` or un-minimize does. Every write is
    /// recorded with the window it actually landed on.
    struct ReorderingController {
        windows: RefCell<Vec<WindowInfo>>,
        hits: RefCell<Vec<String>>,
    }

    impl ReorderingController {
        fn new(windows: Vec<WindowInfo>) -> Self {
            Self {
                windows: RefCell::new(windows),
                hits: RefCell::new(Vec::new()),
            }
        }

        fn hits(&self) -> Vec<String> {
            self.hits.borrow().clone()
        }
    }

    impl WindowController for ReorderingController {
        fn list_windows(
            &self,
            _app: Option<&AppIdentifier>,
        ) -> Result<(ResolvedApp, Vec<WindowInfo>), PolarizeError> {
            Ok((
                ResolvedApp {
                    name: "TestApp".to_string(),
                    bundle_id: None,
                },
                self.windows.borrow().clone(),
            ))
        }

        fn apply_window_writes(
            &self,
            _app: Option<&AppIdentifier>,
            index: usize,
            writes: &[WindowWrite],
        ) -> Result<(), PolarizeError> {
            // Resolve the index ONCE, as a real implementation must.
            let target = self.windows.borrow()[index].clone();
            for write in writes {
                self.hits
                    .borrow_mut()
                    .push(target.title.clone().unwrap_or_default());
                if matches!(write, WindowWrite::SetMinimized(false) | WindowWrite::Raise) {
                    // The app brings the touched window to the front.
                    let mut windows = self.windows.borrow_mut();
                    let at = windows
                        .iter()
                        .position(|w| w.title == target.title)
                        .expect("target still present");
                    let moved = windows.remove(at);
                    windows.insert(0, moved);
                }
            }
            Ok(())
        }
    }

    struct FakeController {
        before: Vec<WindowInfo>,
        after: Option<Vec<WindowInfo>>,
        resolved: ResolvedApp,
        lists: RefCell<Vec<Option<AppIdentifier>>>,
        writes: RefCell<Vec<(Option<AppIdentifier>, usize, WindowWrite)>>,
        fail_write: Option<String>,
    }

    impl FakeController {
        fn new(before: Vec<WindowInfo>) -> Self {
            Self {
                before,
                after: None,
                resolved: ResolvedApp {
                    name: "TestApp".to_string(),
                    bundle_id: None,
                },
                lists: RefCell::new(Vec::new()),
                writes: RefCell::new(Vec::new()),
                fail_write: None,
            }
        }

        fn reading_back(mut self, after: Vec<WindowInfo>) -> Self {
            self.after = Some(after);
            self
        }

        fn with_bundle_id(mut self, bundle_id: &str) -> Self {
            self.resolved.bundle_id = Some(bundle_id.to_string());
            self
        }

        fn failing(mut self, message: &str) -> Self {
            self.fail_write = Some(message.to_string());
            self
        }

        fn list_count(&self) -> usize {
            self.lists.borrow().len()
        }

        fn plan(&self) -> Vec<WindowWrite> {
            self.writes.borrow().iter().map(|call| call.2).collect()
        }
    }

    impl WindowController for FakeController {
        fn list_windows(
            &self,
            app: Option<&AppIdentifier>,
        ) -> Result<(ResolvedApp, Vec<WindowInfo>), PolarizeError> {
            self.lists.borrow_mut().push(app.cloned());
            let first_read = self.lists.borrow().len() == 1;
            let windows = match (&self.after, first_read) {
                (Some(after), false) => after.clone(),
                _ => self.before.clone(),
            };
            Ok((self.resolved.clone(), windows))
        }

        fn apply_window_writes(
            &self,
            app: Option<&AppIdentifier>,
            index: usize,
            writes: &[WindowWrite],
        ) -> Result<(), PolarizeError> {
            for write in writes {
                self.writes.borrow_mut().push((app.cloned(), index, *write));
                if let Some(message) = &self.fail_write {
                    return Err(PolarizeError::Platform(message.clone()));
                }
            }
            Ok(())
        }
    }

    /// A fake [`WindowManager`] that reports one display and records
    /// every `activate_app` call.
    struct FakeWindows {
        display: PixelRect,
        activated: RefCell<Vec<AppIdentifier>>,
    }

    impl FakeWindows {
        fn new() -> Self {
            Self {
                display: DISPLAY,
                activated: RefCell::new(Vec::new()),
            }
        }

        fn with_display(display: PixelRect) -> Self {
            Self {
                display,
                activated: RefCell::new(Vec::new()),
            }
        }
    }

    impl WindowManager for FakeWindows {
        fn activate_app(&self, app: &AppIdentifier) -> Result<(), PolarizeError> {
            self.activated.borrow_mut().push(app.clone());
            Ok(())
        }

        fn resolve_target_rect(
            &self,
            _target: &ScreenshotTarget,
        ) -> Result<PixelRect, PolarizeError> {
            Ok(self.display)
        }

        fn resolve_target_pid(
            &self,
            _target: &ScreenshotTarget,
        ) -> Result<Option<i32>, PolarizeError> {
            // Not exercised here: this module's tests are about window
            // writes (position/size/minimize/…), not the `tap` pid-post
            // path (PINV-47).
            Ok(None)
        }
    }

    fn frame_request() -> SetWindowFrameRequest {
        SetWindowFrameRequest {
            position: Some(FramePoint { x: 0.0, y: 0.0 }),
            size: Some(FrameSize {
                width: 0.5,
                height: 1.0,
            }),
            ..SetWindowFrameRequest::default()
        }
    }

    fn action_request(action: WindowAction) -> WindowActionRequest {
        WindowActionRequest {
            app: None,
            window_title: None,
            window_index: None,
            action,
            display_id: None,
        }
    }

    // ---- PINV-28: select_window ---------------------------------------

    #[test]
    fn no_title_and_no_index_picks_the_frontmost_window() {
        let windows = vec![window(Some("Front")), window(Some("Back"))];
        assert_eq!(select_window(&windows, None, None, "TestApp").unwrap(), 0);
    }

    #[test]
    fn no_title_with_an_index_picks_that_window() {
        let windows = vec![window(Some("Front")), window(Some("Back"))];
        assert_eq!(
            select_window(&windows, None, Some(1), "TestApp").unwrap(),
            1
        );
    }

    #[test]
    fn a_title_picks_the_one_window_that_carries_it() {
        let windows = vec![window(Some("Front")), window(Some("Notes"))];
        assert_eq!(
            select_window(&windows, Some("Notes"), None, "TestApp").unwrap(),
            1
        );
    }

    #[test]
    fn a_title_that_matches_nothing_is_refused_and_names_the_real_titles() {
        let windows = vec![window(Some("Front")), window(None)];
        let err = select_window(&windows, Some("Nope"), None, "TestApp").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("Nope"), "{message}");
        assert!(message.contains("\"Front\""), "{message}");
        assert!(message.contains("<untitled>"), "{message}");
    }

    #[test]
    fn a_title_that_matches_several_windows_is_ambiguous() {
        let windows = vec![
            window(Some("Untitled")),
            window(Some("Other")),
            window(Some("Untitled")),
        ];
        let err = select_window(&windows, Some("Untitled"), None, "TestApp").unwrap_err();
        assert!(
            matches!(err, WindowControlError::Ambiguous { count: 2, .. }),
            "{err}"
        );
        assert!(err.to_string().contains("window_index"), "{err}");
    }

    #[test]
    fn an_index_disambiguates_two_windows_that_share_a_title() {
        let windows = vec![
            window(Some("Untitled")),
            window(Some("Other")),
            window(Some("Untitled")),
        ];
        assert_eq!(
            select_window(&windows, Some("Untitled"), Some(0), "TestApp").unwrap(),
            0
        );
        assert_eq!(
            select_window(&windows, Some("Untitled"), Some(1), "TestApp").unwrap(),
            2
        );
    }

    #[test]
    fn an_index_past_the_matching_windows_is_refused() {
        let windows = vec![window(Some("Untitled")), window(Some("Untitled"))];
        let err = select_window(&windows, Some("Untitled"), Some(2), "TestApp").unwrap_err();
        assert_eq!(
            err,
            WindowControlError::IndexOutOfRange { index: 2, count: 2 }
        );
    }

    #[test]
    fn an_index_past_the_whole_list_is_refused() {
        let windows = vec![window(Some("Front"))];
        let err = select_window(&windows, None, Some(3), "TestApp").unwrap_err();
        assert_eq!(
            err,
            WindowControlError::IndexOutOfRange { index: 3, count: 1 }
        );
    }

    #[test]
    fn an_app_with_no_windows_is_refused_and_names_the_app() {
        let err = select_window(&[], None, None, "TestApp").unwrap_err();
        assert!(err.to_string().contains("TestApp"), "{err}");
    }

    #[test]
    fn a_title_must_match_exactly_not_by_prefix() {
        let windows = vec![window(Some("Notes — Draft"))];
        assert!(select_window(&windows, Some("Notes"), None, "TestApp").is_err());
        assert_eq!(
            select_window(&windows, Some("Notes — Draft"), None, "TestApp").unwrap(),
            0
        );
    }

    #[test]
    fn an_untitled_window_is_still_addressable_by_index() {
        let windows = vec![window(None), window(None)];
        assert_eq!(
            select_window(&windows, None, Some(1), "TestApp").unwrap(),
            1
        );
    }

    // ---- PINV-28: full screen refusal ---------------------------------

    #[test]
    fn a_window_without_ax_full_screen_refuses_both_full_screen_actions() {
        let mut target = window(Some("Plain"));
        target.full_screen = None;

        for action in [WindowAction::EnterFullScreen, WindowAction::ExitFullScreen] {
            let err = plan_action_writes(action, 0, &target).unwrap_err();
            let message = err.to_string();
            assert!(message.contains("AXFullScreen"), "{message}");
            assert!(message.contains("Plain"), "names the window: {message}");
        }
    }

    #[test]
    fn a_window_that_publishes_ax_full_screen_plans_the_write() {
        let target = window(Some("Plain"));
        assert_eq!(
            plan_action_writes(WindowAction::EnterFullScreen, 0, &target).unwrap(),
            vec![WindowWrite::SetFullScreen(true)]
        );
        assert_eq!(
            plan_action_writes(WindowAction::ExitFullScreen, 0, &target).unwrap(),
            vec![WindowWrite::SetFullScreen(false)]
        );
    }

    #[test]
    fn the_full_screen_refusal_never_reaches_the_controller() {
        let mut target = window(Some("Plain"));
        target.full_screen = None;
        let control = FakeController::new(vec![target]);
        let windows = FakeWindows::new();

        let err = perform_window_action(
            &windows,
            &control,
            &action_request(WindowAction::EnterFullScreen),
        )
        .unwrap_err();

        assert!(err.to_string().contains("AXFullScreen"), "{err}");
        assert!(control.writes.borrow().is_empty());
    }

    // ---- action plans -------------------------------------------------

    #[test]
    fn minimize_and_restore_write_the_minimized_flag() {
        let target = window(Some("W"));
        assert_eq!(
            plan_action_writes(WindowAction::Minimize, 0, &target).unwrap(),
            vec![WindowWrite::SetMinimized(true)]
        );
        assert_eq!(
            plan_action_writes(WindowAction::Restore, 0, &target).unwrap(),
            vec![WindowWrite::SetMinimized(false)]
        );
    }

    #[test]
    fn focus_makes_the_window_main_then_raises_it() {
        let target = window(Some("W"));
        assert_eq!(
            plan_action_writes(WindowAction::Focus, 0, &target).unwrap(),
            vec![WindowWrite::SetMain(true), WindowWrite::Raise]
        );
    }

    #[test]
    fn focus_on_a_minimized_window_restores_it_first() {
        // `AXRaise` cannot bring a window out of the Dock. Without this
        // first write, focus reports success and nothing appears.
        let mut target = window(Some("W"));
        target.minimized = true;
        assert_eq!(
            plan_action_writes(WindowAction::Focus, 0, &target).unwrap(),
            vec![
                WindowWrite::SetMinimized(false),
                WindowWrite::SetMain(true),
                WindowWrite::Raise
            ]
        );
    }

    #[test]
    fn close_presses_the_close_button() {
        let target = window(Some("W"));
        assert_eq!(
            plan_action_writes(WindowAction::Close, 0, &target).unwrap(),
            vec![WindowWrite::Close]
        );
    }

    // ---- frame plans --------------------------------------------------

    #[test]
    fn a_move_and_resize_writes_the_position_around_the_size() {
        let position = PixelPoint { x: 10.0, y: 20.0 };
        let size = PixelSize {
            width: 300.0,
            height: 200.0,
        };
        assert_eq!(
            plan_frame_writes(Some(position), Some(size)),
            vec![
                WindowWrite::SetPosition(position),
                WindowWrite::SetSize(size),
                WindowWrite::SetPosition(position),
            ]
        );
    }

    #[test]
    fn a_move_alone_writes_one_position() {
        let position = PixelPoint { x: 10.0, y: 20.0 };
        assert_eq!(
            plan_frame_writes(Some(position), None),
            vec![WindowWrite::SetPosition(position)]
        );
    }

    #[test]
    fn a_resize_alone_writes_one_size() {
        let size = PixelSize {
            width: 300.0,
            height: 200.0,
        };
        assert_eq!(
            plan_frame_writes(None, Some(size)),
            vec![WindowWrite::SetSize(size)]
        );
    }

    #[test]
    fn a_plan_for_neither_writes_nothing() {
        assert_eq!(plan_frame_writes(None, None), Vec::new());
    }

    // ---- fraction to pixel conversion ---------------------------------

    #[test]
    fn a_fraction_request_resolves_against_the_display() {
        let (position, size) = frame_to_pixels(
            Some(FramePoint { x: 0.5, y: 0.0 }),
            Some(FrameSize {
                width: 0.5,
                height: 1.0,
            }),
            FrameUnits::Fraction,
            DISPLAY,
        )
        .unwrap();
        assert_eq!(position, Some(PixelPoint { x: 500.0, y: 0.0 }));
        assert_eq!(
            size,
            Some(PixelSize {
                width: 500.0,
                height: 800.0
            })
        );
    }

    #[test]
    fn a_fraction_request_adds_the_display_origin() {
        // A second display does not start at the global origin. Dropping
        // its origin would move the window onto the primary display —
        // the same failure PINV-4 documents for `tap`.
        let display = rect(1000.0, -200.0, 1000.0, 800.0);
        let (position, _) = frame_to_pixels(
            Some(FramePoint { x: 0.25, y: 0.5 }),
            None,
            FrameUnits::Fraction,
            display,
        )
        .unwrap();
        assert_eq!(
            position,
            Some(PixelPoint {
                x: 1250.0,
                y: 200.0
            })
        );
    }

    #[test]
    fn a_pixel_request_passes_straight_through() {
        let (position, size) = frame_to_pixels(
            Some(FramePoint { x: 1280.0, y: 44.0 }),
            Some(FrameSize {
                width: 1280.0,
                height: 720.0,
            }),
            FrameUnits::Pixels,
            DISPLAY,
        )
        .unwrap();
        assert_eq!(position, Some(PixelPoint { x: 1280.0, y: 44.0 }));
        assert_eq!(
            size,
            Some(PixelSize {
                width: 1280.0,
                height: 720.0
            })
        );
    }

    #[test]
    fn a_pixel_position_may_be_negative() {
        // A display above or left of the main display holds negative
        // global coordinates.
        let (position, _) = frame_to_pixels(
            Some(FramePoint {
                x: -500.0,
                y: -100.0,
            }),
            None,
            FrameUnits::Pixels,
            DISPLAY,
        )
        .unwrap();
        assert_eq!(
            position,
            Some(PixelPoint {
                x: -500.0,
                y: -100.0
            })
        );
    }

    #[test]
    fn a_fraction_outside_the_range_is_refused() {
        // PINV-1: a caller that passed pixels by mistake gets an error,
        // not a window jammed into the top-left corner.
        let err = frame_to_pixels(
            Some(FramePoint { x: 1280.0, y: 44.0 }),
            None,
            FrameUnits::Fraction,
            DISPLAY,
        )
        .unwrap_err();
        assert!(matches!(err, PolarizeError::Coord(_)), "{err}");
        assert!(err.to_string().contains("1280"), "{err}");
    }

    #[test]
    fn a_fraction_size_outside_the_range_is_refused() {
        let err = frame_to_pixels(
            None,
            Some(FrameSize {
                width: 1.5,
                height: 0.5,
            }),
            FrameUnits::Fraction,
            DISPLAY,
        )
        .unwrap_err();
        assert!(matches!(err, PolarizeError::Coord(_)), "{err}");
    }

    #[test]
    fn a_zero_size_is_refused_in_both_units() {
        for units in [FrameUnits::Fraction, FrameUnits::Pixels] {
            let err = frame_to_pixels(
                None,
                Some(FrameSize {
                    width: 0.0,
                    height: 0.5,
                }),
                units,
                DISPLAY,
            )
            .unwrap_err();
            assert!(
                matches!(
                    err,
                    PolarizeError::Coord(CoordError::NonPositiveSize { .. })
                ),
                "{err}"
            );
        }
    }

    #[test]
    fn a_negative_pixel_size_is_refused() {
        let err = frame_to_pixels(
            None,
            Some(FrameSize {
                width: -10.0,
                height: 100.0,
            }),
            FrameUnits::Pixels,
            DISPLAY,
        )
        .unwrap_err();
        assert!(
            matches!(
                err,
                PolarizeError::Coord(CoordError::NonPositiveSize { .. })
            ),
            "{err}"
        );
    }

    // ---- normalized reporting ------------------------------------------

    #[test]
    fn a_window_frame_normalizes_against_the_display() {
        let frame = normalized_frame(rect(500.0, 0.0, 500.0, 800.0), DISPLAY).unwrap();
        assert_eq!(
            frame,
            NormalizedFrame {
                x: 0.5,
                y: 0.0,
                width: 0.5,
                height: 1.0
            }
        );
    }

    #[test]
    fn a_window_off_the_display_reports_an_unclamped_fraction() {
        // A report must stay truthful. PINV-8 clamps an element frame
        // because that frame feeds a clickable tap point; this one does
        // not.
        let frame = normalized_frame(rect(-100.0, 0.0, 2000.0, 800.0), DISPLAY).unwrap();
        assert_eq!(frame.x, -0.1);
        assert_eq!(frame.width, 2.0);
    }

    #[test]
    fn a_window_on_a_second_display_normalizes_against_that_display() {
        let display = rect(1000.0, 0.0, 1000.0, 800.0);
        let frame = normalized_frame(rect(1500.0, 400.0, 500.0, 400.0), display).unwrap();
        assert_eq!(
            frame,
            NormalizedFrame {
                x: 0.5,
                y: 0.5,
                width: 0.5,
                height: 0.5
            }
        );
    }

    #[test]
    fn a_non_positive_display_reports_no_fraction_instead_of_dividing_by_zero() {
        assert_eq!(
            normalized_frame(rect(0.0, 0.0, 10.0, 10.0), rect(0.0, 0.0, 0.0, 800.0)),
            None
        );
        assert_eq!(
            normalized_frame(rect(0.0, 0.0, 10.0, 10.0), rect(0.0, 0.0, 800.0, 0.0)),
            None
        );
    }

    // ---- applied_exactly ------------------------------------------------

    #[test]
    fn an_honored_frame_reports_applied_exactly() {
        assert!(frame_applied_exactly(
            rect(10.0, 20.0, 300.0, 200.0),
            Some(PixelPoint { x: 10.0, y: 20.0 }),
            Some(PixelSize {
                width: 300.0,
                height: 200.0
            })
        ));
    }

    #[test]
    fn a_clamped_size_reports_not_applied_exactly() {
        assert!(!frame_applied_exactly(
            rect(10.0, 20.0, 480.0, 200.0),
            Some(PixelPoint { x: 10.0, y: 20.0 }),
            Some(PixelSize {
                width: 300.0,
                height: 200.0
            })
        ));
    }

    #[test]
    fn a_refused_move_reports_not_applied_exactly() {
        assert!(!frame_applied_exactly(
            rect(10.0, 44.0, 300.0, 200.0),
            Some(PixelPoint { x: 10.0, y: 0.0 }),
            None
        ));
    }

    #[test]
    fn a_sub_pixel_rounding_difference_still_counts_as_exact() {
        assert!(frame_applied_exactly(
            rect(10.5, 20.0, 300.0, 199.5),
            Some(PixelPoint { x: 10.0, y: 20.0 }),
            Some(PixelSize {
                width: 300.0,
                height: 200.0
            })
        ));
    }

    #[test]
    fn a_request_that_named_no_size_says_nothing_about_the_size_it_got() {
        assert!(frame_applied_exactly(
            rect(10.0, 20.0, 999.0, 999.0),
            Some(PixelPoint { x: 10.0, y: 20.0 }),
            None
        ));
    }

    // ---- PINV-29: set_window_frame end to end ---------------------------

    #[test]
    fn set_window_frame_reports_the_frame_it_read_back_not_the_one_it_asked_for() {
        // The app clamps the window to 480 pixels wide. The response
        // must say 480, and it must say the request was not honored.
        let mut clamped = window(Some("Doc"));
        clamped.rect = rect(0.0, 0.0, 480.0, 800.0);
        let control = FakeController::new(vec![window(Some("Doc"))]).reading_back(vec![clamped]);
        let windows = FakeWindows::new();

        let response = perform_set_window_frame(&windows, &control, &frame_request()).unwrap();

        assert_eq!(response.window.pixel_width, 480.0);
        assert_eq!(
            response.requested_size,
            Some(FrameSize {
                width: 500.0,
                height: 800.0
            })
        );
        assert!(!response.applied_exactly);
        assert_eq!(
            response.window.frame,
            Some(NormalizedFrame {
                x: 0.0,
                y: 0.0,
                width: 0.48,
                height: 1.0
            })
        );
    }

    #[test]
    fn set_window_frame_reads_the_window_list_two_times() {
        // One read to address the window, one to report the result.
        let control = FakeController::new(vec![window(Some("Doc"))]);
        let windows = FakeWindows::new();

        perform_set_window_frame(&windows, &control, &frame_request()).unwrap();

        assert_eq!(control.list_count(), 2);
    }

    #[test]
    fn set_window_frame_reads_back_after_the_last_write() {
        // The re-read must follow every write, not sit between them.
        let control = FakeController::new(vec![window(Some("Doc"))]);
        let windows = FakeWindows::new();

        perform_set_window_frame(&windows, &control, &frame_request()).unwrap();

        assert_eq!(control.writes.borrow().len(), 3);
        assert_eq!(control.list_count(), 2);
    }

    #[test]
    fn set_window_frame_honors_a_frame_the_app_accepted() {
        let mut exact = window(Some("Doc"));
        exact.rect = rect(0.0, 0.0, 500.0, 800.0);
        let control = FakeController::new(vec![window(Some("Doc"))]).reading_back(vec![exact]);
        let windows = FakeWindows::new();

        let response = perform_set_window_frame(&windows, &control, &frame_request()).unwrap();

        assert!(response.applied_exactly);
        assert_eq!(response.app_name, "TestApp");
    }

    #[test]
    fn set_window_frame_refuses_a_call_that_names_neither_a_position_nor_a_size() {
        let control = FakeController::new(vec![window(Some("Doc"))]);
        let windows = FakeWindows::new();

        let err = perform_set_window_frame(&windows, &control, &SetWindowFrameRequest::default())
            .unwrap_err();

        assert!(err.to_string().contains("position"), "{err}");
        assert_eq!(control.list_count(), 0, "it must not touch the app");
    }

    #[test]
    fn set_window_frame_refuses_a_bad_fraction_before_it_reads_the_app() {
        let control = FakeController::new(vec![window(Some("Doc"))]);
        let windows = FakeWindows::new();
        let request = SetWindowFrameRequest {
            position: Some(FramePoint { x: 2.0, y: 0.0 }),
            ..SetWindowFrameRequest::default()
        };

        let err = perform_set_window_frame(&windows, &control, &request).unwrap_err();

        assert!(matches!(err, PolarizeError::Coord(_)), "{err}");
        assert_eq!(control.list_count(), 0, "it must not touch the app");
        assert!(control.writes.borrow().is_empty());
    }

    #[test]
    fn set_window_frame_sends_the_resolved_plan_to_the_controller() {
        let control = FakeController::new(vec![window(Some("Doc"))]);
        let windows = FakeWindows::new();

        perform_set_window_frame(&windows, &control, &frame_request()).unwrap();

        let position = PixelPoint { x: 0.0, y: 0.0 };
        let size = PixelSize {
            width: 500.0,
            height: 800.0,
        };
        assert_eq!(
            control.plan(),
            vec![
                WindowWrite::SetPosition(position),
                WindowWrite::SetSize(size),
                WindowWrite::SetPosition(position),
            ]
        );
    }

    #[test]
    fn set_window_frame_addresses_the_window_the_title_named() {
        let control = FakeController::new(vec![window(Some("Front")), window(Some("Notes"))]);
        let windows = FakeWindows::new();
        let request = SetWindowFrameRequest {
            window_title: Some("Notes".to_string()),
            ..frame_request()
        };

        let response = perform_set_window_frame(&windows, &control, &request).unwrap();

        assert!(control.writes.borrow().iter().all(|call| call.1 == 1));
        assert_eq!(response.window.index, 1);
        assert_eq!(response.window.title.as_deref(), Some("Notes"));
    }

    #[test]
    fn set_window_frame_refuses_an_ambiguous_title_before_any_write() {
        let control = FakeController::new(vec![window(Some("Untitled")), window(Some("Untitled"))]);
        let windows = FakeWindows::new();
        let request = SetWindowFrameRequest {
            window_title: Some("Untitled".to_string()),
            ..frame_request()
        };

        let err = perform_set_window_frame(&windows, &control, &request).unwrap_err();

        assert!(matches!(err, PolarizeError::WindowNotFound(_)), "{err}");
        assert!(control.writes.borrow().is_empty());
    }

    #[test]
    fn set_window_frame_pins_the_write_to_the_app_the_platform_resolved() {
        // PINV-18: a request naming no app must not re-resolve
        // "frontmost" for every later call.
        let control =
            FakeController::new(vec![window(Some("Doc"))]).with_bundle_id("com.apple.TextEdit");
        let windows = FakeWindows::new();

        perform_set_window_frame(&windows, &control, &frame_request()).unwrap();

        let pinned = Some(AppIdentifier {
            bundle_id: Some("com.apple.TextEdit".to_string()),
            app_name: None,
        });
        assert_eq!(control.writes.borrow()[0].0, pinned);
        assert_eq!(
            control.lists.borrow()[0],
            None,
            "the first read resolves it"
        );
        assert_eq!(control.lists.borrow()[1], pinned, "the re-read is pinned");
    }

    #[test]
    fn set_window_frame_keeps_the_callers_own_app_identifier() {
        let control = FakeController::new(vec![window(Some("Doc"))]);
        let windows = FakeWindows::new();
        let named = AppIdentifier {
            bundle_id: Some("com.example.App".to_string()),
            app_name: None,
        };
        let request = SetWindowFrameRequest {
            app: Some(named.clone()),
            ..frame_request()
        };

        perform_set_window_frame(&windows, &control, &request).unwrap();

        assert_eq!(control.writes.borrow()[0].0, Some(named.clone()));
        assert_eq!(control.lists.borrow()[0], Some(named));
    }

    #[test]
    fn set_window_frame_reports_a_pixel_request_in_pixels() {
        let mut placed = window(Some("Doc"));
        placed.rect = rect(1280.0, 44.0, 1280.0, 720.0);
        let control = FakeController::new(vec![window(Some("Doc"))]).reading_back(vec![placed]);
        let windows = FakeWindows::new();
        let request = SetWindowFrameRequest {
            units: FrameUnits::Pixels,
            position: Some(FramePoint { x: 1280.0, y: 44.0 }),
            size: Some(FrameSize {
                width: 1280.0,
                height: 720.0,
            }),
            ..SetWindowFrameRequest::default()
        };

        let response = perform_set_window_frame(&windows, &control, &request).unwrap();

        assert!(response.applied_exactly);
        assert_eq!(response.window.pixel_x, 1280.0);
        assert_eq!(
            response.requested_position,
            Some(FramePoint { x: 1280.0, y: 44.0 })
        );
    }

    #[test]
    fn set_window_frame_reports_a_platform_write_failure() {
        let control = FakeController::new(vec![window(Some("Doc"))]).failing("AXError -25204");
        let windows = FakeWindows::new();

        let err = perform_set_window_frame(&windows, &control, &frame_request()).unwrap_err();

        assert!(err.to_string().contains("-25204"), "{err}");
    }

    // ---- PINV-29: window_action end to end ------------------------------

    #[test]
    fn window_action_reports_the_state_it_read_back() {
        let mut minimized = window(Some("Doc"));
        minimized.minimized = true;
        let control = FakeController::new(vec![window(Some("Doc"))]).reading_back(vec![minimized]);
        let windows = FakeWindows::new();

        let response =
            perform_window_action(&windows, &control, &action_request(WindowAction::Minimize))
                .unwrap();

        assert!(response.window.unwrap().minimized);
        assert_eq!(control.plan(), vec![WindowWrite::SetMinimized(true)]);
        assert_eq!(control.list_count(), 2);
    }

    #[test]
    fn window_action_reports_a_write_the_app_ignored() {
        // The app kept the window on screen. The response must say so
        // rather than echo the request.
        let control = FakeController::new(vec![window(Some("Doc"))]);
        let windows = FakeWindows::new();

        let response =
            perform_window_action(&windows, &control, &action_request(WindowAction::Minimize))
                .unwrap();

        assert!(!response.window.unwrap().minimized);
    }

    #[test]
    fn a_focus_call_activates_the_app_before_it_raises_the_window() {
        // PINV-14's rule: raising a window inside a background app
        // leaves the app in the background.
        let control =
            FakeController::new(vec![window(Some("Doc"))]).with_bundle_id("com.apple.TextEdit");
        let windows = FakeWindows::new();

        perform_window_action(&windows, &control, &action_request(WindowAction::Focus)).unwrap();

        assert_eq!(
            windows.activated.borrow().as_slice(),
            &[AppIdentifier {
                bundle_id: Some("com.apple.TextEdit".to_string()),
                app_name: None,
            }]
        );
        assert_eq!(
            control.plan(),
            vec![WindowWrite::SetMain(true), WindowWrite::Raise]
        );
    }

    #[test]
    fn a_minimize_call_does_not_activate_the_app() {
        let control = FakeController::new(vec![window(Some("Doc"))]);
        let windows = FakeWindows::new();

        perform_window_action(&windows, &control, &action_request(WindowAction::Minimize)).unwrap();

        assert!(windows.activated.borrow().is_empty());
    }

    #[test]
    fn a_close_that_removed_the_window_reports_no_window() {
        let control = FakeController::new(vec![window(Some("Doc")), window(Some("Other"))])
            .reading_back(vec![window(Some("Other"))]);
        let windows = FakeWindows::new();

        let response =
            perform_window_action(&windows, &control, &action_request(WindowAction::Close))
                .unwrap();

        assert_eq!(response.window, None);
        assert_eq!(response.window_count, 1);
    }

    #[test]
    fn a_close_the_app_refused_still_reports_the_window() {
        // A "save changes?" sheet keeps the window open. Reporting
        // success there would be a lie.
        let control = FakeController::new(vec![window(Some("Doc"))]);
        let windows = FakeWindows::new();

        let response =
            perform_window_action(&windows, &control, &action_request(WindowAction::Close))
                .unwrap();

        assert_eq!(
            response.window.map(|w| w.title),
            Some(Some("Doc".to_string()))
        );
        assert_eq!(response.window_count, 1);
    }

    #[test]
    fn a_reordered_window_list_is_re_read_by_title_not_by_index() {
        // `AXRaise` moves a window to the front of `AXWindows`. A
        // re-read by index would then report the wrong window.
        let control = FakeController::new(vec![window(Some("Front")), window(Some("Notes"))])
            .reading_back(vec![window(Some("Notes")), window(Some("Front"))]);
        let windows = FakeWindows::new();
        let request = WindowActionRequest {
            window_title: Some("Notes".to_string()),
            ..action_request(WindowAction::Focus)
        };

        let response = perform_window_action(&windows, &control, &request).unwrap();

        let state = response.window.unwrap();
        assert_eq!(state.title.as_deref(), Some("Notes"));
        assert_eq!(state.index, 0, "it moved to the front");
    }

    #[test]
    fn window_action_refuses_a_title_that_matches_nothing_before_any_write() {
        let control = FakeController::new(vec![window(Some("Front"))]);
        let windows = FakeWindows::new();
        let request = WindowActionRequest {
            window_title: Some("Nope".to_string()),
            ..action_request(WindowAction::Close)
        };

        let err = perform_window_action(&windows, &control, &request).unwrap_err();

        assert!(matches!(err, PolarizeError::WindowNotFound(_)), "{err}");
        assert!(control.writes.borrow().is_empty());
    }

    #[test]
    fn window_action_reports_a_platform_write_failure() {
        let control = FakeController::new(vec![window(Some("Doc"))]).failing("AXError -25205");
        let windows = FakeWindows::new();

        let err = perform_window_action(&windows, &control, &action_request(WindowAction::Restore))
            .unwrap_err();

        assert!(err.to_string().contains("-25205"), "{err}");
    }

    #[test]
    fn window_action_normalizes_the_reported_frame_against_the_named_display() {
        let display = rect(1000.0, 0.0, 1000.0, 800.0);
        let mut placed = window(Some("Doc"));
        placed.rect = rect(1500.0, 400.0, 500.0, 400.0);
        let control = FakeController::new(vec![window(Some("Doc"))]).reading_back(vec![placed]);
        let windows = FakeWindows::with_display(display);
        let request = WindowActionRequest {
            display_id: Some(7),
            ..action_request(WindowAction::Focus)
        };

        let response = perform_window_action(&windows, &control, &request).unwrap();

        assert_eq!(
            response.window.unwrap().frame,
            Some(NormalizedFrame {
                x: 0.5,
                y: 0.5,
                width: 0.5,
                height: 0.5
            })
        );
    }

    // ---- error mapping ---------------------------------------------------

    #[test]
    fn an_addressing_refusal_travels_as_window_not_found() {
        let err: PolarizeError = WindowControlError::NoMatch {
            title: "Nope".to_string(),
            available: "\"Front\"".to_string(),
        }
        .into();
        assert!(matches!(err, PolarizeError::WindowNotFound(_)), "{err}");
        assert!(err.to_string().contains("Nope"));
    }

    #[test]
    fn a_refusal_to_act_travels_as_a_platform_error() {
        let err: PolarizeError = WindowControlError::FullScreenUnsupported {
            window: "index=0, title=\"Doc\"".to_string(),
        }
        .into();
        assert!(matches!(err, PolarizeError::Platform(_)), "{err}");
        assert!(err.to_string().contains("AXFullScreen"));

        let err: PolarizeError = WindowControlError::NothingToDo.into();
        assert!(matches!(err, PolarizeError::Platform(_)), "{err}");
    }

    // ---- wire contract ----------------------------------------------------

    #[test]
    fn the_frame_request_round_trips_through_json() {
        let request = SetWindowFrameRequest {
            app: Some(AppIdentifier {
                bundle_id: Some("com.apple.TextEdit".to_string()),
                app_name: None,
            }),
            window_title: Some("Untitled".to_string()),
            window_index: Some(1),
            units: FrameUnits::Pixels,
            position: Some(FramePoint { x: 10.0, y: 20.0 }),
            size: Some(FrameSize {
                width: 300.0,
                height: 200.0,
            }),
            display_id: Some(3),
        };
        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(
            serde_json::from_str::<SetWindowFrameRequest>(&json).unwrap(),
            request
        );
    }

    #[test]
    fn a_frame_request_with_only_a_size_deserializes_and_defaults_to_fractions() {
        let request: SetWindowFrameRequest =
            serde_json::from_str(r#"{"size":{"width":0.5,"height":1.0}}"#).unwrap();
        assert_eq!(request.units, FrameUnits::Fraction);
        assert_eq!(request.app, None);
        assert_eq!(request.position, None);
    }

    #[test]
    fn the_action_request_round_trips_through_json() {
        let request = WindowActionRequest {
            app: None,
            window_title: None,
            window_index: None,
            action: WindowAction::EnterFullScreen,
            display_id: None,
        };
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("enter_full_screen"), "{json}");
        assert_eq!(
            serde_json::from_str::<WindowActionRequest>(&json).unwrap(),
            request
        );
    }

    #[test]
    fn an_action_request_needs_only_an_action() {
        let request: WindowActionRequest =
            serde_json::from_str(r#"{"action":"minimize"}"#).unwrap();
        assert_eq!(request.action, WindowAction::Minimize);
        assert_eq!(request.window_title, None);
    }

    #[test]
    fn the_frame_response_round_trips_through_json() {
        let response = SetWindowFrameResponse {
            app_name: "TestApp".to_string(),
            window: WindowState {
                index: 0,
                title: Some("Doc".to_string()),
                pixel_x: 0.0,
                pixel_y: 0.0,
                pixel_width: 480.0,
                pixel_height: 800.0,
                frame: Some(NormalizedFrame {
                    x: 0.0,
                    y: 0.0,
                    width: 0.48,
                    height: 1.0,
                }),
                minimized: false,
                main: true,
                focused: true,
                full_screen: Some(false),
            },
            requested_position: Some(FramePoint { x: 0.0, y: 0.0 }),
            requested_size: Some(FrameSize {
                width: 500.0,
                height: 800.0,
            }),
            applied_exactly: false,
        };
        let json = serde_json::to_string(&response).unwrap();
        assert_eq!(
            serde_json::from_str::<SetWindowFrameResponse>(&json).unwrap(),
            response
        );
    }

    #[test]
    fn the_action_response_round_trips_through_json() {
        let response = WindowActionResponse {
            app_name: "TestApp".to_string(),
            action: WindowAction::Close,
            window: None,
            window_count: 0,
        };
        let json = serde_json::to_string(&response).unwrap();
        assert_eq!(
            serde_json::from_str::<WindowActionResponse>(&json).unwrap(),
            response
        );
    }
}
