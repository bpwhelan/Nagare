use super::MediaLibrary;
use crate::config::{MediaFilterConfig, MediaFilterMode};

impl MediaFilterConfig {
    pub fn filters_libraries(&self) -> bool {
        !self.included_libraries().is_empty() || !self.excluded_libraries().is_empty()
    }

    pub fn filters_folders(&self) -> bool {
        self.included_folders()
            .iter()
            .chain(self.excluded_folders())
            .any(|folder| !folder.trim().is_empty())
    }

    fn included_libraries(&self) -> &[String] {
        if self.mode == Some(MediaFilterMode::Exclude) {
            &[]
        } else {
            &self.included_libraries
        }
    }

    fn excluded_libraries(&self) -> &[String] {
        if self.mode == Some(MediaFilterMode::Include) {
            &[]
        } else {
            &self.excluded_libraries
        }
    }

    fn included_folders(&self) -> &[String] {
        if self.mode == Some(MediaFilterMode::Exclude) {
            &[]
        } else {
            &self.included_folders
        }
    }

    fn excluded_folders(&self) -> &[String] {
        if self.mode == Some(MediaFilterMode::Include) {
            &[]
        } else {
            &self.excluded_folders
        }
    }

    pub fn allows(
        &self,
        library_id: Option<&str>,
        path: Option<&str>,
        libraries: &[MediaLibrary],
    ) -> bool {
        if self.filters_folders() {
            // Unknown paths must not bypass an exclusion rule.
            let Some(path) = path.filter(|path| !path.trim().is_empty()) else {
                return false;
            };
            if self
                .excluded_folders()
                .iter()
                .any(|folder| path_is_in_folder(path, folder))
            {
                return false;
            }
            if self
                .included_folders()
                .iter()
                .any(|folder| !folder.trim().is_empty())
                && !self
                    .included_folders()
                    .iter()
                    .any(|folder| path_is_in_folder(path, folder))
            {
                return false;
            }
        }
        if !self.filters_libraries() {
            return true;
        }
        // Prefer the provider's actual library ID. Fall back to library roots
        // for providers such as Emby/Jellyfin whose session DTO omits that ID.
        let ids: Vec<&str> = if let Some(id) = library_id.filter(|id| !id.is_empty()) {
            vec![id]
        } else {
            libraries
                .iter()
                .filter(|library| {
                    path.is_some_and(|path| {
                        library
                            .paths
                            .iter()
                            .any(|root| path_is_in_folder(path, root))
                    })
                })
                .map(|library| library.id.as_str())
                .collect()
        };
        // Fail closed when a filtered item's library cannot be established.
        !ids.is_empty()
            && !ids.iter().any(|id| {
                self.excluded_libraries()
                    .iter()
                    .any(|excluded| excluded == id)
            })
            && (self.included_libraries().is_empty()
                || ids.iter().any(|id| {
                    self.included_libraries()
                        .iter()
                        .any(|included| included == id)
                }))
    }
}

fn normalized_path(value: &str) -> String {
    let slashes = value.trim().replace('\\', "/");
    let windows = slashes.starts_with("//") || slashes.as_bytes().get(1) == Some(&b':');
    let rooted = slashes.starts_with('/');
    let mut parts = Vec::new();
    for part in slashes.split('/') {
        match part {
            "" | "." => {}
            ".." if parts.last().is_some_and(|last| *last != "..") => {
                parts.pop();
            }
            ".." if rooted => {}
            _ => parts.push(part),
        }
    }
    let prefix = if slashes.starts_with("//") {
        "//"
    } else if rooted {
        "/"
    } else {
        ""
    };
    let normalized = format!("{prefix}{}", parts.join("/"));
    if windows {
        normalized.to_lowercase()
    } else {
        normalized
    }
}

