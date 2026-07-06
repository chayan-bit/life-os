//! `lifeos-api` - the single DB-token owner. A localhost-only Rust/axum service
//! that is the only process holding the canonical DB credential. Everything
//! (CLI, bot proxy, SPA) talks to the data plane through here, workspace-scoped.

use axum::http::{header, HeaderName, HeaderValue, Method};
use lifeos_api::{agents, build_state, config, routes};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "lifeos_api=info,tower_http=warn".into()),
        )
        .init();

    let config = config::Config::from_env();
    tracing::info!(db = %config.db_path, "opening canonical DB");
    let state = build_state(config.clone())
        .await
        .expect("failed to open/migrate the database");

    match agents::detect() {
        d if d.is_empty() => {
            tracing::warn!("no local agent CLIs detected on PATH - /api/llm will return 501")
        }
        d => tracing::info!(
            agents = %d.iter().map(|a| a.id.as_str()).collect::<Vec<_>>().join(", "),
            "detected local agent CLIs"
        ),
    }

    // Localhost-only API. CORS is restricted to an explicit allow-list
    // (`LIFEOS_CORS_ORIGINS`, default the Vite dev-server origins) rather than a
    // wildcard (security audit finding 3). Only the verbs and headers the SPA
    // actually uses are allowed. `allow_private_network` is intentionally NOT
    // set: the dev SPA is itself a loopback origin (localhost:5173 -> the
    // loopback API), so Chrome's Private Network Access preflight never fires.
    // Credentials are bearer-token based, not cookies, so `allow_credentials`
    // stays off - the client sends the token in an `Authorization` header.
    let allowed_origins: Vec<HeaderValue> = config::cors_origins()
        .iter()
        .filter_map(|origin| match HeaderValue::from_str(origin) {
            Ok(value) => Some(value),
            Err(_) => {
                tracing::warn!("ignoring invalid LIFEOS_CORS_ORIGINS entry: {origin}");
                None
            }
        })
        .collect();
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(allowed_origins))
        .allow_methods([Method::GET, Method::POST, Method::PATCH, Method::DELETE])
        .allow_headers([
            header::CONTENT_TYPE,
            header::AUTHORIZATION,
            HeaderName::from_static("x-workspace-id"),
        ]);

    let app = routes::router(state)
        .layer(cors)
        .layer(TraceLayer::new_for_http());

    let listener = tokio::net::TcpListener::bind(config.bind_addr)
        .await
        .unwrap_or_else(|e| panic!("failed to bind {}: {e}", config.bind_addr));
    tracing::info!("Life OS local API listening on http://{}", config.bind_addr);
    axum::serve(listener, app).await.expect("server error");
}
