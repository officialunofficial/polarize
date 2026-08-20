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
it has real `cargo test` coverage (546 tests as of this writing).

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

### PINV-2: every tool call is gated on at most one permission (decision logic)

- Always: `permission::required_permission` maps each `ToolKind` to at
  most one `PermissionKind`, and `permission::check_permission` refuses
  to run a tool whose required permission is not `Granted` (a permission
  absent from the caller's status list is treated as `NotDetermined`,
  never as implicitly granted). A tool that needs no TCC grant maps to
  `None` and always passes the check.
- Because: `polarize-macos`'s native calls fail in ways that are easy to
  misdiagnose from the raw OS error alone (a denied AX permission and a
  genuinely missing UI element can both surface as "element not found").
  Deciding, from a permission-status list, whether a tool may run — in
  one pure, testable place — is what makes that decision auditable
  independent of which real API `polarize-macos` happens to call to
  learn the status. The mapping must be able to say "none", because
  several tools really need nothing: `app_launch`, `app_quit`,
  `list_displays`, `frontmost_app`, `await_workspace_event`, and a
  clipboard *write* are all unprivileged. A table that could only name a
  permission would force each of those to claim a grant it never uses,
  and the table is read as the answer to "what does `polarize` need?".
  (Scope note: this function encodes the *decision*; see PINV-10 for how
  `polarize-macos`'s real tool implementations actually learn the
  permission status and gate on it today.)
- If violated: a caller sees a confusing native failure (or, worse, a
  `tap`/`keyboard` call that silently no-ops) instead of "grant
  Accessibility access to run this tool". Map a tool to a permission it
  does not use, and the table lies the other way: a caller grants
  something nothing needed, and an audit of what `polarize` asks for
  stops matching what it does.

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

### PINV-26: `set_value` checks the element before it writes

- Always: `set_value::set_element_value` resolves the selector to one
  node, then refuses in three cases. It refuses when the node's role is
  not on the payload's accepted-role list (`TEXT_VALUE_ROLES`,
  `NUMBER_VALUE_ROLES`, or `SELECTED_TEXT_RANGE_ROLES`). It refuses when
  the node's `enabled` flag is `false`. It refuses a number that is not
  finite. A refusal returns a `SetValueError` that names the element,
  and it never calls `ValueSetter::set_value_at_path`.
  `polarize-macos`'s `MacValueSetter` then asks the live element with
  `AXUIElementIsAttributeSettable`, and refuses a read-only attribute
  before it writes.
- Because: a wrong AX write is silent and hard to read back. A string
  written into an `AXStaticText` label, or a number written into a text
  field, comes back as a bare `kAXErrorIllegalArgument`. That code names
  no element, so a caller cannot tell a wrong target from a missing
  permission or a stale path. The tree `describe` already returned
  carries the role and the enabled flag, so the check costs no extra
  native call. The role list cannot be complete, because any app may
  publish a settable `AXValue` on any role. That is why the live
  `AXUIElementIsAttributeSettable` call stays as the second gate. The
  app itself is the only authority here. It knows about a read-only
  field, a locked document, and a secure text field.
- If violated: a caller writes to a label, or to a greyed-out control,
  and reads an error code with no element in it. Or `polarize` reports
  a successful write to an attribute the app never accepted.

### PINV-27: a `set_value` success means the app accepted the write

- Always: `set_value` reports `set: true` when
  `AXUIElementSetAttributeValue` returns `kAXErrorSuccess`. The tool
  claims nothing more than that. Three places state that limit:
  `polarize_core::set_value`'s module docs, `SetValueResponse::set`,
  and `ValueSetter::set_value_at_path`. Each one also names the house
  rule. Press a toggle with `perform_action`. Write text and numbers
  with `set_value`. Type with `keyboard` when the app must see every
  key event.
