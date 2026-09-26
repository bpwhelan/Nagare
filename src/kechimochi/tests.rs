use super::*;
use axum::{Router, extract::Path, routing::get};

#[derive(Default)]
struct MockData {
    media: Vec<RemoteMedia>,
    logs: Vec<RemoteLog>,
    milestones: HashMap<String, Vec<Value>>,
    next_id: i64,
    mutations: usize,
    lose_media_response: bool,
    lose_log_response: bool,
    fail_reads: bool,
    reject_log_title: Option<String>,
    add_personal_log_after_delete: bool,
}

type MockState = Arc<Mutex<MockData>>;

async fn list_media(State(state): State<MockState>) -> (StatusCode, Json<Value>) {
    let data = state.lock().await;
    if data.fail_reads {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!("offline")));
    }
    (StatusCode::OK, Json(json!(data.media)))
}

async fn list_logs(State(state): State<MockState>) -> Json<Value> {
    // GET /logs returns activity summaries with extra display-only fields.
    let logs: Vec<_> = state
        .lock()
        .await
        .logs
        .iter()
        .map(|log| {
            let mut value = serde_json::to_value(log).unwrap();
            value["title"] = json!("Display title");
            value["language"] = json!("Japanese");
            value
        })
        .collect();
    Json(json!(logs))
}

async fn list_media_logs(State(state): State<MockState>, Path(id): Path<i64>) -> Json<Value> {
    Json(json!(
        state
            .lock()
            .await
            .logs
            .iter()
            .filter(|log| log.media_id == id)
            .collect::<Vec<_>>()
    ))
}

async fn add_media(
    State(state): State<MockState>,
    Json(mut media): Json<RemoteMedia>,
) -> (StatusCode, Json<Value>) {
    let mut data = state.lock().await;
    if data
        .media
        .iter()
        .any(|m| m.title == media.title && m.variant == media.variant)
    {
        return (
            StatusCode::CONFLICT,
            Json(json!("duplicate title and variant")),
        );
    }
    data.next_id += 1;
    let id = data.next_id;
    media.id = Some(id);
    media.uid = Some(format!("uid-{id}"));
    data.media.push(media);
    data.mutations += 1;
    if std::mem::take(&mut data.lose_media_response) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!("committed, response lost")),
        );
    }
    (StatusCode::OK, Json(json!(id)))
}

async fn add_log(
    State(state): State<MockState>,
    Json(mut log): Json<RemoteLog>,
) -> (StatusCode, Json<Value>) {
    let mut data = state.lock().await;
    if log.duration_minutes <= 0 && log.characters <= 0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!("Activity must have either duration or characters")),
        );
    }
    if data
        .reject_log_title
        .as_ref()
        .is_some_and(|title| log.notes.contains(title))
    {
        return (StatusCode::BAD_REQUEST, Json(json!("invalid log")));
    }
    assert!(data.media.iter().any(|m| m.id == Some(log.media_id)));
    data.next_id += 1;
    let id = data.next_id;
    log.id = Some(id);
    data.logs.push(log);
    data.mutations += 1;
    if std::mem::take(&mut data.lose_log_response) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!("committed, response lost")),
        );
    }
    (StatusCode::OK, Json(json!(id)))
}

async fn put_media(
    State(state): State<MockState>,
    Path(id): Path<i64>,
    Json(media): Json<RemoteMedia>,
) {
    let mut data = state.lock().await;
    assert_eq!(media.id, Some(id));
    *data.media.iter_mut().find(|m| m.id == Some(id)).unwrap() = media;
    data.mutations += 1;
}

async fn put_log(State(state): State<MockState>, Path(id): Path<i64>, Json(log): Json<RemoteLog>) {
    let mut data = state.lock().await;
    assert_eq!(log.id, Some(id));
    *data.logs.iter_mut().find(|log| log.id == Some(id)).unwrap() = log;
    data.mutations += 1;
}

