# WAVE v2 — architecture and engine ⇄ UI contract

```
 ┌──────────── ui/ (SolidJS + canvas) ────────────┐        ┌──── wave (CLI) ────┐
 │ playlist · mixer · FX windows · library · ...  │        │ context/apply/undo │
 └───────▲───────────────┬────────────────────────┘        └────────▲───────────┘
   events│ "state"       │ invoke(...)          binary Channel       │ local socket
   ┌─────┴───────────────▼───────────────────────────────────────────┴───────────┐
 │ app/ (Tauri shell): commands, session server, CLI shim, project files        │
 └───────────────────────────┬──────────────────────────────────────────────────┘
                             │ Engine handle (Mutex<Project> + history, rtrb rings)
 ┌───────────────────────────▼──────────────────────────────────────────────────┐
 │ crates/engine: model · ops · history · sequencer · instruments · built-in FX  │
 │                mixer graph · recording · offline render · analysis thread    │
 │ crates/audio-io: cpal (WASAPI / ASIO / CoreAudio / ALSA) duplex streams       │
 │ crates/vst3-host: VST3 effects as `Processor`s, scan in a child process      │
 └──────────────────────────────────────────────────────────────────────────────┘
```

Rules that keep it fast:

* **The audio callback never allocates, locks or does I/O.** Structure changes
  are built off-thread and swapped in through an `rtrb` ring; the old graph is
  sent back to be dropped off-thread. DSP state (reverb tails, delay lines) is
  carried across a swap by effect id.
* **Parameter moves do not rebuild anything.** They are a fixed-size message.
* **The UI never polls JSON at frame rate.** Meters, playhead, recording peaks
  and the analysis of *open* effect windows arrive as one binary frame on a
  Tauri `Channel` (≤60 Hz). FFTs run on an analysis thread in Rust, only for
  effects whose window is open (`watch_fx`).
* **The UI draws with canvas** for anything that moves (playlist clips,
  meters, visualisations) with a single `requestAnimationFrame` loop; static
  layers (grids, curves that depend only on parameters) are cached offscreen.

## Data model

`crates/engine/src/model.rs` is the source of truth; JSON is camelCase. A
project looks like:

```jsonc
{
  "version": 2, "bpm": 128, "nextId": 1200, "loop": null,          // or {"a":0,"b":16}
  "tracks":  [{"id":1,"name":"Drums","color":"#4f8ef7","muted":false,"solo":false,"arm":false,"vol":0.7937,"h":56,"insert":1}],
  "inserts": [{"id":0,"name":"Master","color":"#d5dae2","vol":0.7937,"pan":0,"muted":false,"solo":false,
               "fx":[{"id":900,"type":"limit","bypass":false,"preset":0,"params":{"gain":0,"ceil":-0.3}}]}],
  "channels":[{"id":1000,"name":"Kick","color":"#4f8ef7","kind":"inst","inst":"kick","vol":0.7937,"pan":0,
               "mute":false,"solo":false,"insert":null,"root":60},
              {"id":1004,"name":"Stab","color":"#47b8a8","kind":"sample","source":1001,"vol":0.7937,"pan":0,
               "mute":false,"solo":false,"insert":3,"root":60}],
  "patterns":[{"id":1,"name":"Beat","color":"#4f8ef7","len":4,"swing":0.5,
               "notes":[{"ch":1000,"step":0,"len":1},{"ch":1003,"step":0,"len":3,"p":33,"v":0.8}]}],
  "sources": [{"id":1001,"name":"Pad Loop","color":"#47b8a8","path":"…/pad.wav","dur":3.75,"sr":48000,"ch":2,"peakDb":-3.1}],
  "clips":   [{"id":5,"track":0,"start":0,"len":16,"offset":0,"kind":"pat","ref":1,"muted":false,"gain":0,
               "fadeIn":0,"fadeOut":0,"pan":0,"norm":false,"rev":false}]
}
```

## Channels, patterns and notes (the pattern editor)

