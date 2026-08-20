# Invariants

This document lists the properties that must always hold across
`polarize`. Each invariant is written as:

### PINV-N: short name

- Always: the property that must hold
- Because: why it is non-obvious — the reasoning or risk behind it
- If violated: the concrete symptom a violation produces

Invariant IDs are prefixed `PINV-` (Polarize invariant) and numbered in
the order they were added, not by severity or crate.

## Testing harness

`polarize-core` is fully unit-tested. It is pure, platform-agnostic
logic: coordinate normalization, the accessibility-tree data model, MCP
tool schemas, error types, the permission-state enum, and the trait
definitions `polarize-macos` implements. Every invariant that lives in
it has real `cargo test` coverage (224 tests as of this writing).

`polarize-macos` implements those traits with real native calls
(`ScreenCaptureKit`, `AXUIElement`, `CGEvent`, AppKit). These **cannot**
be automatically tested anywhere, not in this repository, not in CI. No
CI runner can grant Screen Recording or Accessibility TCC permission,
or verify pixel/AX content, headlessly. Exercising this crate for real
needs a real macOS session with Screen Recording and Accessibility
permissions granted to whatever process runs it, and a human (or a
scripted local run) driving it interactively. Where `polarize-macos`
factors a piece of genuinely pure logic out of its native calls
(`app_lookup`, `keymap`, `geometry`), that piece *is* covered by real
`cargo test` runs (22 tests as of this writing). Everything else in the
crate is compile/link-checked only.

Consequently, every invariant below that touches native-API behavior
says so explicitly in its enforcement-checklist entry. None of them may
claim automated coverage they don't have.

## Invariants

### PINV-1: coordinate conversion never silently clamps

- Always: `coords::fraction_to_pixel` and `coords::pixel_to_fraction`
  reject out-of-range input (a fraction outside `0.0..=1.0`, a pixel
  outside `0.0..=dimension`, or a non-positive target size) with a
  `CoordError` instead of clamping it into range.
- Because: a `tap` request built from a stale or miscomputed fraction
  (e.g. `x: 1.4` from a caller that mixed up pixels and fractions) must
  not silently land at the target's edge — that produces a click on the
  wrong element that looks like a successful tap.
- If violated: a caller passing garbage coordinates gets a "successful"
  tap at the wrong point instead of a clear error that points at the
  caller's bug.

### PINV-2: every tool call is gated on exactly one permission (decision logic)

- Always: `permission::required_permission` maps each `ToolKind`
  (`Screenshot`, `Describe`, `Tap`, `Keyboard`) to exactly one
  `PermissionKind` (`ScreenRecording` or `Accessibility`), and
  `permission::check_permission` refuses to run a tool whose required
  permission is not `Granted` (a permission absent from the caller's
  status list is treated as `NotDetermined`, never as implicitly
  granted).
- Because: `polarize-macos`'s native calls fail in ways that are easy to
  misdiagnose from the raw OS error alone (a denied AX permission and a
  genuinely missing UI element can both surface as "element not found").
  Deciding, from a permission-status list, whether a tool may run — in
  one pure, testable place — is what makes that decision auditable
  independent of which real API `polarize-macos` happens to call to
  learn the status. (Scope note: this function encodes the *decision*;
  see PINV-10 for how `polarize-macos`'s real tool implementations
  actually learn the permission status and gate on it today.)
- If violated: a caller sees a confusing native failure (or, worse, a
  `tap`/`keyboard` call that silently no-ops) instead of "grant
  Accessibility access to run this tool".

### PINV-3: `describe` output is pre-order and depth-accurate

- Always: `ax::flatten` visits a node before any of its descendants. It
  visits a node's children in their original order. It records each
  node's depth as the number of ancestors above it, so the root is depth
  `0`. `ax::format_tree` renders that flattened sequence as one line per
  node, indented two spaces per depth level.
- Because: `orchestrate::perform_describe` embeds `format_tree`'s
  rendering directly in `DescribeResponse::formatted` (PINV-9). Depth
  must be correct for its indentation to make sense. Pre-order must hold
  for a subtree to read as one contiguous run. A traversal bug could
  reorder children or miscount depth without ever producing an error.
- If violated: `describe`'s `formatted` output renders as a flat or
  mis-indented list. A reader cannot tell which elements nest inside
  which container.

### PINV-4: a tap's fraction is normalized before the platform ever sees it

- Always: `orchestrate::perform_tap` converts the request's `x`/`y`
  fraction to a pixel point via `coords::fraction_to_pixel`. It resolves
  that conversion against the `size` of the `PixelRect` the requested
  target resolves to, from `WindowManager::resolve_target_rect`. It then
  adds that rect's `origin`, to land in the **global** display
  coordinate space. Only this already-resolved global pixel point ever
  reaches `InputSynthesizer::click_at_pixel` — never the raw fraction,
  and never a window- or display-relative point. An out-of-range
  fraction returns a `CoordError` (PINV-1) before `InputSynthesizer` is
  invoked at all.
- Because: `InputSynthesizer` implementations are real `CGEvent` calls
  that cannot be exercised in CI. Pushing the fraction-to-pixel decision
  into this pure, testable function lets a fake implementation prove
  three things without a real screen. It proves the platform layer
  receives correct pixel coordinates. It proves the target's screen
  origin is actually applied. It proves the platform layer is never
  invoked on invalid input. The origin matters because an `App`/`Window`
  target, or a non-primary display, does not start at the global origin.
  This was not a hypothetical. An earlier
  version of `resolve_target_rect` (then `resolve_target_size`) returned
  only size, with no origin. A real `tap` call against a live app
  window, not positioned at the screen's own origin, then silently
  clicked whatever sat at that pixel offset on the *primary* display
  instead. It raised no error at all, because `click_at_pixel` cannot
  tell a window-relative point from a global one.
- If violated: a `tap` request appears to succeed but clicks the wrong
  point — either because the fraction leaked through unconverted,
  because it was normalized against the wrong target's size, or because
  the target's screen origin was dropped.

### PINV-5: bundle id is tried before app name, but a mismatch falls through

- Always: `app_lookup::find_matching_app_index` tries an exact
  `bundle_id` match first when `identifier.bundle_id` is set; only if
  that yields no match does it fall back to a case-insensitive
  `app_name` match.
- Because: `AppIdentifier`'s own doc comment documents this "bundle id
  tried first" contract; a caller that supplies a stale or
  slightly-wrong bundle id (but a correct name) should still resolve
  the app rather than fail outright.
