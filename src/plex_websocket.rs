use crate::config::{Config, PlexConfig};
use crate::session::SessionManager;
use anyhow::{Context, bail};
use futures_util::StreamExt;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::Message;
use tracing::{debug, info, warn};

#[derive(Debug, Deserialize)]
struct PlexNotificationEnvelope {
    #[serde(rename = "NotificationContainer")]
    notification: PlexNotificationContainer,
}

#[derive(Debug, Deserialize)]
struct PlexNotificationContainer {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default, rename = "PlaySessionStateNotification")]
    playing: Vec<PlexPlayingNotification>,
}

#[derive(Debug, Deserialize)]
struct PlexPlayingNotification {
    #[serde(rename = "clientIdentifier")]
    client_identifier: String,
    #[serde(rename = "ratingKey")]
    rating_key: String,
    #[serde(rename = "viewOffset")]
    view_offset: Option<i64>,
    state: String,
}

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

fn websocket_request(
    plex: &PlexConfig,
) -> anyhow::Result<tokio_tungstenite::tungstenite::http::Request<()>> {
    let ws_url = websocket_url(&plex.url, &plex.token)?;
    let mut request = ws_url.into_client_request()?;
    request
        .headers_mut()
        .insert("X-Plex-Product", HeaderValue::from_static("Nagare"));
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
        .insert("X-Plex-Device", HeaderValue::from_static("Nagare"));
    request
        .headers_mut()
        .insert("X-Plex-Device-Name", HeaderValue::from_static("Nagare"));
    request.headers_mut().insert(
        "X-Plex-Client-Identifier",
        HeaderValue::from_static("nagare-plex-websocket"),
    );
    Ok(request)
}

async fn handle_message(manager: &Arc<SessionManager>, message: Message) -> anyhow::Result<()> {
    let payload = match message {
        Message::Text(text) => text.to_string(),
        Message::Binary(data) => String::from_utf8_lossy(&data).into_owned(),
        Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => return Ok(()),
        Message::Close(frame) => {
            bail!("Plex websocket closed: {:?}", frame);
        }
    };

    let envelope: PlexNotificationEnvelope = match serde_json::from_str(&payload) {
        Ok(envelope) => envelope,
        Err(error) => {
            debug!("Ignoring unparseable Plex websocket payload: {}", error);
            return Ok(());
        }
    };

    if envelope.notification.kind != "playing" {
        return Ok(());
    }

    for event in envelope.notification.playing {
        manager
            .handle_plex_playing_event(
                &event.client_identifier,
                &event.rating_key,
                event.view_offset,
                &event.state,
            )
            .await;
    }

    Ok(())
}

async fn listen_once(
    config: Arc<RwLock<Config>>,
    manager: Arc<SessionManager>,
    plex: PlexConfig,
) -> anyhow::Result<()> {
    let request = websocket_request(&plex)?;
    let (mut socket, response) = connect_async(request)
        .await
        .context("connect Plex websocket")?;

    info!("Connected to Plex websocket: {}", response.status());
    manager.set_plex_websocket_connected(true);
    manager.poll_once().await;

    loop {
        tokio::select! {
            maybe_message = socket.next() => {
                match maybe_message {
                    Some(message) => handle_message(&manager, message?).await?,
                    None => bail!("Plex websocket closed without a close frame"),
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(2)) => {
                let current_plex = config.read().await.plex.clone();
                if current_plex.as_ref() != Some(&plex) {
                    info!("Plex websocket config changed; reconnecting");
                    break;
                }
            }
        }
    }

    Ok(())
}

pub async fn run_plex_websocket_listener(
    config: Arc<RwLock<Config>>,
    manager: Arc<SessionManager>,
) {
    loop {
        let plex = config.read().await.plex.clone();
        let Some(plex) = plex.filter(|cfg| cfg.enabled) else {
            manager.set_plex_websocket_connected(false);
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        };

        if let Err(error) = listen_once(config.clone(), manager.clone(), plex).await {
            warn!("Plex websocket listener error: {}", error);
        }

        manager.set_plex_websocket_connected(false);
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_playing_notifications() {
        let payload = r#"{"NotificationContainer":{"type":"playing","size":1,"PlaySessionStateNotification":[{"sessionKey":"29","clientIdentifier":"ytgcp5ywyn8xzsyp6yybukey","guid":"","ratingKey":"144504","url":"","key":"/library/metadata/144504","viewOffset":154000,"state":"playing"}]}}"#;
        let envelope: PlexNotificationEnvelope = serde_json::from_str(payload).unwrap();
        assert_eq!(envelope.notification.kind, "playing");
        assert_eq!(envelope.notification.playing.len(), 1);
        let event = &envelope.notification.playing[0];
        assert_eq!(event.client_identifier, "ytgcp5ywyn8xzsyp6yybukey");
        assert_eq!(event.rating_key, "144504");
        assert_eq!(event.view_offset, Some(154000));
        assert_eq!(event.state, "playing");
    }
}