async fn delete_media(
    State(state): State<MockState>,
    Path(id): Path<i64>,
    headers: axum::http::HeaderMap,
) {
    assert_eq!(headers.get("X-Kechimochi-API").unwrap(), "1");
    let mut data = state.lock().await;
    // Simulate the real cascading deletion, so an unsafe cleanup fails tests.
    data.media.retain(|m| m.id != Some(id));
    data.logs.retain(|log| log.media_id != id);
    data.mutations += 1;
}

async fn delete_log(
    State(state): State<MockState>,
    Path(id): Path<i64>,
    headers: axum::http::HeaderMap,
) {
    assert_eq!(headers.get("X-Kechimochi-API").unwrap(), "1");
    let mut data = state.lock().await;
    data.logs.retain(|log| log.id != Some(id));
    if std::mem::take(&mut data.add_personal_log_after_delete) {
        data.next_id += 1;
        let personal = RemoteLog {
            id: Some(data.next_id),
            media_id: data.media[0].id.unwrap(),
            duration_minutes: 10,
            characters: 0,
            date: "2026-09-15".into(),
            activity_type: "Watching".into(),
            notes: "Added during the sync".into(),
        };
        data.logs.push(personal);
    }
    data.mutations += 1;
}

async fn list_milestones(State(state): State<MockState>, Path(uid): Path<String>) -> Json<Value> {
    Json(json!(
        state
            .lock()
            .await
            .milestones
            .get(&uid)
            .cloned()
            .unwrap_or_default()
    ))
}

struct Mock {
    state: MockState,
    url: String,
    task: tokio::task::JoinHandle<()>,
}

