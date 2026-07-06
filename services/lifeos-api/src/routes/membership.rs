//! Workspace membership, roles, and invites (issue #146, docs/SECURITY.md §5).
//!
//! - `GET  /api/members`            list members + roles (any member; reads free)
//! - `POST /api/member/:id/role`    change a member's role (owner only)
//! - `DELETE /api/member/:id`       remove a member (owner only, never the last owner)
//! - `GET  /api/invites`            list pending invites (owner only)
//! - `POST /api/invite`             create an invite (owner only)
//! - `POST /api/invite/accept`      an authenticated user joins with the invite's role
//! - `DELETE /api/invite/:id`       revoke an invite (owner only)
//!
//! Invite creation is an OWNER-TYPED action from the UI: the human is acting
//! directly, so it executes immediately with no draft/approval gate (the
//! outward-action gate exists to stop the *agent/bot* acting autonomously -
//! docs/SECURITY.md §2 / CLAUDE.md "outward or irreversible actions are
//! human-gated"; an owner clicking "invite" IS the human gate).
//!
//! Owner-only enforcement lives in each handler (via `resolve_role`) so it holds
//! in BOTH strict and local-first mode - the router's role middleware only
//! gates writes in strict mode, so it can never be the sole authority here.

use crate::audit::emit;
use crate::auth::{
    bearer_claims, hash_refresh_token, new_refresh_token, resolve_role, resolve_workspace, Role,
};
use crate::error::{ApiError, ApiResult};
use crate::ids::{new_id, now_secs};
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    http::HeaderMap,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

/// Invites are valid for 7 days.
const INVITE_TTL_SECS: i64 = 60 * 60 * 24 * 7;

/// Resolve the caller: `(workspace_id, user_id?, role)`. The user id is the
/// bearer claim's `sub` when present (always in strict mode; optional locally).
async fn caller(state: &AppState, headers: &HeaderMap) -> ApiResult<(String, Option<String>, Role)> {
    let workspace = resolve_workspace(headers, &state.config, None)?;
    let user_id = bearer_claims(headers, &state.config.jwt_secret).map(|c| c.sub);
    let role = resolve_role(&state.conn, &workspace, user_id.as_deref()).await?;
    Ok((workspace, user_id, role))
}

fn require_owner(role: Role) -> ApiResult<()> {
    if role == Role::Owner {
        Ok(())
    } else {
        Err(ApiError::Forbidden(
            "only a workspace owner can perform this action".into(),
        ))
    }
}

/// The stored role of `user_id` in `workspace`, if they are a member.
async fn member_role(state: &AppState, workspace: &str, user_id: &str) -> ApiResult<Option<Role>> {
    let mut rows = state
        .conn
        .query(
            "SELECT role FROM workspace_members WHERE workspace_id = ?1 AND user_id = ?2",
            libsql::params![workspace, user_id],
        )
        .await?;
    match rows.next().await? {
        Some(row) => {
            let s: String = row.get(0)?;
            Ok(Role::parse(&s))
        }
        None => Ok(None),
    }
}

/// How many owners the workspace currently has - the last-owner guard.
async fn owner_count(state: &AppState, workspace: &str) -> ApiResult<i64> {
    let mut rows = state
        .conn
        .query(
            "SELECT COUNT(*) FROM workspace_members WHERE workspace_id = ?1 AND role = 'owner'",
            libsql::params![workspace],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => row.get(0)?,
        None => 0,
    })
}

/// Materialize the workspace's legacy owner(s) into `workspace_members` the
/// first time it gains explicit membership (on the first invite). Without this,
/// the moment an invitee is added the original single-user owner - who only
/// exists in the legacy `memberships` table - would resolve to `Forbidden`
/// (rows exist, they have none). Idempotent (`ON CONFLICT DO NOTHING`).
async fn materialize_legacy_owners(state: &AppState, workspace: &str) -> ApiResult<()> {
    let now = now_secs();
    let mut rows = state
        .conn
        .query(
            "SELECT user_id FROM memberships WHERE workspace_id = ?1 AND role IN ('owner', 'admin')",
            libsql::params![workspace],
        )
        .await?;
    let mut owners = Vec::new();
    while let Some(row) = rows.next().await? {
        owners.push(row.get::<String>(0)?);
    }
    for uid in owners {
        state
            .conn
            .execute(
                "INSERT INTO workspace_members (workspace_id, user_id, role, invited_by, created_at) \
                 VALUES (?1, ?2, 'owner', NULL, ?3) ON CONFLICT(workspace_id, user_id) DO NOTHING",
                libsql::params![workspace, uid, now],
            )
            .await?;
    }
    Ok(())
}