- If violated: a caller who supplies both fields gets `AppNotFound`
  whenever the bundle id is even slightly wrong, even though the name
  alone would have resolved unambiguously.

### PINV-6: the modifier→flag mapping is a lossless, order-independent OR

- Always: `keymap::modifiers_to_cgevent_flags` ORs together exactly the
  `CGEventFlags` mask bit for each `Modifier` present in the input,
  regardless of input order or duplicates, and no others.
- Because: `CGEventSetFlags` takes a single bitmask; if the mapping
  silently drops a requested modifier (e.g. because of a copy-paste bug
  picking the wrong `CGEventFlags::Mask*` constant) a `keyboard` call
  posts a keystroke with the wrong modifiers held, which is very easy to
  miss in a manual smoke test since *a* key does get pressed.
- If violated: e.g. a caller asks for Command+Shift and the posted event
  silently carries only Command, so a shortcut that depends on Shift
  (rename vs. duplicate, etc.) fires the wrong action.

### PINV-7: an N-click tap posts N down/up pairs with an ascending click state

- Always: `keymap::click_event_sequence` returns exactly
  `2 * max(click_count, 1)` steps: for each `state` in
  `1..=click_count.max(1)`, a `LeftMouseDown` then a `LeftMouseUp`, both
  carrying click state `state`.
- Because: macOS's own double/triple-click recognition is driven by the
  click-state field, not by event timing alone — posting two clicks both
  at state `1` (instead of `1` then `2`) makes the window server treat
  them as two independent single clicks, so a `tap` request with
  `click_count: 2` would silently fail to trigger double-click behavior.
- If violated: a `tap` request that asks for a double-click instead
  performs what the target application sees as two single clicks (e.g.
  selecting a word fails, deselecting instead).

### PINV-8: an AX frame is clamped into `[0.0, 1.0]`, never dropped

- Always: `geometry::safe_normalize_frame` converts a pixel
  position/size pair to a `NormalizedFrame` whose `x`, `y`, `width`, and
  `height` are all clamped into `0.0..=1.0`. This holds even when the
  input pixel rectangle falls partly or fully outside `screen_size`, and
  even when `screen_size` itself is non-positive (guarded against
  dividing by zero).
- Because: a `tap` fraction must error loudly on bad input, to catch a
  caller's coordinate-space mistake (PINV-1). A `describe` response is
  different: it is built from real AX geometry the caller never
  supplied. An off-screen element is a legitimate, common case —
  multi-monitor setups, or a window dragged half off-screen — not a
  caller error. It must degrade to a best-effort frame, not vanish from
  the tree or blank out an entire subtree with a propagated error.
- If violated: `describe` either panics or errors on the first
  off-screen element it meets, making multi-monitor setups unusable. Or
  it passes an out-of-range frame through to callers who trusted the
  `[0,1]` contract every other normalized frame in the response honors.

### PINV-9: a `screenshot` response is a self-contained base64 PNG, never a file path

- Always: `ScreenshotResponse` carries the captured PNG as a base64
  string (`png_base64`) in the tool response body itself.
  `ScreenshotResponse::from_png_bytes` is the only constructor
  `orchestrate::perform_screenshot` uses, and it always base64-encodes;
  nothing in `polarize-core` or `polarize-macos` writes a screenshot to
  a path and returns that path instead.
- Because: `polarize`'s client is often a separate, possibly sandboxed
  process, with no shared filesystem namespace guaranteed. MCP's own
  image content type already expects base64-encoded bytes inline. A file
  path would need a second, out-of-band contract instead: where the file
  lives, who deletes it, whether the client can read it before or after
  the server exits. Base64 in the response avoids all of that, and every
  tool response stays self-contained. (`polarize-macos`'s
  `capture_and_encode` does use a uniquely named temp file internally,
  because `ScreenCaptureKit`'s PNG export is file-based, not
  buffer-based. It reads the bytes back and deletes the file before
  returning, so that detail never crosses the `polarize-core` API
  boundary.)
- If violated: a screenshot response either silently fails against a
  sandboxed MCP client that cannot resolve a returned path, or leaks a
  temp file the caller never asked for and nothing ever cleans up.

### PINV-10: every tool preflights its real permission before any other native call

- Always: `MacAccessibilityInspector::describe` checks
  `AXIsProcessTrusted()`. `MacInputSynthesizer`'s `click_at_pixel`,
  `type_text`, and `press_key` each check `CGPreflightPostEventAccess()`.
  `MacScreenCapture`'s `capture_screen` and `capture_window` each check
  `CGPreflightScreenCaptureAccess()`. In every case, the check runs
  before any further native call, and returns `PolarizeError::Permission`
  when it fails — instead of letting the underlying `AXUIElement`/
  `CGEvent`/`ScreenCaptureKit` call run and fail on its own terms.
- Because: without this preflight, a denied or not-yet-granted
  permission and a genuinely missing UI element or app can both surface
  as an opaque native failure. A caller has no reliable way to tell
  "grant this permission" apart from "your app/window identifier is
  wrong". The preflight turns the common case into a clean, typed
  `PolarizeError::Permission` before any ambiguous native error has a
  chance to occur.
- If violated: a permission problem surfaces as a confusing native error
  instead of an actionable one.

### PINV-11: a native permission preflight failure always reports `NotDetermined`, never falsely `Denied`

- Always: when `AXIsProcessTrusted()`, `CGPreflightPostEventAccess()`, or
  `CGPreflightScreenCaptureAccess()` returns `false`, `polarize-macos`
  reports `PermissionState::NotDetermined`. It never reports
  `PermissionState::Denied` on the strength of that boolean alone.
- Because: all three of those APIs collapse "the user was never asked"
  and "the user explicitly denied it" into the same `false` return
  value. `polarize-macos` cannot distinguish the two from the boolean
  alone. Reporting `Denied` would claim the user made an explicit choice
  `polarize` has no evidence for — a stronger, and potentially wrong,
  claim than the API actually supports. `NotDetermined` is the
  conservative, honest reading of an ambiguous `false`.
- If violated: a caller who has simply never been asked for
  Accessibility access would be told they explicitly "denied" it —
  misleading guidance for how to fix it (macOS never re-prompts a
  denied permission the same way it prompts a not-yet-determined one).

### PINV-12: a single unreadable AX attribute degrades to a default, never aborts the tree walk

