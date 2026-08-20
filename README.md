# polarize

`polarize` is a Rust [MCP](https://modelcontextprotocol.io) (Model
Context Protocol) server that automates real, native macOS AppKit
applications: screenshot capture, accessibility-tree inspection, and
synthetic mouse/keyboard input.

It is the native-macOS analog of
[Argent](https://github.com/software-mansion/argent), which drives iOS
Simulator, Android Emulator, Chromium, and Vega — but explicitly does
not support plain native macOS windows. `polarize` fills that gap.

## Status

All nine tools are implemented and wired into a real `rmcp` stdio MCP
server (`apps/polarize`), backed by real macOS framework bindings
(`crates/polarize-macos`). The workspace builds, lints, and tests
cleanly.

The first four tools have been driven end to end over stdio on a real
macOS session: `initialize`, `tools/list`, and a `tools/call` for each
all round-trip real JSON-RPC, and each tool's permission preflight fires
correctly with no permission granted (see "Permissions"). This machine
has no granted Screen Recording or Accessibility TCC authorization, so
one thing stays unverified: a tool call actually *succeeding* against a
real screen or app. A human on a macOS session with both permissions
granted still needs to confirm a `screenshot` returns real pixels,
`describe` returns a real AX tree, and `tap`/`keyboard` visibly land —
see [`docs/INVARIANTS.md`](docs/INVARIANTS.md)'s "Testing harness"
section.

The five newer tools — `perform_action`, `await_ui_element`,
`await_screen_idle`, `run_applescript`, and `script_dictionary` — have
**not** been run on a real macOS session at all. Their pure logic is
unit-tested. Their native halves compile and link only. Read each tool's
enforcement-checklist entry in
[`docs/INVARIANTS.md`](docs/INVARIANTS.md) before you trust one.

## Tools

`polarize` exposes nine MCP tools:

1. **`screenshot`** — capture a window or the whole screen to PNG,
   optionally scoped by a bundle id or app name.
2. **`describe`** — walk the `AXUIElement` accessibility tree for the
   frontmost (or a named) app, returning each element's role, label or
   title, normalized `[0, 1]` frame, and focusable/interactive flags,
   plus a ready-to-read indented text rendering of the whole tree.
3. **`tap`** — post a synthetic mouse click via `CGEvent` at a screen
   position. Coordinates are normalized `[0, 1]` fractions of the
   target screen or window's width/height, not raw pixels — matching
   Argent's own gesture-tap contract so the same mental model transfers
   across both tools.
4. **`keyboard`** — post synthetic key events via `CGEvent`: type a
   string, or press a named key. Naming a `target` app activates it
   first, so the input reaches that app even without prior focus.
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

## Permissions

`polarize` drives the real macOS UI. Whatever process runs it (your
terminal, your MCP client, or a wrapper binary) needs:

- **Screen Recording** — required for `screenshot`.
- **Accessibility** — required for `describe`, `tap`, `keyboard`,
  `perform_action`, `await_ui_element`, and `await_screen_idle`.
- **Automation** — required for `run_applescript` and
  `script_dictionary`. macOS asks per target app, the first time a
  script addresses one.

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

`polarize` has no GitHub release yet. The commands in this section do
not work today. They document the intended install path instead. This
section stays accurate the moment the first release ships. Every
install path is built and published by
[`dist`](https://opensource.axo.dev/cargo-dist/) — see
[`docs/RELEASING.md`](docs/RELEASING.md) for how.

The recommended path is npm, matching Argent's own install command.
The package is scoped, not `polarize` — that name is already taken on
the public npm registry:

```sh
npx @officialunofficial/polarize@latest
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

Every path installs an **unsigned** binary today. This holds until the
repo owner adds Apple Developer signing secrets to the release
workflow (see `docs/RELEASING.md`'s "Signing" section). Until then,
re-grant Accessibility and Screen Recording after every upgrade. macOS
ties each TCC grant to the binary's code-signing identity. An unsigned
binary gets a new identity on every release, so each new release loses
the old grant.

After installing by either path, run the bootstrap flag once to grant
permissions:

```sh
polarize --request-permissions
```

See [`docs/PERMISSIONS.md`](docs/PERMISSIONS.md) for exactly which
permission each tool needs, and when `polarize` does and doesn't
prompt.

Then register `polarize` with your MCP client. For Claude Code:

```sh
claude mcp add polarize --scope project -- "$(command -v polarize)"
```

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

  Accessibility-tree inspection is **not** built on `objc2-accessibility`,
  despite that crate's name. It binds Apple's newer, unrelated
  content-authoring "Accessibility" framework (`AXCustomContent`,
  `AXChart`, …), not the classic `AXUIElement` inspection API every
  screen reader and UI-automation tool actually walks. No
  `objc2-application-services` crate exists in the `objc2` umbrella
  either. `polarize-macos` fills that gap with a small, hand-written
  `extern "C"` binding to `ApplicationServices` directly
  (`src/ax_ffi.rs`) — see that file's doc comment for the full
  reasoning.

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

### Fixed runtime issue: `libswift_Concurrency.dylib` not found

Every built binary used to fail to launch:

```
dyld[...]: Library not loaded: @rpath/libswift_Concurrency.dylib
  Reason: no LC_RPATH's found
```

The root cause was a Cargo link-arg propagation gap. It was not a
machine-specific toolchain quirk. `screencapturekit`'s own `build.rs`
emits `cargo:rustc-link-arg=-Wl,-rpath,...` for the Swift Concurrency
runtime. But Cargo only applies a plain `rustc-link-arg` to targets
built by the *emitting* package. `polarize-macos` and `apps/polarize`
only depend on `screencapturekit`. They never build its own targets. So
that rpath never reached `polarize`'s binary or `polarize-macos`'s test
binary.

`crates/polarize-macos/build.rs` and `apps/polarize/build.rs` now
re-emit the same rpath flags for their own package, using
`rustc-link-arg-bins` for the `polarize` binary. A plain `cargo build`
or `cargo test` fixes this with no environment variable needed.

## Running tests

```sh
cargo test --workspace
```

This runs `polarize-core`'s full unit-test suite (224 tests covering
coordinate math, the AX-tree model, MCP schemas, permission logic, and
orchestration), plus `polarize-macos`'s tests for the pure sub-logic it
factors out of its native calls (22 tests covering app-identity
matching, modifier/keycode/click-sequence mapping, and pixel-to-fraction
frame clamping). None of these touch a real window server, screen, or
AX tree. `polarize-macos`'s actual native-API behavior has no automated
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

Adjust the config key/shape to match your client (Claude Code, Claude
Desktop, or any other MCP-compatible tool) — the command/args pair
above is the generic stdio shape most clients expect.

For Claude Code specifically, register it with:

```sh
claude mcp add polarize --scope project -- "$(pwd)/target/debug/polarize"
```

This writes `.mcp.json`. That file is `.gitignore`d: the command it
writes is an absolute, machine-specific path. Each clone regenerates
its own by running the command above.

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

Stdio matches how a local MCP client, such as Claude Code, actually
spawns `polarize`: one client, one subprocess, for the process's whole
lifetime. `rmcp` also supports a Streamable HTTP transport, for a real
shared server process multiple clients or machines could reach. Nothing
in this repository builds or wires up that transport yet — it stays a
possible future addition, not a v1 requirement.

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