pub async fn list_members(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let (workspace, _uid, role) = caller(&state, &headers).await?;
    let mut rows = state
        .conn
        .query(
            "SELECT m.user_id, m.role, m.invited_by, m.created_at, u.email, u.name \
             FROM workspace_members m LEFT JOIN users u ON u.id = m.user_id \
             WHERE m.workspace_id = ?1 ORDER BY m.created_at ASC",
            libsql::params![workspace.clone()],
        )
        .await?;
    let mut members = Vec::new();
    while let Some(row) = rows.next().await? {
        members.push(json!({
            "user_id": row.get::<String>(0)?,
            "role": row.get::<String>(1)?,
            "invited_by": row.get::<Option<String>>(2)?,
            "created_at": row.get::<i64>(3)?,
            "email": row.get::<Option<String>>(4)?,
            "name": row.get::<Option<String>>(5)?,
        }));
    }
    Ok(Json(json!({ "members": members, "your_role": role.as_str() })))
}

#[derive(Deserialize)]
pub struct RoleChange {
    role: String,
}

pub async fn set_role(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Json(req): Json<RoleChange>,
) -> ApiResult<Json<Value>> {
    let (workspace, actor, caller_role) = caller(&state, &headers).await?;
    require_owner(caller_role)?;
    let new_role = Role::parse(&req.role)
        .ok_or_else(|| ApiError::BadRequest(format!("invalid role '{}'", req.role)))?;

    let current = member_role(&state, &workspace, &user_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("'{user_id}' is not a member of this workspace")))?;
    if current == Role::Owner && new_role != Role::Owner && owner_count(&state, &workspace).await? <= 1 {
        return Err(ApiError::BadRequest(
            "cannot demote the last owner - promote another member to owner first".into(),
        ));
    }

    state
        .conn
        .execute(
            "UPDATE workspace_members SET role = ?1 WHERE workspace_id = ?2 AND user_id = ?3",
            libsql::params![new_role.as_str(), workspace.clone(), user_id.clone()],
        )
        .await?;
    emit(
        &state.conn,
        &workspace,
        "membership.role_changed",
        None,
        "user",
        &json!({ "user_id": user_id, "role": new_role.as_str(), "by": actor }),
    )
    .await?;
    Ok(Json(json!({ "user_id": user_id, "role": new_role.as_str() })))
}

pub async fn remove_member(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let (workspace, actor, caller_role) = caller(&state, &headers).await?;
    require_owner(caller_role)?;

    let current = member_role(&state, &workspace, &user_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("'{user_id}' is not a member of this workspace")))?;
    if current == Role::Owner && owner_count(&state, &workspace).await? <= 1 {
        return Err(ApiError::BadRequest(
            "cannot remove the last owner - promote another member to owner first".into(),
        ));
    }

    state
        .conn
        .execute(
            "DELETE FROM workspace_members WHERE workspace_id = ?1 AND user_id = ?2",
            libsql::params![workspace.clone(), user_id.clone()],
        )
        .await?;
    emit(
        &state.conn,
        &workspace,
        "membership.removed",
        None,
        "user",
        &json!({ "user_id": user_id, "by": actor }),
    )
    .await?;
    Ok(Json(json!({ "removed": user_id })))
}

pub async fn list_invites(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let (workspace, _uid, role) = caller(&state, &headers).await?;
    require_owner(role)?;
    let mut rows = state
        .conn
        .query(
            "SELECT id, email, role, expires_at, created_at FROM invites \
             WHERE workspace_id = ?1 AND accepted_at IS NULL ORDER BY created_at DESC",
            libsql::params![workspace],
        )
        .await?;
    let mut invites = Vec::new();
    while let Some(row) = rows.next().await? {
        invites.push(json!({
            "id": row.get::<String>(0)?,
            "email": row.get::<String>(1)?,
            "role": row.get::<String>(2)?,
            "expires_at": row.get::<i64>(3)?,
            "created_at": row.get::<i64>(4)?,
        }));
    }
    Ok(Json(json!({ "invites": invites })))
}

#[derive(Deserialize)]
pub struct InviteRequest {
    email: String,
    role: String,
    /// Optional origin used to build the returned accept URL (e.g. the app's
    /// public base). The token, not the URL, is authoritative.
    #[serde(default)]
    base_url: Option<String>,
}

