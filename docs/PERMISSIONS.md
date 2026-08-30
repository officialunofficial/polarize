# Permissions

This document lists every macOS permission `polarize` needs. It states
which tools need which permission, and when `polarize` asks for one.

## Required permissions

| Permission | Required for |
|---|---|
| Accessibility | `describe`, `tap`, `keyboard` |
| Screen Recording | `screenshot` |
| Automation | `run_applescript`, `script_dictionary` |

Accessibility and Screen Recording are required for every install. A
client that only calls `describe` still needs Accessibility granted. A
client that never calls `screenshot` can skip Screen Recording. But
every other tool still needs Accessibility. Automation is only needed
by a client that calls `run_applescript` or `script_dictionary`. It is
granted per target app, not once for the whole binary — see PINV-44 in
[`INVARIANTS.md`](INVARIANTS.md).

Grant all three under System Settings → Privacy & Security. See
README.md's "Permissions" section for the exact panes.

## When `polarize` prompts

`polarize` never prompts for a permission at MCP server startup. No MCP
tool call triggers a permission prompt either. Every tool call
preflights its required permission first, with a non-prompting check:
`AXIsProcessTrusted`, `CGPreflightPostEventAccess`, or
`CGPreflightScreenCaptureAccess`. Without the permission, the tool call
fails with a structured permission error. It never blocks on a system
dialog. It never silently does nothing either. See PINV-10 in
[`INVARIANTS.md`](INVARIANTS.md).

The only thing that triggers a real macOS permission dialog is the
binary's own bootstrap flag:

```sh
polarize --request-permissions
```

Run this once, before registering `polarize` with an MCP client. It
calls the OS's own prompting APIs (`AXIsProcessTrustedWithOptions`,
`CGRequestScreenCaptureAccess`) directly, then reports whether each
permission is now granted. No MCP tool call uses this flag. See
PINV-11 in [`INVARIANTS.md`](INVARIANTS.md).

## The guided setup helper

`--request-permissions` always attempts the three real system prompts
above first, for every permission, before it may launch anything else
(PINV-57). A permission still not granted after its own prompt — a
real denial, or an inconclusive Automation preflight — is a permission
the command hands to a guided setup helper window.

The helper first shows a checklist: one row per still-needed
permission, each with its own "Allow" button. This checklist window
behaves like an ordinary window — it has nothing to coexist with on
screen yet. Tapping Allow opens that permission's own guide screen, a
small window that floats over System Settings without stealing focus.
It shows its position in the list and offers Previous/Next buttons, so
several permissions can be handled in a row without returning to the
checklist each time.

The helper never reads or reports Polarize's own grant state, and it
never requests a TCC grant of its own (PINV-56, PINV-58). A guide
screen only opens the matching System Settings pane and waits. For
Accessibility or Screen Recording, it also offers a drag target:
dragging Polarize's own icon into the System Settings list is an
alternative to the list's own checkbox (PINV-59). Automation gets no
drag, because its grant is per target app, not a list entry a bundle
can be dropped into (PINV-60).

The parent `polarize --request-permissions` process, never the helper,
decides when the command finishes. It polls the same non-prompting
checks described above on its own schedule. It owns the helper's
lifecycle end to end: closing the helper window early does not block
the command from finishing, and the command always terminates the
helper before it exits, including after a 300-second deadline (PINV-
61, PINV-64). The terminal's final report and whatever the helper last
displayed always come from that same single read (PINV-65).

If a System Settings pane anchor does not resolve — for example, on a
future macOS version — the helper falls back to the top-level Privacy
& Security pane plus plain-text instructions, instead of hanging or
crashing (PINV-63).

The helper is only reachable from a bundled run. `locate_helper`
resolves it relative to `Polarize.app/Contents/Resources/`, or from
the `POLARIZE_SETUP_HELPER` environment variable. A bare binary — the
shape every npm and shell-installer path installs today — has neither,
so the command prints that it could not locate the helper and falls
straight through to the same final report, with no window to open.
This is a graceful fallback, not an error: the command still finishes
and reports what remains missing.

These behaviors are designed to this contract; they still need
confirming on a real macOS session — see PINV-56 through PINV-65 in
[`INVARIANTS.md`](INVARIANTS.md) for exactly what each one has, and
has not, had verified live.

## Why a permission grant can disappear after an upgrade

macOS ties an Accessibility or Screen Recording grant to a binary's
code-signing identity, not to its file path or version. Replace the
binary with one that carries a different identity, and the grant is
gone. The next tool call fails its preflight check again, even though
nothing about the permission itself changed.

This is why `polarize`'s own signing story matters as much as its
functional code. See [`RELEASING.md`](RELEASING.md) for the
released-binary case. See README.md's "Keeping TCC permission grants
across rebuilds" section for the local-build case.

## Automation needs its own bundle identity

Accessibility and Screen Recording key their grant to `polarize`'s
code-signing identity alone. The bare `target/debug/polarize` binary
already carries a stable one, after `just build` re-signs it.
Automation needs one more thing: a real `CFBundleIdentifier`. TCC's
"responsible process" climb needs it to land on. For a
directly-launched `polarize` — the normal MCP stdio case — it also
needs a disclaimed self-respawn. That makes `polarize` become that
responsible process itself, rather than whatever launched it. `just
build` assembles this automatically as `dist/Polarize.app`. Run its
bootstrap flag through the bundle:

```sh
./dist/Polarize.app/Contents/MacOS/polarize --request-permissions <App Name>
```

See PINV-52 in [`INVARIANTS.md`](INVARIANTS.md) for the full
mechanism. It also covers what is, and is not, confirmed by a real
macOS session.
