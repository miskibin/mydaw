//! The instrument manifest: what a directory of samples *means*.
//!
//! ```text
//!   assets/instruments/salamander-piano/
//!       manifest.json      <- this
//!       A0v4.flac  C1v4.flac  D#1v4.flac  ...
//! ```
//!
//! # Why not SFZ
//! SFZ is the obvious answer and the wrong one. It is a macro language with
//! `#define`, `#include`, opcode inheritance across `<global>`/`<master>`/
//! `<group>`/`<region>`, curves, CC bindings and two competing dialects; a
//! parser for the whole of it is a project, and a parser for *some* of it is a
//! thing that silently plays the wrong sample when it meets an opcode it
//! skipped. Neither is what this step is for.
//!
//! So the sampler reads a format it fully implements: a flat list of sample
//! entries, each naming a file, the key it was recorded at, the key and
//! velocity span it covers, and an optional detune. Everything an SFZ says that
//! this cannot express is resolved **once, offline**, by the generator in
//! `scripts/` -- which does understand the small subset of SFZ that Salamander
//! actually uses -- and the result is pinned in `assets-manifest.json`. A
//! reader of this file never has to reason about what a `<group>` inherited.
//!
//! # Robustness rules
//! The three that matter, because a manifest is data that arrived from a
//! download rather than from a compiler:
//!
//! 1. **Unknown fields are ignored.** `assets-manifest.json` carries download
//!    URLs and checksums that the runtime has no use for, and is copied to
//!    `manifest.json` verbatim rather than being filtered -- one fewer
//!    transformation to get wrong. A future field must not break an old build.
//! 2. **Every error is typed and names the file.** No `unwrap`, no `Option`
//!    that means five different things.
//! 3. **A missing sample file is a skipped entry and a logged warning, not a
//!    failed load.** An instrument that is 89 of its 90 samples is worth far
//!    more than no instrument at all -- and a partial download is exactly the
//!    state the asset fetcher can leave behind.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Release time used when a manifest does not state one.
///
/// 300 ms is the damper of an acoustic piano: long enough that a released key
/// does not click, short enough that a fast passage does not turn to mud.
pub const DEFAULT_RELEASE_MS: f32 = 300.0;

/// Lowest MIDI velocity that is a note. 0 is a note-on at zero velocity, which
/// CLAP allows and which selects the softest layer.
pub const MIN_VELOCITY: u8 = 1;
/// Highest MIDI velocity and highest MIDI key.
pub const MAX_VELOCITY: u8 = 127;
/// Highest MIDI key number.
pub const MAX_KEY: u8 = 127;

/// The file a loaded instrument is read from.
pub const MANIFEST_FILE_NAME: &str = "manifest.json";

/// One sample and the region of the keyboard it is responsible for.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct SampleEntry {
    /// File name, relative to the manifest's own directory.
    ///
    /// A *name*, not a path: see [`SampleEntry::check`]. A manifest that could
    /// name `../../../etc/passwd` would be a downloaded file choosing what this
    /// process reads.
    pub file: String,
    /// The key this sample was recorded at. Playing `root_key` plays it at its
    /// recorded pitch.
    pub root_key: u8,
    /// Lowest key this entry covers.
    pub key_lo: u8,
    /// Highest key this entry covers.
    pub key_hi: u8,
    /// Lowest velocity this entry covers. Defaults to the whole range, so a
    /// single-layer instrument need not mention velocity at all.
    #[serde(default = "default_vel_lo")]
    pub vel_lo: u8,
    /// Highest velocity this entry covers.
    #[serde(default = "default_vel_hi")]
    pub vel_hi: u8,
    /// Fine detune in cents, applied on top of the key offset. Salamander's
    /// "retuned" variant uses this; the natural variant is all zeros.
    #[serde(default)]
    pub tune_cents: f32,
}

const fn default_vel_lo() -> u8 {
    MIN_VELOCITY
}

const fn default_vel_hi() -> u8 {
    MAX_VELOCITY
}

impl SampleEntry {
    /// Whether this entry answers for `key` at `velocity`.
    pub const fn covers(&self, key: u8, velocity: u8) -> bool {
        key >= self.key_lo
            && key <= self.key_hi
            && velocity >= self.vel_lo
            && velocity <= self.vel_hi
    }

