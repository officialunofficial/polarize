#!/usr/bin/env bash
# Runs on SessionStart, via hooks/hooks.json. Plugins have no
# install-time hook — Claude Code cannot fetch the polarize binary
# itself. This only prints a hint when it is missing.
set -euo pipefail

if ! command -v polarize >/dev/null 2>&1; then
  cat <<'EOF'
polarize (macOS automation MCP server) is not on PATH. Its MCP tools
will fail to start until it is installed. Install it with:

  curl --proto '=https' --tlsv1.2 -LsSf \
    https://github.com/officialunofficial/polarize/releases/latest/download/polarize-installer.sh | sh

Then run "polarize --request-permissions" once, to grant
Accessibility, Screen Recording, and Automation access. Restart this
session afterward. See the "polarize-setup" skill for the full walk-
through.
EOF
fi
