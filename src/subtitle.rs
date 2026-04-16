use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtitleLine {
    pub index: usize,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtitleTrack {
    pub lines: Vec<SubtitleLine>,
}

impl SubtitleTrack {
    pub fn line_at_time(&self, time_ms: i64) -> Option<usize> {
        self.lines
            .iter()
            .position(|l| time_ms >= l.start_ms && time_ms <= l.end_ms)
    }

    pub fn nearest_line(&self, time_ms: i64) -> Option<usize> {
        if self.lines.is_empty() {
            return None;
        }
        let mut best_idx = 0;
        let mut best_dist = i64::MAX;
        for (i, line) in self.lines.iter().enumerate() {
            let mid = (line.start_ms + line.end_ms) / 2;
            let dist = (mid - time_ms).abs();
            if dist < best_dist {
                best_dist = dist;
                best_idx = i;
            }
        }
        Some(best_idx)
    }
}

/// Parse VTT subtitle content into a SubtitleTrack.
pub fn parse_vtt(content: &str) -> SubtitleTrack {
    let mut lines = Vec::new();
    let mut index = 0;
    // Matches both HH:MM:SS.mmm and MM:SS.mmm (Emby serves the two-component form)
    let timestamp_re =
        Regex::new(r"((?:\d+:)?\d{2}:\d{2}[.,]\d{3})\s*-->\s*((?:\d+:)?\d{2}:\d{2}[.,]\d{3})")
            .unwrap();

    let raw_lines: Vec<&str> = content.lines().collect();
    let mut i = 0;

    while i < raw_lines.len() {
        let line = raw_lines[i].trim();

        if let Some(caps) = timestamp_re.captures(line) {
            let start_ms = parse_vtt_timestamp(&caps[1]);
            let end_ms = parse_vtt_timestamp(&caps[2]);

            i += 1;
            let mut text_parts = Vec::new();
            while i < raw_lines.len() && !raw_lines[i].trim().is_empty() {
                text_parts.push(raw_lines[i].trim());
                i += 1;
            }

            let text = strip_tags(&text_parts.join("\n"));
            if !text.is_empty() {
                lines.push(SubtitleLine {
                    index,
                    start_ms,
                    end_ms,
                    text,
                });
                index += 1;
            }
        } else {
            i += 1;
        }
    }

    SubtitleTrack { lines }
}

/// Parse SRT subtitle content into a SubtitleTrack.
pub fn parse_srt(content: &str) -> SubtitleTrack {
    let mut lines = Vec::new();
    let mut index = 0;
    let timestamp_re =
        Regex::new(r"(\d{2}:\d{2}:\d{2}[.,]\d{3})\s*-->\s*(\d{2}:\d{2}:\d{2}[.,]\d{3})").unwrap();

    let raw_lines: Vec<&str> = content.lines().collect();
    let mut i = 0;

    while i < raw_lines.len() {
        let line = raw_lines[i].trim();

        // SRT blocks: number, timestamp, text, blank line
        if line.parse::<u32>().is_ok() {
            i += 1;
            if i >= raw_lines.len() {
                break;
            }
            let ts_line = raw_lines[i].trim();
            if let Some(caps) = timestamp_re.captures(ts_line) {
                let start_ms = parse_vtt_timestamp(&caps[1]);
                let end_ms = parse_vtt_timestamp(&caps[2]);

                i += 1;
                let mut text_parts = Vec::new();
                while i < raw_lines.len() && !raw_lines[i].trim().is_empty() {
                    text_parts.push(raw_lines[i].trim());
                    i += 1;
                }

                let text = strip_tags(&text_parts.join("\n"));
                if !text.is_empty() {
                    lines.push(SubtitleLine {
                        index,
                        start_ms,
                        end_ms,
                        text,
                    });
                    index += 1;
                }
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }

    SubtitleTrack { lines }
}

/// Parse ASS/SSA subtitle content into a SubtitleTrack.
pub fn parse_ass(content: &str) -> SubtitleTrack {
    let mut lines = Vec::new();
    let mut index = 0;
    let mut in_events = false;
    let mut format_fields: Vec<String> = Vec::new();
    let mut text_idx = None;
    let mut start_idx = None;
    let mut end_idx = None;

    for raw_line in content.lines() {
        let line = raw_line.trim();

        if line.eq_ignore_ascii_case("[Events]") {
            in_events = true;
            continue;
        }
        if line.starts_with('[') && in_events {
            break; // Left events section
        }

        if in_events {
            if let Some(rest) = line.strip_prefix("Format:") {
                format_fields = rest.split(',').map(|s| s.trim().to_string()).collect();
                text_idx = format_fields.iter().position(|f| f == "Text");
                start_idx = format_fields.iter().position(|f| f == "Start");
                end_idx = format_fields.iter().position(|f| f == "End");
                continue;
            }

            if let Some(rest) = line.strip_prefix("Dialogue:") {
                if let (Some(ti), Some(si), Some(ei)) = (text_idx, start_idx, end_idx) {
                    // Split only up to the number of format fields - 1 to keep text intact
                    let parts: Vec<&str> = rest.splitn(format_fields.len(), ',').collect();

                    if parts.len() >= format_fields.len() {
                        let start_ms = parse_ass_timestamp(parts[si].trim());
                        let end_ms = parse_ass_timestamp(parts[ei].trim());
                        let raw_text = parts[ti].trim();
                        let text = strip_ass_tags(raw_text);

                        if !text.is_empty() {
                            lines.push(SubtitleLine {
                                index,
                                start_ms,
                                end_ms,
                                text,
                            });
                            index += 1;
                        }
                    }
                }
            }
        }
    }

    // Sort by start time (ASS events might not be in order)
    lines.sort_by_key(|l| l.start_ms);
    for (i, line) in lines.iter_mut().enumerate() {
        line.index = i;
    }

    SubtitleTrack { lines }
}

/// Auto-detect format and parse subtitle content.
pub fn parse_subtitle(content: &str, filename_hint: Option<&str>) -> SubtitleTrack {
    let ext = filename_hint
        .and_then(|f| f.rsplit('.').next())
        .unwrap_or("");

    match ext.to_lowercase().as_str() {
        "srt" => parse_srt(content),
        "ass" | "ssa" => parse_ass(content),
        "vtt" => parse_vtt(content),
        _ => {
            // Auto-detect by content
            if content.starts_with("WEBVTT") {
                parse_vtt(content)
            } else if content.contains("[Events]") || content.contains("[Script Info]") {
                parse_ass(content)
            } else {
                parse_srt(content)
            }
        }
    }
}

/// Parse a VTT/SRT timestamp string into milliseconds.
/// Handles both `HH:MM:SS.mmm` and `MM:SS.mmm` formats.
fn parse_vtt_timestamp(ts: &str) -> i64 {
    // Split on the decimal separator first
    let (time_part, frac_part) = if let Some(p) = ts.find('.') {
        (&ts[..p], &ts[p + 1..])
    } else if let Some(p) = ts.find(',') {
        (&ts[..p], &ts[p + 1..])
    } else {
        (ts, "0")
    };
    let ms: i64 = frac_part.parse().unwrap_or(0);
    let components: Vec<&str> = time_part.split(':').collect();
    match components.len() {
        3 => {
            let h: i64 = components[0].parse().unwrap_or(0);
            let m: i64 = components[1].parse().unwrap_or(0);
            let s: i64 = components[2].parse().unwrap_or(0);
            h * 3_600_000 + m * 60_000 + s * 1_000 + ms
        }
        2 => {
            let m: i64 = components[0].parse().unwrap_or(0);
            let s: i64 = components[1].parse().unwrap_or(0);
            m * 60_000 + s * 1_000 + ms
        }
        _ => 0,
    }
}

/// Parse ASS timestamp format: H:MM:SS.CC (centiseconds)
fn parse_ass_timestamp(ts: &str) -> i64 {
    let parts: Vec<&str> = ts.split(':').collect();
    if parts.len() != 3 {
        return 0;
    }
    let h: i64 = parts[0].parse().unwrap_or(0);
    let m: i64 = parts[1].parse().unwrap_or(0);

    let sec_parts: Vec<&str> = parts[2].split('.').collect();
    let s: i64 = sec_parts.first().and_then(|v| v.parse().ok()).unwrap_or(0);
    let cs: i64 = sec_parts.get(1).and_then(|v| v.parse().ok()).unwrap_or(0);

    h * 3_600_000 + m * 60_000 + s * 1_000 + cs * 10
}

/// Strip HTML tags from subtitle text.
fn strip_tags(text: &str) -> String {
    let tag_re = Regex::new(r"<[^>]+>").unwrap();
    tag_re.replace_all(text, "").trim().to_string()
}

/// Strip ASS override tags like {\b1}, {\an8}, etc. and convert \N to newlines.
fn strip_ass_tags(text: &str) -> String {
    let tag_re = Regex::new(r"\{[^}]*\}").unwrap();
    let result = tag_re.replace_all(text, "");
    result
        .replace("\\N", "\n")
        .replace("\\n", "\n")
        .trim()
        .to_string()
}

/// Normalize Japanese text for fuzzy matching: strip whitespace, punctuation, etc.
pub fn normalize_japanese(text: &str) -> String {
    text.chars()
        .filter(|c| {
            !c.is_whitespace()
                && !matches!(
                    c,
                    '。' | '、'
                        | '！'
                        | '？'
                        | '「'
                        | '」'
                        | '『'
                        | '』'
                        | '（'
                        | '）'
                        | '【'
                        | '】'
                        | '・'
                        | '…'
                        | '―'
                        | '.'
                        | ','
                        | '!'
                        | '?'
                        | '"'
                        | '\''
                        | '('
                        | ')'
                        | ' '
                )
        })
        .collect()
}

/// Find the best matching subtitle line for a given sentence near a position.
/// Returns the index of the matched line.
pub fn find_matching_line(
    track: &SubtitleTrack,
    sentence: &str,
    position_ms: i64,
    window_ms: i64,
) -> Option<usize> {
    // Strip HTML that Yomitan embeds (e.g. <b>word</b>) before matching
    let clean = strip_tags(sentence);
    let normalized_sentence = normalize_japanese(&clean);
    if normalized_sentence.is_empty() {
        return None;
    }

    let window_start = position_ms - window_ms;
    let window_end = position_ms + window_ms;

    // Gather candidates within window
    let mut candidates: Vec<(usize, f64)> = Vec::new();

    for line in &track.lines {
        if line.end_ms < window_start || line.start_ms > window_end {
            continue;
        }

        let normalized_line = normalize_japanese(&line.text);
        if normalized_line.is_empty() {
            continue;
        }

        // Exact substring match
        if normalized_line.contains(&normalized_sentence)
            || normalized_sentence.contains(&normalized_line)
        {
            // Score by proximity to current position
            let time_diff = ((line.start_ms + line.end_ms) / 2 - position_ms).abs() as f64;
            let score = 1.0 - (time_diff / window_ms as f64);
            candidates.push((line.index, score + 1.0)); // Bonus for exact match
        } else {
            // Partial overlap: count matching characters
            let overlap = longest_common_subsequence(&normalized_sentence, &normalized_line);
            let max_len = normalized_sentence.len().max(normalized_line.len());
            if max_len > 0 {
                let similarity = overlap as f64 / max_len as f64;
                if similarity > 0.5 {
                    let time_diff = ((line.start_ms + line.end_ms) / 2 - position_ms).abs() as f64;
                    let time_score = 1.0 - (time_diff / window_ms as f64);
                    candidates.push((line.index, similarity + time_score * 0.3));
                }
            }
        }
    }

    candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    candidates.first().map(|(idx, _)| *idx)
}

/// Find ALL subtitle lines that match a sentence across the entire track (no time window).
/// Returns a list of (line_index, score) sorted by score descending.
/// Used for history-mode matching where no playback position is available.
pub fn find_all_matching_lines(track: &SubtitleTrack, sentence: &str) -> Vec<(usize, f64)> {
    let clean = strip_tags(sentence);
    let normalized_sentence = normalize_japanese(&clean);
    if normalized_sentence.is_empty() {
        return Vec::new();
    }

    let mut candidates: Vec<(usize, f64)> = Vec::new();

    for line in &track.lines {
        let normalized_line = normalize_japanese(&line.text);
        if normalized_line.is_empty() {
            continue;
        }

        if normalized_line.contains(&normalized_sentence)
            || normalized_sentence.contains(&normalized_line)
        {
            candidates.push((line.index, 2.0));
        } else {
            let overlap = longest_common_subsequence(&normalized_sentence, &normalized_line);
            let max_len = normalized_sentence.len().max(normalized_line.len());
            if max_len > 0 {
                let similarity = overlap as f64 / max_len as f64;
                if similarity > 0.5 {
                    candidates.push((line.index, similarity));
                }
            }
        }
    }

    candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    candidates
}

/// Simple LCS length computation for fuzzy matching.
fn longest_common_subsequence(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let m = a_chars.len();
    let n = b_chars.len();

    // Use two rows to save memory
    let mut prev = vec![0usize; n + 1];
    let mut curr = vec![0usize; n + 1];

    for i in 1..=m {
        for j in 1..=n {
            if a_chars[i - 1] == b_chars[j - 1] {
                curr[j] = prev[j - 1] + 1;
            } else {
                curr[j] = prev[j].max(curr[j - 1]);
            }
        }
        std::mem::swap(&mut prev, &mut curr);
        curr.iter_mut().for_each(|v| *v = 0);
    }

    *prev.iter().max().unwrap_or(&0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_vtt() {
        let content = r#"WEBVTT

00:00:01.000 --> 00:00:04.000
こんにちは世界

00:00:05.000 --> 00:00:08.000
これはテストです
"#;
        let track = parse_vtt(content);
        assert_eq!(track.lines.len(), 2);
        assert_eq!(track.lines[0].text, "こんにちは世界");
        assert_eq!(track.lines[0].start_ms, 1000);
        assert_eq!(track.lines[0].end_ms, 4000);
        assert_eq!(track.lines[1].text, "これはテストです");
    }

    #[test]
    fn test_parse_srt() {
        let content = r#"1
00:00:01,000 --> 00:00:04,000
こんにちは世界

2
00:00:05,000 --> 00:00:08,000
これはテストです
"#;
        let track = parse_srt(content);
        assert_eq!(track.lines.len(), 2);
        assert_eq!(track.lines[0].text, "こんにちは世界");
    }

    #[test]
    fn test_parse_ass() {
        let content = r#"[Script Info]
Title: Test

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:01.00,0:00:04.00,Default,,0,0,0,,{\b1}こんにちは{\b0}世界
Dialogue: 0,0:00:05.00,0:00:08.00,Default,,0,0,0,,これはテストです
"#;
        let track = parse_ass(content);
        assert_eq!(track.lines.len(), 2);
        assert_eq!(track.lines[0].text, "こんにちは世界");
        assert_eq!(track.lines[0].start_ms, 1000);
    }

    #[test]
    fn test_normalize_japanese() {
        let text = "「こんにちは、世界！」";
        let normalized = normalize_japanese(text);
        assert_eq!(normalized, "こんにちは世界");
    }

    #[test]
    fn test_find_matching_line() {
        let track = SubtitleTrack {
            lines: vec![
                SubtitleLine {
                    index: 0,
                    start_ms: 0,
                    end_ms: 3000,
                    text: "始まりましょう".to_string(),
                },
                SubtitleLine {
                    index: 1,
                    start_ms: 3000,
                    end_ms: 6000,
                    text: "こんにちは世界".to_string(),
                },
                SubtitleLine {
                    index: 2,
                    start_ms: 6000,
                    end_ms: 9000,
                    text: "さようなら".to_string(),
                },
            ],
        };

        let result = find_matching_line(&track, "こんにちは世界", 4000, 30_000);
        assert_eq!(result, Some(1));
    }
}