    /// Reject an entry that cannot mean anything.
    ///
    /// # Errors
    /// A sentence naming what is wrong, for an inverted range, a key or
    /// velocity outside MIDI's, or a `file` that is anything other than a bare
    /// name in the manifest's own directory.
    pub fn check(&self) -> Result<(), String> {
        if self.file.trim().is_empty() {
            return Err("'file' is empty".to_owned());
        }
        // A downloaded manifest must not be able to point the loader anywhere
        // except next to itself. `Path::components` is what makes this a real
        // check rather than a scan for "..": it normalises separators, so
        // `..\\x`, `../x` and `/x` all fail the same way on every platform.
        let path = Path::new(&self.file);
        let mut components = path.components();
        let only = components.next();
        if components.next().is_some() || !matches!(only, Some(std::path::Component::Normal(_))) {
            return Err(format!(
                "'file' must be a plain file name in the manifest's directory, not '{}'",
                self.file
            ));
        }
        if self.key_lo > self.key_hi {
            return Err(format!(
                "key range is inverted: key_lo {} is above key_hi {}",
                self.key_lo, self.key_hi
            ));
        }
        if self.vel_lo > self.vel_hi {
            return Err(format!(
                "velocity range is inverted: vel_lo {} is above vel_hi {}",
                self.vel_lo, self.vel_hi
            ));
        }
        for (label, value) in [
            ("root_key", self.root_key),
            ("key_lo", self.key_lo),
            ("key_hi", self.key_hi),
        ] {
            if value > MAX_KEY {
                return Err(format!("{label} {value} is not a MIDI key (0..=127)"));
            }
        }
        if self.vel_hi > MAX_VELOCITY {
            return Err(format!(
                "vel_hi {} is not a MIDI velocity (0..=127)",
                self.vel_hi
            ));
        }
        if !self.tune_cents.is_finite() {
            return Err("tune_cents is not a number".to_owned());
        }
        Ok(())
    }
}

/// One instrument: a name, a release time, and the samples that make it.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct InstrumentManifest {
    /// Stable id, and the directory name this instrument lives in. Appears in
    /// project files as part of a source id, so renaming one is a breaking
    /// change.
    pub id: String,
    /// Name for the source picker.
    pub name: String,
    /// One line for a tooltip. Optional; the loader substitutes a description
    /// derived from the sample count when it is empty.
    #[serde(default)]
    pub description: String,
    /// Note-off fade, in milliseconds.
    #[serde(default = "default_release_ms")]
    pub release_ms: f32,
    /// Hard cap on how much of each sample is kept in memory, in seconds.
    ///
    /// The memory budget, expressed as data rather than as a constant in the
    /// loader: Salamander's samples run to 16 seconds each and 90 of those
    /// would be half a gigabyte. `None` keeps whole files, which is right for a
    /// small instrument and wrong for a piano.
    #[serde(default)]
    pub max_seconds: Option<f32>,
    /// Fade applied to the last of a truncated sample, in seconds.
    ///
    /// Without it, truncation is a step discontinuity -- an audible click at
    /// exactly [`max_seconds`](Self::max_seconds) into every note. Ignored when
    /// nothing was truncated.
    #[serde(default)]
    pub fade_out_seconds: Option<f32>,
    /// Attribution line the licence requires be shown. Carried in the manifest
    /// rather than in the code so that it travels with the samples it is about.
    #[serde(default)]
    pub attribution: String,
    pub samples: Vec<SampleEntry>,
}

const fn default_release_ms() -> f32 {
    DEFAULT_RELEASE_MS
}

/// Why a manifest could not be read.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("could not read the manifest at {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("the manifest at {path} is not valid JSON: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    /// Names the entry by index, because a sample list has no other handle and
    /// "one of your ninety samples is wrong" is not a usable error.
    #[error("the manifest at {path}: sample {index} ('{file}') {reason}")]
    Sample {
        path: PathBuf,
        index: usize,
        file: String,
        reason: String,
    },
    #[error("the manifest at {path}: {reason}")]
    Invalid { path: PathBuf, reason: String },
}

impl InstrumentManifest {
    /// Parse and validate a manifest from JSON bytes attributed to `path`.
    ///
    /// `path` is used only for the error messages; nothing is read here. That
    /// split is what lets the tests exercise every failure without a temp
    /// directory, and what lets the downloader validate a manifest it has in
    /// memory before it writes it anywhere.
    ///
    /// # Errors
    /// [`ManifestError::Json`] for malformed JSON or a missing required field,
    /// [`ManifestError::Invalid`] for an empty id or an empty sample list, and
    /// [`ManifestError::Sample`] for an entry that cannot mean anything --
    /// naming the index and the file, since a piano has ninety of them.
    pub fn parse(bytes: &[u8], path: &Path) -> Result<Self, ManifestError> {
        let manifest: Self =
            serde_json::from_slice(bytes).map_err(|source| ManifestError::Json {
                path: path.to_path_buf(),
                source,
            })?;
        manifest.check(path)?;
        Ok(manifest)
    }

    /// Read and parse the manifest at `path`.
    ///
    /// # Errors
    /// [`ManifestError::Read`] when the file cannot be read, plus everything
    /// [`parse`](Self::parse) reports.
    pub fn read(path: &Path) -> Result<Self, ManifestError> {
        let bytes = std::fs::read(path).map_err(|source| ManifestError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&bytes, path)
    }