- Because: a native AppKit control treats an AX write like a user edit.
  Web content usually does not. A `WKWebView`, an Electron app, and a
  React app each accept the write into the DOM node. None of them fires
  an `input` or a `keydown` event. The page's own JavaScript never
  learns about the edit. A controlled React input shows the new text.
  Then it snaps back to the value in its state. A form stays invalid,
  and a submit button stays disabled. `polarize` cannot detect this.
  The app really did return success. `polarize` cannot repair it either.
- If violated: an agent reads `set: true`, believes the field holds the
  new text, and continues. The next step fails somewhere else, and the
  failure points at the wrong cause. The agent then retries the write
  that can never work, instead of typing the text with `keyboard`.
### PINV-28: a window tool checks its target before it writes

- Always: `window_control::select_window` resolves a request to exactly
  one window, or it returns a `WindowControlError`. A `window_title`
  that matches no window, a `window_title` that matches several windows
  with no `window_index`, an out-of-range `window_index`, and an app
  with no windows are all refusals. `window_control::plan_action_writes`
  adds one more: it refuses a full-screen action against a window whose
  `full_screen` is `None`, which means the window publishes no
  `AXFullScreen` attribute. No refusal ever reaches
  `WindowController::apply_window_writes`.
- Because: these writes are destructive, and the caller cannot undo
  them. A `close` throws away unsaved work. A move loses the window's
  old frame, which nothing recorded. So guessing which of two equally
  titled windows the caller meant is worse than refusing. `AXFullScreen`
  needs its own check for a different reason. The attribute is
  undocumented. A window that does not publish it still accepts the
  write, and then does nothing. To an agent that cannot see the screen,
  that is indistinguishable from success.
- If violated: `window_action` closes the wrong document window, or it
  reports a window went full screen while nothing on screen changed.

- Always, second: every write of one action crosses to
  `polarize-macos` in a single `apply_window_writes` call, and the
  implementation resolves the window index once for the whole plan. A
  write reorders the list — un-minimizing a window or raising it moves it
  to the front — so re-resolving between writes would send the rest of
  the plan to whatever moved into that slot. A `focus` on a minimized
  window plans three writes, which is exactly where this bites.

### PINV-29: a window tool reports the frame it re-read, never the frame it requested

- Always: `window_control::perform_set_window_frame` and
  `window_control::perform_window_action` call
  `WindowController::list_windows` again after their last write. Every
  geometric and boolean field of the response comes from that second
  read. `set_window_frame` reports the requested frame in its own
  separate fields, and compares the two into `applied_exactly`.
- Because: an app is free to ignore a write. Every AppKit window has a
  minimum size, many have a maximum, and a document window can refuse a
  move that would put its title bar under the menu bar.
  `AXUIElementSetAttributeValue` returns `kAXErrorSuccess` in all of
  those cases. The app took the message, then applied its own policy. A
  tool that echoed the request back would report a 200-pixel-wide window
  that is really 480 pixels wide. An agent cannot see the screen, so
  nothing catches that.
- If violated: an agent lays out three windows, believes the layout
  succeeded, and every later coordinate it computes from that belief is
  wrong.
### PINV-30: the window join matches on one app, then title, then frame, and never invents a match

- Always: `workspace::merge_window_lists` pairs an accessibility window
  with a window-server window only when both report the same
  `owner_pid`. Within one app it makes three passes in order: same title
  and same frame, then same title, then same frame. Each window-server
  window is claimed at most once. A window that no pass pairs stays in
  the result on its own, marked `AccessibilityOnly` or
  `WindowServerOnly`, with the fields the missing list would have
  supplied left as `None`.
- Because: the two lists disagree by design. A window can be missing
  from the accessibility half because the app publishes nothing for it,
  and missing from the window-server half because it sits on another
  Space. A caller must still see both. Neither key alone is unique
  either: a document app happily shows several windows titled
  "Untitled", and macOS hides `kCGWindowName` from a process without
  Screen Recording permission, which leaves title matching with nothing
  to work on. Dropping an unpaired window would silently hide real
  windows. Pairing on title alone would hand a caller the wrong
  `window_id` for the `screenshot` or `tap` call that follows.
