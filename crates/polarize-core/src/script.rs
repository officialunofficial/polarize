//! AppleScript execution logic: request/response schemas, the
//! `osascript` error mapping, the `sdef` verb scan, and the
//! orchestration in front of [`crate::traits::AppleScriptRunner`].
//!
//! Everything in this module is pure. `polarize-macos` runs the real
//! `osascript` and `sdef` subprocesses; this module decides what to run,
//! how long to allow, and what a failure means.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::PolarizeError;
use crate::permission::{PermissionError, PermissionKind, PermissionState};
use crate::schema::AppIdentifier;
use crate::traits::AppleScriptRunner;

pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;
pub const MIN_TIMEOUT_MS: u64 = 100;
pub const MAX_TIMEOUT_MS: u64 = 120_000;
pub const MAX_OUTPUT_CHARS: usize = 64_000;
pub const MAX_SOURCE_CHARS_IN_ERROR: usize = 48;

pub const ERR_NOT_AUTHORIZED: i32 = -1743;
pub const ERR_NEEDS_CONSENT: i32 = -1744;
pub const ERR_APP_NOT_RUNNING: i32 = -600;
pub const ERR_NO_SUCH_OBJECT: i32 = -1728;
pub const ERR_USER_CANCELLED: i32 = -128;

/// Returns the run timeout to use, in milliseconds.
///
/// An absent timeout becomes [`DEFAULT_TIMEOUT_MS`]. A requested
/// timeout is clamped to [`MIN_TIMEOUT_MS`]..=[`MAX_TIMEOUT_MS`]. The
/// clamp is deliberate: `osascript` can block forever on a modal
/// dialog, and `polarize` speaks MCP over stdio, so one blocked child
/// process would stall every later tool call.
pub fn clamp_timeout_ms(requested: Option<u64>) -> u64 {
    match requested {
        Some(ms) => ms.clamp(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS),
        None => DEFAULT_TIMEOUT_MS,
    }
}

/// Cuts `text` to `max_chars` characters. The second value says whether
/// the cut happened. The cut lands on a character boundary, so the
/// result is always valid UTF-8.
fn truncate_chars(text: &str, max_chars: usize) -> (String, bool) {
    if text.chars().count() <= max_chars {
        (text.to_string(), false)
    } else {
        (text.chars().take(max_chars).collect(), true)
    }
}

/// Cuts script output to [`MAX_OUTPUT_CHARS`].
///
/// A script can print megabytes. The whole output travels inside one
/// MCP response, so `polarize` caps it and tells the caller it did.
pub fn truncate_output(text: &str) -> (String, bool) {
    truncate_chars(text, MAX_OUTPUT_CHARS)
}

/// # PINV-22: an error never carries the script source as written
///
/// - Always: every error `polarize` builds from a `run_applescript`
///   request passes the source through [`redact_source`] first. That
///   function removes the text inside every double-quoted AppleScript
///   literal, flattens the result to one line, and cuts it to
///   [`MAX_SOURCE_CHARS_IN_ERROR`] characters.
/// - Because: a script often carries a secret. `tell application "Mail"
///   to set pw to "hunter2"` is a normal thing for a caller to send.
///   Error strings travel further than the caller expects: into MCP
///   client logs, into transcripts, and into bug reports. An
///   unterminated literal is treated as open to the end of the source,
///   so a malformed script fails closed.
/// - If violated: a password or token that a caller sent once now sits
///   in a log file, and nobody knows it is there.
pub fn redact_source(source: &str) -> String {
    let mut flat = String::new();
    let mut chars = source.chars();
    let mut in_literal = false;
    while let Some(character) = chars.next() {
        match character {
            '"' => {
                flat.push('"');
                if in_literal {
                    in_literal = false;
                } else {
                    flat.push('…');
                    in_literal = true;
                }
            }
            // Skip the character an escape protects, so an escaped
            // quote does not look like the end of the literal.
            '\\' if in_literal => {
                let _ = chars.next();
            }
            _ if in_literal => {}
            '\n' | '\r' | '\t' => flat.push(' '),
            other => flat.push(other),
        }
    }
    let (short, cut) = truncate_chars(&flat, MAX_SOURCE_CHARS_IN_ERROR);
    if cut { format!("{short}…") } else { short }
}

/// Puts `source` inside a `tell application "…"` block.
///
/// This is how a `run_applescript` call aims at a named app. The app
/// name goes into an AppleScript string literal, so a backslash and a
/// double quote are both escaped.
pub fn wrap_in_tell(source: &str, target_app: &str) -> String {
    let escaped = target_app.replace('\\', "\\\\").replace('"', "\\\"");
    format!("tell application \"{escaped}\"\n{source}\nend tell")
}