Channels are **global to the project**, like FL Studio's Channel Rack: one row per
instrument, and every pattern can hold notes for every channel. A note names its
channel with `ch`. Why global and not per pattern: a beat is usually several patterns
(verse / chorus / fill) played by the same kit; per-pattern instruments would duplicate
the kit and its mixer routing in every pattern.

* `channel.kind`: `inst` (built-in `kick`/`clap`/`hat`/`bass`) or `sample` (a one-shot
  sampler playing project source `source`; the note's key repitches it, `root` = the key at
  which the file plays at its own pitch, default 60 = C5 in FL's naming / C4 in this app's
  labels). `vol` is a fader position (unity = 0.7937, gain normalised so unity is exactly
  1), `pan` −1..1 (balance law of the mixer), `mute`/`solo` (with any solo, only soloed
  channels play), `insert`: a mixer insert id, or `null` = **the insert of the playlist track
  the clip is on** (how every pattern note behaved before channels existed; the track's fader
  and mute still apply). A channel routed to an insert skips the track's insert but the
  track still mutes it and scales it by its fader.
* `note`: `step`/`len` in 16ths, `p` MIDI key (absent = the channel's `root`; percussion
  ignores pitch), `v` velocity 0..1 = linear gain (absent = 1). A step-sequencer step is
  simply a note of length 1 at the root, so the step grid and the piano roll edit the same
  notes; a channel whose notes have other lengths/pitches is shown as a mini piano roll in
  the rack and edited in the piano roll.
* `pattern.len` in beats (the editor offers 1–8 bars); notes at or past the end are **kept
  but hidden and silent** (shrinking and growing back loses nothing). When `len` changes,
  pattern clips that show exactly one whole pass (`offset` 0, `len` = the old length) take
  the new length; clips sized on purpose (repeats, offsets) keep theirs. New clips use the
  pattern's length.
* `pattern.swing` 0..1: a note that starts exactly on an odd 16th (0-based 1, 3, 5 …) is
  delayed by `swing × ½` step. 100 % puts the off-beat 16th at 75 % of its 8th (MPC 75 %),
  ~67 % is close to triplet feel; 0 is straight. Even 16ths and off-grid notes never move.
  Swing is applied when the timeline is built (schedule time), never written into the notes.

**Migration.** Old files (and old `wave apply` scripts) say `{"inst":"hat"}` on notes and
have no `channels`. `Project::migrate` (on open, and `putPattern` for each note) gives every
used instrument one built-in channel (in Kick, Clap, Hat, Bass order, unity, centre,
`insert: null`, bass root 36), so an old song renders identically
(`crates/engine/tests/patterns.rs` compares a pre-channels render of the demo). `inst` is
read, never written. A project without channels gets the four built-ins.

**Real time.** Channels are resolved off-thread into a table in the timeline (`ChanRt`:
voice kind, `Arc` of the sample, gain/pan/on, destination insert). Voices keep a channel
index and id; a new timeline re-maps voices by id (no allocation), and knob / mute / solo
moves go through the mixer fast path (`MixState.chans`) without a timeline rebuild, so
ringing notes follow them. Auditions (`note_audition`) and the pattern loop preview
(`pattern_play`) are commands to the audio thread, not edits.

**Silence skipping.** An insert whose input is exact digital silence for longer than its
chain's tail (each `Processor::tail_samples`, summed, plus the dry-path delays) is not
processed: its effects are settled once (`Processor::park`) and skipped until sound returns;
an effect with no known tail (`None`, e.g. hosted plugins) keeps its insert running. Tune,
Voice and Denoise idle after a second of silence themselves, so a skipped chain and a
processed one give the same output (`crates/engine/tests/silence_skip.rs`, below -120 dB).
Live keyboard events do not take the core lock: they read an atomic snapshot (device live,
target channel, armed insert; refreshed on every sync) and push into a dedicated ring that
the audio thread drains with the command ring. Offline renders may run the insert chains on
worker threads (`Engine::set_render_workers`), bit-identical to one thread.