- If violated: `list_windows` reports a durable `window_id` that belongs
  to a different window, or drops the very window the caller was looking
  for and reports success.

### PINV-31: `app_quit` asks politely unless the caller asks to force

- Always: `workspace::perform_app_quit` calls
  `AppLifecycle::request_terminate` with `force: false` unless the
  request sets `force: true`. An absent `force` field is `false`. The
  call never escalates to a force on its own, not after a timeout and
  not after a refused request. It reports `exited` from what the
  platform observed, never from the fact that it asked.
- Because: `terminate()` sends a quit Apple Event, so the app runs its
  own quit path. It can save open documents, and it can put up a "save
  changes?" dialog and stay running. `forceTerminate()` is `SIGKILL`
  with extra steps: unsaved work is gone, with no dialog and no undo. An
  automation tool that escalates by itself destroys a user's work on a
  schedule the user never agreed to. Reporting "quit" while the app
  still shows a save dialog is just as bad, because the caller moves on
  and the app is still there.
- If violated: an `app_quit` call silently discards unsaved documents,
  or a caller believes an app exited while a modal dialog holds it open,
  and every step that follows acts on the wrong app state.
### PINV-32: a hit test and a tap resolve one request to one pixel point

- Always: `hit_test::perform_hit_test` reads a request's `x`/`y`
  fraction the same way `orchestrate::perform_tap` reads it. It resolves
  the request's target through `WindowManager::resolve_target_rect`. It
  converts the fraction against that rect's `size` through
  `coords::fraction_to_pixel`. It adds that rect's `origin`. Only the
  resolved **global** display pixel point reaches `HitTester`, never the
  raw fraction. The `pixel_x`/`pixel_y` a hit test reports equal the
  point a tap of the same request clicks.
- Because: a caller uses the hit test to preflight the tap. It reads the
  element under a point, compares that element with the one it means to
  press, and then taps the same point. The comparison is only worth
  anything while both tools address one point. A difference between the
  two coordinate paths raises no error at all. It just approves a click
  on some other element. This is PINV-4's rule applied to a second tool,
  and it is why the conversion stays in `polarize-core`: a fake
  `WindowManager` and a fake `HitTester` prove the two paths agree
  without a real screen.
- Always, second: the hit test asks the **system-wide** accessibility
  element, never an application element.
  `AXUIElementCopyElementAtPosition` searches only inside the element it
  is called on. Asked on an app, it reports that app's own view under
  the point even when another app's window covers it — which is exactly
  the case this tool exists to detect.
- If violated: a caller confirms the right element, taps, and hits
  something else. Both tools report success. Scope the hit test to one
  app, and it approves every occluded click it was added to prevent.

### PINV-33: a hit test reports one element, never a subtree

- Always: `hit_test::perform_hit_test` clears the `children` list of the
  `AxNode` it reports. `polarize-macos`'s `leaf_node` never reads
  `AXChildren` either.
- Because: macOS returns the deepest element under the point. The
  children of that element do not cover the point, or the hit test would
  have returned one of them. A caller asked what is here, not what this
  contains. `describe` is the tool for a subtree, and one hit test on a
  web view would otherwise carry thousands of nodes.
- If violated: a preflight response grows to the size of a whole
  `describe` response, and a caller cannot tell which node answered its
  question.

### PINV-34: a refused clipboard read is a permission error, not empty text

- Always: `clipboard::classify_read` reports three outcomes apart. It
  reports `Ok(Some(text))` whenever the pasteboard hands over a value,
  and an empty string is such a value. It reports `Ok(None)` when the
  pasteboard does not list the requested type. It reports
  `PermissionError::NotGranted` with `PermissionKind::Clipboard` and
  `PermissionState::NotDetermined` when the pasteboard lists the type
  and still hands over no value.
