//! Native Sudachi.rs, pinned to the upstream 0.6.11 release. No Python process is required.
use anyhow::{Context, bail};
use serde_json::Value;
use std::{
    io::{Cursor, Read},
    path::PathBuf,
    sync::{Arc, LazyLock},
};
use sudachi::{
    config::Config,
    dic::{
        dictionary::JapaneseDictionary,
        storage::{Storage, SudachiDicData},
    },
};
use tokio::sync::Mutex;

pub const CORE_VERSION: &str = "20260723";
static CACHE: LazyLock<Mutex<Option<(PathBuf, Arc<JapaneseDictionary>)>>> =
    LazyLock::new(|| Mutex::new(None));

fn from_bytes(bytes: Vec<u8>) -> anyhow::Result<JapaneseDictionary> {
    Ok(JapaneseDictionary::from_cfg_storage_with_embedded_chardef(
        &Config::new_embedded()?,
        SudachiDicData::new(Storage::Owned(bytes)),
    )?)
}

pub async fn load(database_path: PathBuf) -> anyhow::Result<Arc<JapaneseDictionary>> {
    let custom = std::env::var_os("SUDACHI_DICT_PATH").map(PathBuf::from);
    let path = custom.clone().unwrap_or_else(|| {
        database_path
            .parent()
            .unwrap_or(std::path::Path::new("data"))
            .join("word-mining")
            .join(format!("sudachi-core-{CORE_VERSION}.dic"))
    });
    let mut cache = CACHE.lock().await;
    if let Some((cached_path, dict)) = &*cache {
        if cached_path == &path {
            return Ok(dict.clone());
        }
    }
    if path.exists() {
        let file = path.clone();
        let dict =
            Arc::new(tokio::task::spawn_blocking(move || from_bytes(std::fs::read(file)?)).await??);
        *cache = Some((path, dict.clone()));
        return Ok(dict);
    }
    if custom.is_some() {
        bail!("SUDACHI_DICT_PATH does not point to an existing Sudachi dictionary");
    }
    let client = reqwest::Client::builder()
        .user_agent("Nagare word mining")
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(180))
        .build()?;
    let metadata: Value = client
        .get(format!(
            "https://pypi.org/pypi/sudachidict_core/{CORE_VERSION}/json"
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let url = metadata["urls"]
        .as_array()
        .context("Sudachi dictionary download is unavailable")?
        .iter()
        .find(|a| {
            a["filename"]
                .as_str()
                .is_some_and(|n| n.ends_with("-py3-none-any.whl"))
        })
        .and_then(|a| a["url"].as_str())
        .context("Sudachi core dictionary wheel is unavailable")?;
    if !url.starts_with("https://files.pythonhosted.org/") {
        bail!("Unexpected Sudachi dictionary download location");
    }
    let mut response = client.get(url).send().await?.error_for_status()?;
    let mut archive = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if archive.len() + chunk.len() > 120 * 1024 * 1024 {
            bail!("Sudachi dictionary download is too large");
        }
        archive.extend_from_slice(&chunk);
    }
    let file = path.clone();
    let dict = tokio::task::spawn_blocking(move || -> anyhow::Result<JapaneseDictionary> {
        let mut zip = zip::ZipArchive::new(Cursor::new(archive))?;
        let mut bytes = Vec::new();
        {
            let entry = zip.by_name("sudachidict_core/resources/system.dic")?;
            if entry.size() > 400 * 1024 * 1024 {
                bail!("Sudachi dictionary is too large");
            }
            entry.take(400 * 1024 * 1024).read_to_end(&mut bytes)?;
        }
        let parent = file.parent().context("Missing dictionary directory")?;
        std::fs::create_dir_all(parent)?;
        // Retain upstream notices alongside the cached dictionary. Never extract archive paths.
        for index in 0..zip.len() {
            let mut entry = zip.by_index(index)?;
            let basename = entry.name().rsplit('/').next().unwrap_or("");
            if matches!(
                basename,
                "LICENSE" | "LICENSE.txt" | "LICENSE-2.0.txt" | "LEGAL" | "NOTICE"
            ) && entry.size() < 1024 * 1024
            {
                let filename = parent.join(format!("sudachi-{basename}"));
                let mut data = Vec::new();
                entry.read_to_end(&mut data)?;
                std::fs::write(filename, data)?;
            }
        }
        let temporary = file.with_extension("dic.tmp");
        std::fs::write(&temporary, &bytes)?;
        let dictionary = match from_bytes(bytes) {
            Ok(d) => d,
            Err(e) => {
                let _ = std::fs::remove_file(&temporary);
                return Err(e);
            }
        };
        std::fs::rename(temporary, file)?;
        Ok(dictionary)
    })
    .await??;
    let dict = Arc::new(dict);
    *cache = Some((path, dict.clone()));
    Ok(dict)
}
