//! Runtime configuration, resolved from environment variables with safe defaults.
//!
//! The single DB-token owner reads everything it needs here so the rest of the
//! code never touches `std::env` directly.

use std::net::SocketAddr;

/// The seeded personal workspace. Used as the tenant fallback when a request
/// carries no explicit workspace (the current frontend does this on some calls).
pub const DEFAULT_WORKSPACE: &str = "default-personal-workspace";

/// The well-known insecure dev secret that used to be the silent JWT fallback.
/// Still recognised so strict/shared mode can hard-reject it explicitly
/// (security audit findings 6/7).
const DEV_INSECURE_JWT_SECRET: &str = "lifeos-dev-insecure-secret-change-me";

/// Minimum acceptable `LIFEOS_JWT_SECRET` length in strict/shared mode, where
/// the JWT is the tenancy boundary.
const MIN_JWT_SECRET_LEN: usize = 32;

/// Default CORS allow-list: the Vite dev-server origins. Overridden by
/// `LIFEOS_CORS_ORIGINS` (comma-separated). See [`cors_origins`].
const DEFAULT_CORS_ORIGINS: &[&str] = &["http://localhost:5173", "http://127.0.0.1:5173"];

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

        let trust_workspace_header = std::env::var("LIFEOS_TRUST_WORKSPACE_HEADER")
            .ok()
            .map(|s| s != "0" && !s.eq_ignore_ascii_case("false"))
            .unwrap_or(true);
        if trust_workspace_header {
            tracing::warn!(
                "workspace header trusted - local-first mode; set LIFEOS_TRUST_WORKSPACE_HEADER=0 for shared deployments"
            );
        }

        // Findings 6/7 (security audit): in strict/shared mode the JWT is the
        // tenancy boundary, so an absent, well-known, or too-short secret is a
        // hard startup failure. In local-first mode an unset secret mints a
        // random ephemeral one - there is never a hardcoded default.
        let jwt_secret = match resolve_jwt_secret(
            std::env::var("LIFEOS_JWT_SECRET").ok(),
            trust_workspace_header,
        ) {
            Ok(Some(secret)) => secret,
            Ok(None) => {
                let secret = random_secret();
                tracing::warn!(
                    "LIFEOS_JWT_SECRET not set - generated a random ephemeral secret; sessions will not survive a restart. Set LIFEOS_JWT_SECRET to persist them."
                );
                secret
            }
            Err(msg) => {
                tracing::error!("{msg}");
                eprintln!("FATAL: {msg}");
                std::process::exit(1);
            }
        };

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

/// Decide the effective JWT secret (security audit findings 6/7). Pure so it is
/// unit-tested without touching the environment.
///
/// - `env_secret`: the raw `LIFEOS_JWT_SECRET` value if set (empty is treated as
///   unset).
/// - `trust_workspace_header`: local-first (`true`) vs strict/shared (`false`).
///
/// Returns `Ok(Some(secret))` to use it as-is, `Ok(None)` meaning "mint a random
/// ephemeral secret" (local-first with none set), or `Err(msg)` to hard-fail at
/// startup. In strict mode an absent, well-known, or too-short secret is fatal:
/// a known secret there is total tenancy compromise.
fn resolve_jwt_secret(
    env_secret: Option<String>,
    trust_workspace_header: bool,
) -> Result<Option<String>, String> {
    match env_secret.filter(|s| !s.is_empty()) {
        Some(secret) => {
            if !trust_workspace_header {
                if secret == DEV_INSECURE_JWT_SECRET {
                    return Err(
                        "LIFEOS_JWT_SECRET is the well-known dev constant but LIFEOS_TRUST_WORKSPACE_HEADER=0 (strict/shared mode): set a unique high-entropy secret - the JWT is the tenancy boundary".into(),
                    );
                }
                if secret.len() < MIN_JWT_SECRET_LEN {
                    return Err(format!(
                        "LIFEOS_JWT_SECRET must be at least {MIN_JWT_SECRET_LEN} characters in strict/shared mode (LIFEOS_TRUST_WORKSPACE_HEADER=0): the JWT is the tenancy boundary"
                    ));
                }
            }
            Ok(Some(secret))
        }
        None => {
            if trust_workspace_header {
                Ok(None)
            } else {
                Err(
                    "LIFEOS_JWT_SECRET must be set in strict/shared mode (LIFEOS_TRUST_WORKSPACE_HEADER=0): the JWT is the tenancy boundary and a missing secret means no auth".into(),
                )
            }
        }
    }
}

