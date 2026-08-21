# Sample Root-Pitch and Keyboard Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every audio-file channel directly auditionable from its waveform and a gated chromatic keyboard, with benchmarked root-pitch detection and persistent root calibration.

**Architecture:** Add a standalone non-RT pitch-analysis crate, carry analysis and channel-root data through the existing Rust/TypeScript boundary, and extend the existing fixed-size one-shot voice implementation so Audio Clip track heads also accept notes. Keep creative channel transpose separate from root calibration and preserve the single preview bus for click audition.

**Tech Stack:** Rust 2021, `realfft`, Tauri v2 commands, SolidJS/TypeScript, Vitest, Cargo tests.

## Global Constraints

- Root pitch means MIDI note plus cents, not major/minor musical key.
- Unpitched or ambiguous input must return a typed no-result; never default it to C.
- Both Audio Clip and OneShot channels accept gated live and piano-roll notes.
- Analysis is bounded and never runs on the real-time callback.
- The callback must not allocate, lock, block, perform I/O, log, panic, or do unbounded work.
- Rust DTOs, TypeScript types, `Backend`, `TauriBackend`, and `MockBackend` stay aligned.
- Preserve the user's existing uncommitted work. Do not commit unless explicitly requested.

---

## File map

### New files

- `crates/pitch-analysis/Cargo.toml` — isolated analysis crate and dependencies.
- `crates/pitch-analysis/src/lib.rs` — public result types, metadata/filename hints, YIN-style detector, confidence/rejection policy.
- `crates/pitch-analysis/tests/ground_truth.rs` — deterministic generated corpus and optional manifest-driven external corpus runner.
- `crates/pitch-analysis/tests/data/negative-manifest.json` — generated/redistributable negative cases only.
- `ui/src/lib/samplePitch.ts` — note/cents formatting and target-transpose math.
- `ui/src/lib/samplePitch.test.ts` — pure UI/domain tests.
- `ui/src/lib/sampleKeyboard.ts` — framework-independent pointer/note lifecycle controller.
- `ui/src/lib/sampleKeyboard.test.ts` — note lifecycle and visible-range tests in the existing Node Vitest environment.
- `ui/src/ui/SampleKeyboard.tsx` — pointer-safe compact MIDI keyboard.

### Modified files

- `Cargo.toml`, `app/Cargo.toml` — register and consume `mydaw-pitch-analysis`.
- `plugins/sequencer/src/oneshot.rs` — root-aware shared playback params and reusable fixed voice player.
- `app/src/project.rs`, `app/src/project_file.rs`, `app/src/tracks.rs` — channel root model, persistence, setters, DTO.
- `app/src/state.rs`, `app/src/commands.rs`, `app/src/lib.rs` — analysis/root commands and direct notes on Audio Clip channels.
- `app/src/preview.rs` — click audition remains on the edited-track preview path.
- `ui/src/api/types.ts`, `ui/src/api/backend.ts`, `ui/src/api/mock.ts`, `ui/src/api/mock.test.ts` — complete wire contract.
- `ui/src/views/SampleEditor.tsx`, `ui/src/styles/sample.css` — remove Preview, click audition, keyboard, detection, root and target controls.
- `plugins/sequencer/tests/rt_alloc.rs`, `app/tests/project_persistence.rs`, `app/tests/audio_clip_audio.rs` — RT, persistence, and direct Audio Clip note coverage.

---

### Task 1: Root-pitch analysis crate

**Interfaces:**

- Produces:

```rust
pub enum PitchSource { Metadata, Filename, Analysis }
pub enum NoPitchReason { Silent, TooShort, Unvoiced, Unstable, Polyphonic }
pub struct RootPitch {
    pub midi_note: u8,
    pub cents: f32,
    pub confidence: f32,
    pub voiced_coverage: f32,
    pub source: PitchSource,
}
pub enum PitchOutcome { Detected(RootPitch), NoStablePitch(NoPitchReason) }
pub fn detect_root_pitch(
    stereo: &[f32],
    frames: usize,
    sample_rate: f64,
    range: Range<usize>,
) -> PitchOutcome;
pub fn root_hint(path: &Path) -> Option<RootPitch>;
```

