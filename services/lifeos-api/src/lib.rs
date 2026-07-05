//! `lifeos-api` library surface. The binary (`main.rs`) is a thin wrapper; the
//! whole app is here so integration tests can build the router in-process.

pub mod agents;
pub mod audit;
pub mod auth;
pub mod browser;
pub mod config;
pub mod crypto;
pub mod db;
pub mod error;
pub mod ids;
pub mod integrations;
pub mod kite;
pub mod marketplace_sign;
pub mod memvec_search;
pub mod models;
pub mod nango;
pub mod reading;
pub mod reconcile;
pub mod routes;
pub mod state;
pub mod storage;
pub mod whatsapp;

use crate::browser::{BrowserActuator, ProcessBrowserActuator};
use crate::config::Config;
use crate::kite::{HttpKiteClient, KiteClient};
use crate::nango::{HttpNangoClient, NangoClient};
use crate::reading::{ArticleFetcher, HttpArticleFetcher};
use crate::state::AppState;
use crate::whatsapp::{HttpWhatsAppClient, WhatsAppClient};
use std::sync::Arc;

/// Hard ceiling on a single browser-use subprocess invocation.
const BROWSER_ACTUATOR_TIMEOUT_SECS: u64 = 300;

fn nango_from_config(config: &Config) -> Option<Arc<dyn NangoClient>> {
    match (&config.nango_server_url, &config.nango_secret_key) {
        (Some(url), Some(key)) => Some(Arc::new(HttpNangoClient::new(url.clone(), key.clone()))),
        _ => None,
    }
}

fn kite_from_config(config: &Config) -> Option<Arc<dyn KiteClient>> {
    match (&config.kite_api_key, &config.kite_api_secret, &config.secret_encryption_key) {
        (Some(key), Some(secret), Some(_)) => {
            Some(Arc::new(HttpKiteClient::new(key.clone(), secret.clone())))
        }
        _ => None,
    }
}

fn whatsapp_from_config(config: &Config) -> Option<Arc<dyn WhatsAppClient>> {
    match (&config.gowa_base_url, &config.gowa_basic_auth) {
        (Some(url), Some(auth)) => Some(Arc::new(HttpWhatsAppClient::new(url.clone(), auth.clone()))),
        _ => None,
    }
}

fn browser_from_config(config: &Config) -> Option<Arc<dyn BrowserActuator>> {
    config
        .browser_script_path
        .as_ref()
        .map(|path| Arc::new(ProcessBrowserActuator::new(path.clone(), BROWSER_ACTUATOR_TIMEOUT_SECS)) as Arc<dyn BrowserActuator>)
}

/// Fetching a public URL needs no owned credentials, so this is always
/// wired in production - `config` is unused, kept for signature symmetry
/// with the other `*_from_config` constructors.
fn reading_from_config(_config: &Config) -> Option<Arc<dyn ArticleFetcher>> {
    Some(Arc::new(HttpArticleFetcher::new()))
}

/// Open the DB, detect agents, and assemble shared state from a config.
/// Wires the real HTTP Nango client, Kite client, WhatsApp (GOWA) client,
/// and browser actuator whenever their respective config is present; `None`
/// otherwise (routes then surface a clean NotImplemented - see
/// docs/MANUAL-SETUP.md #47-54).
pub async fn build_state(config: Config) -> Result<AppState, libsql::Error> {
    let nango = nango_from_config(&config);
    let kite = kite_from_config(&config);
    let whatsapp = whatsapp_from_config(&config);
    let browser = browser_from_config(&config);
    let reading = reading_from_config(&config);
    build_state_with_clients(config, nango, kite, whatsapp, browser, reading).await
}

/// Same as `build_state`, but with an explicit Nango client (or `None`) -
/// lets tests inject `nango::mock::MockNangoClient` instead of hitting a real
/// deployment. Other clients are still wired from `config` if configured.
pub async fn build_state_with_nango(
    config: Config,
    nango: Option<Arc<dyn NangoClient>>,
) -> Result<AppState, libsql::Error> {
    let kite = kite_from_config(&config);
    let whatsapp = whatsapp_from_config(&config);
    let browser = browser_from_config(&config);
    let reading = reading_from_config(&config);
    build_state_with_clients(config, nango, kite, whatsapp, browser, reading).await
}

/// Same as `build_state`, but with an explicit Kite client (or `None`) - lets
/// tests inject `kite::mock::MockKiteClient`. Other clients are still wired
/// from `config` if configured.
pub async fn build_state_with_kite(
    config: Config,
    kite: Option<Arc<dyn KiteClient>>,
) -> Result<AppState, libsql::Error> {
    let nango = nango_from_config(&config);
    let whatsapp = whatsapp_from_config(&config);
    let browser = browser_from_config(&config);
    let reading = reading_from_config(&config);
    build_state_with_clients(config, nango, kite, whatsapp, browser, reading).await
}

/// Same as `build_state`, but with an explicit WhatsApp client (or `None`) -
/// lets tests inject `whatsapp::mock::MockWhatsAppClient`. Other clients
/// are still wired from `config` if configured.
pub async fn build_state_with_whatsapp(
    config: Config,
    whatsapp: Option<Arc<dyn WhatsAppClient>>,
) -> Result<AppState, libsql::Error> {
    let nango = nango_from_config(&config);
    let kite = kite_from_config(&config);
    let browser = browser_from_config(&config);
    let reading = reading_from_config(&config);
    build_state_with_clients(config, nango, kite, whatsapp, browser, reading).await
}

/// Same as `build_state`, but with an explicit browser actuator (or `None`),
/// letting tests inject `browser::mock::MockBrowserActuator`. Other clients
/// are still wired from `config` if configured.
pub async fn build_state_with_browser(
    config: Config,
    browser: Option<Arc<dyn BrowserActuator>>,
) -> Result<AppState, libsql::Error> {
    let nango = nango_from_config(&config);
    let kite = kite_from_config(&config);
    let whatsapp = whatsapp_from_config(&config);
    let reading = reading_from_config(&config);
    build_state_with_clients(config, nango, kite, whatsapp, browser, reading).await
}

/// Same as `build_state`, but with an explicit article fetcher (or `None`),
/// letting tests inject `reading::mock::MockArticleFetcher` instead of
/// hitting the real network. Other clients are still wired from `config` if
/// configured.
pub async fn build_state_with_reading(
    config: Config,
    reading: Option<Arc<dyn ArticleFetcher>>,
) -> Result<AppState, libsql::Error> {
    let nango = nango_from_config(&config);
    let kite = kite_from_config(&config);
    let whatsapp = whatsapp_from_config(&config);
    let browser = browser_from_config(&config);
    build_state_with_clients(config, nango, kite, whatsapp, browser, reading).await
}

async fn build_state_with_clients(
    config: Config,
    nango: Option<Arc<dyn NangoClient>>,
    kite: Option<Arc<dyn KiteClient>>,
    whatsapp: Option<Arc<dyn WhatsAppClient>>,
    browser: Option<Arc<dyn BrowserActuator>>,
    reading: Option<Arc<dyn ArticleFetcher>>,
) -> Result<AppState, libsql::Error> {
    let db = db::connect(&config).await?;
    let agents = agents::detect();
    let vcs_store = Arc::new(lifeos_vcs::ObjectStore::new(config.vcs_blob_root.clone()));
    Ok(AppState {
        conn: Arc::new(db.conn),
        database: Arc::new(db.database),
        config,
        agents: Arc::new(agents),
        nango,
        kite,
        whatsapp,
        browser,
        reading,
        vcs_store,
    })
}
