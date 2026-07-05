#!/usr/bin/env python3
"""memvec for Life OS - the semantic half of hybrid recall.

Adapted from the harness `~/.claude/bin/memvec.py` (reused unchanged in spirit:
MiniLM-384 + sqlite-vec vec0), retargeted to the un-synced `lifeos-derived.db`
and given a CLI so `lifeos-api` can shell out to it. The Rust side owns the FTS5
lexical half and fuses results with RRF; this owns `entity_vec` because the vec0
loadable extension is NOT available to the Rust libSQL build.

Honest degradation: if sentence-transformers / sqlite-vec are not installed, we
print a clear message to stderr and exit non-zero so the API falls back to
lexical-only search.

Subcommands (all take --db <derived.db>):
  query      --workspace W --text "..." [--k 20]          -> prints `id<TAB>distance` per line
  embed      --workspace W --id ENT --text "..."           -> upsert one entity's vector
  rebuild    --canonical lifeos.db [--workspace W]         -> re-embed all entities
  cache-put  --workspace W --key K --prompt "..." --completion "..." [--model M]
             -> upsert an llm_cache row + embed the prompt under id `llmcache:<key>`
             (issue #127, docs/AGENT-CORE.md §10 - the two-layer LLM cache)
  cache-get  --workspace W --key K --prompt "..." [--threshold 0.08] [--no-semantic]
             -> prints one JSON line: {"hit":"exact"|"semantic"|null, "completion":..., "distance":...}
"""
import argparse
import json
import struct
import sys
import time

DIM = 384
MODEL_NAME = "all-MiniLM-L6-v2"

# LLM cache tuning (issue #127). The id prefix mirrors Tool-RAG's `tool:`
# convention (issue #123) so both live in the same `entity_vec` index without a
# separate table.
LLM_CACHE_ID_PREFIX = "llmcache:"
CACHE_QUERY_K = 5

_MODEL = None


def die(msg: str, code: int = 3):
    print(f"memvec: {msg}", file=sys.stderr)
    sys.exit(code)


def model():
    """Lazily load the embedder; fail loudly (non-zero) if deps are missing."""
    global _MODEL
    if _MODEL is None:
        try:
            from sentence_transformers import SentenceTransformer
        except ImportError:
            die("sentence-transformers not installed (pip install sentence-transformers)")
        _MODEL = SentenceTransformer(MODEL_NAME)
    return _MODEL


def embed_text(text: str) -> bytes:
    vec = model().encode([text], normalize_embeddings=True)[0]
    return struct.pack(f"{DIM}f", *vec)


def connect(db_path: str):
    """Open the derived DB with sqlite-vec loaded and the vec schema ensured."""
    import sqlite3

    try:
        import sqlite_vec
    except ImportError:
        die("sqlite-vec not installed (pip install sqlite-vec)")

    conn = sqlite3.connect(db_path)
    conn.enable_load_extension(True)
    sqlite_vec.load(conn)
    conn.enable_load_extension(False)
    conn.execute(
        f"CREATE VIRTUAL TABLE IF NOT EXISTS entity_vec USING vec0(embedding float[{DIM}])"
    )
    conn.execute(
        "CREATE TABLE IF NOT EXISTS entity_vec_meta "
        "(rowid INTEGER PRIMARY KEY, id TEXT UNIQUE, workspace_id TEXT)"
    )
    conn.execute(
        "CREATE TABLE IF NOT EXISTS llm_cache ("
        "key TEXT PRIMARY KEY, workspace TEXT NOT NULL, model TEXT, "
        "prompt TEXT NOT NULL, completion TEXT NOT NULL, created_at INTEGER)"
    )
    return conn


def upsert(conn, workspace_id: str, entity_id: str, text: str):
    row = conn.execute(
        "SELECT rowid FROM entity_vec_meta WHERE id = ?", (entity_id,)
    ).fetchone()
    blob = embed_text(text)
    if row:
        rowid = row[0]
        conn.execute("UPDATE entity_vec SET embedding = ? WHERE rowid = ?", (blob, rowid))
        conn.execute(
            "UPDATE entity_vec_meta SET workspace_id = ? WHERE rowid = ?",
            (workspace_id, rowid),
        )
    else:
        cur = conn.execute("INSERT INTO entity_vec(embedding) VALUES (?)", (blob,))
        rowid = cur.lastrowid
        conn.execute(
            "INSERT INTO entity_vec_meta(rowid, id, workspace_id) VALUES (?, ?, ?)",
            (rowid, entity_id, workspace_id),
        )
    conn.commit()


def cmd_query(args):
    conn = connect(args.db)
    blob = embed_text(args.text)
    rows = conn.execute(
        "SELECT v.rowid, v.distance FROM entity_vec v "
        "WHERE v.embedding MATCH ? AND k = ? ORDER BY v.distance",
        (blob, args.k),
    ).fetchall()
    for rowid, distance in rows:
        meta = conn.execute(
            "SELECT id, workspace_id FROM entity_vec_meta WHERE rowid = ?", (rowid,)
        ).fetchone()
        if not meta:
            continue
        ent_id, ws = meta
        if args.workspace and ws != args.workspace:
            continue
        print(f"{ent_id}\t{distance}")


