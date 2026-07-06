//! entity / edge / event / job commands - the generic data-plane verbs.
//! These are READS and non-destructive WRITES only. No order/trade verbs exist
//! anywhere in this CLI by design (trading is read-only for every agent).

use crate::cli::{EdgeCmd, EntityCmd, EventCmd, JobCmd};
use crate::client::{CliError, Client};
use crate::output::Output;
use reqwest::Method;
use serde_json::{Map, Value};

/// Parse an optional `--attrs '{...}'` JSON string into a Value object.
fn parse_attrs(raw: &Option<String>) -> Result<Option<Value>, CliError> {
    match raw {
        None => Ok(None),
        Some(s) => {
            let v: Value = serde_json::from_str(s)
                .map_err(|e| CliError::Local(format!("--attrs is not valid JSON: {e}")))?;
            if !v.is_object() {
                return Err(CliError::Local("--attrs must be a JSON object".into()));
            }
            Ok(Some(v))
        }
    }
}

fn insert_opt(map: &mut Map<String, Value>, key: &str, val: &Option<String>) {
    if let Some(v) = val {
        map.insert(key.into(), Value::String(v.clone()));
    }
}

pub async fn entity(client: &Client, out: Output, cmd: EntityCmd) -> Result<(), CliError> {
    match cmd {
        EntityCmd::Create {
            module,
            r#type,
            title,
            status,
            parent_id,
            attrs,
        } => {
            let mut body = Map::new();
            body.insert("module".into(), Value::String(module));
            body.insert("type".into(), Value::String(r#type));
            insert_opt(&mut body, "title", &title);
            insert_opt(&mut body, "status", &status);
            insert_opt(&mut body, "parent_id", &parent_id);
            if let Some(a) = parse_attrs(&attrs)? {
                body.insert("attrs".into(), a);
            }
            let v = client
                .request(Method::POST, "/api/entity", &[], Some(Value::Object(body)))
                .await?;
            let id = v.get("id").and_then(Value::as_str).unwrap_or("?");
            out.ok(&format!("created entity {id}"), &v);
        }
        EntityCmd::Get { id } => {
            let v = client
                .request(Method::GET, &format!("/api/entity/{id}"), &[], None)
                .await?;
            out.ok("", &v);
        }
        EntityCmd::List {
            module,
            r#type,
            status,
            parent_id,
            limit,
            offset,
        } => {
            let q = vec![
                ("module", module.unwrap_or_default()),
                ("type", r#type.unwrap_or_default()),
                ("status", status.unwrap_or_default()),
                ("parent_id", parent_id.unwrap_or_default()),
                ("limit", limit.map(|n| n.to_string()).unwrap_or_default()),
                ("offset", offset.map(|n| n.to_string()).unwrap_or_default()),
            ];
            let v = client.request(Method::GET, "/api/entity", &q, None).await?;
            out.ok(&count_summary("entities", &v), &v);
        }
        EntityCmd::Update {
            id,
            title,
            status,
            attrs,
        } => {
            let mut body = Map::new();
            insert_opt(&mut body, "title", &title);
            insert_opt(&mut body, "status", &status);
            if let Some(a) = parse_attrs(&attrs)? {
                body.insert("attrs".into(), a);
            }
            if body.is_empty() {
                return Err(CliError::Local(
                    "nothing to update: pass --title, --status, or --attrs".into(),
                ));
            }
            let v = client
                .request(
                    Method::PATCH,
                    &format!("/api/entity/{id}"),
                    &[],
                    Some(Value::Object(body)),
                )
                .await?;
            out.ok(&format!("updated entity {id}"), &v);
        }
    }
    Ok(())
}

pub async fn edge(client: &Client, out: Output, cmd: EdgeCmd) -> Result<(), CliError> {
    match cmd {
        EdgeCmd::Create {
            src_id,
            rel,
            dst_id,
            dst_ref,
            state,
        } => {
            let mut body = Map::new();
            body.insert("src_id".into(), Value::String(src_id));
            body.insert("rel".into(), Value::String(rel));
            insert_opt(&mut body, "dst_id", &dst_id);
            insert_opt(&mut body, "dst_ref", &dst_ref);
            insert_opt(&mut body, "state", &state);
            let v = client
                .request(Method::POST, "/api/edge", &[], Some(Value::Object(body)))
                .await?;
            let id = v.get("id").and_then(Value::as_str).unwrap_or("?");
            out.ok(&format!("created edge {id}"), &v);
        }
        EdgeCmd::List {
            src_id,
            dst_id,
            rel,
            state,
            limit,
        } => {
            let q = vec![
                ("src_id", src_id.unwrap_or_default()),
                ("dst_id", dst_id.unwrap_or_default()),
                ("rel", rel.unwrap_or_default()),
                ("state", state.unwrap_or_default()),
                ("limit", limit.map(|n| n.to_string()).unwrap_or_default()),
            ];
            let v = client.request(Method::GET, "/api/edge", &q, None).await?;
            out.ok(&count_summary("edges", &v), &v);
        }
        EdgeCmd::Update { id, state } => {
            let body = serde_json::json!({ "state": state });
            let v = client
                .request(Method::PATCH, &format!("/api/edge/{id}"), &[], Some(body))
                .await?;
            out.ok(&format!("updated edge {id}"), &v);
        }
    }
    Ok(())
}

pub async fn event(client: &Client, out: Output, cmd: EventCmd) -> Result<(), CliError> {
    match cmd {
        EventCmd::Create {
            r#type,
            entity_id,
            actor,
            attrs,
        } => {
            let mut body = Map::new();
            body.insert("type".into(), Value::String(r#type));
            insert_opt(&mut body, "entity_id", &entity_id);
            insert_opt(&mut body, "actor", &actor);
            if let Some(a) = parse_attrs(&attrs)? {
                body.insert("attrs".into(), a);
            }
            let v = client
                .request(Method::POST, "/api/event", &[], Some(Value::Object(body)))
                .await?;
            let id = v.get("id").and_then(Value::as_str).unwrap_or("?");
            out.ok(&format!("appended event {id}"), &v);
        }
        EventCmd::List {
            r#type,
            entity_id,
            limit,
        } => {
            let q = vec![
                ("type", r#type.unwrap_or_default()),
                ("entity_id", entity_id.unwrap_or_default()),
                ("limit", limit.map(|n| n.to_string()).unwrap_or_default()),
            ];
            let v = client.request(Method::GET, "/api/event", &q, None).await?;
            out.ok(&count_summary("events", &v), &v);
        }
    }
    Ok(())
}

pub async fn job(client: &Client, out: Output, cmd: JobCmd) -> Result<(), CliError> {
    match cmd {
        JobCmd::Create {
            kind,
            payload,
            priority,
        } => {
            let payload_val = match payload {
                Some(s) => serde_json::from_str(&s)
                    .map_err(|e| CliError::Local(format!("--payload is not valid JSON: {e}")))?,
                None => Value::Object(Map::new()),
            };
            let mut body = Map::new();
            body.insert("kind".into(), Value::String(kind));
            body.insert("payload".into(), payload_val);
            if let Some(p) = priority {
                body.insert("priority".into(), Value::Number(p.into()));
            }
            let v = client
                .request(Method::POST, "/api/job", &[], Some(Value::Object(body)))
                .await?;
            let id = v.get("id").and_then(Value::as_str).unwrap_or("?");
            out.ok(&format!("enqueued job {id}"), &v);
        }
        JobCmd::List {
            status,
            kind,
            limit,
        } => {
            let q = vec![
                ("status", status.unwrap_or_default()),
                ("kind", kind.unwrap_or_default()),
                ("limit", limit.map(|n| n.to_string()).unwrap_or_default()),
            ];
            let v = client.request(Method::GET, "/api/jobs", &q, None).await?;
            out.ok(&count_summary("jobs", &v), &v);
        }
    }
    Ok(())
}

fn count_summary(noun: &str, v: &Value) -> String {
    match v.as_array() {
        Some(a) => format!("{} {noun}", a.len()),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{EdgeCmd, EntityCmd, EventCmd, JobCmd};
    use crate::client::Client;
    use crate::output::Output;
    use crate::test_support::{settings, MockServer};

    const OUT: Output = Output { json: false };

    #[tokio::test]
    async fn entity_create_posts_module_type_and_optional_fields() {
        let server = MockServer::start("200 OK", r#"{"id":"ent_1"}"#);
        let client = Client::new(settings(&server.base_url));

        entity(
            &client,
            OUT,
            EntityCmd::Create {
                module: "tasks".into(),
                r#type: "task".into(),
                title: Some("write tests".into()),
                status: None,
                parent_id: None,
                attrs: Some(r#"{"due":1700000000}"#.into()),
            },
        )
        .await
        .unwrap();

        let req = server.last_request();
        assert!(req.starts_with("POST /api/entity"), "unexpected request line: {req}");
        assert!(req.contains(r#""module":"tasks""#));
        assert!(req.contains(r#""type":"task""#));
        assert!(req.contains(r#""title":"write tests""#));
        assert!(req.contains(r#""due":1700000000"#));
    }

    #[tokio::test]
    async fn entity_create_rejects_non_object_attrs_without_calling_the_api() {
        let server = MockServer::start("200 OK", r#"{"id":"ent_1"}"#);
        let client = Client::new(settings(&server.base_url));

        let err = entity(
            &client,
            OUT,
            EntityCmd::Create {
                module: "tasks".into(),
                r#type: "task".into(),
                title: None,
                status: None,
                parent_id: None,
                attrs: Some("[1,2,3]".into()),
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(err, CliError::Local(_)));
    }

    #[tokio::test]
    async fn entity_get_issues_a_get_to_the_entity_by_id_path() {
        let server = MockServer::start("200 OK", r#"{"id":"ent_1"}"#);
        let client = Client::new(settings(&server.base_url));

        entity(&client, OUT, EntityCmd::Get { id: "ent_1".into() }).await.unwrap();

        let req = server.last_request();
        assert!(req.starts_with("GET /api/entity/ent_1"), "unexpected request line: {req}");
    }

    #[tokio::test]
    async fn entity_list_forwards_filters_as_query_params() {
        let server = MockServer::start("200 OK", "[]");
        let client = Client::new(settings(&server.base_url));

        entity(
            &client,
            OUT,
            EntityCmd::List {
                module: Some("tasks".into()),
                r#type: None,
                status: Some("open".into()),
                parent_id: None,
                limit: Some(10),
                offset: None,
            },
        )
        .await
        .unwrap();

        let req = server.last_request();
        let request_line = req.lines().next().unwrap_or_default();
        assert!(request_line.starts_with("GET /api/entity?"), "unexpected request line: {request_line}");
        assert!(request_line.contains("module=tasks"));
        assert!(request_line.contains("status=open"));
        assert!(request_line.contains("limit=10"));
        assert!(!request_line.contains("parent_id="));
    }

    #[tokio::test]
    async fn entity_update_rejects_an_empty_update_without_calling_the_api() {
        let server = MockServer::start("200 OK", r#"{"id":"ent_1"}"#);
        let client = Client::new(settings(&server.base_url));

        let err = entity(
            &client,
            OUT,
            EntityCmd::Update { id: "ent_1".into(), title: None, status: None, attrs: None },
        )
        .await
        .unwrap_err();

        assert!(matches!(err, CliError::Local(_)));
    }

    #[tokio::test]
    async fn entity_update_patches_the_entity_by_id_path() {
        let server = MockServer::start("200 OK", r#"{"id":"ent_1"}"#);
        let client = Client::new(settings(&server.base_url));

        entity(
            &client,
            OUT,
            EntityCmd::Update {
                id: "ent_1".into(),
                title: None,
                status: Some("done".into()),
                attrs: None,
            },
        )
        .await
        .unwrap();

        let req = server.last_request();
        assert!(req.starts_with("PATCH /api/entity/ent_1"), "unexpected request line: {req}");
        assert!(req.contains(r#""status":"done""#));
    }

    #[tokio::test]
    async fn edge_create_posts_src_rel_and_optional_dst() {
        let server = MockServer::start("200 OK", r#"{"id":"edge_1"}"#);
        let client = Client::new(settings(&server.base_url));

        edge(
            &client,
            OUT,
            EdgeCmd::Create {
                src_id: "ent_1".into(),
                rel: "blocks".into(),
                dst_id: Some("ent_2".into()),
                dst_ref: None,
                state: None,
            },
        )
        .await
        .unwrap();

        let req = server.last_request();
        assert!(req.starts_with("POST /api/edge"), "unexpected request line: {req}");
        assert!(req.contains(r#""src_id":"ent_1""#));
        assert!(req.contains(r#""rel":"blocks""#));
        assert!(req.contains(r#""dst_id":"ent_2""#));
    }

    #[tokio::test]
    async fn event_create_posts_type_and_parsed_attrs() {
        let server = MockServer::start("200 OK", r#"{"id":"evt_1"}"#);
        let client = Client::new(settings(&server.base_url));

        event(
            &client,
            OUT,
            EventCmd::Create {
                r#type: "task.completed".into(),
                entity_id: Some("ent_1".into()),
                actor: None,
                attrs: None,
            },
        )
        .await
        .unwrap();

        let req = server.last_request();
        assert!(req.starts_with("POST /api/event"), "unexpected request line: {req}");
        assert!(req.contains(r#""type":"task.completed""#));
        assert!(req.contains(r#""entity_id":"ent_1""#));
    }

    #[tokio::test]
    async fn job_create_rejects_invalid_payload_json_without_calling_the_api() {
        let server = MockServer::start("200 OK", r#"{"id":"job_1"}"#);
        let client = Client::new(settings(&server.base_url));

        let err = job(
            &client,
            OUT,
            JobCmd::Create { kind: "action".into(), payload: Some("not json".into()), priority: None },
        )
        .await
        .unwrap_err();

        assert!(matches!(err, CliError::Local(_)));
    }

    #[tokio::test]
    async fn job_list_hits_the_plural_jobs_endpoint() {
        let server = MockServer::start("200 OK", "[]");
        let client = Client::new(settings(&server.base_url));

        job(&client, OUT, JobCmd::List { status: None, kind: None, limit: None }).await.unwrap();

        let req = server.last_request();
        assert!(req.starts_with("GET /api/jobs"), "unexpected request line: {req}");
    }
}
