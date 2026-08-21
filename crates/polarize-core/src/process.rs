//! Running one child process under a deadline.
//!
//! This module holds no macOS API at all — it is `std::process` and
//! `std::thread`. It lives in `polarize-core` because that makes it
//! testable: `polarize-macos`'s code cannot run in CI, and the failure
//! this module exists to prevent is a *hang*, which only a real
//! subprocess can demonstrate.
//!
//! `polarize_macos::applescript` runs `osascript` and `sdef` through it.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// How often a wait re-checks the child and the pipe readers.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// How long [`run_with_deadline`] waits for the pipe readers after the
/// child is gone. See PINV-25 for why this bound has to exist.
pub const DEFAULT_READER_GRACE: Duration = Duration::from_secs(2);

/// What one finished (or killed) child produced.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommandOutcome {
    pub stdout: String,
    pub stderr: String,
    /// `None` when a signal killed the process, as it does on timeout.
    pub exit_code: Option<i32>,
    /// `true` when the deadline killed the child.
    pub timed_out: bool,
    /// `true` when a pipe reader was still blocked when the grace
    /// expired, so the captured output may be short. See PINV-25.
    pub output_truncated: bool,
}

/// Runs `program` under a deadline, capturing stdout and stderr.
///
/// Uses [`DEFAULT_READER_GRACE`]. See [`run_with_deadline`] for the
/// rules, and for a caller that needs a different grace.
pub fn run(
    program: &str,
    args: &[&str],
    stdin_data: Option<&str>,
    timeout: Duration,
) -> Result<CommandOutcome, String> {
    run_with_deadline(program, args, stdin_data, timeout, DEFAULT_READER_GRACE)
}

/// # PINV-25: a command's deadline bounds the call, not just the child
///
/// - Always: [`run_with_deadline`] returns within roughly
///   `timeout + reader_grace`. It kills the child at `timeout`. It then
///   waits at most `reader_grace` for the stdout and stderr readers, and
///   takes whatever they collected either way, setting
///   [`CommandOutcome::output_truncated`] when it gave up on one.
/// - Because: killing a child does not close its pipes. Any process
///   holding the write end keeps them open, and `read_to_end` returns
///   only when the last writer closes. `osascript` is exactly the case:
///   the scripts it runs start helpers, and a target app can inherit the
///   descriptor. Joining a reader thread unconditionally would then
///   block past the deadline the caller set, with no bound at all. The
///   readers write into a shared buffer rather than returning one, so
///   the partial output survives a reader this function abandons.
/// - If violated: `run_applescript` hangs long past its two-minute
///   clamp, pinning the `tokio` blocking thread that
///   `apps/polarize/src/server.rs` put it on, and the caller sees no
///   error at all — only a tool call that never returns.
pub fn run_with_deadline(
    program: &str,
    args: &[&str],
    stdin_data: Option<&str>,
    timeout: Duration,
    reader_grace: Duration,
) -> Result<CommandOutcome, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(if stdin_data.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot start {program}: {error}"))?;

    if let Some(data) = stdin_data {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| format!("{program} has no stdin pipe"))?;
        let owned = data.to_string();
        // Dropping `stdin` at the end of the closure closes the pipe,
        // which is what tells the child its input is complete. A write
        // to a child that already exited fails, and that failure is not
        // interesting: the exit status reports it.
        thread::spawn(move || {
            let _ = stdin.write_all(owned.as_bytes());
        });
    }

    let stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| format!("{program} has no stdout pipe"))?;
    let stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| format!("{program} has no stderr pipe"))?;
    let (stdout_buffer, stdout_reader) = spawn_reader(stdout_pipe);
    let (stderr_buffer, stderr_reader) = spawn_reader(stderr_pipe);

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break None;
                }
                thread::sleep(POLL_INTERVAL);
            }
            Err(error) => {
                let _ = child.kill();
                return Err(format!("cannot wait for {program}: {error}"));
            }
        }
    };

    // The child is gone, but a process it started may still hold the
    // write end of these pipes. Wait, but only so long. See PINV-25.
    let readers_deadline = Instant::now() + reader_grace;
    while !(stdout_reader.is_finished() && stderr_reader.is_finished()) {
        if Instant::now() >= readers_deadline {
            break;
        }
        thread::sleep(POLL_INTERVAL);
    }
    let output_truncated = !stdout_reader.is_finished() || !stderr_reader.is_finished();

    Ok(CommandOutcome {
        stdout: take_utf8(&stdout_buffer),
        stderr: take_utf8(&stderr_buffer),
        exit_code: status.and_then(|status| status.code()),
        timed_out,
        output_truncated,
    })
}