- Always: `accessibility::build_node` treats every per-attribute read
  failure as a default value rather than propagating an error: an
  unreadable `AXRole` becomes `"AXUnknown"`; an unreadable/empty
  `AXTitle`/`AXDescription`/`AXValue` is skipped in favor of the next
  candidate, defaulting to `label: None` if none read; an unreadable
  `AXPosition`/`AXSize` defaults to a zero point/size (which
  `safe_normalize_frame`, PINV-8, then clamps into a valid frame); an
  unreadable `AXFocused`-settable check or `AXUIElementCopyActionNames`
  call defaults to `false`. No single attribute failure ever aborts
  `describe` for that node or its subtree.
- Because: real-world AX trees are inconsistent. Plenty of elements
  legitimately lack a title. A third-party app's AX implementation may
  error on an attribute Apple's own apps always supply. Treating any one
  such gap as fatal would make `describe` unusable against exactly the
  less-polished apps where an agent most needs accessibility
  introspection to work.
- If violated: `describe` would fail outright (or silently truncate the
  whole tree) the moment it meets one AX element with an unreadable
  attribute, instead of describing everything it successfully could.

### PINV-13: an AX tree walk is capped at `MAX_AX_DEPTH`; deeper subtrees are truncated, not errored

- Always: `accessibility::build_node` stops reading `AXChildren` once
  `depth >= MAX_AX_DEPTH` (`64`) — the node itself is still built and
  included in the tree (role, label, frame, flags), but its `children`
  field is forced to an empty `Vec` rather than recursing further. No
  error is raised and no earlier ancestor is affected.
- Because: real AX trees are supposed to be finite and shallow. A
  misbehaving or adversarial app could in principle expose a very deep,
  or effectively cyclic, tree — some accessibility proxies re-expose a
  wrapped element under a new node. Without a cap, walking such a tree
  would hang or exhaust memory instead of returning a merely incomplete
  result.
- If violated: a single pathological app's AX tree could hang or crash
  every future `describe` call, not just fail to fully describe that
  one app.

### PINV-14: a `keyboard` request activates its target app first

- Always: when a `keyboard` request names a `target` app,
  `orchestrate::perform_keyboard` calls `WindowManager::activate_app`
  with it before calling either `InputSynthesizer::type_text` or
  `InputSynthesizer::press_key`. When `target` is `None`, it calls
  neither.
- Because: `CGEvent` posts to whichever app is currently frontmost, not
  to an app named in the request. Without activating the target first, a
  `target`-scoped `keyboard` call would silently type into whatever app
  the user happened to have focused instead.
- If violated: text or key presses land in the wrong app. Or — if
  `target` were dropped from the schema instead of wired up — `polarize`
  would advertise a field its `keyboard` tool never honors.

### PINV-15: an element selector must name a criterion, and resolves in pre-order

- Always: `selector::find_all` and `selector::find_one` reject an
  `ElementSelector` that sets no criterion, with `SelectorError::Empty`.
  A criterion is one of `identifier`, `role`, `subrole`, `label`, or
  `label_contains` — a field that names an element. `enabled_only` and
  `index` are filters, not criteria: one narrows a set of matches and the
  other picks one out of it, so neither counts toward the guard. Both
  functions return their matches in the same pre-order `ax::flatten` uses
  (PINV-3), so `ElementSelector::index` names the same element every
  time for the same tree.
- Because: a selector resolves to a real press or a real wait. An empty
  selector matches the application root, so a `perform_action` call would
  silently press whatever sits first in the tree, and an
  `await_ui_element` call would report instant success without waiting
  for anything. Counting a filter as a criterion reopens exactly that
  hole through a field that reads like a real one: `{"enabled_only":
  true}` names no element, and every application root is enabled. An unstable match order
  would make one `index` name a different element on each call, which is
  worse than an error, because the caller cannot see it happen.
- If violated: a tool acts on an element the caller never named, and the
  same request acts on a different element on the next run.

### PINV-16: an unread AX attribute degrades to "absent", never to a wrong value

- Always: `accessibility::build_node` reads each enriched attribute
  (`AXEnabled`, `AXSubrole`, `AXRoleDescription`, `AXIdentifier`,
  `AXHelp`, and the action-name list) on its own. A read that fails, or
  returns an empty string, yields `None` for a string attribute and an
  empty list for the action list. A failed `AXEnabled` read yields
  `true`, not `false`. `AxNode`'s serde defaults match, so an older
  `describe` response still deserializes.
- Because: most AX elements publish only some of these attributes, so a
  failed read is the normal case, not an error (PINV-12 says one bad
  attribute must not abort a walk). `AXEnabled` is the asymmetric one:
  the rest of `polarize` reads `enabled: false` as "the app says this
  control is off", and `ElementSelector::enabled_only` skips such an
  element. Defaulting a missing read to `false` would hide every element
  that simply does not publish the attribute.
- If violated: `enabled_only` selectors silently match nothing on apps
  that do not publish `AXEnabled`, and a caller reads an absent
  `AXIdentifier` as a real, empty identifier it can select on.

### PINV-17: `perform_action` checks the element before it acts

- Always: `action::perform_element_action` resolves the selector to one
  node, then refuses in two cases. It refuses when the node's `actions`
  list does not hold the chosen action. It refuses when the node's
  `enabled` flag is `false`. A refusal returns an `ActionError` and
  never calls `ActionPerformer::perform_action_at_path`.
- Because: `AXUIElementPerformAction` is synchronous and has no timeout
  of its own. An app can block the caller for the full AX timeout on an
  action the element does not handle. A greyed-out control is the
  common case: it still publishes `AXPress`, it still answers, and it
  still does nothing. Both cases look like a hang, or like a silent
  success, to an agent that cannot see the screen. The tree `describe`
  already returned carries both facts, so the check costs no extra
  native call.
- If violated: a `perform_action` call blocks the whole stdio MCP
  server until the AX timeout expires. Or it reports `performed: true`
  for a control the app never let the user press.

### PINV-18: one index path names one element in both walks

- Always: the child indices `selector::find_one` resolves against a
  `describe` tree are the same indices `action::walk_path` follows down
  the live `AXUIElement` hierarchy in `polarize-macos`. Both walks read
  the children of a node in the order the app publishes them, and both
  count from `0`. `accessibility::build_node` builds each `AxNode`'s
  `children` from `AxElement::children()` in that same order.
