use super::{Candidate, Workspace, dictionary::Lexicon};
use crate::{session::HistoryEntry, subtitle::SubtitleTrack};
use anyhow::{Context, bail};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use sudachi::{
    analysis::{Mode, stateful_tokenizer::StatefulTokenizer},
    dic::{dictionary::JapaneseDictionary, word_id::WordId},
    prelude::MorphemeList,
};

pub fn hiragana(text: &str) -> String {
    text.chars()
        .map(|c| {
            if ('ァ'..='ヶ').contains(&c) {
                char::from_u32(c as u32 - 0x60).unwrap_or(c)
            } else {
                c
            }
        })
        .collect()
}

pub fn analyze(
    history: HistoryEntry,
    mut track: SubtitleTrack,
    name: String,
    lexicon: Option<Arc<Lexicon>>,
    dictionary: Arc<JapaneseDictionary>,
    split_mode: String,
) -> anyhow::Result<Workspace> {
    if track.lines.is_empty() {
        bail!("No subtitles are saved for this file. Choose an SRT, VTT, or ASS subtitle file.");
    }
    if track.lines.len() > 50_000
        || track.lines.iter().map(|l| l.text.len()).sum::<usize>() > 2_000_000
    {
        bail!("This subtitle is too large (maximum 50,000 lines or 2 MB of text)");
    }
    track.lines.retain(|line| {
        line.end_ms > line.start_ms && line.end_ms > 0 && !line.text.trim().is_empty()
    });
    track.lines.sort_by_key(|line| (line.start_ms, line.end_ms));
    for (index, line) in track.lines.iter_mut().enumerate() {
        line.index = index;
    }
    let mode = match split_mode.as_str() {
        "A" => Mode::A,
        "B" => Mode::B,
        "C" => Mode::C,
        _ => bail!("Choose Sudachi split mode A, B, or C"),
    };
    let mut tokenizer = StatefulTokenizer::create(dictionary.clone(), false, mode);
    let mut morphemes = MorphemeList::empty(dictionary.clone());
    let mut candidates: BTreeMap<String, Candidate> = BTreeMap::new();
    let mut line_words = Vec::new();
    for (index, line) in track.lines.iter().enumerate() {
        let mut words = BTreeSet::new();
        tokenizer.reset().push_str(&line.text);
        tokenizer
            .do_tokenize()
            .with_context(|| format!("Sudachi could not analyze subtitle line {}", index + 1))?;
        morphemes.collect_results(&mut tokenizer)?;
        for token in morphemes.iter() {
            let surface = token.surface().to_string();
            let details = token.part_of_speech();
            let pos = details.first().map(String::as_str).unwrap_or("");
            let sub = details.get(1).map(String::as_str).unwrap_or("");
            if !matches!(
                pos,
                "名詞" | "動詞" | "形容詞" | "形状詞" | "副詞" | "感動詞"
            ) || details.iter().any(|s| s == "数詞")
                || !surface
                    .chars()
                    .any(|c| matches!(c, '\u{3040}'..='\u{30ff}' | '\u{3400}'..='\u{9fff}' | '々'))
            {
                continue;
            }
            let term = if token.dictionary_form().is_empty() {
                surface.clone()
            } else {
                token.dictionary_form().to_string()
            };
            // 非自立可能 means a word *can* be auxiliary, not that it is one here.
            // Keep ordinary verbs such as 行く available. Use the lemma's reading
            // as well as its spelling, including when JMdict is unavailable.
            let lemma_id = token.get_word_info().dictionary_form_word_id();
            let reading = if lemma_id >= 0 && !token.is_oov() {
                dictionary
                    .lexicon()
                    .get_word_info(WordId::new(token.word_id().dic(), lemma_id as u32))
                    .map(|info| hiragana(info.reading_form()))
                    .unwrap_or_else(|_| hiragana(token.reading_form()))
            } else {
                hiragana(token.reading_form())
            };
            words.insert(term.clone());
            let candidate = candidates.entry(term.clone()).or_insert_with(|| {
                let definitions = lexicon
                    .as_ref()
                    .map(|l| l.lookup(&term, &reading))
                    .unwrap_or_default();
                Candidate {
                    term,
                    reading: definitions
                        .first()
                        .map(|d| d.reading.clone())
                        .unwrap_or(reading),
                    common: definitions.iter().any(|d| d.common),
                    definitions,
                    part_of_speech: if sub == "固有名詞" {
                        "proper noun".into()
                    } else {
                        match pos {
                            "名詞" => "noun",
                            "動詞" => "verb",
                            "形容詞" | "形状詞" => "adjective",
                            "副詞" => "adverb",
                            "感動詞" => "interjection",
                            _ => "unknown",
                        }
                        .into()
                    },
                    occurrences: Vec::new(),
                    surfaces: Vec::new(),
                }
            });
            if candidate.occurrences.last() != Some(&index) {
                candidate.occurrences.push(index);
            }
            if !candidate.surfaces.contains(&surface) && candidate.surfaces.len() < 20 {
                candidate.surfaces.push(surface);
            }
        }
        line_words.push(words.into_iter().collect());
    }
    Ok(Workspace {
        revision: uuid::Uuid::new_v4().to_string(),
        history,
        track,
        subtitle_name: name,
        split_mode,
        candidates: candidates.into_values().collect(),
        line_words,
        dictionary_date: lexicon.map(|l| l.date.clone()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::MediaServerKind, subtitle::parse_srt};
    #[tokio::test]
    #[ignore = "downloads Sudachi core; run explicitly for tokenizer integration validation"]
    async fn groups_inflections_and_ignores_particles_with_sparse_subtitle_indices() {
        let history = HistoryEntry {
            history_id: "jellyfin|one".into(),
            server_kind: MediaServerKind::Jellyfin,
            item_id: "one".into(),
            title: "Test".into(),
            series_name: None,
            media_source_id: "one".into(),
            file_path: None,
            duration_ms: Some(10000),
            subtitle_count: 2,
            audio_languages: vec![],
            last_position_ms: 0,
            last_seen: chrono::Utc::now(),
            previous_watches: Vec::new(),
        };
        let mut track = parse_srt(
            "1\n00:00:01,000 --> 00:00:03,000\n猫が魚を食べました。\n\n2\n00:00:04,000 --> 00:00:06,000\n猫は魚を食べる。\n",
        );
        track.lines[1].index = 99;
        let dictionary =
            super::super::tokenizer::load(std::path::PathBuf::from("data/nagare.sqlite"))
                .await
                .unwrap();
        let workspace = analyze(
            history,
            track,
            "test.srt".into(),
            None,
            dictionary,
            "B".into(),
        )
        .unwrap();
        let eat = workspace
            .candidates
            .iter()
            .find(|c| c.term == "食べる")
            .unwrap();
        assert_eq!(eat.occurrences, vec![0, 1]);
        assert_eq!(eat.reading, "たべる");
        assert_eq!(workspace.track.lines[1].index, 1);
        assert!(
            workspace
                .candidates
                .iter()
                .all(|c| c.term != "は" && c.term != "が")
        );
        assert!(workspace.line_words[0].contains(&"食べる".to_string()));
    }
}
