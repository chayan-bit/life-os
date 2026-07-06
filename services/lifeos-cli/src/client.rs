//! Thin authed HTTP client over lifeos-api. The CLI NEVER touches the DB
//! directly - every operation round-trips through localhost HTTP.

use crate::config::Resolved;
use reqwest::Method;
use serde_json::Value;

/// Structured failure with a stable exit code (see `exit_code`).
#[derive(Debug)]
pub enum CliError {
    /// Could not reach the API (connection refused, DNS, timeout).
    Connection(String),
    /// API responded with a non-2xx status.
    Api { status: u16, body: String },
    /// Response body could not be parsed as JSON.
    Parse(String),
    /// Local/usage error (bad input, IO).
    Local(String),
}

impl CliError {
    pub fn exit_code(&self) -> i32 {
        match self {
            CliError::Local(_) => 2,
            CliError::Api { .. } => 3,
            CliError::Connection(_) => 4,
            CliError::Parse(_) => 5,
        }
    }

    pub fn message(&self) -> String {
        match self {
            CliError::Connection(m) => format!(
                "cannot reach lifeos-api: {m}\nhint: is it running? `cargo run -p lifeos-api`"
            ),
            CliError::Api { status, body } => format!("API error {status}: {body}"),
            CliError::Parse(m) => format!("could not parse API response: {m}"),
            CliError::Local(m) => m.clone(),
        }
    }
}

pub struct Client {
    http: reqwest::Client,
    settings: Resolved,
}

impl Client {
    pub fn new(settings: Resolved) -> Self {
        Self {
            http: reqwest::Client::new(),
            settings,
        }
    }

    /// Issue a request and return the parsed JSON body on success.
    pub async fn request(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
    ) -> Result<Value, CliError> {
        let url = format!("{}{}", self.settings.api_url, path);
        let mut req = self.http.request(method, &url);

        let filtered: Vec<(&str, String)> = query
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .cloned()
            .collect();
        if !filtered.is_empty() {
            req = req.query(&filtered);
        }
        if let Some(token) = &self.settings.token {
            req = req.bearer_auth(token);
        }
        if let Some(ws) = &self.settings.workspace {
            req = req.header("X-Workspace-Id", ws);
        }
        if let Some(json) = body {
            req = req.json(&json);
        }

        let resp = req
            .send()
            .await
            .map_err(|e| CliError::Connection(e.to_string()))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| CliError::Connection(e.to_string()))?;