- Because: an `ElementPath` is the only thing that crosses from
  `polarize-core` to `polarize-macos`. `polarize` holds no live
  `AXUIElement` between the two walks, so nothing else identifies the
  element. If either side reordered, filtered, or skipped a child, the
  path would still resolve, and it would resolve to the wrong element.
  A wrong press is silent: the tool reports the element it *resolved*
  in core, not the element the platform actually pressed. Note the
  known limitation this invariant does not remove: the app can change
  its interface between the two walks. `walk_path` reports an
  out-of-range index as an error rather than acting on the parent.
  Both walks also address one app, never "whatever is frontmost now",
  twice: a request that names no app has `action::resolved_target`
  substitute what `describe` resolved, so a focus change between the
  walks cannot send the press into a second app at the same path. The
  substitute is the resolved bundle id when the app publishes one, and
  its localized name only when it does not. A bundle id is unique; a
  localized name is not, and `window::resolve_running_app` matches the
  first process carrying it in whatever order `NSWorkspace` lists them.
- If violated: `perform_action` presses a different element than the
  one its own response names, and no error reports the difference. Drop
  the app substitution, and it presses that element in a different app.
  Substitute a localized name where a bundle id was available, and two
  processes sharing one name reopen the same hole, silently.

### PINV-19: a wait checks at least once, and never waits past its deadline

- Always: `wait::perform_await_ui_element` and
  `wait::perform_await_screen_idle` read the accessibility tree once
  before they can report a timeout, even when `timeout_ms` is `0`.
  Between two reads they wait for at most
  `min(poll_interval_ms, milliseconds left to the deadline)`. A
  `UiChangeWaiter` that reports no change neither ends the wait early
  nor extends it. A `UiChangeWaiter` that *fails* after consuming its
  budget does not end the wait either: `wait::wait_one_slice` reads the
  clock before and after the call, and degrades to polling whenever time
  really passed. A waiter that fails without consuming any of its budget
  does end the wait, because polling on through it would spin. A
  `SelectorError::Empty` fails at once, without waiting.
- Because: this is the hybrid design the tools depend on. `polarize-macos`
  wakes a wait on an accessibility notification, but some trees never
  post one. A web view inside a native window is the usual case: its
  content changes and no `AXLayoutChanged` arrives. Bounding every wait
  by the poll interval turns a missed notification into one extra tree
  read instead of a hang for the whole timeout. Reading the tree before
  the deadline test matters because `timeout_ms: 0` is a legal "is it
  there right now" request. An empty selector matches the application
  root, so retrying it would spend the whole timeout on a request that
  is already wrong (PINV-15). Degrading on a waiter failure matters for
  the same reason polling exists: `AXObserverCreate` fails, and an app
  refuses every notification, on exactly the apps that never post one.
  Aborting there would turn the fallback path into the failure path. The
  clock reading is what makes that safe — it separates "the notification
  channel is unavailable, and a poll interval has passed" from "the
  waiter returned instantly", which no amount of retrying can advance.
- If violated: `await_ui_element` blocks for its full timeout against
  any app whose accessibility tree under-reports, or a zero timeout
  returns a timeout error without ever looking at the tree, or a wait
  against an app with no usable observer fails at the first poll
  boundary instead of polling, or a waiter that fails instantly spins
  the CPU until the deadline.

### PINV-20: one thread owns a whole `AXObserver` lifecycle

- Always: `observer::MacUiChangeWaiter::wait_for_change` preflights
  `AXIsProcessTrusted`, then starts one thread. That thread creates the
  `AXUIElement`, the `AXObserver`, and the `CFRunLoop` source, runs the
  run loop, removes the source, removes each notification, and releases
  both handles, all before it ends. Only a `Result<bool, String>`
  crosses back. Registration is best-effort: the wait fails only when
  every one of `AXCreated`, `AXLayoutChanged`, and `AXValueChanged` is
  refused. The call returns `false` only after its budget really
  elapsed, and it returns an error only after its budget really elapsed
  too.
- Because: an `AXObserverRef` and a `CFRunLoopRef` belong to the thread
  that made them, and neither is `Send`. `apps/polarize` is an async
  `rmcp` server, so no Tokio worker thread runs a `CFRunLoop` at all.
  Cleanup matters because the server runs for hours and calls this once
  per poll interval; one leaked run-loop source per call is a real leak.
  Partial registration matters because many apps support only some
  notifications, and failing on the first refusal would break the tool
  on them. Returning early with `false` would make `wait`'s poll
  fallback re-walk the whole tree as fast as the CPU allows. Sleeping
  out the budget on failure matters for the same reason: `polarize-core`
  reads its clock to decide whether a waiter failure is a missed
  notification or a fault, and a failure with no time elapsed reads as a
  fault (PINV-19).
- If violated: undefined behavior from using a Core Foundation handle on
  the wrong thread, a leaked run-loop source per tool call, a busy loop
  that pins a core, or an `await` tool that refuses to run against
  common apps. Return an error early, and `polarize-core` reads it as a
  fault and ends the wait rather than polling through it.

### PINV-21: an Automation refusal is a permission error, not a script error

- Always: `script::parse_osascript_error` maps `osascript` error codes
  `-1743` and `-1744` to `ScriptFailure::AutomationNotPermitted`, and
  `script::script_failure_to_error` turns that into
  `PolarizeError::Permission` with `PermissionKind::Automation`.
  `script::automation_check_from_status` maps the same two codes the
  same way when the native `AEDeterminePermissionToAutomateTarget`
  preflight reports them. Code `-1743` means the user refused; code
  `-1744` means the user has not been asked. Code `-600` becomes
  `PolarizeError::AppNotFound`. Every other status leaves the run
  allowed, and every other error code stays a script error that keeps
  its message and its code.
- Because: AppleScript reports "you have no Automation permission" and
  "your script has a bug" on the same channel, one line of text on
  stderr. A caller that cannot tell them apart retries a script forever
  against a permission only a human can grant, in System Settings >
  Privacy & Security > Automation. macOS grants Automation per
  (caller, target) pair, so this refusal can appear for one app while
  another app works. The preflight must also never block on a status it
  does not understand: `-600` only means the target app is not running
  yet, and AppleScript can launch it.
- If violated: a missing Automation grant looks like a broken script,
  the caller never learns which app needs approval, and a preflight
  quirk silently blocks scripts that would have run.

### PINV-22: a script source never travels into an error message as written

