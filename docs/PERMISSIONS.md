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
client that never calls `screenshot` can skip Screen Recording — but
every other tool still needs Accessibility. Automation is only needed
by a client that calls `run_applescript` or `script_dictionary`; it is
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
code-signing identity alone; the bare `target/debug/polarize` binary
already carries a stable one after `just build` re-signs it. Automation
needs one more thing: a real `CFBundleIdentifier` for TCC's
"responsible process" climb to land on, and (for a directly-launched
`polarize`, the normal MCP stdio case) a disclaimed self-respawn so
`polarize` becomes that responsible process itself, rather than
whatever launched it. `just build` assembles this automatically as
`target/debug/Polarize.app`; run its bootstrap flag through the bundle:

```sh
./target/debug/Polarize.app/Contents/MacOS/polarize --request-permissions <App Name>
```

See PINV-52 in [`INVARIANTS.md`](INVARIANTS.md) for the full mechanism,
including what is and is not confirmed by a real macOS session.