    fn check(&self, path: &Path) -> Result<(), ManifestError> {
        let invalid = |reason: String| ManifestError::Invalid {
            path: path.to_path_buf(),
            reason,
        };
        if self.id.trim().is_empty() {
            return Err(invalid("'id' is empty".to_owned()));
        }
        if self.name.trim().is_empty() {
            return Err(invalid("'name' is empty".to_owned()));
        }
        if self.samples.is_empty() {
            return Err(invalid(
                "'samples' is empty; an instrument with no samples is not one".to_owned(),
            ));
        }
        if !self.release_ms.is_finite() || self.release_ms < 0.0 {
            return Err(invalid(format!(
                "release_ms {} is not a length of time",
                self.release_ms
            )));
        }
        if self
            .max_seconds
            .is_some_and(|seconds| !seconds.is_finite() || seconds <= 0.0)
        {
            return Err(invalid("max_seconds must be a positive number".to_owned()));
        }
        if self
            .fade_out_seconds
            .is_some_and(|seconds| !seconds.is_finite() || seconds < 0.0)
        {
            return Err(invalid("fade_out_seconds must not be negative".to_owned()));
        }
        for (index, entry) in self.samples.iter().enumerate() {
            entry.check().map_err(|reason| ManifestError::Sample {
                path: path.to_path_buf(),
                index,
                file: entry.file.clone(),
                reason,
            })?;
        }
        Ok(())
    }

