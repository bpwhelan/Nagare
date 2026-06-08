use anyhow::Result;
use base64::Engine;
use std::path::{Path, PathBuf};
use tokio::process::Command;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::config::{AnimatedScreenshotEncoder, AudioCodec, StaticScreenshotFormat};

/// Set to true locally to force AVIF generation through the configured encoder's fallback path.
const FORCE_AVIF_ENCODER_FALLBACK: bool = false;

/// Set to true locally to force static screenshot generation through its fallback path.
const FORCE_SCREENSHOT_JPEG_FALLBACK: bool = false;

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

pub struct GeneratedImage {
    pub path: PathBuf,
    pub data: Vec<u8>,
    pub format: StaticScreenshotFormat,
}

/// Run an ffmpeg command, capturing stderr, returning a descriptive error on failure.
async fn run_ffmpeg(mut cmd: Command) -> Result<()> {
    cmd.kill_on_drop(true);
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

/// Extract audio from a video source to the configured audio codec.
///
/// When `audio_stream_ordinal` is `Some(idx)`, ffmpeg is told to use that specific
/// audio stream (`-map 0:a:{idx}`), which is more reliable than server-provided
/// absolute stream ids. If only `audio_stream_index` is available, it is used as
/// a fallback absolute map. Otherwise the container's default audio track is used.
pub async fn extract_audio(
    source: &str,
    start_ms: i64,
    end_ms: i64,
    audio_stream_index: Option<u32>,
    audio_stream_ordinal: Option<usize>,
    audio_codec: AudioCodec,
) -> Result<(PathBuf, Vec<u8>)> {
    let id = Uuid::new_v4();
    let output_path = temp_dir().join(format!("{}.{}", id, audio_codec.extension()));

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
    ]);

    if let Some(ordinal) = audio_stream_ordinal {
        debug!("Mapping ffmpeg audio output using ordinal 0:a:{}", ordinal);
        cmd.args(["-map", &format!("0:a:{}", ordinal)]);
    } else if let Some(idx) = audio_stream_index {
        warn!(
            "Falling back to absolute ffmpeg stream mapping 0:{} because no audio ordinal was available",
            idx
        );
        cmd.args(["-map", &format!("0:{}", idx)]);
    }

    cmd.arg("-vn");
    cmd.args(audio_codec.ffmpeg_args());
    cmd.arg(output_path.to_str().unwrap());

    run_ffmpeg(cmd)
        .await
        .map_err(|e| anyhow::anyhow!("ffmpeg audio extraction failed: {}", e))?;

    let data = tokio::fs::read(&output_path).await?;
    info!(
        "Extracted {} audio: {:.1}s ({} bytes)",
        audio_codec.as_str(),
        duration_secs,
        data.len()
    );

    Ok((output_path, data))
}

fn build_avif_command(
    source: &str,
    start_secs: f64,
    duration: f64,
    vf: &str,
    crf: i32,
    encoder: AnimatedScreenshotEncoder,
    output_path: &Path,
) -> Command {
    let mut cmd = Command::new("ffmpeg");
    for arg in http_input_args(source) {
        cmd.arg(arg);
    }
    cmd.arg("-y")
        .arg("-ss")
        .arg(format!("{:.3}", start_secs))
        .arg("-i")
        .arg(source)
        .arg("-t")
        .arg(format!("{:.3}", duration))
        .arg("-vf")
        .arg(vf)
        .arg("-c:v")
        .arg(encoder.as_str())
        .arg("-crf")
        .arg(crf.to_string());

    if encoder == AnimatedScreenshotEncoder::Libsvtav1 {
        cmd.args(["-preset", "8"]);
    }

    cmd.arg("-an")
        .arg("-movflags")
        .arg("+faststart")
        .arg(output_path);
    cmd
}

/// Generate an animated AVIF from a video source.
///
/// `max_width` and `max_fps` are upper bounds: the source is never upscaled
/// past them, and longer clips are scaled down further from these caps to keep
/// the resulting file small (mirroring the adaptive tiers used in GSM).
pub async fn generate_avif(
    source: &str,
    start_ms: i64,
    end_ms: i64,
    encoder: AnimatedScreenshotEncoder,
    max_width: u32,
    max_fps: u32,
) -> Result<(PathBuf, Vec<u8>)> {
    let id = Uuid::new_v4();
    let output_path = temp_dir().join(format!("{}.avif", id));

    let start_secs = start_ms as f64 / 1000.0;
    let duration = (end_ms - start_ms) as f64 / 1000.0;

    // Adaptive multipliers scale the configured caps down for longer clips.
    let (fps_multiplier, width_multiplier, crf) = if duration > 10.0 {
        (0.6, 0.75, 45)
    } else if duration > 5.0 {
        (0.8, 5.0 / 6.0, 42)
    } else {
        (1.0, 1.0, 40)
    };

    let fps = ((max_fps as f64 * fps_multiplier).round() as u32).max(1);
    let width = ((max_width as f64 * width_multiplier).round() as u32).max(2);

    // `min(width,iw)` avoids upscaling past the source; `-2` keeps an even,
    // aspect-correct height that the AV1 encoders require.
    let vf = format!("fps={},scale='min({},iw)':-2", fps, width);

    let primary_result = if FORCE_AVIF_ENCODER_FALLBACK {
        Err(anyhow::anyhow!(
            "forced AVIF encoder fallback via FORCE_AVIF_ENCODER_FALLBACK"
        ))
    } else {
        run_ffmpeg(build_avif_command(
            source,
            start_secs,
            duration,
            &vf,
            crf,
            encoder,
            &output_path,
        ))
        .await
    };

    if let Err(primary_error) = primary_result {
        let fallback_encoder = encoder.fallback();
        warn!(
            "AVIF generation with {} failed; retrying with {}: {}",
            encoder.as_str(),
            fallback_encoder.as_str(),
            primary_error
        );
        let _ = tokio::fs::remove_file(&output_path).await;
        run_ffmpeg(build_avif_command(
            source,
            start_secs,
            duration,
            &vf,
            crf,
            fallback_encoder,
            &output_path,
        ))
        .await
        .map_err(|fallback_error| {
            anyhow::anyhow!(
                "ffmpeg AVIF generation failed with {} and {}\n{}: {}\n{}: {}",
                encoder.as_str(),
                fallback_encoder.as_str(),
                encoder.as_str(),
                primary_error,
                fallback_encoder.as_str(),
                fallback_error
            )
        })?;
    }

    let data = tokio::fs::read(&output_path).await?;
    info!(
        "Generated AVIF: {:.1}s, max {}px @ {}fps ({} bytes)",
        duration,
        width,
        fps,
        data.len()
    );

    Ok((output_path, data))
}

