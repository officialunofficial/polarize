# Contributing

## Building and testing locally

```sh
cargo build --workspace
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace -- -D warnings
```

This project only builds on macOS: `polarize-macos` links real macOS
frameworks (ScreenCaptureKit, AppKit, and friends). A
`rust-toolchain.toml` pins the `stable` channel. `cargo build`/`cargo
test` then resolve a correct toolchain regardless of your machine's
ambient rustup default. An Xcode-beta-only machine running only a
nightly toolchain by default is a real thing that has happened here
before.

`crates/polarize-macos/build.rs` and `apps/polarize/build.rs` re-bake
the Swift Concurrency runtime's rpath into every binary and test
target. See README's "Fixed runtime issue" section for why that's
needed. If you ever see `dyld: Library not loaded:
@rpath/libswift_Concurrency.dylib` again, check those two files first.
It means a link-arg regressed, not that you need a machine-specific
workaround.

Use `just build` instead of `cargo build --workspace` day to day. It
re-signs the binary with a stable local identity afterward, and
assembles `target/debug/Polarize.app` — a real bundle around that same
binary, needed for Automation's own identity (PINV-52 in
`docs/INVARIANTS.md`). `just verify-bundle` checks the bundle's
structure and signature. Grant Accessibility and Screen Recording once
with `./target/debug/polarize --request-permissions`; grant Automation
for a given target app through the bundle instead —
`./target/debug/Polarize.app/Contents/MacOS/polarize --request-permissions <App Name>`.
See README's "Keeping TCC permission grants across rebuilds" and
"Automation permission and the app bundle" sections for the full
one-time setup.

This local self-signed certificate is a dev-only analog of a real
problem released binaries also have. See
[`docs/RELEASING.md`](docs/RELEASING.md)'s "Why an unsigned release
still matters" for that side of it, and its "Release PR authentication"
section for the separate secrets `.github/workflows/release-plz.yml`
needs before it can run.

## `polarize-macos` needs manual verification

`cargo test --workspace` exercises `polarize-core` in full, plus the
pure sub-logic `polarize-macos` factors out of its native calls —
app-identity matching, modifier/keycode/click-sequence mapping, and
pixel-to-fraction frame clamping (see `docs/INVARIANTS.md`). It does
**not** exercise any real native call. No CI runner can grant Screen
Recording or Accessibility TCC permission, or verify pixel/AX content,
headlessly. `polarize-macos`'s real native-API behavior has no
automated coverage anywhere, in this repo or in CI.

If your PR touches `polarize-macos`, verify it manually on a real macOS
session with Screen Recording and Accessibility permissions granted:
run the affected tool end to end (via the built `polarize` binary or an
MCP client) and confirm the real behavior, not just that it compiles.
Describe what you verified, and how, in the PR description.

See [`docs/INVARIANTS.md`](docs/INVARIANTS.md) for the invariants this
project holds itself to, including which ones can and cannot be checked
by automated tests.

## Adding or changing behavior

This project is test-first: for any non-trivial change, write the
failing test before the implementation, confirm it fails, then
implement until it passes. Where a change encodes a non-obvious
behavioral rule, add it to `docs/INVARIANTS.md` using the
Always/Because/If-violated format already there.
