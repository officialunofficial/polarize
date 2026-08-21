//! [`FlowRecorder`] over a listen-only `CGEventTap` and `CFRunLoop`.
//!
//! `polarize_core::recording` decides everything about a recording: the
//! duration, the event budget, the redaction, the translation of a raw
//! event into a replayable record, and the report of a tap that macOS
//! disabled. This module does the one thing only macOS can do. It opens
//! the tap, it collects [`RawTapEvent`] values, and it hands them back.
//!
//! ## The tap listens, and never touches an event
//!
//! The tap takes `kCGEventTapOptionListenOnly`. A default tap can change
//! an event, or drop it, and a bug there makes the Mac unusable while
//! `polarize` runs: the user's own keystrokes stop reaching their apps.
//! So the callback returns the same pointer it received, always, and it
//! calls no `CGEventSet*` function at all. See PINV-39.
//!
//! ## macOS turns a tap off, and says nothing more
//!
//! macOS disables a tap when its callback runs too long
//! (`kCGEventTapDisabledByTimeout`), and when the user's input path asks
//! it to (`kCGEventTapDisabledByUserInput`). The tap then stops
//! delivering events. Nothing else reports it. A recording that ends
//! there looks complete, and the caller replays a flow that stops half
//! way.
//!
//! So the callback handles both notices. It re-enables the tap at once,
//! and it appends the notice to the event list. `polarize-core` counts
//! the notices and reports them in the response (PINV-39).
//!
//! ## Why a dedicated thread
//!
//! A tap delivers events through a `CFRunLoop` source, and `apps/polarize`
//! is an async `rmcp` server whose Tokio worker threads run no
//! `CFRunLoop`. A `CFMachPort` and a `CFRunLoopRef` also belong to the
//! thread that made them, and neither is `Send`. So one thread creates,
//! uses, and destroys every Core Foundation handle, and only plain data
//! crosses back. This is the same rule PINV-20 sets for `AXObserver`,
//! and `observer.rs` is the module to read next to this one.
//!
//! ## What is not verified
//!
//! Nothing here has run against a real macOS session. No CI runner can
//! grant Input Monitoring. See the crate-level "what is and is not
//! verified" note, and PINV-39's enforcement entry.

use std::cell::RefCell;
use std::ffi::{c_ulong, c_void};
use std::time::{Duration, Instant};

use objc2_core_foundation::{CFMachPort, CFRunLoop, CFRunLoopRunResult, kCFRunLoopDefaultMode};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventMask, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType, CGPreflightListenEventAccess,
};
use polarize_core::error::PolarizeError;
use polarize_core::permission::{PermissionError, PermissionKind, PermissionState};
use polarize_core::recording::{
    self, FlowRecorder, RawRecording, RawTapEvent, RecordingPlan, is_mouse_move_type,
    kind_for_raw_type,
};

/// The event types the tap asks for.
///
/// The two tap-disabled notices are not in this list on purpose. macOS
/// delivers those to every tap, whatever the mask says.
const RECORDED_TYPES: [u32; 14] = [
    recording::RAW_LEFT_MOUSE_DOWN,
    recording::RAW_LEFT_MOUSE_UP,
    recording::RAW_RIGHT_MOUSE_DOWN,
    recording::RAW_RIGHT_MOUSE_UP,
    recording::RAW_OTHER_MOUSE_DOWN,
    recording::RAW_OTHER_MOUSE_UP,
    recording::RAW_MOUSE_MOVED,
    recording::RAW_LEFT_MOUSE_DRAGGED,
    recording::RAW_RIGHT_MOUSE_DRAGGED,
    recording::RAW_OTHER_MOUSE_DRAGGED,
    recording::RAW_KEY_DOWN,
    recording::RAW_KEY_UP,
    recording::RAW_FLAGS_CHANGED,
    recording::RAW_SCROLL_WHEEL,
];

/// How many `UniChar` values one key event may produce.
///
/// A dead key plus its base character is the longest normal case. A
/// longer string is cut here, and the record says nothing about the
/// cut, because the characters are an aid to a human reader. The key
/// code is what replays the key.
const MAX_CHARACTERS: usize = 8;

