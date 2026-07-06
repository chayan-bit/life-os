//! Node identity + job-claim observability for lifeos-drain (issue #141).
//!
//! A `lifeos-node` container runs the exact same drain loop as the Mac, only on
//! a remote host draining the canonical Turso primary. When several nodes drain
//! one primary, `claim_job`'s atomic `UPDATE ... RETURNING` (lib.rs) already
//! guarantees no double-claim - but only because a pure-remote connection runs
//! that statement server-side on the single primary (see `open_database` in
//! `main.rs`). This module adds the *observability* half: every claim writes a
//! `job.claimed` event stamped with the claiming node's id, so an operator can
//! see which node in a fleet ran which job.
//!
//! Kept out of `lib.rs` deliberately: the claim event is main-loop glue, not
//! part of the queue-core contract, and it mirrors `lib.rs`'s own private
//! `emit_event` shape rather than importing it (that function is not public,
//! and the two crates already accept a small self-contained events mirror).

use libsql::{params, Connection};
use ulid::Ulid;

/// This node's stable identifier for claim events. `LIFEOS_NODE_ID` wins;
/// otherwise the container/host `HOSTNAME` (Docker sets it to the container id
/// by default); otherwise a constant fallback so the stamp is never empty.
pub fn node_id() -> String {
    resolve_node_id(
        std::env::var("LIFEOS_NODE_ID").ok(),
        std::env::var("HOSTNAME").ok(),
    )
}

/// Pure resolver behind [`node_id`], split out so the precedence rule is
/// directly unit-testable without touching process env (which races across
/// parallel tests).
fn resolve_node_id(explicit: Option<String>, hostname: Option<String>) -> String {
    explicit
        .filter(|s| !s.is_empty())
        .or_else(|| hostname.filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "lifeos-node".to_string())
}

/// Append a `job.claimed` event stamping which node claimed a job. Mirrors the
/// `events` INSERT shape used across the codebase (id `evt_<ulid>`, JSON
/// `attrs`) so the row is indistinguishable from one `lifeos-api`/`lib.rs`
/// writes. Returns `Ok(())` once the row lands; the caller treats a failure as
/// non-fatal (a missing observability stamp must never abort the job it
/// annotates), logging rather than propagating.
pub async fn emit_claim_event(
    conn: &Connection,
    workspace_id: &str,
    job_id: &str,
    job_kind: &str,
    node_id: &str,
    now: i64,
) -> libsql::Result<()> {
    let attrs = serde_json::json!({ "node_id": node_id, "job_kind": job_kind });
    let attrs_str = serde_json::to_string(&attrs).unwrap_or_else(|_| "{}".into());
    conn.execute(
        "INSERT INTO events (id, workspace_id, ts, type, entity_id, actor, attrs) \
         VALUES (?1, ?2, ?3, 'job.claimed', ?4, ?5, ?6)",
        params![
            format!("evt_{}", Ulid::new()),
            workspace_id,
            now,
            job_id,
            node_id,
            attrs_str
        ],
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use libsql::Builder;

    #[test]
    fn explicit_node_id_wins_over_hostname() {
        assert_eq!(
            resolve_node_id(Some("home-server".into()), Some("abc123".into())),
            "home-server"
        );
    }

    #[test]
    fn falls_back_to_hostname_when_no_explicit_id() {
        assert_eq!(resolve_node_id(None, Some("abc123".into())), "abc123");
        // An empty explicit value is treated as unset, not as a valid id.
        assert_eq!(
            resolve_node_id(Some(String::new()), Some("abc123".into())),
            "abc123"
        );
    }

    #[test]
    fn falls_back_to_constant_when_nothing_is_set() {
        assert_eq!(resolve_node_id(None, None), "lifeos-node");
        assert_eq!(resolve_node_id(Some(String::new()), Some(String::new())), "lifeos-node");
    }

    #[tokio::test]
    async fn emit_claim_event_stamps_the_node_id_into_attrs() {
        let path = "test_node_claim_event.db";
        let _ = std::fs::remove_file(path);
        let db = Builder::new_local(path).build().await.unwrap();
        let conn = db.connect().unwrap();
        conn.execute(
            "CREATE TABLE events (\
                id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, ts INTEGER NOT NULL, \
                type TEXT NOT NULL, entity_id TEXT, actor TEXT NOT NULL, attrs TEXT NOT NULL)",
            (),
        )
        .await
        .unwrap();

        emit_claim_event(&conn, "ws1", "job_42", "pipeline", "home-server", 1000)
            .await
            .unwrap();

        let mut rows = conn
            .query(
                "SELECT type, entity_id, actor, attrs FROM events WHERE workspace_id='ws1'",
                (),
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        let ty: String = row.get(0).unwrap();
        let entity_id: String = row.get(1).unwrap();
        let actor: String = row.get(2).unwrap();
        let attrs: String = row.get(3).unwrap();
        assert_eq!(ty, "job.claimed");
        assert_eq!(entity_id, "job_42");
        assert_eq!(actor, "home-server");
        let parsed: serde_json::Value = serde_json::from_str(&attrs).unwrap();
        assert_eq!(parsed["node_id"], "home-server");
        assert_eq!(parsed["job_kind"], "pipeline");

        let _ = std::fs::remove_file(path);
    }
}
