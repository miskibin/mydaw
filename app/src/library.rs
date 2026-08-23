//! The sample library: factory Packs plus user extra-search folders.
//!
//! FL Studio's Browser is a filesystem index, not a list of decoded files. This
//! module is that index: extra folders persist in `library.json`, a directory
//! is listed one level at a time, and nothing here touches the audio-source
//! registry. Decode happens when a file is previewed or dropped on the playlist.

use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use mydaw_plugin_sequencer::AUDIO_EXTENSIONS;

/// Id of the bundled factory pack root. Stable across launches; not in
/// `library.json`.
pub const PACKS_ROOT_ID: &str = "packs";

/// The id of the read-only Factory root: the sampled instruments' own files.
pub const FACTORY_ROOT_ID: &str = "factory";

/// How many children one listing will return. A folder of ten thousand takes
/// is a real thing; drawing them all is not.
pub const MAX_DIR_ENTRIES: usize = 500;

/// One extra-search folder the user added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryFolder {
    pub id: String,
    pub name: String,
    pub path: String,
}

/// Persisted extra folders. App-global, not per-project — the same idea as
/// FL Studio's File Settings extra search folders.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibrarySettings {
    #[serde(default)]
    pub folders: Vec<LibraryFolder>,
}

/// A root the browser draws: Packs, or one extra folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryRootDto {
    pub id: String,
    pub name: String,
    /// `"packs"` or `"extra"`.
    pub kind: String,
    pub path: String,
}

/// One child of a listed directory: a subfolder or an audio file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryEntryDto {
    pub name: String,
    /// `"dir"` or `"file"`.
    pub kind: String,
    /// Path relative to the root, using `/`, so the next listing can ask for
    /// it without the UI having to join paths.
    pub rel_path: String,
    /// Absolute path, for drag payloads and preview.
    pub path: String,
}

