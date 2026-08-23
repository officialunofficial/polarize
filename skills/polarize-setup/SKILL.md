---
name: polarize-setup
description: This skill should be used when polarize's MCP tools fail with a permission error, or when a user asks to set up, grant, or troubleshoot polarize's macOS Accessibility, Screen Recording, or Automation access.
version: 0.1.0
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

   This triggers three separate system dialogs: Accessibility, Screen
   Recording, and Automation. Approve each one.

3. If a dialog does not appear for a permission already denied once,
   macOS will not re-prompt. Open System Settings > Privacy &
   Security, find "Polarize" under Accessibility and Screen Recording,
   and enable it directly.

4. Restart this Claude Code session. polarize's MCP tools only see a
   fresh permission grant after their process restarts.

## Reference

See [`docs/PERMISSIONS.md`](${CLAUDE_PLUGIN_ROOT}/docs/PERMISSIONS.md)
for exactly which polarize tool needs which permission, and what each
denial error looks like.