- Because: macOS 26 protects pasteboard contents. It can withhold them
  from a programmatic read that no user paste gesture preceded.
  `NSPasteboard` signals that refusal by returning no string, which is
  exactly what an empty pasteboard returns. The two facts need different
  repairs. An empty clipboard needs a copy; a refusal needs the user.
  `polarize-macos` separates them by asking
  `availableTypeFromArray:` first, because the type list stays readable
  while the contents do not. The state stays `NotDetermined` for the
  same reason PINV-11 gives: the pasteboard offers no evidence that the
  user made an explicit choice. A write needs none of this, because
  macOS never refuses one.
- If violated: `clipboard_read` answers "the clipboard is empty" on a
  Mac whose clipboard holds real text. The caller copies again, reads
  nothing again, and never learns that only the user can repair it.
### PINV-35: a notification banner is found by structure, and a dismiss is proved by a re-read

- Always: `notifications::extract_banners` identifies a banner from the
  shape of the notification centre's accessibility tree — a container
  that holds prose text, usually with a close control beside it. A
  subrole, an identifier, or a role description only ever adds evidence.
  No string value is required for a banner to be found, so a tree shape
  this code has never seen still yields every banner it can identify.
  A control becomes the dismiss control only when it publishes an action
  **and** names itself: subrole `AXCloseButton`, or a label, identifier,
  help string, or subrole that holds "close", "dismiss", or "clear". A
  plain pressable button is never enough.
  `notifications::perform_dismiss_notification` reads the tree again
  after the press, and reports `dismissed` from that second read alone.
- Because: Apple renames banner subroles between macOS releases, and has
  restructured the banner hierarchy more than once. A matcher keyed on
  `"AXNotificationCenterBanner"` reports zero banners on the first macOS
  that renames it, and reports that as a normal empty result, which a
  caller cannot tell from a quiet Mac. Structure has stayed stable
  across every one of those changes. The named-control rule exists
  because a banner can carry a "Reply" button next to its close button:
  pressing the wrong one sends a message the caller never wrote. The
  re-read exists because `AXUIElementPerformAction` returns success for
  a press that changed nothing, and because a banner leaves the screen
  with an animation, so the first read after the press can still carry
  it.
- If violated: `describe_notifications` returns an empty list on a Mac
  that is showing a banner. Or `dismiss_notification` presses "Reply"
  on a message and calls it a dismiss. Or it reports `dismissed: true`
  for a banner still on screen, which is the one claim the tool exists
  to make honestly.

### PINV-36: a workspace wait watches two channels, and names the one that saw the event

- Always: `workspace_events::diff_snapshots` derives every workspace
  event a pair of snapshots can prove, and marks each one
  `WorkspaceEventSource::Poll`.
  `workspace_events::perform_await_workspace_event` reports an event
  from either the notification channel or the poll channel, and every
  event it returns names the channel that saw it.
  `WorkspaceEventKind::WillSleep` is the one kind a poll cannot produce,
  and `WorkspaceEventKind::is_poll_observable` says so. A waiter that
  reports an error ends the wait with that error, rather than looping
  on. `polarize_macos::workspace_events` runs one whole `NSWorkspace`
  observer lifecycle on one thread, exactly as PINV-20 requires of an
  `AXObserver`, and sleeps out its budget when nothing arrives.
- Because: `NSWorkspace` delivers its notifications through a run loop.
  `apps/polarize` runs `tokio` on its main thread, so no `CFRunLoop`
  runs there. Apple does not document which run loop the
  distributed-notification port is scheduled on, so nobody yet knows
  whether these notifications reach this process at all. A wait built on
  notifications alone would answer "nothing happened" while an app
  really did come to the front. A poll of
  `NSWorkspace.frontmostApplication` and the login-session flags proves
  the same facts with no run loop, so the feature works either way, and
  the source field tells the first human on real macOS which channel
  works. The waiter is also the only thing that makes a wait slice take
  time: carrying on after it fails would re-read the workspace as fast
  as the CPU allows.
