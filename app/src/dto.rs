//! Wire shapes owned by the shell.
//!
//! The engine crates define the canonical Rust types; this module exists where
//! the TypeScript contract in `ui/src/api/types.ts` disagrees with serde's
//! default rendering of them, or where a type needs a shape the engine has no
//! business knowing about. [`MeterFrame`] serialises its fields as
//! `peak_l`/`sample_pos` while the UI reads `peakL`/`samplePos`; a
//! [`Pattern`] is a fixed-size array the webview would rather see as a list,
//! with a row label attached. Pushing either convention down into crates that
//! are deliberately Tauri-free and webview-agnostic would be the wrong trade, so
//! the translation lives here, at the boundary that actually cares.

use mydaw_ipc_bridge::{MeterFrame, TrackMeter};
use mydaw_plugin_host_clap::ClapPluginInfo;
use mydaw_plugin_host_vst3::Vst3PluginInfo;
use mydaw_plugin_sequencer::{
    row_label, MelodicNote, Pattern, PatternError, PatternRow, Step, DEFAULT_STEP_VELOCITY,
    MAX_STEPS,
};
use serde::{Deserialize, Serialize};

/// One track's level, as the mixer panel draws it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackMeterDto {
    pub peak_l: f32,
    pub peak_r: f32,
}

impl From<&TrackMeter> for TrackMeterDto {
    fn from(meter: &TrackMeter) -> Self {
        Self {
            peak_l: meter.peak_l,
            peak_r: meter.peak_r,
        }
    }
}

/// [`MeterFrame`] in the shape the webview expects (camelCase).
///
/// The master fields are flat and `Copy`; the per-track levels become a `Vec` of
/// exactly the tracks that exist, because a JSON array of sixteen entries with
/// fourteen of them zero is fourteen entries of nothing crossing the boundary
/// thirty times a second. The RT side stays a fixed array -- see
/// [`MeterFrame::tracks`] -- and this is the one place the two shapes differ.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeterFrameDto {
    pub peak_l: f32,
    pub peak_r: f32,
    pub rms_l: f32,
    pub rms_r: f32,
    /// Transport position in frames at the end of the measured block.
    pub sample_pos: u64,
    pub playing: bool,
    /// Post-strip, pre-master peak of every track, in track order.
    pub tracks: Vec<TrackMeterDto>,
    /// When the pump handed this frame to the webview, on the bench clock
    /// ([`crate::ipc_bench::epoch_ms`]).
    ///
    /// `None` -- and absent from the JSON entirely -- unless the IPC bench asked
    /// the pump to stamp frames, which is the only thing that reads it. That
    /// keeps the normal meter frame byte-for-byte what it was before spike 4.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pump_ms: Option<f64>,
}

impl From<MeterFrame> for MeterFrameDto {
    fn from(frame: MeterFrame) -> Self {
        Self {
            peak_l: frame.peak_l,
            peak_r: frame.peak_r,
            rms_l: frame.rms_l,
            rms_r: frame.rms_r,
            sample_pos: frame.sample_pos,
            playing: frame.playing,
            tracks: frame
                .track_meters()
                .iter()
                .map(TrackMeterDto::from)
                .collect(),
            pump_ms: None,
        }
    }
}

/// One row of the CLAP browser panel.
///
/// `path` is a `String`, not a `PathBuf`: serde renders a `PathBuf` as whatever
/// the OS encoding happens to be, and a Windows path with a non-UTF-8 byte would
/// fail to serialise and take the whole command with it. Lossy conversion here
/// means a mangled character in a tooltip instead of an empty panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClapPluginInfoDto {
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub version: String,
    pub path: String,
    /// What the plugin advertises itself as: `instrument`, `audio-effect`,
    /// `note-effect` or `other`. The browser badges it, and the pickers filter
    /// on it -- an effect cannot head a track and an instrument cannot process
    /// the master bus.
    ///
    /// Read from the descriptor's `features`, which is free; the authoritative
    /// check is the plugin's port layout, and it happens when an instance is
    /// created. See
    /// [`ClapPluginKind`](mydaw_plugin_host_clap::ClapPluginKind).
    pub kind: String,
    /// The declared feature strings, verbatim. Shown as a tooltip, and the raw
    /// material for a smarter filter later.
    pub features: Vec<String>,
}

