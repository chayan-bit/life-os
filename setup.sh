#!/usr/bin/env bash
# Life OS one-shot setup: clone -> ./setup.sh -> ./run.sh and you are live.
# Builds the Rust services and installs frontend deps. Everything runs fully
# local by default (SQLite file DB, keyless AI via any agent CLI on PATH);
# .env.example documents every optional knob.
set -euo pipefail
cd "$(dirname "$0")"

say() { printf '\n\033[1m== %s\033[0m\n' "$*"; }

say "Checking prerequisites"
missing=0
for tool in cargo node npm; do
  if command -v "$tool" >/dev/null 2>&1; then
    printf '  ok  %s (%s)\n' "$tool" "$(command -v "$tool")"
  else
    printf '  MISSING  %s\n' "$tool"
    missing=1
  fi
done
if [ "$missing" = 1 ]; then
  echo
  echo "Install Rust (https://rustup.rs) and Node.js (https://nodejs.org), then re-run."
  exit 1
fi

say "Detecting local agent CLIs (the keyless AI engine)"
found_agent=0
for agent in claude gemini codex opencode hermes antigravity openclaw; do
  if command -v "$agent" >/dev/null 2>&1; then
    printf '  ok  %s\n' "$agent"
    found_agent=1
  fi
done
if [ "$found_agent" = 0 ]; then
  echo "  none found - AI features will be disabled until you install one"
  echo "  (Claude Code, Gemini CLI, OpenCode, ...) or set ANTHROPIC_API_KEY."
fi

say "Building Rust services (lifeos-api + lifeos-drain + lifeos-cli, release)"
(cd services && cargo build --release -p lifeos-api -p lifeos-drain -p lifeos-cli)

say "Installing frontend dependencies"
(cd frontend && npm install)

say "Done"
cat <<'EOF'

Next steps:
  ./run.sh                # start the API + web app (dev mode)
  ./run.sh --with-drain   # also start the heavy-lane job worker

The API auto-creates and migrates lifeos.db on first boot - no manual DB
setup. Optional integrations (Turso sync, Telegram bot, Nango OAuth, ...)
are documented in .env.example and docs/MANUAL-SETUP.md.
EOF
