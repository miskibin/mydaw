use std::path::Path;

/// The shell supplies an absolute path, while a terminal can supply a path
/// relative to the directory of the process that received it.
pub fn project_path(arg: &str, cwd: Option<&Path>) -> Option<String> {
    let path = Path::new(arg);
    if arg.starts_with('-') || !path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("wave")) {
        return None;
    }
    if path.is_absolute() {
        return Some(arg.to_owned());
    }
    let absolute = cwd?.join(path);
    absolute.is_absolute().then(|| absolute.display().to_string())
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
    fn ignores_flags_and_other_file_types() {
        let cwd = std::env::temp_dir();
        for arg in ["--cli", "--test.wave", "Song.wav", "Song.wave.bak", ""] {
            assert_eq!(project_path(arg, Some(&cwd)), None);
        }
        assert_eq!(project_path("Song.wave", None), None);
    }
}
