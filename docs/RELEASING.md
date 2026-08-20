# Releasing

This document explains how a `polarize` release happens. It states
only what the tools configured in this repo actually do today. It does
not describe planned or future steps.

Two tools drive a release. [`cargo-release`](https://github.com/crate-ci/cargo-release)
bumps the version and creates the tag. [`dist`](https://opensource.axo.dev/cargo-dist/)
(config: `dist-workspace.toml`) builds and publishes the binary once
that tag is pushed.

## Cutting a release

```sh
cargo release patch   # or minor, or major
```

`release.toml` configures this for the workspace. `polarize-core`,
`polarize-macos`, and `polarize` share one `version.workspace = true`
field in `Cargo.toml`. `cargo-release` bumps all three in lockstep. It
makes one commit and creates one `vX.Y.Z` tag.

It does not push that tag. `release.toml` sets `push = false`. It does
not run `cargo publish` either. Every crate's `Cargo.toml` sets
`publish = false` — a real Cargo manifest field, not just a
`cargo-release` setting, so a bare `cargo publish` refuses too. None of
these crates belong on crates.io. `dist` distributes the built binary
through GitHub Releases, npm, and Homebrew instead.

Push the tag by hand once you're satisfied with the result:

```sh
git push origin vX.Y.Z
```

Pushing that tag triggers `.github/workflows/release.yml`.

## What `.github/workflows/release.yml` does

This file is generated, not hand-written. Run this after editing
`dist-workspace.toml`. Never edit the workflow file directly:

```sh
dist generate --mode ci
```

The generated workflow runs four stages:

1. Plans the release (`dist plan`).
2. Builds the `polarize` binary for `aarch64-apple-darwin` and
   `x86_64-apple-darwin`, on real macOS runners.
3. Builds the shell installer, the npm package, and the Homebrew
   formula as release-time artifacts. None of these live in this repo
   as checked-in files. `dist` generates `npm/`, `install.sh`, and
   `homebrew/`-equivalent content fresh from `dist-workspace.toml`, at
   build time, every release.
4. Creates a GitHub Release for the tag and uploads every artifact to
   it.

It does not publish to the npm registry. It does not push to a
Homebrew tap either. Both need infrastructure this workflow doesn't
have yet — see "What this doesn't do" below.

## Signing

`dist-workspace.toml` sets `macos-sign = false`. Every released binary
is ad-hoc-signed until this flips to `true`, and three GitHub Actions
secrets exist:

| Secret | Purpose |
|---|---|
| `CODESIGN_IDENTITY` | The Developer ID Application identity string, e.g. `Developer ID Application: Name (TEAMID)`. |
| `CODESIGN_CERTIFICATE_PASSWORD` | Password for the `.p12` export below. |
| `CODESIGN_CERTIFICATE` | Base64-encoded `.p12` export of a Developer ID Application certificate. |

These are `dist`'s own three signing secret names. Its `Codesign::new`
function reads exactly these three environment variables. This repo
did not invent them. All three need a paid Apple Developer Program
membership, held by the repo owner, before they can exist. `dist`
signs with them when present. It falls back to an ad-hoc signature,
with no hard failure, when any secret is missing.

Flipping `macos-sign = true` needs no other change here. Run `dist
generate --mode ci` again, and it adds the signing step to
`.github/workflows/release.yml` on its own.

## Why an unsigned release still matters

This repo's `justfile` already solves the *local* version of this
problem. `just build` signs the debug binary with a self-signed
`polarize-dev` certificate. That keeps Accessibility and Screen
Recording grants alive across a local rebuild — see CONTRIBUTING.md.

A released binary needs the same stability, for the same reason. macOS
ties a TCC permission grant to a binary's code-signing identity, not
its path or version. An ad-hoc-signed release binary gets a new
identity on every release. A local ad-hoc-signed build gets a new
identity on every rebuild, for the same underlying reason. Either way,
the old grant doesn't survive.

Until `macos-sign = true` and the three secrets above exist, a user who
upgrades `polarize` must re-grant both permissions after every release.
There is no way around that today. README.md's "Installing" section
states this caveat for users directly. See also
[`PERMISSIONS.md`](PERMISSIONS.md).

## What this doesn't do

- **Notarization.** `dist` has no built-in notarization step. Adding
  one needs a hand-written `xcrun notarytool submit` call layered on
  top. None of `polarize`'s three install channels — npm, the shell
  installer, Homebrew — trigger Gatekeeper's quarantine check. That
  check only fires against a direct browser download. Notarization
  buys little for these channels, so it stays unimplemented. Revisit
  this only if a browser-download channel is added later.
- **npm registry publish.** `dist build` produces a ready-to-publish
  npm package as a release artifact. It's scoped
  `@officialunofficial/polarize`, matching Argent's own
  `@swmansion/argent` convention — the unscoped name `polarize` is
  already taken on the public registry. Nothing in this workflow runs
  `npm publish` yet. That needs an `NPM_TOKEN` secret and an explicit
  publish job this repo doesn't have.
- **Homebrew tap push.** `dist build` produces a ready-to-use formula
  (`polarize.rb`) as a release artifact. Publishing it needs a real tap
  repository, `officialunofficial/homebrew-tap`, which doesn't exist
  yet. It also needs a token this workflow can push to that repo with.
  Until both exist, a user who wants Homebrew installs from the formula
  artifact by hand.
- **Version bump or tag creation.** Both happen locally. Run `cargo
  release`, then push its tag yourself — see "Cutting a release" above.
