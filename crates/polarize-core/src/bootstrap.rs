//! Pure decision and lifecycle logic behind `apps/polarize`'s
//! `--request-permissions` guided helper (PLZ-5).
//!
//! Everything here is platform-agnostic and fully unit-tested. Three
//! things live in this module:
//!
//! 1. [`needed_permissions`] — decides which permissions still need the
//!    guided helper, after the real system prompts already ran.
//! 2. [`helper_args`] — builds the helper's argv from that decision. The
//!    argv carries only permission names and, when known, Polarize's own
//!    bundle path (PINV-59) — never a status field. See PINV-56's note
//!    that the helper cannot correctly read Polarize's own grant state
//!    and so must never be asked to report one.
//! 3. [`wait_for_grants_or_close`] — the poll-and-terminate loop that
//!    decides when `--request-permissions` may stop waiting on the
//!    helper window. See PINV-61, PINV-64, and PINV-65.
//!
//! `polarize-macos` supplies the real, non-prompting permission re-reads
//! and the real child-process handle; this module only decides, it never
//! calls a system API.

use std::time::Duration;

use crate::script::AutomationCheck;
use crate::wait::Clock;

// ---- needed permissions --------------------------------------------------

/// One permission the guided helper still needs to walk the user
/// through.
///
/// Deliberately carries no status field of its own — see PINV-56. The
/// only Automation state ever named here is "not yet permitted"; there is
/// no `Granted`/`Denied` variant to disagree with a later, independent
/// read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NeededPermission {
    Accessibility,
    ScreenRecording,
    /// `target` is the app name or bundle id Automation was requested
    /// against.
    Automation {
        target: String,
    },
}

/// # PINV-57: `--request-permissions` always attempts the real system prompt before it ever launches the helper
///
/// Decides which permissions the guided helper still needs to cover,
/// from the *results* of the three real-prompt calls
/// (`request_accessibility`, `request_screen_recording`,
/// `request_automation`) — never by skipping them. The caller in
/// `apps/polarize` is responsible for calling those first; this function
/// only reads their outcome.
///
/// `AutomationCheck::Inconclusive` counts as still-needed, alongside
/// `Refused`. Only `Permitted` counts as granted. An inconclusive
/// preflight answer is not evidence the user was ever shown a working
/// consent path, so treating it as "done" could leave Automation broken
/// with no further guidance offered.
pub fn needed_permissions(
    accessibility_trusted: bool,
    screen_recording_trusted: bool,
    automation: AutomationCheck,
    automation_target: &str,
) -> Vec<NeededPermission> {
    let mut needed = Vec::new();
    if !accessibility_trusted {
        needed.push(NeededPermission::Accessibility);
    }
    if !screen_recording_trusted {
        needed.push(NeededPermission::ScreenRecording);
    }
    if automation != AutomationCheck::Permitted {
        needed.push(NeededPermission::Automation {
            target: automation_target.to_string(),
        });
    }
    needed
}

// ---- helper argv ----------------------------------------------------------

/// Builds the guided helper's launch arguments from the still-needed
/// set: one `--needs <name>` pair per permission, `Automation` carrying
/// its target as `automation:<target>`, plus one trailing
/// `--for-bundle <path>` pair when `own_bundle` is known.
///
/// One launch names every missing permission — the helper does not get
/// relaunched once per permission. `own_bundle` is Polarize's own
/// bundle path, resolved by `polarize_macos::setup_helper::own_bundle_path`
/// — never the helper's own bundle (PINV-59). It is `None` for an
/// unbundled dev run, in which case no `--for-bundle` flag is emitted
/// at all and the helper's drag view stays unrendered, which is
/// safe-by-default. The result never carries a status or result field;
/// only [`needed_permissions`]'s own boolean/enum inputs ever decide
/// what is missing, and that decision is made once, by
/// `apps/polarize`, before the helper ever starts. See PINV-56.
pub fn helper_args(needed: &[NeededPermission], own_bundle: Option<&str>) -> Vec<String> {
    let mut args = Vec::with_capacity(needed.len() * 2 + 2);
    for permission in needed {
        args.push("--needs".to_string());
        args.push(match permission {
            NeededPermission::Accessibility => "accessibility".to_string(),
            NeededPermission::ScreenRecording => "screen-recording".to_string(),
            NeededPermission::Automation { target } => format!("automation:{target}"),
        });
    }
    if let Some(bundle) = own_bundle {
        args.push("--for-bundle".to_string());
        args.push(bundle.to_string());
    }
    args
}

