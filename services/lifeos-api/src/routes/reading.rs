//! Article saving for the Reading module (issue #61, docs/MODULES.md §3.6).
//! `save` is free: fetches a URL via `reading::ArticleFetcher`, extracts
//! title/body text with the `scraper` crate (a lighter, dependency-thin
//! stand-in for the vendored Mozilla Readability.js submodule
//! `external/readability` - see the scope note in docs/MODULES.md §3.6),
//! computes a naive extractive summary, and links to any existing `topic`
//! entities whose title appears in the article. No AI subprocess is
//! triggered automatically here (that stays available via the existing
//! `POST /api/llm`, `routes/llm.rs`) - keeps `save` fast, free, and
//! deterministic to test. `highlight` is free too - capturing a quote is a
//! local, reversible action, not an outward write.

use crate::audit::emit;
use crate::auth::resolve_workspace;
use crate::db::index_entity;
use crate::error::{ApiError, ApiResult};
use crate::ids::now_secs;
use crate::models::{read_entity, Entity, COLS_ENTITY};
use crate::state::AppState;
use axum::{extract::State, http::HeaderMap, Json};
use scraper::{Html, Selector};
use serde::Deserialize;
use serde_json::{json, Value};

fn reading_or_501(state: &AppState) -> ApiResult<&dyn crate::reading::ArticleFetcher> {
    state.reading.as_deref().ok_or_else(|| ApiError::NotImplemented("article fetcher is not configured".into()))
}

fn extract(html: &str) -> (String, String) {
    let doc = Html::parse_document(html);
    let title_sel = Selector::parse("title").unwrap();
    let title = doc
        .select(&title_sel)
        .next()
        .map(|e| e.text().collect::<String>().trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "(untitled)".into());

    let body_sel = Selector::parse("article, main, body").unwrap();
    let p_sel = Selector::parse("p").unwrap();
    let text = doc
        .select(&body_sel)
        .next()
        .map(|c| c.select(&p_sel).map(|p| p.text().collect::<String>()).collect::<Vec<_>>().join("\n\n"))
        .unwrap_or_default();

    (title, text)
}

fn naive_summary(text: &str) -> String {
    let sentences: Vec<&str> = text.split(". ").map(str::trim).filter(|s| !s.is_empty()).take(2).collect();
    if sentences.is_empty() {
        String::new()
    } else {
        format!("{}.", sentences.join(". "))
    }
}

fn domain_of(url: &str) -> String {
    url.split("//").nth(1).and_then(|rest| rest.split('/').next()).unwrap_or(url).to_string()
}

#[derive(Deserialize)]
pub struct SaveArticle {
    url: String,
    workspace_id: Option<String>,
}

/// `POST /api/reading/save` - free (`read.save` is unconditionally free,
/// docs/MODULES.md §3.6): fetches, parses, naive-summarizes, and
/// topic-links a URL. Idempotent on the article (re-saving the same URL is
/// a no-op) but always returns the current entity.
pub async fn save(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SaveArticle>,
) -> ApiResult<Json<Entity>> {
    if req.url.trim().is_empty() {
        return Err(ApiError::BadRequest("url is required".into()));
    }
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;

    let html = reading_or_501(&state)?.fetch(&req.url).await?;
    let (title, text) = extract(&html);
    let word_count = text.split_whitespace().count();
    let est_minutes = (word_count / 200).max(1) as i64;
    let excerpt: String = text.chars().take(500).collect();
    let summary = naive_summary(&text);

    let domain = domain_of(&req.url);
    let source_id = format!("source_{workspace_id}_{domain}");
    upsert_source(&state, &workspace_id, &source_id, &domain).await?;

    let article_id = format!("article_{workspace_id}_{}", lifeos_vcs::hash_bytes(req.url.as_bytes()));
    let attrs = json!({
        "url": req.url,
        "title": title,
        "author": Value::Null,
        "published": Value::Null,
        "excerpt": excerpt,
        "summary": summary,
        "read_state": "unread",
        "est_minutes": est_minutes,
    });
    let is_new = upsert_article(&state, &workspace_id, &article_id, &title, &attrs).await?;
    if is_new {
        emit(&state.conn, &workspace_id, "article.saved", Some(&article_id), "api", &attrs).await.ok();
        link_topics(&state, &workspace_id, &article_id, &title, &excerpt).await?;
    }

    fetch_one(&state, &workspace_id, &article_id).await
}

async fn upsert_source(state: &AppState, workspace_id: &str, id: &str, domain: &str) -> ApiResult<bool> {
    let now = now_secs();
    let attrs_str = json!({ "domain": domain }).to_string();
    let rows_affected = state
        .conn
        .execute(
            "INSERT INTO entities \
             (id, workspace_id, module, type, parent_id, title, status, tier, attrs, source, blob_ref, created_at, updated_at) \
             VALUES (?1, ?2, 'reading', 'source', NULL, ?3, NULL, NULL, ?4, 'api', NULL, ?5, ?5) \
             ON CONFLICT(id) DO NOTHING",
            libsql::params![id, workspace_id, domain, attrs_str, now],
        )
        .await?;
    if rows_affected > 0 {
        if let Err(e) = index_entity(&state.conn, id).await {
            tracing::warn!("derived index upsert failed for {id}: {e}");
        }
    }
    Ok(rows_affected > 0)
}