def cmd_embed(args):
    conn = connect(args.db)
    upsert(conn, args.workspace, args.id, args.text)


def cmd_rebuild(args):
    import sqlite3

    conn = connect(args.db)
    conn.execute("DELETE FROM entity_vec")
    conn.execute("DELETE FROM entity_vec_meta")
    conn.commit()

    src = sqlite3.connect(args.canonical)
    sql = "SELECT id, workspace_id, coalesce(title,''), attrs FROM entities"
    params = ()
    if args.workspace:
        sql += " WHERE workspace_id = ?"
        params = (args.workspace,)
    n = 0
    for ent_id, ws, title, attrs in src.execute(sql, params).fetchall():
        text = f"{title} {attrs}".strip()
        upsert(conn, ws, ent_id, text)
        n += 1
    print(f"memvec: embedded {n} entities", file=sys.stderr)


def cmd_cache_put(args):
    """Upsert one exact-hash cache row + embed the prompt for layer 2.

    Hashing happens on the JS side (server/agent/llmCache.js); this only
    stores/looks up by the key it is given.
    """
    conn = connect(args.db)
    now = int(time.time())
    conn.execute(
        "INSERT INTO llm_cache(key, workspace, model, prompt, completion, created_at) "
        "VALUES (?, ?, ?, ?, ?, ?) "
        "ON CONFLICT(key) DO UPDATE SET workspace = excluded.workspace, "
        "model = excluded.model, prompt = excluded.prompt, "
        "completion = excluded.completion, created_at = excluded.created_at",
        (args.key, args.workspace, args.model, args.prompt, args.completion, now),
    )
    conn.commit()
    upsert(conn, args.workspace, f"{LLM_CACHE_ID_PREFIX}{args.key}", args.prompt)


def _exact_cache_hit(conn, workspace, key):
    row = conn.execute(
        "SELECT completion FROM llm_cache WHERE key = ? AND workspace = ?",
        (key, workspace),
    ).fetchone()
    return row[0] if row else None


def _semantic_cache_hit(conn, workspace, prompt, threshold):
    blob = embed_text(prompt)
    rows = conn.execute(
        "SELECT v.rowid, v.distance FROM entity_vec v "
        "WHERE v.embedding MATCH ? AND k = ? ORDER BY v.distance",
        (blob, CACHE_QUERY_K),
    ).fetchall()
    for rowid, distance in rows:
        if distance > threshold:
            continue
        meta = conn.execute(
            "SELECT id, workspace_id FROM entity_vec_meta WHERE rowid = ?", (rowid,)
        ).fetchone()
        if not meta:
            continue
        ent_id, ws = meta
        if not ent_id.startswith(LLM_CACHE_ID_PREFIX) or ws != workspace:
            continue
        cache_key = ent_id[len(LLM_CACHE_ID_PREFIX):]
        completion = _exact_cache_hit(conn, workspace, cache_key)
        if completion is None:
            continue
        return completion, distance
    return None, None


def cmd_cache_get(args):
    conn = connect(args.db)

    exact = _exact_cache_hit(conn, args.workspace, args.key)
    if exact is not None:
        print(json.dumps({"hit": "exact", "completion": exact}))
        return

    if not args.no_semantic:
        completion, distance = _semantic_cache_hit(conn, args.workspace, args.prompt, args.threshold)
        if completion is not None:
            print(json.dumps({"hit": "semantic", "completion": completion, "distance": distance}))
            return

    print(json.dumps({"hit": None}))


def main():
    parser = argparse.ArgumentParser(description="Life OS memvec (semantic recall)")
    parser.add_argument("--db", required=True, help="path to lifeos-derived.db")
    sub = parser.add_subparsers(dest="cmd", required=True)

    q = sub.add_parser("query")
    q.add_argument("--workspace")
    q.add_argument("--text", required=True)
    q.add_argument("--k", type=int, default=20)
    q.set_defaults(func=cmd_query)

    e = sub.add_parser("embed")
    e.add_argument("--workspace", required=True)
    e.add_argument("--id", required=True)
    e.add_argument("--text", required=True)
    e.set_defaults(func=cmd_embed)

    r = sub.add_parser("rebuild")
    r.add_argument("--canonical", required=True)
    r.add_argument("--workspace")
    r.set_defaults(func=cmd_rebuild)

    cp = sub.add_parser("cache-put")
    cp.add_argument("--workspace", required=True)
    cp.add_argument("--key", required=True)
    cp.add_argument("--prompt", required=True)
    cp.add_argument("--completion", required=True)
    cp.add_argument("--model")
    cp.set_defaults(func=cmd_cache_put)

    cg = sub.add_parser("cache-get")
    cg.add_argument("--workspace", required=True)
    cg.add_argument("--key", required=True)
    cg.add_argument("--prompt", required=True)
    cg.add_argument("--threshold", type=float, default=0.08)
    cg.add_argument("--no-semantic", action="store_true")
    cg.set_defaults(func=cmd_cache_get)

    args = parser.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
