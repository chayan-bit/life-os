-- Live presence (issue #150). Deliberately NOT modeled as `events` rows:
-- `events` is the append-only domain log that is never pruned (a hard rule),
-- and per-tab ~30s heartbeats would flood it forever. Presence is ephemeral
-- "who is online right now" telemetry, so it lives in its own tiny table that
-- is UPSERTED (last-write-wins on `last_seen`), one row per (workspace, user).
--
-- Reads aggregate rows whose `last_seen` is within a short TTL (default 120s);
-- stale rows simply age out of every query and are harmless to leave behind.
-- Rebuildable and disposable by nature - it carries no history worth syncing.
CREATE TABLE IF NOT EXISTS presence (
  workspace_id TEXT NOT NULL,
  user_id      TEXT NOT NULL,
  last_seen    INTEGER NOT NULL,          -- unix epoch seconds of the last heartbeat
  PRIMARY KEY (workspace_id, user_id),
  FOREIGN KEY (workspace_id) REFERENCES workspaces(id)
);

-- Backs the "active within the last TTL seconds" scan, workspace-scoped.
CREATE INDEX IF NOT EXISTS ix_presence_ws_seen ON presence(workspace_id, last_seen);