- [ ] Write generated-signal tests first for A4, detuned C4, missing fundamental, vibrato, silence, white noise, a short transient, and a two-note chord.
- [ ] Run `cargo test -p mydaw-pitch-analysis` and verify failure because the crate/API does not exist.
- [ ] Add the crate with only `realfft.workspace = true`, `thiserror.workspace = true`, and test-only `hound.workspace = true`.
- [ ] Implement bounded stereo downmix, DC removal, RMS/silence rejection, overlapping YIN difference/CMNDF frames, candidate confidence, robust cents aggregation, and stability/coverage rejection.
- [ ] Implement MIDI conversion with:

```rust
let midi_float = 69.0 + 12.0 * (frequency_hz / 440.0).log2();
let midi_note = midi_float.round().clamp(0.0, 127.0) as u8;
let cents = ((midi_float - f64::from(midi_note)) * 100.0) as f32;
```

- [ ] Implement WAV `smpl` parsing with bounded RIFF chunk walking and filename hints matching standalone note tokens such as `F#3`, `Gb_3`, and `-060-`; reject incidental digits.
- [ ] Run the crate tests and tune only documented confidence/coverage/spread constants until deterministic positives and negatives pass.

### Task 2: Persistent root model and wire contract

**Interfaces:**

- `TrackDef`: `root_key: u8`, `root_fine_cents: f32`, `root_mode: RootMode`, `detected_pitch: Option<RootPitchDto>`.
- Commands:

```rust
pub fn analyze_track_pitch(track: usize) -> Result<PitchAnalysisDto, String>;
pub fn set_track_sample_root(
    track: usize,
    key: u8,
    fine_cents: f32,
    mode: RootMode,
) -> Result<MixerStateDto, String>;
```

- [ ] Add failing project equality, serde-default, JSON wire-shape, and save/load round-trip tests. Older projects must load with root C4, zero fine cents, and `default` mode.
- [ ] Add `RootMode`, `RootPitchDto`, and `PitchAnalysisDto` to `project.rs`; add sanitized constructors that reject non-finite cents and out-of-range keys.
- [ ] Add fields to `TrackDef`, `TrackEntry`, DTO generation, save conversion, load conversion, and `PartialEq`.
- [ ] Add root setters to `TrackHost` that update scalar truth and the shared playback atomics without replacing active voices.
- [ ] Add analysis in `AppState`: resolve only `AudioClip`/`OneShot`, obtain the decoded source and selected frame range, prefer `root_hint(path)`, then call `detect_root_pitch`.
- [ ] Add and register Tauri commands; add state/command tests for no-such-track, wrong source type, detected result, and typed no-result.
- [ ] Run `cargo test -p mydaw-app project_file` and `cargo test -p mydaw-app state`.

### Task 3: Root-aware note playback for both audio-file engines

**Interfaces:**

- `SamplePlaybackParams::new(start, end, pitch, fades, root_key, root_fine_cents)`.
- `SamplePlaybackParams::set_root(root_key, root_fine_cents)`.
- Playback rate:

```rust
let cents = params.pitch_cents
    + (f32::from(key) - f32::from(params.root_key)) * 100.0
    - params.root_fine_cents;
let step = playback_step(source.sample_rate(), sample_rate) * pitch_ratio(cents);
```

- [ ] Add failing one-shot tests proving a detected `F#3 +12 ct` root plays concert F#3 at key 54 and channel transpose stacks independently.
- [ ] Refactor the fixed `[Voice; ONESHOT_VOICES]` logic into a reusable note-driven sample player without changing its allocation behavior or gated release.
- [ ] Build Audio Clip track heads as a composite of the existing arrangement player and the reusable note player, both summing into the same track buffer before inserts.
- [ ] Add an integration test where one Audio Clip channel plays a playlist clip and a live note in the same render and both are present.
- [ ] Extend `rt_alloc.rs` to send note-on/off through Audio Clip and OneShot nodes after root changes and assert zero callback allocations.
- [ ] Run `cargo test -p mydaw-plugin-sequencer` and the focused app audio-clip integration test.

### Task 4: TypeScript backend and mock alignment

**Interfaces:**

```ts
export type RootMode = 'default' | 'detected' | 'manual';
export type PitchSource = 'metadata' | 'filename' | 'analysis';
export type NoPitchReason = 'silent' | 'tooShort' | 'unvoiced' | 'unstable' | 'polyphonic';
export type PitchAnalysis =
  | { kind: 'detected'; midiNote: number; cents: number; confidence: number; voicedCoverage: number; source: PitchSource }
  | { kind: 'noStablePitch'; reason: NoPitchReason };
```

