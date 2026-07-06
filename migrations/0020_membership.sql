-- Workspace membership, roles, and invites (issue #146, docs/SECURITY.md §5).
--
-- `workspace_members` is the RBAC join table: it is SEEDED WITH NOTHING, so a
-- workspace that has no rows here at all is treated as legacy single-user mode
-- (resolve_role -> owner) and every existing personal deployment keeps working
-- untouched. A workspace gains explicit membership only when its owner issues
-- the first invite (the owner is materialized here at that moment). Once rows
-- exist, a caller with no row is denied in strict mode.
--
-- Distinct from the legacy `memberships` table (0002_control_plane.sql), which
-- login uses to resolve a user's primary workspace and whose role vocabulary
-- (owner|admin|member) predates this RBAC model (owner|editor|viewer|agent).
CREATE TABLE IF NOT EXISTS workspace_members (
  workspace_id TEXT NOT NULL,
  user_id      TEXT NOT NULL,
  role         TEXT NOT NULL CHECK (role IN ('owner', 'editor', 'viewer', 'agent')),
  invited_by   TEXT,
  created_at   INTEGER NOT NULL,
  PRIMARY KEY (workspace_id, user_id),
  FOREIGN KEY (workspace_id) REFERENCES workspaces(id),
  FOREIGN KEY (user_id) REFERENCES users(id)
);

CREATE INDEX IF NOT EXISTS ix_workspace_members_user ON workspace_members(user_id);

-- Invites carry only a SHA-256 `token_hash`; the raw token is returned exactly
-- once from POST /api/invite and never stored, mirroring how sessions store
-- only a refresh-token hash. `email` is the addressee for display/audit;
-- `accepted_at` is the CAS field that makes acceptance single-use.
CREATE TABLE IF NOT EXISTS invites (
  id           TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL,
  email        TEXT NOT NULL,
  role         TEXT NOT NULL CHECK (role IN ('owner', 'editor', 'viewer', 'agent')),
  token_hash   TEXT NOT NULL,
  expires_at   INTEGER NOT NULL,
  accepted_at  INTEGER,
  created_by   TEXT,
  created_at   INTEGER NOT NULL,
  FOREIGN KEY (workspace_id) REFERENCES workspaces(id)
);

CREATE INDEX IF NOT EXISTS ix_invites_workspace ON invites(workspace_id);
CREATE INDEX IF NOT EXISTS ix_invites_token ON invites(token_hash);
