# syntax=docker/dockerfile:1
#
# lifeos-node: the remote agent container (issue #141).
#
# Bundles the heavy-lane job worker (`lifeos-drain`) + the Node harness
# (`server/`, needed by the agent loop) + the Claude Code CLI, so the heavy lane
# runs anywhere (home server, VPS) with the Mac off. Auth is keyless: a
# subscription OAuth token minted by `claude setup-token`, passed in as
# CLAUDE_CODE_OAUTH_TOKEN. `lifeos-agents::detect()` finds `claude` on PATH and
# routes every AI lane through it - no ANTHROPIC_API_KEY anywhere.
#
# Build from the repo root:
#   docker build -f infra/lifeos-node/Dockerfile -t lifeos-node .
# (docker compose in this dir sets `context: ../..` so this resolves for you.)

# ----------------------------------------------------------------- rust builder
FROM rust:1.94-slim-bookworm AS rust-builder
WORKDIR /build
# libSQL's bundled SQLite (the `core` feature) compiles C, so we need a C
# toolchain; TLS is rustls (no OpenSSL), so pkg-config/libssl are not required.
RUN apt-get update \
 && apt-get install -y --no-install-recommends build-essential \
 && rm -rf /var/lib/apt/lists/*
# The whole workspace is needed - lifeos-drain path-depends on its sibling
# crates (ingest/vcs/agents/pipelines/memory/actions).
COPY services/ ./services/
WORKDIR /build/services
RUN cargo build --release -p lifeos-drain

# --------------------------------------------------------------------- runtime
FROM node:20-bookworm-slim AS runtime

# ca-certificates: TLS to Turso / Telegram / Anthropic. git: the Claude Code CLI
# expects it available. procps: `pgrep` for the healthcheck.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates git procps \
 && rm -rf /var/lib/apt/lists/*

# Claude Code CLI, installed globally so `claude` lands on PATH (/usr/local/bin)
# for `lifeos-agents::detect()`. NOTE: the repo's nix-only package policy applies
# to the *Mac host*, not to container images - inside an image, `npm i -g` is the
# normal, reproducible way to vendor a CLI (the image itself is the pinned
# artifact). Pin the major to keep rebuilds deterministic.
RUN npm install -g @anthropic-ai/claude-code@2 \
 && npm cache clean --force

WORKDIR /app/server
# Manifests first so `npm ci` caches independently of harness source churn.
# `--omit=dev` skips vitest; Playwright's browsers are deliberately NOT installed
# (`npx playwright install` is skipped) - they are only used by the codegen
# render-smoke validators, which an untrusted node never runs.
COPY server/package.json server/package-lock.json ./
RUN npm ci --omit=dev
COPY server/ ./

# The release binary from the rust stage.
COPY --from=rust-builder /build/services/target/release/lifeos-drain /usr/local/bin/lifeos-drain

WORKDIR /app
# Where the drain shells `node build/run.js` / `scaffold.js` from (only when
# trusted; untrusted nodes never invoke it, but the path is still correct).
ENV LIFEOS_SERVER_DIR=/app/server
# Mounted-DB default location (overridden to a Turso URL via env when in remote
# mode). The compose file wires the volume to /data.
ENV LIFEOS_DB_PATH=/data/lifeos.db

# Run unprivileged. The node image ships a `node` user; give it the app + a
# writable home for the Claude CLI's config/cache and the /data mount.
RUN mkdir -p /data \
 && chown -R node:node /app /data
USER node
ENV HOME=/home/node

# The drain has no HTTP surface, so liveness = the worker process is up. The
# loop is resilient (it logs and retries claim/DB errors rather than exiting),
# so a running process is the honest health signal.
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
  CMD pgrep -x lifeos-drain > /dev/null || exit 1

ENTRYPOINT ["lifeos-drain"]
