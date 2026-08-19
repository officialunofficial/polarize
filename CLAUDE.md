# polarize

A Rust MCP server that automates real native macOS AppKit applications:
screenshot capture, accessibility-tree inspection, and synthetic mouse
and keyboard input. It is the native-macOS analog of Argent, which
covers iOS Simulator, Android Emulator, Chromium, and Vega, but not
plain AppKit windows.

## Workspace layout

- `crates/polarize-core` — platform-agnostic logic: coordinate
  normalization, the accessibility-tree data model, MCP tool schemas,
  error and permission types, and the traits `polarize-macos`
  implements. No macOS-only dependencies. Fully unit-tested.
- `crates/polarize-macos` — real macOS framework bindings for those
  traits. macOS-only. Its native-API behavior has no automated test
  coverage; see `docs/INVARIANTS.md`.
- `apps/polarize` — the thin `rmcp` stdio MCP server binary.

## Commands

```sh
cargo build --workspace
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

A `rust-toolchain.toml` pins the `stable` channel.

## Engineering conventions

- **TDD, always.** Write the failing test first, confirm it fails for
  the right reason, then implement until it passes.
- **Document non-obvious rules as invariants.** Use `docs/INVARIANTS.md`'s
  Always/Because/If-violated format, numbered `PINV-N`. State plainly
  which test file covers each one, or that it needs a real macOS session
  with Screen Recording and Accessibility permissions granted.
- **Never claim test coverage that does not exist.** `polarize-macos`'s
  native calls cannot run in CI. Say so in the enforcement checklist
  instead of working around it.

## Documentation style

Every doc, README section, doc comment, commit message, and PR
description in this repo follows ASD-STE100 Simplified Technical
English: short sentences (20 words or fewer), one idea per sentence,
active voice, one term per concept, no noun strings over three words.
Code identifiers and established terms (`rmcp`, `AXUIElement`,
`PINV`-numbered invariants) are exempt.