- Always: every error `polarize` builds from a `run_applescript`
  request passes the source through `script::redact_source` first. That
  function removes the text inside every double-quoted AppleScript
  literal, flattens the rest to one line, and cuts it to
  `script::MAX_SOURCE_CHARS_IN_ERROR` characters. An unterminated
  literal counts as open to the end of the source.
- Because: a script often carries a secret. `set pw to "hunter2"` is a
  normal thing for a caller to send, and the timeout path is exactly
  when an error wants to quote the script back. Error strings travel
  further than a caller expects: into MCP client logs, into
  transcripts, and into bug reports. Truncation alone is not enough,
  because a secret often sits in the first line.
- If violated: a password or a token that a caller sent once sits in a
  log file, and nobody knows it is there.


### PINV-23: every tool preflights the login session, and reports the console first

- Always: after its TCC permission check, and before any other native
  call, each tool that captures pixels, reads the accessibility tree, or
  posts synthetic input calls
  `polarize_macos::session::ensure_session_usable`. That is `screenshot`,
  `describe`, `tap`, `keyboard`, `perform_action`, `await_ui_element`,
  and `await_screen_idle`. The two AppleScript tools do not call it, and
  must not — see the exclusion note below.
  That call reads `CGSessionCopyCurrentDictionary` and applies
  `polarize_core::session::check_session`. `check_session` returns
  `PolarizeError::SessionNotOnConsole` when the session does not own the
  console, and `PolarizeError::ScreenLocked` when the session owns the
  console but the screen is locked. When both facts hold, it reports the
  console error, never the lock error.
- Because: both states break the native calls without any error.
  `ScreenCaptureKit` hands back black or lock-window pixels, the AX tree
  describes the login window instead of the target app, and a posted
  `CGEvent` reaches no one. The two states also need different repairs.
  Fast User Switching raises both flags at once, because it locks the
  session it switches away from. An unlock repairs only a session that
  still owns the console. So the console fact is the one that tells a
  caller what to do next.
- If violated: a caller gets a black screenshot, a lock-screen AX tree,
  or a click that lands nowhere, and no error explains why. A caller
  told "screen is locked" during Fast User Switching unlocks the Mac,
  sees no change, and has no next step.
- Exclusion: `run_applescript` and `script_dictionary` skip this
  preflight on purpose. Neither one captures pixels, reads the
  accessibility tree, or posts input. AppleScript sends an Apple Event,
  and `sdef` reads a static file from an app bundle. Both still work
  while the screen is locked. Refusing them there would invent a failure
  the caller would otherwise never see, which is worse than having no
  preflight at all.

### PINV-24: an absent session key reads as a usable session, never as a blocked one

- Always: `SessionState::from_flags` maps an absent console flag to
  `on_console: true`, and an absent lock flag to `screen_locked: false`.
  `MacSessionInspector` reports the same usable default when
  `CGSessionCopyCurrentDictionary` returns nothing at all. The preflight
  fails open.
- Because: macOS adds the `"CGSSessionScreenIsLocked"` key only while
  the screen is locked. An unlocked Mac simply has no such key, so a
  missing key is the normal case. `CGSessionCopyCurrentDictionary` also
  returns nothing for a process outside a GUI login session. Failing
  closed on either would refuse every tool call on a healthy Mac. This
  preflight improves a diagnosis; it is not a security boundary. A truly
  unusable environment still fails at the native call, with that call's
  own error.
- If violated: `polarize` refuses every tool call on an unlocked Mac,
  and one wrong key name (the two names are not symmetric — see
  `crates/polarize-macos/src/session.rs`) turns the whole server off.


### PINV-25: a command's deadline bounds the call, not just the child

- Always: `process::run_with_deadline` returns within roughly
  `timeout + reader_grace`. It kills the child at `timeout`. It then
  waits at most `reader_grace` for the stdout and stderr reader threads,
  and takes whatever they collected either way, reporting
  `CommandOutcome::output_truncated` when it abandoned one.
  `polarize_macos::applescript` folds that flag into `timed_out`.
- Because: killing a child does not close its pipes. Any process still
  holding the write end keeps them open, and `read_to_end` returns only
  once the last writer closes. `osascript` is exactly that case: the
  scripts it runs start helpers, and a target app can inherit the
  descriptor. Joining a reader thread unconditionally would then block
  with no bound at all, long past the deadline the caller set. The
  readers append into a shared buffer instead of returning one, so the
  output that did arrive survives a reader this code abandons.
- If violated: `run_applescript` hangs far past its two-minute clamp,
  pinning the `tokio` blocking thread `apps/polarize/src/server.rs` put
  it on, and the caller never sees an error — only a tool call that
  never returns.

### PINV-37: a Vision box is flipped into top-left space, never passed through

- Always: `find_text::flip_to_top_left` converts a `VisionRect` into a
  `NormalizedFrame`. A `VisionRect` has its origin at the **bottom**
  left, and its `y` grows upward, because that is the space Vision
  reports. A `NormalizedFrame` has its origin at the **top** left, and
  its `y` grows downward, because that is the space every other
  `polarize` response uses (PINV-8). The rule is
  `top = 1 - (bottom + height)`. The result is clamped into `0.0..=1.0`,
  and a non-finite component becomes `0.0`. Every `find_text` result
  passes through this function. `polarize-macos`'s `vision` module must
  not flip a box itself.
- Because: both spaces normalize to `0.0..=1.0`. So a dropped flip
  produces numbers that pass every range check `polarize` makes,
  including `tap`'s (PINV-1). Nothing errors, and nothing looks wrong in
  the response. The tap simply lands on the vertical mirror of the right
  place. This is the one `find_text` bug a caller cannot see in the
  output, which is why the conversion lives in `polarize-core` as pure
  arithmetic instead of inside the Vision call.
- If violated: every `find_text` result taps the mirror image of the
  text it found. A match near the top of a window presses whatever sits
  near the bottom, and the tool still reports success.

### PINV-38: a `find_text` match is filtered, then ordered, then indexed

- Always: `find_text::scan_lines` performs three steps in this order. It
  drops every recognized line below the confidence floor
  (`min_confidence`, or `DEFAULT_MIN_CONFIDENCE` when the request sets
  none). It orders what is left top to bottom by the recognized line's
  own top edge, then left to right, then by text. It keeps the lines
  that satisfy the request's match mode. Only then does
  `find_text::pick_match` apply the request's `index`. An empty request
  text, or a `min_confidence` outside `0.0..=1.0`, is rejected before
  any capture or OCR runs at all.
