use anyhow::Result;
use base64::Engine;
use std::path::{Path, PathBuf};
use tokio::process::Command;
use tracing::{debug, info};
use uuid::Uuid;

/// Temporary directory for ffmpeg output.
fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join("nagare");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Build the common HTTP input args used when source is a URL.
fn http_input_args(source: &str) -> Vec<&'static str> {
    if source.starts_with("http://") || source.starts_with("https://") {
        vec![
            "-protocol_whitelist",
            "file,http,https,tcp,tls",
            "-reconnect",
            "1",
            "-reconnect_streamed",
            "1",
        ]
    } else {
        vec![]
    }
}

/// Run an ffmpeg command, capturing stderr, returning a descriptive error on failure.
async fn run_ffmpeg(mut cmd: Command) -> Result<()> {
    let output = cmd
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "ffmpeg exited with {}\nstderr: {}",
            output.status,
            stderr.trim()
        );
    }
    Ok(())
}

/// Extract audio from a video source to Opus format.
pub async fn extract_audio(source: &str, start_ms: i64, end_ms: i64) -> Result<(PathBuf, Vec<u8>)> {
    let id = Uuid::new_v4();
    let output_path = temp_dir().join(format!("{}.opus", id));

    let start_secs = start_ms as f64 / 1000.0;
    let duration_secs = (end_ms - start_ms) as f64 / 1000.0;

    let mut cmd = Command::new("ffmpeg");
    for arg in http_input_args(source) {
        cmd.arg(arg);
    }
    cmd.args([
        "-y",
        "-ss",
        &format!("{:.3}", start_secs),
        "-i",
        source,
        "-t",
        &format!("{:.3}", duration_secs),
        "-vn",
        "-acodec",
        "libopus",
        "-b:a",
        "64k",
        "-ac",
        "1",
        output_path.to_str().unwrap(),
    ]);

    run_ffmpeg(cmd)
        .await
        .map_err(|e| anyhow::anyhow!("ffmpeg audio extraction failed: {}", e))?;

    let data = tokio::fs::read(&output_path).await?;
    info!(
        "Extracted audio: {:.1}s ({} bytes)",
        duration_secs,
        data.len()
    );

    Ok((output_path, data))
}

/// Generate an animated AVIF from a video source.
pub async fn generate_avif(source: &str, start_ms: i64, end_ms: i64) -> Result<(PathBuf, Vec<u8>)> {
    let id = Uuid::new_v4();
    let output_path = temp_dir().join(format!("{}.avif", id));

    let start_secs = start_ms as f64 / 1000.0;
    let duration = (end_ms - start_ms) as f64 / 1000.0;

    // Adaptive settings based on duration
    let (fps, scale, crf) = if duration > 10.0 {
        (6, 360, 45)
    } else if duration > 5.0 {
        (8, 400, 42)
    } else {
        (10, 480, 40)
    };

    let vf = format!("fps={},scale={}:-1", fps, scale);

    let mut cmd = Command::new("ffmpeg");
    for arg in http_input_args(source) {
        cmd.arg(arg);
    }
    cmd.args([
        "-y",
        "-ss",
        &format!("{:.3}", start_secs),
        "-i",
        source,
        "-t",
        &format!("{:.3}", duration),
        "-vf",
        &vf,
        "-c:v",
        "libaom-av1",
        "-crf",
        &crf.to_string(),
        "-an",
        "-movflags",
        "+faststart",
        output_path.to_str().unwrap(),
    ]);

    run_ffmpeg(cmd)
        .await
        .map_err(|e| anyhow::anyhow!("ffmpeg AVIF generation failed: {}", e))?;

    let data = tokio::fs::read(&output_path).await?;
    info!(
        "Generated AVIF: {:.1}s, {}x? @ {}fps ({} bytes)",
        duration,
        scale,
        fps,
        data.len()
    );

    Ok((output_path, data))
}

/// Generate a single screenshot (still image) from video.
pub async fn generate_screenshot(source: &str, time_ms: i64) -> Result<(PathBuf, Vec<u8>)> {
    let id = Uuid::new_v4();
    let output_path = temp_dir().join(format!("{}.webp", id));

    let time_secs = time_ms as f64 / 1000.0;

    let mut cmd = Command::new("ffmpeg");
    for arg in http_input_args(source) {
        cmd.arg(arg);
    }
    cmd.args([
        "-y",
        "-ss",
        &format!("{:.3}", time_secs),
        "-i",
        source,
        "-frames:v",
        "1",
        "-vf",
        "scale=640:-1",
        "-c:v",
        "libwebp",
        "-quality",
        "90",
        output_path.to_str().unwrap(),
    ]);

    run_ffmpeg(cmd)
        .await
        .map_err(|e| anyhow::anyhow!("ffmpeg screenshot failed: {}", e))?;

    let data = tokio::fs::read(&output_path).await?;
    debug!("Generated screenshot: {} bytes", data.len());

    Ok((output_path, data))
}

/// Determine the media source path/URL based on config.
pub fn resolve_media_source(
    config: &crate::config::Config,
    server: Option<&dyn crate::media_server::MediaServer>,
    item_id: &str,
    media_source_id: &str,
    file_path: Option<&str>,
) -> Result<String> {
    use crate::config::MediaAccessMode;

    let disk_source = file_path.and_then(|path| {
        let local = config.map_path(path);
        local.exists().then(|| local.to_string_lossy().to_string())
    });

    match config.media_access_mode {
        MediaAccessMode::Disk => {
            if let Some(source) = disk_source {
                return Ok(source);
            }
            if let Some(server) = server {
                return Ok(server.get_stream_url(item_id, media_source_id));
            }
            anyhow::bail!(
                "Media source is not available on disk and no media server is configured"
            );
        }
        MediaAccessMode::Api => {
            if let Some(server) = server {
                Ok(server.get_stream_url(item_id, media_source_id))
            } else {
                anyhow::bail!("Media access mode is API but no media server is configured");
            }
        }
        MediaAccessMode::Auto => {
            // Try disk first for performance
            if let Some(source) = disk_source {
                return Ok(source);
            }
            if let Some(server) = server {
                return Ok(server.get_stream_url(item_id, media_source_id));
            }
            anyhow::bail!(
                "Media source is not available on disk and no media server is configured"
            );
        }
    }
}

/// Encode bytes to base64 for AnkiConnect.
pub fn to_base64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

/// Clean up temporary files.
pub async fn cleanup_temp_file(path: &Path) {
    let _ = tokio::fs::remove_file(path).await;
}