async fn upsert_article(state: &AppState, workspace_id: &str, id: &str, title: &str, attrs: &Value) -> ApiResult<bool> {
    let now = now_secs();
    let attrs_str = serde_json::to_string(attrs).unwrap_or_else(|_| "{}".into());
    let rows_affected = state
        .conn
        .execute(
            "INSERT INTO entities \
             (id, workspace_id, module, type, parent_id, title, status, tier, attrs, source, blob_ref, created_at, updated_at) \
             VALUES (?1, ?2, 'reading', 'article', NULL, ?3, NULL, NULL, ?4, 'api', NULL, ?5, ?5) \
             ON CONFLICT(id) DO NOTHING",
            libsql::params![id, workspace_id, title, attrs_str, now],
        )
        .await?;
    if rows_affected > 0 {
        if let Err(e) = index_entity(&state.conn, id).await {
            tracing::warn!("derived index upsert failed for {id}: {e}");
        }
    }
    Ok(rows_affected > 0)
}

/// Naive keyword topic-link (docs/MODULES.md §3.6, `article ─derived_from→
/// topic`): an existing `topic` entity whose title appears (case-insensitive)
/// in the article's title or excerpt gets a `derived_from` edge. Real
/// semantic linking (embeddings/AI) is deferred - this is a real,
/// deterministic mechanism, not a stub.
async fn link_topics(state: &AppState, workspace_id: &str, article_id: &str, title: &str, excerpt: &str) -> ApiResult<()> {
    let haystack = format!("{title} {excerpt}").to_lowercase();
    let mut rows = state
        .conn
        .query(
            "SELECT id, title FROM entities WHERE workspace_id = ?1 AND module = 'learning' AND type = 'topic'",
            libsql::params![workspace_id],
        )
        .await?;
    let mut topics = Vec::new();
    while let Some(row) = rows.next().await? {
        let id: String = row.get(0)?;
        let topic_title: Option<String> = row.get(1)?;
        topics.push((id, topic_title));
    }

    for (topic_id, topic_title) in topics {
        let Some(topic_title) = topic_title else { continue };
        let needle = topic_title.to_lowercase();
        if needle.trim().is_empty() || !haystack.contains(&needle) {
            continue;
        }
        ensure_edge(state, workspace_id, article_id, &topic_id, "derived_from").await?;
    }
    Ok(())
}

async fn ensure_edge(state: &AppState, workspace_id: &str, src_id: &str, dst_id: &str, rel: &str) -> ApiResult<()> {
    let mut rows = state
        .conn
        .query(
            "SELECT 1 FROM edges WHERE workspace_id = ?1 AND src_id = ?2 AND dst_id = ?3 AND rel = ?4",
            libsql::params![workspace_id, src_id, dst_id, rel],
        )
        .await?;
    if rows.next().await?.is_some() {
        return Ok(());
    }
    state
        .conn
        .execute(
            "INSERT INTO edges (id, workspace_id, src_id, dst_id, dst_ref, rel, state, created_by, created_at) \
             VALUES (?1, ?2, ?3, ?4, NULL, ?5, 'accepted', 'reading-sync', ?6)",
            libsql::params![crate::ids::new_id("edg"), workspace_id, src_id, dst_id, rel, now_secs()],
        )
        .await?;
    Ok(())
}

#[derive(Deserialize)]
pub struct SaveHighlight {
    article_id: String,
    quote: String,
    t_offset: Option<i64>,
    color: Option<String>,
    workspace_id: Option<String>,
}

/// `POST /api/reading/highlight` - free: capturing a quote is local and
/// reversible, not an outward write.
pub async fn highlight(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SaveHighlight>,
) -> ApiResult<Json<Entity>> {
    if req.article_id.trim().is_empty() || req.quote.trim().is_empty() {
        return Err(ApiError::BadRequest("article_id and quote are required".into()));
    }
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;

    let exists = {
        let mut rows = state
            .conn
            .query(
                "SELECT 1 FROM entities WHERE id = ?1 AND workspace_id = ?2 AND module = 'reading' AND type = 'article'",
                libsql::params![req.article_id.clone(), workspace_id.clone()],
            )
            .await?;
        rows.next().await?.is_some()
    };
    if !exists {
        return Err(ApiError::NotFound(format!("article '{}' not found", req.article_id)));
    }

    let id = crate::ids::new_id("highlight");
    let now = now_secs();
    let attrs = json!({ "quote": req.quote, "t_offset": req.t_offset, "color": req.color });
    let attrs_str = serde_json::to_string(&attrs).unwrap_or_else(|_| "{}".into());
    state
        .conn
        .execute(
            "INSERT INTO entities \
             (id, workspace_id, module, type, parent_id, title, status, tier, attrs, source, blob_ref, created_at, updated_at) \
             VALUES (?1, ?2, 'reading', 'highlight', ?3, ?4, NULL, NULL, ?5, 'api', NULL, ?6, ?6)",
            libsql::params![id.clone(), workspace_id.clone(), req.article_id.clone(), req.quote.clone(), attrs_str, now],
        )
        .await?;
    if let Err(e) = index_entity(&state.conn, &id).await {
        tracing::warn!("derived index upsert failed for {id}: {e}");
    }
    emit(&state.conn, &workspace_id, "highlight.created", Some(&id), "api", &attrs).await.ok();

    fetch_one(&state, &workspace_id, &id).await
}

async fn fetch_one(state: &AppState, workspace_id: &str, id: &str) -> ApiResult<Json<Entity>> {
    let mut rows = state
        .conn
        .query(&format!("SELECT {COLS_ENTITY} FROM entities WHERE id = ?1 AND workspace_id = ?2"), libsql::params![id, workspace_id])
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Json(read_entity(&row)?)),
        None => Err(ApiError::NotFound(format!("entity '{id}' not found"))),
    }
}
