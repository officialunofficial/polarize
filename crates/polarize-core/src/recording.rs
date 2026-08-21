//! Flow recording: a bounded capture of the real input a user makes.
//!
//! `polarize` can already post synthetic input (`tap`, `keyboard`). This
//! module reads input the other way round. It records what a human does,
//! as a list of plain records, so a caller can replay the same flow
//! later.
//!
//! ## Why a bounded recording, and not a subscription
//!
//! `polarize` is an `rmcp` stdio server. It answers discrete tool calls.
//! It has no channel to push an event stream into, and no client
//! subscribed to one. So the tool call itself is the delivery point,
//! exactly as it is for `await_workspace_event` (PINV-36).
//!
//! `record_flow` therefore records for a bounded time, and returns every
//! event it saw in its own response. It stops at the requested duration,
//! or earlier when the event budget fills. Input that happens while no
//! `record_flow` call is running is not recorded, and this documentation
//! says so rather than implying a complete history.
//!
//! ## Why Input Monitoring, and not Accessibility
//!
//! A listen-only `CGEventTap` needs the Input Monitoring TCC grant
//! (`kTCCServiceListenEvent`, `CGPreflightListenEventAccess`). That is a
//! different grant from the Accessibility one `tap` and `keyboard`
//! already hold, and it lives in a different System Settings pane. A
//! caller who is told to grant the wrong pane cannot fix the problem, so
//! [`crate::permission::PermissionKind::InputMonitoring`] names the pane
//! it really needs.
//!
//! ## Why a recording redacts typed text by default
//!
//! A recording captures every keystroke, and some of those keystrokes
//! are passwords. So a default recording holds the key *events* without
//! the characters they produced. A caller opts in to the characters with
//! `capture_text: true`. See PINV-40, and the same reasoning behind
//! [`crate::script::redact_source`] (PINV-22).
//!
//! ## What is pure here
//!
//! Everything in this module is pure. The event translation, the
//! normalization of a click point, the offset arithmetic, the clamps,
//! the redaction, and the report of a tap that macOS disabled are all
//! plain functions over plain data, and `cargo test -p polarize-core`
//! covers them. `polarize-macos` only runs the tap and hands back
//! [`RawTapEvent`] values. See PINV-39.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::PolarizeError;
use crate::schema::{Modifier, NamedKey};
use crate::traits::DisplayLister;
use crate::workspace::DisplayInfo;

// ---- defaults and clamps ------------------------------------------------

/// How long a recording runs when the caller names no duration.
pub const DEFAULT_DURATION_MS: u64 = 5_000;

/// The longest recording a caller may ask for. A tool call that blocks
/// for longer than a minute looks like a hung server to an MCP client.
/// A caller who wants more makes a second call.
pub const MAX_DURATION_MS: u64 = 60_000;

/// How many events a recording keeps when the caller names no limit.
pub const DEFAULT_MAX_EVENTS: usize = 500;

/// The most events one recording may keep. Every event travels back
/// inside one MCP response, so an unbounded list is an unbounded
/// response.
pub const MAX_EVENT_LIMIT: usize = 5_000;

// ---- the raw `CGEvent` vocabulary ---------------------------------------
//
// These are the `CGEventType` and `CGEventFlags` values `polarize-macos`
// reads off a real event. They are literal numbers here, not extern
// symbols, for the reason `crate::workspace_events` gives for the
// `NSWorkspace` notification names: `polarize-core` has no macOS
// dependency at all, the values are long-stable public API, and a
// literal cannot link against a wrong symbol. `polarize-macos` asserts
// each one against the matching `objc2_core_graphics` constant.

/// `kCGEventLeftMouseDown`.
pub const RAW_LEFT_MOUSE_DOWN: u32 = 1;
/// `kCGEventLeftMouseUp`.
pub const RAW_LEFT_MOUSE_UP: u32 = 2;
/// `kCGEventRightMouseDown`.
pub const RAW_RIGHT_MOUSE_DOWN: u32 = 3;
/// `kCGEventRightMouseUp`.
pub const RAW_RIGHT_MOUSE_UP: u32 = 4;
/// `kCGEventMouseMoved`.
pub const RAW_MOUSE_MOVED: u32 = 5;
/// `kCGEventLeftMouseDragged`.
pub const RAW_LEFT_MOUSE_DRAGGED: u32 = 6;
/// `kCGEventRightMouseDragged`.
pub const RAW_RIGHT_MOUSE_DRAGGED: u32 = 7;
/// `kCGEventKeyDown`.
pub const RAW_KEY_DOWN: u32 = 10;
/// `kCGEventKeyUp`.
pub const RAW_KEY_UP: u32 = 11;
/// `kCGEventFlagsChanged`.
pub const RAW_FLAGS_CHANGED: u32 = 12;
/// `kCGEventScrollWheel`.
pub const RAW_SCROLL_WHEEL: u32 = 22;
/// `kCGEventOtherMouseDown`.
pub const RAW_OTHER_MOUSE_DOWN: u32 = 25;
/// `kCGEventOtherMouseUp`.
pub const RAW_OTHER_MOUSE_UP: u32 = 26;
/// `kCGEventOtherMouseDragged`.
pub const RAW_OTHER_MOUSE_DRAGGED: u32 = 27;

/// `kCGEventTapDisabledByTimeout`. macOS turns a tap off when its
/// callback takes too long. See PINV-39.
pub const RAW_TAP_DISABLED_BY_TIMEOUT: u32 = 4_294_967_294;

/// `kCGEventTapDisabledByUserInput`. macOS turns a tap off when the user
/// asks it to, through the secure-input path. See PINV-39.
pub const RAW_TAP_DISABLED_BY_USER_INPUT: u32 = 4_294_967_295;

/// `kCGEventFlagMaskShift`.
pub const FLAG_MASK_SHIFT: u64 = 0x0002_0000;
/// `kCGEventFlagMaskControl`.
pub const FLAG_MASK_CONTROL: u64 = 0x0004_0000;
/// `kCGEventFlagMaskAlternate`, the Option key.
pub const FLAG_MASK_ALTERNATE: u64 = 0x0008_0000;
/// `kCGEventFlagMaskCommand`.
pub const FLAG_MASK_COMMAND: u64 = 0x0010_0000;

/// The order modifiers appear in every recorded event.
///
/// The order is fixed so two recordings of the same flow compare equal.
/// It matches the order [`Modifier`] declares its variants in.
pub const MODIFIER_ORDER: [Modifier; 4] = [
    Modifier::Command,
    Modifier::Shift,
    Modifier::Option,
    Modifier::Control,
];

// ---- errors -------------------------------------------------------------

/// A `record_flow` request this module refused.
///
/// `polarize-core` has no error variant of its own for a bad recording
/// request, and `crate::error` belongs to another change. So these
/// convert to [`PolarizeError::Platform`] at the boundary, through the
/// [`From`] implementation below.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RecordingError {
    /// The caller asked for a recording of zero milliseconds.
    #[error("duration_ms must be at least 1; a recording of 0 ms can capture nothing")]
    EmptyDuration,

    /// The caller asked for a recording that keeps no events.
    #[error("max_events must be at least 1; a recording that keeps 0 events reports nothing")]
    EmptyEventLimit,

    /// The platform listed no display.
    #[error("no display was reported, so a recorded click has nothing to normalize against")]
    NoDisplays,
}

// ---- what crosses the platform boundary ---------------------------------

/// The settings the tap runs with, after defaults and clamps.
///
/// `polarize-core` resolves this, and `polarize-macos` obeys it. Two
/// fields reach into the tap itself. `record_mouse_moves` decides
/// whether the callback keeps a move at all, because a move stream fills
/// the event budget in well under a second. `capture_text` decides
/// whether the callback reads the characters of a key event at all, so a
/// default recording never holds a password even in memory (PINV-40).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordingPlan {
    /// How long the tap runs, in milliseconds.
    pub duration_ms: u64,
    /// The most events the tap collects before it stops early.
    pub max_events: usize,
    /// Whether the tap keeps mouse moves and drags.
    pub record_mouse_moves: bool,
    /// Whether the tap reads the characters a key event produced.
    pub capture_text: bool,
}

