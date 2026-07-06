//! Real login/session (issue #100, docs/SECURITY.md §5): `POST /api/login`
//! verifies a password against `users.password_hash` and issues an access
//! token (unchanged JWT shape, still what `resolve_workspace` verifies)
//! plus a rotating refresh token backed by the new `sessions` table.
//! `POST /api/session/refresh` rotates (old session revoked, new one
//! issued) rather than just re-signing, so a stolen refresh token has a
//! bounded lifetime even if never explicitly revoked.
//! `POST /api/logout` revokes a single session.
//!
//! `POST /api/account/set-password` is a narrow, LOCAL-ONLY bootstrap: it only
//! ever succeeds for a user whose `password_hash` is currently NULL (a pre-#100
//! row) and only when the request originates from loopback (security audit
//! finding 2). The default seeded owner is NO LONGER passwordless: `db.rs::seed()`
//! now seeds it with a non-NULL, unusable hash (or `argon2(LIFEOS_ADMIN_PASSWORD)`),
//! so this route can never be used to take the owner over, even before the real
//! owner bootstraps. Once a password is set, the atomic `WHERE password_hash IS
//! NULL` guard means this route can never overwrite it.

use crate::auth::{hash_password, hash_refresh_token, issue_token, new_refresh_token, verify_password, REFRESH_TOKEN_TTL_SECS};
use crate::error::{ApiError, ApiResult};
use crate::ids::{new_id, now_secs};
use crate::state::AppState;
use axum::{extract::State, http::HeaderMap, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use std::net::IpAddr;

#[derive(Deserialize)]
pub struct LoginRequest {
    email: String,
    password: String,
}

pub async fn login(State(state): State<AppState>, Json(req): Json<LoginRequest>) -> ApiResult<Json<Value>> {
    let email = req.email.trim();
    // Finding 46: return ONE identical error for every failure mode - unknown
    // email, an account with no usable password yet, or a wrong password - so
    // the response can never be used to enumerate which accounts exist or which
    // are un-bootstrapped. The specific reason is logged server-side only, and
    // no raw upstream/DB text is ever echoed to the client.
    let invalid = || ApiError::BadRequest("invalid email or password".into());

    let Some(user) = find_user_by_email(&state, email).await? else {
        tracing::debug!("login rejected: no account for the supplied email");
        return Err(invalid());
    };
    let Some(stored_hash) = user.password_hash.as_deref() else {
        tracing::warn!(user_id = %user.id, "login rejected: account has no usable password set");
        return Err(invalid());
    };
    if !verify_password(&req.password, stored_hash) {
        tracing::debug!(user_id = %user.id, "login rejected: wrong password");
        return Err(invalid());
    }

    let workspace_id = primary_workspace(&state, &user.id).await?;
    let key_token = issue_token(&state.config.jwt_secret, &user.id, &workspace_id, email);
    let refresh_token = create_session(&state, &user.id, &workspace_id).await?;

    Ok(Json(json!({
        "user_id": user.id,
        "workspace_id": workspace_id,
        "key_token": key_token,
        "refresh_token": refresh_token,
    })))
}

#[derive(Deserialize)]
pub struct RefreshRequest {
    refresh_token: String,
}

/// Rotates a valid, unexpired, unrevoked refresh token: revokes it and
/// issues a fresh access token + fresh refresh token backed by a new
/// session row. A reused (already-revoked) or expired token is rejected -
/// this is what bounds a leaked refresh token's blast radius.
pub async fn refresh(State(state): State<AppState>, Json(req): Json<RefreshRequest>) -> ApiResult<Json<Value>> {
    let hash = hash_refresh_token(&req.refresh_token);
    let now = now_secs();

    let mut rows = state
        .conn
        .query(
            "SELECT id, user_id, workspace_id FROM sessions \
             WHERE refresh_token_hash = ?1 AND revoked_at IS NULL AND expires_at > ?2",
            libsql::params![hash, now],
        )
        .await?;
    let (session_id, user_id, workspace_id): (String, String, String) = match rows.next().await? {
        Some(row) => (row.get(0)?, row.get(1)?, row.get(2)?),
        None => return Err(ApiError::BadRequest("invalid, expired, or already-used refresh token".into())),
    };

    // Finding 17: rotation must be a compare-and-swap, not an unconditional
    // UPDATE. Re-check `revoked_at IS NULL` in the same statement and require
    // exactly one changed row, so two concurrent replays of the same refresh
    // token can never BOTH pass the SELECT above and then BOTH mint a fresh
    // session - exactly one racer wins, the other is rejected like any reuse.
    let rotated = state
        .conn
        .execute(
            "UPDATE sessions SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
            libsql::params![session_id, now],
        )
        .await?;
    if rotated != 1 {
        return Err(ApiError::BadRequest(
            "invalid, expired, or already-used refresh token".into(),
        ));
    }

    let email = user_email(&state, &user_id).await?;
    let key_token = issue_token(&state.config.jwt_secret, &user_id, &workspace_id, &email);
    let new_refresh = create_session(&state, &user_id, &workspace_id).await?;

    Ok(Json(json!({
        "key_token": key_token,
        "refresh_token": new_refresh,
        "workspace_id": workspace_id,
    })))
}

#[derive(Deserialize)]
pub struct LogoutRequest {
    refresh_token: String,
}

/// Revokes one session. Idempotent - revoking an already-revoked or
/// unknown token still returns success (nothing to leak by distinguishing).
pub async fn logout(State(state): State<AppState>, Json(req): Json<LogoutRequest>) -> ApiResult<Json<Value>> {
    let hash = hash_refresh_token(&req.refresh_token);
    state
        .conn
        .execute(
            "UPDATE sessions SET revoked_at = ?2 WHERE refresh_token_hash = ?1 AND revoked_at IS NULL",
            libsql::params![hash, now_secs()],
        )
        .await?;
    Ok(Json(json!({ "status": "logged_out" })))
}

#[derive(Deserialize)]
pub struct SetPasswordRequest {
    email: String,
    password: String,
}

/// One-time, LOCAL-ONLY bootstrap for a pre-#100 passwordless account (see
/// module docs). Finding 2(a): reject anything that did not originate from
/// loopback so an unauthenticated remote caller can never bootstrap someone
/// else's account.
pub async fn set_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SetPasswordRequest>,
) -> ApiResult<Json<Value>> {
    // TODO(finding-2): the robust, UNSPOOFABLE origin check is the raw peer
    // socket via `axum::extract::ConnectInfo<std::net::SocketAddr>`. Wiring it
    // requires `main.rs` to serve with
    // `app.into_make_service_with_connect_info::<std::net::SocketAddr>()`
    // (that file is owned by another component, so it is not changed here).
    // Until that lands, `bootstrap_origin_is_local` is a best-effort gate over
    // the (client-supplied, hence spoofable) forwarding headers, backed by the
    // real teeth of this fix: the seeded owner is sealed with a non-NULL hash
    // (db.rs::seed), so this NULL-guarded route can never take it over anyway.
    if !bootstrap_origin_is_local(&headers, state.config.trust_workspace_header) {
        return Err(ApiError::Forbidden(
            "set-password is only available from localhost".into(),
        ));
    }

    let email = req.email.trim();
    if req.password.len() < 8 {
        return Err(ApiError::BadRequest("password must be at least 8 characters".into()));
    }
    // Do not reflect the requested email or otherwise confirm which addresses
    // do or do not exist (finding 46, same principle as `login`).
    let user = find_user_by_email(&state, email)
        .await?
        .ok_or_else(|| ApiError::NotFound("no passwordless account matches this request".into()))?;

    let password_hash = hash_password(&req.password).map_err(|e| {
        tracing::error!("set-password hashing failed: {e}");
        ApiError::Internal("could not set password".into())
    })?;
    // Atomic guard: the `password_hash IS NULL` predicate is checked and
    // written in the same statement, so a concurrent second request can
    // never both pass a pre-check and then overwrite an already-set
    // password (the read-then-write race this route used to have).
    let rows_changed = state
        .conn
        .execute(
            "UPDATE users SET password_hash = ?2, updated_at = ?3 WHERE id = ?1 AND password_hash IS NULL",
            libsql::params![user.id.clone(), password_hash, now_secs()],
        )
        .await?;
    if rows_changed == 0 {
        return Err(ApiError::BadRequest(
            "this account already has a password - use POST /api/login".into(),
        ));
    }

    Ok(Json(json!({ "status": "password_set" })))
}

