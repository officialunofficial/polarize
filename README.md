# polarize

`polarize` is a Rust [MCP](https://modelcontextprotocol.io) (Model
Context Protocol) server that automates real, native macOS AppKit
applications: screenshot capture, accessibility-tree inspection, and
synthetic mouse/keyboard input.

## Status

The workspace builds, lints, and tests cleanly. Native-API behavior
cannot be exercised in CI — see [`docs/INVARIANTS.md`](docs/INVARIANTS.md)
for exactly what's live-verified versus compile-checked only, per
tool.

## Tools

`polarize` exposes 25 MCP tools, in six families:

1. **`screenshot`** — capture a window or the whole screen to PNG,
   optionally scoped by a bundle id or app name.
2. **`describe`** — walk the `AXUIElement` accessibility tree for the
   frontmost (or a named) app, returning each element's role, label or
   title, normalized `[0, 1]` frame, and focusable/interactive flags,
   plus a ready-to-read indented text rendering of the whole tree.
3. **`tap`** — post a synthetic mouse click via `CGEvent` at a screen
   position. Coordinates are normalized `[0, 1]` fractions of the
   target screen or window's width/height, not raw pixels.
4. **`keyboard`** — post synthetic key events via `CGEvent`: type a
   string, or press a named key. Naming a `target` app posts the keys
   to that app by pid, without stealing focus. When no pid resolves, it
   activates the target first.
5. **`perform_action`** — press one element through its own
   `AXUIElementPerformAction` action, naming the element by identifier,
   role, subrole, or label instead of by coordinate. This reaches an
   occluded element and an element below click-target size, neither of
   which `tap` can hit. It refuses an action the element does not
   publish, and refuses a disabled control, before it calls the
   platform.
6. **`await_ui_element`** — block until an element appears, instead of
   polling `describe` in a loop. It wakes on an `AXObserver`
   notification and re-reads the tree every poll interval regardless,
   because some accessibility trees never post one.
7. **`await_screen_idle`** — block until an app's accessibility tree
   stops changing for a given window. Use it after an action that
   starts an animation or a load, when there is no single element to
   wait for.
8. **`run_applescript`** — run AppleScript source through `osascript`,
   optionally wrapped in a `tell application` block. This reaches
   scriptable apps such as Finder, Mail, Safari, Music, and Notes with
   semantic operations no accessibility or `CGEvent` call can express.
9. **`script_dictionary`** — list a scriptable app's own verbs and
   classes, read from its `sdef` dictionary. Call it before
   `run_applescript` to find out what an app accepts.
10. **`set_value`** — write one accessibility attribute directly: text, a
    number, or a selected-text range. This avoids the focus race and the
    keyboard-layout dependence of simulated typing. Read PINV-27 first:
    web content often takes the write without firing `input` or
    `keydown`, so a controlled component can update and then snap back.
11. **`hit_test_at_point`** — report the element that really sits under a
    point. It asks the system-wide accessibility element, so another
    app's window counts as occlusion. It resolves the same fraction
    `tap` resolves, so it preflights a click.
12. **`set_window_frame`** — move and resize one window. The response
    reports the frame the window really got, re-read after the write.
    Apps clamp their own minimum and maximum size.
13. **`window_action`** — minimize, restore, focus, close, or full-screen
    one window.
14. **`list_windows`** — list an app's windows, merging the accessibility
    list with the window server's for durable window ids and an
    on-screen flag.
15. **`app_launch`** — start an app, or report it was already running.
16. **`app_quit`** — ask an app to exit. It asks politely unless the
    caller asks to force, because a forced quit discards unsaved work.
17. **`list_displays`** — report every active display, in the same pixel
    space `screenshot` and `tap` use.
18. **`find_text`** — find on-screen text with Vision OCR, for apps whose
    accessibility tree is sparse, missing, or wrong. Each match carries a
    frame a caller can hand straight to `tap`. The first call after an OS
    update takes about 27 seconds, while macOS compiles the model.
19. **`clipboard_read`** — read the pasteboard. A refused read reports a
    permission error, never empty text.
20. **`clipboard_write`** — replace the pasteboard contents.
21. **`describe_notifications`** — read every notification banner on
    screen: its app, title, body, and frame.
22. **`dismiss_notification`** — close one banner, then re-read to report
    whether it really went away.
23. **`frontmost_app`** — report the app that holds focus now.
24. **`await_workspace_event`** — wait for an app switch, a wake, or a
    session change.
