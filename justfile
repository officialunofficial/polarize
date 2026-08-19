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
# One-time setup: create a "Code Signing" certificate named
# `polarize-dev` in your login keychain (see CONTRIBUTING.md), then
# grant it Accessibility and Screen Recording access under System
# Settings > Privacy & Security. `just build` re-signs with that same
# identity from then on, so the grant persists.

bin := "target/debug/polarize"
identity := "polarize-dev"
identifier := "com.officialunofficial.polarize"

build:
    cargo build --workspace
    codesign --force --sign {{identity}} --identifier {{identifier}} {{bin}}

test:
    cargo test --workspace

check:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