/// How long to sleep when the run loop reports it has nothing to do.
///
/// `CFRunLoopRunInMode` returns `Finished` the moment a run loop holds
/// no source. That must not happen here, because the tap's source stays
/// installed for the whole recording. If it happens anyway, this sleep
/// is what stops the loop from pinning a core for the whole duration.
const IDLE_SLEEP: Duration = Duration::from_millis(10);

/// Everything the tap callback writes into.
///
/// This lives on the tap thread's stack for the whole recording, inside
/// a [`RefCell`], and the callback reaches it through the `user_info`
/// pointer.
///
/// The [`RefCell`] is what makes the sharing sound. Both the callback
/// and [`run_tap`] write to this state, and the callback runs on this
/// same thread, inside `CFRunLoopRunInMode`. A `&mut` handed out as a
/// raw pointer would go stale the moment `run_tap` touched the value
/// again. A shared reference to a cell never does, because a cell
/// carries its own interior mutability. `observer.rs` shares an
/// `AtomicBool` with its callback for the same reason.
struct Collector {
    plan: RecordingPlan,
    started: Instant,
    events: Vec<RawTapEvent>,
    /// How many real events the tap kept. A tap-disabled notice does
    /// not count, so one disabled tap does not shorten the budget.
    kept: usize,
    /// The tap's own port, borrowed. The callback needs it to turn a
    /// disabled tap back on. It is null until the port exists.
    tap: *const CFMachPort,
}

/// [`FlowRecorder`] over `CGEventTapCreate`.
#[derive(Debug, Default)]
pub struct MacFlowRecorder;

impl FlowRecorder for MacFlowRecorder {
    fn record_raw_events(&self, plan: &RecordingPlan) -> Result<RawRecording, PolarizeError> {
        // Input Monitoring first, then the login session. This is the
        // order every tool in this crate uses (PINV-10, PINV-23).
        //
        // `CGPreflightListenEventAccess` cannot tell "never asked" from
        // "denied", so report the more conservative of the two, exactly
        // as `AXIsProcessTrusted` does (PINV-11).
        if !CGPreflightListenEventAccess() {
            return Err(PolarizeError::Permission(PermissionError::NotGranted {
                kind: PermissionKind::InputMonitoring,
                state: PermissionState::NotDetermined,
            }));
        }
        // A tap off the console sees nothing a caller can use: the
        // input goes to the user who holds the display. PINV-23 covers
        // this tool for the same reason it covers `tap` and `keyboard`.
        crate::session::ensure_session_usable()?;

        let plan = *plan;
        std::thread::Builder::new()
            .name("polarize-event-tap".to_string())
            .spawn(move || run_tap(plan))
            .map_err(|err| {
                PolarizeError::Platform(format!("could not start the event tap thread: {err}"))
            })
            .and_then(|handle| match handle.join() {
                Ok(Ok(recording)) => Ok(recording),
                Ok(Err(message)) => Err(PolarizeError::Platform(message)),
                Err(_) => Err(PolarizeError::Platform(
                    "the event tap thread panicked".to_string(),
                )),
            })
    }
}

/// The mask of event types one plan asks for.
///
/// A mouse move arrives at the display's refresh rate. Leaving moves
/// out of the mask, and not only out of the record, is what keeps a
/// default recording cheap. `polarize_core` owns the rule that says
/// which type is a move.
fn event_mask(plan: &RecordingPlan) -> CGEventMask {
    let mut mask: CGEventMask = 0;
    for raw_type in RECORDED_TYPES {
        if !plan.record_mouse_moves && is_mouse_move_type(raw_type) {
            continue;
        }
        mask |= 1u64 << raw_type;
    }
    mask
}

