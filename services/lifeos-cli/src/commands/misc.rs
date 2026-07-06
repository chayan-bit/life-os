//! status / metrics / file / config commands.

use crate::cli::{ConfigCmd, FileCmd};
use crate::client::{CliError, Client};
use crate::config::CliConfig;
use crate::output::Output;
use base64::Engine as _;
use reqwest::Method;
use serde_json::{json, Value};

pub async fn status(client: &Client, out: Output) -> Result<(), CliError> {
    match client.request(Method::GET, "/api/health", &[], None).await {
        Ok(v) => {
            let ws = v.get("workspace_id").and_then(Value::as_str).unwrap_or("?");
            out.ok(&format!("lifeos-api: ONLINE (workspace {ws})"), &v);
            Ok(())
        }
        Err(CliError::Connection(_)) => {
            out.ok(
                "lifeos-api: OFFLINE",
                &json!({ "status": "offline", "api": false }),
            );
            Ok(())
        }
        Err(e) => Err(e),
    }
}

pub async fn metrics(client: &Client, out: Output) -> Result<(), CliError> {
    let v = client.request(Method::GET, "/api/metrics", &[], None).await?;
    out.ok("metrics", &v);
    Ok(())
}

/// `file` maps to the real lifeos-vcs HTTP surface (issue #86, building on
/// #81-#85): commit (read a local path, upload the bytes), history (a plain
/// query over `version.created` events), checkout (retrieval by hash - no
/// separate mutating verb, and no history-rewrite verb exists at all, per
/// docs/AGENT-CONTROL.md §1).
pub async fn file(client: &Client, out: Output, cmd: FileCmd) -> Result<(), CliError> {
    match cmd {
        FileCmd::History { entity_id } => {
            let q = vec![("entity_id", entity_id)];
            let v = client.request(Method::GET, "/api/vcs/history", &q, None).await?;
            out.ok("history", &v);
        }
        FileCmd::Commit { path, message, entity_id } => {
            let bytes = std::fs::read(&path).map_err(|e| CliError::Local(format!("cannot read '{path}': {e}")))?;
            let name = std::path::Path::new(&path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| path.clone());
            let content_base64 = base64::engine::general_purpose::STANDARD.encode(&bytes);

            let mut body = serde_json::Map::new();
            body.insert("name".into(), Value::String(name));
            body.insert("content_base64".into(), Value::String(content_base64));
            if let Some(m) = message {
                body.insert("message".into(), Value::String(m));
            }
            if let Some(e) = entity_id {
                body.insert("entity_id".into(), Value::String(e));
            }
            let v = client
                .request(Method::POST, "/api/vcs/commit", &[], Some(Value::Object(body)))
                .await?;
            out.ok("committed", &v);
        }
        FileCmd::Checkout { entity_id, blob_ref, out: out_path } => {
            let q = vec![("entity_id", entity_id), ("blob_ref", blob_ref.unwrap_or_default())];
            let bytes = client.request_raw(Method::GET, "/api/vcs/checkout", &q).await?;
            std::fs::write(&out_path, &bytes).map_err(|e| CliError::Local(format!("cannot write '{out_path}': {e}")))?;
            out.ok(
                &format!("checked out {} bytes to {out_path}", bytes.len()),
                &json!({ "out": out_path, "bytes": bytes.len() }),
            );
        }
    }
    Ok(())
}

const KNOWN_KEYS: [&str; 3] = ["api_url", "token", "workspace"];

pub fn config(out: Output, cmd: ConfigCmd) -> Result<(), CliError> {
    match cmd {
        ConfigCmd::Path => {
            out.ok("", &json!({ "path": CliConfig::path().display().to_string() }));
        }
        ConfigCmd::List => {
            let cfg = CliConfig::load();
            // Mask the token so `config list` is safe to paste into a log.
            let masked = cfg.token.as_ref().map(|t| mask(t));
            out.ok(
                "",
                &json!({
                    "api_url": cfg.api_url,
                    "token": masked,
                    "workspace": cfg.workspace,
                }),
            );
        }
        ConfigCmd::Get { key } => {
            ensure_known(&key)?;
            let cfg = CliConfig::load();
            out.ok("", &json!({ &key: cfg.get(&key) }));
        }
        ConfigCmd::Set { key, value } => {
            ensure_known(&key)?;
            let mut cfg = CliConfig::load();
            cfg.set(&key, value);
            cfg.save().map_err(|e| CliError::Local(format!("could not write config: {e}")))?;
            out.ok(&format!("set {key}"), &json!({ "ok": true }));
        }
    }
    Ok(())
}

fn ensure_known(key: &str) -> Result<(), CliError> {
    if KNOWN_KEYS.contains(&key) {
        Ok(())
    } else {
        Err(CliError::Local(format!(
            "unknown config key '{key}' (known: {})",
            KNOWN_KEYS.join(", ")
        )))
    }
}