/// A cryptographically random 256-bit secret, hex-encoded - the local-first
/// default when `LIFEOS_JWT_SECRET` is unset (finding 6), so the docs' "random
/// if unset" claim is now literally true.
fn random_secret() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// The CORS allow-list for the local API (finding 3), read from
/// `LIFEOS_CORS_ORIGINS` (comma-separated), defaulting to the Vite dev-server
/// origins. Deliberately a standalone reader rather than a `Config` field:
/// keeping it here upholds config.rs as the sole `std::env` boundary without
/// adding a field that would ripple through every `Config { .. }` constructor.
pub fn cors_origins() -> Vec<String> {
    parse_cors_origins(std::env::var("LIFEOS_CORS_ORIGINS").ok().as_deref())
}

/// Pure parser behind [`cors_origins`] - split on commas, trim, drop empties,
/// and fall back to the dev defaults when unset or blank.
fn parse_cors_origins(raw: Option<&str>) -> Vec<String> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        Some(list) => list
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        None => DEFAULT_CORS_ORIGINS.iter().map(|s| s.to_string()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cors_defaults_to_dev_origins_when_unset() {
        assert_eq!(
            parse_cors_origins(None),
            vec![
                "http://localhost:5173".to_string(),
                "http://127.0.0.1:5173".to_string(),
            ]
        );
    }

    #[test]
    fn cors_parses_comma_list_and_trims_whitespace() {
        assert_eq!(
            parse_cors_origins(Some(" https://app.example.com , https://admin.example.com ")),
            vec![
                "https://app.example.com".to_string(),
                "https://admin.example.com".to_string(),
            ]
        );
    }

    #[test]
    fn cors_blank_string_falls_back_to_defaults() {
        assert_eq!(parse_cors_origins(Some("   ")), parse_cors_origins(None));
    }

    #[test]
    fn jwt_local_first_unset_mints_random() {
        assert!(matches!(resolve_jwt_secret(None, true), Ok(None)));
        assert!(matches!(resolve_jwt_secret(Some(String::new()), true), Ok(None)));
    }

    #[test]
    fn jwt_local_first_accepts_any_set_secret() {
        // Local-first is permissive: even the dev constant is fine for personal use.
        assert_eq!(
            resolve_jwt_secret(Some(DEV_INSECURE_JWT_SECRET.to_string()), true).unwrap(),
            Some(DEV_INSECURE_JWT_SECRET.to_string())
        );
    }

    #[test]
    fn jwt_strict_unset_hard_fails() {
        assert!(resolve_jwt_secret(None, false).is_err());
        assert!(resolve_jwt_secret(Some(String::new()), false).is_err());
    }

    #[test]
    fn jwt_strict_dev_constant_hard_fails() {
        assert!(resolve_jwt_secret(Some(DEV_INSECURE_JWT_SECRET.to_string()), false).is_err());
    }

    #[test]
    fn jwt_strict_short_secret_hard_fails_at_boundary() {
        assert!(resolve_jwt_secret(Some("a".repeat(MIN_JWT_SECRET_LEN - 1)), false).is_err());
        assert!(resolve_jwt_secret(Some("a".repeat(MIN_JWT_SECRET_LEN)), false).is_ok());
    }

    #[test]
    fn jwt_strict_strong_secret_passes_through() {
        let strong = "b".repeat(48);
        assert_eq!(
            resolve_jwt_secret(Some(strong.clone()), false).unwrap(),
            Some(strong)
        );
    }

    #[test]
    fn random_secret_is_high_entropy_and_unique() {
        let a = random_secret();
        let b = random_secret();
        assert_ne!(a, b);
        assert!(a.len() >= MIN_JWT_SECRET_LEN);
    }
}