/// One event, exactly as the tap read it off a real `CGEvent`.
///
/// Every field is a plain number or a plain string. Nothing here is
/// decided: the translation into a [`RecordedEvent`] is pure logic, and
/// [`translate_event`] does it in this crate. See PINV-39.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RawTapEvent {
    /// The `CGEventType` raw value.
    pub event_type: u32,
    /// The event's own timestamp, in nanoseconds, on the same clock
    /// [`RawRecording::started_ns`] reads.
    pub timestamp_ns: u64,
    /// `kCGKeyboardEventKeycode`.
    pub key_code: u16,
    /// The `CGEventFlags` bits.
    pub flags: u64,
    /// `kCGMouseEventClickState`.
    pub click_count: i64,
    /// `CGEventGetLocation().x`, in the global display pixel space.
    pub pixel_x: f64,
    /// `CGEventGetLocation().y`, in the global display pixel space.
    pub pixel_y: f64,
    /// `kCGScrollWheelEventDeltaAxis2`, the horizontal scroll.
    pub scroll_delta_x: i64,
    /// `kCGScrollWheelEventDeltaAxis1`, the vertical scroll.
    pub scroll_delta_y: i64,
    /// The characters this key event produced. Always `None` unless the
    /// caller opted in with `capture_text`.
    pub characters: Option<String>,
}

/// Everything one tap run collected.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RawRecording {
    /// Every event the tap kept, in the order it saw them.
    pub events: Vec<RawTapEvent>,
    /// The clock reading when the tap started, in nanoseconds.
    pub started_ns: u64,
    /// How long the tap really ran, in milliseconds.
    pub elapsed_ms: u64,
    /// How many events the tap declined to keep, because its budget was
    /// already full.
    ///
    /// Only the tap can count these. It stops collecting the moment
    /// `max_events` fills, so the events it turned away never reach
    /// `polarize-core` at all. Without this count a truncated recording
    /// looks exactly like a complete one. See PINV-39.
    pub dropped_events: usize,
}

/// Runs a listen-only `CGEventTap` for a bounded time. `polarize-macos`
/// implements this over `CGEventTapCreate` and `CFRunLoop`.
///
/// The trait sits here, next to the logic it feeds, for the same reason
/// [`crate::workspace_events::WorkspaceNotificationWaiter`] does: the MCP
/// server never calls it directly.
///
/// An implementation decides nothing. It runs the tap, it collects
/// [`RawTapEvent`] values, and it hands them back. It must take
/// `kCGEventTapOptionListenOnly`, and it must never modify or swallow an
/// event (PINV-39).
pub trait FlowRecorder {
    /// Records for `plan.duration_ms`, or until `plan.max_events`
    /// events arrive, whichever happens first.
    ///
    /// An implementation appends a tap-disabled notice to
    /// [`RawRecording::events`] like any other event, and re-enables the
    /// tap. This crate counts those notices and reports them; see
    /// [`assemble_recording`] and PINV-39.
    fn record_raw_events(&self, plan: &RecordingPlan) -> Result<RawRecording, PolarizeError>;
}

// ---- the recorded event model -------------------------------------------

/// What one recorded event is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RecordedEventKind {
    /// A key went down.
    KeyDown,
    /// A key came up.
    KeyUp,
    /// A modifier key changed state.
    FlagsChanged,
    /// A mouse button went down.
    MouseDown,
    /// A mouse button came up.
    MouseUp,
    /// The mouse moved with no button held.
    MouseMoved,
    /// The mouse moved with a button held.
    MouseDragged,
    /// The scroll wheel or a trackpad scroll moved.
    Scroll,
}

/// Which mouse button an event names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Right,
    /// Any button beyond the first two.
    Other,
}

/// Why a recording stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The requested duration elapsed.
    Duration,
    /// The event budget filled first.
    EventLimit,
}

/// One event of a recorded flow.
///
/// Every field a given kind does not use is `None`. A click carries a
/// point and a click count. A key press carries a key code, and a
/// [`NamedKey`] when one matches. A scroll carries two deltas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RecordedEvent {
    pub kind: RecordedEventKind,
    /// Milliseconds from the start of the recording.
    pub offset_ms: u64,
    /// The modifiers held while this event happened, in
    /// [`MODIFIER_ORDER`].
    pub modifiers: Vec<Modifier>,
    /// The named key this key code matches, when one does. `keyboard`
    /// replays this key directly.
    pub key: Option<NamedKey>,
    /// The macOS virtual key code. Present on every key event, even one
    /// no [`NamedKey`] names.
    pub key_code: Option<u16>,
    /// The characters this key event produced. `None` unless the caller
    /// opted in with `capture_text` (PINV-40).
    pub text: Option<String>,
    /// Whether this module withheld the characters of a key event.
    pub redacted: bool,
    pub button: Option<MouseButton>,
    /// `1` for a single click, `2` for a double click.
    pub click_count: Option<u8>,
    /// The click point, as a fraction of its display's width.
    pub x: Option<f64>,
    /// The click point, as a fraction of its display's height.
    pub y: Option<f64>,
    /// The `CGDirectDisplayID` the point was normalized against. `tap`
    /// takes this as its `display_id`.
    pub display_id: Option<u32>,
    pub scroll_delta_x: Option<i64>,
    pub scroll_delta_y: Option<i64>,
}

// ---- request and response -----------------------------------------------

/// Records real user input for a bounded time.
///
/// This is a plain object, not a tagged union, so the MCP server needs
/// no schema patch for it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct RecordFlowRequest {
    /// How long to record, in milliseconds. Defaults to
    /// [`DEFAULT_DURATION_MS`], clamped to [`MAX_DURATION_MS`].
    #[serde(default)]
    pub duration_ms: Option<u64>,
    /// Stop early after this many events. Defaults to
    /// [`DEFAULT_MAX_EVENTS`], clamped to [`MAX_EVENT_LIMIT`].
    #[serde(default)]
    pub max_events: Option<usize>,
    /// Record mouse moves and drags too. Defaults to `false`, because a
    /// move stream fills the event budget in well under a second.
    #[serde(default)]
    pub record_mouse_moves: Option<bool>,
    /// Record the characters each key event produced. Defaults to
    /// `false`. A recording captures real keystrokes, and some of those
    /// are passwords. See PINV-40.
    #[serde(default)]
    pub capture_text: Option<bool>,
}

/// Everything one `record_flow` call recorded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RecordFlowResponse {
    pub events: Vec<RecordedEvent>,
    pub event_count: usize,
    /// The duration this call really asked the tap for, after the
    /// default and the clamp.
    pub requested_duration_ms: u64,
    /// How long the tap really ran.
    pub recorded_ms: u64,
    pub stopped_because: StopReason,
    /// Whether this recording holds typed characters.
    pub text_captured: bool,
    /// How many times macOS disabled the tap on a timeout, and this
    /// module re-enabled it. See PINV-39.
    pub tap_disabled_by_timeout: u32,
    /// How many times macOS disabled the tap on user input, and this
    /// module re-enabled it.
    pub tap_disabled_by_user_input: u32,
    /// How many events the budget dropped.
    pub dropped_events: usize,
    /// Whether this recording holds every event the user made. `false`
    /// means the flow is short, and the warnings say why.
    pub complete: bool,
    /// One line per reason this recording is not complete.
    pub warnings: Vec<String>,
}

// ---- pure logic ---------------------------------------------------------

