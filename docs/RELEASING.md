# Releasing

This document explains how a `polarize` release happens. It states
only what the tools configured in this repo actually do today. It does
not describe planned or future steps.

Two tools drive a release. [`release-plz`](https://release-plz.dev/)
(config: `release-plz.toml`) opens a Release PR, then tags `vX.Y.Z`
when that PR merges. [`dist`](https://opensource.axo.dev/cargo-dist/)
(config: `dist-workspace.toml`) builds and publishes the binary once
that tag exists.

## Cutting a release

Open (or update) the Release PR from the Actions tab. Run the
`release-plz` workflow's `release-plz-pr` job via `workflow_dispatch`.
It gathers every Conventional Commit merged since the last tag into one
PR, as a single version bump. `polarize-core`, `polarize-macos`, and
`polarize` all share one `version_group` in `release-plz.toml`. They
also all inherit `Cargo.toml`'s `version.workspace = true`, so bumping
that one shared field moves all three at once. Confirmed by running
`release-plz update` locally — it logged all three under one version
group and computed one shared next version.

Review the PR. It contains the version bump and a regenerated
`apps/polarize/CHANGELOG.md`. Merging it to `main` triggers the
`release-plz-release` job. That job tags `vX.Y.Z` and cuts a GitHub
Release for that tag — see `release-plz.toml`'s
`git_tag_name`/`git_release_name`.

None of `polarize`'s three crates publish to crates.io. Every crate's
`Cargo.toml` sets `publish = false` — a real Cargo manifest field,
which `release-plz` and a bare `cargo publish` both respect. `dist`
distributes the built binary through GitHub Releases, npm, and
Homebrew instead. `release-plz.toml` sets `git_only = true` on the one
package that tags and releases, for the same reason: there's no
crates.io version to compare against, only git tags.

**Token**: both `release-plz` jobs mint a GitHub App installation token
at runtime (`actions/create-github-app-token`), from two repo secrets —
`RELEASE_PLZ_APP_ID` and `RELEASE_PLZ_APP_PRIVATE_KEY`. A tag created
with the default `GITHUB_TOKEN` does not trigger other workflows, so
`dist`'s release.yml would never fire on the release tag. The App token
does. Both secrets need to exist first. The App also needs its own
access to this repo, granted separately. See CONTRIBUTING.md if either
is still missing.

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
- **Automatic release cuts.** `release-plz-pr` only runs on a manual
  `workflow_dispatch`, not on every push to `main`. No Release PR opens
  until someone deliberately triggers it — see "Cutting a release"
  above.