fn build_screenshot_command(
    source: &str,
    time_secs: f64,
    output_path: &Path,
    format: StaticScreenshotFormat,
) -> Command {
    let mut cmd = Command::new("ffmpeg");
    for arg in http_input_args(source) {
        cmd.arg(arg);
    }
    cmd.arg("-y")
        .arg("-ss")
        .arg(format!("{:.3}", time_secs))
        .arg("-i")
        .arg(source)
        .arg("-frames:v")
        .arg("1")
        .arg("-vf")
        .arg("scale=640:-1");

    match format {
        StaticScreenshotFormat::Webp => {
            cmd.args(["-c:v", "libwebp", "-quality", "90"]);
        }
        StaticScreenshotFormat::Jpg => {
            cmd.args(["-c:v", "mjpeg", "-q:v", "2"]);
        }
        StaticScreenshotFormat::Png => {
            cmd.args(["-c:v", "png", "-compression_level", "6"]);
        }
    }

    cmd.arg(output_path);
    cmd
}

fn static_screenshot_attempts(primary: StaticScreenshotFormat) -> Vec<StaticScreenshotFormat> {
    let mut attempts = vec![primary];
    for format in [
        StaticScreenshotFormat::Jpg,
        StaticScreenshotFormat::Webp,
        StaticScreenshotFormat::Png,
    ] {
        if format != primary {
            attempts.push(format);
        }
    }
    attempts
}

/// Generate a single screenshot (still image) from video.
pub async fn generate_screenshot(
    source: &str,
    time_ms: i64,
    primary_format: StaticScreenshotFormat,
) -> Result<GeneratedImage> {
    let id = Uuid::new_v4();

    let time_secs = time_ms as f64 / 1000.0;

    let mut errors = Vec::new();
    for (attempt_index, format) in static_screenshot_attempts(primary_format)
        .into_iter()
        .enumerate()
    {
        let output_path = temp_dir().join(format!("{}.{}", id, format.extension()));
        let result = if attempt_index == 0 && FORCE_SCREENSHOT_JPEG_FALLBACK {
            Err(anyhow::anyhow!(
                "forced static screenshot fallback via FORCE_SCREENSHOT_JPEG_FALLBACK"
            ))
        } else {
            run_ffmpeg(build_screenshot_command(
                source,
                time_secs,
                &output_path,
                format,
            ))
            .await
        };

        match result {
            Ok(()) => {
                let data = tokio::fs::read(&output_path).await?;
                debug!(
                    "Generated {} screenshot: {} bytes",
                    format.as_str(),
                    data.len()
                );

                return Ok(GeneratedImage {
                    path: output_path,
                    data,
                    format,
                });
            }
            Err(error) => {
                warn!(
                    "Screenshot generation with {} failed{}: {}",
                    format.as_str(),
                    if attempt_index == 0 {
                        "; retrying fallback"
                    } else {
                        ""
                    },
                    error
                );
                let _ = tokio::fs::remove_file(&output_path).await;
                errors.push(format!("{}: {}", format.as_str(), error));
            }
        }
    }

    anyhow::bail!(
        "ffmpeg screenshot failed with all configured fallback formats\n{}",
        errors.join("\n")
    )
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

    let mapped_disk_source = file_path.map(|path| config.map_path(path));
    let disk_source = mapped_disk_source
        .as_ref()
        .filter(|path| path.exists())
        .map(|path| path.to_string_lossy().to_string());

    if let Some(server_path) = file_path {
        if let Some(local_path) = mapped_disk_source.as_ref() {
            if local_path.exists() {
                debug!(
                    "Resolved media source on disk: server_path={} local_path={}",
                    server_path,
                    local_path.display()
                );
            } else {
                warn!(
                    "Mapped media path does not exist on disk: server_path={} local_path={}",
                    server_path,
                    local_path.display()
                );
            }
        }
    }

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
                debug!(
                    "Using media server stream URL because media_access_mode=api for item {}",
                    item_id
                );
                Ok(server.get_stream_url(item_id, media_source_id))
            } else {
                anyhow::bail!("Media access mode is API but no media server is configured");
            }
        }
        MediaAccessMode::Auto => {
            // Try disk first for performance and reliability.
            if let Some(source) = disk_source {
                return Ok(source);
            }
            if let Some(server) = server {
                warn!(
                    "Falling back to media server stream URL for item {} because no mapped disk path was available",
                    item_id
                );
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
