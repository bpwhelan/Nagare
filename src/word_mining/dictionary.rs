use super::db;
use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::{Cursor, Read},
    path::PathBuf,
    sync::{Arc, LazyLock},
};
use tokio::sync::Mutex;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Definition {
    pub reading: String,
    pub meaning: String,
    pub common: bool,
}

#[derive(Deserialize)]
struct Spelling {
    text: String,
    #[serde(default)]
    common: bool,
    #[serde(default, rename = "appliesToKanji")]
    applies_to_kanji: Vec<String>,
}

#[derive(Deserialize)]
struct Gloss {
    lang: String,
    text: String,
}

#[derive(Deserialize)]
struct Sense {
    gloss: Vec<Gloss>,
    #[serde(default, rename = "appliesToKanji")]
    kanji: Vec<String>,
    #[serde(default, rename = "appliesToKana")]
    kana: Vec<String>,
}

#[derive(Deserialize)]
struct Word {
    kanji: Vec<Spelling>,
    kana: Vec<Spelling>,
    sense: Vec<Sense>,
}

#[derive(Deserialize)]
struct Dictionary {
    #[serde(rename = "dictDate")]
    date: String,
    words: Vec<Word>,
}

pub struct Lexicon {
    pub date: String,
    entries: HashMap<String, Vec<Arc<Word>>>,
}

fn applies(restrictions: &[String], spelling: &str) -> bool {
    restrictions.is_empty() || restrictions.iter().any(|s| s == "*" || s == spelling)
}

impl Lexicon {
    fn from_archive(bytes: &[u8]) -> anyhow::Result<Self> {
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(Cursor::new(bytes)));
        for entry in archive.entries()? {
            let entry = entry?;
            if entry.path()?.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            if entry.size() > 300 * 1024 * 1024 {
                bail!("Dictionary is too large");
            }
            let dictionary: Dictionary = serde_json::from_reader(entry.take(300 * 1024 * 1024))?;
            if dictionary.words.len() < 10_000 {
                bail!("The downloaded dictionary is incomplete");
            }
            return Ok(Self::from_dictionary(dictionary));
        }
        bail!("No JMdict JSON was found in the download")
    }

    fn from_dictionary(dictionary: Dictionary) -> Self {
        let mut entries: HashMap<String, Vec<Arc<Word>>> = HashMap::new();
        for word in dictionary.words {
            let word = Arc::new(word);
            for spelling in word.kanji.iter().chain(&word.kana) {
                entries
                    .entry(spelling.text.clone())
                    .or_default()
                    .push(word.clone());
            }
        }
        Self {
            date: dictionary.date,
            entries,
        }
    }

    pub fn lookup(&self, term: &str, preferred_reading: &str) -> Vec<Definition> {
        let mut definitions = Vec::new();
        for word in self.entries.get(term).into_iter().flatten() {
            let is_kanji = word.kanji.iter().any(|s| s.text == term);
            for reading in &word.kana {
                if is_kanji && !applies(&reading.applies_to_kanji, term) {
                    continue;
                }
                if !is_kanji && reading.text != term {
                    continue;
                }
                let meaning = word
                    .sense
                    .iter()
                    .filter(|s| {
                        (!is_kanji || applies(&s.kanji, term)) && applies(&s.kana, &reading.text)
                    })
                    .map(|s| {
                        s.gloss
                            .iter()
                            .filter(|g| g.lang == "eng")
                            .map(|g| g.text.as_str())
                            .collect::<Vec<_>>()
                            .join("; ")
                    })
                    .filter(|s| !s.is_empty())
                    .take(6)
                    .collect::<Vec<_>>()
                    .join("\n");
                if meaning.is_empty() {
                    continue;
                }
                let common =
                    reading.common || word.kanji.iter().any(|s| s.text == term && s.common);
                if !definitions
                    .iter()
                    .any(|d: &Definition| d.reading == reading.text && d.meaning == meaning)
                {
                    definitions.push(Definition {
                        reading: reading.text.clone(),
                        meaning,
                        common,
                    });
                }
            }
        }
        definitions.sort_by_key(|d| (d.reading != preferred_reading, !d.common));
        definitions.truncate(8);
        definitions
    }
}

static CACHE: LazyLock<Mutex<Option<(PathBuf, Arc<Lexicon>)>>> = LazyLock::new(|| Mutex::new(None));

pub async fn load(path: PathBuf, update: bool) -> anyhow::Result<Arc<Lexicon>> {
    let mut cache = CACHE.lock().await;
    if !update {
        if let Some((cached_path, lexicon)) = &*cache {
            if cached_path == &path {
                return Ok(lexicon.clone());
            }
        }
        if let Some(bytes) = db::dictionary_archive(path.clone()).await? {
            let lexicon = Arc::new(
                tokio::task::spawn_blocking(move || Lexicon::from_archive(&bytes)).await??,
            );
            *cache = Some((path, lexicon.clone()));
            return Ok(lexicon);
        }
    }
    // Only public dictionary assets are downloaded; no media or subtitle text is sent.
    let client = reqwest::Client::builder()
        .user_agent("Nagare word mining")
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(120))
        .build()?;
    let release: serde_json::Value = client
        .get("https://api.github.com/repos/scriptin/jmdict-simplified/releases/latest")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let asset = release["assets"]
        .as_array()
        .context("Dictionary release has no assets")?
        .iter()
        .find(|a| {
            a["name"]
                .as_str()
                .is_some_and(|n| n.starts_with("jmdict-eng-3.") && n.ends_with(".json.tgz"))
        })
        .context("No compatible English JMdict download was found")?;
    let url = asset["browser_download_url"]
        .as_str()
        .context("Missing dictionary download URL")?;
    if !url.starts_with("https://github.com/scriptin/jmdict-simplified/releases/download/") {
        bail!("Unexpected dictionary download location");
    }
    let mut response = client.get(url).send().await?.error_for_status()?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > 40 * 1024 * 1024 {
            bail!("Dictionary download is too large");
        }
        bytes.extend_from_slice(&chunk);
    }
    let (lexicon, bytes) = tokio::task::spawn_blocking(move || {
        Lexicon::from_archive(&bytes).map(|lexicon| (lexicon, bytes))
    })
    .await??;
    db::save_dictionary(path.clone(), bytes).await?;
    let lexicon = Arc::new(lexicon);
    *cache = Some((path, lexicon.clone()));
    Ok(lexicon)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dictionary_respects_reading_and_sense_restrictions() {
        let data = serde_json::json!({"dictDate":"2026-09-14","words":[{
            "kanji":[{"text":"生","common":true}],
            "kana":[{"text":"なま","common":true,"appliesToKanji":["生"]},{"text":"せい","appliesToKanji":["生"]}],
            "sense":[{"appliesToKana":["なま"],"gloss":[{"lang":"eng","text":"raw"}]},
                {"appliesToKana":["せい"],"gloss":[{"lang":"eng","text":"life"}]}]
        }]});
        let lexicon = Lexicon::from_dictionary(serde_json::from_value(data).unwrap());
        let definitions = lexicon.lookup("生", "せい");
        assert_eq!(definitions[0].meaning, "life");
        assert_eq!(definitions[1].meaning, "raw");
        assert!(lexicon.lookup("missing", "").is_empty());
    }
}