impl From<&ClapPluginInfo> for ClapPluginInfoDto {
    fn from(info: &ClapPluginInfo) -> Self {
        Self {
            id: info.id.clone(),
            name: info.name.clone(),
            vendor: info.vendor.clone(),
            version: info.version.clone(),
            path: info.path.to_string_lossy().into_owned(),
            kind: info.kind().id().to_owned(),
            features: info.features.clone(),
        }
    }
}

/// One row of the VST3 browser panel.
///
/// The counterpart of [`ClapPluginInfoDto`], and deliberately close to it so one
/// UI list can show both with a format badge. The differences are the format's:
/// `id` is a 32-digit class id rather than a reverse-DNS string, and
/// `subcategories` is one `|`-separated string rather than a list -- carried
/// verbatim, for the same reason CLAP's `features` are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Vst3PluginInfoDto {
    /// Class id as 32 uppercase hex digits. Unique within a module, and the
    /// half of a locator a project file stores next to the path.
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub version: String,
    /// Absolute path of the `.vst3` module (bundle directory or flat DLL).
    pub path: String,
    /// Which picker this plugin belongs in: `instrument`, `audio-effect`,
    /// `other`, or `unknown`.
    ///
    /// Shares its vocabulary with [`ClapPluginInfoDto::kind`] so one list can
    /// filter both formats. `unknown` has no CLAP equivalent: it means the module
    /// exposes no `IPluginFactory2` and therefore declares no subcategories at
    /// all, which is a real state and not a failure.
    pub kind: String,
    /// The declared subcategories, split for display (`["Fx", "Reverb"]`).
    pub subcategories: Vec<String>,
}

impl From<&Vst3PluginInfo> for Vst3PluginInfoDto {
    fn from(info: &Vst3PluginInfo) -> Self {
        Self {
            id: info.cid.to_hex(),
            name: info.name.clone(),
            vendor: info.vendor.clone(),
            version: info.version.clone(),
            path: info.path.to_string_lossy().into_owned(),
            kind: info.kind().id().to_owned(),
            subcategories: info.subcategory_list(),
        }
    }
}

// ---------------------------------------------------------------- sequencer

/// One cell of the step grid.
///
/// `velocity` is carried even for a cell that is off, so toggling a step in the
/// UI and toggling it back does not silently reset an accent.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepDto {
    pub on: bool,
    /// Normalised velocity, `0.0..=1.0`.
    #[serde(default = "default_velocity")]
    pub velocity: f32,
}

fn default_velocity() -> f32 {
    DEFAULT_STEP_VELOCITY
}

impl From<&Step> for StepDto {
    fn from(step: &Step) -> Self {
        Self {
            on: step.on,
            velocity: step.velocity,
        }
    }
}

/// One row of the step grid.
///
/// `label` is derived, not stored: the row's identity is its key, and the name
/// ("Kick", "CH") is a rendering of that key. It is sent so the UI does not have
/// to keep a second copy of the General MIDI drum map, and ignored on the way
/// back in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatternRowDto {
    pub key: u8,
    #[serde(default)]
    pub channel: u8,
    #[serde(default)]
    pub label: String,
    /// Exactly [`PatternDto::steps`] entries -- the cells that play, not the
    /// engine's full-length array. See [`Pattern::try_from`].
    pub steps: Vec<StepDto>,
}

impl PatternRowDto {
    /// This row's first `length` cells, which are the ones the pattern plays.
    pub fn of(row: &PatternRow, length: usize) -> Self {
        Self {
            key: row.key,
            channel: row.channel,
            label: row_label(row.key),
            steps: row
                .steps
                .iter()
                .take(length.min(MAX_STEPS))
                .map(StepDto::from)
                .collect(),
        }
    }
}

