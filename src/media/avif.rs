use std::future::Future;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use tokio::process::Command;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::{cleanup_temp_file, http_input_args, run_ffmpeg, temp_dir};
use crate::config::{AnimatedScreenshotEncoder, AvifSizePriority, AvifSizingMode, MiningConfig};

/// Set locally to exercise the configured encoder's fallback path.
const FORCE_AVIF_ENCODER_FALLBACK: bool = false;
const BASE_CRF: u32 = 40;
const MAX_CRF: u32 = 56;
const MIN_WIDTH: u32 = 240;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AvifParameters {
    fps: u32,
    width: u32,
    crf: u32,
}

impl AvifParameters {
    fn from_caps(max_width: u32, max_fps: u32) -> Self {
        Self {
            fps: max_fps.max(1),
            width: (max_width & !1).max(2),
            crf: BASE_CRF,
        }
    }

    fn for_duration(duration: f64, max_width: u32, max_fps: u32) -> Self {
        let (fps_multiplier, width_multiplier, crf) = if duration > 10.0 {
            (0.6, 0.75, 45)
        } else if duration > 5.0 {
            (0.8, 5.0 / 6.0, 42)
        } else {
            (1.0, 1.0, BASE_CRF)
        };
        Self {
            crf,
            ..Self::from_caps(
                (max_width as f64 * width_multiplier).round() as u32,
                (max_fps as f64 * fps_multiplier).round() as u32,
            )
        }
    }
}

/// Ordered from most desirable to smallest. Exhaust the less preferred setting
/// first; the preferred setting can still decrease when the target is very small.
fn size_candidates(caps: AvifParameters, priority: AvifSizePriority) -> Vec<AvifParameters> {
    let mut candidates = vec![caps];
    let mut current = caps;
    let stages = match priority {
        AvifSizePriority::PreferFps => [false, true],
        AvifSizePriority::PreferQuality => [true, false],
    };
    for reduce_fps in stages {
        if reduce_fps {
            while current.fps > 1 {
                current.fps = (current.fps as f64 * 0.75).floor().max(1.0) as u32;
                candidates.push(current);
            }
        } else {
            while current.crf < MAX_CRF {
                current.crf = (current.crf + 2).min(MAX_CRF);
                candidates.push(current);
            }
            let min_width = caps.width.min(MIN_WIDTH);
            while current.width > min_width {
                current.width = (((current.width as f64 * 0.75) as u32) & !1).max(min_width);
                candidates.push(current);
            }
        }
    }
    candidates
}

/// Search measured estimates rather than assuming a fixed bitrate or a fixed
/// relationship between CRF and file size. A binary search bounds sampling work.
async fn choose_size_parameters<F, Fut>(
    candidates: &[AvifParameters],
    target_bytes: u64,
    mut estimate: F,
) -> Result<(AvifParameters, u64)>
where
    F: FnMut(AvifParameters) -> Fut,
    Fut: Future<Output = Result<u64>>,
{
    let first_size = estimate(candidates[0]).await?;
    if first_size <= target_bytes || candidates.len() == 1 {
        return Ok((candidates[0], first_size));
    }

    let mut lower = 0;
    let mut upper = candidates.len() - 1;
    let mut upper_size = estimate(candidates[upper]).await?;
    if upper_size > target_bytes {
        return Ok((candidates[upper], upper_size));
    }
    while upper - lower > 1 {
        let middle = lower + (upper - lower) / 2;
        let size = estimate(candidates[middle]).await?;
        if size <= target_bytes {
            upper = middle;
            upper_size = size;
        } else {
            lower = middle;
        }
    }
    Ok((candidates[upper], upper_size))
}

