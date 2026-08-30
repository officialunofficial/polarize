---
name: polarize-setup
description: This skill should be used when polarize's MCP tools fail with a permission error, or when a user asks to set up, grant, or troubleshoot polarize's macOS Accessibility, Screen Recording, or Automation access.
version: 0.2.0
---

# polarize setup

polarize needs three macOS permissions, granted once per Mac: Accessibility, Screen Recording, and Automation. This skill walks through granting them.

## When this applies

- A polarize tool call fails with a `PermissionDenied` or similar error.
- The user asks to set up, grant, or reset polarize's permissions.
- `hooks/check-polarize.sh` reported polarize is missing from `PATH`.

## Steps

1. Confirm polarize is installed. Run `polarize --version` in a
   terminal. If that fails, install it first:

   ```sh
   curl --proto '=https' --tlsv1.2 -LsSf \
     https://github.com/officialunofficial/polarize/releases/latest/download/polarize-installer.sh | sh
   ```

2. Run the bootstrap flag once, from a terminal, not from inside this
   session:

   ```sh
   polarize --request-permissions
   ```

   This always triggers three separate system dialogs first:
   Accessibility, Screen Recording, and Automation. Approve each one.
   On a fresh install, these dialogs are the whole flow.

   If a permission is still not granted after its dialog, the command
   is designed to open a guided setup helper window for it — but only
   when `polarize` runs from inside `Polarize.app` (the notarized
   release asset, or a `just build` output). A bare `polarize` binary
   cannot locate the helper. In that case the command prints "Could
   not locate the guided permission helper" and falls straight to step
   4 below.

3. If the guided setup helper opens, it first shows a checklist
   window: one row per still-needed permission, each with an "Allow"
   button. This window behaves like an ordinary app window — it has
   normal title-bar controls and can be closed directly.

   Tapping Allow for a permission opens its guide screen. This screen
   floats a small window over System Settings and does not steal
   focus:
   - For Accessibility or Screen Recording, it opens the matching
     Privacy & Security pane. Enable Polarize in the list, or drag the
     Polarize icon the window shows into that list.
   - For Automation, it opens the Automation pane. No drag applies
     here. Find Polarize's row for the named app and allow it.

   The guide screen shows its position in the list ("2 of 3") and
   offers Previous and Next buttons, so multiple permissions can be
   handled one after another without returning to the checklist each
   time. A "‹ Back" button returns to the checklist directly.

   The terminal command polls for the grant on its own. Once every
   requested permission is granted, the current screen shows a short
   success message, then the whole helper closes by itself. Closing
   any helper window early is safe — the command still finishes and
   prints what is still missing. After 5 minutes with no grant, the
   command times out and prints the same report.

4. Fall back to the manual path when the helper does not open, opens
   an unrecognized System Settings pane, closes early, or times out.
   Open System Settings > Privacy & Security, find "Polarize" under
   Accessibility, Screen Recording, or Automation, and enable it
   directly.

5. Restart this Claude Code session. polarize's MCP tools only see a
   fresh permission grant after their process restarts.

## Reference

See [`docs/PERMISSIONS.md`](${CLAUDE_PLUGIN_ROOT}/docs/PERMISSIONS.md)
for exactly which polarize tool needs which permission, and what each
denial error looks like.
