# `just build` compiles the workspace, then re-signs `polarize`'s
# binary with a stable local identity.
#
# macOS keys Accessibility and Screen Recording TCC grants to a
# binary's code-signing identity. A plain `cargo build` binary is
# unsigned. macOS then falls back to an ad-hoc identity. That identity
# comes from the binary's content hash. The hash changes on every
# rebuild. So you must re-grant permissions after every rebuild.
# Signing with a real certificate keeps the identity stable. A fixed
# `--identifier` does too. Either one, even self-signed, lets a TCC
# grant survive a rebuild.
#
# Automation needs one more thing. Code-signing identity alone does
# not give it. TCC needs a real bundle identity too. TCC attributes an
# Apple Event send to the nearest ancestor process with a
# `CFBundleIdentifier`. It climbs past any process that has none. A
# bare Mach-O binary has no bundle identity to climb to. Its
# Automation grant then lands on whatever launched it instead. This
# reliably reproduces. See `apps/polarize/build.rs`. It embeds a real
# `Info.plist` into the binary's `__TEXT,__info_plist` section, for
# exactly this reason. Its `CFBundleIdentifier` matches `identifier`
# below.
# `--options runtime` is also required, plus the
# `com.apple.security.automation.apple-events` entitlement
# (`apps/polarize/polarize.entitlements`). An unentitled binary's
# Apple Event sends are refused outright, even with a bundle identity
# in place.
#
# One-time setup: create a "Code Signing" certificate named
# `polarize-dev` in your login keychain (see CONTRIBUTING.md). Grant
# it Accessibility and Screen Recording access, under System Settings
# > Privacy & Security. `just build` re-signs with that same identity
# from then on. So the grant persists.
#
# `build` also assembles `Polarize.app` in `dist/` — a real bundle
# directory around the same signed binary (see `bundle-app` below). A
# bare, non-bundle executable cannot hold a stable Automation identity
# of its own. Research confirmed this. See PINV-52 in
# `docs/INVARIANTS.md`. See PINV-44's "Correction" section too, for
# the live `-600` (`kLSApplicationNotFoundErr`) finding that motivated
# it. `apps/polarize/src/main.rs` pairs the bundle with a disclaimed
# self-respawn at server startup. The two together let `polarize`
# become its own TCC-responsible process, independent of what launched
# it. The bundle's name and location — `Polarize.app`, under `dist/`
# — were a deliberate human decision, not a default left unquestioned.
# See PINV-52's own note for the earlier open question this answers.

bin := "target/debug/polarize"
bundle := "dist/Polarize.app"
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
    codesign --force --sign "{{identity}}" --identifier {{identifier}} --options runtime --entitlements {{entitlements}} {{bin}}
    just bundle-app

# Assembles `Polarize.app`, under `dist/`. It builds a real bundle
# directory — `Contents/{Info.plist,MacOS/polarize,Resources/Polarize.icns}`
# — around the same signed binary `build` produces. Then it signs the
# bundle itself. The bundle's own `Info.plist` renders from the same
# template `apps/polarize/build.rs` embeds into the bare binary. It
# carries `identifier`, so `--identifier` is not passed again on this
# `codesign` call. The bundle's `CFBundleIdentifier` is what macOS
# reads instead. `Polarize.icns` is a committed, pre-built asset — not
# rebuilt from source at build time. It started from
# `assets/polarize-logo.svg`, rasterized at 1024×1024. macOS does not
# auto-round third-party `.icns` artwork on most versions, unlike its
# own system icons. So the source was masked to Apple's own squircle
# convention first. That is a rounded rect, corner radius 22% of
# width. It was then scaled down through the standard iconset sizes,
# via `sips`. It was packed with `iconutil` last. A flat, unmasked
# square here would show hard corners in the Dock, next to every
# rounded system icon.
bundle-app:
    mkdir -p {{bundle}}/Contents/MacOS {{bundle}}/Contents/Resources
    sed 's/__VERSION__/{{version}}/' {{plist_template}} > {{bundle}}/Contents/Info.plist
    cp {{bin}} {{bundle}}/Contents/MacOS/polarize
    cp apps/polarize/bundle/Polarize.icns {{bundle}}/Contents/Resources/Polarize.icns
    codesign --force --sign "{{identity}}" --options runtime --entitlements {{entitlements}} {{bundle}}

# Verifies `Polarize.app` is a well-formed, LaunchServices-acceptable
# bundle. Every check here needs zero TCC grants. See PINV-52's own
# "not automatable" note for what this cannot check.
verify-bundle: bundle-app
    plutil -lint {{bundle}}/Contents/Info.plist
    codesign --verify --strict --deep {{bundle}}
    codesign -dv {{bundle}} 2>&1 | grep -q "Identifier={{identifier}}"
    # Asks LaunchServices to resolve the bundle by identity, with no
    # launch — checkable with zero TCC grants. PINV-44 documented a
    # bare binary failing this with `-600` (`kLSApplicationNotFoundErr`)
    # on an earlier macOS. That specific failure did not reproduce
    # against the bare binary this time. This bundle work was verified
    # live, on a macOS 27 beta. So read this as: the bundle is
    # well-formed and LaunchServices-acceptable. It is not a guaranteed
    # regression check against that exact prior finding. See PINV-52's
    # own note.
    # `spctl` is deliberately not run here. It always fails against
    # this self-signed dev certificate. That is expected, not a
    # regression.
    #
    # `open -a` resolves a relative path as an app name lookup. It
    # searches LaunchServices' own database, not a literal filesystem
    # path. Confirmed live: a relative `{{bundle}}` path fails, "Unable
    # to find application." The same path, made absolute, succeeds.
    # `justfile_directory()` makes it absolute.
    open -Ra {{justfile_directory()}}/{{bundle}}
    @echo "Polarize.app: well-formed, signed, and LaunchServices-acceptable."

test:
    cargo test --workspace

check:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings

# Checks that every `codesign` line in `build` and `bundle-app` still
# parses as shell when `identity` holds a real Developer ID string.
# Those strings contain parentheses, e.g.
# `Developer ID Application: Name (TEAMID)`. An unquoted `{{identity}}`
# then breaks the recipe with `syntax error near unexpected token '('`.
# That exact failure broke every notarized-app release job through
# v0.4.0. This recipe dry-runs both recipes with such an identity, then
# feeds each generated line through `sh -n`. Nothing executes and
# nothing gets signed. `.github/workflows/ci.yml` runs this on every
# push.
check-quoting:
    #!/usr/bin/env bash
    set -euo pipefail
    id='Developer ID Application: Example Name (ABCDE12345)'
    for recipe in build bundle-app; do
        just -n bin=/dev/null identity="$id" "$recipe" 2>&1 \
            | grep '^codesign' \
            | while IFS= read -r line; do
                sh -n -c "$line" || { echo "unparseable $recipe line: $line" >&2; exit 1; }
            done
    done
    echo "check-quoting: ok"