/// One classified `osascript` failure. See PINV-21.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptFailure {
    /// The user has not granted Automation permission for this target.
    AutomationNotPermitted {
        state: PermissionState,
        message: String,
    },
    /// The target application is not running.
    AppNotRunning { message: String },
    /// The script asked for an object the app does not have.
    ObjectNotFound { message: String },
    /// The user cancelled a dialog the script opened.
    Cancelled { message: String },
    /// Any other script failure, with its code when `osascript` printed
    /// one.
    Script { code: Option<i32>, message: String },
    /// The runner killed the script at its deadline.
    Timeout {
        timeout_ms: u64,
        /// A redacted excerpt of the source; see PINV-22.
        source_excerpt: String,
    },
}

/// Reads the error code `osascript` prints at the end of its last
/// stderr line, as in `... (-1743)`.
///
/// Returns `None` when the last line does not end with a parenthesized
/// integer.
fn last_line_code(message: &str) -> Option<i32> {
    let line = message.lines().next_back()?.trim_end();
    let inner = line.strip_suffix(')')?;
    let (_, code) = inner.rsplit_once('(')?;
    code.trim().parse().ok()
}

/// # PINV-21: an AppleScript failure keeps its cause
///
/// - Always: [`parse_osascript_error`] classifies `osascript` stderr by
///   the error code it prints, and [`script_failure_to_error`] maps each
///   class to a matching [`PolarizeError`]. Code `-1743` and code
///   `-1744` become [`PolarizeError::Permission`] with
///   [`PermissionKind::Automation`], never a plain platform error. Code
///   `-600` becomes [`PolarizeError::AppNotFound`].
///   [`automation_check_from_status`] maps the same two codes the same
///   way when they come from the native
///   `AEDeterminePermissionToAutomateTarget` preflight.
/// - Because: AppleScript reports "you have no Automation permission"
///   and "your script has a bug" through the same channel — a line of
///   text on stderr. A caller that cannot tell them apart retries a
///   script forever against a permission it needs a human to grant in
///   System Settings. The two codes differ in one way that matters:
///   `-1743` means the user said no, and `-1744` means the user has not
///   been asked yet.
/// - If violated: a missing Automation grant looks like a broken
///   script, and the caller never learns which app needs approval.
pub fn parse_osascript_error(stderr: &str) -> ScriptFailure {
    let trimmed = stderr.trim();
    let message = if trimmed.is_empty() {
        "osascript failed and printed no message".to_string()
    } else {
        trimmed.to_string()
    };
    match last_line_code(&message) {
        Some(ERR_NOT_AUTHORIZED) => ScriptFailure::AutomationNotPermitted {
            state: PermissionState::Denied,
            message,
        },
        Some(ERR_NEEDS_CONSENT) => ScriptFailure::AutomationNotPermitted {
            state: PermissionState::NotDetermined,
            message,
        },
        Some(ERR_APP_NOT_RUNNING) => ScriptFailure::AppNotRunning { message },
        Some(ERR_NO_SUCH_OBJECT) => ScriptFailure::ObjectNotFound { message },
        Some(ERR_USER_CANCELLED) => ScriptFailure::Cancelled { message },
        code => ScriptFailure::Script { code, message },
    }
}

/// Turns a classified failure into the error `polarize` reports. See
/// PINV-21.
pub fn script_failure_to_error(failure: ScriptFailure) -> PolarizeError {
    match failure {
        ScriptFailure::AutomationNotPermitted { state, .. } => {
            PolarizeError::Permission(PermissionError::NotGranted {
                kind: PermissionKind::Automation,
                state,
            })
        }
        ScriptFailure::AppNotRunning { message } => PolarizeError::AppNotFound(message),
        ScriptFailure::ObjectNotFound { message } => {
            PolarizeError::Platform(format!("AppleScript found no such object: {message}"))
        }
        ScriptFailure::Cancelled { message } => {
            PolarizeError::Platform(format!("AppleScript was cancelled: {message}"))
        }
        ScriptFailure::Script {
            code: Some(code),
            message,
        } => PolarizeError::Platform(format!("AppleScript error ({code}): {message}")),
        ScriptFailure::Script {
            code: None,
            message,
        } => PolarizeError::Platform(format!("AppleScript error: {message}")),
        ScriptFailure::Timeout {
            timeout_ms,
            source_excerpt,
        } => PolarizeError::Platform(format!(
            "AppleScript timed out after {timeout_ms} ms: {source_excerpt}"
        )),
    }
}

/// What the native Automation preflight decided. See PINV-21.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomationCheck {
    /// The caller may send Apple Events to this target.
    Permitted,
    /// The caller may not. The state says whether the user refused, or
    /// has not been asked.
    Refused(PermissionState),
    /// The preflight answered nothing useful. The caller runs the
    /// script anyway and lets the `osascript` error mapping decide.
    Inconclusive,
}