- If violated: `await_workspace_event` times out on a Mac where the
  event really happened, and nothing in the response explains it. Or a
  `will_sleep` result implies a poll can see a sleep coming, which it
  cannot. Or a failed observer turns the tool into a busy loop that
  pins a core for the whole timeout.
- Scope note: `polarize` reports a workspace event only while an
  `await_workspace_event` call is running. It has no event stream. An
  `rmcp` stdio server answers discrete tool calls and has nowhere to
  push an asynchronous stream, so the tool call itself is the delivery
  point. An event that happens between two calls is not reported, and
  the tool's own documentation says so rather than implying a complete
  history.
- Scope note: neither workspace tool preflights the login session
  (PINV-23), and neither needs a TCC permission. Both read
  `NSWorkspace`, which captures no pixels, reads no accessibility tree,
  and posts no input. Refusing to run off the console would also break
  the one tool that reports the console: `frontmost_app` returns
  `on_console` as a field, and `await_workspace_event` exists partly to
  report a Fast User Switch. This is the same reasoning PINV-23's
  exclusion note gives for the two AppleScript tools.

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
- **PINV-26** — split. The core half is fully covered by automated
  `cargo test -p polarize-core` (`set_value::tests`): a text write to a
  label, a text write to a button, a number write to a text field, a
  range write to a slider, a disabled element, and a non-finite number
  are each refused. Each refusal test asserts the fake `ValueSetter` saw
  no call. The happy paths assert the exact path and the exact typed
  write that reached the fake. The native half is **not** automated
  anywhere. Whether `AXUIElementIsAttributeSettable` really answers for
  a live element, and whether the role lists match what real apps
  publish, needs a real macOS session with Accessibility permission
  granted. A human must confirm four things against a real app. First, a
  text write into a `TextEdit` document or a Safari address field lands.
  Second, a number write moves a real slider without a drag. Third, a
  range write moves the caret of a real text view. Fourth, a write to a
  field the app publishes as read-only returns the settability refusal,
  not a bare AX error. The macOS code is type-checked against
  `aarch64-apple-darwin`.
- **PINV-27** — **not** automated anywhere, and it cannot be. The
  invariant is about what a real app does after a real write, so no
  fake can demonstrate it. `polarize-core` covers only the claim itself:
  `SetValueResponse::set` is documented as "the app accepted the write",
  and the module docs carry the house rule. A human on a real macOS
  session must confirm the failure mode, so that the documentation
  stays honest. Write text into a controlled React input. A search box
  of a web app in Safari, or in an Electron app, is a good target.
  Confirm three things: the text appears, the page's own handlers do not
  run, and the value can snap back. Then confirm the `keyboard` tool
  types the same text into the same field.
- **PINV-28** — the decision half is fully covered by automated `cargo
  test -p polarize-core` (`window_control::tests`): the frontmost-window
  default, an index with no title, a title that names one window, a
  title that matches nothing (asserting the message lists the real
  titles, `<untitled>` included), a title that matches two windows,
  an index that disambiguates those two, an index past the matching
  windows, an index past the whole list, an app with no windows, a
  prefix that must not match, an untitled window addressed by index,
  both full-screen actions refused on a window that publishes no
  `AXFullScreen`, and — for the refusal cases — proof that the recording
  fake `WindowController` was never written to. What is **not**
  automated: whether a real app's `AXWindows` list is really published
  front to back, so index `0` is really the frontmost window; and
  whether a real AppKit window really publishes `AXFullScreen`, which is
  an undocumented attribute with no Apple guarantee behind it. A human
  on a real macOS session with Accessibility permission granted must
  confirm both. Open two windows of one app, and check that a
  `window_action` with no `window_title` acts on the front one. Then run
  a full-screen action against a normal document window, against a
  utility panel, and against a window with no full-screen button, and
  confirm the last two return the refusal rather than a silent success.
