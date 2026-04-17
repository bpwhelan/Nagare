#![allow(dead_code)]

#[path = "../config.rs"]
mod config;

use anyhow::{Context, bail};
use futures_util::StreamExt;
use rusqlite::{Connection, OptionalExtension};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::Message;

fn websocket_url(base_url: &str, token: &str) -> anyhow::Result<String> {
    let base_url = base_url.trim_end_matches('/');
    let ws_base = if let Some(rest) = base_url.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base_url.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if base_url.starts_with("wss://") || base_url.starts_with("ws://") {
        base_url.to_string()
    } else {
        bail!("Unsupported Plex URL scheme: {base_url}");
    };

    Ok(format!(
        "{ws_base}/:/websockets/notifications?X-Plex-Token={token}"
    ))
}

fn load_config(data_dir: &std::path::Path) -> anyhow::Result<config::Config> {
    let db_path = data_dir.join("nagare.sqlite");
    if db_path.exists() {
        let conn =
            Connection::open(&db_path).with_context(|| format!("open SQLite DB {:?}", db_path))?;
        let raw: Option<String> = conn
            .query_row(
                "SELECT config_json FROM app_config WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .context("query app_config")?;
        if let Some(raw) = raw {
            return serde_json::from_str(&raw).context("parse config_json from SQLite");
        }
    }

    Ok(config::Config::load_or_default(
        &data_dir.join("config.json"),
    ))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let data_dir = std::env::var("DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("data"));

    let cfg = load_config(&data_dir)?;
    let plex = cfg.plex.as_ref().context("Plex is not configured")?;

    if !plex.enabled {
        eprintln!("Plex is disabled in config; probing anyway");
    }

    let ws_url = websocket_url(&plex.url, &plex.token)?;
    let mut request = ws_url.into_client_request()?;
    request.headers_mut().insert(
        "X-Plex-Product",
        HeaderValue::from_static("Nagare Plex Probe"),
    );
    request.headers_mut().insert(
        "X-Plex-Version",
        HeaderValue::from_static(env!("CARGO_PKG_VERSION")),
    );
    request.headers_mut().insert(
        "X-Plex-Platform",
        HeaderValue::from_static(std::env::consts::OS),
    );
    request
        .headers_mut()
        .insert("X-Plex-Device", HeaderValue::from_static("Nagare Probe"));
    request.headers_mut().insert(
        "X-Plex-Device-Name",
        HeaderValue::from_static("Nagare Probe"),
    );
    request.headers_mut().insert(
        "X-Plex-Client-Identifier",
        HeaderValue::from_static("nagare-plex-probe"),
    );

    eprintln!("Connecting to Plex websocket using configured server");
    let (mut socket, response) = connect_async(request).await?;
    eprintln!("Connected: HTTP {}", response.status());

    while let Some(message) = socket.next().await {
        match message? {
            Message::Text(text) => {
                println!("{text}");
            }
            Message::Binary(data) => {
                println!("{}", String::from_utf8_lossy(&data));
            }
            Message::Ping(_) | Message::Pong(_) => {}
            Message::Frame(_) => {}
            Message::Close(frame) => {
                eprintln!("Closed: {:?}", frame);
                break;
            }
        }
    }

    Ok(())
}