/// Maps an `AEDeterminePermissionToAutomateTarget` status to a
/// decision.
///
/// Only the two documented refusal codes block a run. Every other
/// status is [`AutomationCheck::Inconclusive`], including
/// `procNotFound` (`-600`), which only means the target app is not
/// running yet — AppleScript can launch it. See PINV-21.
pub fn automation_check_from_status(status: i32) -> AutomationCheck {
    match status {
        0 => AutomationCheck::Permitted,
        ERR_NOT_AUTHORIZED => AutomationCheck::Refused(PermissionState::Denied),
        ERR_NEEDS_CONSENT => AutomationCheck::Refused(PermissionState::NotDetermined),
        _ => AutomationCheck::Inconclusive,
    }
}

/// One named element from an app's scripting dictionary: a command
/// (a verb) or a class (a noun).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SdefEntry {
    pub name: String,
    pub description: Option<String>,
}

/// Decodes the five predefined XML entities.
fn decode_entities(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Reads one attribute value out of the text inside a start tag.
///
/// The attribute name must start the text or follow whitespace, so
/// `key="name"` does not match inside `class-name="…"`.
fn attribute_value(attributes: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=\"");
    let mut rest = attributes;
    loop {
        let position = rest.find(&needle)?;
        let value_start = position + needle.len();
        let stands_alone = position == 0 || rest[..position].ends_with(|c: char| c.is_whitespace());
        if stands_alone {
            let end = rest[value_start..].find('"')?;
            return Some(decode_entities(&rest[value_start..value_start + end]));
        }
        rest = &rest[value_start..];
    }
}

/// Scans `sdef` XML for every `<element …>` start tag and returns its
/// `name` and `description` attributes.
///
/// This is a shallow text scan, not an XML parse, and `polarize` adds
/// no XML dependency for it. The scan misses, or gets wrong:
///
/// - an element inside an XML comment or a CDATA section, which it
///   still reports;
/// - an attribute written with single quotes, which XML allows;
/// - an `xi:include` reference to another file, which `sdef` normally
///   resolves before it prints;
/// - any structure, such as which suite a command belongs to.
///
/// The result is a flat, de-duplicated list, in the order the names
/// first appear. A dictionary that lists one command in several suites
/// reports it once.
pub fn scan_sdef_elements(xml: &str, element: &str) -> Vec<SdefEntry> {
    let open = format!("<{element}");
    let mut entries: Vec<SdefEntry> = Vec::new();
    let mut rest = xml;
    while let Some(position) = rest.find(&open) {
        let after = &rest[position + open.len()..];
        let mut next = after;
        let name_ends = after.starts_with(|c: char| c.is_whitespace() || c == '>' || c == '/');
        if name_ends && let Some(end) = after.find('>') {
            let attributes = &after[..end];
            if let Some(name) = attribute_value(attributes, "name")
                && !entries.iter().any(|entry| entry.name == name)
            {
                entries.push(SdefEntry {
                    description: attribute_value(attributes, "description"),
                    name,
                });
            }
            next = &after[end + 1..];
        }
        rest = next;
    }
    entries
}

/// Scans `sdef` XML for its commands and its classes. See
/// [`scan_sdef_elements`] for what this shallow scan misses.
pub fn scan_sdef(xml: &str) -> (Vec<SdefEntry>, Vec<SdefEntry>) {
    (
        scan_sdef_elements(xml, "command"),
        scan_sdef_elements(xml, "class"),
    )
}

/// A `run_applescript` call.
///
/// `target_app` names the app the script talks to.
/// [`perform_run_applescript`] wraps `source` in a `tell application`
/// block for it, and `polarize-macos` preflights Automation permission
/// for it. A blank `target_app` counts as none: the script then runs as
/// written, and may name its own targets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunAppleScriptRequest {
    pub source: String,
    pub target_app: Option<String>,
    /// Milliseconds to allow. Clamped by [`clamp_timeout_ms`].
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunAppleScriptResponse {
    /// What the script printed, with trailing whitespace removed.
    pub output: String,
    /// `true` when `output` hit [`MAX_OUTPUT_CHARS`].
    pub truncated: bool,
    /// The timeout the run actually used, after the clamp.
    pub timeout_ms: u64,
}

/// A `script_dictionary` call: which app's scripting dictionary to
/// read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ScriptDictionaryRequest {
    pub app: AppIdentifier,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ScriptDictionaryResponse {
    pub app_name: String,
    /// The verbs the app publishes.
    pub commands: Vec<SdefEntry>,
    /// The nouns the app publishes.
    pub classes: Vec<SdefEntry>,
}