/// One note of the piano roll.
///
/// `start` and `length` are in **ticks** -- 960 to the quarter note, so a
/// sixteenth is 240. Named without the unit in the field itself because this
/// shape is written into the project file as well as sent to the webview, and
/// `startTicks` in a file whose every other key is `snake_case` would be a wart
/// on the format for the rest of its life. The units are documented here, in
/// `ui/src/api/types.ts`, and in the spec.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MelodicNoteDto {
    /// MIDI key, `0..=127`. 60 is middle C.
    pub key: u8,
    /// Note channel, i.e. the index of the track this note plays.
    #[serde(default)]
    pub channel: u8,
    /// Offset from the start of the pattern, in ticks.
    pub start: u32,
    /// How long the note sounds, in ticks. At least 1.
    pub length: u32,
    #[serde(default = "default_velocity")]
    pub velocity: f32,
}

impl From<&MelodicNote> for MelodicNoteDto {
    fn from(note: &MelodicNote) -> Self {
        Self {
            key: note.key,
            channel: note.channel,
            start: note.start_ticks,
            length: note.length_ticks,
            velocity: note.velocity,
        }
    }
}

impl From<&MelodicNoteDto> for MelodicNote {
    fn from(dto: &MelodicNoteDto) -> Self {
        Self {
            key: dto.key,
            channel: dto.channel,
            start_ticks: dto.start,
            length_ticks: dto.length,
            velocity: dto.velocity,
        }
    }
}

/// The whole pattern, as the webview reads and writes it.
///
/// Field order is the order it appears in a `.mydaw` file (see
/// [`crate::project_file`]), so `notes` goes **after** `rows`: a version 1 file
/// then reads as a version 2 one with the notes missing, which is exactly what
/// the migration wants.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatternDto {
    /// Steps in the loop: 16, 32 or 64. Every row carries this many entries.
    pub steps: u8,
    pub rows: Vec<PatternRowDto>,
    /// The piano roll. Absent in a version 1 project file, which is how a
    /// migration to version 2 is a `#[serde(default)]` rather than a branch.
    #[serde(default)]
    pub notes: Vec<MelodicNoteDto>,
}

impl From<&Pattern> for PatternDto {
    fn from(pattern: &Pattern) -> Self {
        let length = pattern.length_steps as usize;
        Self {
            steps: pattern.length_steps,
            rows: pattern
                .rows
                .iter()
                .map(|row| PatternRowDto::of(row, length))
                .collect(),
            notes: pattern.notes.iter().map(MelodicNoteDto::from).collect(),
        }
    }
}

impl TryFrom<&PatternDto> for Pattern {
    type Error = PatternError;

    /// Rebuild a [`Pattern`] from what the webview sent, then validate it.
    ///
    /// A row whose `steps` array is not exactly `dto.steps` long is rejected
    /// here rather than being padded or truncated: a UI that sent 15 steps has a
    /// bug, and quietly inventing the sixteenth would hide it behind a beat that
    /// is almost right.
    ///
    /// Note that the *engine's* rows are always [`MAX_STEPS`] long whatever the
    /// pattern's length -- the tail is kept so that shortening a pattern is not
    /// destructive -- but the wire only ever carries the steps that play. A
    /// round trip through the webview therefore loses the tail, which is the
    /// right trade: the UI has no way to show it, and sending 64 cells to draw
    /// 16 of them would be inviting a UI bug to become a data one.
    fn try_from(dto: &PatternDto) -> Result<Self, Self::Error> {
        let length = dto.steps as usize;
        if !Pattern::is_legal_length(length) {
            return Err(PatternError::WrongStepCount(length));
        }
        let mut rows = Vec::with_capacity(dto.rows.len());
        for (index, row) in dto.rows.iter().enumerate() {
            if row.steps.len() != length {
                return Err(PatternError::WrongRowLength {
                    row: index,
                    got: row.steps.len(),
                    expected: length,
                });
            }
            let mut steps = [Step::OFF; MAX_STEPS];
            for (step, source) in steps.iter_mut().zip(&row.steps) {
                *step = Step {
                    on: source.on,
                    velocity: source.velocity,
                };
            }
            rows.push(PatternRow {
                key: row.key,
                channel: row.channel,
                steps,
            });
        }
        let pattern = Self {
            rows,
            length_steps: dto.steps,
            notes: dto.notes.iter().map(MelodicNote::from).collect(),
        };
        pattern.validate()?;
        Ok(pattern)
    }
}