25. **`record_flow`** — record real mouse and keyboard input for a
    bounded window, for flow replay. The tap listens only: it never
    modifies or swallows an event. Typed characters are withheld unless
    the caller opts in, because a recording captures real keystrokes and
    those include passwords.

## Permissions

`polarize` drives the real macOS UI. Whatever process runs it (your
terminal, your MCP client, or a wrapper binary) needs:

- **Screen Recording** — required for `screenshot` and `find_text`.
- **Accessibility** — required for `describe`, `tap`, `keyboard`,
  `perform_action`, `await_ui_element`, `await_screen_idle`,
  `set_value`, `hit_test_at_point`, `set_window_frame`, `window_action`,
  `list_windows`, `describe_notifications`, and `dismiss_notification`.
- **Automation** — required for `run_applescript` and
  `script_dictionary`. macOS asks per target app, the first time a
  script addresses one.
- **Input Monitoring** — required for `record_flow`, and for nothing
  else. This is a separate grant from Accessibility: posting an event
  and listening to one are different privileges.

`app_launch`, `app_quit`, `list_displays`, `frontmost_app`,
`await_workspace_event`, and `clipboard_write` need no grant at all.
`clipboard_read` can be refused by macOS, and reports that as a
permission error rather than as empty text.

Grant them under System Settings → Privacy & Security.

Every tool that captures pixels, reads the accessibility tree, or posts
input also checks the login session first. A locked screen reports a
`ScreenLocked` error, and a session that lost the console to Fast User
Switching reports a `SessionNotOnConsole` error, instead of returning a
black screenshot or a lock-screen accessibility tree.

Every tool preflights its permission before any other native call runs
(`AXIsProcessTrusted`, `CGPreflightPostEventAccess`, or
`CGPreflightScreenCaptureAccess`, matched to the tool). Without the
right permission, a tool fails with a clean, structured permission
error instead of an opaque native one. No tool silently does nothing.
See PINV-10 in [`docs/INVARIANTS.md`](docs/INVARIANTS.md).

## Installing