pub(crate) fn path_is_in_folder(path: &str, folder: &str) -> bool {
    if path.trim().is_empty() || folder.trim().is_empty() {
        return false;
    }
    let path = normalized_path(path);
    let folder = normalized_path(folder);
    !folder.is_empty()
        && (path == folder
            || path
                .strip_prefix(&folder)
                .is_some_and(|suffix| folder == "/" || suffix.starts_with('/')))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn libraries() -> Vec<MediaLibrary> {
        vec![
            MediaLibrary {
                id: "jp".into(),
                name: "Japanese".into(),
                paths: vec!["/media/日本語".into()],
            },
            MediaLibrary {
                id: "en".into(),
                name: "English".into(),
                paths: vec!["/media/English".into()],
            },
        ]
    }

    #[test]
    fn folder_matching_respects_boundaries_case_and_path_styles() {
        assert!(path_is_in_folder(
            "/media/日本語/show/file.mkv",
            "/media/日本語/"
        ));
        assert!(!path_is_in_folder(
            "/media/日本語-extra/file.mkv",
            "/media/日本語"
        ));
        assert!(!path_is_in_folder(
            "/media/English/file.mkv",
            "/media/english"
        ));
        assert!(path_is_in_folder(
            r"D:\Media\Japanese\file.mkv",
            "d:/media/japanese/"
        ));
        assert!(path_is_in_folder(
            r"\\NAS\Media\Japanese\file.mkv",
            "//nas/media/japanese"
        ));
        assert!(path_is_in_folder("/media/file.mkv", "/"));
        assert!(path_is_in_folder(r"C:\file.mkv", r"c:\"));
        assert!(!path_is_in_folder(
            "/media/Japanese/../English/file.mkv",
            "/media/Japanese"
        ));
        assert!(!path_is_in_folder("/media/file.mkv", " "));
    }

    #[test]
    fn default_filters_preserve_existing_behavior() {
        assert!(MediaFilterConfig::default().allows(None, None, &[]));
        let old: crate::config::JellyfinConfig = serde_json::from_value(serde_json::json!({
            "url": "http://localhost", "api_key": "test", "users": ["user"]
        }))
        .unwrap();
        assert_eq!(old.media_filter, MediaFilterConfig::default());
    }

    #[test]
    fn whitelist_and_blacklist_modes_are_exclusive() {
        let mut filter = MediaFilterConfig {
            mode: Some(MediaFilterMode::Include),
            included_libraries: vec!["jp".into()],
            excluded_libraries: vec!["jp".into()],
            included_folders: vec!["/media/日本語".into()],
            excluded_folders: vec!["/media/日本語".into()],
        };
        // A whitelist ignores blacklist rules, even in a stale API payload.
        assert!(filter.allows(Some("jp"), Some("/media/日本語/show.mkv"), &[]));
        assert!(!filter.allows(Some("en"), Some("/media/English/show.mkv"), &[]));

        filter.mode = Some(MediaFilterMode::Exclude);
        assert!(!filter.allows(Some("jp"), Some("/media/日本語/show.mkv"), &[]));
        assert!(filter.allows(Some("en"), Some("/media/English/show.mkv"), &[]));
        let saved = serde_json::to_string(&filter).unwrap();
        let restored: MediaFilterConfig = serde_json::from_str(&saved).unwrap();
        assert_eq!(restored.mode, Some(MediaFilterMode::Exclude));
        assert!(restored.allows(Some("en"), Some("/media/English/show.mkv"), &[]));
    }

    #[test]
    fn inactive_lists_do_not_require_library_or_folder_metadata() {
        let filter = MediaFilterConfig {
            mode: Some(MediaFilterMode::Include),
            excluded_libraries: vec!["en".into()],
            excluded_folders: vec!["/English".into()],
            ..Default::default()
        };
        assert!(!filter.filters_libraries());
        assert!(!filter.filters_folders());
        assert!(filter.allows(None, None, &[]));
    }

    #[test]
    fn libraries_resolve_by_id_or_server_path_and_exclusions_win() {
        let mut filter = MediaFilterConfig {
            included_libraries: vec!["jp".into()],
            ..Default::default()
        };
        assert!(filter.allows(Some("jp"), None, &[]));
        assert!(filter.allows(None, Some("/media/日本語/file.mkv"), &libraries()));
        assert!(!filter.allows(None, Some("/media/English/file.mkv"), &libraries()));
        assert!(!filter.allows(Some("en"), Some("/media/日本語/file.mkv"), &libraries()));
        assert!(!filter.allows(None, None, &libraries()));
        filter.excluded_libraries.push("jp".into());
        assert!(!filter.allows(Some("jp"), None, &libraries()));
        filter.included_libraries.clear();
        assert!(filter.allows(Some("en"), None, &[]));
        assert!(!filter.allows(None, Some("/unknown/file.mkv"), &libraries()));
    }

    #[test]
    fn library_and_folder_includes_intersect_and_folder_exclusions_win() {
        let filter = MediaFilterConfig {
            included_libraries: vec!["jp".into()],
            included_folders: vec!["/media/日本語/anime".into()],
            excluded_folders: vec!["/media/日本語/anime/dubbed".into()],
            ..Default::default()
        };
        assert!(filter.allows(Some("jp"), Some("/media/日本語/anime/show.mkv"), &[]));
        assert!(!filter.allows(Some("jp"), Some("/media/日本語/anime/dubbed/show.mkv"), &[]));
        assert!(!filter.allows(Some("jp"), Some("/media/日本語/movies/show.mkv"), &[]));
        assert!(!filter.allows(Some("en"), Some("/media/日本語/anime/show.mkv"), &[]));
        let exclusion = MediaFilterConfig {
            excluded_folders: vec!["/English".into()],
            ..Default::default()
        };
        assert!(!exclusion.allows(None, None, &[]));
    }
}
