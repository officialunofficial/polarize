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
respected by both `release-plz` and a bare `cargo publish`. `dist`
distributes the built binary through GitHub Releases, npm, and
Homebrew instead. `release-plz.toml` sets `git_only = true` on the one
package that tags and releases, for the same reason: there's no
crates.io version to compare against, only git tags.

Pushing that tag triggers `.github/workflows/release.yml`.

## Release PR authentication

Both `release-plz` jobs mint a GitHub App installation token at
runtime (`actions/create-github-app-token`), from two repo secrets:

| Secret | Purpose |
|---|---|
| `RELEASE_PLZ_APP_ID` | The `offuno-release-plz` GitHub App's numeric App ID. |
| `RELEASE_PLZ_APP_PRIVATE_KEY` | That App's private key (PEM), used to mint short-lived installation tokens. |

Both need to exist before this workflow can run. The App also needs
its own access to this repo, granted separately under the org's
installation settings. See CONTRIBUTING.md if either is still missing.

A tag created with the default `GITHUB_TOKEN` does not trigger other
workflows. `dist`'s release.yml would never fire on the release tag
because of that. The App token does fire it — that's the whole reason
this workflow mints one instead of using `GITHUB_TOKEN` directly.

## What `.github/workflows/release.yml` does

This file is generated, not hand-written. Run this after editing
`dist-workspace.toml`. Never edit the workflow file directly:

```sh
dist generate --mode ci
```

The generated workflow runs five stages:

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
5. Assembles, signs, and notarizes `Polarize.app`, then uploads it as
   an extra release asset — a hand-written custom job, not a generated
   stage. See "Signing and notarization" below.

`dist-workspace.toml` also sets `github-attestations = true`. Each
built artifact gets a signed build-provenance attestation
(`actions/attest`), so anyone can verify it really came from this
repo's CI. This needs no secrets of its own.

6. Publishes the npm package to the registry as `@unooo/polarize`.
   See "npm publish" below.

It does not push to a Homebrew tap. That needs infrastructure this
workflow doesn't have yet — see "What this doesn't do" below.

## npm publish

`dist-workspace.toml` lists `./publish-npm` in `publish-jobs`. That
is the hand-written `.github/workflows/publish-npm.yml`, not `dist`'s
built-in `npm` job. It runs after `host` creates the GitHub Release.
It downloads the generated npm package and runs `npm publish`.

The package is scoped `@unooo/polarize`. The unscoped name `polarize`
is already taken on the public registry. The `unooo` npm org owns the
scope. Argent publishes `@swmansion/argent` under its org the same way.

### Why not `NPM_TOKEN`

`dist`'s built-in `npm` publish job authenticates with a long-lived
`NPM_TOKEN` secret. npm deprecated that path in July 2026. Granular
tokens that bypass 2FA lost management rights in August 2026. They
lose publish rights around January 2027. npm recommends trusted
publishing (OIDC) instead. The custom job uses it. It needs no secret.

Trusted publishing works like this. The job requests `id-token: write`.
GitHub mints a short-lived OIDC token that names this repo and the
workflow file. npm checks that token against the trusted publisher
registered on the package. npm also generates a provenance attestation
for the publish, with no extra flags.

### One-time bootstrap

npm only lets you register a trusted publisher on a package that
already exists. So the first version of `@unooo/polarize` was
published by hand, from a maintainer's laptop, with 2FA. Every later
version publishes from CI.

If the trusted publisher ever needs re-creating:

1. Open <https://www.npmjs.com/package/@unooo/polarize/access>.
2. Under "Trusted publisher", pick GitHub Actions.
3. Organization or user: `officialunofficial`. Repository: `polarize`.
   Workflow filename: `release.yml`. Environment: leave empty.

The publish job runs as a reusable workflow. GitHub's OIDC token then
carries two workflow names: the caller (`release.yml`) and the callee
(`publish-npm.yml`). If a publish fails with a "workflow does not
match" error, register `publish-npm.yml` instead.

### Failure handling

A failed npm publish does not roll back the GitHub Release. Re-run the
`publish-npm` job from the Actions UI after the fix. npm refuses to
publish the same version twice, so a re-run after a partial success
fails until the next release.

## Signing

`dist-workspace.toml` sets `macos-sign = true`. `dist` needs three
GitHub Actions secrets to sign with a real identity. All three exist
now:

| Secret | Purpose | Present? |
|---|---|---|
| `CODESIGN_IDENTITY` | The Developer ID Application identity string, e.g. `Developer ID Application: Name (TEAMID)`. | Yes |
| `CODESIGN_CERTIFICATE_PASSWORD` | Password for the `.p12` export below. | Yes |
| `CODESIGN_CERTIFICATE` | Base64-encoded `.p12` export of a Developer ID Application certificate. | Yes |

