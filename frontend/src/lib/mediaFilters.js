export function mediaFilterMode(filter) {
  if (filter.mode === 'include' || filter.mode === 'exclude') return filter.mode;
  return filter.included_libraries.length || filter.included_folders.some((folder) => folder.trim())
    ? 'include'
    : 'exclude';
}

export function mediaFilterSelection(filter) {
  return mediaFilterMode(filter) === 'include'
    ? { libraries: filter.included_libraries, folders: filter.included_folders }
    : { libraries: filter.excluded_libraries, folders: filter.excluded_folders };
}

// One mode applies to both libraries and folders. Never save both lists.
export function updateMediaFilter(filter, mode, selection = mediaFilterSelection(filter)) {
  return {
    ...filter,
    mode,
    included_libraries: mode === 'include' ? selection.libraries : [],
    excluded_libraries: mode === 'exclude' ? selection.libraries : [],
    included_folders: mode === 'include' ? selection.folders : [],
    excluded_folders: mode === 'exclude' ? selection.folders : [],
  };
}
