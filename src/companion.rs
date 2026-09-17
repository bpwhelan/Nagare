//! HTTP snapshots for userscript managers, whose privileged requests work
//! across HTTPS/HTTP origins without a page-owned WebSocket or iframe.
use crate::api::{AppState, active_subtitle_data, audio_tracks_data};
use axum::{
    Json,
    extract::{Query, State},
    http::header,
    response::IntoResponse,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    hash::{DefaultHasher, Hash, Hasher},
    sync::Arc,
};
use tokio::sync::{Mutex, broadcast};

const EVENT_LIMIT: usize = 256;

pub struct EventLog {
    epoch: String,
    entries: Mutex<Entries>,
}

#[derive(Default)]
struct Entries {
    cursor: u64,
    events: VecDeque<(u64, Value)>,
}

impl Default for EventLog {
    fn default() -> Self {
        Self {
            epoch: uuid::Uuid::new_v4().to_string(),
            entries: Mutex::new(Entries::default()),
        }
    }
}

impl Entries {
    fn push(&mut self, event: Value) {
        self.cursor += 1;
        self.events.push_back((self.cursor, event));
        if self.events.len() > EVENT_LIMIT {
            self.events.pop_front();
        }
    }

    fn after(&self, cursor: u64) -> Vec<Value> {
        self.events
            .iter()
            .filter(|(id, _)| *id > cursor)
            .map(|(_, event)| event.clone())
            .collect()
    }
}

/// Subscribe before starting producers. Retain outcomes between polls so a
/// quick enhancement or failed remote command cannot disappear between reads.
pub fn start(state: &Arc<AppState>) {
    let mut cards = state.new_card_tx.subscribe();
    let mut results = state.enhancement_result_tx.subscribe();
    let mut remote = state.remote_result_tx.subscribe();
    let state = state.clone();
    tokio::spawn(async move {
        loop {
            let event = tokio::select! {
                event = cards.recv() => event.map(|card| json!({"type": "new_card", "new_card": card})),
                event = results.recv() => event.map(|result| json!({"type": "enhancement_result", "enhancement_result": result})),
                event = remote.recv() => event.map(|result| json!({"type": "remote_result", "remote_result": result})),
            };
            match event {
                Ok(event) => state.companion_events.entries.lock().await.push(event),
                Err(broadcast::error::RecvError::Lagged(count)) => {
                    tracing::warn!(count, "Companion event receiver lagged")
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

#[derive(Default, Deserialize)]
pub struct SnapshotQuery {
    epoch: Option<String>,
    after: Option<u64>,
    subtitle_revision: Option<String>,
}

pub async fn snapshot(
    State(state): State<Arc<AppState>>,
    Query(query): Query<SnapshotQuery>,
) -> impl IntoResponse {
    // Capture events before the snapshot. Events arriving during the read are
    // delivered next time; the pending-card snapshot remains authoritative.
    let log = state.companion_events.entries.lock().await;
    let same_epoch = query.epoch.as_deref() == Some(&state.companion_events.epoch);
    let after = query
        .after
        .filter(|cursor| same_epoch && *cursor <= log.cursor);
    let events = after.map(|cursor| log.after(cursor)).unwrap_or_default();
    let cursor = log.cursor;
    drop(log);

    let session = state.session_rx.borrow().clone();
    let revision = {
        let subs = state
            .subtitles
            .read()
            .await
            .as_ref()
            .map(|t| (t.lines.len(), t.offset_ms));
        let native = state
            .native_subtitles
            .read()
            .await
            .as_ref()
            .map(|t| (t.lines.len(), t.offset_ms));
        let candidates = state.subtitle_candidates.read().await.clone();
        let np = session.now_playing.as_ref();
        // Same change signature as the WebSocket feed. Avoid copying a whole
        // audiobook's subtitle text on every playback-position poll.
        let signature = json!([
            np.map(|np| &np.history_id),
            np.and_then(|np| np.subtitle_candidate_id.as_ref()),
            np.map(|np| np.subtitle_selection_mode),
            np.map(|np| np.subtitle_loading),
            subs,
            native,
            candidates,
        ]);
        let mut hasher = DefaultHasher::new();
        signature.to_string().hash(&mut hasher);
        format!("{:x}", hasher.finish())
    };
    let subtitles = if !same_epoch || query.subtitle_revision.as_deref() != Some(&revision) {
        Some(active_subtitle_data(&state).await)
    } else {
        None
    };
    let mining = state.config.read().await.mining.clone();
    let pending = crate::api::get_pending_enrichments(State(state.clone()))
        .await
        .0;
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({
            "protocol": 1,
            "epoch": state.companion_events.epoch,
            "cursor": cursor,
            "events": events,
            "subtitle_revision": revision,
            "mining": {
                "audio_start_offset_ms": mining.audio_start_offset_ms,
                "audio_end_offset_ms": mining.audio_end_offset_ms,
                "generate_avif": mining.generate_avif,
            },
            "snapshot": {
                "type": "full_update",
                "state": session,
                "subtitles": subtitles,
                "pending_cards": pending,
                "anki_status": state.anki_status.read().await.clone(),
                "enhancement_queue": state.enhancement_queue.read().await.clone(),
                "audio_tracks": audio_tracks_data(&state).await,
            },
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcomes_between_polls_are_replayed_once_using_the_cursor() {
        let mut entries = Entries::default();
        entries.push(json!({"type": "enhancement_result", "success": false}));
        let cursor = entries.cursor;
        entries.push(json!({"type": "remote_result", "success": false}));
        assert_eq!(
            entries.after(cursor),
            vec![json!({"type": "remote_result", "success": false})]
        );
        assert!(entries.after(entries.cursor).is_empty());
    }

    #[test]
    fn disconnected_clients_cannot_grow_the_event_log_without_bound() {
        let mut entries = Entries::default();
        for n in 0..EVENT_LIMIT + 20 {
            entries.push(json!(n));
        }
        assert_eq!(entries.events.len(), EVENT_LIMIT);
        assert_eq!(entries.after(0).first(), Some(&json!(20)));
        assert_eq!(entries.cursor, (EVENT_LIMIT + 20) as u64);
    }
}
