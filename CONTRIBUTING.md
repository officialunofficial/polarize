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
re-signs the binary with a stable local identity afterward. It also
assembles `dist/Polarize.app` — a real bundle around that same binary.
This is needed for Automation's own identity (PINV-52 in
`docs/INVARIANTS.md`). `just build` also needs a Swift toolchain on
`PATH` now — it builds `PolarizeSetupHelper`, a second AppKit
executable, via SwiftPM (`apps/setup-helper`). It signs the result
straight into the bundle's `Contents/MacOS/`, alongside `polarize`,
carrying the same bundle identity (PINV-66). Xcode or the Command Line Tools
provide `swift`; `just build` fails with a clear error if neither is
installed. `just verify-bundle` checks the bundle's structure and
signature. Grant Accessibility and Screen Recording
once, with `./target/debug/polarize --request-permissions`. Grant
Automation for a given target app through the bundle instead:
`./dist/Polarize.app/Contents/MacOS/polarize --request-permissions <App Name>`.
See README's "Keeping TCC permission grants across rebuilds" section.
See its "Automation permission and the app bundle" section too.
Together they cover the full one-time setup.

This local self-signed certificate is a dev-only analog of a real
problem released binaries used to have. See
[`docs/RELEASING.md`](docs/RELEASING.md)'s "Why a stable signing
identity matters" for that side of it, and its "Release PR authentication"
section for the separate secrets `.github/workflows/release-plz.yml`
needs before it can run.

## `polarize-macos` and `apps/setup-helper` need manual verification

`cargo test --workspace` exercises `polarize-core` in full. It also
exercises the pure sub-logic `polarize-macos` factors out of its
native calls: app-identity matching, modifier/keycode/click-sequence
mapping, and pixel-to-fraction frame clamping. See `docs/INVARIANTS.md`
for the full list. `cargo test` does **not** exercise any real native
call. `swift test --package-path apps/setup-helper` exercises the same
kind of pure sub-logic in the Swift setup helper: argv parsing,
permission-to-pane mapping, drag-payload construction, and window-frame
math. It does **not** exercise any real AppKit call either.

No CI runner can grant Screen Recording or Accessibility TCC
permission headlessly. No CI runner can verify pixel or AX content, or
open a real window, headlessly either. `polarize-macos`'s real
native-API behavior has no automated coverage anywhere, in this repo
or in CI. The same is true for `apps/setup-helper`'s real AppKit
behavior: real drag-and-drop, real window tracking, real focus
behavior, and the real TCC grant a drag or a native dialog produces.

If your PR touches `polarize-macos` or `apps/setup-helper`, verify it
manually on a real macOS session with Screen Recording and
Accessibility permissions granted. Run the affected tool, or the
affected helper screen, end to end. Confirm the real behavior, not
just that it compiles. Describe what you verified, and how, in the PR
description.

See [`docs/INVARIANTS.md`](docs/INVARIANTS.md) for the invariants this
project holds itself to, including which ones can and cannot be checked
by automated tests.

## Adding or changing behavior

This project is test-first: for any non-trivial change, write the
failing test before the implementation, confirm it fails, then
implement until it passes. Where a change encodes a non-obvious
behavioral rule, add it to `docs/INVARIANTS.md` using the
Always/Because/If-violated format already there.
