//! Runtime configuration, resolved from environment variables with safe defaults.
//!
//! The single DB-token owner reads everything it needs here so the rest of the
//! code never touches `std::env` directly.

use std::net::SocketAddr;

/// The seeded personal workspace. Used as the tenant fallback when a request
/// carries no explicit workspace (the current frontend does this on some calls).
pub const DEFAULT_WORKSPACE: &str = "default-personal-workspace";

#[derive(Clone, Debug)]
pub struct Config {
    /// libSQL/SQLite file path for the canonical DB (embedded replica on the Mac).
    pub db_path: String,
    /// Canonical Turso primary URL. When set (with `turso_token`), `db_path`
    /// becomes an embedded replica syncing against it; otherwise the canonical DB
    /// is a pure local file (fully offline - the personal-Mac default).
    pub turso_url: Option<String>,
    /// Auth token for the Turso primary. Held only by this single DB-token owner.
    pub turso_token: Option<String>,
    /// Background pull interval (seconds) for the embedded replica.
    pub sync_interval_secs: u64,
    /// Separate, NEVER-synced SQLite file holding derived/search state (FTS5 +
    /// sqlite-vec). Physically distinct from `db_path` so it can never be pushed
    /// to the primary (libSQL has no table-level sync-exclusion). See DATA-MODEL §5.
    pub derived_db_path: String,
    /// Address the local API binds to. Localhost-only by design (single-owner).
    pub bind_addr: SocketAddr,
    /// HMAC secret for signing/verifying `key_token` JWTs.
    pub jwt_secret: String,
    /// When true (default, local-first), an unauthenticated `x-workspace-id`
    /// header or body `workspace_id` is trusted outright - the current
    /// single-user Mac behavior. When false (shared/SaaS deployments), any
    /// explicit workspace value is honored only alongside a verified JWT, and
    /// the JWT's own `workspace_id` claim is authoritative: a mismatching
    /// header/body value is rejected rather than silently overridden. See
    /// docs/SECURITY.md.
    pub trust_workspace_header: bool,
    /// Working directory agent CLIs are spawned in (OpenDesign-style managed cwd).
    pub agent_cwd: Option<String>,
    /// Hard ceiling on how long a single agent invocation may run.
    pub agent_timeout_secs: u64,
    /// Directory the JS agent runtime (`agent/run.js`, `scaffold.js`) is spawned
    /// from - the `server/` dir in the repo. `/api/agent` shells into it the same
    /// way `lifeos-drain` shells `node scaffold.js` (issue #122).
    pub server_dir: String,
    /// Base URL of the self-hosted Nango instance (infra/nango/). `None` means
    /// no Nango deployment is configured yet - connection routes return
    /// `ApiError::NotImplemented` rather than pretending to work.
    pub nango_server_url: Option<String>,
    /// Bearer secret lifeos-api authenticates to Nango's API with. Never sent
    /// to the client, never logged (docs/SECURITY.md §1).
    pub nango_secret_key: Option<String>,
    /// Kite Connect app credentials (docs/MANUAL-SETUP.md #51). `None` means
    /// `/api/connections/kite/*` and `/api/broker/positions` return
    /// NotImplemented rather than pretending Kite is wired up.
    pub kite_api_key: Option<String>,
    pub kite_api_secret: Option<String>,
    /// AES-256-GCM master key (32 raw bytes, base64) for `connections.secret_enc`,
    /// the envelope used by non-Nango connectors (Kite now, WhatsApp in #52).
    /// `None` disables those connectors entirely; a secret is never stored unencrypted.
    pub secret_encryption_key: Option<crate::crypto::EncryptionKey>,
    /// Base URL of the self-hosted GOWA instance (infra/gowa/,
    /// docs/MANUAL-SETUP.md #52). `None` means the WhatsApp routes return
    /// NotImplemented.
    pub gowa_base_url: Option<String>,
    /// GOWA's server-wide Basic Auth credential (`"user:pass"`) - the only
    /// secret WhatsApp needs, since GOWA has no per-workspace token to mint
    /// (unlike Kite's daily access_token). Never sent to the client, never
    /// logged (docs/SECURITY.md §1).
    pub gowa_basic_auth: Option<String>,
    /// Shared secret used to verify `X-Hub-Signature-256` on inbound
    /// `/api/webhooks/whatsapp` calls - must match GOWA's own
    /// `WHATSAPP_WEBHOOK_SECRET` (infra/gowa/.env).
    pub gowa_webhook_secret: Option<String>,
    /// Path to `scripts/browser_actuator.py`, the thin CLI over the vendored
    /// `external/browser-use` submodule (docs/MANUAL-SETUP.md #54). `None`
    /// means `/api/browser/*` and `/api/connections/browser/session` return
    /// NotImplemented rather than pretending a browser actuator is wired up.
    pub browser_script_path: Option<String>,
    /// Root directory for the lifeos-vcs CAS object store (issue #81/#86).
    /// Needs no credentials, so unlike the connectors above this is always
    /// wired - see `routes/vcs.rs`.
    pub vcs_blob_root: String,
    /// ed25519 signing key for the module marketplace (issue #101,
    /// `LIFEOS_MARKETPLACE_SIGNING_SEED`, base64 32-byte seed). `None` means
    /// `/api/marketplace/publish` returns NotImplemented rather than
    /// signing with an implicit, unconfigured key.
    pub marketplace_signing_key: Option<ed25519_dalek::SigningKey>,
    /// Turso platform account API token (distinct from `turso_token`, which
    /// authenticates to one already-provisioned database) - needed to
    /// create a new database-per-workspace (issue #104). `None` means
    /// `/api/workspace/provision-db` returns NotImplemented.
    pub turso_platform_api_token: Option<String>,
    /// Turso organization slug the platform API provisions new databases
    /// under.
    pub turso_org_slug: Option<String>,
}

