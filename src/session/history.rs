//! Playback history is independent of the player selected for subtitle mining.
use super::{
    HistoryEntry, ServerSession, StreamType, scoped_history_id, session_playback_activity_at,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};

const REWATCH_CONFIRMATION: Duration = Duration::from_secs(5 * 60);
// Providers commonly repeat a position between ten-second check-ins. Repeated
// samples never qualify a rewatch, but allow those normal reporting intervals.
const MAX_PROGRESS_GAP: Duration = Duration::from_secs(25);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreviousWatch {
    pub last_position_ms: i64,
    pub last_seen: chrono::DateTime<chrono::Utc>,
}

fn completed(entry: &HistoryEntry) -> bool {
    entry.duration_ms.is_some_and(|duration| {
        duration > 0
            && i128::from(entry.last_position_ms) * 100 >= i128::from(duration) * 80
            && entry.last_position_ms.saturating_add(300_000) >= duration
    })
}

fn can_start_rewatch(entry: &HistoryEntry, position: i64) -> bool {
    completed(entry)
        && position.saturating_add(60_000) < entry.last_position_ms
        && entry
            .duration_ms
            .is_some_and(|duration| i128::from(position) * 100 < i128::from(duration) * 80)
}

struct ClientProgress {
    position: i64,
    last_advance: Instant,
    last_observed: Instant,
    paused: bool,
    watch_number: usize,
    rewatch_since: Option<(Instant, i64)>,
}

#[derive(Default)]
struct Evidence {
    changed: bool,
    advancing: bool,
    rewatch: bool,
}

impl ClientProgress {
    fn observe(
        &mut self,
        position: i64,
        paused: bool,
        history: &HistoryEntry,
        now: Instant,
    ) -> Option<Evidence> {
        if self.watch_number < history.previous_watches.len()
            && position > history.last_position_ms.saturating_add(30_000)
        {
            // Another client confirmed a rewatch. A leftover player from the
            // preceding watch cannot finish that new watch on its behalf.
            self.rewatch_since = None;
            self.position = position;
            self.last_advance = now;
            self.last_observed = now;
            self.paused = paused;
            return None;
        }
        let delta = position.saturating_sub(self.position);
        let elapsed = now.saturating_duration_since(self.last_advance);
        let elapsed_ms = elapsed.as_millis() as i128;
        // Accept normal playback rates (0.25–3x) plus reporting jitter. Seeks
        // and tiny periodic nudges cannot turn wall-clock idle time into credit.
        let continuous = !paused
            && !self.paused
            && now.saturating_duration_since(self.last_observed) <= MAX_PROGRESS_GAP
            && elapsed <= MAX_PROGRESS_GAP
            && delta > 0
            && i128::from(delta) <= elapsed_ms * 3 + 1_500
            && i128::from(delta) * 4 + 1_500 >= elapsed_ms;
        if paused
            || self.paused
            || delta < 0
            || (delta != 0 && !continuous)
            || elapsed > MAX_PROGRESS_GAP
            || self.watch_number != history.previous_watches.len()
        {
            self.rewatch_since = None;
            self.last_advance = now;
        }
        if !paused && self.rewatch_since.is_none() && can_start_rewatch(history, position) {
            self.rewatch_since = Some((now, position));
        }
        if continuous && let Some((start, start_position)) = self.rewatch_since {
            let run_ms = now.saturating_duration_since(start).as_millis() as i128;
            let run_progress = i128::from(position.saturating_sub(start_position));
            if run_progress * 4 + 1_500 < run_ms || run_progress > run_ms * 3 + 1_500 {
                self.rewatch_since =
                    can_start_rewatch(history, position).then_some((now, position));
            }
        }
        let evidence = Evidence {
            changed: delta != 0,
            advancing: delta > 0 && !paused,
            rewatch: continuous
                && self.rewatch_since.is_some_and(|(start, _)| {
                    now.saturating_duration_since(start) >= REWATCH_CONFIRMATION
                }),
        };
        if delta != 0 {
            self.last_advance = now;
        }
        self.position = position;
        self.last_observed = now;
        self.paused = paused;
        self.watch_number = history.previous_watches.len();
        Some(evidence)
    }
}

#[derive(Default)]
pub(super) struct HistoryTracker {
    clients: HashMap<String, ClientProgress>,
    owners: HashMap<String, String>,
}

