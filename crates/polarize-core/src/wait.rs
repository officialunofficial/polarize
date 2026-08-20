//! Waiting for a UI change, instead of polling `describe` in a loop.
//!
//! Today a caller must call `describe` again and again to notice that a
//! sheet opened or a button appeared. The two functions here do that
//! waiting inside one tool call: [`perform_await_ui_element`] waits for
//! one element to appear, and [`perform_await_screen_idle`] waits for
//! the tree to stop changing.
//!
//! ## Why the wait is a hybrid
//!
//! macOS can wake this crate up when an app posts an accessibility
//! notification. `polarize-macos` exposes that through
//! [`UiChangeWaiter`]. A pure notification design would be wrong,
//! because some accessibility trees under-report. A web view inside a
//! native window is the common case: its content changes, and no
//! `AXLayoutChanged` arrives. So every wait here is bounded by a poll
//! interval as well. A missed notification then costs one poll
//! interval, not the whole timeout. See PINV-19 in `docs/INVARIANTS.md`.
//!
//! ## Why a `Clock` trait
//!
//! The waiting policy is pure logic, so it is unit-tested here. A test
//! that sleeps for real is a slow test and a flaky test. [`Clock`] lets
//! a test drive time by hand. [`SystemClock`] is the real
//! implementation the server uses.

use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ax::AxNode;
use crate::error::PolarizeError;
use crate::schema::AppIdentifier;
use crate::selector::{self, ElementPath, ElementSelector, SelectorError};
use crate::traits::{AccessibilityInspector, UiChangeWaiter};

// ---- defaults and clamps ------------------------------------------------

/// How long a wait runs when the caller names no timeout.
pub const DEFAULT_TIMEOUT_MS: u64 = 5_000;

/// The longest wait a caller may ask for. A tool call that blocks for
/// longer than two minutes looks like a hung server to an MCP client.
pub const MAX_TIMEOUT_MS: u64 = 120_000;

/// How long one wait slice runs when the caller names no poll interval.
/// This is the worst-case cost of a notification the app never posts.
pub const DEFAULT_POLL_INTERVAL_MS: u64 = 250;

/// The shortest poll interval a caller may ask for. Each poll walks the
/// whole accessibility tree, which is not cheap.
pub const MIN_POLL_INTERVAL_MS: u64 = 10;

/// The longest poll interval a caller may ask for. A longer interval
/// would make the notification fallback useless.
pub const MAX_POLL_INTERVAL_MS: u64 = 5_000;

/// How long the tree must stay unchanged when the caller names no idle
/// window.
pub const DEFAULT_IDLE_MS: u64 = 500;

/// The longest idle window a caller may ask for.
pub const MAX_IDLE_MS: u64 = 30_000;

// ---- clock --------------------------------------------------------------

/// A monotonic millisecond counter.
///
/// The counter's zero point does not matter. This crate only ever reads
/// differences between two calls.
pub trait Clock {
    /// Milliseconds since this clock started. Never goes backwards.
    fn now_ms(&self) -> u64;
}

/// The real [`Clock`], over [`std::time::Instant`].
#[derive(Debug)]
pub struct SystemClock {
    start: std::time::Instant,
}

impl SystemClock {
    /// Starts a new clock at zero.
    pub fn new() -> Self {
        Self {
            start: std::time::Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        // A `u64` of milliseconds covers 584 million years, so the
        // saturating cast can never lose a real measurement.
        self.start.elapsed().as_millis().min(u64::MAX as u128) as u64
    }
}

// ---- resolved settings --------------------------------------------------

/// A caller's timeout and poll interval, after defaults and clamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitBudget {
    pub timeout_ms: u64,
    pub poll_interval_ms: u64,
}

impl WaitBudget {
    /// Applies the defaults and the clamps this module documents.
    ///
    /// A zero timeout is kept as zero. It is a legal request, and it
    /// means "check once, then give up" — see PINV-19.
    pub fn resolve(timeout_ms: Option<u64>, poll_interval_ms: Option<u64>) -> Self {
        Self {
            timeout_ms: timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS).min(MAX_TIMEOUT_MS),
            poll_interval_ms: poll_interval_ms
                .unwrap_or(DEFAULT_POLL_INTERVAL_MS)
                .clamp(MIN_POLL_INTERVAL_MS, MAX_POLL_INTERVAL_MS),
        }
    }
}

