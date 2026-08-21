# Sample root-pitch and keyboard design

Status: approved
Depends on: sample channel editor, one-shot playback, audio-clip channel routing

## Goal

Make a loaded sample immediately playable and tunable from its editor:

1. A click on the waveform auditions the current edited region. Existing drag
   gestures still edit the region and double-click still restores the full file.
2. A compact piano at the bottom auditions any MIDI pitch with gated note
   lifetime.
3. `Find pitch` identifies the root pitch of a stable monophonic sample, reports
   note plus cents and confidence, and explicitly reports unpitched or ambiguous
   material instead of inventing a root.
4. The detected root calibrates piano-roll and keyboard playback. A target-note
   selector can also retune the sample by changing the existing channel pitch.

This is root-pitch detection, not major/minor musical-key estimation.

## Product decisions

- Waveform click starts the selected region from its beginning. A later scrub or
  click-position preview is separate because the current waveform also owns
  region drawing.
- Both `AudioClip` and `OneShot` sample channels accept live and piano-roll
  notes. Playlist playback and note-triggered playback may overlap and sum
  through the same channel strip.
- Notes are gated: note-off ends the voice with the existing click-free release.
- Root calibration and creative channel transpose are separate values.
- Analysis reads trustworthy embedded root metadata first, then a filename hint,
  then signal analysis. The UI identifies the source of the answer.
- Silence, noise, percussion without stable periodicity, polyphony, and
  insufficient voiced material return a typed no-result state.
- Analysis is bounded, cancellable/stale-safe, cached outside the audio callback,
  and never performs work on the real-time thread.

## UX

### Waveform

The Preview button is removed. On pointer release:

- movement below the existing drag slop auditions;
- movement at or above the slop commits a region edit;
- handle drags only edit;
- double-click restores the full-file region and does not also audition.

Rapid audition clicks replace the current preview through the existing
click-free preview bus. Loading, decode, or device errors use the existing
status/error path.

### Keyboard

A low-profile keyboard spans the editor footer and scrolls horizontally over
MIDI 0–127, initially centered around the detected root or C4. White and black
keys remain usable pointer targets; octave controls move the viewport without
changing pitch.

Pointer down sends note-on. Pointer enter while captured glides to another key
by releasing the previous note first. Pointer up, cancel, lost capture, editor
close, track change, and window blur always send note-off. The global typing
keyboard and MIDI input highlight matching visible keys.

The keyboard is present for both audio-file channel engines and absent for
unrelated built-ins or hosted plugins.

### Pitch analysis and retuning

`Find pitch` has four visible states:

- idle;
- analyzing, with the action disabled;
- detected, e.g. `F#3 +12 ct · 92%`;
- no stable pitch, with a short reason.

Detection never changes sound automatically. `Use as root` stores the detected
note/cents as the sample's playback root. A target note control then computes a
channel transpose from the detected pitch and writes the existing `pitchCents`
parameter. Re-running analysis does not destroy a manual root override.

## Data model

The engine owns the authoritative values:

- `detected_pitch`: optional analysis result containing MIDI note, cents,
  confidence, voiced coverage, and source (`metadata`, `filename`, `analysis`);
- `root_key`: MIDI note used for note-to-rate mapping;
- `root_fine_cents`: measured deviation of the recording from that note;
- `root_mode`: `default`, `detected`, or `manual`.

Analysis results belong to the decoded audio source and are cached by source
identity plus the analyzed frame range. Root mapping belongs to the channel
because two uses of one file may intentionally map it differently. Channel root
mapping and detected result persist in the project; transient analysis progress
does not.

Rust DTOs, TypeScript wire types, `Backend`, `TauriBackend`, and `MockBackend`
remain aligned. Views do not invoke Tauri directly.

## Playback

The current one-shot equation

`channel pitch + (note - C4)`

becomes

`channel pitch + (note - root key) - root fine cents`.

The Audio Clip track head gains the same fixed-size note voice pool used by the
one-shot player while retaining its compiled playlist player. Both processors
read the same immutable audio source and channel playback parameters and sum
before inserts/fader/pan. Root edits are atomic snapshots for new voices;
already sounding voices are not retuned.

No allocation, lock, I/O, logging, or unbounded work is added to the callback.

## Pitch detector

The first shipping candidate is a deterministic pYIN/YIN-style offline detector:

1. downmix with peak-safe averaging and remove DC;
2. reject silence and very short regions;
3. analyze bounded overlapping frames over the musically useful range;
4. produce multiple periodicity candidates and voiced probabilities;
5. smooth candidates over time to suppress octave jumps;
6. aggregate stable voiced frames in log-frequency/cents space;
7. reject low voiced coverage, weak confidence, or excessive pitch spread;
8. quantize the robust center to MIDI note plus signed cents.

The detector sits behind a small analysis interface so a benchmark can compare
it with CREPE or another implementation without changing IPC or project data.
No neural runtime ships unless measured results justify its product and binary
cost.

## Validation

### Deterministic tests

- sine waves across supported pitch and sample-rate ranges;
- missing fundamentals, harmonic-rich waves, detuned notes, vibrato, fades,
  stereo phase differences, DC, noise, silence, and short transients;
- explicit rejection of hats/noise, low-confidence kicks, chords, and
  insufficient material;
- metadata and filename precedence;
- note/cents conversion around semitone and octave boundaries;
- stale analysis answers cannot update a newly selected sample.

### Ground-truth evaluation

The benchmark runner consumes manifests rather than committing licensed audio.
It supports:

- a stratified NSynth holdout covering families, pitches, velocities, and
  acoustic/electronic sources;
- MDB-stem-synth or an equivalent frame-annotated monophonic set;
- a small manually verified real-world sample-pack corpus;
- a negative corpus of percussion, noise, silence, chords, and loops.

Report median and 95th-percentile absolute cents error, exact-note and octave
error rates, unpitched false-positive rate, rejection rate on pitched samples,
confidence calibration, runtime, and peak memory.

Initial acceptance gates:

- at least 95% exact-note accuracy on the approved pitched holdout;
- median absolute error at most 10 cents and P95 at most 35 cents;
- at most 2% pitched false positives on the approved negative corpus;
- analysis of a 15-second stereo file completes within 250 ms on the reference
  development machine without blocking UI or audio;
- no audio-thread allocations in the expanded sample player.

The thresholds may only be changed with benchmark evidence and explicit
approval.

## Integration tests

- waveform click auditions exactly once; drag and double-click do not;
- keyboard pointer lifecycle cannot leave a held note;
- Audio Clip and OneShot channels both respond to keyed notes and preserve
  playlist behavior;
- detected root plus fine cents maps its MIDI key to concert pitch;
- target retune updates channel pitch without overwriting root calibration;
- root and analysis data round-trip through project save/load;
- mocks reproduce success, no-result, stale, and failure behavior.

## Out of scope

- major/minor key estimation for loops or songs;
- polyphonic note transcription;
- Bitwig-style time-varying root tracking;
- formant-preserving or constant-duration pitch shifting;
- writing detected metadata back into source audio files.
