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

`polarize-core` is fully unit-tested: it is pure, platform-agnostic
logic (coordinate normalization, the accessibility-tree data model, MCP
tool schemas, error types, the permission-state enum, and the trait
definitions `polarize-macos` implements), and every invariant that lives
in it has real `cargo test` coverage (55 tests as of this writing).

`polarize-macos` implements those traits with real native calls
(`ScreenCaptureKit`, `AXUIElement`, `CGEvent`, AppKit) that **cannot**
be automatically tested anywhere — not in this repository, not in CI.
No CI runner can grant Screen Recording or Accessibility TCC
permission, or verify pixel/AX content, headlessly. Exercising this
crate for real requires a real macOS session with Screen Recording and
Accessibility permissions granted to whatever process runs it, and a
human (or a scripted local run) driving it interactively. Where
`polarize-macos` factors a piece of genuinely pure logic out of its
native calls (`app_lookup`, `keymap`, `geometry`), that piece *is*
covered by real `cargo test` runs (24 tests as of this writing) —
everything else in the crate is compile/link-checked only.

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

- Always: `ax::flatten` visits a node before any of its descendants,
  visits a node's children in their original order, and records each
  node's depth as the number of ancestors above it (the root is depth
  `0`). `ax::format_tree` renders that flattened sequence one line per
  node, indented two spaces per depth level.
- Because: the `describe` tool's consumer (an agent picking a tap
  target) relies on depth to render indentation and on pre-order to read
  a subtree as a contiguous run — both silently break if a traversal bug
  reorders children or miscounts depth, without ever producing an error.
- If violated: `describe` output renders as a flat or mis-indented list,
  so the agent using it cannot tell which elements nest inside which
  container.

### PINV-4: a tap's fraction is normalized before the platform ever sees it

- Always: `orchestrate::perform_tap` converts the request's `x`/`y`
  fraction to a pixel point via `coords::fraction_to_pixel`, resolved
  against the real size of the requested target (from `WindowManager`),
  and only calls `InputSynthesizer::click_at_pixel` with that
  already-resolved pixel point — never with the raw fraction. An
  out-of-range fraction returns a `CoordError` (PINV-1) before
  `InputSynthesizer` is invoked at all.
- Because: `InputSynthesizer` implementations are real `CGEvent` calls
  that cannot be exercised in CI. Pushing the fraction-to-pixel decision
  into this pure, testable function is what lets a fake implementation
  prove the platform layer receives correct pixel coordinates, and is
  never invoked on invalid input, without ever running on a real screen.
- If violated: a `tap` request appears to succeed but clicks the wrong
  point — either because the fraction leaked through unconverted, or
  because it was normalized against the wrong target's size.

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
  `height` are all clamped into `0.0..=1.0`, even when the input pixel
  rectangle falls partly or fully outside `screen_size` (and even when
  `screen_size` itself is non-positive, which is guarded against
  dividing by zero).