/// Everything the sequencer panel and the transport bar need in one message.
///
/// Tempo travels with the pattern rather than in
/// [`UiSnapshot`](mydaw_ipc_bridge::UiSnapshot): that snapshot is the *kernel's*
/// registry (plugins and panels), taken once at boot from a kernel that has no
/// audio device yet, and `ipc-bridge` has no business depending on the
/// sequencer. The two values a sequencer needs to draw itself belong together
/// and arrive together.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SequencerStateDto {
    /// Session tempo, already clamped to the transport's legal range.
    pub bpm: f64,
    /// The open device's sample rate, or `0` when no stream is running.
    ///
    /// The panel needs it to turn the transport position it gets in *frames*
    /// into a step number. Without it the UI would have to assume 48 kHz, and
    /// the moving highlight would sit in the wrong step on a 44.1 kHz interface
    /// -- visible, and exactly the kind of small lie that erodes trust in a
    /// playhead.
    pub sample_rate: f64,
    pub pattern: PatternDto,
}

/// What the open audio device is, for the status bar's BUFFER and LATENCY.
///
/// Every field is `0`/empty when no stream is running, which is a real state
/// rather than a failure: the UI shows a dash and says so. `latency_ms` is
/// derived here rather than in the webview so the two never disagree about
/// which block size the figure was computed from.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamInfoDto {
    /// The device the stream is on, or an empty string while stopped.
    pub device_name: String,
    pub sample_rate: f64,
    pub channels: u32,
    /// Largest block the callback can be handed, in frames.
    pub buffer_frames: u32,
    /// One block's worth of time, in milliseconds. `0` while stopped.
    pub latency_ms: f64,
    /// Whether a stream is open at all.
    pub running: bool,
}

impl StreamInfoDto {
    /// The "no device" answer: what the status bar draws before audio starts.
    pub fn stopped() -> Self {
        Self {
            device_name: String::new(),
            sample_rate: 0.0,
            channels: 0,
            buffer_frames: 0,
            latency_ms: 0.0,
            running: false,
        }
    }
}

