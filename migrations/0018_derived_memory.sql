-- 0018_derived_memory.sql - FTS5 index over memory_nodes, in the NEVER-SYNCED
-- derived DB (attached as `d`), mirroring the entities_idx/entities_fts
-- pattern of 0003. Applied by bootstrap_derived (triggers/FTS DDL can't be
-- schema-qualified through an ATTACH alias). Fully rebuildable: the memory
-- projector repopulates it from memory_nodes, which are themselves replayed
-- from `events` (docs/AI-MEMORY.md §2-3).

CREATE TABLE IF NOT EXISTS memory_idx (
    rowid        INTEGER PRIMARY KEY,
    id           TEXT NOT NULL UNIQUE,
    workspace_id TEXT NOT NULL,
    content      TEXT NOT NULL,
    ts           INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_memory_idx_ws ON memory_idx (workspace_id);

CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(
    content,
    content='memory_idx',
    content_rowid='rowid',
    tokenize='porter unicode61'
);

CREATE TRIGGER IF NOT EXISTS memory_idx_ai AFTER INSERT ON memory_idx BEGIN
    INSERT INTO memory_fts(rowid, content) VALUES (new.rowid, new.content);
END;

CREATE TRIGGER IF NOT EXISTS memory_idx_ad AFTER DELETE ON memory_idx BEGIN
    INSERT INTO memory_fts(memory_fts, rowid, content)
    VALUES ('delete', old.rowid, old.content);
END;

CREATE TRIGGER IF NOT EXISTS memory_idx_au AFTER UPDATE ON memory_idx BEGIN
    INSERT INTO memory_fts(memory_fts, rowid, content)
    VALUES ('delete', old.rowid, old.content);
    INSERT INTO memory_fts(rowid, content) VALUES (new.rowid, new.content);
END;

-- Embedding-state marker for the semantic (memvec) lane. A memory node's
-- vector lives in the memvec-owned entity_vec index under a `mem:<ws>` label;
-- this table records which node ids have already been embedded so the API's
-- best-effort embed step only shells memvec for genuinely-new nodes. It lives
-- in the derived (never-synced) DB because embedding state is per-machine
-- derived state, exactly like the FTS index above. Node ids are
-- content-deterministic (BLAKE3 of workspace_id|event_id), so an embedding
-- stays valid across a rebuild and this marker never needs invalidating.
CREATE TABLE IF NOT EXISTS memory_embedded (
    id           TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    embedded_ts  INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_memory_embedded_ws ON memory_embedded (workspace_id);