Effect types: `eq` (7 `bands`), `comp`, `sat`, `delay`, `reverb`, `limit`,
`vst3` (`plugin: {format,path,cid,name,vendor,state}`). Parameter ids, ranges
and presets come from `fx_catalog` — the UI must not hard-code them.

## Ops (`crates/engine/src/ops.rs`)

All edits are "put" ops, applied atomically per batch:
`putClips{clips}`, `removeClips{ids}`, `putTrack{track}`, `putInsert{insert}`,
`removeInsert{id}`, `putFx{insertId,fx,index?}`, `removeFx{fxId}`,
`moveFx{fxId,to}`, `putPattern{pattern}`, `putChannel{channel,index?}`, `removeChannel{id}`
(also removes its notes in every pattern; `removeSource` removes sampler channels of that
source), `putSource{source}`,
`removeSource{id}`, `putAutomation{automation}`, `removeAutomation{id}`,
`putComp{comp}`, `removeComp{id}`, `putNotebook{notebook}`, `setBpm{bpm}`, `setLoop{loop}`.

Every effect has `mix` (0..1 dry/wet, default 1). Automation:
`project.automations[] = {id, name, color, target, points:[{t, v}]}`; a clip of
kind `auto` with `ref` = automation id plays it (`offset` in beats into it).
Targets: `{kind:"fx", fxId, param}` (catalog id, or `band:<i>:f|g|q` for EQ:
f log 20–20k, g −30..30 dB, q log 0.1–18), `{kind:"mix", fxId}`,
`{kind:"insertVol"|"insertPan", id}`, `{kind:"trackVol", id}`. `v` is 0..1;
log parameters are normalised logarithmically, vol is the fader position, pan
maps −1..1 to 0..1. While playing, the engine applies automation every block. New ids come from the
client: take them from `project.nextId` and increase locally (the engine bumps
`nextId` past any id it sees).

## Tauri commands