impl From<mydaw_plugin_audio_io::StreamInfo> for StreamInfoDto {
    fn from(info: mydaw_plugin_audio_io::StreamInfo) -> Self {
        let sample_rate = f64::from(info.sample_rate);
        let buffer_frames = info.max_block as u32;
        let latency_ms = if sample_rate > 0.0 {
            f64::from(buffer_frames) / sample_rate * 1000.0
        } else {
            0.0
        };
        Self {
            device_name: info.device_name,
            sample_rate,
            channels: u32::from(info.channels),
            buffer_frames,
            latency_ms,
            running: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_plugin_sequencer::DEFAULT_STEPS;

    #[test]
    fn serialises_with_the_key_names_the_ui_reads() {
        let dto = MeterFrameDto::from(MeterFrame {
            peak_l: 0.5,
            peak_r: 0.25,
            rms_l: 0.1,
            rms_r: 0.2,
            sample_pos: 4_096,
            playing: true,
            ..MeterFrame::SILENT
        });
        let json: serde_json::Value = serde_json::to_value(dto).unwrap();
        let object = json.as_object().unwrap();

        // Mirrors `MeterFrame` in ui/src/api/types.ts, key for key.
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "peakL",
                "peakR",
                "playing",
                "rmsL",
                "rmsR",
                "samplePos",
                "tracks"
            ]
        );
        assert_eq!(object["peakL"], 0.5);
        assert_eq!(object["samplePos"], 4_096);
        assert_eq!(object["playing"], true);
        assert_eq!(object["tracks"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn only_the_tracks_that_exist_cross_the_boundary() {
        let mut peaks = mydaw_rt_core::TrackPeaks::empty(2);
        peaks.set(
            1,
            mydaw_rt_core::TrackPeak {
                peak_l: 0.75,
                peak_r: 0.5,
            },
        );
        let mut frame = MeterFrame::SILENT;
        frame.set_track_peaks(&peaks);

        let dto = MeterFrameDto::from(frame);
        assert_eq!(dto.tracks.len(), 2, "not the fixed sixteen");
        assert_eq!(
            dto.tracks[0],
            TrackMeterDto {
                peak_l: 0.0,
                peak_r: 0.0
            }
        );
        assert_eq!(dto.tracks[1].peak_l, 0.75);

        let json: serde_json::Value = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["tracks"][1]["peakL"], 0.75);
        assert_eq!(json["tracks"][1]["peakR"], 0.5);
    }

    #[test]
    fn the_bench_timestamp_is_absent_unless_the_pump_stamped_the_frame() {
        let plain = MeterFrameDto::from(MeterFrame::SILENT);
        assert_eq!(plain.pump_ms, None);
        let json = serde_json::to_string(&plain).unwrap();
        assert!(
            !json.contains("pumpMs"),
            "an unstamped frame carries nothing the bench added: {json}"
        );

        let stamped = MeterFrameDto {
            pump_ms: Some(1234.5),
            ..plain
        };
        let json: serde_json::Value = serde_json::to_value(stamped).unwrap();
        assert_eq!(json["pumpMs"], 1234.5);
    }

    #[test]
    fn clap_plugin_info_serialises_with_the_keys_the_ui_reads() {
        let dto = ClapPluginInfoDto::from(&ClapPluginInfo {
            id: "com.acme.thing".to_owned(),
            name: "Thing".to_owned(),
            vendor: "Acme".to_owned(),
            version: "1.2.3".to_owned(),
            path: std::path::PathBuf::from(r"C:\CLAP\Thing.clap"),
            features: vec!["instrument".to_owned(), "stereo".to_owned()],
        });
        let json: serde_json::Value = serde_json::to_value(dto).unwrap();
        let object = json.as_object().unwrap();

        // Mirrors `ClapPluginInfoDto` in ui/src/api/types.ts, key for key.
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["features", "id", "kind", "name", "path", "vendor", "version"]
        );
        assert_eq!(object["id"], "com.acme.thing");
        assert_eq!(object["name"], "Thing");
        assert_eq!(object["vendor"], "Acme");
        assert_eq!(object["version"], "1.2.3");
        assert_eq!(object["path"], r"C:\CLAP\Thing.clap");
        assert_eq!(
            object["kind"], "instrument",
            "the picker filters on this, so it has to come from the features"
        );
    }

    #[test]
    fn a_plugin_with_no_vendor_or_version_still_serialises() {
        // `scan_bundle` leaves these empty rather than dropping the plugin, so the
        // DTO has to survive it -- the UI shows a dash.
        let dto = ClapPluginInfoDto::from(&ClapPluginInfo {
            id: "x".to_owned(),
            name: String::new(),
            vendor: String::new(),
            version: String::new(),
            path: std::path::PathBuf::from("x.clap"),
            features: Vec::new(),
        });
        let json = serde_json::to_string(&dto).unwrap();
        assert!(json.contains("\"vendor\":\"\""), "{json}");
        assert_eq!(
            dto.kind, "other",
            "a bundle that declares no features is not a lie, just unhelpful"
        );
    }

    // ------------------------------------------------------------ sequencer

    #[test]
    fn the_sequencer_state_serialises_with_the_keys_the_panel_reads() {
        let dto = SequencerStateDto {
            bpm: 130.0,
            sample_rate: 44_100.0,
            pattern: PatternDto::from(&Pattern::default()),
        };
        let json: serde_json::Value = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["bpm"], 130.0);
        assert_eq!(json["sampleRate"], 44_100.0);
        assert_eq!(json["pattern"]["steps"], 16);

        let rows = json["pattern"]["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0]["key"], 36);
        assert_eq!(rows[0]["label"], "Kick");
        assert_eq!(rows[3]["label"], "OH");
        assert_eq!(rows[0]["steps"].as_array().unwrap().len(), 16);
        assert_eq!(rows[0]["steps"][0]["on"], false);
        assert!(rows[0]["steps"][0]["velocity"].is_number());

        // Mirrors `PatternRowDto` in ui/src/api/types.ts, key for key.
        let mut keys: Vec<&str> = rows[0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["channel", "key", "label", "steps"]);
    }

    #[test]
    fn a_pattern_round_trips_through_the_wire_shape_unchanged() {
        let mut pattern = Pattern::default();
        pattern.rows[0] = PatternRow::empty(36).with_steps(&[0, 4, 8, 12]);
        pattern.rows[1].steps[4] = Step::accent();
        pattern.rows[1].channel = 2;

        let dto = PatternDto::from(&pattern);
        let json = serde_json::to_string(&dto).unwrap();
        let parsed: PatternDto = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, dto);
        assert_eq!(Pattern::try_from(&parsed).unwrap(), pattern);
    }