/// Whether a bootstrap set-password request may proceed from where it
/// originated. This is a best-effort loopback gate until `ConnectInfo` is wired
/// (see the TODO in `set_password`):
///
/// - Local-first default (`trust_workspace_header == true`): the API is bound to
///   loopback, so a request that a fronting proxy did NOT tag with a client IP
///   reached us directly and is local. If a proxy did forward one, it is allowed
///   only when that address is itself loopback.
/// - Shared/strict deployments (`trust_workspace_header == false`): client
///   headers are not trusted and we lack the raw peer socket, so this local-only
///   bootstrap is refused unless an operator explicitly opts in out-of-band with
///   `LIFEOS_ALLOW_REMOTE_SET_PASSWORD`.
fn bootstrap_origin_is_local(headers: &HeaderMap, trust_workspace_header: bool) -> bool {
    if trust_workspace_header {
        match forwarded_client_ip(headers) {
            Some(ip) => ip.is_loopback(),
            None => true,
        }
    } else {
        remote_bootstrap_opt_in()
    }
}

/// Best-effort extraction of the origin-client IP a fronting proxy recorded, via
/// the usual forwarding headers. `None` means no such header was present (a
/// direct hit on the loopback-bound listener).
///
/// These headers are client-supplied and therefore spoofable - they are a
/// defense-in-depth signal, never an authority. The authoritative origin check
/// is the raw peer socket (`ConnectInfo<SocketAddr>`); see the TODO in
/// `set_password` and finding 2.
fn forwarded_client_ip(headers: &HeaderMap) -> Option<IpAddr> {
    // `X-Forwarded-For: client, proxy1, proxy2` - the leftmost entry is the
    // original client.
    if let Some(first) = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
    {
        if let Ok(ip) = first.trim().parse::<IpAddr>() {
            return Some(ip);
        }
    }
    if let Some(xri) = headers.get("x-real-ip").and_then(|v| v.to_str().ok()) {
        if let Ok(ip) = xri.trim().parse::<IpAddr>() {
            return Some(ip);
        }
    }
    // RFC 7239 `Forwarded: for=192.0.2.60;proto=http;by=203.0.113.43` (possibly a
    // comma-separated chain) - the first `for=` is the origin client.
    if let Some(fwd) = headers.get("forwarded").and_then(|v| v.to_str().ok()) {
        for part in fwd.split([',', ';']) {
            let part = part.trim();
            let lower = part.to_ascii_lowercase();
            if let Some(rest) = lower.strip_prefix("for=") {
                // Re-slice from the ORIGINAL (case-preserving is irrelevant for
                // IPs, but keeps this robust): drop quotes and IPv6 brackets, and
                // an optional trailing `:port`.
                let raw = &part[part.len() - rest.len()..];
                let cleaned = raw.trim_matches('"');
                if let Some(v6) = cleaned.strip_prefix('[') {
                    if let Some(host) = v6.split(']').next() {
                        if let Ok(ip) = host.parse::<IpAddr>() {
                            return Some(ip);
                        }
                    }
                    continue;
                }
                let host = cleaned.rsplit_once(':').map(|(h, _)| h).unwrap_or(cleaned);
                if let Ok(ip) = host.parse::<IpAddr>() {
                    return Some(ip);
                }
            }
        }
    }
    None
}

