//! Bounds a blocking native call that has no timeout of its own.
//!
//! `polarize-macos`'s `ScreenCaptureKit` calls block on a `Condvar` with
//! no timeout, waiting for a completion callback that can go unanswered
//! — e.g. under stale Screen Recording TCC state (see issue #50). This
//! module gives those callers a bounded wait instead of an indefinite
//! hang. It has no macOS dependency, so it is fully covered here.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::error::PolarizeError;

/// Caps how many [`with_timeout`] calls may have an abandoned worker
/// thread outstanding at once.
///
/// A stalled native call leaks its worker thread — see this module's
/// own doc, and [`with_timeout`]'s. Nothing can stop that thread from
/// the outside. Left unchecked, a caller retrying a stuck operation in
/// a loop would leak one more thread per attempt, without bound. This
/// budget turns that into a circuit breaker: past its cap, a new call
/// refuses to spawn another thread. It fails at once, instead of
/// piling on.
struct WorkerBudget {
    outstanding: AtomicUsize,
    cap: usize,
}

impl WorkerBudget {
    const fn new(cap: usize) -> Self {
        Self {
            outstanding: AtomicUsize::new(0),
            cap,
        }
    }

    /// Reserves one slot, unless the cap is already reached.
    ///
    /// `true` means a slot reserved. The caller must call
    /// [`Self::release`] exactly once, whenever — if ever — the work it
    /// started finishes. `false` means the cap is full. The caller
    /// reserved nothing, and must not call [`Self::release`].
    fn try_reserve(&self) -> bool {
        let mut current = self.outstanding.load(Ordering::SeqCst);
        loop {
            if current >= self.cap {
                return false;
            }
            match self.outstanding.compare_exchange_weak(
                current,
                current + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return true,
                Err(observed) => current = observed,
            }
        }
    }

    /// Frees one slot [`Self::try_reserve`] reserved.
    fn release(&self) {
        self.outstanding.fetch_sub(1, Ordering::SeqCst);
    }
}

/// How many [`with_timeout`] calls may have an abandoned worker thread
/// outstanding at once, process-wide, before a new call refuses to
/// spawn another. Eight stuck calls already means something is
/// systematically wrong. A ninth would not learn anything new. It
/// would only leak one more thread.
const MAX_OUTSTANDING_WORKERS: usize = 8;

static WORKER_BUDGET: WorkerBudget = WorkerBudget::new(MAX_OUTSTANDING_WORKERS);

/// Runs `f` on a dedicated thread and waits at most `duration` for it to
/// finish, returning `f`'s own result when it does in time.
///
/// `f` keeps running on its own thread even after a timeout — there is
/// no way to cancel a blocked native call from the outside. This only
/// bounds how long a caller waits for one, so a stalled native call
/// surfaces a real, timely error instead of hanging the whole request
/// forever.
///
/// A call past [`MAX_OUTSTANDING_WORKERS`] refuses to spawn a new
/// thread at all. It fails immediately instead, with its own
/// [`PolarizeError::Platform`]. See [`WorkerBudget`]'s own doc for why.
pub fn with_timeout<T, F>(duration: Duration, f: F) -> Result<T, PolarizeError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, PolarizeError> + Send + 'static,
{
    run_with_budget(&WORKER_BUDGET, duration, f)
}