    /// The entry that answers for `key` at `velocity`, if any.
    ///
    /// First match wins, which makes the manifest's order meaningful and keeps
    /// overlapping regions a documented precedence rather than a coin toss.
    pub fn entry_for(&self, key: u8, velocity: u8) -> Option<&SampleEntry> {
        self.samples
            .iter()
            .find(|entry| entry.covers(key, velocity))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path() -> PathBuf {
        PathBuf::from("assets/instruments/test/manifest.json")
    }

    fn minimal() -> serde_json::Value {
        serde_json::json!({
            "id": "test",
            "name": "Test",
            "samples": [
                { "file": "a.wav", "root_key": 60, "key_lo": 59, "key_hi": 61 }
            ]
        })
    }

    fn parse(value: &serde_json::Value) -> Result<InstrumentManifest, ManifestError> {
        InstrumentManifest::parse(value.to_string().as_bytes(), &path())
    }

    #[test]
    fn a_minimal_manifest_parses_and_defaults_the_rest() {
        let manifest = parse(&minimal()).expect("a minimal manifest is valid");
        assert_eq!(manifest.id, "test");
        assert_eq!(manifest.name, "Test");
        assert_eq!(manifest.release_ms, DEFAULT_RELEASE_MS);
        assert_eq!(manifest.max_seconds, None);
        assert_eq!(manifest.fade_out_seconds, None);
        assert!(manifest.attribution.is_empty());

        let entry = &manifest.samples[0];
        assert_eq!(entry.root_key, 60);
        assert_eq!(
            (entry.vel_lo, entry.vel_hi),
            (MIN_VELOCITY, MAX_VELOCITY),
            "an entry that does not mention velocity covers all of it"
        );
        assert_eq!(entry.tune_cents, 0.0);
    }

    /// The rule that lets `assets-manifest.json` be copied to `manifest.json`
    /// verbatim, carrying download URLs and checksums the runtime ignores.
    #[test]
    fn unknown_fields_are_ignored_rather_than_refused() {
        let mut value = minimal();
        value["schema"] = serde_json::json!(1);
        value["source"] = serde_json::json!({ "repo": "https://example.invalid" });
        value["samples"][0]["url"] = serde_json::json!("https://example.invalid/a.wav");
        value["samples"][0]["sha256"] = serde_json::json!("00");
        value["samples"][0]["bytes"] = serde_json::json!(1234);

        let manifest = parse(&value).expect("unknown fields must not break a load");
        assert_eq!(manifest.samples.len(), 1);
        assert_eq!(manifest.samples[0].file, "a.wav");
    }

    #[test]
    fn malformed_json_names_the_file_it_came_from() {
        let err = InstrumentManifest::parse(b"{ not json", &path()).unwrap_err();
        assert!(matches!(err, ManifestError::Json { .. }));
        assert!(err.to_string().contains("manifest.json"), "{err}");
    }

    #[test]
    fn a_missing_required_field_is_a_json_error_not_a_panic() {
        let err = parse(&serde_json::json!({ "name": "No Id", "samples": [] })).unwrap_err();
        assert!(matches!(err, ManifestError::Json { .. }), "{err}");
    }

    #[test]
    fn an_instrument_with_no_samples_is_refused() {
        let mut value = minimal();
        value["samples"] = serde_json::json!([]);
        let err = parse(&value).unwrap_err();
        assert!(matches!(err, ManifestError::Invalid { .. }));
        assert!(err.to_string().contains("no samples"), "{err}");
    }

    #[test]
    fn an_empty_id_or_name_is_refused() {
        for field in ["id", "name"] {
            let mut value = minimal();
            value[field] = serde_json::json!("   ");
            let err = parse(&value).unwrap_err();
            assert!(
                matches!(err, ManifestError::Invalid { .. }),
                "{field}: {err}"
            );
        }
    }

    /// A manifest arrives from a download. It must not be able to name a file
    /// outside its own directory.
    #[test]
    fn a_file_that_escapes_the_manifest_directory_is_refused() {
        for escape in [
            "../secrets.wav",
            "..\\secrets.wav",
            "sub/a.wav",
            "sub\\a.wav",
            "/etc/passwd",
            "C:\\Windows\\win.ini",
            "..",
            "",
        ] {
            let mut value = minimal();
            value["samples"][0]["file"] = serde_json::json!(escape);
            let Err(err) = parse(&value) else {
                panic!("'{escape}' must be refused");
            };
            assert!(
                matches!(err, ManifestError::Sample { .. }),
                "'{escape}' gave {err}"
            );
        }
    }

    #[test]
    fn an_inverted_or_out_of_range_span_names_the_entry_that_has_it() {
        let cases = [
            (
                serde_json::json!({ "key_lo": 70, "key_hi": 60 }),
                "inverted",
            ),
            (
                serde_json::json!({ "vel_lo": 90, "vel_hi": 10 }),
                "inverted",
            ),
            (serde_json::json!({ "root_key": 200 }), "MIDI key"),
            (serde_json::json!({ "vel_hi": 200 }), "MIDI velocity"),
        ];
        for (patch, needle) in cases {
            let mut value = minimal();
            // Two entries, so the index in the message has to be right.
            value["samples"] = serde_json::json!([
                { "file": "a.wav", "root_key": 60, "key_lo": 59, "key_hi": 61 },
                { "file": "b.wav", "root_key": 72, "key_lo": 71, "key_hi": 73 },
            ]);
            for (key, patched) in patch.as_object().unwrap() {
                value["samples"][1][key] = patched.clone();
            }
            let err = parse(&value).unwrap_err();
            let ManifestError::Sample {
                index, ref file, ..
            } = err
            else {
                panic!("expected a sample error for {needle}, got {err}");
            };
            assert_eq!(index, 1, "{err}");
            assert_eq!(file, "b.wav", "{err}");
            assert!(err.to_string().contains(needle), "{err}");
        }
    }

    #[test]
    fn a_nonsense_release_or_budget_is_refused() {
        for (field, value) in [
            ("release_ms", serde_json::json!(-5.0)),
            ("max_seconds", serde_json::json!(0.0)),
            ("max_seconds", serde_json::json!(-1.0)),
            ("fade_out_seconds", serde_json::json!(-1.0)),
        ] {
            let mut manifest = minimal();
            manifest[field] = value.clone();
            let err = parse(&manifest).unwrap_err();
            assert!(
                matches!(err, ManifestError::Invalid { .. }),
                "{field}={value}: {err}"
            );
        }
    }

    #[test]
    fn lookup_takes_the_first_entry_that_covers_the_note() {
        let value = serde_json::json!({
            "id": "layers",
            "name": "Layers",
            "samples": [
                { "file": "soft.wav", "root_key": 60, "key_lo": 0, "key_hi": 127,
                  "vel_lo": 1, "vel_hi": 63 },
                { "file": "loud.wav", "root_key": 60, "key_lo": 0, "key_hi": 127,
                  "vel_lo": 64, "vel_hi": 127 },
            ]
        });
        let manifest = parse(&value).unwrap();
        assert_eq!(manifest.entry_for(60, 1).unwrap().file, "soft.wav");
        assert_eq!(manifest.entry_for(60, 63).unwrap().file, "soft.wav");
        assert_eq!(manifest.entry_for(60, 64).unwrap().file, "loud.wav");
        assert_eq!(manifest.entry_for(60, 127).unwrap().file, "loud.wav");
        // Velocity 0 is a legal CLAP note-on and is covered by nothing here;
        // the bank widens the softest layer to catch it (see `bank`).
        assert!(manifest.entry_for(60, 0).is_none());
    }

    #[test]
    fn the_manifest_round_trips_through_json() {
        let manifest = parse(&minimal()).unwrap();
        let json = serde_json::to_string(&manifest).unwrap();
        let again = InstrumentManifest::parse(json.as_bytes(), &path()).unwrap();
        assert_eq!(manifest, again);
    }
}