- **PINV-29** — the orchestration half is fully covered by automated
  `cargo test -p polarize-core` (`window_control::tests`) against a fake
  `WindowController` that returns a *different* window list on its
  second read, so an echoed request cannot pass: a clamped width
  reported as the real 480 pixels with `applied_exactly: false`, a
  frame the app honored reported as `true`, a sub-pixel rounding
  difference still counted as exact, a `window_action` whose write the
  app ignored, a `close` that removed the window reported as no window,
  a `close` the app refused still reporting the window, a re-read that
  follows a reordered `AXWindows` list by title rather than by index,
  and a count of exactly two `list_windows` calls per tool call. The
  half this invariant depends on but cannot verify is **not** automated
  anywhere: whether `polarize-macos`'s real
  `AXUIElementSetAttributeValue` calls for `AXPosition`, `AXSize`,
  `AXMinimized`, `AXMain`, and `AXFullScreen` move a real window, and
  whether a real re-read reports the app's clamping. A human on a real
  macOS session with Accessibility permission granted must confirm:
  `set_window_frame` with `{"size":{"width":0.1,"height":0.1}}` against
  an app with a large minimum window size returns `applied_exactly:
  false` and the app's real minimum size; `AXRaise` brings a background
  window forward; `AXPress` on `AXCloseButton` closes a document window,
  and reports the window as still open when a "save changes?" sheet
  appears. Nobody has checked whether the position-size-position write
  order in `window_control::plan_frame_writes` really defeats a given
  app's clamping — the order is asserted by a unit test, its effect is
  not. The macOS code is type-checked against `aarch64-apple-darwin`
  (`cargo clippy --target aarch64-apple-darwin -D warnings`, clean), but
  nothing runs it.
- **PINV-30** — fully covered by automated `cargo test -p polarize-core`
  (`workspace::tests`): a window both lists report, a window only the
  accessibility tree reports, a window only the window server reports,
  two empty lists, two windows of different apps that share a title, two
  windows of one app that share a title and are listed in opposite
  orders, a title match when the frames differ, a frame match when the
  titles differ, three accessibility windows against two identical
  window-server windows (which proves each one is claimed at most once),
  a sub-pixel frame difference, a frame difference wider than the
  tolerance, an empty title read as an absent title, and the output
  order. The join is pure logic over two in-memory lists, so it needs no
  macOS session at all. What is **not** automated is the reading that
  feeds it: whether `kAXWindowsAttribute` and
  `CGWindowListCopyWindowInfo` really report the same window's frame in
  the same coordinate space, within the one-pixel tolerance
  `FRAME_TOLERANCE_PX` allows. A human on a real macOS session must
  confirm that, and must check the case the tolerance exists for: run
  `list_windows` against an app with two windows of the same title and
  confirm each `window_id` belongs to the window the record describes.
- **PINV-31** — the decision half is fully covered by automated `cargo
  test -p polarize-core` (`workspace::tests`): the default polite
  request, a forced request only when asked, no escalation after a
  timeout, an app that exits, an app that does not, an app that is not
  running, the exact budget sequence for a 250 ms timeout, a zero
  timeout that still checks once, a clamped timeout, and a refused
  request. The native half is **not** automated anywhere. Whether
  `NSRunningApplication::terminate()` really lets an app save its
  documents, whether `forceTerminate()` really kills it, and whether
  `isTerminated()` flips when it exits, all need a real macOS session. A
  human must check three cases against a real app: quit an app with no
  unsaved work and confirm `exited: true`; quit an app holding an
  unsaved document and confirm the save dialog appears, the app stays
  running, and the response reports `exited: false` rather than success;
  then repeat with `force: true` and confirm the app dies with no
  dialog.