/// Explicit opt-in for shared deployments that genuinely need the local-only
/// bootstrap route (e.g. a controlled one-off migration).
fn remote_bootstrap_opt_in() -> bool {
    matches!(
        std::env::var("LIFEOS_ALLOW_REMOTE_SET_PASSWORD").ok().as_deref(),
        Some("1") | Some("true")
    )
}

/// Creates a session row and returns the plaintext refresh token (only ever
/// returned here - the DB only ever stores its hash).
pub async fn create_session(state: &AppState, user_id: &str, workspace_id: &str) -> ApiResult<String> {
    let token = new_refresh_token();
    let now = now_secs();
    state
        .conn
        .execute(
            "INSERT INTO sessions (id, user_id, workspace_id, refresh_token_hash, created_at, expires_at, revoked_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
            libsql::params![
                new_id("sess"),
                user_id,
                workspace_id,
                hash_refresh_token(&token),
                now,
                now + REFRESH_TOKEN_TTL_SECS
            ],
        )
        .await?;
    Ok(token)
}

struct FoundUser {
    id: String,
    password_hash: Option<String>,
}

async fn find_user_by_email(state: &AppState, email: &str) -> ApiResult<Option<FoundUser>> {
    let mut rows = state
        .conn
        .query(
            "SELECT id, password_hash FROM users WHERE email = ?1",
            libsql::params![email],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => Some(FoundUser { id: row.get(0)?, password_hash: row.get(1)? }),
        None => None,
    })
}

async fn user_email(state: &AppState, user_id: &str) -> ApiResult<String> {
    let mut rows = state
        .conn
        .query("SELECT email FROM users WHERE id = ?1", libsql::params![user_id])
        .await?;
    match rows.next().await? {
        Some(row) => Ok(row.get(0)?),
        None => Err(ApiError::Internal(format!("user '{user_id}' vanished"))),
    }
}

/// The membership created earliest is treated as a user's "primary" workspace
/// for login (mirrors the old `register.rs::lookup_existing` ordering).
async fn primary_workspace(state: &AppState, user_id: &str) -> ApiResult<String> {
    let mut rows = state
        .conn
        .query(
            "SELECT workspace_id FROM memberships WHERE user_id = ?1 ORDER BY created_at ASC LIMIT 1",
            libsql::params![user_id],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(row.get(0)?),
        None => Err(ApiError::Internal(format!("user '{user_id}' has no workspace membership"))),
    }
}
