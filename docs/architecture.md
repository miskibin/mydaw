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
  "patterns":[{"id":1,"name":"Beat","color":"#4f8ef7","len":4,
               "notes":[{"inst":"kick","step":0,"len":1},{"inst":"bass","step":0,"len":3,"p":33}]}],
  "sources": [{"id":1001,"name":"Pad Loop","color":"#47b8a8","path":"…/pad.wav","dur":3.75,"sr":48000,"ch":2,"peakDb":-3.1}],
  "clips":   [{"id":5,"track":0,"start":0,"len":16,"offset":0,"kind":"pat","ref":1,"muted":false,"gain":0,
               "fadeIn":0,"fadeOut":0,"pan":0,"norm":false,"rev":false}]
}
```

Effect types: `eq` (7 `bands`), `comp`, `sat`, `delay`, `reverb`, `limit`,
`vst3` (`plugin: {format,path,cid,name,vendor,state}`). Parameter ids, ranges
and presets come from `fx_catalog` — the UI must not hard-code them.

## Ops (`crates/engine/src/ops.rs`)

All edits are "put" ops, applied atomically per batch:
`putClips{clips}`, `removeClips{ids}`, `putTrack{track}`, `putInsert{insert}`,
`removeInsert{id}`, `putFx{insertId,fx,index?}`, `removeFx{fxId}`,
`moveFx{fxId,to}`, `putPattern{pattern}`, `putSource{source}`,
`removeSource{id}`, `putAutomation{automation}`, `removeAutomation{id}`,
`setBpm{bpm}`, `setLoop{loop}`.

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
| `pitch_analyze` | `{clipId}` | `{rate, curve: (midi\|null)[], levels: number[], notes: [{start,end,pitch,level}]}` — pitch every 1/rate s (null = unvoiced: breath, sibilant, silence); times in seconds of the source |
| `clip_peaks` | `{clipId}` | like `source_peaks`, but of what the clip actually plays (norm/rev/pitch edits applied) |
| `trim_clip` | `{clipId}` | `{sourceId}` — writes a new file with only the clip region, clip now points at it |
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
| `vst3_scan` | `{rescan}` | `[{path,cid,name,vendor,category}]` |
| `vst3_editor` | `{fxId, open}` | – (native window) |
| `vst3_params` | `{fxId}` | `[{id,name,value,display,def}]` |
| `vst3_set_param` | `{fxId,id,value}` | – |
| `cli_status` | – | `{installed, command, path, note?}` |

Events: `state` (a `StateDto`, after every change from any origin: UI, CLI,
undo, import, recording, file open), `library-progress`, `audio-status`.

```ts
type StateDto = { project: Project; revision: number; canUndo: boolean; canRedo: boolean;
                  transport: TransportDto; path: string | null; dirty: boolean };
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

## Pitch edits on audio clips

`clip.tune = { notes: [{start, end, shift, drift, formant, gain}], pencil: [[sec, midi], …] }`
(seconds into the source; `shift`/`formant` in cents; `drift` 0..1 flattens pitch
movement inside the note toward its centre; `gain` dB). Start from
`pitch_analyze` notes, edit, send with `putClips`. Rendering is offline and
latency-free; during a gesture the previous render keeps playing and the new
one is made at `end_gesture`.