/// Runs one AppleScript through `runner` and shapes the result.
///
/// The steps are: clamp the timeout, wrap the source for the target
/// app, run it, then classify what came back. A run that timed out, or
/// that exited non-zero, becomes a structured error (PINV-21). No error
/// carries the source as written (PINV-22).
pub fn perform_run_applescript<R>(
    runner: &R,
    request: &RunAppleScriptRequest,
) -> Result<RunAppleScriptResponse, PolarizeError>
where
    R: AppleScriptRunner,
{
    let timeout_ms = clamp_timeout_ms(request.timeout_ms);
    let target_app = request
        .target_app
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty());
    let source = match target_app {
        Some(name) => wrap_in_tell(&request.source, name),
        None => request.source.clone(),
    };

    let outcome = runner.run_script(&source, target_app, timeout_ms)?;
    if outcome.timed_out {
        return Err(script_failure_to_error(ScriptFailure::Timeout {
            timeout_ms,
            source_excerpt: redact_source(&request.source),
        }));
    }
    if outcome.exit_code != Some(0) {
        return Err(script_failure_to_error(parse_osascript_error(
            &outcome.stderr,
        )));
    }

    let (output, truncated) = truncate_output(outcome.stdout.trim_end());
    Ok(RunAppleScriptResponse {
        output,
        truncated,
        timeout_ms,
    })
}