/// Applies the default and the clamp for an idle window.
///
/// The idle window is not clamped against the timeout. An idle window
/// longer than the timeout always times out, and the error names both
/// numbers, which tells the caller more than a silent adjustment does.
pub fn resolve_idle_ms(idle_ms: Option<u64>) -> u64 {
    idle_ms.unwrap_or(DEFAULT_IDLE_MS).min(MAX_IDLE_MS)
}

// ---- errors -------------------------------------------------------------

/// A wait that reached its deadline.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WaitError {
    /// [`perform_await_ui_element`] never matched its selector.
    #[error(
        "timed out after {waited_ms} ms and {polls} tree check(s): no element matches selector ({selector})"
    )]
    ElementTimeout {
        selector: String,
        waited_ms: u64,
        polls: u32,
    },

    /// [`perform_await_screen_idle`] never saw the tree hold still.
    #[error(
        "timed out after {waited_ms} ms and {polls} tree check(s): the UI never stayed unchanged for {idle_ms} ms"
    )]
    IdleTimeout {
        idle_ms: u64,
        waited_ms: u64,
        polls: u32,
    },
}

/// A timeout travels to the caller as a [`PolarizeError`].
///
/// `PolarizeError` has no `Wait` variant yet, because `error.rs` belongs
/// to another change in flight. `Platform` carries the full message, so
/// the caller still reads exactly what expired and after how long.
impl From<WaitError> for PolarizeError {
    fn from(err: WaitError) -> Self {
        PolarizeError::Platform(err.to_string())
    }
}

// ---- requests and responses ---------------------------------------------

/// Waits for one element to appear in an app's accessibility tree.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct AwaitUiElementRequest {
    /// Which app to watch. `None` watches the frontmost app.
    #[serde(default)]
    pub app: Option<AppIdentifier>,
    /// The element to wait for. It must name at least one criterion,
    /// exactly as [`crate::selector::find_one`] requires (PINV-15).
    pub selector: ElementSelector,
    /// Gives up after this many milliseconds. Defaults to
    /// [`DEFAULT_TIMEOUT_MS`], clamped to [`MAX_TIMEOUT_MS`].
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Re-reads the tree at least this often, even when the app posts
    /// no notification. Defaults to [`DEFAULT_POLL_INTERVAL_MS`],
    /// clamped to [`MIN_POLL_INTERVAL_MS`]..=[`MAX_POLL_INTERVAL_MS`].
    #[serde(default)]
    pub poll_interval_ms: Option<u64>,
}

/// The element [`perform_await_ui_element`] found.
///
/// There is no "found" flag. A wait that never matches returns an error
/// instead, so an `Ok` response always describes a real element.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AwaitUiElementResponse {
    pub app_name: String,
    /// The child indices from the tree root down to the element.
    pub path: ElementPath,
    /// The matched element, and its whole subtree.
    pub node: AxNode,
    /// How long the wait took, in milliseconds.
    pub waited_ms: u64,
    /// How many times the wait read the accessibility tree.
    pub polls: u32,
}

/// Waits for an app's accessibility tree to stop changing.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct AwaitScreenIdleRequest {
    /// Which app to watch. `None` watches the frontmost app.
    #[serde(default)]
    pub app: Option<AppIdentifier>,
    /// The tree must stay unchanged for this many milliseconds.
    /// Defaults to [`DEFAULT_IDLE_MS`], clamped to [`MAX_IDLE_MS`].
    #[serde(default)]
    pub idle_ms: Option<u64>,
    /// Gives up after this many milliseconds. Defaults to
    /// [`DEFAULT_TIMEOUT_MS`], clamped to [`MAX_TIMEOUT_MS`].
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Re-reads the tree at least this often. Defaults to
    /// [`DEFAULT_POLL_INTERVAL_MS`], clamped to
    /// [`MIN_POLL_INTERVAL_MS`]..=[`MAX_POLL_INTERVAL_MS`].
    #[serde(default)]
    pub poll_interval_ms: Option<u64>,
}

/// The result of a successful [`perform_await_screen_idle`] call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AwaitScreenIdleResponse {
    pub app_name: String,
    /// The idle window that was satisfied, after defaults and clamps.
    pub idle_ms: u64,
    /// How long the wait took, in milliseconds.
    pub waited_ms: u64,
    /// How many times the wait read the accessibility tree.
    pub polls: u32,
}

// ---- orchestration ------------------------------------------------------

