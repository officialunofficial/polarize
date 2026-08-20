# Permissions

This document lists every macOS permission `polarize` needs. It states
which tools need which permission, and when `polarize` asks for one.

## Required permissions

| Permission | Required for |
|---|---|
| Accessibility | `describe`, `tap`, `keyboard` |
| Screen Recording | `screenshot` |

Both are required. `polarize` has no optional permission. A client
that only calls `describe` still needs Accessibility granted. A client
that never calls `screenshot` can skip Screen Recording — but every
other tool still needs Accessibility.

Grant both under System Settings → Privacy & Security. See README.md's
"Permissions" section for the exact panes.

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