- **Workspace tool permissions** (`polarize-core/src/permission.rs`,
  `required_permission`, no invariant number) — the mapping is
  fully covered by automated `cargo test -p polarize-core`
  (`permission::tests`): `list_windows` needs
  Accessibility, the other three need nothing, a permission-free tool
  passes even when every permission is denied, and `list_windows` is not
  satisfied by a granted Screen Recording status. What is **not**
  automated is the native side: that `MacWorkspace::open_app`,
  `request_terminate`, `sleep_until_exit`, `window_server_windows`, and
  `displays` really do work with no TCC grant at all, and that
  `MacWorkspace::accessibility_windows` really refuses without
  Accessibility. A human must confirm both halves on a real macOS
  session, with Accessibility revoked and then granted.
- **Workspace native bindings** (`polarize-macos/src/workspace.rs`, no
  invariant number) — **not** automated anywhere. The module
  type-checks and lints clean against `aarch64-apple-darwin`, and
  nothing more. A human on a real macOS session must confirm: the
  `kCGWindow*` dictionary keys read the values this code expects, and
  `CGRectMakeWithDictionaryRepresentation` unpacks `kCGWindowBounds`;
  `kCGWindowIsOnscreen` really flips when a window moves to another
  Space, and reads as absent rather than `false` there;
  `launchApplication:` opens an app named only by display name, and
  `openApplicationAtURL:configuration:completionHandler:` with a `None`
  handler opens one named by bundle id; and
  `CGDisplayModeGetPixelWidth`/`CGDisplayModeGetWidth` give `2.0` on a
  Retina display and `1.0` on a standard one. The `list_displays`
  frames must be checked against a `screenshot` of the same display, on
  a two-monitor setup, because that is the whole point of reporting
  them.
- **PINV-32** — the coordinate half is fully covered by automated
  `cargo test -p polarize-core` (`hit_test::tests`). One test runs
  `perform_hit_test` and `perform_tap` over the same request, against
  fakes with a non-zero target origin, and asserts the point the fake
  `HitTester` received equals the point the fake `InputSynthesizer`
  clicked, at four fractions. Other tests cover the origin addition, the
  target defaulting, and the rule that an out-of-range fraction never
  reaches the platform. What is **not** automated: whether
  `AXUIElementCopyElementAtPosition` reads the same global pixel space
  `CGEvent` posts into on a real screen. A human on a real macOS session
  with Accessibility permission granted must confirm that a hit test and
  a tap of one request address the same element, on a window that does
  not sit at the screen origin, and on a second display.
- **PINV-33** — the `polarize-core` half is fully covered by automated
  `cargo test -p polarize-core` (`hit_test::tests`): a fake `HitTester`
  returns a node with two children, and the response carries none. The
  `polarize-macos` half is compile-checked only: `leaf_node` in
  `crates/polarize-macos/src/hit_test.rs` reads no `AXChildren`, which a
  reader can confirm, but no test runs it.
- **PINV-34** — the classification is fully covered by automated
  `cargo test -p polarize-core` (`clipboard::tests`): text present, an
  absent type, a declared type with no value, an empty string, a value
  with no declared type, and the same three cases through
  `perform_clipboard_read`. What is **not** automated, and cannot be:
  that macOS really answers `availableTypeFromArray:` while it withholds
  `stringForType:`. That is the whole premise of the rule, and only a
  real macOS 26 session can confirm it. A human must copy text in
  another app, call `clipboard_read` from `polarize` with no preceding
  paste gesture, and check that the result is either the text or a
  `Clipboard` permission error — never an empty answer. The same human
  must confirm `clipboard_write` replaces the pasteboard contents, and
  that a following Command+V pastes the written text.