/// # PINV-19: a wait checks at least once, and never waits past its deadline
///
/// - Always: [`perform_await_ui_element`] and
///   [`perform_await_screen_idle`] read the accessibility tree once
///   before they can report a timeout, even when `timeout_ms` is `0`.
///   Between two reads they wait for at most
///   `min(poll_interval_ms, milliseconds left to the deadline)`. A
///   [`UiChangeWaiter`] that returns `false` neither ends the wait early
///   nor extends it. A [`SelectorError::Empty`] fails at once, without
///   waiting.
/// - Because: this is the hybrid design. `polarize-macos` wakes the wait
///   on an accessibility notification, but some trees never post one — a
///   web view inside a native window is the usual case. Bounding each
///   wait by the poll interval turns a missed notification into one
///   extra poll instead of a hang for the whole timeout. Checking once
///   before the deadline test matters because `timeout_ms: 0` is a legal
///   "is it there right now" request. An empty selector matches the
///   application root, so retrying it would waste the whole timeout on a
///   request that is already wrong.
/// - If violated: `await_ui_element` blocks for its full timeout against
///   any app whose accessibility tree under-reports, or a zero timeout
///   returns a timeout error without ever looking at the tree.
pub fn perform_await_ui_element<A, W, C>(
    inspector: &A,
    waiter: &W,
    clock: &C,
    request: &AwaitUiElementRequest,
) -> Result<AwaitUiElementResponse, PolarizeError>
where
    A: AccessibilityInspector,
    W: UiChangeWaiter,
    C: Clock,
{
    let budget = WaitBudget::resolve(request.timeout_ms, request.poll_interval_ms);
    let app = request.app.as_ref();
    let start = clock.now_ms();
    let deadline = start.saturating_add(budget.timeout_ms);
    let mut polls: u32 = 0;

    loop {
        let (app_name, root) = inspector.describe(app)?;
        polls = polls.saturating_add(1);

        match selector::find_one(&root, &request.selector) {
            Ok(path) => {
                let node = selector::node_at_path(&root, &path)
                    .expect("find_one resolves a path into the tree it searched")
                    .clone();
                return Ok(AwaitUiElementResponse {
                    app_name,
                    path,
                    node,
                    waited_ms: clock.now_ms().saturating_sub(start),
                    polls,
                });
            }
            // An empty selector is wrong now and stays wrong. Waiting
            // for it would burn the whole timeout. See PINV-19.
            Err(empty @ SelectorError::Empty) => return Err(empty.into()),
            // No match yet, or not enough matches for `index` yet. Both
            // can still come true, so keep waiting.
            Err(_) => {}
        }

        let now = clock.now_ms();
        if now >= deadline {
            return Err(WaitError::ElementTimeout {
                selector: request.selector.describe(),
                waited_ms: now.saturating_sub(start),
                polls,
            }
            .into());
        }
        waiter.wait_for_change(app, next_slice(now, deadline, budget.poll_interval_ms))?;
    }
}

/// The wait before the next tree read: the poll interval, or the time
/// left to the deadline, whichever is shorter. This bound is the hybrid
/// design — see PINV-19.
fn next_slice(now_ms: u64, deadline_ms: u64, poll_interval_ms: u64) -> Duration {
    Duration::from_millis(deadline_ms.saturating_sub(now_ms).min(poll_interval_ms))
}