// ---- wait loop --------------------------------------------------------

/// How long `wait_for_grants_or_close` waits for a grant before giving
/// up on the helper window. Generous: a user reading System Settings'
/// own instructions and dragging an icon over needs real time, not a
/// developer's idea of "should be quick."
pub const DEFAULT_WAIT_DEADLINE_MS: u64 = 300_000;

/// How often `wait_for_grants_or_close` re-reads permission state and
/// checks whether the helper is still open.
pub const DEFAULT_WAIT_POLL_INTERVAL_MS: u64 = 1_000;

/// How long `wait_for_grants_or_close` waits, after telling the helper
/// every permission is granted, before it kills the helper (PLZ-9).
/// Gives the helper time to swap in and paint its success frame before
/// the window vanishes.
///
/// Cross-language coupling: `SetupHelperCore`'s own quit delay
/// (`SuccessPlan.quitDelaySeconds` in
/// `apps/setup-helper/Sources/SetupHelperCore/SuccessPlan.swift`) must
/// stay well under this value, or the helper's `SIGKILL` could land
/// before it finishes rendering the success frame. No shared constant
/// enforces this — only the comment on each side.
pub const GRANT_SUCCESS_GRACE_MS: u64 = 1_500;

/// A running helper child process, from the wait loop's point of view.
///
/// [`polarize_macos`](../../polarize_macos/index.html)'s real
/// `std::process::Child` implements this. A fake implementation drives
/// the tests below without spawning anything.
pub trait HelperChild {
    /// Whether the child is still running. `false` once it has exited,
    /// on its own or otherwise.
    fn still_running(&mut self) -> bool;
    /// Tells the child the parent's own read says every requested
    /// permission is now granted, so it can show a success frame before
    /// it closes (PLZ-9). A courtesy notification only — the child is
    /// still forcibly ended by a following [`Self::terminate`] call, so
    /// a child that never reacts, or a stale binary with no handler for
    /// it, still gets closed.
    fn notify_all_granted(&mut self);
    /// Ends the child, and waits for it to actually exit. Safe to call
    /// on a child that has already exited.
    fn terminate(&mut self);
}

/// Sleeps for a real or fake duration, so [`wait_for_grants_or_close`]
/// never has to call `std::thread::sleep` directly — a test can inject
/// an instant, no-op sleeper and drive [`Clock`] by hand instead.
pub trait Sleeper {
    fn sleep_ms(&self, ms: u64);
}

/// The real [`Sleeper`], over `std::thread::sleep`.
#[derive(Debug, Default)]
pub struct SystemSleeper;

impl Sleeper for SystemSleeper {
    fn sleep_ms(&self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

/// Why [`wait_for_grants_or_close`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitOutcome {
    /// The most recent poll found nothing still needed.
    AllGranted,
    /// The helper process exited on its own — the user closed the
    /// window, or it crashed — before every permission read granted.
    HelperExited,
    /// The deadline passed with the helper still open and at least one
    /// permission still not granted.
    TimedOut,
}

/// The result of a [`wait_for_grants_or_close`] call: why it returned,
/// and the permission state its own final poll observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitResult {
    pub outcome: WaitOutcome,
    /// What the loop's *last* poll found still needed. Empty exactly
    /// when `outcome` is [`WaitOutcome::AllGranted`].
    pub still_needed: Vec<NeededPermission>,
}