| command | args | returns |
| --- | --- | --- |
| `get_state` | – | `StateDto` |
| `apply_ops` | `{ops, label?, transient?}` | `{revision}`; error string on invalid batch (nothing applied) |
| `begin_gesture` / `end_gesture` | `{label?}` | – . Ops sent with `transient:true` between them become one undo step |
| `undo` / `redo` | – | `StateDto` |
| `transport_play` | `{fromBeat}` | – |
| `transport_stop` | – | – |
| `transport_set` | `{metro?, countIn?, rec?, monitor?}` | `TransportDto` |
| `telemetry_subscribe` | `{channel}` (Tauri `Channel<ArrayBuffer>`) | – |
| `watch_fx` | `{ids: number[]}` | – (effects whose windows are open; ≤ 8) |
| `fx_catalog` | – | `{types:{[type]:{name, params:[{id,label,min,max,def,unit,log}]}}, presets:{[type]:[{name, params?, bands?, beats?}]}}` |
| `import_audio` | `{paths, track?, beat?}` | `{sourceIds, clipIds}` (placed clips only when track/beat given) |
| `source_peaks` | `{id, norm?, rev?}` | `ArrayBuffer` of f32 `[min,max]` pairs, 100 per second of audio |
| `pitch_analyze` | `{clipId}` | `{rate, curve: (midi\|null)[], levels: number[], confidence: number[], notes: [{start,end,pitch,level}], backend: "rmvpe"\|"yin"}` — pitch every 1/rate s (null = unvoiced: breath, sibilant, silence); times in seconds of the source |
| `pitch_target` | `{clipId, reference?: RefTune}` | `PitchTarget` (below) — Tune to Reference for the clip, with `reference` or the clip's own `tune.reference` (preview before applying) |
| `f0_model_status` | – | `{id, name, bytes, installed, backend, licenseNote, licenseUrl, path, error}` |
| `f0_model_install` | – | – ; downloads RMVPE, verifies size and SHA-256, activates it; progress as event `model-progress {id,done,total,error?}` |
| `music_analyze` | `{clipId?, excludeFx?}` | `{tempo: {bpm, confidence, candidates:[{bpm,score}], firstBeat}\|null, key: {root, minor, confidence, candidates:[{root,minor,score}], chroma:number[12], tuningCents}\|null}` — tempo and key of a clip, or of the project's active audio clips (excluding tracks routed to `excludeFx`'s insert) |
| `clip_peaks` | `{clipId}` | like `source_peaks`, but of what the clip actually plays (norm/rev/pitch edits applied) |
| `trim_clip` | `{clipId}` | `{sourceId}` — writes a new file with only the clip region, clip now points at it |
| `flatten_comp` | `{clipId}` | `{sourceId}` — renders a comp clip to a new file; the clip becomes an `audio` clip, the takes go (one undo step) |
| `comp_audition` | `{compId, takeId?}` | – ; plays one whole take instead of the comp (`null` = the comp). Live only: not saved, not undone, ignored by renders |
| `note_audition` | `{ch, key?, vel?, pattern?}` | – ; plays one note of a channel now (step clicks, piano-roll keys). `pattern`: the edited pattern, so a track-routed channel is heard through that pattern's track. Not an edit |
| `pattern_play` | `{pattern}` (`null` stops) | – ; loops one pattern on its own. Starting it stops the song, starting the song stops it (FL's PAT / SONG). Position in telemetry `patBeat` |
| `preview_clip` / `preview_file` / `preview_stop` | `{clipId}` / `{path}` / – | – |
| `audio_hosts` | – | `[{id, name}]` (ids like `"wasapi"`, `"asio"`, `"alsa"`) |
| `audio_devices` | `{host}` | `{outputs: string[], inputs: string[]}` |
| `audio_get_config` / `audio_set_config` | `– / AudioConfig` | `AudioStatus` |
| `asio_control_panel` | – | – (opens the driver's own panel, Windows only) |
| `library_list` | – | `{kits:[{id,name,description,bytes,count,installed}], folders:string[]}` |
| `library_install` | `{id}` | – ; progress as event `library-progress {id,done,total,error?}` |
| `library_files` | `{kit?}` or `{folder?}` | `[{name, path, group}]` |
| `library_add_folder` / `library_remove_folder` | – (native dialog) / `{path}` | updated `folders` |
| `project_new` / `project_open` / `project_save` | – / `{path?}` / `{saveAs?}` | `StateDto` / `StateDto` / `{path}` |
| `export_audio` | `{useLoop, tail, sampleRate}` | `{out, fromBeat, toBeat, tail, stats}` or `{cancelled:true}`; native save dialog; offline master → 32-bit float stereo WAV |
| `export_cancel` | – | – (abort an in-flight `export_audio` before a file is written) |
| `vst3_scan` | `{rescan}` | `[{path,cid,name,vendor,category}]` |
| `vst3_editor` | `{fxId, open}` | – (native window) |
| `vst3_params` | `{fxId}` | `[{id,name,value,display,def}]` |
| `vst3_set_param` | `{fxId,id,value}` | – |
| `cli_status` | – | `{installed, command, path, note?}` |

Events: `state` (a `StateDto`, after every change from any origin: UI, CLI,
undo, import, recording, file open), `library-progress`, `model-progress`, `audio-status`.

```ts
type StateDto = { project: Project; revision: number; canUndo: boolean; canRedo: boolean;
                  transport: TransportDto; path: string | null; dirty: boolean;
                  audition: { [compId: string]: number } };   // comp → auditioned take
type TransportDto = { playing: boolean; rec: boolean; monitor: boolean; metro: boolean; countIn: boolean; startBeat: number };
type AudioConfig = { host: string; output: string | null; input: string | null; sampleRate: number | null; bufferSize: number | null };
type AudioStatus = AudioConfig & { running: boolean; actualRate: number; actualBuffer: number; latencyMs: number;
                                   inputChannels: number; error: string | null; note: string | null; asioCompiled: boolean };
```

## Telemetry frame (binary, little-endian, ≤ 60 Hz)

```
u32 magic = 0x54564157 ("WAVT")   u32 seq
f64 beat            // playhead in beats; negative during count-in
u8  flags           // bit0 playing, bit1 recording, bit2 previewing
u8  pad, u16 pad
f32 inLevel         // input peak since last frame (linear)
f32 previewPos      // seconds into the previewed file, -1 when not previewing
f32 cpu             // audio callback load 0..1
u32 nIns  × { u32 insertId, f32 peakL, f32 peakR }            // linear peaks since last frame
u32 nFx   × { u32 fxId, f32 inPeak, f32 outPeak, f32 grDb,     // only watched fx (every frame, 60 Hz)
              f32 meters[32],                                   // per-type live readings, see "Effect meters"
              f32 specIn[128], f32 specOut[128],               // dBFS, 128 log bands 20 Hz..20 kHz (reverb: specOut = wet only)
              f32 scopeIn[256], f32 scopeOut[256] }            // last samples, mono
u32 rec             // 0, or 1 followed by:
    u32 track, f64 clipStart, f32 seconds, u32 peakOffset, u32 nPeaks, f32 peaks[2*nPeaks]   // new [min,max] pairs at 100/s
f64 patBeat         // beats into the pattern the loop preview plays; −1 = stopped (decoders treat a missing field as −1)
```

## Performance budget

* Callback: < 30 % of the buffer period with 8 tracks × 3 built-in effects at 48 kHz / 128 frames.
* UI: playlist pan/zoom at 60 fps with 500 clips; 6 open effect windows (incl. 3 reverbs) at 60 fps.

## Effect meters (`meters[32]` in the telemetry frame)

| type | meters |
| --- | --- |
| `eq` | `[0..24]` current dynamic gain offset of band *k* in dB (0 for static bands) |
| `fet` | `[0]` gain reduction dB, `[1]` input peak dBFS (after the Input knob), `[2]` output peak dBFS |
| `deess` | `[0]` gain reduction dB, `[1]` sidechain (high band) level dB, `[2]` sibilance 0..1, `[3]` threshold dB |
| `denoise` | `[0..16]` learned noise floor dB per band, `[16..32]` gain applied per band dB (≤ 0); 16 log bands 50 Hz – 16 kHz. Total reduction in `grDb`. Adds `fft_size` (2048 at ≤ 64 kHz) samples of latency, compensated by PDC |
| `tune` | `[0]` detected input Hz (0 = unvoiced), `[1]` target note Hz, `[2]` output Hz, `[3]` confidence 0..1, `[4]` input offset from target in cents, `[5]` correction applied in cents |
| `voice` | `[0]` detected input Hz (0 = unvoiced), `[1]` output Hz, `[2]` confidence 0..1, `[3]` pitch shift applied in semitones. Same latency as `tune`, compensated by PDC; an effect's dry/wet (`fx.mix`) delays its dry path by the effect's latency, so blends line up |

## Take lanes and comping

Recording with a loop set cycles over it and makes **one take per pass**;
recording over an existing clip adds a take instead of overwriting it. The
takes and the comp built from them are a project-level take group:

```jsonc
"comps": [{ "id": 2000, "name": "Comp 1", "xfade": 0.01,             // default boundary crossfade, s
  "takes":   [{ "id": 2001, "name": "Take 1", "ref": 1000,            // source id
                "start": 0, "len": 16, "offset": 0 }],                // comp beats; offset = beats into the source
  "regions": [{ "take": 2001, "from": 0, "to": 4, "xfade": 0.02 }] }] // sorted, non-overlapping, gaps silent
"clips":   [{ "kind": "comp", "ref": 2000, "start": 32, "len": 16, "offset": 0, … }]
```

**Why a clip kind and not takes on the track.** A comp is played by an
ordinary clip of kind `comp`, exactly like an audio clip plays a source:
comp beat = `clip.offset + (timeline beat − clip.start)`. So moving, trimming,
splitting, muting, gain, pan and fades of the comp reuse the clip machinery
and the existing timeline build; the take group itself never stores timeline
positions. Copies of a comp clip play the same group (like pattern clips).
Projects without `comps` open unchanged (`#[serde(default)]`).

* Takes are never cut. `regions` choose which take plays where; the UI's swipe
  writes a range (replacing what overlapped, merging touching pieces of the
  same take: `Comp::write`, mirrored in `ui/src/lib/comp.ts`).
* Every region boundary is an **equal-power crossfade** (cos/sin) centred on
  the boundary; its length is the region's `xfade` (at its start) or the
  comp's default, clamped so both halves fit. A region followed by silence
  fades out with the same length.
* The comp is resolved off the audio thread into segments (`comp::build`)
  when the timeline is rebuilt; the callback only mixes them (no allocation).
  Offline render and "flatten" use the same code, so they are sample-identical.
* Landing a recording (`comp::land_takes`): over a comp clip → new takes of its
  comp; over plain audio clips → those become the first takes of a new comp;
  elsewhere one pass is a plain audio clip and several passes a new comp. The
  newest take plays over its range (a last pass cut short by Stop only where
  it has audio). A last pass shorter than one beat is dropped. The audio
  thread marks each loop wrap with a NaN frame in the input ring.
* Recordings stay sources (`"Take N"`) next to the project like any audio
  (`<project> Audio/`, or the scratch folder until the first save); a new file
  never overwrites an existing one (`Take 2 1234 (2).wav`).
* **Takes line up with what was heard.** Input captured at a frame was sung
  to playback heard *round trip + PDC* frames earlier: the device layer
  publishes the measured round trip (`Shared::io_latency`: output
  playback−callback + input callback−capture + the input ring backlog, from
  cpal timestamps; one buffer each way when a host reports none) and the
  engine adds `engine::playback_latency` (slowest non-master insert + master
  chain). `finalize_take` drops that many frames from the front; pass marks
  stay put, since they already sit on the heard loop boundary. The last
  *latency* of a take (still in flight at Stop) is not captured.
* **Punch-in**: turning `rec` on while playing (with an armed track) starts a
  take at the playhead (`Cmd::PunchIn`; the audio thread reports the exact beat
  in `Shared::rec_start`); with a loop set it records in passes like loop
  recording. Turning `rec` off punches out and keeps playing.
* The device records **one input channel** (`AudioConfig.inputChannel`,
  0-based, default the first) as mono in both sides, so a mic on input 1 of a
  stereo interface is not left-only. Offline renders drop the PDC delay, so an
  export starts on its first beat and keeps its last `PDC` frames.

## Lyrics notebook ("Notatnik", F8)

```jsonc
"notebook": { "pages": [{ "id": 3000, "name": "Zwrotka 1", "startBar": 1,
                          "text": "[Zwrotka]\nidę przez miasto nocą\n\nświatła mi nie pomogą" }] }
```

Text that belongs to the song, so it is **project data**, not UI state: saved with the
project, undone, visible to the CLI (`wave context` lists the pages, `--full` has the
text, `putNotebook` through `apply`). It never touches the audio graph (the op returns an
empty `Dirty`). Old files without `notebook` open with no pages (`#[serde(default)]`).

* **One line = one bar**, counted from the page's `startBar` (1-based, 4 beats per bar,
  like the playlist ruler). An empty line is a bar of rest and still counts; a line whose
  trimmed text is `[…]` is a header and takes no bar. `ui/src/lib/notebook.ts` holds the
  mapping (`lineBars`, `lineOfBar`, `barOfBeat`), the Polish syllable counter and the rhyme
  grouping (ending from the penultimate vowel — Polish stress — with `ó`/`u`, `rz`/`ż`,
  `ch`/`h` and final devoicing folded; an ending needs ≥ 2 lines to get a colour).
* `putNotebook` replaces all pages (tab order = array order); ids are unique, `startBar`
  1..9999, ≤ 64 pages, ≤ 100 kB of text per page.
* **Typing is coalesced in the UI** (`ui/src/state/notebook.ts`): the textarea's text is a
  pending draft sent as one `putNotebook` (one undo step) after 0.9 s without typing, or on
  blur, page switch, undo/redo, save, new/open. Ctrl+Z inside the editor flushes the draft
  and runs the project's undo, never the textarea's own.
* **Draft backup.** Until the text is in a saved file, a copy sits in `localStorage`
  (`wave.notebook.backup:<path>`), together with the hash of the notebook it was based on.
  When that project is loaded again and its notebook is still exactly that base (nothing
  newer was saved), the copy is put back as one undoable edit. Saving drops the copy.
* **The window** is project-level, not part of the effect-window layer: F5 and clicks on
  the playlist never hide it. Geometry, font size (Ctrl+wheel) and the rhyme toggle are
  per-user view settings in `localStorage`. While the transport plays, the bar under the
  playhead (from telemetry) is highlighted on the current page and kept centred, unless the
  user typed or scrolled in the last 2 s. Clicking a bar number moves the playhead there.
  Rendering: a transparent-text `<textarea>` over a mirror of the same text (same font,
  fixed line height, no soft wrap), so gutter, highlight and syllable column stay aligned.

## Pitch edits on audio clips

`clip.tune = { notes: [{start, end, shift, drift, formant, gain}], pencil: [[sec, midi], …] }`
(seconds into the source; `shift`/`formant` in cents; `drift` 0..1 flattens pitch
movement inside the note toward its centre; `gain` dB). Start from
`pitch_analyze` notes, edit, send with `putClips`. Rendering is offline and
latency-free; during a gesture the previous render keeps playing and the new
one is made at `end_gesture`.

### Tune to Reference (`crates/engine/src/reference.rs`)

`clip.tune.reference = {source, correction, vibrato, influence, octave: number|null, key, scale}`
(0..1 amounts; `octave` null = automatic) makes another source the clip's
melody. Offline, deterministic, and the same code for GUI, CLI and tests:

1. **Analysis** — pitch (`F0Estimator`: RMVPE through `ort` when its model is
   installed, else YIN; 200 frames/s, MIDI, NaN = unvoiced) and content
   (MFCC, 100 frames/s; the spectral envelope c1–c6, which survives another
   singer, octave and vowel colour) of take and reference.
2. **Alignment** — coarse subsequence DTW over the whole reference finds where
   the take is; a banded DTW refines it with pitch class and contour shape.
   If that scores low, a content-only path is tried too. A jump-and-stall of
   the path (an implausible tempo map) is bridged by a straight line and
   aligned again narrowly around it, kept only if the content there vouches
   for it.
3. **Confidence** per frame — plausible tempo map (a veto, lifted where the
   content matches far better than chance), content against chance, contour
   correlation, and a gate on the take being more than 7 semitones from the
   reference (exact octave slips of the detector excepted). The median over
   sung frames is the score; below 0.3 the reference is refused and nothing
   is corrected.
4. **Register** — the multiple of 12 bringing the reference closest to the take.
5. **Target** — the reference's notes (segmented on the take's time axis) give
   centres; the take's own notes give vibrato and transitions (movement around
   a drift curve; octave slips of the detector and transitions to neighbours
   excluded from the drift). `target = take + correction·gate(confidence)·(centre + vibrato·movement − take)`,
   the interval folded to ±6 semitones so a detector octave slip still moves by
   the right amount.
6. **Render** — the existing TD-PSOLA with a continuous ratio(t), smoothed
   ±17.5 ms; manual note edits apply on top; a clip whose correction is zero
   everywhere renders bit-identical to its source.

```ts
type PitchTarget = { rate: number; start: number;            // first analysis frame of the region
  sourceF0: (number|null)[]; referenceF0: (number|null)[]; targetF0: (number|null)[];
  alignmentConfidence: number[]; refTime: number[];          // seconds into the reference
  octave: number; notes: Note[]; score: number; reason: string | null };  // reason set = refused
```

The RMVPE model is not shipped: `f0_model_install` downloads it (URL,
size, SHA-256 and licence link are constants in `crates/rmvpe`) to
`<data>/Models/rmvpe.onnx`; `WAVE_RMVPE_MODEL` overrides the path.
`crates/tune-bench` measures the whole pipeline (see `docs/cli.md`).