/// [`with_timeout`]'s real logic, against any `'static` [`WorkerBudget`]
/// — not just the process-wide [`WORKER_BUDGET`]. Split out so a test
/// can exercise the cap-refusal behavior against its own, small,
/// isolated budget, instead of fighting every other test in this
/// module over the one shared global.
fn run_with_budget<T, F>(
    budget: &'static WorkerBudget,
    duration: Duration,
    f: F,
) -> Result<T, PolarizeError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, PolarizeError> + Send + 'static,
{
    if !budget.try_reserve() {
        return Err(PolarizeError::Platform(format!(
            "refused to start: {} operations are already stuck past their own timeout",
            budget.cap
        )));
    }

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = f();
        // The thread has finished. It is no longer stuck, whether or
        // not anyone is still listening for its result.
        budget.release();
        // `send` failing just means the caller already timed out and
        // stopped listening — not a bug here.
        let _ = tx.send(result);
    });
    match rx.recv_timeout(duration) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(PolarizeError::Platform(format!(
            "operation timed out after {duration:?} with no response"
        ))),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            // The worker thread panicked before it could release its
            // own slot. It no longer exists to do so itself.
            budget.release();
            Err(PolarizeError::Platform(
                "operation's worker thread ended without sending a result".to_string(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_budget_reserves_up_to_its_cap() {
        let budget = WorkerBudget::new(2);

        assert!(budget.try_reserve());
        assert!(budget.try_reserve());
        assert!(!budget.try_reserve());
    }

    #[test]
    fn worker_budget_frees_a_slot_on_release() {
        let budget = WorkerBudget::new(1);
        assert!(budget.try_reserve());
        assert!(!budget.try_reserve());

        budget.release();

        assert!(budget.try_reserve());
    }

    #[test]
    fn worker_budget_of_zero_reserves_nothing() {
        let budget = WorkerBudget::new(0);

        assert!(!budget.try_reserve());
    }

    #[test]
    fn with_timeout_returns_the_closures_ok_when_it_finishes_in_time() {
        let result = with_timeout(Duration::from_secs(1), || Ok::<_, PolarizeError>(42));

        assert_eq!(result.unwrap(), 42);
    }

    #[test]
    fn with_timeout_returns_the_closures_own_err_when_it_finishes_in_time() {
        let result: Result<i32, PolarizeError> = with_timeout(Duration::from_secs(1), || {
            Err(PolarizeError::Platform("boom".to_string()))
        });

        match result {
            Err(PolarizeError::Platform(message)) => assert_eq!(message, "boom"),
            other => panic!("expected the closure's own error, got {other:?}"),
        }
    }

    #[test]
    fn with_timeout_reports_a_platform_error_when_the_closure_never_returns_in_time() {
        let result: Result<i32, PolarizeError> = with_timeout(Duration::from_millis(30), || {
            std::thread::sleep(Duration::from_millis(300));
            Ok(1)
        });

        match result {
            Err(PolarizeError::Platform(message)) => {
                assert!(message.contains("timed out"), "message was {message:?}");
            }
            other => panic!("expected a timeout error, got {other:?}"),
        }
    }

    /// Its own budget, separate from [`WORKER_BUDGET`], so filling it
    /// past its cap with real leaked-past-their-timeout workers cannot
    /// starve any other test in this module — each test's `static` is
    /// its own isolated memory, even though both are `WorkerBudget`s.
    static REFUSAL_TEST_BUDGET: WorkerBudget = WorkerBudget::new(2);

    #[test]
    fn run_with_budget_refuses_a_new_call_once_its_budget_is_full() {
        // Fill the budget with calls whose own timeout is short, but
        // whose closure sleeps well past it — so each one times out
        // for its caller while its worker thread stays outstanding,
        // exactly the "abandoned" state the cap defends against.
        for _ in 0..2 {
            let result: Result<i32, PolarizeError> =
                run_with_budget(&REFUSAL_TEST_BUDGET, Duration::from_millis(20), || {
                    std::thread::sleep(Duration::from_secs(2));
                    Ok(1)
                });
            assert!(
                matches!(result, Err(PolarizeError::Platform(ref m)) if m.contains("timed out")),
                "expected each budget-filling call to time out, got {result:?}"
            );
        }

        // The budget is now full of threads still sleeping. One more
        // call must refuse outright — no new thread, no wait for
        // `duration` at all.
        let refused: Result<i32, PolarizeError> =
            run_with_budget(&REFUSAL_TEST_BUDGET, Duration::from_secs(5), || Ok(1));

        match refused {
            Err(PolarizeError::Platform(message)) => {
                assert!(message.contains("refused"), "message was {message:?}");
            }
            other => panic!("expected the call past the cap to be refused, got {other:?}"),
        }
    }
}
