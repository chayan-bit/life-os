-- 0019_memory_communities.sql - GraphRAG global-query read model (issue #139,
-- docs/AGENT-CORE.md §13, docs/AI-MEMORY.md). Same convention as 0017: a
-- DERIVED, rebuildable projection - here of the label-propagation clustering
-- pass over `memory_edges`/`memory_nodes`, not of the raw `events` log
-- directly. Rebuilt (delete+reinsert per workspace) on every sleep cycle, so
-- it lives in the canonical DB alongside the other memory read models.

CREATE TABLE IF NOT EXISTS memory_communities (
  id           TEXT NOT NULL,                  -- deterministic: mc_<blake3(ws|sorted member ids)>
  workspace_id TEXT NOT NULL,
  member_ids   TEXT NOT NULL DEFAULT '[]',      -- JSON array of memory_nodes.id
  summary      TEXT NOT NULL,
  size         INTEGER NOT NULL,
  built_ts     INTEGER NOT NULL,                -- when this rebuild produced the row
  PRIMARY KEY (id, workspace_id),
  FOREIGN KEY (workspace_id) REFERENCES workspaces(id)
);

CREATE INDEX IF NOT EXISTS ix_memory_communities_ws ON memory_communities(workspace_id);