fn build_avif_command(
    source: &str,
    start_secs: f64,
    duration: f64,
    parameters: AvifParameters,
    encoder: AnimatedScreenshotEncoder,
    sizing_mode: AvifSizingMode,
    output_path: &Path,
) -> Command {
    // Keep even, aspect-correct dimensions without upscaling the source. In
    // size mode, round up timestamps so low FPS still emits a very short clip.
    let rounding = if sizing_mode == AvifSizingMode::TargetSize {
        ":round=up"
    } else {
        ""
    };
    let vf = format!(
        "fps={}{},scale='2*trunc(min({},iw)/2)':-2",
        parameters.fps, rounding, parameters.width
    );
    let mut cmd = Command::new("ffmpeg");
    cmd.args(http_input_args(source));
    cmd.arg("-y")
        .arg("-ss")
        .arg(format!("{start_secs:.3}"))
        .arg("-i")
        .arg(source)
        .arg("-t")
        .arg(format!("{duration:.3}"))
        .arg("-vf")
        .arg(vf)
        .arg("-c:v")
        .arg(encoder.as_str())
        .arg("-crf")
        .arg(parameters.crf.to_string());
    if encoder == AnimatedScreenshotEncoder::Libsvtav1 {
        cmd.args(["-preset", "8"]);
    }
    cmd.args(["-an", "-movflags", "+faststart"])
        .arg(output_path);
    cmd
}

fn sample_ranges(start_secs: f64, duration: f64) -> Vec<(f64, f64)> {
    if duration <= 3.0 {
        vec![(start_secs, duration)]
    } else {
        // Beginning, middle, and end capture more than a single still scene.
        vec![
            (start_secs, 1.0),
            (start_secs + (duration - 1.0) / 2.0, 1.0),
            (start_secs + duration - 1.0, 1.0),
        ]
    }
}

async fn estimate_size(
    source: &str,
    start_secs: f64,
    duration: f64,
    parameters: AvifParameters,
    encoder: AnimatedScreenshotEncoder,
) -> Result<u64> {
    let sample_path = temp_dir().join(format!("{}-sample.avif", Uuid::new_v4()));
    let result = async {
        let mut total_bytes = 0;
        let mut sampled_duration = 0.0;
        for (sample_start, sample_duration) in sample_ranges(start_secs, duration) {
            run_ffmpeg(build_avif_command(
                source,
                sample_start,
                sample_duration,
                parameters,
                encoder,
                AvifSizingMode::TargetSize,
                &sample_path,
            ))
            .await?;
            let bytes = tokio::fs::metadata(&sample_path).await?.len();
            if bytes == 0 {
                bail!("AVIF size sample was empty");
            }
            total_bytes += bytes;
            sampled_duration += sample_duration;
        }
        // Including each sample's container/keyframe overhead is deliberately
        // conservative. Scene changes and inter-frame compression still make
        // this an estimate, not a guaranteed maximum size.
        let estimated_bytes = (total_bytes as f64 * duration / sampled_duration).ceil() as u64;
        debug!(?parameters, estimated_bytes, "Estimated AVIF size");
        Ok(estimated_bytes)
    }
    .await;
    cleanup_temp_file(&sample_path).await;
    result
}

async fn encode_with_settings(
    source: &str,
    start_secs: f64,
    duration: f64,
    config: &MiningConfig,
    encoder: AnimatedScreenshotEncoder,
    output_path: &Path,
) -> Result<AvifParameters> {
    let parameters = match config.avif_sizing_mode {
        AvifSizingMode::Duration => {
            AvifParameters::for_duration(duration, config.avif_max_width, config.avif_max_fps)
        }
        AvifSizingMode::TargetSize => {
            let candidates = size_candidates(
                AvifParameters::from_caps(config.avif_max_width, config.avif_max_fps),
                config.avif_size_priority,
            );
            let target_bytes = u64::from(config.avif_target_size_kb) * 1024;
            let (parameters, estimated_bytes) =
                choose_size_parameters(&candidates, target_bytes, |parameters| {
                    estimate_size(source, start_secs, duration, parameters, encoder)
                })
                .await?;
            info!(
                ?parameters,
                ?config.avif_size_priority,
                target_bytes,
                estimated_bytes,
                "Selected AVIF settings for target image size"
            );
            if estimated_bytes > target_bytes {
                warn!(
                    "AVIF size target is below the estimate at minimum settings; using the smallest settings"
                );
            }
            parameters
        }
    };
    run_ffmpeg(build_avif_command(
        source,
        start_secs,
        duration,
        parameters,
        encoder,
        config.avif_sizing_mode,
        output_path,
    ))
    .await?;
    Ok(parameters)
}