- Because: Vision returns its observations in no order a caller can rely
  on, and it reads low-confidence garbage out of textured backgrounds
  and window shadows. `index` must name the same line on two calls
  against the same screen, exactly as `ElementSelector::index` does for
  the accessibility tree (PINV-15). Indexing before filtering, or
  indexing an unordered list, moves the caller's chosen match every time
  a faint line appears or disappears at the edge of the screen.
- If violated: `index: 1` presses a different control on each call, and
  the caller cannot see it happen, because both calls succeed.

## Enforcement checklist

- **PINV-1** — fully covered by automated `cargo test -p polarize-core`
  (`coords::tests`): table-driven cases across both corners, the center,
  and out-of-range fractions/pixels/sizes on both conversion directions,
  plus a round-trip stability case.
- **PINV-2** — the decision logic itself is fully covered by automated
  `cargo test -p polarize-core` (`permission::tests`): every
  `ToolKind`→`PermissionKind` mapping, the
  granted/denied/not-determined/restricted usability rule, and the
  absent-status-means-not-determined rule. Note:
  `required_permission`/`check_permission` are not currently called from
  `polarize-macos` or `apps/polarize`. The real tool implementations gate
  permission a different way (see PINV-10). That real-world gating path
  is native-only, and has **not** been automatically tested anywhere.
- **PINV-3** — fully covered by automated `cargo test -p polarize-core`
  (`ax::tests`): depth-zero single node, pre-order traversal with
  correct depths across a multi-level tree, children-order preservation,
  and exact `format_tree` output for a hand-built tree. `orchestrate::tests`
  additionally asserts `perform_describe`'s `formatted` field equals
  `ax::format_tree`'s output for the same tree, confirming the real
  consumer this invariant names.