/// Why a library operation was refused.
#[derive(Debug, Error)]
pub enum LibraryError {
    #[error("that path is not a folder")]
    NotAFolder { path: PathBuf },
    #[error("the root of a drive is too broad to add as a sample folder")]
    DriveRoot { path: PathBuf },
    #[error("that folder is already in the library")]
    AlreadyAdded { path: PathBuf },
    #[error("there is no library folder '{0}'")]
    UnknownRoot(String),
    #[error("that path is not inside the library folder")]
    Escape,
    #[error("could not read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Load extra folders from `path`. A missing file is an empty library, not an
/// error — first launch has nothing to remember.
pub fn load_settings(path: &Path) -> LibrarySettings {
    let Ok(bytes) = fs::read(path) else {
        return LibrarySettings::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

/// Write extra folders to `path`, creating the parent directory if needed.
///
/// # Errors
/// [`LibraryError::Io`] when the parent cannot be created or the file cannot
/// be written.
pub fn save_settings(path: &Path, settings: &LibrarySettings) -> Result<(), LibraryError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| LibraryError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let json = serde_json::to_vec_pretty(settings).expect("LibrarySettings is always serialisable");
    fs::write(path, json).map_err(|source| LibraryError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Whether `path` is a drive root (`/`, `C:\`) and therefore too broad to
/// scan for samples.
pub fn is_drive_root(path: &Path) -> bool {
    let comps: Vec<Component<'_>> = path
        .components()
        .filter(|component| !matches!(component, Component::CurDir))
        .collect();
    matches!(
        comps.as_slice(),
        [Component::RootDir] | [Component::Prefix(_)] | [Component::Prefix(_), Component::RootDir]
    )
}

/// A stable id for an extra folder, derived from its path so adding the same
/// directory twice is detectable without comparing `PathBuf`s that may differ
/// only by a trailing slash.
pub fn folder_id(path: &Path) -> String {
    let key = normalize_key(path);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    format!("extra-{:016x}", hasher.finish())
}

/// Display name for a folder: its last component, or the path if it has none.
pub fn folder_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Add `path` as an extra-search folder.
///
/// # Errors
/// Not a directory, a drive root, or a folder that is already listed.
pub fn add_folder(
    settings: &mut LibrarySettings,
    path: PathBuf,
) -> Result<LibraryFolder, LibraryError> {
    if is_drive_root(&path) {
        return Err(LibraryError::DriveRoot { path });
    }
    if !path.is_dir() {
        return Err(LibraryError::NotAFolder { path });
    }
    let id = folder_id(&path);
    if settings.folders.iter().any(|folder| folder.id == id) {
        return Err(LibraryError::AlreadyAdded { path });
    }
    let folder = LibraryFolder {
        id,
        name: folder_name(&path),
        path: path.to_string_lossy().into_owned(),
    };
    settings.folders.push(folder.clone());
    Ok(folder)
}

/// Drop an extra folder from the library. Does not touch the disk.
///
/// # Errors
/// [`LibraryError::UnknownRoot`] when `id` is not an extra folder. Factory and
/// Packs cannot be removed.
pub fn remove_folder(settings: &mut LibrarySettings, id: &str) -> Result<(), LibraryError> {
    let before = settings.folders.len();
    settings.folders.retain(|folder| folder.id != id);
    if settings.folders.len() == before {
        return Err(LibraryError::UnknownRoot(id.to_owned()));
    }
    Ok(())
}

/// Roots the browser draws: Factory, Packs, then extra folders in add order.
///
/// The Factory root is the sampled instruments' own directory. It is listed
/// because a kit is two things at once -- an instrument you play from the
/// keyboard, and a folder of one-shots you drag onto a channel or chop -- and
/// without it the second is unreachable. It is read-only: nothing in the browser
/// writes, and [`remove_folder`] refuses it along with Packs.
pub fn roots(
    factory_dir: &Path,
    packs_dir: &Path,
    settings: &LibrarySettings,
) -> Vec<LibraryRootDto> {
    let mut out = vec![
        LibraryRootDto {
            id: FACTORY_ROOT_ID.to_owned(),
            name: "Factory".to_owned(),
            kind: "factory".to_owned(),
            path: factory_dir.to_string_lossy().into_owned(),
        },
        LibraryRootDto {
            id: PACKS_ROOT_ID.to_owned(),
            name: "Packs".to_owned(),
            kind: "packs".to_owned(),
            path: packs_dir.to_string_lossy().into_owned(),
        },
    ];
    for folder in &settings.folders {
        out.push(LibraryRootDto {
            id: folder.id.clone(),
            name: folder.name.clone(),
            kind: "extra".to_owned(),
            path: folder.path.clone(),
        });
    }
    out
}

/// Resolve `rel` under `root`, refusing `..` and absolute fragments.
///
/// # Errors
/// [`LibraryError::Escape`] when `rel` would leave `root`.
pub fn join_rel(root: &Path, rel: &str) -> Result<PathBuf, LibraryError> {
    if rel.is_empty() {
        return Ok(root.to_path_buf());
    }
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        return Err(LibraryError::Escape);
    }
    for component in rel_path.components() {
        match component {
            Component::Normal(_) => {}
            _ => return Err(LibraryError::Escape),
        }
    }
    Ok(root.join(rel_path))
}

/// One level of `root` / `rel`: directories and audio files, dirs first,
/// case-insensitive names, hidden names skipped, capped at [`MAX_DIR_ENTRIES`].
///
/// A missing directory returns an empty list rather than an error — a factory
/// pack that was not bundled, or an extra folder the user deleted, should
/// draw as empty, not as a status-bar failure on every expand.
///
/// # Errors
/// [`LibraryError::Escape`] for a `rel` that leaves the root, or
/// [`LibraryError::Io`] when the directory exists but cannot be read.
pub fn list_dir(root: &Path, rel: &str) -> Result<Vec<LibraryEntryDto>, LibraryError> {
    let dir = join_rel(root, rel)?;
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(LibraryError::Io { path: dir, source }),
    };

    let mut dirs: Vec<LibraryEntryDto> = Vec::new();
    let mut files: Vec<LibraryEntryDto> = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let child_rel = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        let file_type = entry.file_type().ok();
        let is_dir = file_type
            .map(|kind| kind.is_dir())
            .unwrap_or_else(|| path.is_dir());
        if is_dir {
            dirs.push(LibraryEntryDto {
                name,
                kind: "dir".to_owned(),
                rel_path: child_rel.replace('\\', "/"),
                path: path.to_string_lossy().into_owned(),
            });
        } else if is_audio_path(&path) {
            files.push(LibraryEntryDto {
                name,
                kind: "file".to_owned(),
                rel_path: child_rel.replace('\\', "/"),
                path: path.to_string_lossy().into_owned(),
            });
        }
        if dirs.len() + files.len() >= MAX_DIR_ENTRIES {
            break;
        }
    }

    dirs.sort_by(|a, b| {
        a.name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
    });
    files.sort_by(|a, b| {
        a.name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
    });
    dirs.extend(files);
    Ok(dirs)
}

/// Where the bundled factory pack lives.
///
/// Production: Tauri's resource directory. Dev: `assets/packs` next to the
/// workspace, so `cargo run` without a bundle still finds the kit.
pub fn resolve_packs_dir(resource_dir: Option<&Path>) -> PathBuf {
    if let Some(resource) = resource_dir {
        for candidate in [
            resource.join("packs"),
            resource.join("assets").join("packs"),
        ] {
            if candidate.is_dir() {
                return candidate;
            }
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("assets")
        .join("packs")
}

fn is_audio_path(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
        return false;
    };
    AUDIO_EXTENSIONS
        .iter()
        .any(|known| known.eq_ignore_ascii_case(ext))
}

fn normalize_key(path: &Path) -> String {
    let mut key = path.to_string_lossy().replace('\\', "/");
    while key.ends_with('/') && key.len() > 1 {
        key.pop();
    }
    key.to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let base = std::env::temp_dir().join(format!(
                "mydaw-library-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&base);
            fs::create_dir_all(&base).expect("temp dir");
            Self(base)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn touch(dir: &Path, name: &str) {
        fs::write(dir.join(name), b"x").unwrap();
    }

    #[test]
    fn a_unix_root_and_a_windows_drive_are_drive_roots() {
        assert!(is_drive_root(Path::new("/")));
        #[cfg(windows)]
        {
            assert!(is_drive_root(Path::new("C:\\")));
            assert!(is_drive_root(Path::new("C:/")));
            assert!(is_drive_root(Path::new("D:")));
            assert!(!is_drive_root(Path::new("C:\\Samples")));
            assert!(!is_drive_root(Path::new("D:\\music\\kits")));
        }
        assert!(!is_drive_root(Path::new("/samples")));
    }

    #[test]
    fn adding_a_folder_persists_round_trip() {
        let dir = TempDir::new("add");
        let samples = dir.path().join("samples");
        fs::create_dir(&samples).unwrap();
        let settings_path = dir.path().join("library.json");

        let mut settings = load_settings(&settings_path);
        let folder = add_folder(&mut settings, samples.clone()).expect("adds");
        assert_eq!(folder.name, "samples");
        assert!(folder.id.starts_with("extra-"));
        save_settings(&settings_path, &settings).expect("saves");

        let loaded = load_settings(&settings_path);
        assert_eq!(loaded.folders.len(), 1);
        assert_eq!(loaded.folders[0].path, samples.to_string_lossy());
    }

    #[test]
    fn adding_the_same_folder_twice_is_refused() {
        let dir = TempDir::new("dup");
        let samples = dir.path().join("kit");
        fs::create_dir(&samples).unwrap();
        let mut settings = LibrarySettings::default();
        add_folder(&mut settings, samples.clone()).unwrap();
        let err = add_folder(&mut settings, samples).unwrap_err();
        assert!(matches!(err, LibraryError::AlreadyAdded { .. }));
    }

    #[test]
    fn a_missing_settings_file_is_an_empty_library() {
        let settings = load_settings(Path::new("no-such-library-9f2a.json"));
        assert!(settings.folders.is_empty());
    }

    #[test]
    fn listing_skips_hidden_and_non_audio_and_puts_dirs_first() {
        let dir = TempDir::new("list");
        let root = dir.path();
        fs::create_dir(root.join("Drums")).unwrap();
        fs::create_dir(root.join("loops")).unwrap();
        touch(root, "kick.wav");
        touch(root, "snare.FLAC");
        touch(root, "notes.txt");
        touch(root, ".secret.wav");
        touch(root, "hat.ogg");
        touch(root, "loop.mp3");

        let listed = list_dir(root, "").expect("lists");
        let names: Vec<&str> = listed.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Drums",
                "loops",
                "hat.ogg",
                "kick.wav",
                "loop.mp3",
                "snare.FLAC"
            ]
        );
        assert_eq!(listed[0].kind, "dir");
        assert_eq!(listed[2].kind, "file");
        assert_eq!(listed[2].rel_path, "hat.ogg");
    }

    #[test]
    fn listing_a_nested_rel_path_keeps_the_relative_prefix() {
        let dir = TempDir::new("nested");
        let drums = dir.path().join("Drums");
        fs::create_dir(&drums).unwrap();
        touch(&drums, "kick.wav");

        let listed = list_dir(dir.path(), "Drums").expect("lists");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].rel_path, "Drums/kick.wav");
        assert_eq!(listed[0].name, "kick.wav");
    }

    #[test]
    fn a_parent_rel_path_is_refused() {
        let err = join_rel(Path::new("/samples"), "../etc").unwrap_err();
        assert!(matches!(err, LibraryError::Escape));
        let err = join_rel(Path::new("/samples"), "/absolute").unwrap_err();
        assert!(matches!(err, LibraryError::Escape));
    }

    #[test]
    fn a_missing_directory_lists_as_empty() {
        let listed = list_dir(Path::new("no-such-packs-9f2a"), "").expect("empty");
        assert!(listed.is_empty());
    }

    #[test]
    fn resolve_packs_dir_falls_back_to_workspace_assets() {
        let path = resolve_packs_dir(None);
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("packs")
        );
        assert!(path.ends_with(Path::new("assets").join("packs")));
    }

    #[test]
    fn resolve_packs_dir_prefers_an_existing_resource_subdir() {
        let dir = TempDir::new("resource");
        let packs = dir.path().join("packs");
        fs::create_dir(&packs).unwrap();
        assert_eq!(resolve_packs_dir(Some(dir.path())), packs);
    }

    #[test]
    fn resolve_packs_dir_skips_a_resource_dir_that_has_no_packs() {
        let dir = TempDir::new("empty-resource");
        let fallback = resolve_packs_dir(None);
        assert_eq!(resolve_packs_dir(Some(dir.path())), fallback);
    }

    #[test]
    fn the_built_in_roots_lead_and_cannot_be_removed() {
        let dir = TempDir::new("roots");
        let extra = dir.path().join("mine");
        fs::create_dir(&extra).unwrap();
        let mut settings = LibrarySettings::default();
        add_folder(&mut settings, extra).unwrap();

        let listed = roots(
            Path::new("/app/instruments"),
            Path::new("/app/packs"),
            &settings,
        );
        assert_eq!(listed[0].id, FACTORY_ROOT_ID);
        assert_eq!(listed[0].kind, "factory");
        assert_eq!(listed[0].name, "Factory");
        assert_eq!(listed[1].id, PACKS_ROOT_ID);
        assert_eq!(listed[1].kind, "packs");
        assert_eq!(listed[2].kind, "extra");

        // Neither built-in root is a folder the user added, so neither can be
        // taken away.
        for built_in in [FACTORY_ROOT_ID, PACKS_ROOT_ID] {
            let err = remove_folder(&mut settings, built_in).unwrap_err();
            assert!(matches!(err, LibraryError::UnknownRoot(_)), "{built_in}");
        }
        remove_folder(&mut settings, &listed[2].id).expect("removes extra");
        assert!(settings.folders.is_empty());
    }
}