Every install path below is built and published by
[`dist`](https://opensource.axo.dev/cargo-dist/) — see
[`docs/RELEASING.md`](docs/RELEASING.md) for how.

The recommended path is npm. The package is scoped, not `polarize` —
that name is already taken on the public npm registry:

```sh
npx @unooo/polarize@latest
```

This downloads the release archive for your Mac, verifies its
checksum, and installs the `polarize` binary.

Prefer a shell script over Node.js? Use the fallback installer instead:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/officialunofficial/polarize/releases/latest/download/polarize-installer.sh | sh
```

This script detects your Mac's architecture, downloads the matching
release archive from GitHub Releases, and verifies its checksum.

Homebrew isn't available yet. `dist` builds a formula on every release.
Publishing it needs a tap repository that doesn't exist yet — see
"What this doesn't do" in [`docs/RELEASING.md`](docs/RELEASING.md).

Every path installs a binary signed with a real Developer ID identity
today (see `docs/RELEASING.md`'s "Signing" section). macOS ties each
TCC grant to the binary's code-signing identity. That identity stays
the same across releases, so Accessibility and Screen Recording grants
survive an upgrade.

After installing by either path, run the bootstrap flag once to grant
permissions:

```sh
polarize --request-permissions
```

See [`docs/PERMISSIONS.md`](docs/PERMISSIONS.md) for exactly which
permission each tool needs, and when `polarize` does and doesn't
prompt.

Then register `polarize` with your MCP client — see "Using it with an
MCP client" below for the generic stdio config shape most clients
expect.

## Workspace layout

- `crates/polarize-core` — platform-agnostic logic: coordinate
  normalization, the accessibility-tree data model, MCP tool schemas,
  error types, the permission-state enum, and the traits
  `polarize-macos` implements. No macOS-only dependencies. Fully
  unit-tested with `cargo test`.
- `crates/polarize-macos` — real macOS framework bindings implementing
  those traits, via ScreenCaptureKit (screen capture), `objc2-core-graphics`
  (`CGEvent` synthetic input), and `objc2-app-kit` (window/app
  enumeration and activation).

  Accessibility-tree inspection (`AXUIElement`) binds
  `ApplicationServices` directly via a small `extern "C"` layer
  (`src/ax_ffi.rs`) — not `objc2-accessibility`, which despite its name
  binds a different, newer Apple framework
  (`AXCustomContent`/`AXChart`, for content authoring).

  This crate is macOS-only. Its real native-API behavior cannot be
  exercised in CI — see [`docs/INVARIANTS.md`](docs/INVARIANTS.md) for
  what that means and how it's handled honestly.
- `apps/polarize` — the thin `rmcp`-based stdio MCP server binary that
  wires MCP tool calls to the two crates above.

## Building from source

This section is for contributors. Most users should follow
"Installing" above instead.

```sh
cargo build --release
```

The release binary is at `target/release/polarize`. This project only
builds on macOS, since `polarize-macos` links real macOS frameworks.

## Running tests

```sh
cargo test --workspace
```

This runs `polarize-core`'s full unit-test suite (coordinate math, the
AX-tree model, MCP schemas, permission logic, and orchestration), plus
`polarize-macos`'s tests for the pure sub-logic it factors out of its
native calls (app-identity matching, modifier/keycode/click-sequence
mapping, pixel-to-fraction frame clamping, and private-symbol
resolution). None of these touch a real window server, screen, or AX
tree. `polarize-macos`'s actual native-API behavior has no automated
coverage anywhere, in this repo or in CI — see
[`docs/INVARIANTS.md`](docs/INVARIANTS.md).

## Using it with an MCP client

`polarize` speaks MCP over stdio, via `rmcp`'s `ServiceExt::serve`
attached to `rmcp::transport::stdio()` — see `apps/polarize/src/main.rs`.
Register it with any MCP client that supports a stdio server by pointing
it at the built binary:

```json
{
  "mcpServers": {
    "polarize": {
      "command": "/path/to/polarize/target/release/polarize",
      "args": []
    }
  }
}
```

Adjust the config key/shape to match your client — the command/args
pair above is the generic stdio shape most MCP-compatible clients
expect. Most clients write their own project- or machine-scoped config
file for this (check your client's docs for its exact location and
CLI, if it has one); treat that file as machine-specific and keep it
out of version control, since it embeds an absolute local path.

### Using it as a Claude Code plugin

`polarize` is also its own Claude Code plugin marketplace — this repo
carries `.claude-plugin/marketplace.json` and `.claude-plugin/plugin.json`
at its root. Install it directly, once `polarize` itself is installed
and on `PATH` (see "Installing" above):

```
/plugin marketplace add officialunofficial/polarize
/plugin install polarize@polarize
```

This registers `polarize`'s MCP tools automatically — no manual
stdio config needed. It also installs the `polarize-setup` skill,
which walks through the one-time `--request-permissions` bootstrap.
A `SessionStart` hook prints an install hint if `polarize` isn't on
`PATH` yet. See [`docs/PERMISSIONS.md`](docs/PERMISSIONS.md) for what
each permission covers.

The plugin has no install-time hook of its own — it can't fetch the
`polarize` binary for you. Install `polarize` first, through any of
the channels above, then install the plugin.

`claude plugin validate .claude-plugin/marketplace.json --strict`
passes clean. `claude plugin validate .claude-plugin/plugin.json
--strict` reports one warning: this repo's own root `CLAUDE.md` is
not loaded as plugin context. That is expected, by Claude Code's own
design, for any plugin whose source is a full repo root — see
[Anthropic's plugin reference](https://code.claude.com/docs/en/plugins-reference).
It is not a defect. Moving the plugin into its own subdirectory would
silence it. But `docs/PERMISSIONS.md` would then stop shipping inside
the plugin's own source tree — a worse trade.

### Keeping TCC permission grants across rebuilds

`describe`, `tap`, and `keyboard` need Accessibility access.
`screenshot` needs Screen Recording access. macOS ties each grant to
the binary's code-signing identity. An unsigned `cargo build` output
falls back to an ad-hoc identity keyed on the binary's content hash. So
every rebuild produces a "new" binary, and macOS forgets the grant.

Sign the binary with a stable local certificate instead, so its
identity — and the grant — survives rebuilds. One-time setup:

```sh
# Create a self-signed Code Signing certificate in your login keychain.
openssl req -x509 -newkey rsa:2048 -keyout /tmp/polarize-dev.key \
  -out /tmp/polarize-dev.crt -days 3650 -nodes -sha256 -subj "/CN=polarize-dev" \
  -addext "extendedKeyUsage=critical,codeSigning" -addext "basicConstraints=critical,CA:false"
openssl pkcs12 -export -legacy -out /tmp/polarize-dev.p12 \
  -inkey /tmp/polarize-dev.key -in /tmp/polarize-dev.crt -passout pass:polarize-dev-temp
security import /tmp/polarize-dev.p12 -k ~/Library/Keychains/login.keychain-db \
  -P polarize-dev-temp -A -T /usr/bin/codesign
security add-trusted-cert -p codeSign -k ~/Library/Keychains/login.keychain-db /tmp/polarize-dev.crt
rm /tmp/polarize-dev.key /tmp/polarize-dev.p12
```

Then run `just build` instead of `cargo build --workspace`. It signs
`target/debug/polarize` with that certificate afterward, using a fixed
`--identifier`. Rebuilding with plain `cargo build` (no re-sign) still
works. But it loses the stable identity, and reverts to the
ad-hoc-signature behavior above.

Grant the two permissions once, using the binary's own bootstrap flag
rather than System Settings' Accessibility/Screen Recording "+"
picker:

```sh
./target/debug/polarize --request-permissions
```

Adding a raw (non-`.app`) binary through that "+" picker does not
reliably produce a working grant. The entry can show up toggled on.
Yet `AXIsProcessTrusted`/`CGPreflightScreenCaptureAccess` still report
`false`. `--request-permissions` calls the OS's own prompting APIs
instead (`AXIsProcessTrustedWithOptions`, `CGRequestScreenCaptureAccess`).
That is what actually registers a functional grant. Approve the system
dialogs it triggers. Then run the flag again to confirm both
permissions report `true`.

This flag is a one-time setup helper. No MCP tool call uses it. Every
real `describe`/`tap`/`keyboard`/`screenshot` call still preflights
with the non-prompting checks — PINV-10/PINV-11 in
`docs/INVARIANTS.md`.

If Screen Recording still reports `false` after you approve the
dialog, check your *terminal app* too (Ghostty, iTerm, Terminal.app,
…). macOS sometimes attributes the request to the terminal, not to
`polarize` itself. Enable the terminal under System Settings > Privacy
& Security > Screen Recording as well.

An earlier manual "+"-add can also leave a stuck decision behind.
Remove that entry first, then retry. Or run `tccutil reset
ScreenCapture` — this resets Screen Recording for *every* app on the
Mac, not just `polarize`, so prefer removing the single stuck entry
when you can.

### Automation permission and the app bundle

`run_applescript` and `script_dictionary` need Automation access.
macOS grants it per target app (Messages, Mail, Finder, …), not once
for the whole binary. Accessibility and Screen Recording key their
grant to `polarize`'s code-signing identity alone. The bare binary
already has a stable one, after `just build`. Automation needs one
more thing: a real `CFBundleIdentifier` for macOS to hold the grant
against.

`just build` assembles this as `dist/Polarize.app`, alongside the bare
`target/debug/polarize`. `just bundle-app` on its own re-runs just
that step. `just verify-bundle` lints and codesign-verifies it, and
confirms LaunchServices accepts it. Run the bootstrap flag through the
bundle, to request Automation for a target app:

```sh
./dist/Polarize.app/Contents/MacOS/polarize --request-permissions Messages
```

The MCP server itself respawns itself once at startup, through a
disclaimed spawn. Your client launches it, from either the bare binary
or the bundle. The respawn gives it its own responsible-process
identity. Otherwise it would inherit the launching shell's or client's
identity instead. See PINV-52 in `docs/INVARIANTS.md` for the full
mechanism. See it too for what a real macOS session still needs to
confirm, about the resulting grant.

Stdio matches how a local MCP client typically spawns `polarize`: one
client, one subprocess, for the process's whole lifetime. `rmcp` also
supports a Streamable HTTP transport, for a real shared server process
multiple clients or machines could reach. Nothing in this repository
builds or wires up that transport yet — it stays a possible future
addition, not a v1 requirement.

## Design notes

`apps/polarize` depends on `rmcp` with its `"macros"` feature enabled,
not on the separate `rmcp-macros` crate directly. `rmcp`'s `"macros"`
feature already re-exports `rmcp-macros`'s `#[tool]`/`#[tool_router]`/
`#[tool_handler]` attributes. A direct `rmcp-macros` dependency resolved
a second, version-mismatched copy of that proc-macro crate alongside
the one `rmcp` pulls in itself — see `apps/polarize/Cargo.toml`'s
comment for the detail.

## License

Dual-licensed under [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option.
