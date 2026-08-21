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
#
# `build` also assembles `Polarize.app`, a real bundle directory
# around the same signed binary (see `bundle-app` below). Research
# found a bare, non-bundle executable cannot hold a stable Automation
# identity of its own — see PINV-52 in `docs/INVARIANTS.md`, and
# PINV-44's "Correction" section for the live `-600`
# (`kLSApplicationNotFoundErr`) finding that motivated it. `apps/
# polarize/src/main.rs` pairs the bundle with a disclaimed self-respawn
# at server startup; the two together are what let `polarize` become
# its own TCC-responsible process, independent of what launched it.

bin := "target/debug/polarize"
bundle := "target/debug/Polarize.app"
identity := "polarize-dev"
identifier := "com.officialunofficial.polarize"
entitlements := "apps/polarize/polarize.entitlements"
plist_template := "apps/polarize/bundle/Info.plist.in"
# Read straight from the workspace's own version field, so this can
# never drift from what `apps/polarize/build.rs` embeds via
# `CARGO_PKG_VERSION`.
version := `awk -F'"' '/^version = /{print $2; exit}' Cargo.toml`

build:
    cargo build --workspace
    codesign --force --sign {{identity}} --identifier {{identifier}} --options runtime --entitlements {{entitlements}} {{bin}}
    just bundle-app

# Assembles `Polarize.app`: a real bundle directory
# (`Contents/{Info.plist,MacOS/polarize}`) around the same signed
# binary `build` produces, then signs the bundle itself. The bundle's
# own `Info.plist` — rendered from the same template
# `apps/polarize/build.rs` embeds into the bare binary — carries
# `identifier`, so `--identifier` is not passed again on this
# `codesign` call; the bundle's `CFBundleIdentifier` is what macOS
# reads instead.
bundle-app:
    mkdir -p {{bundle}}/Contents/MacOS
    sed 's/__VERSION__/{{version}}/' {{plist_template}} > {{bundle}}/Contents/Info.plist
    cp {{bin}} {{bundle}}/Contents/MacOS/polarize
    codesign --force --sign {{identity}} --options runtime --entitlements {{entitlements}} {{bundle}}

# Verifies `Polarize.app` is a well-formed, LaunchServices-acceptable
# bundle. Every check here needs zero TCC grants — see PINV-52's own
# "not automatable" note for what this cannot check.
verify-bundle: bundle-app
    plutil -lint {{bundle}}/Contents/Info.plist
    codesign --verify --strict --deep {{bundle}}
    codesign -dv {{bundle}} 2>&1 | grep -q "Identifier={{identifier}}"
    # Asks LaunchServices to resolve the bundle by identity, with no
    # launch — checkable with zero TCC grants. PINV-44 documented a
    # bare binary failing this with `-600` (`kLSApplicationNotFoundErr`)
    # on an earlier macOS; that specific failure did not reproduce
    # against the bare binary when this bundle work was verified live
    # on a macOS 27 beta, so read this as "the bundle is well-formed
    # and LaunchServices-acceptable," not as a guaranteed regression
    # check against that exact prior finding — see PINV-52's own note.
    # `spctl` is deliberately not run here: it always fails against
    # this self-signed dev certificate, which is expected, not a
    # regression.
    #
    # `open -a` resolves a relative path as an app *name* lookup
    # against LaunchServices' own database, not a literal filesystem
    # path — confirmed live: a relative `{{bundle}}` fails "Unable to
    # find application," while the same path made absolute succeeds.
    # `justfile_directory()` makes it absolute.
    open -Ra {{justfile_directory()}}/{{bundle}}
    @echo "Polarize.app: well-formed, signed, and LaunchServices-acceptable."

test:
    cargo test --workspace

check:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