- [ ] Add failing mock tests for detected/no-result analysis, root persistence in `getTracks`, target transpose, and note audition on an Audio Clip track.
- [ ] Extend `TrackDto` and add `PitchAnalysis` types.
- [ ] Add `analyzeTrackPitch(track)` and `setTrackSampleRoot(track, key, fineCents, mode)` to `Backend` and `TauriBackend`.
- [ ] Implement deterministic MockBackend outcomes keyed by fixture filename (`tone-A4.wav`, `noise.wav`) and mirror Rust validation.
- [ ] Run `pnpm --dir ui test -- mock.test.ts`.

### Task 5: Pure pitch UI helpers and keyboard lifecycle

**Interfaces:**

```ts
export function formatDetectedPitch(result: DetectedPitch): string;
export function targetTransposeCents(
  detectedNote: number,
  detectedCents: number,
  targetNote: number,
): number;
export function visibleKeyboardRange(center: number, octaves: number): readonly [number, number];
```

- [ ] Write failing tests for enharmonic-safe scientific note names, signed cents, semitone-boundary rounding, transpose clamping, and keyboard ranges at MIDI edges.
- [ ] Implement `samplePitch.ts` using the existing `noteName()` convention.
- [ ] Write keyboard controller tests for pointer down, glissando, pointer up, cancel, lost capture, track change, and cleanup; each sequence must have balanced note-on/off calls.
- [ ] Implement the framework-independent controller in `sampleKeyboard.ts`, then bind it from `SampleKeyboard.tsx` with accessible piano-key buttons, pointer capture, visible held-note state, and octave viewport controls.
- [ ] Run the focused Vitest files.

### Task 6: SampleEditor workflow

**Interfaces:**

- Consumes Task 4 backend methods and Task 5 helpers/component.
- Produces no direct Tauri calls.

- [ ] Extract or test the waveform gesture decision so click, drag, handle drag, and double-click are mutually exclusive.
- [ ] Remove the Preview button and `IconPlay` import.
- [ ] Change the no-move branch of `onRegionPointer` to call edited-track audition exactly once; suppress the click generated by double-click reset.
- [ ] Add local stale-safe analysis state keyed by track id, audio source id, and region; discard answers whose key no longer matches.
- [ ] Add `Find pitch`, result/reason copy, `Use as root`, root display, and target-note selection. Target selection computes and writes the existing pitch knob value without altering root fields.
- [ ] Mount `SampleKeyboard` for both `clip` and `sample` engines. Ensure the editor selects its track before the first note and releases notes on source/track change and cleanup.
- [ ] Style the controls using existing tokens, preserve the 660×440 default usability, and increase default height only if the keyboard would otherwise overlap metadata.
- [ ] Add pure tests for waveform gesture arbitration and stale analysis keys, then run `pnpm --dir ui test` plus `pnpm --dir ui build`.

### Task 7: Ground-truth benchmark harness

**Interfaces:**

- Optional environment variable `MYDAW_PITCH_CORPUS` points to a directory containing:

```json
{
  "samples": [
    { "file": "guitar_A3.wav", "midiNote": 57, "cents": 0, "pitched": true },
    { "file": "hat.wav", "pitched": false }
  ]
}
```

- [ ] Add generated corpus metrics tests that fail when exact-note accuracy, cents error, or negative false positives exceed the approved gates.
- [ ] Add an ignored external-corpus test that reads the manifest, never downloads data, and prints exact-note accuracy, median/P95 cents error, false positives, rejection rate, and elapsed time.
- [ ] Document NSynth/MDB-stem-synth preparation in the test module comments, including expected manifest fields and licensing boundaries.
- [ ] Run the generated benchmark in release mode and record timing in the final report.
- [ ] If the deterministic detector misses the approved gates, retain the same public interface, document evidence, and compare a second candidate before changing production behavior.

### Task 8: Full verification and regression review

- [ ] Run `cargo fmt --all --check`.
- [ ] Run `cargo test --workspace`.
- [ ] Run `cargo clippy --workspace --all-targets -- -D warnings`.
- [ ] Run `pnpm --dir ui test`.
- [ ] Run `pnpm --dir ui build`.
- [ ] Inspect `git diff --check`, `git diff --stat`, and focused diffs to ensure generated `ui/dist`, dependency stores, and unrelated user files were not modified.
- [ ] Manually verify in the real app: click waveform, drag region, double-click reset, keyboard press/glide/release, Audio Clip and OneShot notes, pitched and unpitched analysis, root calibration, target retune, project save/load, and transport overlap.