- **PINV-35** — split, and the untestable half is the tree shapes
  themselves. The extraction rules are fully covered by automated `cargo
  test -p polarize-core` (`notifications::tests`, 37 tests): today's
  banner shape, a plausible future shape with every subrole renamed and
  the text split across two sub-groups, a banner whose role this code
  has never seen, a banner with no close control at all, a "Reply"
  button proved not to be a dismiss control, action-button text kept out
  of the body, the one/two/many text readings, an identifier hint
  holding two sub-groups together, and a path round trip back through
  `selector::node_at_path`. The dismiss rules are covered too: the
  pressed path and action, the notification centre addressed rather than
  the frontmost app, a banner that stays put reported as
  `dismissed: false`, a re-read that succeeds on the third try, two
  identical banners counted rather than matched by text, a refusal that
  presses nothing, and every filter and clamp. All of it runs against
  hand-built trees, so it needs no macOS session.
  What is **not** automated, and cannot be: whether any of those tree
  shapes matches what
  `com.apple.notificationcenterui` really publishes. **Every banner tree
  in these tests is an informed guess.** Nobody has recorded a real
  one. A human on a real macOS session with Accessibility permission
  granted must: post a notification (`osascript -e 'display
  notification "body" with title "title"'`), run `describe` against
  `com.apple.notificationcenterui` and read the raw tree, then run
  `describe_notifications` and compare the two. Then run
  `dismiss_notification` and watch the banner leave the screen while the
  response reports `dismissed: true`. Repeat on a banner that carries
  action buttons, such as a Messages notification, and confirm the tool
  presses the close control and not "Reply". Repeat on the notification
  centre panel opened from the menu bar, which is a different shape
  again. The macOS code is type-checked against `aarch64-apple-darwin`,
  and `polarize-macos/src/notifications.rs` holds nothing but wiring and
  one error message.
- **PINV-36** — split. The whole decision half is fully covered by
  automated `cargo test -p polarize-core` (`workspace_events::tests`, 32
  tests): the notification-name table in both directions, an unknown
  name, the rule that only `WillSleep` is not poll-observable, every
  snapshot difference (a new frontmost app, a renamed app with one
  bundle id proved *not* to be an activation, the console lost and
  regained, a wake read from a wall-clock jump, a small clock correction
  proved not to be a wake, three events in one step), and the whole wait
  policy (a notification ending the wait at once, a poll finding what
  the notification channel missed, a notification with no app filled in
  from the snapshot, the app filter narrowing an activation but leaving
  the other kinds alone, the exact budget sequence `[250, 250, 100]` for
  a 600 ms timeout, a zero timeout, an empty kind list refused before
  any wait, and a waiter failure ending the wait after exactly one
  call). A fake clock advances only when the fake waiter says time
  passed, so no test sleeps.
  What is **not** automated is every native call, and one of them is a
  real open question. A human on a real macOS session must check, in
  this order:
  1. **Whether `NSWorkspace` notifications arrive at all.** Run
     `await_workspace_event`, switch to another app, and read
     `event.source`. `notification` means
     `polarize-macos/src/workspace_events.rs` works. `poll` means the
     notification port is not scheduled on the observer thread's run
     loop, and the poll channel is carrying the whole feature. Either
     answer is useful; nobody knows which one is true today.
  2. **Whether `willSleep` ever fires.** It is the one event the poll
     channel cannot cover. Start a wait, then close the lid or run
     `pmset sleepnow`. If it never arrives, and step 1 said `poll`,
     then `WillSleep` is dead weight and should be removed from
     `WorkspaceEventKind::ALL` rather than left as a promise.
  3. **That a wake is reported.** Wake the Mac during a long wait and
     confirm a `did_wake` event, from either channel.
  4. **That a Fast User Switch is reported.** Switch to a second user
     account and back.
  5. **That nothing leaks.** Run a few hundred waits and watch the
     process's Mach port count. One thread and one observer object per
     slice must both go away.
  6. **That `NSWorkspace.frontmostApplication` is safe off the main
     thread.** `polarize` reads it from a `tokio` blocking thread and
     from the observer thread. Apple documents `NSWorkspace` as
     thread-safe, but nothing here has confirmed it.
  The macOS code is type-checked against `aarch64-apple-darwin`,
  including the `define_class!` observer and its selector, and `cargo
  clippy --target aarch64-apple-darwin -- -D warnings` is clean. A
  type-check cannot prove the selector name in `sel!` matches the method
  the macro defined, nor that `addObserver:selector:name:object:`
  accepts it. Both are runtime facts.

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