/// Waits until an app's accessibility tree holds still.
///
/// The wait succeeds once two consecutive reads compare equal for at
/// least `idle_ms`. See PINV-19 for the timing rules it shares with
/// [`perform_await_ui_element`].
pub fn perform_await_screen_idle<A, W, C>(
    inspector: &A,
    waiter: &W,
    clock: &C,
    request: &AwaitScreenIdleRequest,
) -> Result<AwaitScreenIdleResponse, PolarizeError>
where
    A: AccessibilityInspector,
    W: UiChangeWaiter,
    C: Clock,
{
    let budget = WaitBudget::resolve(request.timeout_ms, request.poll_interval_ms);
    let idle_ms = resolve_idle_ms(request.idle_ms);
    let app = request.app.as_ref();
    let start = clock.now_ms();
    let deadline = start.saturating_add(budget.timeout_ms);

    let (app_name, mut previous) = inspector.describe(app)?;
    let mut polls: u32 = 1;
    let mut unchanged_since = clock.now_ms();

    loop {
        let now = clock.now_ms();
        // The idle test runs before the deadline test. A wait that goes
        // idle on the very millisecond of its deadline succeeded.
        if now.saturating_sub(unchanged_since) >= idle_ms {
            return Ok(AwaitScreenIdleResponse {
                app_name,
                idle_ms,
                waited_ms: now.saturating_sub(start),
                polls,
            });
        }
        if now >= deadline {
            return Err(WaitError::IdleTimeout {
                idle_ms,
                waited_ms: now.saturating_sub(start),
                polls,
            }
            .into());
        }

        waiter.wait_for_change(app, next_slice(now, deadline, budget.poll_interval_ms))?;

        let (_, current) = inspector.describe(app)?;
        polls = polls.saturating_add(1);
        if current != previous {
            previous = current;
            unchanged_since = clock.now_ms();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    // ---- fakes -----------------------------------------------------

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

    /// An inspector that returns each tree in `trees` once, then repeats
    /// the last one for every further call.
    struct FakeInspector {
        app_name: String,
        trees: Vec<AxNode>,
        calls: Cell<usize>,
        seen_apps: RefCell<Vec<Option<AppIdentifier>>>,
        fail_on_call: Option<usize>,
    }

    impl FakeInspector {
        fn new(trees: Vec<AxNode>) -> Self {
            Self {
                app_name: "TextEdit".to_string(),
                trees,
                calls: Cell::new(0),
                seen_apps: RefCell::new(Vec::new()),
                fail_on_call: None,
            }
        }

        fn failing_on_call(trees: Vec<AxNode>, call: usize) -> Self {
            Self {
                fail_on_call: Some(call),
                ..Self::new(trees)
            }
        }

        fn call_count(&self) -> usize {
            self.calls.get()
        }
    }

    impl AccessibilityInspector for FakeInspector {
        fn describe(&self, app: Option<&AppIdentifier>) -> Result<(String, AxNode), PolarizeError> {
            let index = self.calls.get();
            self.calls.set(index + 1);
            self.seen_apps.borrow_mut().push(app.cloned());
            if self.fail_on_call == Some(index + 1) {
                return Err(PolarizeError::AppNotFound("gone".to_string()));
            }
            let tree = self
                .trees
                .get(index)
                .or_else(|| self.trees.last())
                .expect("FakeInspector needs at least one tree")
                .clone();
            Ok((self.app_name.clone(), tree))
        }
    }

    /// What a [`FakeWaiter`] does with the budget it is handed.
    enum WaiterMode {
        /// Consumes the whole budget, then reports no change. This is
        /// the app that never posts a notification.
        NeverSignals,
        /// Reports a change after `ms`, if the budget allows it.
        SignalsAfter(u64),
        /// Fails, to prove the error reaches the caller.
        Fails,
    }

    struct FakeWaiter {
        clock: Rc<FakeClock>,
        mode: WaiterMode,
        budgets_ms: RefCell<Vec<u64>>,
        seen_apps: RefCell<Vec<Option<AppIdentifier>>>,
    }

    impl FakeWaiter {
        fn new(clock: Rc<FakeClock>, mode: WaiterMode) -> Self {
            Self {
                clock,
                mode,
                budgets_ms: RefCell::new(Vec::new()),
                seen_apps: RefCell::new(Vec::new()),
            }
        }

        fn budgets(&self) -> Vec<u64> {
            self.budgets_ms.borrow().clone()
        }
    }

    impl UiChangeWaiter for FakeWaiter {
        fn wait_for_change(
            &self,
            app: Option<&AppIdentifier>,
            budget: Duration,
        ) -> Result<bool, PolarizeError> {
            let budget_ms = budget.as_millis() as u64;
            self.budgets_ms.borrow_mut().push(budget_ms);
            self.seen_apps.borrow_mut().push(app.cloned());
            match self.mode {
                WaiterMode::NeverSignals => {
                    self.clock.advance(budget_ms);
                    Ok(false)
                }
                WaiterMode::SignalsAfter(ms) => {
                    let elapsed = ms.min(budget_ms);
                    self.clock.advance(elapsed);
                    Ok(ms <= budget_ms)
                }
                WaiterMode::Fails => Err(PolarizeError::Platform("observer failed".to_string())),
            }
        }
    }

    // ---- test trees ------------------------------------------------

    fn empty_window() -> AxNode {
        AxNode {
            role: "AXWindow".to_string(),
            label: Some("Untitled".to_string()),
            ..AxNode::default()
        }
    }

    fn window_with_save_button() -> AxNode {
        AxNode {
            children: vec![AxNode {
                role: "AXButton".to_string(),
                label: Some("Save".to_string()),
                identifier: Some("save".to_string()),
                actions: vec!["AXPress".to_string()],
                interactive: true,
                ..AxNode::default()
            }],
            ..empty_window()
        }
    }

    fn save_selector() -> ElementSelector {
        ElementSelector {
            identifier: Some("save".to_string()),
            ..ElementSelector::default()
        }
    }

    fn element_request(trees_timeout: Option<u64>) -> AwaitUiElementRequest {
        AwaitUiElementRequest {
            app: None,
            selector: save_selector(),
            timeout_ms: trees_timeout,
            poll_interval_ms: None,
        }
    }

    fn setup(mode: WaiterMode) -> (Rc<FakeClock>, FakeWaiter) {
        let clock = Rc::new(FakeClock::default());
        let waiter = FakeWaiter::new(Rc::clone(&clock), mode);
        (clock, waiter)
    }

    // ---- defaults and clamps ---------------------------------------

    #[test]
    fn wait_budget_applies_the_documented_defaults() {
        let budget = WaitBudget::resolve(None, None);
        assert_eq!(budget.timeout_ms, DEFAULT_TIMEOUT_MS);
        assert_eq!(budget.poll_interval_ms, DEFAULT_POLL_INTERVAL_MS);
    }

    #[test]
    fn wait_budget_clamps_an_absurd_timeout_and_poll_interval() {
        let budget = WaitBudget::resolve(Some(u64::MAX), Some(u64::MAX));
        assert_eq!(budget.timeout_ms, MAX_TIMEOUT_MS);
        assert_eq!(budget.poll_interval_ms, MAX_POLL_INTERVAL_MS);

        let budget = WaitBudget::resolve(Some(0), Some(0));
        assert_eq!(budget.timeout_ms, 0, "a zero timeout stays zero");
        assert_eq!(budget.poll_interval_ms, MIN_POLL_INTERVAL_MS);
    }

    #[test]
    fn wait_budget_keeps_a_reasonable_request_unchanged() {
        let budget = WaitBudget::resolve(Some(1_500), Some(100));
        assert_eq!(budget.timeout_ms, 1_500);
        assert_eq!(budget.poll_interval_ms, 100);
    }

    #[test]
    fn idle_window_defaults_and_clamps() {
        assert_eq!(resolve_idle_ms(None), DEFAULT_IDLE_MS);
        assert_eq!(resolve_idle_ms(Some(u64::MAX)), MAX_IDLE_MS);
        assert_eq!(resolve_idle_ms(Some(0)), 0);
        assert_eq!(resolve_idle_ms(Some(750)), 750);
    }

    // ---- await_ui_element ------------------------------------------

    #[test]
    fn await_ui_element_matches_on_the_first_try_without_waiting() {
        let (_clock, waiter) = setup(WaiterMode::NeverSignals);
        let inspector = FakeInspector::new(vec![window_with_save_button()]);

        let response =
            perform_await_ui_element(&inspector, &waiter, &*_clock, &element_request(None))
                .unwrap();

        assert_eq!(response.polls, 1);
        assert_eq!(response.waited_ms, 0);
        assert_eq!(response.path, vec![0]);
        assert_eq!(response.app_name, "TextEdit");
        assert!(
            waiter.budgets().is_empty(),
            "a match on the first read must not wait at all"
        );
    }

    #[test]
    fn await_ui_element_returns_the_resolved_path_and_the_matched_node() {
        let (clock, waiter) = setup(WaiterMode::NeverSignals);
        let inspector = FakeInspector::new(vec![window_with_save_button()]);

        let response =
            perform_await_ui_element(&inspector, &waiter, &*clock, &element_request(None)).unwrap();

        assert_eq!(response.path, vec![0]);
        assert_eq!(response.node.label.as_deref(), Some("Save"));
        assert_eq!(response.node.identifier.as_deref(), Some("save"));
    }

    #[test]
    fn await_ui_element_matches_after_several_polls() {
        let (clock, waiter) = setup(WaiterMode::NeverSignals);
        let inspector = FakeInspector::new(vec![
            empty_window(),
            empty_window(),
            window_with_save_button(),
        ]);

        let response =
            perform_await_ui_element(&inspector, &waiter, &*clock, &element_request(None)).unwrap();

        assert_eq!(response.polls, 3);
        assert_eq!(response.waited_ms, 2 * DEFAULT_POLL_INTERVAL_MS);
        assert_eq!(
            waiter.budgets(),
            vec![DEFAULT_POLL_INTERVAL_MS, DEFAULT_POLL_INTERVAL_MS]
        );
    }

    #[test]
    fn await_ui_element_still_matches_when_the_waiter_never_signals() {
        // The poll fallback is the whole point of the hybrid design.
        // Every `wait_for_change` here reports "no change", and the
        // element is still found. See PINV-19.
        let (clock, waiter) = setup(WaiterMode::NeverSignals);
        let mut trees = vec![empty_window(); 8];
        trees.push(window_with_save_button());
        let inspector = FakeInspector::new(trees);

        let response =
            perform_await_ui_element(&inspector, &waiter, &*clock, &element_request(None)).unwrap();

        assert_eq!(response.polls, 9);
        assert_eq!(waiter.budgets().len(), 8);
    }

    #[test]
    fn await_ui_element_re_reads_the_tree_as_soon_as_the_waiter_signals() {
        let (clock, waiter) = setup(WaiterMode::SignalsAfter(5));
        let inspector = FakeInspector::new(vec![empty_window(), window_with_save_button()]);

        let response =
            perform_await_ui_element(&inspector, &waiter, &*clock, &element_request(None)).unwrap();

        assert_eq!(response.polls, 2);
        assert_eq!(
            response.waited_ms, 5,
            "a notification must end the wait slice early"
        );
    }

    #[test]
    fn await_ui_element_bounds_each_wait_by_the_time_left() {
        // 600 ms of timeout at a 250 ms poll interval leaves 100 ms for
        // the last slice. See PINV-19.
        let (clock, waiter) = setup(WaiterMode::NeverSignals);
        let inspector = FakeInspector::new(vec![empty_window()]);

        let err =
            perform_await_ui_element(&inspector, &waiter, &*clock, &element_request(Some(600)))
                .unwrap_err();

        assert!(err.to_string().contains("timed out after 600 ms"));
        assert_eq!(waiter.budgets(), vec![250, 250, 100]);
    }

    #[test]
    fn await_ui_element_times_out_with_a_clear_error() {
        let (clock, waiter) = setup(WaiterMode::NeverSignals);
        let inspector = FakeInspector::new(vec![empty_window()]);

        let err =
            perform_await_ui_element(&inspector, &waiter, &*clock, &element_request(Some(1_000)))
                .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("timed out after 1000 ms"), "{message}");
        assert!(message.contains("save"), "{message}");
        assert_eq!(inspector.call_count(), 5, "4 waits of 250 ms, 5 reads");
    }

    #[test]
    fn await_ui_element_with_a_zero_timeout_still_reads_the_tree_once() {
        let (clock, waiter) = setup(WaiterMode::NeverSignals);
        let inspector = FakeInspector::new(vec![empty_window()]);

        let err = perform_await_ui_element(&inspector, &waiter, &*clock, &element_request(Some(0)))
            .unwrap_err();

        assert!(err.to_string().contains("1 tree check"));
        assert_eq!(inspector.call_count(), 1);
        assert!(
            waiter.budgets().is_empty(),
            "a zero timeout leaves nothing to wait for"
        );
    }

    #[test]
    fn await_ui_element_with_a_zero_timeout_still_matches_a_present_element() {
        let (clock, waiter) = setup(WaiterMode::NeverSignals);
        let inspector = FakeInspector::new(vec![window_with_save_button()]);

        let response =
            perform_await_ui_element(&inspector, &waiter, &*clock, &element_request(Some(0)))
                .unwrap();

        assert_eq!(response.polls, 1);
    }

    #[test]
    fn await_ui_element_rejects_an_empty_selector_at_once() {
        let (clock, waiter) = setup(WaiterMode::NeverSignals);
        let inspector = FakeInspector::new(vec![empty_window()]);
        let request = AwaitUiElementRequest {
            selector: ElementSelector::default(),
            ..element_request(None)
        };

        let err = perform_await_ui_element(&inspector, &waiter, &*clock, &request).unwrap_err();

        assert!(matches!(err, PolarizeError::Selector(SelectorError::Empty)));
        assert_eq!(inspector.call_count(), 1);
        assert!(
            waiter.budgets().is_empty(),
            "an empty selector must not wait for the timeout"
        );
    }

    #[test]
    fn await_ui_element_keeps_waiting_when_the_index_is_not_reached_yet() {
        // Two matches are needed; the first read has one. This is
        // `SelectorError::IndexOutOfRange`, which can still come true.
        let (clock, waiter) = setup(WaiterMode::NeverSignals);
        let two_buttons = AxNode {
            children: vec![
                window_with_save_button().children[0].clone(),
                window_with_save_button().children[0].clone(),
            ],
            ..empty_window()
        };
        let inspector = FakeInspector::new(vec![window_with_save_button(), two_buttons]);
        let request = AwaitUiElementRequest {
            selector: ElementSelector {
                index: Some(1),
                ..save_selector()
            },
            ..element_request(None)
        };

        let response = perform_await_ui_element(&inspector, &waiter, &*clock, &request).unwrap();

        assert_eq!(response.polls, 2);
        assert_eq!(response.path, vec![1]);
    }

    #[test]
    fn await_ui_element_passes_the_app_to_the_inspector_and_the_waiter() {
        let (clock, waiter) = setup(WaiterMode::NeverSignals);
        let inspector = FakeInspector::new(vec![empty_window(), window_with_save_button()]);
        let app = AppIdentifier {
            bundle_id: Some("com.apple.TextEdit".to_string()),
            app_name: None,
        };
        let request = AwaitUiElementRequest {
            app: Some(app.clone()),
            ..element_request(None)
        };

        perform_await_ui_element(&inspector, &waiter, &*clock, &request).unwrap();

        assert_eq!(
            inspector.seen_apps.borrow().as_slice(),
            &[Some(app.clone()), Some(app.clone())]
        );
        assert_eq!(waiter.seen_apps.borrow().as_slice(), &[Some(app)]);
    }

    #[test]
    fn await_ui_element_reports_an_inspector_failure_rather_than_retrying_it() {
        let (clock, waiter) = setup(WaiterMode::NeverSignals);
        let inspector = FakeInspector::failing_on_call(vec![empty_window()], 1);

        let err = perform_await_ui_element(&inspector, &waiter, &*clock, &element_request(None))
            .unwrap_err();

        assert!(matches!(err, PolarizeError::AppNotFound(_)));
        assert_eq!(inspector.call_count(), 1);
    }

    #[test]
    fn await_ui_element_reports_a_waiter_failure() {
        let (clock, waiter) = setup(WaiterMode::Fails);
        let inspector = FakeInspector::new(vec![empty_window()]);

        let err = perform_await_ui_element(&inspector, &waiter, &*clock, &element_request(None))
            .unwrap_err();

        assert!(err.to_string().contains("observer failed"));
    }

    // ---- await_screen_idle -----------------------------------------

    fn idle_request(idle_ms: Option<u64>, timeout_ms: Option<u64>) -> AwaitScreenIdleRequest {
        AwaitScreenIdleRequest {
            app: None,
            idle_ms,
            timeout_ms,
            poll_interval_ms: None,
        }
    }

    fn changing_tree(label: &str) -> AxNode {
        AxNode {
            label: Some(label.to_string()),
            ..empty_window()
        }
    }

    #[test]
    fn await_screen_idle_succeeds_once_the_tree_holds_still() {
        let (clock, waiter) = setup(WaiterMode::NeverSignals);
        let inspector = FakeInspector::new(vec![empty_window()]);

        let response =
            perform_await_screen_idle(&inspector, &waiter, &*clock, &idle_request(None, None))
                .unwrap();

        assert_eq!(response.idle_ms, DEFAULT_IDLE_MS);
        assert_eq!(response.waited_ms, DEFAULT_IDLE_MS);
        assert_eq!(response.polls, 3, "one read, then two 250 ms wait slices");
        assert_eq!(response.app_name, "TextEdit");
    }

    #[test]
    fn await_screen_idle_restarts_the_idle_window_on_every_change() {
        let (clock, waiter) = setup(WaiterMode::NeverSignals);
        let inspector = FakeInspector::new(vec![
            changing_tree("a"),
            changing_tree("a"),
            changing_tree("b"),
            changing_tree("b"),
        ]);

        let response =
            perform_await_screen_idle(&inspector, &waiter, &*clock, &idle_request(None, None))
                .unwrap();

        // Without the change at 500 ms the wait would have ended there.
        assert_eq!(response.waited_ms, 1_000);
        assert_eq!(response.polls, 5);
    }

    #[test]
    fn await_screen_idle_times_out_when_the_tree_never_holds_still() {
        let (clock, waiter) = setup(WaiterMode::NeverSignals);
        let inspector = FakeInspector::new(vec![
            changing_tree("a"),
            changing_tree("b"),
            changing_tree("c"),
            changing_tree("d"),
            changing_tree("e"),
        ]);

        let err = perform_await_screen_idle(
            &inspector,
            &waiter,
            &*clock,
            &idle_request(Some(400), Some(1_000)),
        )
        .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("timed out after 1000 ms"), "{message}");
        assert!(message.contains("400 ms"), "{message}");
    }

    #[test]
    fn await_screen_idle_with_a_zero_timeout_still_reads_the_tree_once() {
        let (clock, waiter) = setup(WaiterMode::NeverSignals);
        let inspector = FakeInspector::new(vec![empty_window()]);

        let err =
            perform_await_screen_idle(&inspector, &waiter, &*clock, &idle_request(None, Some(0)))
                .unwrap_err();

        assert!(err.to_string().contains("1 tree check"));
        assert_eq!(inspector.call_count(), 1);
        assert!(waiter.budgets().is_empty());
    }

    #[test]
    fn await_screen_idle_with_a_zero_idle_window_succeeds_on_the_first_read() {
        let (clock, waiter) = setup(WaiterMode::NeverSignals);
        let inspector = FakeInspector::new(vec![empty_window()]);

        let response =
            perform_await_screen_idle(&inspector, &waiter, &*clock, &idle_request(Some(0), None))
                .unwrap();

        assert_eq!(response.polls, 1);
        assert_eq!(response.waited_ms, 0);
    }

    #[test]
    fn await_screen_idle_bounds_each_wait_by_the_time_left() {
        let (clock, waiter) = setup(WaiterMode::NeverSignals);
        let inspector = FakeInspector::new(vec![
            changing_tree("a"),
            changing_tree("b"),
            changing_tree("c"),
            changing_tree("d"),
        ]);

        let _ = perform_await_screen_idle(
            &inspector,
            &waiter,
            &*clock,
            &idle_request(Some(400), Some(600)),
        );

        assert_eq!(waiter.budgets(), vec![250, 250, 100]);
    }

    #[test]
    fn await_screen_idle_passes_the_app_to_the_inspector_and_the_waiter() {
        let (clock, waiter) = setup(WaiterMode::NeverSignals);
        let inspector = FakeInspector::new(vec![empty_window()]);
        let app = AppIdentifier {
            bundle_id: None,
            app_name: Some("Safari".to_string()),
        };
        let request = AwaitScreenIdleRequest {
            app: Some(app.clone()),
            ..idle_request(None, None)
        };

        perform_await_screen_idle(&inspector, &waiter, &*clock, &request).unwrap();

        assert!(
            inspector
                .seen_apps
                .borrow()
                .iter()
                .all(|seen| seen.as_ref() == Some(&app))
        );
        assert!(
            waiter
                .seen_apps
                .borrow()
                .iter()
                .all(|seen| seen.as_ref() == Some(&app))
        );
    }

    #[test]
    fn await_screen_idle_reports_an_inspector_failure() {
        let (clock, waiter) = setup(WaiterMode::NeverSignals);
        let inspector = FakeInspector::failing_on_call(vec![empty_window()], 2);

        let err =
            perform_await_screen_idle(&inspector, &waiter, &*clock, &idle_request(None, None))
                .unwrap_err();

        assert!(matches!(err, PolarizeError::AppNotFound(_)));
    }

    // ---- wire contract ---------------------------------------------

    #[test]
    fn an_await_ui_element_request_needs_only_a_selector() {
        let request: AwaitUiElementRequest =
            serde_json::from_str(r#"{"selector":{"role":"AXButton"}}"#).unwrap();
        assert_eq!(request.app, None);
        assert_eq!(request.timeout_ms, None);
        assert_eq!(request.poll_interval_ms, None);
    }

    #[test]
    fn an_await_screen_idle_request_needs_no_field_at_all() {
        let request: AwaitScreenIdleRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(request, AwaitScreenIdleRequest::default());
    }

    #[test]
    fn the_responses_round_trip_through_json() {
        let element = AwaitUiElementResponse {
            app_name: "TextEdit".to_string(),
            path: vec![0, 2],
            node: window_with_save_button(),
            waited_ms: 250,
            polls: 2,
        };
        let json = serde_json::to_string(&element).unwrap();
        assert_eq!(
            serde_json::from_str::<AwaitUiElementResponse>(&json).unwrap(),
            element
        );

        let idle = AwaitScreenIdleResponse {
            app_name: "Safari".to_string(),
            idle_ms: 500,
            waited_ms: 750,
            polls: 4,
        };
        let json = serde_json::to_string(&idle).unwrap();
        assert_eq!(
            serde_json::from_str::<AwaitScreenIdleResponse>(&json).unwrap(),
            idle
        );
    }

    #[test]
    fn a_timeout_becomes_a_polarize_error_that_keeps_its_message() {
        let err = WaitError::ElementTimeout {
            selector: "identifier=\"save\"".to_string(),
            waited_ms: 5_000,
            polls: 21,
        };
        let message = err.to_string();
        let converted: PolarizeError = err.into();
        assert!(converted.to_string().contains(&message));
    }

    #[test]
    fn the_system_clock_starts_at_zero_and_never_goes_backwards() {
        let clock = SystemClock::new();
        let first = clock.now_ms();
        let second = clock.now_ms();
        assert!(first <= second);
        assert!(second < 1_000, "a clock read must not take a second");
    }
}