/// Runs one whole event-tap lifecycle on the calling thread.
///
/// Every Core Foundation handle is created, used, and destroyed before
/// this function returns, so nothing that is not `Send` escapes the
/// thread. Only plain data crosses back. See PINV-39 and PINV-20.
fn run_tap(plan: RecordingPlan) -> Result<RawRecording, String> {
    let collector = RefCell::new(Collector {
        plan,
        started: Instant::now(),
        events: Vec::new(),
        kept: 0,
        tap: std::ptr::null(),
    });
    let user_info = std::ptr::from_ref(&collector).cast_mut().cast::<c_void>();

    // `kCGSessionEventTap` sees every event of this login session, which
    // is what a flow recording needs. `kCGHIDEventTap` would also see
    // events aimed at other sessions.
    //
    // `kCGEventTapOptionListenOnly` is the whole safety story of this
    // module. See PINV-39.
    let port = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::SessionEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::ListenOnly,
            event_mask(&plan),
            Some(tap_callback),
            user_info,
        )
    }
    .ok_or_else(|| {
        "CGEventTapCreate returned null; macOS refused the event tap. Grant Input Monitoring to \
         this binary in System Settings, then call the tool again"
            .to_string()
    })?;

    // The callback needs the port to turn a disabled tap back on. The
    // tap delivers nothing until its source joins the run loop below,
    // so this write always happens first.
    collector.borrow_mut().tap = std::ptr::from_ref(&*port);

    let source = CFMachPort::new_run_loop_source(None, Some(&port), 0)
        .ok_or("CFMachPortCreateRunLoopSource returned null for the event tap")?;
    let run_loop = CFRunLoop::current().ok_or("CFRunLoopGetCurrent returned null")?;
    let mode = unsafe { kCFRunLoopDefaultMode };

    run_loop.add_source(Some(&source), mode);
    CGEvent::tap_enable(&port, true);

    let duration = Duration::from_millis(plan.duration_ms);
    // No `?` sits between here and the teardown below, so the source is
    // always removed and the port is always invalidated.
    loop {
        // The borrow ends before the run loop runs, so the callback
        // always finds the cell free.
        let (elapsed, kept) = {
            let state = collector.borrow();
            (state.started.elapsed(), state.kept)
        };
        if elapsed >= duration || kept >= plan.max_events {
            break;
        }
        let remaining = duration - elapsed;
        // `true` means "return as soon as one source is handled", which
        // is what lets this loop re-check the event budget after every
        // event. A `false` here would run the whole duration and record
        // far past the budget.
        let result = CFRunLoop::run_in_mode(mode, remaining.as_secs_f64(), true);
        if result == CFRunLoopRunResult::Finished {
            std::thread::sleep(IDLE_SLEEP);
        }
    }

    CGEvent::tap_enable(&port, false);
    run_loop.remove_source(Some(&source), mode);
    source.invalidate();
    // The callback must not reach a port that is about to go away.
    collector.borrow_mut().tap = std::ptr::null();
    port.invalidate();

    let state = collector.into_inner();
    let elapsed_ms = u64::try_from(state.started.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(RawRecording {
        events: state.events,
        // Every timestamp below is already an offset from this same
        // start, so the start itself is zero. See `read_event`.
        started_ns: 0,
        elapsed_ms,
    })
}

/// The tap's callback.
///
/// It runs on the tap thread, inside `CFRunLoopRunInMode`. It does the
/// least work it can, because macOS disables a tap whose callback is
/// slow.
///
/// It returns the pointer it received, every time. A listen-only tap
/// cannot change the event stream, and this callback must never try.
/// See PINV-39.
///
/// # Safety
/// `user_info` is the `*mut c_void` handed to `CGEventTapCreate`.
/// [`run_tap`] always passes a pointer to a [`Collector`] that outlives
/// the tap.
unsafe extern "C-unwind" fn tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: std::ptr::NonNull<CGEvent>,
    user_info: *mut c_void,
) -> *mut CGEvent {
    // The one value this function returns, on every path.
    let passthrough = event.as_ptr();

    let Some(cell) = (unsafe { user_info.cast::<RefCell<Collector>>().as_ref() }) else {
        return passthrough;
    };
    // `try_borrow_mut`, not `borrow_mut`. A panic here would unwind
    // through Core Foundation's own stack frames, which is undefined
    // behavior. Losing one event is the smaller failure, and it cannot
    // happen anyway: `run_tap` holds no borrow while the run loop runs.
    let Ok(mut collector) = cell.try_borrow_mut() else {
        return passthrough;
    };
    let raw_type = event_type.0;
    let timestamp_ns = u64::try_from(collector.started.elapsed().as_nanos()).unwrap_or(u64::MAX);

    // macOS turned the tap off. Turn it back on, and leave a record
    // that it happened. `polarize_core` reports the count, because a
    // recording that silently stops is worse than one that errors.
    if recording::is_tap_disabled_type(raw_type) {
        if let Some(tap) = unsafe { collector.tap.as_ref() } {
            CGEvent::tap_enable(tap, true);
        }
        collector.events.push(RawTapEvent {
            event_type: raw_type,
            timestamp_ns,
            ..RawTapEvent::default()
        });
        return passthrough;
    }

    if collector.kept >= collector.plan.max_events {
        return passthrough;
    }
    // Both tests belong to `polarize_core`. The mask already leaves a
    // move out when the caller did not ask for one; this repeats the
    // test because a mask is a request, not a promise.
    if !collector.plan.record_mouse_moves && is_mouse_move_type(raw_type) {
        return passthrough;
    }
    if kind_for_raw_type(raw_type).is_none() {
        return passthrough;
    }

    let plan = collector.plan;
    let raw = read_event(unsafe { event.as_ref() }, raw_type, timestamp_ns, &plan);
    collector.events.push(raw);
    collector.kept += 1;
    passthrough
}