pub async fn create_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<InviteRequest>,
) -> ApiResult<Json<Value>> {
    let (workspace, actor, caller_role) = caller(&state, &headers).await?;
    require_owner(caller_role)?;
    let role = Role::parse(&req.role)
        .ok_or_else(|| ApiError::BadRequest(format!("invalid role '{}'", req.role)))?;
    let email = req.email.trim();
    if email.is_empty() {
        return Err(ApiError::BadRequest("email is required".into()));
    }

    // First invite promotes this workspace out of legacy single-user mode; keep
    // the existing owner(s) as owners so they don't lose access (see helper).
    materialize_legacy_owners(&state, &workspace).await?;

    let token = new_refresh_token();
    let token_hash = hash_refresh_token(&token);
    let id = new_id("inv");
    let now = now_secs();
    let expires_at = now + INVITE_TTL_SECS;
    state
        .conn
        .execute(
            "INSERT INTO invites (id, workspace_id, email, role, token_hash, expires_at, accepted_at, created_by, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8)",
            libsql::params![
                id.clone(),
                workspace.clone(),
                email,
                role.as_str(),
                token_hash,
                expires_at,
                actor.clone(),
                now
            ],
        )
        .await?;
    emit(
        &state.conn,
        &workspace,
        "membership.invited",
        None,
        "user",
        &json!({ "invite_id": id, "email": email, "role": role.as_str(), "by": actor }),
    )
    .await?;

    let base = req.base_url.as_deref().unwrap_or("").trim_end_matches('/');
    let accept_url = format!("{base}/invite/accept?token={token}");
    // The raw token is returned exactly once here; only its hash is persisted.
    Ok(Json(json!({
        "id": id,
        "email": email,
        "role": role.as_str(),
        "expires_at": expires_at,
        "token": token,
        "accept_url": accept_url,
    })))
}

#[derive(Deserialize)]
pub struct AcceptRequest {
    token: String,
}

pub async fn accept_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<AcceptRequest>,
) -> ApiResult<Json<Value>> {
    // Joining requires a proven identity even in local-first mode - the joining
    // user is not yet a member, so `resolve_role` would (correctly) deny them;
    // the invite token, not membership, is the authority here.
    let claims = bearer_claims(&headers, &state.config.jwt_secret)
        .ok_or_else(|| ApiError::Unauthorized("you must be logged in to accept an invite".into()))?;
    let token_hash = hash_refresh_token(&req.token);
    let now = now_secs();

    let mut rows = state
        .conn
        .query(
            "SELECT id, workspace_id, role, expires_at, accepted_at, created_by FROM invites WHERE token_hash = ?1",
            libsql::params![token_hash],
        )
        .await?;
    let (invite_id, workspace, role_s, expires_at, accepted_at, created_by): (
        String,
        String,
        String,
        i64,
        Option<i64>,
        Option<String>,
    ) = match rows.next().await? {
        Some(row) => (
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
        ),
        None => return Err(ApiError::BadRequest("invalid or unknown invite token".into())),
    };
    if accepted_at.is_some() {
        return Err(ApiError::Conflict("this invite has already been accepted".into()));
    }
    if expires_at <= now {
        return Err(ApiError::BadRequest("this invite has expired".into()));
    }

    // CAS: only the first accept flips `accepted_at`, so the token is single-use
    // even under a concurrent double-tap.
    let changed = state
        .conn
        .execute(
            "UPDATE invites SET accepted_at = ?1 WHERE id = ?2 AND accepted_at IS NULL",
            libsql::params![now, invite_id.clone()],
        )
        .await?;
    if changed == 0 {
        return Err(ApiError::Conflict("this invite has already been accepted".into()));
    }

    state
        .conn
        .execute(
            "INSERT INTO workspace_members (workspace_id, user_id, role, invited_by, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(workspace_id, user_id) DO NOTHING",
            libsql::params![workspace.clone(), claims.sub.clone(), role_s.clone(), created_by, now],
        )
        .await?;
    emit(
        &state.conn,
        &workspace,
        "membership.joined",
        None,
        "user",
        &json!({ "user_id": claims.sub, "role": role_s, "invite_id": invite_id }),
    )
    .await?;
    Ok(Json(json!({ "workspace_id": workspace, "role": role_s, "status": "joined" })))
}

pub async fn revoke_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let (workspace, actor, caller_role) = caller(&state, &headers).await?;
    require_owner(caller_role)?;
    let changed = state
        .conn
        .execute(
            "DELETE FROM invites WHERE id = ?1 AND workspace_id = ?2",
            libsql::params![id.clone(), workspace.clone()],
        )
        .await?;
    if changed == 0 {
        return Err(ApiError::NotFound(format!("invite '{id}' not found")));
    }
    emit(
        &state.conn,
        &workspace,
        "membership.invite_revoked",
        None,
        "user",
        &json!({ "invite_id": id, "by": actor }),
    )
    .await?;
    Ok(Json(json!({ "revoked": id })))
}