/// Reads `pipe` into a buffer the caller can read at any time.
///
/// The buffer is shared, rather than returned from the thread, so a
/// caller that stops waiting still gets what already arrived. See
/// PINV-25.
fn spawn_reader<R>(mut pipe: R) -> (Arc<Mutex<Vec<u8>>>, thread::JoinHandle<()>)
where
    R: Read + Send + 'static,
{
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let writer = Arc::clone(&buffer);
    let handle = thread::spawn(move || {
        let mut chunk = [0_u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => match writer.lock() {
                    Ok(mut buffer) => buffer.extend_from_slice(&chunk[..read]),
                    Err(_) => break,
                },
            }
        }
    });
    (buffer, handle)
}

/// Reads a shared buffer out as text.
///
/// A poisoned lock still yields its data: the only writer is a reader
/// thread, and a panic there does not corrupt a `Vec<u8>` of bytes
/// already appended.
fn take_utf8(buffer: &Arc<Mutex<Vec<u8>>>) -> String {
    let bytes = buffer.lock().unwrap_or_else(|err| err.into_inner());
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LONG: Duration = Duration::from_secs(30);

    #[test]
    fn a_command_that_exits_reports_its_output_and_code() {
        let outcome = run("sh", &["-c", "echo out; echo err >&2; exit 3"], None, LONG).unwrap();
        assert_eq!(outcome.stdout.trim(), "out");
        assert_eq!(outcome.stderr.trim(), "err");
        assert_eq!(outcome.exit_code, Some(3));
        assert!(!outcome.timed_out);
        assert!(!outcome.output_truncated);
    }

    #[test]
    fn stdin_reaches_the_child() {
        let outcome = run("cat", &[], Some("hello stdin"), LONG).unwrap();
        assert_eq!(outcome.stdout, "hello stdin");
        assert_eq!(outcome.exit_code, Some(0));
    }

    #[test]
    fn a_child_that_outlives_its_deadline_is_killed() {
        let started = Instant::now();
        let outcome = run("sh", &["-c", "sleep 30"], None, Duration::from_millis(200)).unwrap();
        assert!(outcome.timed_out);
        assert_eq!(outcome.exit_code, None);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_grandchild_holding_the_pipe_cannot_outlast_the_reader_grace() {
        // PINV-25. Killing `sh` does not close stdout: the backgrounded
        // subshell inherited the write end and holds it for 30 seconds.
        // `read_to_end` on that pipe would block for all 30. The grace
        // is what bounds the call.
        let started = Instant::now();
        let outcome = run_with_deadline(
            "sh",
            &["-c", "( sleep 30 ) & sleep 30"],
            None,
            Duration::from_millis(200),
            Duration::from_millis(300),
        )
        .unwrap();

        assert!(outcome.timed_out);
        assert!(outcome.output_truncated, "the reader was abandoned");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "run_with_deadline hung for {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn output_written_before_a_timeout_survives_the_kill() {
        // The reader writes into a shared buffer, so abandoning it does
        // not throw away what already arrived. See PINV-25.
        let outcome = run_with_deadline(
            "sh",
            &["-c", "echo early; ( sleep 30 ) & sleep 30"],
            None,
            Duration::from_millis(300),
            Duration::from_millis(300),
        )
        .unwrap();

        assert_eq!(outcome.stdout.trim(), "early");
        assert!(outcome.timed_out);
        assert!(outcome.output_truncated);
    }

    #[test]
    fn a_missing_program_is_an_error_not_a_panic() {
        let err = run("polarize-no-such-program", &[], None, LONG).unwrap_err();
        assert!(err.contains("cannot start"), "{err}");
    }

    #[test]
    fn large_output_is_captured_whole() {
        let outcome = run("sh", &["-c", "yes abcdefgh | head -c 900000"], None, LONG).unwrap();
        assert_eq!(outcome.stdout.len(), 900_000);
        assert!(!outcome.output_truncated);
    }
}