/// Copies the fields `polarize_core` needs out of one live `CGEvent`.
///
/// The timestamp is the moment the tap saw the event, not the event's
/// own `CGEventGetTimestamp`. That field holds mach absolute time, and
/// converting it needs `mach_timebase_info`, which no `objc2` crate
/// binds. A listen-only tap sees an event within microseconds of the
/// hardware, so the delivery time is the same number for a flow's
/// purposes. Both readings come from one `Instant`, which is what
/// `polarize_core::recording` requires of the two.
fn read_event(
    event: &CGEvent,
    raw_type: u32,
    timestamp_ns: u64,
    plan: &RecordingPlan,
) -> RawTapEvent {
    let point = CGEvent::location(Some(event));
    let key_code = CGEvent::integer_value_field(Some(event), CGEventField::KeyboardEventKeycode);
    RawTapEvent {
        event_type: raw_type,
        timestamp_ns,
        key_code: u16::try_from(key_code).unwrap_or(0),
        flags: CGEvent::flags(Some(event)).0,
        click_count: CGEvent::integer_value_field(Some(event), CGEventField::MouseEventClickState),
        pixel_x: point.x,
        pixel_y: point.y,
        // Axis 1 is the vertical scroll, and axis 2 is the horizontal
        // one. The names do not say so; Apple's header does.
        scroll_delta_x: CGEvent::integer_value_field(
            Some(event),
            CGEventField::ScrollWheelEventDeltaAxis2,
        ),
        scroll_delta_y: CGEvent::integer_value_field(
            Some(event),
            CGEventField::ScrollWheelEventDeltaAxis1,
        ),
        // The characters are read only when the caller opted in. A
        // default recording never holds a typed password, not even in
        // this process's memory. See PINV-40.
        characters: if plan.capture_text {
            read_characters(event)
        } else {
            None
        },
    }
}

/// The characters one key event produced, when it produced any.
fn read_characters(event: &CGEvent) -> Option<String> {
    let mut buffer = [0u16; MAX_CHARACTERS];
    let mut length: c_ulong = 0;
    unsafe {
        CGEvent::keyboard_get_unicode_string(
            Some(event),
            MAX_CHARACTERS as c_ulong,
            &mut length,
            buffer.as_mut_ptr(),
        );
    }
    let length = usize::try_from(length).unwrap_or(0).min(MAX_CHARACTERS);
    if length == 0 {
        return None;
    }
    let text = String::from_utf16_lossy(&buffer[..length]);
    if text.is_empty() { None } else { Some(text) }
}

#[cfg(test)]
mod tests {
    use super::*;

    // These tests touch no window server and need no permission. They
    // check that this module's copies of the `CGEventType` and
    // `CGEventFlags` numbers match the real framework constants, which
    // is the one part of this file a test can prove. Everything else
    // here needs a real macOS session; see PINV-39's enforcement entry.

