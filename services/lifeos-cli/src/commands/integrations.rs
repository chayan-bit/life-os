//! gmail / calendar / drive / notion / slack commands (issue #53) - thin
//! wrappers over lifeos-api's per-provider proxy tools. `list` verbs are
//! free reads; every other verb only ever creates a gated draft entity, the
//! CLI never talks to a provider directly.

use crate::cli::{CalendarCmd, DriveCmd, GmailCmd, NotionCmd, SlackCmd};
use crate::client::{CliError, Client};
use crate::output::Output;
use reqwest::Method;
use serde_json::{json, Value};

pub async fn gmail(client: &Client, out: Output, cmd: GmailCmd) -> Result<(), CliError> {
    match cmd {
        GmailCmd::List { q } => {
            let query = vec![("q", q.unwrap_or_default())];
            let v = client.request(Method::GET, "/api/gmail/list", &query, None).await?;
            out.ok("", &v);
        }
        GmailCmd::Send { to, subject, body } => {
            let payload = json!({ "to": to, "subject": subject, "body": body.unwrap_or_default() });
            let v = client.request(Method::POST, "/api/gmail/send", &[], Some(payload)).await?;
            out.ok(&drafted_summary(&v), &v);
        }
    }
    Ok(())
}

pub async fn calendar(client: &Client, out: Output, cmd: CalendarCmd) -> Result<(), CliError> {
    match cmd {
        CalendarCmd::List => {
            let v = client.request(Method::GET, "/api/calendar/list", &[], None).await?;
            out.ok("", &v);
        }
        CalendarCmd::Create { summary, start, end } => {
            let payload = json!({ "summary": summary, "start": start, "end": end });
            let v = client.request(Method::POST, "/api/calendar/create", &[], Some(payload)).await?;
            out.ok(&drafted_summary(&v), &v);
        }
    }
    Ok(())
}

pub async fn drive(client: &Client, out: Output, cmd: DriveCmd) -> Result<(), CliError> {
    match cmd {
        DriveCmd::List => {
            let v = client.request(Method::GET, "/api/drive/list", &[], None).await?;
            out.ok("", &v);
        }
        DriveCmd::Upload { name, source_ref } => {
            let payload = json!({ "name": name, "source_ref": source_ref });
            let v = client.request(Method::POST, "/api/drive/upload", &[], Some(payload)).await?;
            out.ok(&drafted_summary(&v), &v);
        }
    }
    Ok(())
}

pub async fn notion(client: &Client, out: Output, cmd: NotionCmd) -> Result<(), CliError> {
    match cmd {
        NotionCmd::List => {
            let v = client.request(Method::GET, "/api/notion/list", &[], None).await?;
            out.ok("", &v);
        }
        NotionCmd::Create { parent_id, title } => {
            let payload = json!({ "parent_id": parent_id, "title": title });
            let v = client.request(Method::POST, "/api/notion/create", &[], Some(payload)).await?;
            out.ok(&drafted_summary(&v), &v);
        }
    }
    Ok(())
}

pub async fn slack(client: &Client, out: Output, cmd: SlackCmd) -> Result<(), CliError> {
    match cmd {
        SlackCmd::List => {
            let v = client.request(Method::GET, "/api/slack/list", &[], None).await?;
            out.ok("", &v);
        }
        SlackCmd::Post { channel, text } => {
            let payload = json!({ "channel": channel, "text": text });
            let v = client.request(Method::POST, "/api/slack/post", &[], Some(payload)).await?;
            out.ok(&drafted_summary(&v), &v);
        }
    }
    Ok(())
}

fn drafted_summary(v: &Value) -> String {
    let id = v.get("id").and_then(Value::as_str).unwrap_or("?");
    format!("drafted {id} - pending approval")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::Client;
    use crate::output::Output;
    use crate::test_support::{settings, MockServer};

    const OUT: Output = Output { json: false };

    #[test]
    fn drafted_summary_reports_the_id_and_pending_state() {
        let v = json!({"id": "draft_1"});
        assert_eq!(drafted_summary(&v), "drafted draft_1 - pending approval");
        assert_eq!(drafted_summary(&json!({})), "drafted ? - pending approval");
    }

    #[tokio::test]
    async fn gmail_list_proxies_a_free_read_with_the_query() {
        let server = MockServer::start("200 OK", "[]");
        let client = Client::new(settings(&server.base_url));

        gmail(&client, OUT, GmailCmd::List { q: Some("from:boss".into()) }).await.unwrap();

        let req = server.last_request();
        let request_line = req.lines().next().unwrap_or_default();
        assert!(request_line.starts_with("GET /api/gmail/list?"), "unexpected request line: {request_line}");
        assert!(request_line.contains("q=from"));
    }

    #[tokio::test]
    async fn gmail_send_only_ever_posts_a_draft_payload() {
        let server = MockServer::start("200 OK", r#"{"id":"draft_1"}"#);
        let client = Client::new(settings(&server.base_url));

        gmail(
            &client,
            OUT,
            GmailCmd::Send { to: "a@b.com".into(), subject: "hi".into(), body: None },
        )
        .await
        .unwrap();

        let req = server.last_request();
        assert!(req.starts_with("POST /api/gmail/send"), "unexpected request line: {req}");
        assert!(req.contains(r#""to":"a@b.com""#));
        assert!(req.contains(r#""subject":"hi""#));
    }

    #[tokio::test]
    async fn slack_post_hits_the_post_endpoint_with_channel_and_text() {
        let server = MockServer::start("200 OK", r#"{"id":"draft_2"}"#);
        let client = Client::new(settings(&server.base_url));

        slack(&client, OUT, SlackCmd::Post { channel: "#general".into(), text: "hi team".into() })
            .await
            .unwrap();

        let req = server.last_request();
        assert!(req.starts_with("POST /api/slack/post"), "unexpected request line: {req}");
        assert!(req.contains("\"channel\":\"#general\""));
        assert!(req.contains(r#""text":"hi team""#));
    }
}