/// Generate an animated AVIF using duration tiers or an estimated size budget.
/// Width/FPS remain upper bounds in both modes. Encoder fallback re-estimates
/// size, since different encoders can produce substantially different sizes.
pub async fn generate_avif(
    source: &str,
    start_ms: i64,
    end_ms: i64,
    config: &MiningConfig,
) -> Result<(PathBuf, Vec<u8>)> {
    config.validate_avif_settings()?;
    let duration_ms = end_ms
        .checked_sub(start_ms)
        .filter(|duration| *duration > 0);
    let Some(duration_ms) = duration_ms else {
        bail!("Animated screenshot must have a positive duration");
    };
    let start_secs = start_ms as f64 / 1000.0;
    let duration = duration_ms as f64 / 1000.0;
    let output_path = temp_dir().join(format!("{}.avif", Uuid::new_v4()));
    let encoder = config.animated_screenshot_encoder;
    let primary_result = if FORCE_AVIF_ENCODER_FALLBACK {
        Err(anyhow::anyhow!("forced AVIF encoder fallback"))
    } else {
        encode_with_settings(source, start_secs, duration, config, encoder, &output_path).await
    };
    let parameters = match primary_result {
        Ok(parameters) => parameters,
        Err(primary_error) => {
            let fallback_encoder = encoder.fallback();
            warn!(
                "AVIF generation with {} failed; retrying with {}: {}",
                encoder.as_str(),
                fallback_encoder.as_str(),
                primary_error
            );
            cleanup_temp_file(&output_path).await;
            match encode_with_settings(
                source,
                start_secs,
                duration,
                config,
                fallback_encoder,
                &output_path,
            )
            .await
            {
                Ok(parameters) => parameters,
                Err(fallback_error) => {
                    cleanup_temp_file(&output_path).await;
                    bail!(
                        "ffmpeg AVIF generation failed with {} and {}\n{}: {}\n{}: {}",
                        encoder.as_str(),
                        fallback_encoder.as_str(),
                        encoder.as_str(),
                        primary_error,
                        fallback_encoder.as_str(),
                        fallback_error
                    );
                }
            }
        }
    };
    let data = match tokio::fs::read(&output_path).await {
        Ok(data) => data,
        Err(error) => {
            cleanup_temp_file(&output_path).await;
            return Err(error.into());
        }
    };
    info!(
        "Generated AVIF: {:.1}s, max {}px @ {}fps, CRF {} ({} bytes)",
        duration,
        parameters.width,
        parameters.fps,
        parameters.crf,
        data.len()
    );
    Ok((output_path, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modeled_size(parameters: AvifParameters) -> u64 {
        u64::from(parameters.fps) * u64::from(parameters.width).pow(2)
            / u64::from(parameters.crf - BASE_CRF + 1)
    }

    #[test]
    fn duration_mode_preserves_existing_thresholds() {
        for (duration, fps, width, crf) in [
            (5.0, 10, 480, 40),
            (5.001, 8, 400, 42),
            (10.0, 8, 400, 42),
            (10.001, 6, 360, 45),
        ] {
            assert_eq!(
                AvifParameters::for_duration(duration, 480, 10),
                AvifParameters { fps, width, crf }
            );
        }
    }

    #[tokio::test]
    async fn fitting_size_preserves_full_caps_for_both_preferences() {
        let caps = AvifParameters::from_caps(1280, 30);
        for priority in [AvifSizePriority::PreferFps, AvifSizePriority::PreferQuality] {
            let mut calls = 0;
            let (selected, size) =
                choose_size_parameters(&size_candidates(caps, priority), 500 * 1024, |_| {
                    calls += 1;
                    std::future::ready(Ok(500 * 1024))
                })
                .await
                .unwrap();
            assert_eq!(selected, caps);
            assert_eq!(size, 500 * 1024);
            assert_eq!(calls, 1);
        }
    }

    #[tokio::test]
    async fn size_preferences_preserve_the_requested_dimension_first() {
        let caps = AvifParameters::from_caps(480, 10);
        let target = modeled_size(caps) / 2;
        for priority in [AvifSizePriority::PreferFps, AvifSizePriority::PreferQuality] {
            let candidates = size_candidates(caps, priority);
            let (selected, estimated) =
                choose_size_parameters(&candidates, target, |p| async move { Ok(modeled_size(p)) })
                    .await
                    .unwrap();
            assert!(estimated <= target);
            match priority {
                AvifSizePriority::PreferFps => {
                    assert_eq!(selected.fps, caps.fps);
                    assert!(selected.crf > caps.crf);
                }
                AvifSizePriority::PreferQuality => {
                    assert_eq!(selected.crf, caps.crf);
                    assert_eq!(selected.width, caps.width);
                    assert!(selected.fps < caps.fps);
                }
            }
            let previous = candidates.iter().position(|p| *p == selected).unwrap() - 1;
            assert!(modeled_size(candidates[previous]) > target);
        }
    }

    #[tokio::test]
    async fn impossible_target_stops_at_minimum_settings() {
        let candidates = size_candidates(
            AvifParameters::from_caps(480, 30),
            AvifSizePriority::PreferFps,
        );
        let mut calls = 0;
        let (selected, estimated) = choose_size_parameters(&candidates, 1, |p| {
            calls += 1;
            std::future::ready(Ok(modeled_size(p)))
        })
        .await
        .unwrap();
        assert_eq!(
            selected,
            AvifParameters {
                fps: 1,
                width: MIN_WIDTH,
                crf: MAX_CRF
            }
        );
        assert!(estimated > 1);
        assert_eq!(calls, 2);
    }

    #[tokio::test]
    async fn size_search_is_bounded_and_propagates_sampling_errors() {
        let candidates = size_candidates(
            AvifParameters::from_caps(1280, 30),
            AvifSizePriority::PreferQuality,
        );
        let mut calls = 0;
        choose_size_parameters(&candidates, 100_000, |p| {
            calls += 1;
            std::future::ready(Ok(modeled_size(p)))
        })
        .await
        .unwrap();
        assert!(calls <= 2 + candidates.len().ilog2() + 1);

        let error = choose_size_parameters(&candidates, 100_000, |_| async {
            bail!("encoder unavailable")
        })
        .await
        .unwrap_err();
        assert!(error.to_string().contains("encoder unavailable"));
    }

    #[test]
    fn candidates_stay_within_caps_and_never_have_odd_or_zero_dimensions() {
        for (width, fps) in [
            (0, 0),
            (101, 1),
            (481, 10),
            (1280, 30),
            (u32::MAX, u32::MAX),
        ] {
            let caps = AvifParameters::from_caps(width, fps);
            for priority in [AvifSizePriority::PreferFps, AvifSizePriority::PreferQuality] {
                let candidates = size_candidates(caps, priority);
                assert!(candidates.len() < 200);
                assert!(candidates.iter().all(|p| {
                    p.width >= 2
                        && p.width <= caps.width
                        && p.width % 2 == 0
                        && p.fps >= 1
                        && p.fps <= caps.fps
                        && (BASE_CRF..=MAX_CRF).contains(&p.crf)
                }));
                assert!(candidates.windows(2).all(|pair| pair[0] != pair[1]));
            }
        }
    }

    #[test]
    fn samples_cover_clip_without_reading_outside_its_range() {
        for duration in [0.05, 1.0, 3.0, 3.001, 12.0, 60.0] {
            let ranges = sample_ranges(9.25, duration);
            assert!(ranges.iter().all(|(start, length)| {
                *start >= 9.25 && *length > 0.0 && start + length <= 9.25 + duration
            }));
            assert!(ranges.iter().map(|(_, length)| length).sum::<f64>() <= 3.0);
            assert_eq!(ranges.first().unwrap().0, 9.25);
            let (last_start, last_duration) = ranges.last().unwrap();
            assert_eq!(last_start + last_duration, 9.25 + duration);
        }
    }

    #[tokio::test]
    async fn invalid_duration_and_target_fail_before_running_ffmpeg() {
        let config = MiningConfig::default();
        for (start, end) in [(10, 10), (20, 10), (i64::MIN, i64::MAX)] {
            assert!(generate_avif("unused", start, end, &config).await.is_err());
        }
        let invalid = MiningConfig {
            avif_sizing_mode: AvifSizingMode::TargetSize,
            avif_target_size_kb: 0,
            ..config
        };
        assert!(generate_avif("unused", 0, 1000, &invalid).await.is_err());
    }

    /// Run explicitly on hosts with ffmpeg, ffprobe, libsvtav1, and libaom-av1.
    #[tokio::test]
    #[ignore = "requires ffmpeg AV1 encoders and ffprobe"]
    async fn ffmpeg_size_mode_smoke() {
        let source = temp_dir().join(format!("{}-fixture.mkv", Uuid::new_v4()));
        let mut fixture = Command::new("ffmpeg");
        fixture
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=320x180:rate=30",
                "-t",
                "12",
                "-c:v",
                "ffv1",
            ])
            .arg(&source);
        run_ffmpeg(fixture).await.unwrap();
        let source_str = source.to_str().unwrap();
        let result: Result<()> = async {
            for encoder in [
                AnimatedScreenshotEncoder::Libsvtav1,
                AnimatedScreenshotEncoder::LibaomAv1,
            ] {
                let caps = AvifParameters::from_caps(480, 10);
                let baseline = estimate_size(source_str, 2.0, 8.0, caps, encoder).await?;
                for priority in [AvifSizePriority::PreferFps, AvifSizePriority::PreferQuality] {
                    let config = MiningConfig {
                        animated_screenshot_encoder: encoder,
                        avif_sizing_mode: AvifSizingMode::TargetSize,
                        avif_target_size_kb: ((baseline as f64 * 0.7 / 1024.0) as u32).max(1),
                        avif_size_priority: priority,
                        ..MiningConfig::default()
                    };
                    let output = temp_dir().join(format!("{}-smoke.avif", Uuid::new_v4()));
                    let selected =
                        encode_with_settings(source_str, 2.0, 8.0, &config, encoder, &output)
                            .await?;
                    let probe = Command::new("ffprobe")
                        .args([
                            "-v",
                            "error",
                            "-show_entries",
                            "stream=codec_name,width,height,nb_frames",
                            "-of",
                            "json",
                        ])
                        .arg(&output)
                        .output()
                        .await?;
                    let bytes = tokio::fs::metadata(&output).await?.len();
                    cleanup_temp_file(&output).await;
                    assert!(probe.status.success());
                    let probe: serde_json::Value = serde_json::from_slice(&probe.stdout)?;
                    // AVIF can expose a still cover stream before its animation.
                    let stream = probe["streams"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|stream| {
                            stream["nb_frames"]
                                .as_str()
                                .and_then(|n| n.parse::<u32>().ok())
                                .is_some_and(|n| n > 1)
                        })
                        .expect("AVIF must contain an animated stream");
                    assert_eq!(stream["codec_name"], "av1");
                    assert!(stream["width"].as_u64().unwrap() <= 320);
                    assert_eq!(stream["width"].as_u64().unwrap() % 2, 0);
                    assert_eq!(stream["height"].as_u64().unwrap() % 2, 0);
                    assert!(stream["nb_frames"].as_str().unwrap().parse::<u32>()? > 1);
                    assert_ne!(selected, caps);
                    match priority {
                        AvifSizePriority::PreferFps => assert_eq!(selected.fps, caps.fps),
                        AvifSizePriority::PreferQuality => {
                            assert_eq!(selected.width, caps.width);
                            assert_eq!(selected.crf, caps.crf);
                        }
                    }
                    eprintln!(
                        "{} {priority:?}: {selected:?}, target={} KB, actual={bytes} bytes",
                        encoder.as_str(),
                        config.avif_target_size_kb
                    );
                }
            }
            // A tiny target must still produce a decodable frame for short clips.
            let config = MiningConfig {
                avif_sizing_mode: AvifSizingMode::TargetSize,
                avif_target_size_kb: 1,
                ..MiningConfig::default()
            };
            let (path, data) = generate_avif(source_str, 0, 100, &config).await?;
            cleanup_temp_file(&path).await;
            assert!(!data.is_empty());
            Ok(())
        }
        .await;
        cleanup_temp_file(&source).await;
        result.unwrap();
    }
}