fn mask(token: &str) -> String {
    if token.len() <= 8 {
        "****".into()
    } else {
        format!("{}…{}", &token[..4], &token[token.len() - 4..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::ConfigCmd;
    use crate::client::Client;
    use crate::output::Output;
    use crate::test_support::{settings, unreachable_url, MockServer};
    use std::sync::Mutex;

    const OUT: Output = Output { json: false };

    // `ConfigCmd` reads/writes the real `$XDG_CONFIG_HOME/lifeos/config.json`
    // path (see config.rs), so config tests must not run concurrently with
    // each other or with any other test that touches that env var/file.
    static CONFIG_ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_temp_config_home<T>(f: impl FnOnce() -> T) -> T {
        let _guard = CONFIG_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("lifeos-cli-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let previous = std::env::var("XDG_CONFIG_HOME").ok();
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        let result = f();
        match previous {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        let _ = std::fs::remove_dir_all(&dir);
        result
    }

    #[tokio::test]
    async fn status_reports_online_with_the_workspace_id() {
        let server = MockServer::start("200 OK", r#"{"workspace_id":"ws1"}"#);
        let client = Client::new(settings(&server.base_url));

        status(&client, OUT).await.unwrap();

        let req = server.last_request();
        assert!(req.starts_with("GET /api/health"), "unexpected request line: {req}");
    }

    #[tokio::test]
    async fn status_reports_offline_instead_of_erroring_when_unreachable() {
        let client = Client::new(settings(&unreachable_url()));

        // A connection failure on /api/health is downgraded to an OFFLINE
        // report, not a hard error - the CLI should still exit 0 here.
        let result = status(&client, OUT).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn status_propagates_non_connection_errors() {
        let server = MockServer::start("500 Internal Server Error", "boom");
        let client = Client::new(settings(&server.base_url));

        let err = status(&client, OUT).await.unwrap_err();

        assert!(matches!(err, CliError::Api { status: 500, .. }));
    }

    #[tokio::test]
    async fn metrics_gets_the_metrics_endpoint() {
        let server = MockServer::start("200 OK", r#"{"entities":3}"#);
        let client = Client::new(settings(&server.base_url));

        metrics(&client, OUT).await.unwrap();

        let req = server.last_request();
        assert!(req.starts_with("GET /api/metrics"), "unexpected request line: {req}");
    }

    #[tokio::test]
    async fn file_history_queries_by_entity_id() {
        let server = MockServer::start("200 OK", "[]");
        let client = Client::new(settings(&server.base_url));

        file(&client, OUT, FileCmd::History { entity_id: "ent_1".into() }).await.unwrap();

        let req = server.last_request();
        let request_line = req.lines().next().unwrap_or_default();
        assert!(request_line.starts_with("GET /api/vcs/history?"), "unexpected request line: {request_line}");
        assert!(request_line.contains("entity_id=ent_1"));
    }

    #[tokio::test]
    async fn file_commit_reads_the_local_path_and_base64_encodes_it() {
        let server = MockServer::start("200 OK", r#"{"blob_ref":"abc"}"#);
        let client = Client::new(settings(&server.base_url));
        let path = std::env::temp_dir().join(format!("lifeos-cli-commit-test-{}.txt", std::process::id()));
        std::fs::write(&path, b"hello world").unwrap();

        file(
            &client,
            OUT,
            FileCmd::Commit { path: path.to_string_lossy().to_string(), message: Some("first".into()), entity_id: None },
        )
        .await
        .unwrap();
        let _ = std::fs::remove_file(&path);

        let req = server.last_request();
        assert!(req.starts_with("POST /api/vcs/commit"), "unexpected request line: {req}");
        // base64("hello world") == "aGVsbG8gd29ybGQ="
        assert!(req.contains("aGVsbG8gd29ybGQ="));
        assert!(req.contains(r#""message":"first""#));
    }

    #[tokio::test]
    async fn file_commit_reports_a_local_error_for_a_missing_path() {
        let server = MockServer::start("200 OK", r#"{"blob_ref":"abc"}"#);
        let client = Client::new(settings(&server.base_url));

        let err = file(
            &client,
            OUT,
            FileCmd::Commit { path: "/no/such/path/lifeos-cli-test".into(), message: None, entity_id: None },
        )
        .await
        .unwrap_err();

        assert!(matches!(err, CliError::Local(_)));
    }

    #[tokio::test]
    async fn file_checkout_writes_the_response_bytes_to_the_output_path() {
        let server = MockServer::start("200 OK", "file-bytes");
        let client = Client::new(settings(&server.base_url));
        let out_path = std::env::temp_dir().join(format!("lifeos-cli-checkout-test-{}.txt", std::process::id()));

        file(
            &client,
            OUT,
            FileCmd::Checkout { entity_id: "ent_1".into(), blob_ref: None, out: out_path.to_string_lossy().to_string() },
        )
        .await
        .unwrap();

        let written = std::fs::read(&out_path).unwrap();
        let _ = std::fs::remove_file(&out_path);
        assert_eq!(written, b"file-bytes");
    }

    #[test]
    fn config_get_rejects_unknown_keys() {
        with_temp_config_home(|| {
            let err = config(OUT, ConfigCmd::Get { key: "bogus".into() }).unwrap_err();
            assert!(matches!(err, CliError::Local(_)));
        });
    }

    #[test]
    fn config_set_then_get_roundtrips_through_disk() {
        with_temp_config_home(|| {
            config(OUT, ConfigCmd::Set { key: "workspace".into(), value: "ws_test".into() }).unwrap();
            let loaded = CliConfig::load();
            assert_eq!(loaded.workspace.as_deref(), Some("ws_test"));
            // `get`/`list` on a known key succeed without touching the network.
            assert!(config(OUT, ConfigCmd::Get { key: "workspace".into() }).is_ok());
            assert!(config(OUT, ConfigCmd::List).is_ok());
        });
    }

    #[test]
    fn config_path_reports_a_path_without_touching_the_network() {
        with_temp_config_home(|| {
            assert!(config(OUT, ConfigCmd::Path).is_ok());
        });
    }

    #[test]
    fn mask_hides_the_middle_of_long_tokens_and_blanks_short_ones() {
        assert_eq!(mask("sk-1234567890"), "sk-1…7890");
        assert_eq!(mask("short"), "****");
    }
}