/// # PINV-61 / PINV-64 / PINV-65: the parent alone decides completion, always terminates its helper, and reports exactly the read that decided it
///
/// Polls `poll` — a non-prompting re-read of every requested permission
/// — at least once, then on a bounded interval, until one of three
/// things happens: every permission reads granted, the helper process
/// exits on its own, or `deadline_ms` passes. In every case it calls
/// `child.terminate()` before returning, and the [`WaitResult`] it
/// returns is built from that same poll — there is never a second,
/// independent read backing the report (PINV-65).
///
/// `poll` is checked *before* `child.still_running()` on every
/// iteration, so a helper that exits right as the user finishes granting
/// still reports "all granted," not "helper exited" — the permission
/// read, not the window's own lifecycle, is authoritative.
///
/// On the grant path only (PLZ-9), this calls `child.notify_all_granted()`
/// before `child.terminate()`, then sleeps `grace_ms` between the two —
/// giving the helper time to show a success frame instead of vanishing
/// mid-sentence. `notify_all_granted` is never called on the
/// `HelperExited` or `TimedOut` paths: sending it there would risk a
/// false "Granted!" flash on a window that never actually saw every
/// permission land.
///
/// - Always: returns within `deadline_ms` of when it was called, plus
///   `grace_ms` on the grant path only (plus at most one
///   `poll`/`still_running` call's own real cost), and always calls
///   `child.terminate()` exactly once before returning.
/// - Because: the helper is a launched child UI a user is free to close,
///   ignore, or never see finish — `--request-permissions` must still
///   finish and print a real report from the terminal alone (PINV-61),
///   and it must never leave a stray helper process behind (PINV-64).
/// - If violated: `--request-permissions` hangs on a window the user
///   already dismissed, or exits leaving the helper running, or prints a
///   report that disagrees with what the helper showed on screen
///   (PINV-65).
pub fn wait_for_grants_or_close<C, S, H, F>(
    child: &mut H,
    clock: &C,
    sleeper: &S,
    deadline_ms: u64,
    poll_interval_ms: u64,
    grace_ms: u64,
    mut poll: F,
) -> WaitResult
where
    C: Clock,
    S: Sleeper,
    H: HelperChild,
    F: FnMut() -> Vec<NeededPermission>,
{
    let start = clock.now_ms();
    let deadline = start.saturating_add(deadline_ms);

    loop {
        let still_needed = poll();
        if still_needed.is_empty() {
            child.notify_all_granted();
            sleeper.sleep_ms(grace_ms);
            child.terminate();
            return WaitResult {
                outcome: WaitOutcome::AllGranted,
                still_needed,
            };
        }
        if !child.still_running() {
            child.terminate();
            return WaitResult {
                outcome: WaitOutcome::HelperExited,
                still_needed,
            };
        }
        let now = clock.now_ms();
        if now >= deadline {
            child.terminate();
            return WaitResult {
                outcome: WaitOutcome::TimedOut,
                still_needed,
            };
        }
        let slice = deadline.saturating_sub(now).min(poll_interval_ms);
        sleeper.sleep_ms(slice);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wait::SystemClock;
    use std::cell::{Cell, RefCell};

    // ---- needed_permissions -----------------------------------------

    #[test]
    fn needed_permissions_is_empty_when_everything_is_already_granted() {
        let needed = needed_permissions(true, true, AutomationCheck::Permitted, "Finder");
        assert!(needed.is_empty(), "{needed:?}");
    }

    #[test]
    fn needed_permissions_names_every_permission_still_missing() {
        let needed = needed_permissions(
            false,
            false,
            AutomationCheck::Refused(crate::permission::PermissionState::Denied),
            "Finder",
        );
        assert_eq!(
            needed,
            vec![
                NeededPermission::Accessibility,
                NeededPermission::ScreenRecording,
                NeededPermission::Automation {
                    target: "Finder".to_string()
                },
            ]
        );
    }

    #[test]
    fn needed_permissions_treats_inconclusive_automation_as_still_needed() {
        let needed = needed_permissions(true, true, AutomationCheck::Inconclusive, "Mail");
        assert_eq!(
            needed,
            vec![NeededPermission::Automation {
                target: "Mail".to_string()
            }]
        );
    }

    #[test]
    fn needed_permissions_names_only_accessibility_when_only_it_is_missing() {
        let needed = needed_permissions(false, true, AutomationCheck::Permitted, "Finder");
        assert_eq!(needed, vec![NeededPermission::Accessibility]);
    }

    // ---- helper_args --------------------------------------------------

    #[test]
    fn helper_args_is_empty_for_no_needed_permission_and_no_bundle_path() {
        assert!(helper_args(&[], None).is_empty());
    }

    #[test]
    fn helper_args_names_one_permission_per_needs_flag() {
        let args = helper_args(
            &[
                NeededPermission::Accessibility,
                NeededPermission::ScreenRecording,
                NeededPermission::Automation {
                    target: "Finder".to_string(),
                },
            ],
            None,
        );
        assert_eq!(
            args,
            vec![
                "--needs",
                "accessibility",
                "--needs",
                "screen-recording",
                "--needs",
                "automation:Finder",
            ]
        );
    }

    /// The testable half of PINV-56/PINV-65: the argv surface has no
    /// place for a self-reported permission status to leak through.
    /// Every flag is either `--needs` (naming a permission, and for
    /// Automation its target) or `--for-bundle` (naming Polarize's own
    /// bundle path, PINV-59) — never a word like "granted" or
    /// "denied", which would mean the helper's launch line was carrying
    /// a status this module never computed.
    #[test]
    fn helper_args_never_carries_a_status_or_result_field() {
        let needed = vec![
            NeededPermission::Accessibility,
            NeededPermission::ScreenRecording,
            NeededPermission::Automation {
                target: "Finder".to_string(),
            },
        ];
        let args = helper_args(&needed, Some("/Applications/Polarize.app"));

        // Every flag is "--needs" or "--for-bundle", each paired with
        // exactly one value.
        assert_eq!(args.len() % 2, 0);
        for pair in args.chunks(2) {
            assert!(
                pair[0] == "--needs" || pair[0] == "--for-bundle",
                "unexpected flag {:?}",
                pair[0]
            );
        }
        // No value anywhere spells out a granted/denied/trusted-style
        // status word.
        const STATUS_WORDS: &[&str] = &[
            "granted",
            "denied",
            "trusted",
            "permitted",
            "refused",
            "inconclusive",
            "not_determined",
            "restricted",
        ];
        for value in args.iter().skip(1).step_by(2) {
            let lower = value.to_lowercase();
            for word in STATUS_WORDS {
                assert!(
                    !lower.contains(word),
                    "helper_args value {value:?} looks like it carries a status ({word})"
                );
            }
        }
    }

    #[test]
    fn helper_args_appends_for_bundle_when_the_bundle_path_is_known() {
        let args = helper_args(
            &[NeededPermission::Accessibility],
            Some("/Applications/Polarize.app"),
        );
        assert_eq!(
            args,
            vec![
                "--needs",
                "accessibility",
                "--for-bundle",
                "/Applications/Polarize.app",
            ]
        );
    }

    #[test]
    fn helper_args_omits_for_bundle_when_the_bundle_path_is_unknown() {
        let args = helper_args(&[NeededPermission::Accessibility], None);
        assert_eq!(args, vec!["--needs", "accessibility"]);
    }

    // ---- wait_for_grants_or_close --------------------------------------

    /// A clock a test drives by hand. Nothing here ever sleeps.
    #[derive(Debug, Default)]
    struct FakeClock {
        ms: Cell<u64>,
    }

    impl FakeClock {
        fn advance(&self, ms: u64) {
            self.ms.set(self.ms.get() + ms);
        }
    }

    impl Clock for FakeClock {
        fn now_ms(&self) -> u64 {
            self.ms.get()
        }
    }

    /// A sleeper that advances the fake clock instead of blocking the
    /// test thread.
    struct FakeSleeper<'a> {
        clock: &'a FakeClock,
        calls: Cell<u32>,
    }

    impl<'a> FakeSleeper<'a> {
        fn new(clock: &'a FakeClock) -> Self {
            Self {
                clock,
                calls: Cell::new(0),
            }
        }
    }

    impl Sleeper for FakeSleeper<'_> {
        fn sleep_ms(&self, ms: u64) {
            self.calls.set(self.calls.get() + 1);
            self.clock.advance(ms);
        }
    }

    /// A helper child a test drives by hand: `still_running` reports
    /// whatever `running` currently holds, `terminate` and
    /// `notify_all_granted` are each counted so a test can assert how
    /// often they ran, and `call_log` records the order the two were
    /// called in.
    struct FakeChild {
        running: bool,
        terminate_calls: RefCell<u32>,
        notify_calls: RefCell<u32>,
        call_log: RefCell<Vec<&'static str>>,
    }

    impl FakeChild {
        fn running() -> Self {
            Self {
                running: true,
                terminate_calls: RefCell::new(0),
                notify_calls: RefCell::new(0),
                call_log: RefCell::new(Vec::new()),
            }
        }

        fn already_exited() -> Self {
            Self {
                running: false,
                terminate_calls: RefCell::new(0),
                notify_calls: RefCell::new(0),
                call_log: RefCell::new(Vec::new()),
            }
        }
    }

    impl HelperChild for FakeChild {
        fn still_running(&mut self) -> bool {
            self.running
        }

        fn notify_all_granted(&mut self) {
            *self.notify_calls.borrow_mut() += 1;
            self.call_log.borrow_mut().push("notify");
        }

        fn terminate(&mut self) {
            *self.terminate_calls.borrow_mut() += 1;
            self.call_log.borrow_mut().push("terminate");
            self.running = false;
        }
    }

    fn accessibility_still_needed() -> Vec<NeededPermission> {
        vec![NeededPermission::Accessibility]
    }

    #[test]
    fn returns_all_granted_immediately_when_the_first_poll_finds_nothing_missing() {
        let clock = FakeClock::default();
        let sleeper = FakeSleeper::new(&clock);
        let mut child = FakeChild::running();

        let result = wait_for_grants_or_close(
            &mut child,
            &clock,
            &sleeper,
            DEFAULT_WAIT_DEADLINE_MS,
            DEFAULT_WAIT_POLL_INTERVAL_MS,
            0,
            Vec::new,
        );

        assert_eq!(result.outcome, WaitOutcome::AllGranted);
        assert!(result.still_needed.is_empty());
        assert_eq!(*child.terminate_calls.borrow(), 1);
        assert_eq!(
            sleeper.calls.get(),
            1,
            "a grant on the first poll must not wait between polls, only the one grace sleep (0ms here) runs"
        );
        assert_eq!(
            clock.now_ms(),
            0,
            "grace_ms is 0, so the grace sleep advances nothing"
        );
    }

    #[test]
    fn a_helper_that_never_exits_still_returns_bounded_by_the_deadline_and_is_terminated() {
        let clock = FakeClock::default();
        let sleeper = FakeSleeper::new(&clock);
        let mut child = FakeChild::running();

        let result = wait_for_grants_or_close(
            &mut child,
            &clock,
            &sleeper,
            2_000,
            500,
            0,
            accessibility_still_needed,
        );

        assert_eq!(result.outcome, WaitOutcome::TimedOut);
        assert_eq!(result.still_needed, accessibility_still_needed());
        assert_eq!(*child.terminate_calls.borrow(), 1);
        assert_eq!(
            *child.notify_calls.borrow(),
            0,
            "a timeout must never notify"
        );
        assert_eq!(clock.now_ms(), 2_000, "must not overshoot the deadline");
    }

    #[test]
    fn a_helper_that_exits_early_still_returns_with_the_polls_own_state_not_the_childs() {
        let clock = FakeClock::default();
        let sleeper = FakeSleeper::new(&clock);
        let mut child = FakeChild::already_exited();

        let result = wait_for_grants_or_close(
            &mut child,
            &clock,
            &sleeper,
            DEFAULT_WAIT_DEADLINE_MS,
            DEFAULT_WAIT_POLL_INTERVAL_MS,
            0,
            accessibility_still_needed,
        );

        assert_eq!(result.outcome, WaitOutcome::HelperExited);
        // The permission state comes from `poll`, not from any status
        // the child process itself carried — PINV-65.
        assert_eq!(result.still_needed, accessibility_still_needed());
        assert_eq!(*child.terminate_calls.borrow(), 1);
        assert_eq!(
            *child.notify_calls.borrow(),
            0,
            "a helper that exited on its own must never be told it succeeded"
        );
    }

    #[test]
    fn a_grant_observed_after_the_helper_already_exited_still_reports_all_granted() {
        // The poll runs before the still_running check on every
        // iteration, so a grant that lands in the same instant the
        // helper closes is reported as a grant, not as "helper exited."
        let clock = FakeClock::default();
        let sleeper = FakeSleeper::new(&clock);
        let mut child = FakeChild::already_exited();

        let result = wait_for_grants_or_close(
            &mut child,
            &clock,
            &sleeper,
            DEFAULT_WAIT_DEADLINE_MS,
            DEFAULT_WAIT_POLL_INTERVAL_MS,
            0,
            Vec::new,
        );

        assert_eq!(result.outcome, WaitOutcome::AllGranted);
    }

    #[test]
    fn polls_and_sleeps_between_checks_until_the_grant_lands() {
        let clock = FakeClock::default();
        let sleeper = FakeSleeper::new(&clock);
        let mut child = FakeChild::running();
        let poll_calls = Cell::new(0u32);

        let result = wait_for_grants_or_close(
            &mut child,
            &clock,
            &sleeper,
            DEFAULT_WAIT_DEADLINE_MS,
            1_000,
            0,
            || {
                let n = poll_calls.get();
                poll_calls.set(n + 1);
                if n < 2 {
                    accessibility_still_needed()
                } else {
                    vec![]
                }
            },
        );

        assert_eq!(result.outcome, WaitOutcome::AllGranted);
        assert_eq!(poll_calls.get(), 3);
        assert_eq!(
            sleeper.calls.get(),
            3,
            "two polling sleeps plus the final (0ms) grace sleep on the grant"
        );
        assert_eq!(
            clock.now_ms(),
            2_000,
            "grace_ms is 0, so the grace sleep adds no time"
        );
    }

    #[test]
    fn each_wait_slice_is_bounded_by_the_time_left_to_the_deadline() {
        // 2_500 ms of deadline at a 1_000 ms poll interval leaves 500 ms
        // for the last slice.
        let clock = FakeClock::default();
        let sleeper = FakeSleeper::new(&clock);
        let mut child = FakeChild::running();

        let _ = wait_for_grants_or_close(
            &mut child,
            &clock,
            &sleeper,
            2_500,
            1_000,
            0,
            accessibility_still_needed,
        );

        assert_eq!(sleeper.calls.get(), 3);
        assert_eq!(clock.now_ms(), 2_500);
    }

    #[test]
    fn all_granted_notifies_before_terminating_and_sleeps_for_exactly_one_grace_period() {
        let clock = FakeClock::default();
        let sleeper = FakeSleeper::new(&clock);
        let mut child = FakeChild::running();

        let result = wait_for_grants_or_close(
            &mut child,
            &clock,
            &sleeper,
            DEFAULT_WAIT_DEADLINE_MS,
            DEFAULT_WAIT_POLL_INTERVAL_MS,
            GRANT_SUCCESS_GRACE_MS,
            Vec::new,
        );

        assert_eq!(result.outcome, WaitOutcome::AllGranted);
        assert_eq!(*child.notify_calls.borrow(), 1);
        assert_eq!(*child.terminate_calls.borrow(), 1);
        assert_eq!(
            *child.call_log.borrow(),
            vec!["notify", "terminate"],
            "notify_all_granted must run before terminate"
        );
        assert_eq!(
            sleeper.calls.get(),
            1,
            "exactly one grace sleep, no polling sleep on an immediate grant"
        );
        assert_eq!(clock.now_ms(), GRANT_SUCCESS_GRACE_MS);
    }

    #[test]
    fn helper_exited_never_calls_notify_all_granted() {
        let clock = FakeClock::default();
        let sleeper = FakeSleeper::new(&clock);
        let mut child = FakeChild::already_exited();

        let _ = wait_for_grants_or_close(
            &mut child,
            &clock,
            &sleeper,
            DEFAULT_WAIT_DEADLINE_MS,
            DEFAULT_WAIT_POLL_INTERVAL_MS,
            GRANT_SUCCESS_GRACE_MS,
            accessibility_still_needed,
        );

        assert_eq!(*child.notify_calls.borrow(), 0);
    }

    #[test]
    fn timed_out_never_calls_notify_all_granted() {
        let clock = FakeClock::default();
        let sleeper = FakeSleeper::new(&clock);
        let mut child = FakeChild::running();

        let _ = wait_for_grants_or_close(
            &mut child,
            &clock,
            &sleeper,
            2_000,
            500,
            GRANT_SUCCESS_GRACE_MS,
            accessibility_still_needed,
        );

        assert_eq!(*child.notify_calls.borrow(), 0);
    }

    #[test]
    fn the_system_sleeper_actually_sleeps() {
        let sleeper = SystemSleeper;
        let clock = SystemClock::new();
        let before = clock.now_ms();
        sleeper.sleep_ms(5);
        let after = clock.now_ms();
        assert!(after >= before + 5, "before={before} after={after}");
    }
}
