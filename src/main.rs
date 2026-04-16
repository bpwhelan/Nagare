mod anki;
mod api;
mod config;
mod media;
mod media_server;
mod mining;
mod session;
mod subtitle;

use crate::anki::{AnkiClient, AnkiStatus, NewCardEvent};
use crate::api::AppState;
use crate::config::Config;
use crate::media_server::{EmbyClient, JellyfinClient, PlexClient, ServerMap};
use crate::mining::{AppDatabase, EnrichmentDialogState};
use crate::session::{SessionManager, SessionState};
use axum::body::Body;
use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{RwLock, broadcast, mpsc, watch};
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};
use tracing::{error, info, warn};

#[derive(rust_embed::RustEmbed)]
#[folder = "frontend/dist"]
struct EmbeddedFrontend;

/// Build media server clients for every enabled service in the current config.
pub fn build_media_servers(config: &Config) -> ServerMap {
    let mut servers = ServerMap::new();

    if let Some(emby) = config.emby.as_ref().filter(|cfg| cfg.enabled) {
        info!("Using Emby server at {}", emby.url);
        servers.insert(
            crate::config::MediaServerKind::Emby,
            Arc::new(EmbyClient::new(&emby.url, &emby.api_key)),
        );
    }

    if let Some(jf) = config.jellyfin.as_ref().filter(|cfg| cfg.enabled) {
        info!("Using Jellyfin server at {}", jf.url);
        servers.insert(
            crate::config::MediaServerKind::Jellyfin,
            Arc::new(JellyfinClient::new_jellyfin(&jf.url, &jf.api_key)),
        );
    }

    if let Some(plex) = config.plex.as_ref().filter(|cfg| cfg.enabled) {
        info!("Using Plex server at {}", plex.url);
        servers.insert(
            crate::config::MediaServerKind::Plex,
            Arc::new(PlexClient::new(&plex.url, &plex.token)),
        );
    }

    servers
}

fn asset_response(path: &str) -> Option<Response> {
    let file = EmbeddedFrontend::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();

    let mut response = Response::new(Body::from(file.data));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(mime.as_ref())
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    Some(response)
}

async fn serve_embedded_frontend(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let asset_path = if path.is_empty() { "index.html" } else { path };

    if let Some(response) = asset_response(asset_path) {
        return response;
    }

    if !asset_path.contains('.') {
        if let Some(response) = asset_response("index.html") {
            return response;
        }
    }

    (StatusCode::NOT_FOUND, "Not Found").into_response()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nagare=info,tower_http=info".into()),
        )
        .init();

    info!("Nagare starting up...");

    // Data directory for persistent state
    let data_dir = std::env::var("DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("data"));

    if let Err(e) = std::fs::create_dir_all(&data_dir) {
        error!("Failed to create data directory {:?}: {}", data_dir, e);
    } else {
        info!("Data directory: {:?}", data_dir);
    }

    let db = Arc::new(
        AppDatabase::new(
            data_dir.join("nagare.sqlite"),
            Some(data_dir.join("mining_history.sqlite")),
        )
        .await?,
    );

    // Load config from SQLite, importing legacy JSON on first run if needed.
    let config = db
        .load_config_or_default(data_dir.join("config.json"))
        .await?;

    info!("Target language: {}", config.target_language);
    info!("Media access mode: {:?}", config.media_access_mode);

    let listen_address = config.listen_address.clone();

    // Create media server client (may be None if unconfigured)
    let servers = build_media_servers(&config);

    if servers.is_empty() {
        warn!("No enabled media servers configured — configure one via the web UI");
    }

    // Create AnkiConnect client
    let anki_client = Arc::new(AnkiClient::new(&config.anki.url));
    let config = Arc::new(RwLock::new(config));
    let anki_status = Arc::new(RwLock::new(AnkiStatus::default()));

    // Session state channel
    let initial_state = SessionState {
        sessions: Vec::new(),
        active_session_id: None,
        now_playing: None,
    };
    let (session_tx, session_rx) = watch::channel(initial_state);

    // New card events are processed centrally, then broadcast as prepared dialog payloads.
    let (raw_card_tx, raw_card_rx) = mpsc::channel::<NewCardEvent>(64);
    let (card_tx, _) = broadcast::channel::<EnrichmentDialogState>(64);

    // Create session manager
    let session_manager = Arc::new(
        SessionManager::new(
            Arc::new(RwLock::new(servers)),
            config.clone(),
            session_tx,
            data_dir.clone(),
            db.clone(),
        )
        .await?,
    );

    let subtitles = session_manager.subtitles();
    let subtitle_history = session_manager.subtitle_history();
    let history = session_manager.history();

    // Build shared app state
    let app_state = Arc::new(AppState {
        config: config.clone(),
        db: db.clone(),
        session_manager: session_manager.clone(),
        servers: session_manager.servers(),
        anki_client: Arc::new(RwLock::new(anki_client)),
        anki_status: anki_status.clone(),
        enhancement_status: Arc::new(RwLock::new(None)),
        session_rx,
        new_card_tx: card_tx.clone(),
        subtitles,
        subtitle_history,
        history,
        pending_enrichments: Arc::new(RwLock::new(Vec::new())),
    });

    // Start background tasks
    let sm = session_manager.clone();
    tokio::spawn(async move {
        session::run_session_poller(sm).await;
    });

    let poller_config = config.clone();
    let poller_anki = app_state.anki_client.clone();
    let poller_status = anki_status.clone();
    let poller_card_tx = raw_card_tx;
    tokio::spawn(async move {
        anki::run_anki_poller(poller_anki, poller_config, poller_status, poller_card_tx).await;
    });

    let processor_state = app_state.clone();
    tokio::spawn(async move {
        api::run_new_card_processor(processor_state, raw_card_rx).await;
    });

    // Build router
    let api_router = api::create_router(app_state);

    let app = if let Ok(frontend_dir) = std::env::var("FRONTEND_DIR") {
        info!("Serving frontend from disk at {}", frontend_dir);
        let index_path = PathBuf::from(&frontend_dir).join("index.html");
        axum::Router::new()
            .merge(api_router)
            .fallback_service(
                ServeDir::new(frontend_dir)
                    .append_index_html_on_directories(true)
                    .not_found_service(ServeFile::new(index_path)),
            )
            .layer(CorsLayer::permissive())
    } else {
        info!("Serving embedded frontend assets");
        axum::Router::new()
            .merge(api_router)
            .fallback(serve_embedded_frontend)
            .layer(CorsLayer::permissive())
    };

    let listen_addr: SocketAddr = listen_address
        .parse()
        .unwrap_or_else(|_| "0.0.0.0:9470".parse().unwrap());

    info!("Listening on http://{}", listen_addr);

    let listener = tokio::net::TcpListener::bind(listen_addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