- Because: unlike a `tap` fraction (PINV-1, which must error loudly on
  bad input to catch a caller's coordinate-space mistake), a `describe`
  response is built from real AX geometry the caller never supplied —
  an off-screen element is a legitimate, common case (multi-monitor
  setups, partially dragged-off windows), not a caller error, so it must
  degrade to a best-effort frame instead of vanishing from the tree or
  propagating an error that would blank out an entire subtree.
- If violated: either `describe` panics/errors on the first off-screen
  element it meets (multi-monitor setups become unusable), or it passes
  an out-of-range frame through to callers who trusted the `[0,1]`
  contract every other normalized frame in the response honors.

### PINV-9: a `screenshot` response is a self-contained base64 PNG, never a file path

- Always: `ScreenshotResponse` carries the captured PNG as a base64
  string (`png_base64`) in the tool response body itself.
  `ScreenshotResponse::from_png_bytes` is the only constructor
  `orchestrate::perform_screenshot` uses, and it always base64-encodes;
  nothing in `polarize-core` or `polarize-macos` writes a screenshot to
  a path and returns that path instead.
- Because: `polarize` is a stdio MCP server whose client is often a
  separate, possibly sandboxed process with no shared filesystem
  namespace guarantee, and MCP's own image content type already expects
  base64-encoded bytes inline. A file path would need a second,
  out-of-band contract (where the file lives, who deletes it, whether
  the client can read it before/after the server exits) that base64 in
  the response avoids entirely — every tool response stays
  self-contained. (`polarize-macos`'s `capture_and_encode` does use a
  uniquely-named temp file internally, because `ScreenCaptureKit`'s PNG
  export is file-based, not buffer-based — but it reads the bytes back
  and deletes the file before returning, so that detail never crosses
  the `polarize-core` API boundary.)
- If violated: a screenshot response either silently fails against a
  sandboxed MCP client that cannot resolve a returned path, or leaks a
  temp file the caller never asked for and nothing ever cleans up.

### PINV-10: `describe`/`tap`/`keyboard` preflight the real Accessibility permission before any native call; `screenshot` does not yet

- Always: `MacAccessibilityInspector::describe` checks
  `AXIsProcessTrusted()` and `MacInputSynthesizer`'s `click_at_pixel`/
  `type_text`/`press_key` each check `CGPreflightPostEventAccess()` —
  in every case, before making any further native call — and return
  `PolarizeError::Permission` when the check fails, rather than letting
  the underlying `AXUIElement`/`CGEvent` call run and fail on its own
  terms. `MacScreenCapture`'s `capture_screen`/`capture_window` have no
  equivalent `CGPreflightScreenCaptureAccess`-style check today; a
  missing Screen Recording grant surfaces as whatever raw error
  `SCScreenshotManager` itself returns.
- Because: without this preflight, a denied or not-yet-granted
  permission and a genuinely missing UI element/app can both surface as
  an opaque native failure, and a caller has no reliable way to tell
  "grant Accessibility" apart from "your app/window identifier is
  wrong". The preflight turns the common case into a clean, typed
  `PolarizeError::Permission` before any ambiguous native error has a
  chance to occur.
- If violated: for `describe`/`tap`/`keyboard`, a permission problem
  would surface as a confusing native error instead of an actionable
  one. For `screenshot`, this is already the current, documented state
  — not a hypothetical: see `apps/polarize/src/server.rs`'s module doc
  comment for the open gap.

### PINV-11: a native permission preflight failure always reports `NotDetermined`, never falsely `Denied`

- Always: when `AXIsProcessTrusted()` or `CGPreflightPostEventAccess()`
  returns `false`, `polarize-macos` reports `PermissionState::NotDetermined`
  — it never reports `PermissionState::Denied` on the strength of that
  boolean alone.
- Because: both of those APIs collapse "the user was never asked" and
  "the user explicitly denied it" into the same `false` return value;
  `polarize-macos` cannot distinguish the two from the boolean alone.
  Reporting `Denied` would claim the user made an explicit choice that
  `polarize` has no evidence for, which is a stronger (and potentially
  wrong) claim than the API actually supports; `NotDetermined` is the
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
- Because: real-world AX trees are inconsistent — plenty of elements
  legitimately lack a title, or a third-party app's AX implementation
  returns an error for an attribute Apple's own apps always supply.
  Treating any one such gap as fatal would make `describe` unusable
  against exactly the less-polished apps where an agent most needs
  accessibility introspection to work.
- If violated: `describe` would fail outright (or silently truncate the
  whole tree) the moment it meets one AX element with an unreadable
  attribute, instead of describing everything it successfully could.

### PINV-13: an AX tree walk is capped at `MAX_AX_DEPTH`; deeper subtrees are truncated, not errored

- Always: `accessibility::build_node` stops reading `AXChildren` once
  `depth >= MAX_AX_DEPTH` (`64`) — the node itself is still built and
  included in the tree (role, label, frame, flags), but its `children`
  field is forced to an empty `Vec` rather than recursing further. No
  error is raised and no earlier ancestor is affected.
- Because: real AX trees are supposed to be finite and shallow, but a
  misbehaving or adversarial app could in principle expose a very deep,
  or effectively cyclic, tree (some accessibility proxies re-expose a
  wrapped element under a new node). Without a cap, walking such a tree
  would hang or exhaust memory instead of returning a (merely
  incomplete) result.
- If violated: a single pathological app's AX tree could hang or crash
  every future `describe` call, not just fail to fully describe that
  one app.

## Known gap this document does not paper over

`WindowManager::resolve_target_size` (`crates/polarize-macos/src/window.rs`)
returns only a `PixelSize` — no origin — for an `App`/`Window`-scoped
target. `perform_tap` (PINV-4) normalizes a tap fraction against that
size and hands the result to `click_at_pixel`, whose contract requires a
pixel point in the **global** display coordinate space. For
`Screen { display_id: None }` (global origin `(0, 0)`) this is exactly
correct. For a non-primary display, or an `App`/`Window` target whose
window does not start at the global origin, the resulting pixel point
is window/display-relative, not global — a real, open gap between this
trait shape and `click_at_pixel`'s documented contract, not an
invariant that currently holds. It is not listed above as a PINV because
an invariant states a property that *does* hold; this one, for those two
target shapes, does not. Fixing it needs either a `PixelSize` →
`PixelRect` change in `polarize-core` or a trait shape that passes the
origin through to `InputSynthesizer` directly. See `window.rs`'s own
"Known limitation" doc comment.

## Enforcement checklist

- **PINV-1** — fully covered by automated `cargo test -p polarize-core`
  (`coords::tests`): table-driven cases across both corners, the center,
  and out-of-range fractions/pixels/sizes on both conversion directions,
  plus a round-trip stability case.
- **PINV-2** — the decision logic itself is fully covered by automated
  `cargo test -p polarize-core` (`permission::tests`): every
  `ToolKind`→`PermissionKind` mapping, the
  granted/denied/not-determined/restricted usability rule, and the
  absent-status-means-not-determined rule. Note: `required_permission`/
  `check_permission` are not currently called from `polarize-macos` or
  `apps/polarize` — the real tool implementations gate permission a
  different way (see PINV-10). That real-world gating path is native-only
  and has **not** been automatically tested anywhere; it was exercised
  once, manually, during `apps/polarize`'s implementation phase by
  driving the built binary over stdio with no permission granted (not
  repeatable in CI, and not re-verified since).
- **PINV-3** — fully covered by automated `cargo test -p polarize-core`
  (`ax::tests`): depth-zero single node, pre-order traversal with
  correct depths across a multi-level tree, children-order preservation,
  and exact `format_tree` output for a hand-built tree.
- **PINV-4** — the orchestration half (fraction→pixel conversion, dispatch,
  and the "never call the platform on bad input" rule) is fully covered
  by automated `cargo test -p polarize-core` (`orchestrate::tests`)
  against a fake `WindowManager`/`InputSynthesizer`. The half this
  invariant depends on but cannot itself verify — that `polarize-macos`'s
  real `CGEvent` call actually lands the click at the given pixel point
  on a real screen — is **not** automated anywhere. Verifying that
  requires a real macOS session with Accessibility permission granted,
  driving the real `tap` tool interactively and observing the result;
  see also the window-scoped-target gap noted above.
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
  clamping to `1.0`, an oversized dimension clamping to `1.0`, a
  non-positive `screen_size` not dividing by zero, and the
  best-effort/clamped-fallback pair.
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
  `SCScreenshotManager`). It was manually exercised once, during
  `apps/polarize`'s implementation phase, by driving the built binary
  over real MCP stdio traffic with no permission granted: `describe`/
  `tap`/`keyboard` each returned a clean, structured
  `PolarizeError::Permission` before any further native call; `screenshot`
  returned `ScreenCaptureKit`'s own raw, unstructured error, confirming
  the documented gap. That single manual run is not a substitute for
  automated coverage and has not been re-verified since — needs a real
  macOS session with (and, to fully exercise the negative path, without)
  Screen Recording and Accessibility permissions granted.
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