        if !status.is_success() {
            return Err(CliError::Api {
                status: status.as_u16(),
                body: text,
            });
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|e| CliError::Parse(e.to_string()))
    }

    /// Like `request`, but returns the raw response body instead of parsing
    /// it as JSON - for endpoints like `/api/vcs/checkout` that return file
    /// bytes directly.
    pub async fn request_raw(&self, method: Method, path: &str, query: &[(&str, String)]) -> Result<Vec<u8>, CliError> {
        let url = format!("{}{}", self.settings.api_url, path);
        let mut req = self.http.request(method, &url);

        let filtered: Vec<(&str, String)> = query.iter().filter(|(_, v)| !v.is_empty()).cloned().collect();
        if !filtered.is_empty() {
            req = req.query(&filtered);
        }
        if let Some(token) = &self.settings.token {
            req = req.bearer_auth(token);
        }
        if let Some(ws) = &self.settings.workspace {
            req = req.header("X-Workspace-Id", ws);
        }

        let resp = req.send().await.map_err(|e| CliError::Connection(e.to_string()))?;
        let status = resp.status();
        let bytes = resp.bytes().await.map_err(|e| CliError::Connection(e.to_string()))?;

        if !status.is_success() {
            return Err(CliError::Api {
                status: status.as_u16(),
                body: String::from_utf8_lossy(&bytes).to_string(),
            });
        }
        Ok(bytes.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{settings, unreachable_url, MockServer};

    #[test]
    fn exit_codes_are_stable_per_variant() {
        assert_eq!(CliError::Local("x".into()).exit_code(), 2);
        assert_eq!(CliError::Api { status: 404, body: "x".into() }.exit_code(), 3);
        assert_eq!(CliError::Connection("x".into()).exit_code(), 4);
        assert_eq!(CliError::Parse("x".into()).exit_code(), 5);
    }

    #[test]
    fn messages_surface_the_useful_detail() {
        assert!(CliError::Connection("refused".into()).message().contains("refused"));
        assert!(CliError::Connection("refused".into()).message().contains("lifeos-api"));
        assert_eq!(
            CliError::Api { status: 404, body: "not found".into() }.message(),
            "API error 404: not found"
        );
        assert_eq!(CliError::Local("bad input".into()).message(), "bad input");
    }

    #[tokio::test]
    async fn request_sends_bearer_token_and_workspace_header() {
        let server = MockServer::start("200 OK", r#"{"ok":true}"#);
        let mut cfg = settings(&server.base_url);
        cfg.token = Some("secret-token".into());
        cfg.workspace = Some("ws1".into());
        let client = Client::new(cfg);

        let result = client.request(Method::GET, "/api/entity", &[], None).await.unwrap();

        assert_eq!(result, serde_json::json!({"ok": true}));
        let req = server.last_request().to_lowercase();
        assert!(req.starts_with("get /api/entity"), "unexpected request line: {req}");
        assert!(req.contains("authorization: bearer secret-token"));
        assert!(req.contains("x-workspace-id: ws1"));
    }

    #[tokio::test]
    async fn request_omits_auth_headers_when_unset() {
        let server = MockServer::start("200 OK", r#"{"ok":true}"#);
        let client = Client::new(settings(&server.base_url));

        client.request(Method::GET, "/api/health", &[], None).await.unwrap();

        let req = server.last_request().to_lowercase();
        assert!(!req.contains("authorization"));
        assert!(!req.contains("x-workspace-id"));
    }

    #[tokio::test]
    async fn request_sends_the_json_body_it_was_given() {
        let server = MockServer::start("200 OK", r#"{"id":"ent_1"}"#);
        let client = Client::new(settings(&server.base_url));

        let body = serde_json::json!({"module": "tasks", "type": "task"});
        let result = client
            .request(Method::POST, "/api/entity", &[], Some(body))
            .await
            .unwrap();

        assert_eq!(result["id"], "ent_1");
        let req = server.last_request();
        assert!(req.starts_with("POST /api/entity"), "unexpected request line: {req}");
        assert!(req.contains(r#""module":"tasks""#));
        assert!(req.contains(r#""type":"task""#));
    }

    #[tokio::test]
    async fn request_filters_out_empty_query_values_but_keeps_non_empty_ones() {
        let server = MockServer::start("200 OK", "[]");
        let client = Client::new(settings(&server.base_url));

        client
            .request(
                Method::GET,
                "/api/entity",
                &[("module", "tasks".to_string()), ("status", String::new())],
                None,
            )
            .await
            .unwrap();

        let req = server.last_request();
        let request_line = req.lines().next().unwrap_or_default();
        assert!(request_line.contains("module=tasks"), "missing module param: {request_line}");
        assert!(!request_line.contains("status="), "empty param leaked through: {request_line}");
    }

    #[tokio::test]
    async fn non_2xx_status_becomes_a_cli_api_error() {
        let server = MockServer::start("404 Not Found", r#"{"error":"no such entity"}"#);
        let client = Client::new(settings(&server.base_url));

        let err = client.request(Method::GET, "/api/entity/nope", &[], None).await.unwrap_err();

        match err {
            CliError::Api { status, body } => {
                assert_eq!(status, 404);
                assert!(body.contains("no such entity"));
            }
            other => panic!("expected CliError::Api, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_success_body_parses_as_null() {
        let server = MockServer::start("200 OK", "");
        let client = Client::new(settings(&server.base_url));

        let result = client.request(Method::DELETE, "/api/entity/x", &[], None).await.unwrap();

        assert_eq!(result, Value::Null);
    }

    #[tokio::test]
    async fn non_json_success_body_becomes_a_parse_error() {
        let server = MockServer::start("200 OK", "not json");
        let client = Client::new(settings(&server.base_url));

        let err = client.request(Method::GET, "/api/entity", &[], None).await.unwrap_err();

        assert!(matches!(err, CliError::Parse(_)));
    }

    #[tokio::test]
    async fn connection_failure_becomes_a_cli_connection_error() {
        let client = Client::new(settings(&unreachable_url()));

        let err = client.request(Method::GET, "/api/health", &[], None).await.unwrap_err();

        assert!(matches!(err, CliError::Connection(_)));
    }

    #[tokio::test]
    async fn request_raw_returns_bytes_on_success_and_api_error_on_failure() {
        let server = MockServer::start("200 OK", "raw-bytes");
        let client = Client::new(settings(&server.base_url));

        let bytes = client.request_raw(Method::GET, "/api/vcs/checkout", &[]).await.unwrap();
        assert_eq!(bytes, b"raw-bytes");

        let server = MockServer::start("500 Internal Server Error", "boom");
        let client = Client::new(settings(&server.base_url));
        let err = client.request_raw(Method::GET, "/api/vcs/checkout", &[]).await.unwrap_err();
        match err {
            CliError::Api { status, body } => {
                assert_eq!(status, 500);
                assert_eq!(body, "boom");
            }
            other => panic!("expected CliError::Api, got {other:?}"),
        }
    }
}