impl RecordingPlan {
    /// Applies the defaults and the clamps this module documents.
    ///
    /// A zero duration and a zero event budget are both refused, rather
    /// than raised to one. Each one asks for a recording that can hold
    /// nothing, so the caller has a bug, and an error names it.
    pub fn resolve(request: &RecordFlowRequest) -> Result<Self, RecordingError> {
        let duration_ms = request.duration_ms.unwrap_or(DEFAULT_DURATION_MS);
        if duration_ms == 0 {
            return Err(RecordingError::EmptyDuration);
        }
        let max_events = request.max_events.unwrap_or(DEFAULT_MAX_EVENTS);
        if max_events == 0 {
            return Err(RecordingError::EmptyEventLimit);
        }
        Ok(Self {
            duration_ms: duration_ms.min(MAX_DURATION_MS),
            max_events: max_events.min(MAX_EVENT_LIMIT),
            record_mouse_moves: request.record_mouse_moves.unwrap_or(false),
            capture_text: request.capture_text.unwrap_or(false),
        })
    }
}

/// Whether this raw type is a mouse move or a drag.
///
/// `polarize-macos` calls this inside the tap callback. A move stream
/// runs at the display's refresh rate, so it fills any event budget in
/// well under a second. Dropping a move at the tap, and not here, is
/// what keeps a default recording full of clicks and keystrokes.
pub fn is_mouse_move_type(event_type: u32) -> bool {
    matches!(
        event_type,
        RAW_MOUSE_MOVED
            | RAW_LEFT_MOUSE_DRAGGED
            | RAW_RIGHT_MOUSE_DRAGGED
            | RAW_OTHER_MOUSE_DRAGGED
    )
}

/// Whether this raw type is a key event.
///
/// `polarize-macos` calls this inside the tap callback, to decide
/// whether reading the characters is even a sensible question.
/// `CGEventKeyboardGetUnicodeString` is a keyboard API, and a mouse
/// event has no characters to give it. See PINV-40.
pub fn is_key_type(event_type: u32) -> bool {
    matches!(event_type, RAW_KEY_DOWN | RAW_KEY_UP)
}

/// Whether this raw type is one of the two tap-disabled notices.
pub fn is_tap_disabled_type(event_type: u32) -> bool {
    matches!(
        event_type,
        RAW_TAP_DISABLED_BY_TIMEOUT | RAW_TAP_DISABLED_BY_USER_INPUT
    )
}

/// The [`RecordedEventKind`] a raw type carries, or `None` for a type
/// this module does not record.
pub fn kind_for_raw_type(event_type: u32) -> Option<RecordedEventKind> {
    match event_type {
        RAW_KEY_DOWN => Some(RecordedEventKind::KeyDown),
        RAW_KEY_UP => Some(RecordedEventKind::KeyUp),
        RAW_FLAGS_CHANGED => Some(RecordedEventKind::FlagsChanged),
        RAW_LEFT_MOUSE_DOWN | RAW_RIGHT_MOUSE_DOWN | RAW_OTHER_MOUSE_DOWN => {
            Some(RecordedEventKind::MouseDown)
        }
        RAW_LEFT_MOUSE_UP | RAW_RIGHT_MOUSE_UP | RAW_OTHER_MOUSE_UP => {
            Some(RecordedEventKind::MouseUp)
        }
        RAW_MOUSE_MOVED => Some(RecordedEventKind::MouseMoved),
        RAW_LEFT_MOUSE_DRAGGED | RAW_RIGHT_MOUSE_DRAGGED | RAW_OTHER_MOUSE_DRAGGED => {
            Some(RecordedEventKind::MouseDragged)
        }
        RAW_SCROLL_WHEEL => Some(RecordedEventKind::Scroll),
        _ => None,
    }
}

/// The button a raw type names, or `None` for an event with no button.
pub fn button_for_raw_type(event_type: u32) -> Option<MouseButton> {
    match event_type {
        RAW_LEFT_MOUSE_DOWN | RAW_LEFT_MOUSE_UP | RAW_LEFT_MOUSE_DRAGGED => Some(MouseButton::Left),
        RAW_RIGHT_MOUSE_DOWN | RAW_RIGHT_MOUSE_UP | RAW_RIGHT_MOUSE_DRAGGED => {
            Some(MouseButton::Right)
        }
        RAW_OTHER_MOUSE_DOWN | RAW_OTHER_MOUSE_UP | RAW_OTHER_MOUSE_DRAGGED => {
            Some(MouseButton::Other)
        }
        _ => None,
    }
}

/// The mask bit one [`Modifier`] sets in a `CGEventFlags` value.
///
/// This is the inverse of `polarize_macos::keymap::modifiers_to_cgevent_flags`
/// (PINV-6). The two tables must agree, so a flow `record_flow` captured
/// replays through `keyboard` with the same modifiers held.
fn mask_for_modifier(modifier: Modifier) -> u64 {
    match modifier {
        Modifier::Command => FLAG_MASK_COMMAND,
        Modifier::Shift => FLAG_MASK_SHIFT,
        Modifier::Option => FLAG_MASK_ALTERNATE,
        Modifier::Control => FLAG_MASK_CONTROL,
    }
}

/// The modifiers a `CGEventFlags` bit set holds, in [`MODIFIER_ORDER`].
///
/// A bit no [`Modifier`] names is dropped. Caps lock and the numeric
/// keypad flag are the common cases, and `keyboard` can replay neither.
pub fn modifiers_from_flags(flags: u64) -> Vec<Modifier> {
    MODIFIER_ORDER
        .into_iter()
        .filter(|modifier| flags & mask_for_modifier(*modifier) != 0)
        .collect()
}

/// The [`NamedKey`] a macOS virtual key code names, when one does.
///
/// This is the inverse of `polarize_macos::keymap::named_key_to_keycode`,
/// and the two tables must agree. The values are the same physical-position
/// codes that module documents (`kVK_*` in Carbon's `Events.h`). A code no
/// [`NamedKey`] names is normal: `NamedKey` covers no printable key at all.
pub fn named_key_from_key_code(key_code: u16) -> Option<NamedKey> {
    match key_code {
        0x24 => Some(NamedKey::Return),
        0x30 => Some(NamedKey::Tab),
        0x31 => Some(NamedKey::Space),
        0x33 => Some(NamedKey::Delete),
        0x35 => Some(NamedKey::Escape),
        0x7B => Some(NamedKey::ArrowLeft),
        0x7C => Some(NamedKey::ArrowRight),
        0x7D => Some(NamedKey::ArrowDown),
        0x7E => Some(NamedKey::ArrowUp),
        _ => None,
    }
}