- **PINV-4** — the orchestration half (fraction→pixel conversion,
  origin addition, dispatch, and the "never call the platform on bad
  input" rule) is fully covered by automated `cargo test -p
  polarize-core` (`orchestrate::tests`) against a fake
  `WindowManager`/`InputSynthesizer`, including a regression test for a
  target whose origin is not `(0, 0)`. The half this invariant depends
  on but cannot itself verify — that `polarize-macos`'s real `CGEvent`
  call actually lands the click at the given pixel point on a real
  screen, and that `resolve_target_rect`'s real `CGDisplayBounds`/
  `SCWindow::frame()` origins are themselves correct — is **not**
  automated anywhere. Verified manually against a real running app
  (Uno.app) on 2026-08-19: before the origin fix, an `App`-scoped `tap`
  silently clicked the wrong point with no error; after it, `tap`
  against a window not positioned at the screen origin landed correctly.
- **PINV-5** — fully covered by automated `cargo test -p polarize-macos`
  (`app_lookup::tests`): exact bundle-id match, case-insensitive name
  fallback, bundle-id-mismatch-falls-through-to-name, bundle-id-wins-
  over-a-differing-name, empty identifier, empty candidate list, and
  no-match cases.
- **PINV-6** — fully covered by automated `cargo test -p polarize-macos`
  (`keymap::tests`): no modifiers, each single modifier, multiple
  modifiers ORed, order independence, and duplicate-modifier
  idempotence, all asserted against real `objc2_core_graphics::CGEventFlags`
  values.
- **PINV-7** — fully covered by automated `cargo test -p polarize-macos`
  (`keymap::tests`): exact step sequence for a single click, exact
  ascending-click-state sequence for a double click, and the
  zero-click-count-treated-as-one-click case.
- **PINV-8** — fully covered by automated `cargo test -p polarize-macos`
  (`geometry::tests`): an on-screen rectangle normalizing exactly,
  negative position clamping to `0.0`, an out-of-bounds position
  clamping to `1.0`, an oversized dimension clamping to `1.0`, and a
  non-positive `screen_size` not dividing by zero.
- **PINV-9** — the response-shape half (always base64, round-trips
  correctly, an all-`polarize-core` decision) is fully covered by
  automated `cargo test -p polarize-core` (`schema::tests`:
  `screenshot_response_round_trips`,
  `screenshot_response_encodes_and_decodes_png_bytes`). Whether a real
  captured `CGImage` actually round-trips through `capture_and_encode`'s
  temp-file PNG bridge (`crates/polarize-macos/src/capture.rs`) to
  produce bytes that decode to a recognizable screenshot is **not**
  automated anywhere — it needs a real macOS session with Screen
  Recording permission granted.
- **PINV-10** — **not** automated anywhere; this is entirely native
  behavior (`AXIsProcessTrusted`, `CGPreflightPostEventAccess`,
  `CGPreflightScreenCaptureAccess`). It has been exercised manually,
  more than once, by driving the built release binary over real MCP
  stdio traffic with no permission granted: `screenshot` returns a
  clean `{"permission_kind":"screen_recording","permission_state":"not_determined"}`
  error, and `describe`/`keyboard` each return the matching
  `accessibility` error, all before any further native call runs. This
  confirms the preflight fires correctly with permission absent. It does
  **not** confirm the granted path — that a preflight correctly returns
  `true` once the permission is actually granted — which still needs a
  real macOS session with both permissions granted.
- **PINV-11** — **not** automated anywhere. Distinguishing "never asked"
  from "explicitly denied" requires actually denying the permission on a
  real macOS session (e.g. via System Settings) and comparing the
  resulting behavior against the never-asked case — this has not been
  done. Needs a real macOS session with Accessibility permission
  explicitly denied (not merely un-granted) to verify.
- **PINV-12** — **not** automated anywhere. `AxElement` wraps a raw,
  non-mockable `AXUIElementRef`, so exercising a specific attribute-read
  failure (as opposed to a wholesale missing-permission failure) needs a
  real AX element that genuinely fails to answer one specific attribute
  query — needs a real macOS session against an app known to have such a
  gap.
- **PINV-13** — **not** automated anywhere. Exercising the depth cap
  needs either a real, unusually deep AX tree or a real accessibility
  proxy that re-exposes an element cyclically — neither is producible
  without a live accessibility session. `MAX_AX_DEPTH`'s value itself
  (`64`) is a plain constant with no logic to unit test in isolation.
- **PINV-14** — the orchestration half (activate-before-type,
  activate-before-key-press, and no-activation-when-`target`-is-`None`)
  is fully covered by automated `cargo test -p polarize-core`
  (`orchestrate::tests`) against a fake `WindowManager`. It was also
  confirmed over real MCP stdio traffic: a `keyboard` call with a
  `target` reached `WindowManager::activate_app`'s real
  `resolve_running_app` lookup (observed as `"app not found:
  com.apple.TextEdit"`, since no matching app runs in this environment),
  while a `keyboard` call with `target: null` stopped at the
  Accessibility preflight instead, without attempting activation. What
  is **not** verified: whether `activateWithOptions` actually brings a
  real, running target app to the front on a live macOS session — that
  needs one, with Accessibility permission granted and a real target
  app running.
- **PINV-15** — fully covered by automated `cargo test -p polarize-core`
  (`selector::tests`), including which fields count toward the guard: an
  `index`-only selector, an `enabled_only`-only selector, and a selector
  carrying both filters and no criterion are each rejected, while
  `enabled_only` still filters normally once a real criterion is named.
  (`selector::tests`): the empty-selector rejection (including a
  selector that sets only `index`), pre-order match paths, `index`
  selection and its out-of-range error, every single-field criterion,
  combined criteria, and a round-trip from every found path back to a
  matching node.
- **PINV-16** — split. The serde half — defaults for every enriched
  field, `enabled` defaulting to `true`, and an older `describe`
  response still deserializing — is fully covered by automated `cargo
  test -p polarize-core` (`ax::tests`). The native half is **not**
  automated anywhere: whether a real `AXUIElementCopyAttributeValue`
  call for `AXEnabled`/`AXSubrole`/`AXIdentifier`/`AXHelp` returns the
  value a real app publishes needs a live macOS session with
  Accessibility permission granted. The macOS code is type-checked
  against `aarch64-apple-darwin`, and CI compiles it on a real macOS
  runner, but neither runs it.
- **PINV-17** — fully covered by automated `cargo test -p polarize-core`
  (`action::tests`): an action the element does not publish, an element
  that publishes no action at all, a disabled element, the exact text of
  both refusal messages, and — in every refusal case — proof that the
  recording fake `ActionPerformer` was never called. The happy path,
  the default to `AXPress`, `index` selection, root-path selection, and
  both error paths out of `describe` and the performer are covered as
  well. The check is pure logic over an in-memory tree, so it needs no
  macOS session at all.
- **PINV-18** — split, and the untestable half is the important one.
  The app-substitution rule is fully covered by automated `cargo test -p
  polarize-core` (`action::tests`): a request naming no app reaches the
  performer as the name `describe` resolved, a request naming an app
  keeps the caller's own identifier including its bundle id, a resolved
  app publishing a bundle id reaches the performer as that bundle id
  rather than as a name, a resolved app without one falls back to its
  name, and an app publishing neither falls back to the frontmost app.
  What `polarize-macos` actually reads into `ResolvedApp` —
  `NSRunningApplication::bundleIdentifier` — is **not** automated; a
  human must confirm a real `describe` response carries the target app's
  real bundle id.
  The `polarize-core` half is covered by automated `cargo test -p
  polarize-core`: `selector::tests` proves every found path reads back
  to a matching node, and `action::tests` proves the recording fake
  `ActionPerformer` receives the exact path `find_one` resolved, with
  nothing rewriting it in between. The `polarize-macos` half is **not**
  automated anywhere. Whether `action::walk_path`'s
  `AxElement::children()` walk reaches the same real element that
  `accessibility::build_node` reported at that path needs a live macOS
  session with Accessibility permission granted, and a real app with a
  nested interface. A human must confirm it: run `describe`, pick a
  deeply nested control, run `perform_action` on it, and watch that
  exact control respond. The macOS code is type-checked against
  `aarch64-apple-darwin`, and CI compiles it on a real macOS runner,
  but neither runs it.
- **PINV-19** — fully covered by automated `cargo test -p polarize-core`
  (`wait::tests`), including the waiter-failure rules: a waiter that
  fails after consuming its budget is polled through to a real match and
  still times out on its own deadline, and a waiter that fails without
  consuming any budget ends the wait after a bounded number of tree
  reads. Both `await` tools are covered. The `polarize-macos` half of
  the degrade rule — that `observer.rs` really sleeps out its budget
  before reporting a failure — is **not** automated; see PINV-20.
  (`wait::tests`, 31 tests). A fake `AccessibilityInspector` returns a
  different tree on each call, a fake `UiChangeWaiter` records every
  budget it is handed, and a fake `Clock` advances only when the fake
  waiter says time passed, so no test sleeps. The tests cover: a match
  on the first read with no wait at all, a match after several polls, a
  match when the waiter never signals (the poll fallback), a wait that
  ends early when the waiter does signal, the exact budget sequence
  `[250, 250, 100]` for a 600 ms timeout at a 250 ms poll interval, a
  timeout with its message, a zero timeout that still reads the tree
  once, an empty selector that fails without waiting, an `index` that is
  not reached yet and keeps waiting, the idle window restarting on a
  change, an idle timeout, and every default and clamp. The real
  `SystemClock` is checked only for "starts near zero, never goes
  backwards"; it wraps `Instant`, which has nothing else to test.
- **PINV-20** — **not** automated anywhere, and it cannot be. Every
  claim in it is native behavior: `AXObserverCreate`,
  `AXObserverAddNotification`, `AXObserverGetRunLoopSource`,
  `CFRunLoopAddSource`, `CFRunLoopRunInMode`, and `CFRelease`. No CI
  runner can grant Accessibility permission or post a real `AXCreated`
  notification. The module type-checks clean against
  `aarch64-apple-darwin`, including the `AXObserverCallback` signature's
  shape, but a type-check cannot prove the signature matches Apple's
  header — a wrong one is undefined behavior that compiles. A human on a
  real macOS session with Accessibility permission granted must confirm,
  against both a native app and an app with a web view: an
  `await_ui_element` call returns as soon as the element appears rather
  than at the next poll (which proves notifications arrive at all); an
  `await_screen_idle` call reports idle while the app is quiet; the
  three notifications actually register on an application-level element;
  and a long run of repeated calls leaks no run-loop sources (check with
  Instruments, or watch the process's Mach port count).
- **PINV-21** — the mapping half is fully covered by automated `cargo
  test -p polarize-core` (`script::tests`): both Automation codes and
  their different permission states, the `-600`, `-1728`, and `-128`
  codes, an unknown code, stderr with no code at all, stderr whose last
  parentheses hold no number, empty stderr, a code read from the last
  line of several, and every `ScriptFailure`→`PolarizeError` arm.
  `perform_run_applescript` is tested end to end against a fake runner
  for the refusal case. The native half is **not** automated anywhere:
  whether `AEDeterminePermissionToAutomateTarget` returns `-1743`,
  `-1744`, or `0` for a real (caller, target) pair, and whether
  `osascript` prints these codes in the shape the parser expects, needs
  a real macOS session. A human must check three cases there: a target
  app that has never been approved, one the user refused in System
  Settings > Privacy & Security > Automation, and one that is approved.
  The `polarize-macos` code is type-checked against
  `aarch64-apple-darwin`. Nothing has confirmed that the
  `CoreServices` framework link resolves the three `AE*` symbols at
  link time, because `cargo check` does not link.
- **PINV-22** — fully covered by automated `cargo test -p polarize-core`
  (`script::tests`): a literal's contents removed, an escaped quote
  inside a literal, an unterminated literal failing closed, a long
  source flattened and cut, and a timeout error asserted not to contain
  the secret the script carried. What is **not** covered: whatever
  `osascript` itself chooses to echo of the script on stderr.
  `polarize` passes that text through unchanged, so a script that makes
  `osascript` quote a literal back can still leak it. Nobody has
  surveyed which `osascript` errors quote source text.
- **AppleScript subprocess runner** (`polarize-macos/src/applescript.rs`,
  no invariant number) — **not** automated in this repository. The
  process-control logic (write stdin on its own thread, read both
  output pipes on their own threads, poll for the exit, kill at the
  deadline) was copied out of the module and exercised as a plain Linux
  program on 2026-08-20: a stdin round trip, a 1 MB stdin, a 900 KB
  stdout, a killed `sleep 30` that reported `timed_out`, a child that
  exits before reading its stdin, captured stderr with a non-zero exit,
  and a missing program. That checks the logic, not this module. Nobody
  has run `osascript` or `sdef` from `polarize` on real macOS. A human
  must confirm: `osascript` reads a script from stdin and returns its
  output, a script that blocks on a modal dialog is killed at the
  deadline, and `sdef` prints a dictionary for a real app bundle path.

- **PINV-23** — split. The decision half is fully covered by automated
  `cargo test -p polarize-core` (`session::tests`): each single blocked
  fact, the both-facts-hold precedence, the usable case, and
  `ensure_session_usable` against a fake `SessionInspector`. The
  `error::tests` display cases cover both new error messages. The native
  half is **not** automated anywhere. Whether
  `CGSessionCopyCurrentDictionary` reports the flags this code expects,
  and whether the preflight fires before the native call, needs a real
  macOS session. A human must lock the screen and confirm `screenshot`
  returns the `ScreenLocked` error, then switch to a second user account
  with Fast User Switching and confirm the affected tools return the
  `SessionNotOnConsole` error. The macOS code is type-checked against
  `aarch64-apple-darwin`.
- **PINV-24** — split. The fail-open rule itself is fully covered by
  automated `cargo test -p polarize-core` (`session::tests`): an absent
  console flag, an absent lock flag, and both flags absent. What is
  **not** automated is the key reading in
  `crates/polarize-macos/src/session.rs`: that the literal strings
  `"kCGSSessionOnConsoleKey"` and `"CGSSessionScreenIsLocked"` match the
  keys macOS really publishes, and that their values really are
  `CFBoolean`. A wrong key or a wrong value type is silent — it reads as
  an absent key and reports a usable session. A human on a real macOS
  session must confirm the two keys read `true` while the Mac is
  unlocked and on the console, and that the lock key flips when the
  screen locks.
- **PINV-25** — fully covered by automated `cargo test -p polarize-core`
  (`process::tests`), with real subprocesses. The deciding case starts a
  backgrounded subshell that inherits stdout and outlives the kill; the
  test asserts the call returns inside the grace, reports
  `output_truncated`, and still returns the output written before the
  kill. This is why `process` lives in `polarize-core` and not in
  `polarize-macos`: the module holds no macOS API, and the failure it
  prevents is a hang, which only a real subprocess can demonstrate.
  What is **not** automated is `applescript.rs`'s thin adapter over it,
  or whether `osascript` in particular leaves a pipe holder behind.
- **PINV-37** — split. The flip itself is fully covered by automated
  `cargo test -p polarize-core` (`find_text::tests`): a full-frame box,
  a box on the bottom edge, a box on the top edge, all four corners in
  one table, an x-axis-never-moves case, a box reaching outside the unit
  square on both sides, a non-finite box, a mirrored-center case, and a
  table asserting every flipped center is a fraction
  `coords::fraction_to_pixel` accepts. What is **not** automated
  anywhere is the half that decides whether the flip runs in the right
  direction: that Vision really does report a bottom-left origin for
  `VNRecognizedTextObservation`, and that
  `crates/polarize-macos/src/vision.rs` copies that box through
  unchanged. No OCR has run in this environment, not once. A human on a
  real macOS session with Screen Recording granted must confirm it, and
  it is the single most important `find_text` check: call `find_text`
  for a word near the **top** of a window, then feed
  `matched.center_x`/`matched.center_y` straight into `tap` with the
  same `target`. The click must land on that word. A click near the
  bottom of the window means the flip runs the wrong way, and no test
  and no error message can show it.
- **PINV-38** — the whole ordering rule is fully covered by automated
  `cargo test -p polarize-core` (`find_text::tests`): a line below the
  floor never matching, the default floor applying when the request sets
  none, an out-of-range `min_confidence` and an empty request text both
  rejected before the platform is called, shuffled lines coming back in
  reading order, index `0` by default, an explicit index, an index past
  the last match, and the no-match error naming what the OCR did read.
  All of it runs against a fake `ScreenCapture` and a fake
  `TextRecognizer`, so it needs no macOS session. What is **not**
  automated is whether Vision's real observations, ordered by this rule,
  read the way a human reads the screen. A human must confirm that
  `index: 1` names the second match on a real screen with repeated text,
  and that the default confidence floor does not hide real UI text.
  `crates/polarize-macos/src/vision.rs` is type-checked against
  `aarch64-apple-darwin` and nothing more: no `VNRecognizeTextRequest`
  has ever run here. A human must also confirm the first call's roughly
  27 second model compile, and that later calls return in about 100 ms.