These are `dist`'s own three signing secret names. Its `Codesign::new`
function reads exactly these three environment variables. This repo
did not invent them. All three needed a paid Apple Developer Program
membership, held by the repo owner, before they could exist.
`.github/workflows/release.yml`'s build job already maps all three
secrets into `dist`'s environment. The next tagged release signs the
binary with the real Developer ID identity, not an ad-hoc signature.
No further workflow change is needed for that.

This Developer ID signature does not, on its own, make the released
binary hardened-runtime-signed or notarizable. `dist`'s generated
workflow maps only these three secrets. It never sets
`CODESIGN_OPTIONS`, so `dist`'s own signing step never adds
`--options runtime`, and it passes no `--entitlements`. That gap
matters only for the notarized `.app` bundle "Signing and
notarization" covers below, not for this real-Developer-ID-signature
milestone.

## Why a stable signing identity matters

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

Now that all three secrets in "Signing" exist, `dist` signs each
release with the same Developer ID identity every time. A user who
upgrades `polarize` keeps their existing Accessibility and Screen
Recording grants across that upgrade. README.md's "Installing" section
and [`PERMISSIONS.md`](PERMISSIONS.md) still describe the older,
ad-hoc-signed caveat for releases cut before this fix — update both if
they still claim every release re-prompts.

## Signing and notarization: `Polarize.app`

`dist` has no built-in bundle-assembly or notarization step. Its own
`sign/macos.rs` module comment says so directly. Nothing in "Signing"
above makes `Polarize.app` notarizable — that section only covers the
bare `polarize` binary `dist` builds and signs itself.

A hand-written job closes that gap:
[`.github/workflows/build-notarized-app.yml`](../.github/workflows/build-notarized-app.yml).
`dist-workspace.toml`'s `publish-jobs` key runs it as a custom publish
job, after the `host` job creates the GitHub Release — see
[`dist`'s CI customization docs](https://axodotdev.github.io/cargo-dist/book/ci/customizing.html).
Unlike `release.yml`, this file is not generated. Edit it directly.

It builds `polarize`, then assembles `Polarize.app` via `just
bundle-app`. It signs the bundle with the same Developer ID identity
as "Signing" above, plus `--options runtime`, `--timestamp`, and the
bundle's entitlements. It submits the signed bundle to Apple's notary
service with `xcrun notarytool submit --wait`, then staples the
ticket with `xcrun stapler staple`. It uploads the stapled result as
an extra release asset — alongside, not instead of, the existing
npm/shell-installer/Homebrew channels.

Four decisions, made by the repo owner, shape this job:

- **Additional asset, not a replacement.** The bare-binary channels
  keep working exactly as before. `Polarize.app` is a new, fourth
  asset, for standalone use or later embedding into another app.
- **Two bundles, not one universal binary.** `Polarize-aarch64-apple-darwin.zip`
  and `Polarize-x86_64-apple-darwin.zip` notarize separately, matching
  `dist`'s own two-target build matrix. No `lipo` merge step exists.
- **A notarization failure does not block the release.** The `host`
  job already created the GitHub Release before this job runs. A
  failed notarization leaves that release published, without the
  `.app` asset. The job itself, and the `announce` job after it, both
  report the failure.
- **`notarytool` authenticates with an App Store Connect API key**, not
  an Apple ID and app-specific password. No 2FA prompt, works cleanly
  in CI. See
  [Apple's TN3147](https://developer.apple.com/documentation/technotes/tn3147-migrating-to-the-latest-notarization-tool)
  for both supported auth methods.

Three more GitHub Actions secrets drive this job, unrelated to the
`CODESIGN_*` secrets above. Those sign the bundle. These authenticate
the notarization submission itself:

| Secret | Purpose |
|---|---|
| `NOTARY_KEY_ID` | The App Store Connect API key's Key ID. |
| `NOTARY_ISSUER_ID` | The API key's Issuer ID (a UUID). |
| `NOTARY_KEY` | Base64-encoded `.p8` private key file for that API key. |

All three exist now, added 2026-08-27. They come from one App Store
Connect API key with the Developer role. The repo owner generates it
at <https://appstoreconnect.apple.com/access/integrations/api>. Apple
lets you download the `.p8` file once. Store it base64-encoded:

```sh
base64 -i AuthKey_XXXX.p8 | gh secret set NOTARY_KEY
```

If any of the three is missing, `custom-build-notarized-app` fails at
its notarization submission step. The release itself still publishes —
see the non-blocking decision above.

Before v0.4.1, this job failed earlier than that, on every release.
`justfile` passed the signing identity to `codesign` unquoted. Developer
ID identities contain parentheses, so the shell rejected the line.
#55 fixed it. `just check-quoting` guards against a repeat, and CI
runs it on every push.

See PINV-54 in [`docs/INVARIANTS.md`](INVARIANTS.md) for the
enforcement story, and PINV-52/PINV-53 for why `Polarize.app` needs
its own signing identity at all.

## What this doesn't do

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
