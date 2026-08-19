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

All four tools are implemented and wired into a real `rmcp` stdio MCP
server (`apps/polarize`), backed by real macOS framework bindings
(`crates/polarize-macos`). The workspace builds, lints, and tests
cleanly.

The server has been driven end to end over stdio on a real macOS
session: `initialize`, `tools/list`, and a `tools/call` for each of the
four tools all round-trip real JSON-RPC, and each tool's permission
preflight fires correctly with no permission granted (see
"Permissions"). This machine has no granted Screen Recording or
Accessibility TCC authorization, so one thing stays unverified: a tool
call actually *succeeding* against a real screen or app. A human on a
macOS session with both permissions granted still needs to confirm a
`screenshot` returns real pixels, `describe` returns a real AX tree, and
`tap`/`keyboard` visibly land — see
[`docs/INVARIANTS.md`](docs/INVARIANTS.md)'s "Testing harness" section.

## Tools

`polarize` exposes four MCP tools:

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

## Permissions

`polarize` drives the real macOS UI. Whatever process runs it (your
terminal, your MCP client, or a wrapper binary) needs:

- **Screen Recording** — required for `screenshot`.
- **Accessibility** — required for `describe`, `tap`, and `keyboard`.

Grant both under System Settings → Privacy & Security.

Every tool preflights its permission before any other native call runs
(`AXIsProcessTrusted`, `CGPreflightPostEventAccess`, or
`CGPreflightScreenCaptureAccess`, matched to the tool). Without the
right permission, a tool fails with a clean, structured permission
error instead of an opaque native one. No tool silently does nothing.
See PINV-10 in [`docs/INVARIANTS.md`](docs/INVARIANTS.md).

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

## Building

```sh
cargo build --release
```

The release binary is at `target/release/polarize`. This project only
builds on macOS, since `polarize-macos` links real macOS frameworks.

### Known runtime issue: `libswift_Concurrency.dylib` not found

On at least one build/test machine (a pre-release macOS with an
Xcode-beta toolchain selected), the built binary fails to launch at
all:

```
dyld[...]: Library not loaded: @rpath/libswift_Concurrency.dylib
  Reason: no LC_RPATH's found
```

`polarize-macos` links `ScreenCaptureKit`, whose Swift entry points pull
in Swift's Concurrency runtime. On a normal macOS install, this resolves
from the system's dyld shared cache with no help needed. On the
affected machine, it did not. The binary needed the Swift 5.5
back-deployment concurrency library made available at run time, for
example:

```sh
DYLD_LIBRARY_PATH="$(xcrun --show-sdk-platform-path 2>/dev/null; echo)/../../Toolchains/XcodeDefault.xctoolchain/usr/lib/swift-5.5/macosx" \
  ./target/release/polarize
```

(equivalently, point `DYLD_LIBRARY_PATH` at
`.../CommandLineTools/usr/lib/swift-5.5/macosx` if you have the Command
Line Tools installed instead of full Xcode). This is a Swift/Xcode
toolchain quirk on that one environment, not a `Cargo.toml` or code
issue. Confirm whether your own machine needs it before assuming it
does — if `cargo run -p polarize` launches cleanly with no
`DYLD_LIBRARY_PATH` override, ignore this section.

## Running tests

```sh
cargo test --workspace
```

This runs `polarize-core`'s full unit-test suite (59 tests covering
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
above is the generic stdio shape most clients expect. If your machine
hits the "Known runtime issue" above, add the same `DYLD_LIBRARY_PATH`
under an `"env"` key in this config instead of exporting it in a shell.

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