    #[test]
    fn the_label_is_derived_on_the_way_out_and_ignored_on_the_way_in() {
        let json = r#"{
            "steps": 16,
            "rows": [{
                "key": 36,
                "label": "whatever the webview felt like",
                "steps": [
                    {"on": true, "velocity": 1.0},
                    {"on": false, "velocity": 0.78},
                    {"on": false, "velocity": 0.78},
                    {"on": false, "velocity": 0.78},
                    {"on": false, "velocity": 0.78},
                    {"on": false, "velocity": 0.78},
                    {"on": false, "velocity": 0.78},
                    {"on": false, "velocity": 0.78},
                    {"on": false, "velocity": 0.78},
                    {"on": false, "velocity": 0.78},
                    {"on": false, "velocity": 0.78},
                    {"on": false, "velocity": 0.78},
                    {"on": false, "velocity": 0.78},
                    {"on": false, "velocity": 0.78},
                    {"on": false, "velocity": 0.78},
                    {"on": false, "velocity": 0.78}
                ]
            }]
        }"#;
        let dto: PatternDto = serde_json::from_str(json).unwrap();
        let pattern = Pattern::try_from(&dto).unwrap();
        assert_eq!(pattern.rows[0].key, 36);
        assert_eq!(pattern.rows[0].channel, 0, "absent means zero");
        assert!(pattern.rows[0].steps[0].on);
        // On the way back out the label is the one the drum map says.
        assert_eq!(PatternDto::from(&pattern).rows[0].label, "Kick");
    }

    #[test]
    fn a_row_of_the_wrong_length_is_rejected_by_name_rather_than_padded() {
        let dto = PatternDto {
            steps: 16,
            rows: vec![PatternRowDto {
                key: 36,
                channel: 0,
                label: String::new(),
                steps: vec![
                    StepDto {
                        on: true,
                        velocity: 1.0
                    };
                    15
                ],
            }],
            notes: Vec::new(),
        };
        assert_eq!(
            Pattern::try_from(&dto),
            Err(PatternError::WrongRowLength {
                row: 0,
                got: 15,
                expected: 16
            })
        );
    }

    #[test]
    fn a_pattern_the_engine_cannot_run_is_refused_at_the_boundary() {
        let row = |key: u8, velocity: f32| PatternRowDto {
            key,
            channel: 0,
            label: String::new(),
            steps: vec![StepDto { on: true, velocity }; DEFAULT_STEPS],
        };

        // Too many rows.
        let dto = PatternDto {
            steps: 16,
            rows: (0..17).map(|k| row(k as u8, 1.0)).collect(),
            notes: Vec::new(),
        };
        assert!(matches!(
            Pattern::try_from(&dto),
            Err(PatternError::TooManyRows(17))
        ));

        // The wrong number of steps.
        let dto = PatternDto {
            steps: 8,
            rows: vec![row(36, 1.0)],
            notes: Vec::new(),
        };
        assert!(matches!(
            Pattern::try_from(&dto),
            Err(PatternError::WrongStepCount(8))
        ));

        // A velocity the webview should never have sent.
        for bad in [-0.5_f32, 1.5, f32::NAN] {
            let dto = PatternDto {
                steps: 16,
                rows: vec![row(36, bad)],
                notes: Vec::new(),
            };
            assert!(
                matches!(
                    Pattern::try_from(&dto),
                    Err(PatternError::InvalidVelocity { .. })
                ),
                "velocity {bad} should have been refused"
            );
        }

        // A key outside the MIDI range.
        let dto = PatternDto {
            steps: 16,
            rows: vec![row(200, 1.0)],
            notes: Vec::new(),
        };
        assert!(matches!(
            Pattern::try_from(&dto),
            Err(PatternError::KeyOutOfRange { row: 0, key: 200 })
        ));
    }

    #[test]
    fn a_step_with_no_velocity_at_all_gets_the_default_rather_than_zero() {
        let dto: StepDto = serde_json::from_str(r#"{"on": true}"#).unwrap();
        assert!(dto.on);
        assert_eq!(dto.velocity, DEFAULT_STEP_VELOCITY);
    }
}