/// Holds `value` inside `0.0..=1.0`. A non-finite value becomes `0.0`.
///
/// This is the same rule [`crate::ax::NormalizedFrame`] applies (PINV-8).
fn clamp_unit(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Whether a display's frame holds this global pixel point.
fn display_holds(display: &DisplayInfo, pixel_x: f64, pixel_y: f64) -> bool {
    let frame = display.frame;
    pixel_x >= frame.x
        && pixel_x < frame.x + frame.width
        && pixel_y >= frame.y
        && pixel_y < frame.y + frame.height
}

/// The display a global pixel point sits on, and the point as fractions
/// of that display.
///
/// The display that holds the point wins. A point no display holds falls
/// back to the main display, and the fractions clamp into `0.0..=1.0`
/// there. Both parts matter. A `tap` request refuses a fraction outside
/// that range (PINV-1), so an unclamped record would be a record no
/// caller can replay. See PINV-39.
pub fn normalize_to_display(
    pixel_x: f64,
    pixel_y: f64,
    displays: &[DisplayInfo],
) -> Option<(u32, f64, f64)> {
    let display = displays
        .iter()
        .find(|display| display_holds(display, pixel_x, pixel_y))
        .or_else(|| displays.iter().find(|display| display.is_main))
        .or_else(|| displays.first())?;

    let frame = display.frame;
    if frame.width <= 0.0 || frame.height <= 0.0 {
        // A display of no size normalizes nothing. Report the point at
        // that display's own origin rather than a division by zero.
        return Some((display.display_id, 0.0, 0.0));
    }
    Some((
        display.display_id,
        clamp_unit((pixel_x - frame.x) / frame.width),
        clamp_unit((pixel_y - frame.y) / frame.height),
    ))
}

/// Turns one raw event into a recorded one.
///
/// Returns `None` for a raw type this module does not record, for a tap
/// notice, and for a move the caller did not ask for.
pub fn translate_event(
    raw: &RawTapEvent,
    started_ns: u64,
    displays: &[DisplayInfo],
    plan: &RecordingPlan,
) -> Option<RecordedEvent> {
    if !plan.record_mouse_moves && is_mouse_move_type(raw.event_type) {
        return None;
    }
    let kind = kind_for_raw_type(raw.event_type)?;

    // A `CGEvent` carries the hardware timestamp of the real keypress,
    // so an event already in flight can be older than the tap itself.
    // A saturating subtraction reports that as offset zero.
    let offset_ms = raw.timestamp_ns.saturating_sub(started_ns) / 1_000_000;

    let is_key_event = matches!(
        kind,
        RecordedEventKind::KeyDown | RecordedEventKind::KeyUp | RecordedEventKind::FlagsChanged
    );
    let is_mouse_event = matches!(
        kind,
        RecordedEventKind::MouseDown
            | RecordedEventKind::MouseUp
            | RecordedEventKind::MouseMoved
            | RecordedEventKind::MouseDragged
    );
    let is_button_event = matches!(
        kind,
        RecordedEventKind::MouseDown | RecordedEventKind::MouseUp
    );
    let is_scroll = kind == RecordedEventKind::Scroll;

    // The redaction runs here as well as in the tap. The tap must not
    // read the characters at all without the opt-in, and this drops
    // whatever arrives anyway. One bug in `polarize-macos` then still
    // cannot put a password in a response. See PINV-40.
    //
    // Only a key event may carry text. `CGEventKeyboardGetUnicodeString`
    // is a keyboard API, so anything it yields for a mouse or scroll
    // event is meaningless — and a click record with a `text` field
    // would tell a caller the user typed during a click.
    let text = if plan.capture_text && is_key_event {
        raw.characters.clone()
    } else {
        None
    };
    let redacted = is_key_event && !plan.capture_text;

    let (display_id, x, y) = if is_mouse_event || is_scroll {
        match normalize_to_display(raw.pixel_x, raw.pixel_y, displays) {
            Some((id, x, y)) => (Some(id), Some(x), Some(y)),
            None => (None, None, None),
        }
    } else {
        (None, None, None)
    };

    Some(RecordedEvent {
        kind,
        offset_ms,
        modifiers: modifiers_from_flags(raw.flags),
        key: if is_key_event {
            named_key_from_key_code(raw.key_code)
        } else {
            None
        },
        key_code: if is_key_event {
            Some(raw.key_code)
        } else {
            None
        },
        text,
        redacted,
        button: button_for_raw_type(raw.event_type),
        click_count: if is_button_event {
            // The window server reports click state `0` on a click it
            // did not group with another. `tap` reads that as one
            // click (PINV-7), so a record says the same.
            Some(u8::try_from(raw.click_count.max(1)).unwrap_or(u8::MAX))
        } else {
            None
        },
        x,
        y,
        display_id,
        scroll_delta_x: if is_scroll {
            Some(raw.scroll_delta_x)
        } else {
            None
        },
        scroll_delta_y: if is_scroll {
            Some(raw.scroll_delta_y)
        } else {
            None
        },
    })
}

/// Turns one raw recording into the tool's response.
///
/// This is where a tap that macOS disabled becomes a number a caller can
/// read. A recording that silently lost input is worse than one that
/// fails: the caller believes a short flow is the whole flow. See
/// PINV-39.
pub fn assemble_recording(
    raw: &RawRecording,
    displays: &[DisplayInfo],
    plan: &RecordingPlan,
) -> RecordFlowResponse {
    let mut events: Vec<RecordedEvent> = Vec::new();
    let mut tap_disabled_by_timeout = 0;
    let mut tap_disabled_by_user_input = 0;

    for event in &raw.events {
        match event.event_type {
            RAW_TAP_DISABLED_BY_TIMEOUT => {
                tap_disabled_by_timeout += 1;
                continue;
            }
            RAW_TAP_DISABLED_BY_USER_INPUT => {
                tap_disabled_by_user_input += 1;
                continue;
            }
            _ => {}
        }
        if let Some(mut recorded) = translate_event(event, raw.started_ns, displays, plan) {
            // Two events can arrive out of order, and a flow replayed
            // from decreasing offsets runs its steps in the wrong
            // order. So an offset never goes backwards.
            if let Some(previous) = events.last() {
                recorded.offset_ms = recorded.offset_ms.max(previous.offset_ms);
            }
            events.push(recorded);
        }
    }

    // What the tap turned away, plus anything this function trimmed.
    // The first is the real number in production: the tap stops at the
    // budget, so it rarely hands over more than `max_events`.
    let dropped_events = raw.dropped_events + events.len().saturating_sub(plan.max_events);
    events.truncate(plan.max_events);

    let stopped_because = if events.len() >= plan.max_events {
        StopReason::EventLimit
    } else {
        StopReason::Duration
    };

    let mut warnings = Vec::new();
    if tap_disabled_by_timeout > 0 {
        warnings.push(format!(
            "macOS disabled the event tap {tap_disabled_by_timeout} time(s) on a timeout, and polarize turned it back on; input during that gap is missing"
        ));
    }
    if tap_disabled_by_user_input > 0 {
        warnings.push(format!(
            "macOS disabled the event tap {tap_disabled_by_user_input} time(s) on user input, and polarize turned it back on; input during that gap is missing"
        ));
    }
    if dropped_events > 0 {
        warnings.push(format!(
            "the event budget of {} filled, so {dropped_events} later event(s) are missing; raise max_events or shorten duration_ms",
            plan.max_events
        ));
    }

    RecordFlowResponse {
        event_count: events.len(),
        events,
        requested_duration_ms: plan.duration_ms,
        recorded_ms: raw.elapsed_ms,
        stopped_because,
        text_captured: plan.capture_text,
        tap_disabled_by_timeout,
        tap_disabled_by_user_input,
        dropped_events,
        complete: warnings.is_empty(),
        warnings,
    }
}

/// Records real user input, then reports it.
///
/// The display list is read first, and for one reason: a recorded click
/// is a fraction of the display it landed on (PINV-4, PINV-8). A bad
/// request is refused before the tap opens, so a caller's mistake never
/// asks the user for the Input Monitoring grant.
pub fn perform_record_flow<R, D>(
    recorder: &R,
    displays: &D,
    request: &RecordFlowRequest,
) -> Result<RecordFlowResponse, PolarizeError>
where
    R: FlowRecorder,
    D: DisplayLister,
{
    let plan = RecordingPlan::resolve(request)?;
    let displays = displays.displays()?;
    if displays.is_empty() {
        return Err(RecordingError::NoDisplays.into());
    }
    let raw = recorder.record_raw_events(&plan)?;
    Ok(assemble_recording(&raw, &displays, &plan))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::PixelFrame;
    use std::cell::RefCell;

    // ---- fakes ----------------------------------------------------------

    fn display(id: u32, x: f64, y: f64, width: f64, height: f64, is_main: bool) -> DisplayInfo {
        DisplayInfo {
            display_id: id,
            frame: PixelFrame::new(x, y, width, height),
            scale_factor: 2.0,
            is_main,
        }
    }

    fn one_display() -> Vec<DisplayInfo> {
        vec![display(1, 0.0, 0.0, 1000.0, 500.0, true)]
    }

    fn two_displays() -> Vec<DisplayInfo> {
        vec![
            display(1, 0.0, 0.0, 1000.0, 500.0, true),
            display(2, 1000.0, 0.0, 800.0, 400.0, false),
        ]
    }

    struct FakeDisplays(Vec<DisplayInfo>);

    impl DisplayLister for FakeDisplays {
        fn displays(&self) -> Result<Vec<DisplayInfo>, PolarizeError> {
            Ok(self.0.clone())
        }
    }

    struct FailingDisplays;

    impl DisplayLister for FailingDisplays {
        fn displays(&self) -> Result<Vec<DisplayInfo>, PolarizeError> {
            Err(PolarizeError::Platform("no window server".to_string()))
        }
    }

    struct FakeRecorder {
        recording: RawRecording,
        seen: RefCell<Vec<RecordingPlan>>,
    }

    impl FakeRecorder {
        fn new(recording: RawRecording) -> Self {
            Self {
                recording,
                seen: RefCell::new(Vec::new()),
            }
        }
    }

    impl FlowRecorder for FakeRecorder {
        fn record_raw_events(&self, plan: &RecordingPlan) -> Result<RawRecording, PolarizeError> {
            self.seen.borrow_mut().push(*plan);
            Ok(self.recording.clone())
        }
    }

    struct FailingRecorder;

    impl FlowRecorder for FailingRecorder {
        fn record_raw_events(&self, _plan: &RecordingPlan) -> Result<RawRecording, PolarizeError> {
            Err(PolarizeError::Platform("tap refused".to_string()))
        }
    }

    fn plan() -> RecordingPlan {
        RecordingPlan {
            duration_ms: 5_000,
            max_events: 500,
            record_mouse_moves: false,
            capture_text: false,
        }
    }

    fn raw(event_type: u32, timestamp_ns: u64) -> RawTapEvent {
        RawTapEvent {
            event_type,
            timestamp_ns,
            ..RawTapEvent::default()
        }
    }

    // ---- defaults and clamps --------------------------------------------

    #[test]
    fn an_absent_duration_takes_the_default() {
        let resolved = RecordingPlan::resolve(&RecordFlowRequest::default()).expect("resolve");
        assert_eq!(resolved.duration_ms, DEFAULT_DURATION_MS);
    }

    #[test]
    fn a_long_duration_is_clamped() {
        let request = RecordFlowRequest {
            duration_ms: Some(10 * MAX_DURATION_MS),
            ..RecordFlowRequest::default()
        };
        let resolved = RecordingPlan::resolve(&request).expect("resolve");
        assert_eq!(resolved.duration_ms, MAX_DURATION_MS);
    }

    #[test]
    fn a_zero_duration_is_refused() {
        let request = RecordFlowRequest {
            duration_ms: Some(0),
            ..RecordFlowRequest::default()
        };
        assert_eq!(
            RecordingPlan::resolve(&request).unwrap_err(),
            RecordingError::EmptyDuration
        );
    }

    #[test]
    fn an_absent_event_limit_takes_the_default() {
        let resolved = RecordingPlan::resolve(&RecordFlowRequest::default()).expect("resolve");
        assert_eq!(resolved.max_events, DEFAULT_MAX_EVENTS);
    }

    #[test]
    fn a_large_event_limit_is_clamped() {
        let request = RecordFlowRequest {
            max_events: Some(MAX_EVENT_LIMIT * 4),
            ..RecordFlowRequest::default()
        };
        let resolved = RecordingPlan::resolve(&request).expect("resolve");
        assert_eq!(resolved.max_events, MAX_EVENT_LIMIT);
    }

    #[test]
    fn a_zero_event_limit_is_refused() {
        let request = RecordFlowRequest {
            max_events: Some(0),
            ..RecordFlowRequest::default()
        };
        assert_eq!(
            RecordingPlan::resolve(&request).unwrap_err(),
            RecordingError::EmptyEventLimit
        );
    }

    #[test]
    fn text_capture_and_mouse_moves_are_both_off_by_default() {
        let resolved = RecordingPlan::resolve(&RecordFlowRequest::default()).expect("resolve");
        assert!(!resolved.capture_text, "text capture must be opt-in");
        assert!(!resolved.record_mouse_moves);
    }

    #[test]
    fn both_opt_ins_reach_the_plan() {
        let request = RecordFlowRequest {
            capture_text: Some(true),
            record_mouse_moves: Some(true),
            ..RecordFlowRequest::default()
        };
        let resolved = RecordingPlan::resolve(&request).expect("resolve");
        assert!(resolved.capture_text);
        assert!(resolved.record_mouse_moves);
    }

    // ---- the raw type table ---------------------------------------------

    #[test]
    fn moves_and_drags_are_the_high_frequency_types() {
        for event_type in [
            RAW_MOUSE_MOVED,
            RAW_LEFT_MOUSE_DRAGGED,
            RAW_RIGHT_MOUSE_DRAGGED,
            RAW_OTHER_MOUSE_DRAGGED,
        ] {
            assert!(is_mouse_move_type(event_type), "type {event_type}");
        }
        assert!(!is_mouse_move_type(RAW_LEFT_MOUSE_DOWN));
        assert!(!is_mouse_move_type(RAW_KEY_DOWN));
    }

    #[test]
    fn both_disable_notices_are_recognized() {
        assert!(is_tap_disabled_type(RAW_TAP_DISABLED_BY_TIMEOUT));
        assert!(is_tap_disabled_type(RAW_TAP_DISABLED_BY_USER_INPUT));
        assert!(!is_tap_disabled_type(RAW_KEY_DOWN));
    }

    #[test]
    fn every_recorded_type_maps_to_one_kind() {
        assert_eq!(
            kind_for_raw_type(RAW_KEY_DOWN),
            Some(RecordedEventKind::KeyDown)
        );
        assert_eq!(
            kind_for_raw_type(RAW_KEY_UP),
            Some(RecordedEventKind::KeyUp)
        );
        assert_eq!(
            kind_for_raw_type(RAW_FLAGS_CHANGED),
            Some(RecordedEventKind::FlagsChanged)
        );
        assert_eq!(
            kind_for_raw_type(RAW_LEFT_MOUSE_DOWN),
            Some(RecordedEventKind::MouseDown)
        );
        assert_eq!(
            kind_for_raw_type(RAW_RIGHT_MOUSE_UP),
            Some(RecordedEventKind::MouseUp)
        );
        assert_eq!(
            kind_for_raw_type(RAW_MOUSE_MOVED),
            Some(RecordedEventKind::MouseMoved)
        );
        assert_eq!(
            kind_for_raw_type(RAW_LEFT_MOUSE_DRAGGED),
            Some(RecordedEventKind::MouseDragged)
        );
        assert_eq!(
            kind_for_raw_type(RAW_SCROLL_WHEEL),
            Some(RecordedEventKind::Scroll)
        );
    }

    #[test]
    fn an_unknown_raw_type_maps_to_no_kind() {
        // 23 is `kCGEventTabletPointer`, which this module does not
        // record. A disable notice is not an event either.
        assert_eq!(kind_for_raw_type(23), None);
        assert_eq!(kind_for_raw_type(RAW_TAP_DISABLED_BY_TIMEOUT), None);
    }

    #[test]
    fn the_button_table_covers_all_three_buttons() {
        assert_eq!(
            button_for_raw_type(RAW_LEFT_MOUSE_DOWN),
            Some(MouseButton::Left)
        );
        assert_eq!(
            button_for_raw_type(RAW_RIGHT_MOUSE_UP),
            Some(MouseButton::Right)
        );
        assert_eq!(
            button_for_raw_type(RAW_OTHER_MOUSE_DOWN),
            Some(MouseButton::Other)
        );
        assert_eq!(button_for_raw_type(RAW_KEY_DOWN), None);
        assert_eq!(button_for_raw_type(RAW_SCROLL_WHEEL), None);
    }

    // ---- flags and key codes --------------------------------------------

    #[test]
    fn no_flags_produce_no_modifiers() {
        assert!(modifiers_from_flags(0).is_empty());
    }

    #[test]
    fn each_mask_produces_its_own_modifier() {
        assert_eq!(
            modifiers_from_flags(FLAG_MASK_COMMAND),
            vec![Modifier::Command]
        );
        assert_eq!(modifiers_from_flags(FLAG_MASK_SHIFT), vec![Modifier::Shift]);
        assert_eq!(
            modifiers_from_flags(FLAG_MASK_ALTERNATE),
            vec![Modifier::Option]
        );
        assert_eq!(
            modifiers_from_flags(FLAG_MASK_CONTROL),
            vec![Modifier::Control]
        );
    }

    #[test]
    fn modifiers_come_back_in_a_fixed_order() {
        let flags = FLAG_MASK_CONTROL | FLAG_MASK_COMMAND | FLAG_MASK_SHIFT;
        assert_eq!(
            modifiers_from_flags(flags),
            vec![Modifier::Command, Modifier::Shift, Modifier::Control]
        );
    }

    #[test]
    fn unrelated_flag_bits_are_ignored() {
        // Bit 0 is `kCGEventFlagMaskAlphaShift` (caps lock), which
        // `keyboard` cannot replay, plus one bit macOS reserves.
        assert!(modifiers_from_flags(0x0001_0000 | 0x0000_0001).is_empty());
    }

    #[test]
    fn known_key_codes_map_back_to_their_named_keys() {
        assert_eq!(named_key_from_key_code(0x24), Some(NamedKey::Return));
        assert_eq!(named_key_from_key_code(0x30), Some(NamedKey::Tab));
        assert_eq!(named_key_from_key_code(0x31), Some(NamedKey::Space));
        assert_eq!(named_key_from_key_code(0x33), Some(NamedKey::Delete));
        assert_eq!(named_key_from_key_code(0x35), Some(NamedKey::Escape));
        assert_eq!(named_key_from_key_code(0x7B), Some(NamedKey::ArrowLeft));
        assert_eq!(named_key_from_key_code(0x7C), Some(NamedKey::ArrowRight));
        assert_eq!(named_key_from_key_code(0x7D), Some(NamedKey::ArrowDown));
        assert_eq!(named_key_from_key_code(0x7E), Some(NamedKey::ArrowUp));
    }

    #[test]
    fn a_letter_key_code_names_no_named_key() {
        // 0x00 is the `A` key. `NamedKey` covers no printable key, so
        // the record carries the raw code alone.
        assert_eq!(named_key_from_key_code(0x00), None);
    }

    // ---- normalization --------------------------------------------------

    #[test]
    fn a_point_normalizes_against_the_display_that_holds_it() {
        let (id, x, y) = normalize_to_display(500.0, 250.0, &one_display()).expect("a display");
        assert_eq!(id, 1);
        assert!((x - 0.5).abs() < 1e-9);
        assert!((y - 0.5).abs() < 1e-9);
    }

    #[test]
    fn a_point_on_a_second_display_reports_that_display() {
        let (id, x, y) = normalize_to_display(1400.0, 100.0, &two_displays()).expect("a display");
        assert_eq!(id, 2);
        assert!((x - 0.5).abs() < 1e-9);
        assert!((y - 0.25).abs() < 1e-9);
    }

    #[test]
    fn an_off_screen_point_clamps_against_the_main_display() {
        // A point no display holds still has to travel back as a legal
        // fraction, because `tap` refuses anything outside 0.0..=1.0
        // (PINV-1).
        let (id, x, y) = normalize_to_display(-40.0, 9_000.0, &two_displays()).expect("a display");
        assert_eq!(id, 1);
        assert_eq!(x, 0.0);
        assert_eq!(y, 1.0);
    }

    #[test]
    fn no_display_normalizes_nothing() {
        assert_eq!(normalize_to_display(10.0, 10.0, &[]), None);
    }

    // ---- translation ----------------------------------------------------

    #[test]
    fn a_key_down_carries_its_code_and_its_named_key() {
        let event = RawTapEvent {
            event_type: RAW_KEY_DOWN,
            key_code: 0x24,
            ..raw(RAW_KEY_DOWN, 0)
        };
        let recorded = translate_event(&event, 0, &one_display(), &plan()).expect("an event");
        assert_eq!(recorded.kind, RecordedEventKind::KeyDown);
        assert_eq!(recorded.key_code, Some(0x24));
        assert_eq!(recorded.key, Some(NamedKey::Return));
    }

    #[test]
    fn a_key_event_with_no_named_key_still_carries_its_code() {
        let event = RawTapEvent {
            key_code: 0x00,
            ..raw(RAW_KEY_DOWN, 0)
        };
        let recorded = translate_event(&event, 0, &one_display(), &plan()).expect("an event");
        assert_eq!(recorded.key_code, Some(0x00));
        assert_eq!(recorded.key, None);
    }

    #[test]
    fn a_key_event_carries_the_modifiers_held_with_it() {
        let event = RawTapEvent {
            key_code: 0x24,
            flags: FLAG_MASK_COMMAND | FLAG_MASK_SHIFT,
            ..raw(RAW_KEY_DOWN, 0)
        };
        let recorded = translate_event(&event, 0, &one_display(), &plan()).expect("an event");
        assert_eq!(recorded.modifiers, vec![Modifier::Command, Modifier::Shift]);
    }

    #[test]
    fn a_click_carries_a_normalized_point_and_a_click_count() {
        let event = RawTapEvent {
            pixel_x: 250.0,
            pixel_y: 125.0,
            click_count: 2,
            ..raw(RAW_LEFT_MOUSE_DOWN, 0)
        };
        let recorded = translate_event(&event, 0, &one_display(), &plan()).expect("an event");
        assert_eq!(recorded.kind, RecordedEventKind::MouseDown);
        assert_eq!(recorded.button, Some(MouseButton::Left));
        assert_eq!(recorded.click_count, Some(2));
        assert_eq!(recorded.display_id, Some(1));
        assert_eq!(recorded.x, Some(0.25));
        assert_eq!(recorded.y, Some(0.25));
        assert_eq!(recorded.key_code, None);
    }

    #[test]
    fn a_click_count_of_zero_reads_as_one_click() {
        let event = RawTapEvent {
            click_count: 0,
            ..raw(RAW_LEFT_MOUSE_UP, 0)
        };
        let recorded = translate_event(&event, 0, &one_display(), &plan()).expect("an event");
        assert_eq!(recorded.click_count, Some(1));
    }

    #[test]
    fn a_scroll_carries_both_deltas_and_no_key() {
        let event = RawTapEvent {
            scroll_delta_x: -2,
            scroll_delta_y: 7,
            ..raw(RAW_SCROLL_WHEEL, 0)
        };
        let recorded = translate_event(&event, 0, &one_display(), &plan()).expect("an event");
        assert_eq!(recorded.kind, RecordedEventKind::Scroll);
        assert_eq!(recorded.scroll_delta_x, Some(-2));
        assert_eq!(recorded.scroll_delta_y, Some(7));
        assert_eq!(recorded.key_code, None);
        assert_eq!(recorded.click_count, None);
    }

    #[test]
    fn a_move_is_dropped_unless_the_caller_asks_for_it() {
        let event = raw(RAW_MOUSE_MOVED, 0);
        assert_eq!(translate_event(&event, 0, &one_display(), &plan()), None);
        let asked = RecordingPlan {
            record_mouse_moves: true,
            ..plan()
        };
        let recorded = translate_event(&event, 0, &one_display(), &asked).expect("an event");
        assert_eq!(recorded.kind, RecordedEventKind::MouseMoved);
    }

    #[test]
    fn a_disable_notice_is_never_an_event() {
        let event = raw(RAW_TAP_DISABLED_BY_TIMEOUT, 0);
        assert_eq!(translate_event(&event, 0, &one_display(), &plan()), None);
    }

    #[test]
    fn an_offset_counts_from_the_start_of_the_recording() {
        let event = raw(RAW_KEY_DOWN, 1_500_000_000);
        let recorded =
            translate_event(&event, 1_000_000_000, &one_display(), &plan()).expect("an event");
        assert_eq!(recorded.offset_ms, 500);
    }

    #[test]
    fn an_event_stamped_before_the_start_gets_offset_zero() {
        // A `CGEvent` carries the hardware timestamp of the keypress, so
        // an event already in flight when the tap started can be older
        // than the tap. A wrapped subtraction there would report an
        // offset of millions of years.
        let event = raw(RAW_KEY_DOWN, 5);
        let recorded =
            translate_event(&event, 1_000_000_000, &one_display(), &plan()).expect("an event");
        assert_eq!(recorded.offset_ms, 0);
    }

    // ---- redaction ------------------------------------------------------

    #[test]
    fn typed_text_is_withheld_by_default() {
        let event = RawTapEvent {
            key_code: 0x00,
            characters: Some("hunter2".to_string()),
            ..raw(RAW_KEY_DOWN, 0)
        };
        let recorded = translate_event(&event, 0, &one_display(), &plan()).expect("an event");
        assert_eq!(recorded.text, None, "a default recording holds no text");
        assert!(recorded.redacted);
    }

    #[test]
    fn typed_text_travels_only_when_the_caller_opts_in() {
        let event = RawTapEvent {
            characters: Some("a".to_string()),
            ..raw(RAW_KEY_DOWN, 0)
        };
        let opted_in = RecordingPlan {
            capture_text: true,
            ..plan()
        };
        let recorded = translate_event(&event, 0, &one_display(), &opted_in).expect("an event");
        assert_eq!(recorded.text.as_deref(), Some("a"));
        assert!(!recorded.redacted);
    }

    #[test]
    fn a_click_is_never_marked_redacted() {
        let recorded =
            translate_event(&raw(RAW_LEFT_MOUSE_DOWN, 0), 0, &one_display(), &plan()).expect("one");
        assert!(!recorded.redacted, "a click carries no characters at all");
        assert_eq!(recorded.text, None);
    }

    // ---- assembly -------------------------------------------------------

    #[test]
    fn only_a_key_event_ever_carries_text() {
        // `CGEventKeyboardGetUnicodeString` is a keyboard API. Running
        // it on a mouse or scroll event yields nothing meaningful, and a
        // click record with a `text` field it never produced would tell
        // a caller the user typed during a click.
        let mut opted_in = plan();
        opted_in.capture_text = true;

        for raw_type in [RAW_LEFT_MOUSE_DOWN, RAW_LEFT_MOUSE_UP, RAW_SCROLL_WHEEL] {
            let mut event = raw(raw_type, 0);
            event.characters = Some("hunter2".to_string());
            let recorded = translate_event(&event, 0, &one_display(), &opted_in)
                .expect("the event is recordable");
            assert_eq!(
                recorded.text, None,
                "a {:?} event carried text",
                recorded.kind
            );
        }

        // The same opt-in still gives a key event its characters.
        let mut key = raw(RAW_KEY_DOWN, 0);
        key.characters = Some("hunter2".to_string());
        let recorded = translate_event(&key, 0, &one_display(), &opted_in).unwrap();
        assert_eq!(recorded.text.as_deref(), Some("hunter2"));
    }

    #[test]
    fn a_recording_the_tap_truncated_is_not_complete() {
        // The tap stops collecting the moment its budget fills, so the
        // events it declined never reach `polarize-core` to be counted.
        // Only the tap knows they happened. A recording that reports
        // itself complete while real input is missing is exactly what
        // PINV-39 forbids.
        let recording = RawRecording {
            events: vec![raw(RAW_KEY_DOWN, 0), raw(RAW_KEY_UP, 1_000_000)],
            started_ns: 0,
            elapsed_ms: 5_000,
            dropped_events: 7,
        };
        let mut plan = plan();
        plan.max_events = 2;

        let response = assemble_recording(&recording, &one_display(), &plan);

        assert_eq!(response.dropped_events, 7);
        assert!(!response.complete, "input was lost");
        assert_eq!(response.stopped_because, StopReason::EventLimit);
        assert!(
            response.warnings.iter().any(|w| w.contains("7")),
            "the warning must name how many were lost: {:?}",
            response.warnings
        );
    }

    #[test]
    fn a_clean_recording_reports_itself_complete() {
        let recording = RawRecording {
            events: vec![raw(RAW_KEY_DOWN, 0), raw(RAW_KEY_UP, 1_000_000)],
            started_ns: 0,
            elapsed_ms: 5_000,
            dropped_events: 0,
        };
        let response = assemble_recording(&recording, &one_display(), &plan());
        assert_eq!(response.event_count, 2);
        assert_eq!(response.events.len(), 2);
        assert!(response.complete);
        assert!(response.warnings.is_empty());
        assert_eq!(response.stopped_because, StopReason::Duration);
        assert_eq!(response.recorded_ms, 5_000);
        assert_eq!(response.dropped_events, 0);
    }

    #[test]
    fn a_tap_disabled_by_timeout_is_counted_and_reported() {
        let recording = RawRecording {
            events: vec![
                raw(RAW_KEY_DOWN, 0),
                raw(RAW_TAP_DISABLED_BY_TIMEOUT, 1),
                raw(RAW_KEY_UP, 2),
            ],
            started_ns: 0,
            elapsed_ms: 5_000,
            dropped_events: 0,
        };
        let response = assemble_recording(&recording, &one_display(), &plan());
        assert_eq!(response.tap_disabled_by_timeout, 1);
        assert_eq!(response.tap_disabled_by_user_input, 0);
        assert_eq!(response.event_count, 2, "the notice is not an event");
        assert!(!response.complete, "a disabled tap loses real input");
        assert_eq!(response.warnings.len(), 1);
        assert!(
            response.warnings[0].contains("timeout"),
            "the warning must name the cause: {}",
            response.warnings[0]
        );
    }

    #[test]
    fn a_tap_disabled_by_user_input_is_counted_and_reported() {
        let recording = RawRecording {
            events: vec![raw(RAW_TAP_DISABLED_BY_USER_INPUT, 0)],
            started_ns: 0,
            elapsed_ms: 5_000,
            dropped_events: 0,
        };
        let response = assemble_recording(&recording, &one_display(), &plan());
        assert_eq!(response.tap_disabled_by_user_input, 1);
        assert!(!response.complete);
        assert_eq!(response.warnings.len(), 1);
    }

    #[test]
    fn every_disable_notice_is_counted_not_just_the_first() {
        let recording = RawRecording {
            events: vec![
                raw(RAW_TAP_DISABLED_BY_TIMEOUT, 0),
                raw(RAW_TAP_DISABLED_BY_TIMEOUT, 1),
                raw(RAW_TAP_DISABLED_BY_USER_INPUT, 2),
            ],
            started_ns: 0,
            elapsed_ms: 100,
            dropped_events: 0,
        };
        let response = assemble_recording(&recording, &one_display(), &plan());
        assert_eq!(response.tap_disabled_by_timeout, 2);
        assert_eq!(response.tap_disabled_by_user_input, 1);
        assert_eq!(response.warnings.len(), 2, "one warning per cause");
    }

    #[test]
    fn a_full_event_budget_truncates_and_says_so() {
        let small = RecordingPlan {
            max_events: 2,
            ..plan()
        };
        let recording = RawRecording {
            events: vec![
                raw(RAW_KEY_DOWN, 0),
                raw(RAW_KEY_UP, 1),
                raw(RAW_KEY_DOWN, 2),
                raw(RAW_KEY_UP, 3),
            ],
            started_ns: 0,
            elapsed_ms: 40,
            dropped_events: 0,
        };
        let response = assemble_recording(&recording, &one_display(), &small);
        assert_eq!(response.events.len(), 2);
        assert_eq!(response.event_count, 2);
        assert_eq!(response.dropped_events, 2);
        assert_eq!(response.stopped_because, StopReason::EventLimit);
        assert!(!response.complete);
        assert_eq!(response.warnings.len(), 1);
    }

    #[test]
    fn offsets_never_go_backwards() {
        // The window server can deliver an event stamped before one it
        // already delivered. A flow replayed from decreasing offsets
        // runs its steps in the wrong order.
        let recording = RawRecording {
            events: vec![
                raw(RAW_KEY_DOWN, 3_000_000),
                raw(RAW_KEY_UP, 1_000_000),
                raw(RAW_KEY_DOWN, 9_000_000),
            ],
            started_ns: 0,
            elapsed_ms: 10,
            dropped_events: 0,
        };
        let response = assemble_recording(&recording, &one_display(), &plan());
        let offsets: Vec<u64> = response
            .events
            .iter()
            .map(|event| event.offset_ms)
            .collect();
        assert_eq!(offsets, vec![3, 3, 9]);
    }

    #[test]
    fn an_unrecorded_raw_type_is_left_out_silently() {
        let recording = RawRecording {
            // 24 is `kCGEventTabletProximity`.
            events: vec![raw(24, 0), raw(RAW_KEY_DOWN, 0)],
            started_ns: 0,
            elapsed_ms: 10,
            dropped_events: 0,
        };
        let response = assemble_recording(&recording, &one_display(), &plan());
        assert_eq!(response.event_count, 1);
        assert!(response.complete, "an unread type is not lost input");
    }

    #[test]
    fn the_response_reports_whether_text_was_captured() {
        let recording = RawRecording::default();
        let response = assemble_recording(&recording, &one_display(), &plan());
        assert!(!response.text_captured);
        let opted_in = RecordingPlan {
            capture_text: true,
            ..plan()
        };
        let response = assemble_recording(&recording, &one_display(), &opted_in);
        assert!(response.text_captured);
    }

    // ---- the whole call -------------------------------------------------

    #[test]
    fn the_resolved_plan_reaches_the_recorder() {
        let recorder = FakeRecorder::new(RawRecording::default());
        let request = RecordFlowRequest {
            duration_ms: Some(1_234),
            max_events: Some(7),
            record_mouse_moves: Some(true),
            capture_text: Some(true),
        };
        perform_record_flow(&recorder, &FakeDisplays(one_display()), &request).expect("a response");
        let seen = recorder.seen.borrow();
        assert_eq!(seen.len(), 1);
        assert_eq!(
            seen[0],
            RecordingPlan {
                duration_ms: 1_234,
                max_events: 7,
                record_mouse_moves: true,
                capture_text: true,
            }
        );
    }

    #[test]
    fn a_refused_request_never_starts_a_tap() {
        let recorder = FakeRecorder::new(RawRecording::default());
        let request = RecordFlowRequest {
            duration_ms: Some(0),
            ..RecordFlowRequest::default()
        };
        let error =
            perform_record_flow(&recorder, &FakeDisplays(one_display()), &request).unwrap_err();
        assert!(error.to_string().contains("duration_ms"));
        assert!(
            recorder.seen.borrow().is_empty(),
            "a bad request must not open a tap"
        );
    }

    #[test]
    fn an_empty_display_list_is_an_error() {
        let recorder = FakeRecorder::new(RawRecording::default());
        let error = perform_record_flow(
            &recorder,
            &FakeDisplays(Vec::new()),
            &RecordFlowRequest::default(),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            PolarizeError::from(RecordingError::NoDisplays).to_string()
        );
        assert!(recorder.seen.borrow().is_empty());
    }

    #[test]
    fn a_display_read_failure_travels_back_unchanged() {
        let recorder = FakeRecorder::new(RawRecording::default());
        let error = perform_record_flow(&recorder, &FailingDisplays, &RecordFlowRequest::default())
            .unwrap_err();
        assert!(error.to_string().contains("no window server"));
    }

    #[test]
    fn a_tap_failure_travels_back_unchanged() {
        let error = perform_record_flow(
            &FailingRecorder,
            &FakeDisplays(one_display()),
            &RecordFlowRequest::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("tap refused"));
    }

    #[test]
    fn the_response_carries_the_resolved_duration_and_the_real_one() {
        let recorder = FakeRecorder::new(RawRecording {
            events: vec![raw(RAW_KEY_DOWN, 0)],
            started_ns: 0,
            elapsed_ms: 812,
            dropped_events: 0,
        });
        let request = RecordFlowRequest {
            duration_ms: Some(5 * MAX_DURATION_MS),
            ..RecordFlowRequest::default()
        };
        let response =
            perform_record_flow(&recorder, &FakeDisplays(one_display()), &request).expect("ok");
        assert_eq!(response.requested_duration_ms, MAX_DURATION_MS);
        assert_eq!(response.recorded_ms, 812);
    }

    #[test]
    fn a_platform_that_leaks_characters_is_still_redacted() {
        // The tap must not read the characters at all when the caller
        // did not opt in. This crate drops them a second time anyway,
        // so one bug in `polarize-macos` cannot leak a password
        // (PINV-40).
        let recorder = FakeRecorder::new(RawRecording {
            events: vec![RawTapEvent {
                characters: Some("secret".to_string()),
                ..raw(RAW_KEY_DOWN, 0)
            }],
            started_ns: 0,
            elapsed_ms: 10,
            dropped_events: 0,
        });
        let response = perform_record_flow(
            &recorder,
            &FakeDisplays(one_display()),
            &RecordFlowRequest::default(),
        )
        .expect("ok");
        assert_eq!(response.events[0].text, None);
        assert!(response.events[0].redacted);
        assert!(!response.text_captured);
    }

    #[test]
    fn a_recorded_click_is_a_fraction_a_tap_accepts() {
        // A recording is only useful if `tap` takes its numbers back.
        // `coords::fraction_to_pixel` refuses anything outside
        // 0.0..=1.0 (PINV-1).
        let recorder = FakeRecorder::new(RawRecording {
            events: vec![RawTapEvent {
                pixel_x: 12_000.0,
                pixel_y: -3.0,
                ..raw(RAW_LEFT_MOUSE_DOWN, 0)
            }],
            started_ns: 0,
            elapsed_ms: 10,
            dropped_events: 0,
        });
        let response = perform_record_flow(
            &recorder,
            &FakeDisplays(two_displays()),
            &RecordFlowRequest::default(),
        )
        .expect("ok");
        let event = &response.events[0];
        let fraction = crate::coords::Fraction {
            x: event.x.expect("an x"),
            y: event.y.expect("a y"),
        };
        let size = crate::coords::PixelSize {
            width: 1000.0,
            height: 500.0,
        };
        assert!(crate::coords::fraction_to_pixel(fraction, size).is_ok());
    }

    #[test]
    fn the_request_and_the_response_round_trip_as_json() {
        let request = RecordFlowRequest {
            duration_ms: Some(2_000),
            max_events: Some(10),
            record_mouse_moves: Some(true),
            capture_text: Some(false),
        };
        let json = serde_json::to_string(&request).expect("serialize");
        let back: RecordFlowRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, request);

        let recorder = FakeRecorder::new(RawRecording {
            events: vec![raw(RAW_KEY_DOWN, 0)],
            started_ns: 0,
            elapsed_ms: 10,
            dropped_events: 0,
        });
        let response =
            perform_record_flow(&recorder, &FakeDisplays(one_display()), &request).expect("ok");
        let json = serde_json::to_string(&response).expect("serialize");
        let back: RecordFlowResponse = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, response);
    }

    #[test]
    fn an_empty_request_deserializes_from_an_empty_object() {
        let request: RecordFlowRequest = serde_json::from_str("{}").expect("deserialize");
        assert_eq!(request, RecordFlowRequest::default());
    }
}
