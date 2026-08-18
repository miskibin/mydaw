//! Failures of the file-writing side of a capture.
//!
//! Every variant names the path it was working on: a capture harness that says
//! only "permission denied" costs more time than it saves.

use std::path::PathBuf;

/// Something went wrong writing a capture artefact.
#[derive(Debug, thiserror::Error)]
pub enum CaptureIoError {
    /// Filesystem failure (creating the directory, opening the file, flushing).
    #[error("could not write {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// `hound` refused the WAV, e.g. a sample rate that does not fit a `u32`.
    #[error("could not write the WAV {path}: {source}")]
    Wav {
        path: PathBuf,
        #[source]
        source: hound::Error,
    },

    /// The PNG encoder refused the image.
    #[error("could not write the PNG {path}: {source}")]
    Png {
        path: PathBuf,
        #[source]
        source: png::EncodingError,
    },

    /// The axis sidecar could not be serialised.
    #[error("could not write the axis sidecar {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

impl CaptureIoError {
    /// The path the failure happened on.
    pub fn path(&self) -> &PathBuf {
        match self {
            Self::Io { path, .. }
            | Self::Wav { path, .. }
            | Self::Png { path, .. }
            | Self::Json { path, .. } => path,
        }
    }
}

/// Create `path`'s parent directory, mapping the error to [`CaptureIoError`].
pub fn ensure_parent_dir(path: &std::path::Path) -> Result<(), CaptureIoError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() || parent.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(parent).map_err(|source| CaptureIoError::Io {
        path: parent.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_reports_the_path_it_failed_on() {
        let error = CaptureIoError::Io {
            path: PathBuf::from("a/b.wav"),
            source: std::io::Error::other("nope"),
        };
        assert_eq!(error.path(), &PathBuf::from("a/b.wav"));
        assert!(error.to_string().contains("a/b.wav"), "{error}");
    }

    #[test]
    fn a_path_without_a_parent_needs_no_directory() {
        assert!(ensure_parent_dir(std::path::Path::new("x.wav")).is_ok());
    }
}