impl Config {
    pub fn from_env() -> Self {
        let db_path = std::env::var("LIFEOS_DB_PATH").unwrap_or_else(|_| "lifeos.db".to_string());

        // Embedded-replica sync is opt-in: only when BOTH the URL and token are set.
        let turso_url = std::env::var("TURSO_URL").ok().filter(|s| !s.is_empty());
        let turso_token = std::env::var("TURSO_TOKEN").ok().filter(|s| !s.is_empty());
        let sync_interval_secs = std::env::var("LIFEOS_SYNC_INTERVAL_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(60);
        let derived_db_path =
            std::env::var("LIFEOS_DERIVED_DB_PATH").unwrap_or_else(|_| "lifeos-derived.db".to_string());

        let bind_addr = std::env::var("LIFEOS_BIND_ADDR")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 8080)));

        let jwt_secret = std::env::var("LIFEOS_JWT_SECRET").unwrap_or_else(|_| {
            tracing::warn!(
                "LIFEOS_JWT_SECRET not set - using an insecure dev secret. Set it before any non-local use."
            );
            "lifeos-dev-insecure-secret-change-me".to_string()
        });

        let trust_workspace_header = std::env::var("LIFEOS_TRUST_WORKSPACE_HEADER")
            .ok()
            .map(|s| s != "0" && !s.eq_ignore_ascii_case("false"))
            .unwrap_or(true);
        if trust_workspace_header {
            tracing::warn!(
                "workspace header trusted - local-first mode; set LIFEOS_TRUST_WORKSPACE_HEADER=0 for shared deployments"
            );
        }

        let agent_cwd = std::env::var("LIFEOS_AGENT_CWD").ok();

        let agent_timeout_secs = std::env::var("LIFEOS_AGENT_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(180);

        let server_dir = std::env::var("LIFEOS_SERVER_DIR").unwrap_or_else(|_| "server".to_string());

        let nango_server_url = std::env::var("NANGO_SERVER_URL").ok().filter(|s| !s.is_empty());
        let nango_secret_key = std::env::var("NANGO_SECRET_KEY_DEV").ok().filter(|s| !s.is_empty());

        let kite_api_key = std::env::var("KITE_API_KEY").ok().filter(|s| !s.is_empty());
        let kite_api_secret = std::env::var("KITE_API_SECRET").ok().filter(|s| !s.is_empty());
        let secret_encryption_key = std::env::var("LIFEOS_SECRET_ENCRYPTION_KEY")
            .ok()
            .filter(|s| !s.is_empty())
            .and_then(|s| match crate::crypto::parse_key(&s) {
                Ok(key) => Some(key),
                Err(e) => {
                    tracing::error!("LIFEOS_SECRET_ENCRYPTION_KEY is set but invalid: {e} - non-Nango connectors will stay disabled");
                    None
                }
            });

        let gowa_base_url = std::env::var("GOWA_BASE_URL").ok().filter(|s| !s.is_empty());
        let gowa_basic_auth = std::env::var("GOWA_BASIC_AUTH").ok().filter(|s| !s.is_empty());
        let gowa_webhook_secret = std::env::var("GOWA_WEBHOOK_SECRET").ok().filter(|s| !s.is_empty());

        let browser_script_path = std::env::var("BROWSER_ACTUATOR_SCRIPT").ok().filter(|s| !s.is_empty());

        let vcs_blob_root = std::env::var("LIFEOS_VCS_BLOB_ROOT").unwrap_or_else(|_| "lifeos-blobs".to_string());

        let marketplace_signing_key = std::env::var("LIFEOS_MARKETPLACE_SIGNING_SEED")
            .ok()
            .filter(|s| !s.is_empty())
            .and_then(|s| match crate::marketplace_sign::parse_signing_key(&s) {
                Ok(key) => Some(key),
                Err(e) => {
                    tracing::error!("LIFEOS_MARKETPLACE_SIGNING_SEED is set but invalid: {e} - marketplace publish/sign will stay disabled");
                    None
                }
            });

        let turso_platform_api_token = std::env::var("TURSO_PLATFORM_API_TOKEN").ok().filter(|s| !s.is_empty());
        let turso_org_slug = std::env::var("TURSO_ORG_SLUG").ok().filter(|s| !s.is_empty());

        Self {
            db_path,
            turso_url,
            turso_token,
            sync_interval_secs,
            derived_db_path,
            bind_addr,
            jwt_secret,
            trust_workspace_header,
            agent_cwd,
            agent_timeout_secs,
            server_dir,
            nango_server_url,
            nango_secret_key,
            kite_api_key,
            kite_api_secret,
            secret_encryption_key,
            gowa_base_url,
            gowa_basic_auth,
            gowa_webhook_secret,
            browser_script_path,
            vcs_blob_root,
            marketplace_signing_key,
            turso_platform_api_token,
            turso_org_slug,
        }
    }
}