/// Reads one app's scripting dictionary through `runner` and lists its
/// verbs and nouns. See [`scan_sdef_elements`] for the limits of the
/// scan.
pub fn perform_script_dictionary<R>(
    runner: &R,
    request: &ScriptDictionaryRequest,
) -> Result<ScriptDictionaryResponse, PolarizeError>
where
    R: AppleScriptRunner,
{
    let sdef = runner.app_sdef(&request.app)?;
    let (commands, classes) = scan_sdef(&sdef.xml);
    Ok(ScriptDictionaryResponse {
        app_name: sdef.app_name,
        commands,
        classes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::{AppSdef, ScriptOutcome};
    use std::cell::RefCell;

    const SDEF_FRAGMENT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<dictionary title="Finder Terminology">
  <suite name="Standard Suite" code="????" description="Common commands.">
    <command name="open" code="aevtodoc" description="Open the specified object(s)">
      <direct-parameter type="specifier"/>
    </command>
    <command name="close" code="coreclos" description="Close an object">
    </command>
    <command name="count" code="corecnt"/>
    <class name="window" code="cwin" description="A window">
      <property name="name" code="pnam" type="text"/>
    </class>
    <class name="item" code="cobj" description="An item &amp; its info"/>
  </suite>
  <suite name="Finder Basics" code="fndr">
    <command name="open" code="aevtodoc" description="Open the specified object(s)"/>
    <commandline name="not-a-command" code="xxxx"/>
  </suite>
</dictionary>"#;

    fn outcome_ok(stdout: &str) -> ScriptOutcome {
        ScriptOutcome {
            stdout: stdout.to_string(),
            stderr: String::new(),
            exit_code: Some(0),
            timed_out: false,
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct RecordedRun {
        source: String,
        target_app: Option<String>,
        timeout_ms: u64,
    }

    struct FakeRunner {
        outcome: ScriptOutcome,
        sdef: AppSdef,
        fail_with: Option<String>,
        runs: RefCell<Vec<RecordedRun>>,
        sdef_calls: RefCell<Vec<AppIdentifier>>,
    }

    impl FakeRunner {
        fn new(outcome: ScriptOutcome) -> Self {
            Self {
                outcome,
                sdef: AppSdef {
                    app_name: "Finder".to_string(),
                    xml: SDEF_FRAGMENT.to_string(),
                },
                fail_with: None,
                runs: RefCell::new(Vec::new()),
                sdef_calls: RefCell::new(Vec::new()),
            }
        }

        fn failing(message: &str) -> Self {
            let mut runner = Self::new(outcome_ok(""));
            runner.fail_with = Some(message.to_string());
            runner
        }
    }

    impl AppleScriptRunner for FakeRunner {
        fn run_script(
            &self,
            source: &str,
            target_app: Option<&str>,
            timeout_ms: u64,
        ) -> Result<ScriptOutcome, PolarizeError> {
            self.runs.borrow_mut().push(RecordedRun {
                source: source.to_string(),
                target_app: target_app.map(str::to_string),
                timeout_ms,
            });
            match &self.fail_with {
                Some(message) => Err(PolarizeError::Platform(message.clone())),
                None => Ok(self.outcome.clone()),
            }
        }

        fn app_sdef(&self, app: &AppIdentifier) -> Result<AppSdef, PolarizeError> {
            self.sdef_calls.borrow_mut().push(app.clone());
            match &self.fail_with {
                Some(message) => Err(PolarizeError::Platform(message.clone())),
                None => Ok(self.sdef.clone()),
            }
        }
    }

    // --- timeout clamp ---

    #[test]
    fn absent_timeout_becomes_the_default() {
        assert_eq!(clamp_timeout_ms(None), DEFAULT_TIMEOUT_MS);
    }

    #[test]
    fn timeout_clamps_to_the_allowed_range() {
        assert_eq!(clamp_timeout_ms(Some(0)), MIN_TIMEOUT_MS);
        assert_eq!(clamp_timeout_ms(Some(1)), MIN_TIMEOUT_MS);
        assert_eq!(clamp_timeout_ms(Some(u64::MAX)), MAX_TIMEOUT_MS);
    }

    #[test]
    fn timeout_inside_the_range_passes_through() {
        assert_eq!(clamp_timeout_ms(Some(5_000)), 5_000);
    }

    // --- output truncation ---

    #[test]
    fn short_output_is_not_truncated() {
        let (output, truncated) = truncate_output("hello");
        assert_eq!(output, "hello");
        assert!(!truncated);
    }

    #[test]
    fn long_output_is_cut_to_the_cap() {
        let text = "a".repeat(MAX_OUTPUT_CHARS + 10);
        let (output, truncated) = truncate_output(&text);
        assert_eq!(output.chars().count(), MAX_OUTPUT_CHARS);
        assert!(truncated);
    }

    #[test]
    fn truncation_keeps_whole_characters() {
        let text = "é".repeat(MAX_OUTPUT_CHARS + 10);
        let (output, truncated) = truncate_output(&text);
        assert!(truncated);
        assert_eq!(output.chars().count(), MAX_OUTPUT_CHARS);
        assert!(output.chars().all(|c| c == 'é'));
    }

    // --- source redaction (PINV-22) ---

    #[test]
    fn redaction_removes_the_text_inside_string_literals() {
        let redacted = redact_source("set pw to \"hunter2\"");
        assert!(!redacted.contains("hunter2"), "leaked: {redacted}");
        assert_eq!(redacted, "set pw to \"…\"");
    }

    #[test]
    fn redaction_handles_an_escaped_quote_inside_a_literal() {
        let redacted = redact_source("display dialog \"say \\\" hunter2\"");
        assert!(!redacted.contains("hunter2"), "leaked: {redacted}");
    }

    #[test]
    fn redaction_fails_closed_on_an_unterminated_literal() {
        let redacted = redact_source("set pw to \"hunter2");
        assert!(!redacted.contains("hunter2"), "leaked: {redacted}");
    }

    #[test]
    fn redaction_truncates_and_flattens_a_long_source() {
        let source = format!("tell application \"Finder\"\n{}\nend tell", "x".repeat(500));
        let redacted = redact_source(&source);
        assert!(redacted.chars().count() <= MAX_SOURCE_CHARS_IN_ERROR + 1);
        assert!(!redacted.contains('\n'));
        assert!(redacted.ends_with('…'));
    }

    // --- tell wrapping ---

    #[test]
    fn wrapping_puts_the_source_inside_a_tell_block() {
        assert_eq!(
            wrap_in_tell("get name of front window", "Finder"),
            "tell application \"Finder\"\nget name of front window\nend tell"
        );
    }

    #[test]
    fn wrapping_escapes_quotes_and_backslashes_in_the_app_name() {
        let wrapped = wrap_in_tell("beep", "Od\"d\\App");
        assert!(wrapped.starts_with("tell application \"Od\\\"d\\\\App\"\n"));
    }

    #[test]
    fn wrapping_keeps_a_multi_line_source_intact() {
        let wrapped = wrap_in_tell("set a to 1\nset b to 2", "Notes");
        assert!(wrapped.contains("set a to 1\nset b to 2"));
    }

    // --- osascript error mapping (PINV-21) ---

    #[test]
    fn not_authorized_maps_to_an_automation_permission_failure() {
        let stderr =
            "/dev/stdin: execution error: Not authorized to send Apple events to Finder. (-1743)\n";
        assert_eq!(
            parse_osascript_error(stderr),
            ScriptFailure::AutomationNotPermitted {
                state: PermissionState::Denied,
                message: "/dev/stdin: execution error: Not authorized to send Apple events to Finder. (-1743)".to_string(),
            }
        );
    }

    #[test]
    fn needs_consent_maps_to_a_not_determined_automation_failure() {
        let stderr = "execution error: Not authorized. (-1744)";
        assert_eq!(
            parse_osascript_error(stderr),
            ScriptFailure::AutomationNotPermitted {
                state: PermissionState::NotDetermined,
                message: "execution error: Not authorized. (-1744)".to_string(),
            }
        );
    }

    #[test]
    fn app_not_running_maps_to_its_own_failure() {
        let stderr = "execution error: Application isn’t running. (-600)";
        assert!(matches!(
            parse_osascript_error(stderr),
            ScriptFailure::AppNotRunning { .. }
        ));
    }

    #[test]
    fn missing_object_maps_to_its_own_failure() {
        let stderr = "execution error: Finder got an error: Can’t get window 9. (-1728)";
        assert!(matches!(
            parse_osascript_error(stderr),
            ScriptFailure::ObjectNotFound { .. }
        ));
    }

    #[test]
    fn user_cancel_maps_to_its_own_failure() {
        let stderr = "execution error: User canceled. (-128)";
        assert!(matches!(
            parse_osascript_error(stderr),
            ScriptFailure::Cancelled { .. }
        ));
    }

    #[test]
    fn an_unknown_code_stays_a_plain_script_failure() {
        let stderr = "/dev/stdin: syntax error: Expected end of line. (-2741)";
        assert_eq!(
            parse_osascript_error(stderr),
            ScriptFailure::Script {
                code: Some(-2741),
                message: stderr.to_string(),
            }
        );
    }

    #[test]
    fn a_number_that_is_not_an_error_code_stays_a_plain_script_failure() {
        let stderr = "execution error: something odd happened (42)";
        assert_eq!(
            parse_osascript_error(stderr),
            ScriptFailure::Script {
                code: Some(42),
                message: stderr.to_string(),
            }
        );
    }

    #[test]
    fn stderr_with_no_code_stays_a_plain_script_failure() {
        let stderr = "osascript: no such file";
        assert_eq!(
            parse_osascript_error(stderr),
            ScriptFailure::Script {
                code: None,
                message: stderr.to_string(),
            }
        );
    }

    #[test]
    fn trailing_parentheses_that_hold_no_number_are_not_a_code() {
        let stderr = "execution error: bad result (result was 3 items)";
        assert_eq!(
            parse_osascript_error(stderr),
            ScriptFailure::Script {
                code: None,
                message: stderr.to_string(),
            }
        );
    }

    #[test]
    fn empty_stderr_still_produces_a_message() {
        let failure = parse_osascript_error("   \n");
        match failure {
            ScriptFailure::Script { code, message } => {
                assert_eq!(code, None);
                assert!(!message.is_empty());
            }
            other => panic!("expected a plain script failure, got {other:?}"),
        }
    }

    #[test]
    fn the_code_comes_from_the_last_line_of_stderr() {
        let stderr = "warning: something (7)\nexecution error: nope. (-1728)";
        assert!(matches!(
            parse_osascript_error(stderr),
            ScriptFailure::ObjectNotFound { .. }
        ));
    }

    // --- failure to PolarizeError (PINV-21) ---

    #[test]
    fn an_automation_failure_becomes_a_permission_error() {
        let err = script_failure_to_error(ScriptFailure::AutomationNotPermitted {
            state: PermissionState::Denied,
            message: "nope".to_string(),
        });
        match err {
            PolarizeError::Permission(PermissionError::NotGranted { kind, state }) => {
                assert_eq!(kind, PermissionKind::Automation);
                assert_eq!(state, PermissionState::Denied);
            }
            other => panic!("expected a permission error, got {other:?}"),
        }
    }

    #[test]
    fn an_app_not_running_failure_becomes_an_app_not_found_error() {
        let err = script_failure_to_error(ScriptFailure::AppNotRunning {
            message: "Application isn’t running. (-600)".to_string(),
        });
        assert!(matches!(err, PolarizeError::AppNotFound(_)));
    }

    #[test]
    fn a_plain_script_failure_becomes_a_platform_error_with_its_code() {
        let err = script_failure_to_error(ScriptFailure::Script {
            code: Some(-2741),
            message: "syntax error".to_string(),
        });
        let text = err.to_string();
        assert!(text.contains("-2741"), "{text}");
        assert!(text.contains("syntax error"), "{text}");
    }

    #[test]
    fn a_missing_object_failure_keeps_its_message() {
        let err = script_failure_to_error(ScriptFailure::ObjectNotFound {
            message: "Can’t get window 9. (-1728)".to_string(),
        });
        assert!(err.to_string().contains("window 9"));
    }

    #[test]
    fn a_cancel_failure_keeps_its_message() {
        let err = script_failure_to_error(ScriptFailure::Cancelled {
            message: "User canceled. (-128)".to_string(),
        });
        assert!(err.to_string().contains("canceled"));
    }

    // --- Automation preflight status mapping (PINV-21) ---

    #[test]
    fn a_zero_status_means_the_preflight_passed() {
        assert_eq!(automation_check_from_status(0), AutomationCheck::Permitted);
    }

    #[test]
    fn the_two_refusal_statuses_report_different_permission_states() {
        assert_eq!(
            automation_check_from_status(ERR_NOT_AUTHORIZED),
            AutomationCheck::Refused(PermissionState::Denied)
        );
        assert_eq!(
            automation_check_from_status(ERR_NEEDS_CONSENT),
            AutomationCheck::Refused(PermissionState::NotDetermined)
        );
    }

    #[test]
    fn a_not_running_app_leaves_the_preflight_inconclusive() {
        assert_eq!(
            automation_check_from_status(ERR_APP_NOT_RUNNING),
            AutomationCheck::Inconclusive
        );
    }

    #[test]
    fn an_unknown_status_never_blocks_the_script() {
        assert_eq!(
            automation_check_from_status(-12345),
            AutomationCheck::Inconclusive
        );
    }

    // --- sdef scan ---

    #[test]
    fn the_scan_finds_command_names_and_descriptions() {
        let (commands, _) = scan_sdef(SDEF_FRAGMENT);
        assert_eq!(
            commands,
            vec![
                SdefEntry {
                    name: "open".to_string(),
                    description: Some("Open the specified object(s)".to_string()),
                },
                SdefEntry {
                    name: "close".to_string(),
                    description: Some("Close an object".to_string()),
                },
                SdefEntry {
                    name: "count".to_string(),
                    description: None,
                },
            ]
        );
    }

    #[test]
    fn the_scan_finds_class_names_and_decodes_entities() {
        let (_, classes) = scan_sdef(SDEF_FRAGMENT);
        assert_eq!(
            classes,
            vec![
                SdefEntry {
                    name: "window".to_string(),
                    description: Some("A window".to_string()),
                },
                SdefEntry {
                    name: "item".to_string(),
                    description: Some("An item & its info".to_string()),
                },
            ]
        );
    }

    #[test]
    fn the_scan_reports_a_repeated_command_once() {
        let (commands, _) = scan_sdef(SDEF_FRAGMENT);
        assert_eq!(commands.iter().filter(|c| c.name == "open").count(), 1);
    }

    #[test]
    fn the_scan_does_not_match_an_element_whose_name_only_starts_the_same() {
        let (commands, _) = scan_sdef(SDEF_FRAGMENT);
        assert!(commands.iter().all(|c| c.name != "not-a-command"));
    }

    #[test]
    fn the_scan_reads_attributes_in_any_order() {
        let xml = r#"<command description="Say hello" name="greet"/>"#;
        assert_eq!(
            scan_sdef_elements(xml, "command"),
            vec![SdefEntry {
                name: "greet".to_string(),
                description: Some("Say hello".to_string()),
            }]
        );
    }

    #[test]
    fn the_scan_ignores_an_element_with_no_name_attribute() {
        let xml = r#"<command code="xxxx"/><command name="ok"/>"#;
        assert_eq!(
            scan_sdef_elements(xml, "command"),
            vec![SdefEntry {
                name: "ok".to_string(),
                description: None,
            }]
        );
    }

    #[test]
    fn the_scan_is_shallow_and_reads_a_commented_out_command() {
        // This documents a known limit, not a wanted behavior: the scan
        // does not parse XML, so it cannot see comments.
        let xml = r#"<!-- <command name="ghost"/> -->"#;
        assert_eq!(
            scan_sdef_elements(xml, "command"),
            vec![SdefEntry {
                name: "ghost".to_string(),
                description: None,
            }]
        );
    }

    // --- perform_run_applescript ---

    #[test]
    fn a_run_without_a_target_passes_the_source_through_unwrapped() {
        let runner = FakeRunner::new(outcome_ok("hello\n"));
        let request = RunAppleScriptRequest {
            source: "return \"hello\"".to_string(),
            target_app: None,
            timeout_ms: None,
        };
        let response = perform_run_applescript(&runner, &request).unwrap();
        assert_eq!(response.output, "hello");
        assert!(!response.truncated);
        assert_eq!(response.timeout_ms, DEFAULT_TIMEOUT_MS);
        assert_eq!(
            runner.runs.borrow().as_slice(),
            &[RecordedRun {
                source: "return \"hello\"".to_string(),
                target_app: None,
                timeout_ms: DEFAULT_TIMEOUT_MS,
            }]
        );
    }

    #[test]
    fn a_run_with_a_target_wraps_the_source_and_names_the_target() {
        let runner = FakeRunner::new(outcome_ok("Untitled\n"));
        let request = RunAppleScriptRequest {
            source: "get name of front window".to_string(),
            target_app: Some("Finder".to_string()),
            timeout_ms: Some(1_500),
        };
        perform_run_applescript(&runner, &request).unwrap();
        let runs = runner.runs.borrow();
        assert_eq!(runs.len(), 1);
        assert_eq!(
            runs[0].source,
            "tell application \"Finder\"\nget name of front window\nend tell"
        );
        assert_eq!(runs[0].target_app.as_deref(), Some("Finder"));
        assert_eq!(runs[0].timeout_ms, 1_500);
    }

    #[test]
    fn a_blank_target_app_is_treated_as_no_target() {
        let runner = FakeRunner::new(outcome_ok(""));
        let request = RunAppleScriptRequest {
            source: "beep".to_string(),
            target_app: Some("   ".to_string()),
            timeout_ms: None,
        };
        perform_run_applescript(&runner, &request).unwrap();
        let runs = runner.runs.borrow();
        assert_eq!(runs[0].source, "beep");
        assert_eq!(runs[0].target_app, None);
    }

    #[test]
    fn a_run_clamps_an_out_of_range_timeout_before_the_platform_sees_it() {
        let runner = FakeRunner::new(outcome_ok(""));
        let request = RunAppleScriptRequest {
            source: "beep".to_string(),
            target_app: None,
            timeout_ms: Some(u64::MAX),
        };
        let response = perform_run_applescript(&runner, &request).unwrap();
        assert_eq!(response.timeout_ms, MAX_TIMEOUT_MS);
        assert_eq!(runner.runs.borrow()[0].timeout_ms, MAX_TIMEOUT_MS);
    }

    #[test]
    fn a_refused_script_becomes_a_permission_error() {
        let runner = FakeRunner::new(ScriptOutcome {
            stdout: String::new(),
            stderr: "execution error: Not authorized to send Apple events to Finder. (-1743)"
                .to_string(),
            exit_code: Some(1),
            timed_out: false,
        });
        let request = RunAppleScriptRequest {
            source: "beep".to_string(),
            target_app: Some("Finder".to_string()),
            timeout_ms: None,
        };
        let err = perform_run_applescript(&runner, &request).unwrap_err();
        assert!(
            matches!(
                err,
                PolarizeError::Permission(PermissionError::NotGranted {
                    kind: PermissionKind::Automation,
                    ..
                })
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn a_timed_out_run_reports_the_deadline_and_never_leaks_the_source() {
        let runner = FakeRunner::new(ScriptOutcome {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
            timed_out: true,
        });
        let request = RunAppleScriptRequest {
            source: "set pw to \"hunter2\"\nrepeat\nend repeat".to_string(),
            target_app: None,
            timeout_ms: Some(1_000),
        };
        let err = perform_run_applescript(&runner, &request).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("1000"), "{text}");
        assert!(!text.contains("hunter2"), "leaked the source: {text}");
    }

    #[test]
    fn a_long_output_comes_back_marked_as_truncated() {
        let runner = FakeRunner::new(outcome_ok(&"z".repeat(MAX_OUTPUT_CHARS + 1)));
        let request = RunAppleScriptRequest {
            source: "beep".to_string(),
            target_app: None,
            timeout_ms: None,
        };
        let response = perform_run_applescript(&runner, &request).unwrap();
        assert!(response.truncated);
        assert_eq!(response.output.chars().count(), MAX_OUTPUT_CHARS);
    }

    #[test]
    fn a_runner_that_cannot_start_reports_its_own_error() {
        let runner = FakeRunner::failing("osascript not found");
        let request = RunAppleScriptRequest {
            source: "beep".to_string(),
            target_app: None,
            timeout_ms: None,
        };
        let err = perform_run_applescript(&runner, &request).unwrap_err();
        assert!(err.to_string().contains("osascript not found"));
    }

    // --- perform_script_dictionary ---

    #[test]
    fn the_dictionary_tool_returns_scanned_verbs_for_the_named_app() {
        let runner = FakeRunner::new(outcome_ok(""));
        let request = ScriptDictionaryRequest {
            app: AppIdentifier {
                bundle_id: Some("com.apple.finder".to_string()),
                app_name: None,
            },
        };
        let response = perform_script_dictionary(&runner, &request).unwrap();
        assert_eq!(response.app_name, "Finder");
        assert_eq!(
            response
                .commands
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            vec!["open", "close", "count"]
        );
        assert_eq!(response.classes.len(), 2);
        assert_eq!(runner.sdef_calls.borrow().len(), 1);
        assert_eq!(
            runner.sdef_calls.borrow()[0].bundle_id.as_deref(),
            Some("com.apple.finder")
        );
    }

    #[test]
    fn the_dictionary_tool_reports_a_runner_error() {
        let runner = FakeRunner::failing("sdef failed");
        let request = ScriptDictionaryRequest {
            app: AppIdentifier::default(),
        };
        let err = perform_script_dictionary(&runner, &request).unwrap_err();
        assert!(err.to_string().contains("sdef failed"));
    }

    #[test]
    fn a_run_request_may_omit_both_optional_fields() {
        let request: RunAppleScriptRequest =
            serde_json::from_str(r#"{"source":"beep"}"#).expect("deserialize");
        assert_eq!(request.source, "beep");
        assert_eq!(request.target_app, None);
        assert_eq!(request.timeout_ms, None);
    }

    // --- wire shapes ---

    fn round_trip<T>(value: &T) -> T
    where
        T: Serialize + for<'de> Deserialize<'de>,
    {
        let json = serde_json::to_string(value).expect("serialize");
        serde_json::from_str(&json).expect("deserialize")
    }

    #[test]
    fn the_run_request_and_response_round_trip() {
        let request = RunAppleScriptRequest {
            source: "beep".to_string(),
            target_app: Some("Finder".to_string()),
            timeout_ms: Some(2_000),
        };
        assert_eq!(round_trip(&request), request);

        let response = RunAppleScriptResponse {
            output: "ok".to_string(),
            truncated: false,
            timeout_ms: 2_000,
        };
        assert_eq!(round_trip(&response), response);
    }

    #[test]
    fn the_dictionary_request_and_response_round_trip() {
        let request = ScriptDictionaryRequest {
            app: AppIdentifier {
                bundle_id: None,
                app_name: Some("Mail".to_string()),
            },
        };
        assert_eq!(round_trip(&request), request);

        let response = ScriptDictionaryResponse {
            app_name: "Mail".to_string(),
            commands: vec![SdefEntry {
                name: "check for new mail".to_string(),
                description: None,
            }],
            classes: vec![],
        };
        assert_eq!(round_trip(&response), response);
    }
}