impl HistoryTracker {
    /// Returns true when a new/ended watch requires an immediate database save.
    pub(super) fn update(
        &mut self,
        history: &mut HashMap<String, HistoryEntry>,
        sessions: &[ServerSession],
        now: Instant,
    ) -> bool {
        let old_client_count = self.clients.len();
        self.clients
            .retain(|id, _| sessions.iter().any(|s| s.id() == *id));
        let mut force_save = self.clients.len() < old_client_count;
        self.owners.retain(|_, id| self.clients.contains_key(id));
        let mut by_item: HashMap<String, Vec<(&ServerSession, Evidence)>> = HashMap::new();
        for active in sessions.iter().filter(|s| s.reported) {
            let Some(item) = &active.session.now_playing else {
                continue;
            };
            let history_id = scoped_history_id(active.kind, &item.item_id);
            if active.newly_observed {
                self.clients.remove(&active.id());
            }
            let Some(position) = active.session.position_ms().filter(|p| *p >= 0) else {
                // Keep newly opened items available for mining even before a
                // player supplies its clock, without carrying a rewatch streak
                // across samples for which playback cannot be verified.
                self.clients.remove(&active.id());
                by_item
                    .entry(history_id)
                    .or_default()
                    .push((active, Evidence::default()));
                continue;
            };
            let paused = active.session.play_state.is_paused;
            let watch_number = history
                .get(&history_id)
                .map_or(0, |h| h.previous_watches.len());
            let first_sample = !self.clients.contains_key(&active.id());
            let client = self
                .clients
                .entry(active.id())
                .or_insert_with(|| ClientProgress {
                    position,
                    last_advance: now,
                    last_observed: now,
                    paused,
                    watch_number,
                    rewatch_since: None,
                });
            let evidence = history.get(&history_id).map_or_else(
                || Some(Evidence::default()),
                |entry| client.observe(position, paused, entry, now),
            );
            let Some(mut evidence) = evidence else {
                continue;
            };
            // Offline ABS listening can arrive as a new, already-paused
            // session. Preserve its forward progress without calling it a
            // measured playback streak or letting it displace a live writer.
            evidence.changed |= first_sample
                && history.get(&history_id).is_some_and(|entry| {
                    entry.previous_watches.is_empty() && position > entry.last_position_ms
                });
            by_item
                .entry(history_id)
                .or_default()
                .push((active, evidence));
        }
        for (history_id, observations) in by_item {
            // Real movement wins over an open/stalled player. Ties retain the
            // current writer; UI selection never participates in this decision.
            let owner = self.owners.get(&history_id);
            let (active, evidence) = observations
                .iter()
                .enumerate()
                .max_by_key(|(index, (s, e))| {
                    (
                        e.rewatch,
                        e.advancing,
                        owner == Some(&s.id()),
                        !s.session.play_state.is_paused,
                        std::cmp::Reverse(*index),
                    )
                })
                .map(|(_, observation)| observation)
                .unwrap();
            let item = active.session.now_playing.as_ref().unwrap();
            let position = active.session.position_ms().unwrap_or(0).max(0);
            let activity_at = session_playback_activity_at(&active.session);
            let entry = history.entry(history_id.clone()).or_insert_with(|| {
                force_save = true;
                HistoryEntry {
                    history_id: history_id.clone(),
                    server_kind: active.kind,
                    item_id: item.item_id.clone(),
                    title: item.display_title(),
                    series_name: item.series_name.clone(),
                    media_source_id: String::new(),
                    file_path: None,
                    duration_ms: None,
                    subtitle_count: 0,
                    audio_languages: Vec::new(),
                    last_position_ms: position,
                    last_seen: activity_at,
                    previous_watches: Vec::new(),
                }
            });
            if evidence.rewatch {
                entry.previous_watches.push(PreviousWatch {
                    last_position_ms: entry.last_position_ms,
                    last_seen: entry.last_seen,
                });
                entry.last_position_ms = position;
                entry.last_seen = activity_at;
                if let Some(client) = self.clients.get_mut(&active.id()) {
                    client.watch_number = entry.previous_watches.len();
                    client.rewatch_since = None;
                }
                force_save = true;
            } else if evidence.changed && (!completed(entry) || position > entry.last_position_ms) {
                // A completed watch stays frozen while an earlier position is
                // being qualified. Merely reopening it cannot erase its log.
                entry.last_position_ms = position;
                entry.last_seen = activity_at;
            }
            entry.title = item.display_title();
            entry.series_name = item.series_name.clone();
            entry.media_source_id = item
                .media_source_id
                .clone()
                .unwrap_or_else(|| format!("mediasource_{}", item.item_id));
            if item.path.is_some() {
                entry.file_path = item.path.clone();
            }
            if let Some(duration) = item.run_time_ticks.map(|t| t / 10_000).filter(|d| *d > 0) {
                entry.duration_ms = Some(duration);
            }
            let languages: Vec<_> = item
                .media_streams
                .iter()
                .filter(|stream| stream.stream_type == StreamType::Audio)
                .filter_map(|stream| stream.language.clone())
                .collect();
            if !languages.is_empty() {
                entry.audio_languages = languages;
            }
            self.owners.insert(history_id, active.id());
        }
        force_save
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MediaServerKind;

    fn completed_episode() -> HistoryEntry {
        HistoryEntry {
            history_id: "jellyfin|episode".into(),
            server_kind: MediaServerKind::Jellyfin,
            item_id: "episode".into(),
            title: "Episode".into(),
            series_name: Some("Show".into()),
            media_source_id: "source".into(),
            file_path: None,
            duration_ms: Some(1_440_000),
            subtitle_count: 0,
            audio_languages: vec!["jpn".into()],
            last_position_ms: 1_400_000,
            last_seen: chrono::Utc::now(),
            previous_watches: Vec::new(),
        }
    }

    fn client(now: Instant) -> ClientProgress {
        ClientProgress {
            position: 0,
            last_advance: now,
            last_observed: now,
            paused: false,
            watch_number: 0,
            rewatch_since: None,
        }
    }

    #[test]
    fn rewatch_requires_five_real_minutes_at_every_supported_speed() {
        for speed in [0.25, 1.0, 2.0, 3.0] {
            let entry = completed_episode();
            let now = Instant::now();
            let mut client = client(now);
            for seconds in 0..=300 {
                // A repeated server position between ten-second check-ins is
                // normal. Only the advancing check-in may confirm the watch.
                let position = ((seconds / 10 * 10) as f64 * 1_000.0 * speed) as i64;
                let result = client
                    .observe(position, false, &entry, now + Duration::from_secs(seconds))
                    .unwrap();
                assert_eq!(result.rewatch, seconds == 300, "speed={speed}, t={seconds}");
            }
        }
    }

    #[test]
    fn open_paused_stalled_seeked_and_tiny_nudges_never_qualify() {
        let entry = completed_episode();
        for mode in [
            "paused",
            "stalled",
            "forward seeks",
            "backward seeks",
            "nudges",
            "missing reports",
        ] {
            let now = Instant::now();
            let mut client = client(now);
            for seconds in (0..=900).step_by(if mode == "missing reports" { 30 } else { 1 }) {
                let position = match mode {
                    "forward seeks" => (seconds * 20_000) % 900_000,
                    "backward seeks" => (seconds % 100) * 1_000,
                    "nudges" => seconds,
                    "missing reports" => seconds * 1_000,
                    _ => 0,
                };
                assert!(
                    !client
                        .observe(
                            position as i64,
                            mode == "paused",
                            &entry,
                            now + Duration::from_secs(seconds)
                        )
                        .unwrap()
                        .rewatch,
                    "{mode} at {seconds}"
                );
            }
        }
    }

    #[test]
    fn pause_or_seek_restarts_the_entire_qualification_window() {
        for interruption in ["pause", "seek", "stall", "gap"] {
            let entry = completed_episode();
            let now = Instant::now();
            let mut client = client(now);
            for seconds in 0..=240 {
                assert!(
                    !client
                        .observe(
                            seconds * 1_000,
                            false,
                            &entry,
                            now + Duration::from_secs(seconds as u64)
                        )
                        .unwrap()
                        .rewatch
                );
            }
            let mut position = if interruption == "seek" { 0 } else { 240_000 };
            if interruption != "gap" {
                for seconds in 241..=270 {
                    assert!(
                        !client
                            .observe(
                                position,
                                interruption == "pause",
                                &entry,
                                now + Duration::from_secs(seconds)
                            )
                            .unwrap()
                            .rewatch
                    );
                }
            }
            // Re-establish the clock after the interruption, then demand a
            // fresh five minutes instead of adding together separate bursts.
            client.observe(position, false, &entry, now + Duration::from_secs(271));
            let mut confirmed_at = None;
            for seconds in 272..=900 {
                position += 1_000;
                if client
                    .observe(position, false, &entry, now + Duration::from_secs(seconds))
                    .unwrap()
                    .rewatch
                {
                    confirmed_at = Some(seconds);
                    break;
                }
            }
            assert!(
                confirmed_at.is_some_and(|seconds| seconds >= 541),
                "{interruption}: {confirmed_at:?}"
            );
        }
    }

    #[test]
    fn unfinished_media_and_small_end_rewinds_are_not_rewatches() {
        let now = Instant::now();
        let mut entry = completed_episode();
        entry.last_position_ms = 900_000;
        let mut client = client(now);
        for seconds in 0..=600 {
            assert!(
                !client
                    .observe(
                        seconds * 1_000,
                        false,
                        &entry,
                        now + Duration::from_secs(seconds as u64)
                    )
                    .unwrap()
                    .rewatch
            );
        }
        let entry = completed_episode();
        assert!(!can_start_rewatch(&entry, 1_350_000));
    }
}