    #[test]
    fn the_raw_type_numbers_match_the_framework() {
        assert_eq!(recording::RAW_LEFT_MOUSE_DOWN, CGEventType::LeftMouseDown.0);
        assert_eq!(recording::RAW_LEFT_MOUSE_UP, CGEventType::LeftMouseUp.0);
        assert_eq!(
            recording::RAW_RIGHT_MOUSE_DOWN,
            CGEventType::RightMouseDown.0
        );
        assert_eq!(recording::RAW_RIGHT_MOUSE_UP, CGEventType::RightMouseUp.0);
        assert_eq!(recording::RAW_MOUSE_MOVED, CGEventType::MouseMoved.0);
        assert_eq!(
            recording::RAW_LEFT_MOUSE_DRAGGED,
            CGEventType::LeftMouseDragged.0
        );
        assert_eq!(
            recording::RAW_RIGHT_MOUSE_DRAGGED,
            CGEventType::RightMouseDragged.0
        );
        assert_eq!(recording::RAW_KEY_DOWN, CGEventType::KeyDown.0);
        assert_eq!(recording::RAW_KEY_UP, CGEventType::KeyUp.0);
        assert_eq!(recording::RAW_FLAGS_CHANGED, CGEventType::FlagsChanged.0);
        assert_eq!(recording::RAW_SCROLL_WHEEL, CGEventType::ScrollWheel.0);
        assert_eq!(
            recording::RAW_OTHER_MOUSE_DOWN,
            CGEventType::OtherMouseDown.0
        );
        assert_eq!(recording::RAW_OTHER_MOUSE_UP, CGEventType::OtherMouseUp.0);
        assert_eq!(
            recording::RAW_OTHER_MOUSE_DRAGGED,
            CGEventType::OtherMouseDragged.0
        );
    }

    #[test]
    fn the_disable_notice_numbers_match_the_framework() {
        assert_eq!(
            recording::RAW_TAP_DISABLED_BY_TIMEOUT,
            CGEventType::TapDisabledByTimeout.0
        );
        assert_eq!(
            recording::RAW_TAP_DISABLED_BY_USER_INPUT,
            CGEventType::TapDisabledByUserInput.0
        );
    }

    #[test]
    fn the_flag_masks_match_the_framework() {
        use objc2_core_graphics::CGEventFlags;
        assert_eq!(recording::FLAG_MASK_SHIFT, CGEventFlags::MaskShift.0);
        assert_eq!(recording::FLAG_MASK_CONTROL, CGEventFlags::MaskControl.0);
        assert_eq!(
            recording::FLAG_MASK_ALTERNATE,
            CGEventFlags::MaskAlternate.0
        );
        assert_eq!(recording::FLAG_MASK_COMMAND, CGEventFlags::MaskCommand.0);
    }

    #[test]
    fn the_tap_is_listen_only() {
        // A default tap can change or drop the user's real input. This
        // test is a reader's guard, not a proof: only a human on real
        // macOS can confirm the running tap swallows nothing.
        assert_eq!(CGEventTapOptions::ListenOnly.0, 1);
    }

    #[test]
    fn a_default_mask_leaves_the_move_types_out() {
        let plan = RecordingPlan {
            duration_ms: 1_000,
            max_events: 10,
            record_mouse_moves: false,
            capture_text: false,
        };
        let mask = event_mask(&plan);
        assert_eq!(mask & (1u64 << recording::RAW_MOUSE_MOVED), 0);
        assert_eq!(mask & (1u64 << recording::RAW_LEFT_MOUSE_DRAGGED), 0);
        assert_ne!(mask & (1u64 << recording::RAW_KEY_DOWN), 0);
        assert_ne!(mask & (1u64 << recording::RAW_LEFT_MOUSE_DOWN), 0);
    }

    #[test]
    fn an_opted_in_mask_asks_for_the_move_types() {
        let plan = RecordingPlan {
            duration_ms: 1_000,
            max_events: 10,
            record_mouse_moves: true,
            capture_text: false,
        };
        let mask = event_mask(&plan);
        assert_ne!(mask & (1u64 << recording::RAW_MOUSE_MOVED), 0);
        assert_ne!(mask & (1u64 << recording::RAW_LEFT_MOUSE_DRAGGED), 0);
    }

    #[test]
    fn the_mask_never_asks_for_a_disable_notice() {
        // Both notice numbers are far outside a 64-bit mask, so asking
        // for one would shift a bit off the end. macOS delivers them
        // whatever the mask holds.
        assert!(!RECORDED_TYPES.contains(&recording::RAW_TAP_DISABLED_BY_TIMEOUT));
        assert!(!RECORDED_TYPES.contains(&recording::RAW_TAP_DISABLED_BY_USER_INPUT));
        for raw_type in RECORDED_TYPES {
            assert!(raw_type < 64, "type {raw_type} does not fit in a mask");
        }
    }
}