impl Mock {
    async fn new() -> Self {
        let state = MockState::default();
        let app = Router::new()
            .route("/api/version", get(|| async { Json("http-0.3.2") }))
            .route("/api/media", get(list_media).post(add_media))
            .route(
                "/api/media/{id}",
                axum::routing::put(put_media).delete(delete_media),
            )
            .route("/api/logs", get(list_logs).post(add_log))
            .route("/api/logs/media/{id}", get(list_media_logs))
            .route(
                "/api/logs/{id}",
                axum::routing::put(put_log).delete(delete_log),
            )
            .route("/api/media/{uid}/milestones", get(list_milestones))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self { state, url, task }
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Fixture {
    service: Arc<SyncService>,
    directory: std::path::PathBuf,
}

impl Fixture {
    async fn new(url: &str) -> Self {
        let directory =
            std::env::temp_dir().join(format!("nagare-kechimochi-{}", uuid::Uuid::new_v4()));
        let db = Arc::new(
            AppDatabase::new(directory.join("test.sqlite"), None)
                .await
                .unwrap(),
        );
        let mut config = Config::default();
        config.kechimochi.api_url = url.into();
        config.kechimochi.enabled = true;
        Self {
            service: Arc::new(SyncService::new(Arc::new(RwLock::new(config)), db)),
            directory,
        }
    }

    async fn save(&self, items: Vec<HistoryEntry>) {
        self.service
            .db
            .save_session_history(
                items
                    .into_iter()
                    .map(|h| (h.history_id.clone(), h))
                    .collect(),
                None,
            )
            .await
            .unwrap();
    }

    async fn sync(&self) -> SyncState {
        let _guard = self.service.gate.lock().await;
        self.service.execute(false).await.unwrap()
    }

    async fn remove_source(&self, id: &str) {
        let id = id.to_string();
        self.service
            .db
            .kechimochi_query(move |conn| {
                conn.execute("DELETE FROM media_history WHERE history_id = ?1", [id])?;
                Ok(())
            })
            .await
            .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn entry(id: &str, position: i64) -> HistoryEntry {
    HistoryEntry {
        history_id: format!("plex|{id}"),
        server_kind: MediaServerKind::Plex,
        item_id: id.into(),
        title: format!("Show episode {id}"),
        series_name: Some("Show".into()),
        media_source_id: id.into(),
        file_path: Some("/anime/Show.mkv".into()),
        duration_ms: Some(1_400_000),
        subtitle_count: 123,
        audio_languages: vec!["jpn".into()],
        last_position_ms: position,
        last_seen: "2026-04-17T01:00:00Z".parse().unwrap(),
        previous_watches: Vec::new(),
    }
}

#[tokio::test]
async fn rewatches_create_separate_logs_without_replacing_the_original() {
    let mock = Mock::new().await;
    let fixture = Fixture::new(&mock.url).await;
    let mut episode = entry("rewatch", 1_400_000);
    fixture.save(vec![episode.clone()]).await;
    assert_eq!(fixture.sync().await.report.logs_created, 1);
    let original = mock.state.lock().await.logs[0].clone();
    episode
        .previous_watches
        .push(crate::session::PreviousWatch {
            last_position_ms: episode.last_position_ms,
            last_seen: episode.last_seen,
        });
    episode.last_position_ms = 300_000;
    episode.last_seen = "2026-09-19T12:00:00Z".parse().unwrap();
    fixture.save(vec![episode.clone()]).await;
    assert_eq!(fixture.sync().await.report.logs_created, 1);
    {
        let data = mock.state.lock().await;
        assert_eq!(data.logs.len(), 2);
        assert_eq!(
            data.logs.iter().find(|log| log.id == original.id),
            Some(&original)
        );
        let rewatch = data.logs.iter().find(|log| log.id != original.id).unwrap();
        assert_eq!(rewatch.duration_minutes, 5);
        assert_eq!(rewatch.date, "2026-09-19");
    }
    episode.last_position_ms = 1_400_000;
    fixture.save(vec![episode]).await;
    assert_eq!(fixture.sync().await.report.logs_updated, 1);
    let mutations = mock.state.lock().await.mutations;
    let restarted = SyncService::new(fixture.service.config.clone(), fixture.service.db.clone());
    restarted.execute(false).await.unwrap();
    assert_eq!(mock.state.lock().await.mutations, mutations);
    assert_eq!(mock.state.lock().await.logs.len(), 2);
    fixture.remove_source("plex|rewatch").await;
    assert_eq!(fixture.sync().await.report.logs_deleted, 2);
}

#[tokio::test]
async fn backfills_every_item_and_a_second_pass_does_not_write() {
    let mock = Mock::new().await;
    let fixture = Fixture::new(&mock.url).await;
    let mut book = entry("book", 9_000_000);
    book.history_id = "audiobookshelf|book".into();
    book.server_kind = MediaServerKind::Audiobookshelf;
    book.duration_ms = Some(20_000_000);
    book.audio_languages = vec!["eng".into()];
    fixture
        .save(vec![entry("partial", 90_000), entry("zero", 0), book])
        .await;
    let first = fixture.sync().await;
    assert!(first.last_error.is_none(), "{:?}", first.last_error);
    assert_eq!(
        (
            first.report.history_items,
            first.report.media_created,
            first.report.logs_created
        ),
        (3, 2, 2)
    );
    let data = mock.state.lock().await;
    assert!(
        data.logs
            .iter()
            .any(|log| log.activity_type == "Listening" && log.duration_minutes == 150)
    );
    assert!(data.media.iter().any(|media| media.language == "English"));
    assert!(data.logs.iter().all(|log| log.date == "2026-04-16"));
    assert_eq!(first.report.media_only_items, 1);
    assert!(data.logs.iter().all(|log| log.duration_minutes > 0));
    let mutations = data.mutations;
    drop(data);
    let second = fixture.sync().await;
    assert_eq!(second.report.unchanged_logs, 2);
    assert_eq!(mock.state.lock().await.mutations, mutations);
}

#[tokio::test]
async fn updates_progress_dates_and_titles_without_losing_user_metadata() {
    let mock = Mock::new().await;
    let fixture = Fixture::new(&mock.url).await;
    fixture.save(vec![entry("one", 300_000)]).await;
    fixture.sync().await;
    let old_id;
    {
        let mut data = mock.state.lock().await;
        old_id = data.logs[0].id;
        data.logs[0].notes.push_str("\nMy personal notes");
        data.media[0].cover_image = "my-cover.jpg".into();
        data.media[0].description = "My description".into();
        data.media[0].status = "Paused".into();
        let mut extra: Value = serde_json::from_str(&data.media[0].extra_data).unwrap();
        extra["custom"] = json!({"keep": true});
        data.media[0].extra_data = extra.to_string();
    }
    let mut changed = entry("one", 780_000);
    changed.title = "Corrected episode title".into();
    changed.last_seen = "2026-05-01T20:00:00Z".parse().unwrap();
    fixture.save(vec![changed]).await;
    let result = fixture.sync().await;
    assert!(result.last_error.is_none());
    assert_eq!(result.report.logs_updated, 1);
    let data = mock.state.lock().await;
    assert_eq!(data.logs[0].id, old_id);
    assert_eq!(data.logs[0].duration_minutes, 13);
    assert_eq!(data.logs[0].date, "2026-05-01");
    assert!(data.logs[0].notes.contains("Corrected episode title"));
    assert!(data.logs[0].notes.ends_with("My personal notes"));
    assert_eq!(data.media[0].cover_image, "my-cover.jpg");
    assert_eq!(data.media[0].description, "My description");
    assert_eq!(data.media[0].status, "Paused");
    assert_eq!(
        serde_json::from_str::<Value>(&data.media[0].extra_data).unwrap()["custom"]["keep"],
        true
    );
}

#[tokio::test]
async fn committed_posts_are_rediscovered_after_lost_responses_and_restart() {
    for lose_media in [true, false] {
        let mock = Mock::new().await;
        let fixture = Fixture::new(&mock.url).await;
        fixture.save(vec![entry("one", 300_000)]).await;
        {
            let mut data = mock.state.lock().await;
            data.lose_media_response = lose_media;
            data.lose_log_response = !lose_media;
        }
        let first = fixture.sync().await;
        assert_eq!(first.consecutive_failures, 1);
        assert!(first.last_success_at.is_none());
        let restarted =
            SyncService::new(fixture.service.config.clone(), fixture.service.db.clone());
        let second = restarted.execute(false).await.unwrap();
        assert!(second.last_error.is_none(), "{:?}", second.last_error);
        assert_eq!(second.consecutive_failures, 0);
        let data = mock.state.lock().await;
        assert_eq!(data.media.len(), 1);
        assert_eq!(data.logs.len(), 1);
    }
}

#[tokio::test]
async fn changing_destination_does_not_reuse_foreign_ids_or_touch_the_old_destination() {
    let first = Mock::new().await;
    let second = Mock::new().await;
    let fixture = Fixture::new(&first.url).await;
    fixture.save(vec![entry("one", 300_000)]).await;
    fixture.sync().await;
    let old_mutations = first.state.lock().await.mutations;
    let foreign = RemoteMedia {
        id: Some(1),
        uid: Some("foreign".into()),
        title: "My existing book".into(),
        ..Default::default()
    };
    second.state.lock().await.media.push(foreign.clone());
    second.state.lock().await.next_id = 1;
    fixture.service.config.write().await.kechimochi.api_url = second.url.clone();
    let result = fixture.sync().await;
    assert!(result.last_error.is_none());
    assert_eq!(result.report.media_created, 1);
    assert_eq!(first.state.lock().await.mutations, old_mutations);
    assert_eq!(second.state.lock().await.media[0], foreign);
    assert_ne!(second.state.lock().await.logs[0].media_id, 1);
}

#[tokio::test]
async fn deleted_remote_objects_are_recreated_and_duplicates_are_reconciled() {
    let mock = Mock::new().await;
    let fixture = Fixture::new(&mock.url).await;
    fixture.save(vec![entry("one", 300_000)]).await;
    fixture.sync().await;
    {
        let mut data = mock.state.lock().await;
        data.media.clear();
        data.logs.clear();
    }
    let restored = fixture.sync().await;
    assert_eq!(
        (restored.report.media_created, restored.report.logs_created),
        (1, 1)
    );
    {
        let mut data = mock.state.lock().await;
        let mut duplicate = data.logs[0].clone();
        data.next_id += 1;
        duplicate.id = Some(data.next_id);
        data.logs.push(duplicate);
    }
    let deduplicated = fixture.sync().await;
    assert_eq!(deduplicated.report.logs_deleted, 1);
    assert_eq!(mock.state.lock().await.logs.len(), 1);
}

#[tokio::test]
async fn renaming_a_series_preserves_media_identity_and_updates_logs() {
    let mock = Mock::new().await;
    let fixture = Fixture::new(&mock.url).await;
    fixture.save(vec![entry("one", 300_000)]).await;
    fixture.sync().await;
    let original_log = mock.state.lock().await.logs[0].id;
    let original_media = mock.state.lock().await.media[0].id;
    mock.state.lock().await.media[0].cover_image = "keep-cover.jpg".into();
    let mut changed = entry("one", 60_000);
    changed.series_name = Some("Renamed show".into());
    fixture.save(vec![changed]).await;
    let result = fixture.sync().await;
    assert!(result.last_error.is_none(), "{:?}", result.last_error);
    assert_eq!(result.report.media_deleted, 0);
    let data = mock.state.lock().await;
    assert_eq!(data.media.len(), 1);
    assert_eq!(data.media[0].title, "Renamed show");
    assert_eq!(data.media[0].id, original_media);
    assert_eq!(data.media[0].cover_image, "keep-cover.jpg");
    assert_eq!(data.logs[0].id, original_log);
    assert_eq!(data.logs[0].duration_minutes, 1);
}

#[tokio::test]
async fn source_deletions_preserve_foreign_logs_milestones_and_other_instances() {
    for has_milestone in [false, true] {
        let mock = Mock::new().await;
        let fixture = Fixture::new(&mock.url).await;
        fixture.save(vec![entry("one", 300_000)]).await;
        fixture.sync().await;
        {
            let mut data = mock.state.lock().await;
            if has_milestone {
                let uid = data.media[0].uid.clone().unwrap();
                data.milestones
                    .insert(uid, vec![json!({"name": "My milestone"})]);
            } else {
                let mut foreign = data.logs[0].clone();
                data.next_id += 1;
                foreign.id = Some(data.next_id);
                foreign.notes = "[nagare:another-instance:b25l]\nDo not touch".into();
                data.logs.push(foreign);
            }
        }
        fixture.remove_source("plex|one").await;
        let result = fixture.sync().await;
        assert!(result.last_error.is_none());
        assert_eq!(result.report.logs_deleted, 1);
        assert_eq!(result.report.retained_media, 1);
        assert_eq!(mock.state.lock().await.media.len(), 1);
        assert_eq!(
            mock.state.lock().await.logs.len(),
            usize::from(!has_milestone)
        );
    }
}

#[tokio::test]
async fn failure_skips_cleanup_and_persists_retry_state() {
    let mock = Mock::new().await;
    let fixture = Fixture::new(&mock.url).await;
    fixture.save(vec![entry("one", 300_000)]).await;
    fixture.sync().await;
    fixture.remove_source("plex|one").await;
    mock.state.lock().await.fail_reads = true;
    let result = fixture.sync().await;
    assert!(result.last_error.as_deref().unwrap().contains("503"));
    assert_eq!(result.consecutive_failures, 1);
    assert_eq!(mock.state.lock().await.logs.len(), 1);
    let saved = fixture
        .service
        .db
        .kechimochi_state(mock.url.clone())
        .await
        .unwrap();
    assert_eq!(saved.last_error, result.last_error);
    mock.state.lock().await.fail_reads = false;
    let recovered = fixture.sync().await;
    assert!(recovered.last_error.is_none());
    assert_eq!(recovered.report.logs_deleted, 1);
    assert_eq!(recovered.report.media_deleted, 1);
}

#[tokio::test]
async fn concurrent_manual_requests_share_one_run_and_disabled_scheduler_does_nothing() {
    let mock = Mock::new().await;
    let fixture = Fixture::new(&mock.url).await;
    fixture.save(vec![entry("one", 300_000)]).await;
    fixture.service.config.write().await.kechimochi.enabled = false;
    fixture.service.scheduled_check().await.unwrap();
    assert_eq!(mock.state.lock().await.mutations, 0);
    assert!(fixture.service.start());
    assert!(!fixture.service.start());
    let guard = fixture.service.gate.lock().await;
    assert_eq!(mock.state.lock().await.logs.len(), 1);
    drop(guard);
    fixture.service.config.write().await.kechimochi.enabled = true;
    let mutations = mock.state.lock().await.mutations;
    fixture.service.scheduled_check().await.unwrap();
    assert_eq!(mock.state.lock().await.mutations, mutations);
}

#[test]
fn schedules_survive_restart_back_off_and_handle_daylight_saving() {
    let mut config = KechimochiConfig {
        enabled: true,
        ..Default::default()
    };
    let now = "2026-03-08T05:00:00Z".parse().unwrap();
    assert_eq!(
        next_run(&config, &SyncState::default(), now).unwrap(),
        Some(now)
    );
    let mut state = SyncState {
        last_success_at: Some(now),
        last_finished_at: Some(now),
        ..Default::default()
    };
    assert_eq!(
        next_run(&config, &state, now).unwrap(),
        Some(now + TimeDelta::minutes(5))
    );
    state.consecutive_failures = 1;
    assert_eq!(
        next_run(&config, &state, now).unwrap(),
        Some(now + TimeDelta::seconds(30))
    );
    state.consecutive_failures = 100;
    assert_eq!(
        next_run(&config, &state, now).unwrap(),
        Some(now + TimeDelta::hours(1))
    );
    state.consecutive_failures = 0;
    config.sync_mode = SyncMode::Daily;
    config.daily_hour = 2;
    config.daily_minute = 30;
    // Spring's nonexistent 02:30 runs at the first valid time, 03:00 EDT.
    assert_eq!(
        next_run(&config, &state, now).unwrap(),
        Some("2026-03-08T07:00:00Z".parse().unwrap())
    );
    config.daily_hour = 1;
    state.last_success_at = Some("2026-11-01T04:00:00Z".parse().unwrap());
    let fall = next_run(&config, &state, now).unwrap().unwrap();
    assert_eq!(
        fall,
        "2026-11-01T05:30:00Z".parse::<DateTime<Utc>>().unwrap()
    );
    state.last_success_at = Some(fall);
    assert_eq!(
        next_run(&config, &state, fall).unwrap(),
        Some("2026-11-02T06:30:00Z".parse().unwrap())
    );
    config.enabled = false;
    assert_eq!(next_run(&config, &state, now).unwrap(), None);
}

#[test]
fn validates_settings_and_preserves_legacy_config_defaults() {
    let old: Config = serde_json::from_value(json!({})).unwrap();
    assert!(!old.kechimochi.enabled);
    let mut config = KechimochiConfig {
        api_url: "HTTP://LOCALHOST:3031/api/".into(),
        ..Default::default()
    };
    config.normalize_and_validate().unwrap();
    assert_eq!(config.api_url, "http://localhost:3031");
    for invalid in [
        "file:///tmp/data",
        "http://host?query",
        "http://user:password@host",
        "not a url",
    ] {
        config.api_url = invalid.into();
        assert!(config.normalize_and_validate().is_err());
    }
    config = KechimochiConfig::default();
    config.timezone = "not-a-zone".into();
    assert!(config.normalize_and_validate().is_err());
    config.timezone = "UTC".into();
    config.interval_minutes = 0;
    assert!(config.normalize_and_validate().is_err());
}

#[test]
fn progress_is_bounded_and_markers_support_arbitrary_history_ids() {
    let mut history = entry("日本語|:\n[]", i64::MAX);
    assert_eq!(progress_ms(&history), 1_400_000);
    history.duration_ms = None;
    assert_eq!(progress_ms(&history), i64::MAX);
    history.last_position_ms = -1;
    assert_eq!(progress_ms(&history), 0);
    let id = history.history_id.clone();
    let group = groups(
        vec![SourceItem {
            history,
            mined_notes: 0,
        }],
        "jpn",
    )
    .into_values()
    .next()
    .unwrap();
    let log = desired_log(&group.items[0], &group, 1, "instance", chrono_tz::UTC, None);
    assert_eq!(log_key(&log, "instance"), Some(id));
    assert_eq!(log_key(&log, "other-instance"), None);
}

#[tokio::test]
async fn an_individual_log_error_does_not_starve_other_items_in_its_group() {
    let mock = Mock::new().await;
    let fixture = Fixture::new(&mock.url).await;
    fixture
        .save(vec![entry("one", 60_000), entry("two", 120_000)])
        .await;
    mock.state.lock().await.reject_log_title = Some("Show episode one".into());
    let failed = fixture.sync().await;
    assert_eq!(failed.report.logs_created, 1);
    assert_eq!(failed.report.errors.len(), 1);
    assert!(
        mock.state.lock().await.logs[0]
            .notes
            .contains("Show episode two")
    );
    mock.state.lock().await.reject_log_title = None;
    let recovered = fixture.sync().await;
    assert!(recovered.last_error.is_none());
    assert_eq!(recovered.report.logs_created, 1);
    assert_eq!(recovered.report.unchanged_logs, 1);
}

#[tokio::test]
async fn cleanup_refreshes_activity_before_cascading_media_deletion() {
    let mock = Mock::new().await;
    let fixture = Fixture::new(&mock.url).await;
    fixture.save(vec![entry("one", 60_000)]).await;
    fixture.sync().await;
    mock.state.lock().await.add_personal_log_after_delete = true;
    fixture.remove_source("plex|one").await;
    let cleaned = fixture.sync().await;
    assert!(cleaned.last_error.is_none());
    assert_eq!(cleaned.report.logs_deleted, 1);
    assert_eq!(cleaned.report.retained_media, 1);
    let data = mock.state.lock().await;
    assert_eq!(data.media.len(), 1);
    assert_eq!(data.logs[0].notes, "Added during the sync");
}

#[tokio::test]
async fn scheduler_backfills_and_a_restarted_scheduler_catches_up() {
    let mock = Mock::new().await;
    let fixture = Fixture::new(&mock.url).await;
    fixture.save(vec![entry("one", 60_000)]).await;
    fixture.service.scheduled_check().await.unwrap();
    assert_eq!(mock.state.lock().await.logs.len(), 1);
    fixture.save(vec![entry("two", 120_000)]).await;
    let mut saved = fixture
        .service
        .db
        .kechimochi_state(mock.url.clone())
        .await
        .unwrap();
    saved.last_success_at = Some(Utc::now() - TimeDelta::days(2));
    fixture
        .service
        .db
        .save_kechimochi_state(mock.url.clone(), saved)
        .await
        .unwrap();
    let restarted = SyncService::new(fixture.service.config.clone(), fixture.service.db.clone());
    restarted.scheduled_check().await.unwrap();
    assert_eq!(mock.state.lock().await.logs.len(), 2);
}

#[tokio::test]
async fn zero_progress_is_media_only_and_subminute_progress_uses_the_api_minimum() {
    let mock = Mock::new().await;
    let fixture = Fixture::new(&mock.url).await;
    fixture.save(vec![entry("one", 0)]).await;
    let zero = fixture.sync().await;
    assert!(zero.last_error.is_none());
    assert_eq!(zero.report.media_only_items, 1);
    assert_eq!(mock.state.lock().await.media.len(), 1);
    assert!(mock.state.lock().await.logs.is_empty());
    fixture.save(vec![entry("one", 1000)]).await;
    let started = fixture.sync().await;
    assert!(started.last_error.is_none());
    assert_eq!(started.report.logs_created, 1);
    assert_eq!(mock.state.lock().await.logs[0].duration_minutes, 1);
    fixture.save(vec![entry("one", 0)]).await;
    let reset = fixture.sync().await;
    assert!(reset.last_error.is_none());
    assert_eq!(reset.report.logs_deleted, 1);
    assert_eq!(reset.report.media_only_items, 1);
    assert_eq!(mock.state.lock().await.media.len(), 1);
    assert!(mock.state.lock().await.logs.is_empty());
}
