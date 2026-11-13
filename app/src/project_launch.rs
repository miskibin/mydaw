use std::path::{Component, Path, PathBuf};

/// The shell supplies an absolute path, while a terminal can supply a path
/// relative to the directory of the process that received it (`.\Song.wave`).
/// The result is normalized so the same file always reads as the same path:
/// the UI compares it with the open project's path to avoid reopening it.
pub fn project_path(arg: &str, cwd: Option<&Path>) -> Option<String> {
    let path = Path::new(arg);
    if arg.starts_with('-') || !path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("wave")) {
        return None;
    }
    let absolute = if path.is_absolute() { path.to_path_buf() } else { cwd?.join(path) };
    let absolute = normalize(&absolute);
    absolute.is_absolute().then(|| absolute.display().to_string())
}

/// Drops `.` and resolves `..` lexically, like Windows' `GetFullPathNameW`.
/// (`canonicalize` would also touch the disk and add the `\\?\` prefix on Windows.)
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                // `..` at the root stays at the root
                if out.file_name().is_some() {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_project_paths_against_the_launch_directory() {
        let cwd = std::env::temp_dir();
        let name = "Nowy utwór.WAVE";
        let absolute = cwd.join(name);
        let absolute = absolute.to_str().unwrap();
        assert_eq!(project_path(name, Some(&cwd)).as_deref(), Some(absolute));
        assert_eq!(project_path(absolute, None).as_deref(), Some(absolute));
    }

    #[test]
    fn normalizes_dot_segments_so_the_open_project_is_recognized() {
        let cwd = std::env::temp_dir().join("Music");
        let expected = cwd.join("Song.wave");
        let expected = expected.to_str();
        let dot = Path::new(".").join("Song.wave");
        assert_eq!(project_path(dot.to_str().unwrap(), Some(&cwd)).as_deref(), expected);
        let up = Path::new("..").join("Music").join(".").join("Song.wave");
        assert_eq!(project_path(up.to_str().unwrap(), Some(&cwd)).as_deref(), expected);
        let absolute = cwd.join("Sub").join("..").join("Song.wave");
        assert_eq!(project_path(absolute.to_str().unwrap(), None).as_deref(), expected);
    }

    #[test]
    fn parent_segments_stop_at_the_root() {
        let tmp = std::env::temp_dir();
        let root = tmp.ancestors().last().unwrap();
        let arg = root.join("..").join("..").join("Song.wave");
        let expected = root.join("Song.wave");
        assert_eq!(project_path(arg.to_str().unwrap(), None).as_deref(), expected.to_str());
    }

    #[cfg(windows)]
    #[test]
    fn normalizes_windows_explorer_style_paths() {
        let cwd = Path::new(r"C:\Music");
        assert_eq!(project_path(r".\Song.wave", Some(cwd)).as_deref(), Some(r"C:\Music\Song.wave"));
        assert_eq!(project_path(r"C:/Music/./Song.wave", None).as_deref(), Some(r"C:\Music\Song.wave"));
        assert_eq!(project_path(r"\\server\share\x\..\Song.wave", None).as_deref(), Some(r"\\server\share\Song.wave"));
    }

    #[test]
    fn ignores_flags_and_other_file_types() {
        let cwd = std::env::temp_dir();
        for arg in ["--cli", "--test.wave", "Song.wav", "Song.wave.bak", ""] {
            assert_eq!(project_path(arg, Some(&cwd)), None);
        }
        assert_eq!(project_path("Song.wave", None), None);
    }
}
