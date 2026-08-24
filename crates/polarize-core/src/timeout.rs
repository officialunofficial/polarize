//! Bounds a blocking native call that has no timeout of its own.
//!
//! `polarize-macos`'s `ScreenCaptureKit` calls block on a `Condvar` with
//! no timeout, waiting for a completion callback that can go unanswered
//! — e.g. under stale Screen Recording TCC state (see issue #50). This
//! module gives those callers a bounded wait instead of an indefinite
//! hang. It has no macOS dependency, so it is fully covered here.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::error::PolarizeError;

/// Runs `f` on a dedicated thread and waits at most `duration` for it to
/// finish, returning `f`'s own result when it does in time.
///
/// `f` keeps running on its own thread even after a timeout — there is
/// no way to cancel a blocked native call from the outside. This only
/// bounds how long a caller waits for one, so a stalled native call
/// surfaces a real, timely error instead of hanging the whole request
/// forever.
pub fn with_timeout<T, F>(duration: Duration, f: F) -> Result<T, PolarizeError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, PolarizeError> + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        // `send` failing just means the caller already timed out and
        // stopped listening — not a bug here.
        let _ = tx.send(f());
    });
    match rx.recv_timeout(duration) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(PolarizeError::Platform(format!(
            "operation timed out after {duration:?} with no response"
        ))),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(PolarizeError::Platform(
            "operation's worker thread ended without sending a result".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
