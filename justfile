# `just build` compiles the workspace, then re-signs `polarize`'s
# binary with a stable local identity.
#
# macOS keys Accessibility/Screen Recording TCC grants to a binary's
# code-signing identity. A plain `cargo build` binary is unsigned, so
# macOS falls back to an ad-hoc identity derived from the binary's
# content hash — which changes on every rebuild, forcing you to
# re-grant permissions each time. Signing with a real (even
# self-signed) certificate and a fixed `--identifier` keeps that
# identity stable across rebuilds, so a TCC grant survives.
#
# Automation needs one more thing code-signing identity alone does not
# give: a real bundle identity. TCC attributes an Apple Event send to
# whichever ancestor process carries a `CFBundleIdentifier`, climbing
# past any process that has none — a bare Mach-O binary has no bundle
# identity to climb to, so its Automation grant always lands on
# whatever launched it instead, and reliably reproduces this. See
# `apps/polarize/build.rs`, which embeds a real `Info.plist` (with a
# `CFBundleIdentifier` matching `identifier` below) into the binary's
# `__TEXT,__info_plist` section for exactly this reason.
# `--options runtime` plus the `com.apple.security.automation.apple-events`
# entitlement (`apps/polarize/polarize.entitlements`) is also required —
# an unentitled binary's Apple Event sends are refused outright, even
# with a bundle identity in place.
#
# One-time setup: create a "Code Signing" certificate named
# `polarize-dev` in your login keychain (see CONTRIBUTING.md), then
# grant it Accessibility and Screen Recording access under System
# Settings > Privacy & Security. `just build` re-signs with that same
# identity from then on, so the grant persists.

bin := "target/debug/polarize"
identity := "polarize-dev"
identifier := "com.officialunofficial.polarize"
entitlements := "apps/polarize/polarize.entitlements"

build:
    cargo build --workspace
    codesign --force --sign {{identity}} --identifier {{identifier}} --options runtime --entitlements {{entitlements}} {{bin}}

test:
    cargo test --workspace

check:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
