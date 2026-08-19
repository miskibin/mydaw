# WAVE Studio — the shell

**Status:** implemented.
**Replaces:** WAVE Shell v2 (`2026-08-19-wave-shell-v2-ui.md`) — the panel dock,
the bottom panel and the single-pattern arrangement are gone.
**Design:** `.design/wave-studio.html`.
**Engine contracts this is built against:**
`2026-08-19-engine-patterns-playlist.md` §8–9,
`2026-08-19-mixer-v2-routing.md` §7,
`2026-08-19-param-system-effects-v1.md`.

This document is what the shell *is*, why it differs from the mock where it
does, and what it deliberately refuses to draw.

---

## 1. The shape of it

```text
┌──────────────────────────────────────────────────────────────────────────┐
│ TopBar   WAVE · transport · clock · BPM · PAT/SONG · pattern · views · ☀ │
├──────────┬────────────────────────────────────────────┬──────────────────┤
│ Browser  │  PLAYLIST | RACK | MIXER                   │ Picker           │
│ Sounds   │                                            │ Inspector        │
│ Plugins  │        (or the fullscreen PIANO ROLL,      │  instrument      │
│ Audio    │         which covers all three)            │  insert cards    │
│ assets   │                                            │  + effect chips  │
├──────────┴────────────────────────────────────────────┴──────────────────┤
│ StatusBar  device · buffer · latency · ⟨parameter readout⟩ · hint/error   │
└──────────────────────────────────────────────────────────────────────────┘
```

Three main views (F5 / F6 / F9) and a fullscreen piano roll (F7, Escape to
leave). Both side panes collapse to a rail.

Files:

```text
src/
├── api/            types.ts · backend.ts (interface + TauriBackend) · mock.ts
├── lib/            pattern.ts · param.ts · undo.ts · playhead.ts · keyboard.ts
├── ui/             Knob · Meter · NumberField · drag · frameLoop · icons
├── shell/          TopBar · PatternMenu · Browser · SidePanel · StatusBar · view.tsx
├── views/          Rack · Playlist · Mixer · Roll (+ roll/draw.ts) · InstrumentMenu
├── state.tsx       the IPC boundary: commands, gestures, undo, the readout
└── styles/         base.css + one sheet per region
```

`state.tsx` is the only file that talks to the engine; `shell/view.tsx` holds
what is *shown* and never crosses the wall. Nothing else imports
`@tauri-apps/api`, which is what keeps `pnpm dev` a fully usable standalone app
against `api/mock.ts`.

---

## 2. Mapping: every control and the command behind it

### Top bar

| Control | Engine |
| --- | --- |
| Play / Pause | `transport_play` / `transport_stop` |
| Stop | `transport_stop` + `transport_seek(0)` |
| Record | **disabled** — no capture path exists |
| Loop | **disabled** — no loop points exist |
| Clock, bar counter | `MeterFrame.samplePos` via `Playhead`, painted on the frame loop |
| BPM | `set_bpm` (rejects out of range; optimistic with rollback) |
| SIG `4/4` | **disabled readout** — the engine's `ticksPerBar` is 4/4 only |
| PAT / SONG | `set_play_mode` |
| Pattern selector | `set_active_pattern`; the menu also does `add_pattern`, `clone_pattern`, `rename_pattern`, `set_pattern_color`, `remove_pattern` |
| Project name → menu | `new_project`, `open_project`, `save_project`, `save_project(saveAs)` |
| Theme | local, persisted to `localStorage` |

### Channel rack

| Control | Engine |
| --- | --- |
| Pattern tabs (click / double-click) | `set_active_pattern` / `rename_pattern` |
| `16 / 32 / 64` | `set_pattern_length` |
| LED | `set_track_mute` |
| Channel name click | opens the hosted plugin's editor (`open_clap_gui` / `open_vst3_gui`) when it has one; otherwise selects |
| Caret → instrument menu | `set_track_source` / `set_track_source_clap` / `set_track_source_vst3` |
| Roll button | opens the piano roll (view state) |
| Step cell | `set_pattern` (whole-pattern replacement) |
| `steps` / `roll` | `set_pattern` — a real content conversion, see §4.2 |
| × | `remove_track` |
| Drop zone / `+ add a channel` | `add_track` (+ a source swap for a hosted plugin) |

### Piano roll

| Control | Engine |
| --- | --- |
| Draw / Paint / Select / Delete / Slice | `set_pattern` |
| Marquee, shift-click, Ctrl+A, Ctrl+D, Delete | `set_pattern` |
| Velocity lane | `set_pattern` |
| Key column | `note_on` / `note_off` (live, on the selected channel) |
| Ruler | `transport_seek` |
| Snap, zoom, ghost notes | view state |
| Quantize · Legato · Len ×2 · Len ÷2 · Chord · Humanize | client-side, then `set_pattern` |
| Pan / Mod X lanes | **disabled** — a `MelodicNote` has key, start, length, velocity and nothing else |

### Playlist

| Control | Engine |
| --- | --- |
| Paint / drag-drop a clip | `add_pattern_clip` / `add_audio_clip` |
| Move / resize | `move_clip` / `resize_clip` |
| Right-click, Delete tool | `remove_clip` |
| Double-click a pattern clip | `set_active_pattern` + go to the rack |
| Lane LED | `set_playlist_track_mute` |
| Lane name (double-click) | `rename_playlist_track` |
| Lane × | `remove_playlist_track` |
| `+ Add lane` | `add_playlist_track` |
| Ruler | `transport_seek` |
| Clip waveform | `get_audio_waveform` (fetched once per source, cached) |

### Mixer

| Control | Engine |
| --- | --- |
| Strip select | `set_selected_track` (tracks only) + view state |
| Mute / Solo | `set_track_mute` · `set_track_solo` · `set_bus_mute` · `set_bus_solo` |
| Fader | `set_track_gain` · `set_bus_gain` · `set_master_gain` |
| Pan | `set_track_pan` · `set_bus_pan` |
| `+ Bus` | `add_bus` |
| Insert slot: dot / click / right-click | `set_insert_bypass` / select + inspector / `remove_insert` |
| `+ effect` | `add_insert` |
| Send cell: click | `add_send` / `remove_send` |
| Send cell: drag | `set_send_level` (per frame), `get_tracks` on commit |
| PRE / post | `set_send_pre` |
| Record arm | **disabled** |
| LUFS | **replaced** by a real peak dBFS readout |

### Browser and side panel

| Control | Engine |
| --- | --- |
| Sounds tree | `get_sources`; `+` → `add_track`; click → `note_on`/`note_off` |
| Plugins tree | `get_clap_plugins` / `get_vst3_plugins` (lazy), Rescan → the same with `rescan` |
| Audio tab, `Load audio file…` | **`open_audio_file`** (new, §6) → `get_audio_sources` |
| Audio row × | `unload_audio_source` (refused while clips play it — the message is shown) |
| Asset row | `get_asset_status` / `start_asset_download` |
| Picker rows | set the brush; `+ New pattern` → `add_pattern` |
| Inspector insert cards | `set_insert_param` (per frame), `set_insert_bypass`, `move_insert`, `remove_insert` |
| Compressor gain reduction | `get_chain_readouts`, polled at ~15 Hz **only while that chain is on screen** |
| Hosted master effect | `get_master_effects` / `get_vst3_master_effects` / `set_master_effect_clap` / `set_master_effect_vst3` / `clear_master_effect` |

---

## 3. What is disabled, and why

Everything drawn is either wired to the engine or **visibly disabled with a
`title` naming the limit**. There is no third category. The complete list:

| Control | Why |
| --- | --- |
| Record button | The engine has no capture path. |
| Loop button | No loop points. PAT already loops the pattern; SONG runs to `endTicks`. |
| Time signature `4/4` | `ticksPerBar` is 3840 and nothing reads a signature. |
| Piano-roll **Pan** and **Mod X** lanes | `MelodicNote` carries only key/start/length/velocity. Velocity works. |
| Mixer **R** (record arm) | As above — no recording. |
| Effects in the browser's Plugins tree | A hosted effect can only go in the master insert; the tooltip points at the Inspector, which is where it is placed. |

And three things the mock draws that are **not drawn at all**, because a
disabled control is a promise and these are not owed:

| Dropped | Why |
| --- | --- |
| CPU meter | The engine publishes no load figure. A number invented in the webview is the most quietly dishonest thing a DAW can show. |
| Master **LUFS** | `MeterFrame` carries peak and RMS, no loudness. Replaced with a real peak dBFS readout with a peak hold. |
| Strip **stereo separation** slider | No strip has a width parameter. `Utility.width` is an insert, and it is available as one. |
| Playlist lane volume / solo / arm | A `PlaylistTrack` has a name, a mute and an order. Nothing else. The header shows the clip count instead. |
| Master mute | There is no `set_master_mute`. The fader reaches −60 dB. |
| **Presets** browser tab | There is no preset system. Replaced by the **Audio** tab, which is real. |
| In-app plugin windows | Plugin editors are native OS windows (WebView2 cannot composite a plugin's own toolkit). The mock's floating windows would have been a drawing of a feature that works differently. |

---

## 4. UX decisions

### 4.1 PAT / SONG is explicit, and view-independent

The mock implies the mode from the view: the rack plays the pattern, the
playlist plays the song. That is a hidden mode change, and the case it breaks is
the most ordinary one there is — opening the mixer while the arrangement plays
would have stopped the song and started looping a pattern.

So the mode is a visible toggle next to the transport, exactly where FL Studio
puts it, and **switching views never changes what plays**. The playlist's
playhead is drawn only in SONG mode, because in PAT mode it would be pointing at
a timeline the transport is not running.

### 4.2 The rack's `steps` / `roll` button is a conversion

The engine's compiler plays a channel's step grid **and** its notes
(`Pattern::compile`); `mode` is only *derived* from whether notes exist. A view
toggle would therefore have doubled every hit the moment a grid was "shown as"
notes.

So it converts, carrying as much as the other representation can hold, and the
parameter readout says what was lost:

- **steps → roll**: each switched-on cell becomes a sixteenth-long note at that
  cell's velocity, and the cells are cleared.
- **roll → steps**: each note lands on its nearest step at its own velocity, the
  row's key becomes the one most notes used, and the notes are cleared. Lossy,
  and the readout says so ("12 notes folded onto the grid, lengths dropped").

### 4.3 Step cells never stretch

A cell is always 13 × 22 px, grouped in fours by a beat gap and sixteens by a
bar gap, and the lane scrolls. A 64-step pattern stretched to fit a narrow
window is 64 slivers, and a grid you cannot hit is not a grid.

### 4.4 The parameter readout

Every continuous gesture writes `Thing · Parameter  value` into the status bar,
formatted in the parameter's own unit from its `ParamDef` — `245 Hz`, `−6.0 dB`,
`4.0:1`, `24 dB/oct`. It is the single answer to "what did that drag just do",
and it is why `Knob` is one component: a second knob would have been a second
opinion about what shift does.

### 4.5 One knob, laid out from metadata

`ui/Knob.tsx` renders any `ParamDefDto`: `log` curves taper (a pixel is a
constant musical interval), `db` is laid out linearly in dB (tapering decibels
again is the classic mistake), `step` quantises, shift is fine control live
mid-drag, and double-click restores the default. A parameter the engine declared
as a switch (`smoothingMs: 0` with a step) is drawn as a switch and cycled by
clicking — never animated.

**Switch position names are the UI's.** The engine declares a three-position
parameter and refuses to name the positions, because a name is a design decision
that would end up in a file format. `lib/param.ts` holds the table
(`effects.delay/division` → `1/1 … 1/64`) with an honest fallback (`Off`/`On`,
or the index) so a new effect renders sensibly the day it lands.

### 4.6 The canvas piano roll

One canvas, not a div per note: a pattern holds up to 1024 notes per channel and
a thousand absolutely-positioned elements is a layout pass per pointer move. It
also puts the keys, ruler, grid, notes, ghosts and velocity lane in one
coordinate system, so they cannot drift apart by a rounding.

It redraws **on change**, not on the frame tick; the frame loop only drives the
playhead. A canvas that repainted only on the next animation frame is a canvas
that never repaints in a window the compositor has parked.

Ghost notes are the other channels of the same pattern, faded — a pure render of
state already in hand, no extra fetch.

### 4.7 Note operations

Client-side on the selection (or on everything, when nothing is selected), then
one `set_pattern`:

| Op | What it does |
| --- | --- |
| Quantize | each start onto the snap grid |
| Legato | each note extended to the next start **on that channel**, selected or not — stopping at the selection would leave a gap |
| Len ×2 / ÷2 | scale lengths, floor of a 64th |
| Chord | adds +4 and +7 semitones. Deliberately **not** diatonic: the engine has no key signature, and a major triad is obviously a choice rather than a wrong analysis |
| Humanize | ±10 ticks, ±8 velocity, from a **seeded** PRNG so the same selection humanises the same way twice and undo/redo lands on the same notes |

### 4.8 Selection survives editing

`withNotes` deliberately does **not** re-order the note array. The roll
addresses the note it is dragging by index, and sorting on every edit would make
a marquee of forty notes point at different notes halfway through the drag.

---

## 5. Undo and redo

**The engine has no undo.** The stack here (`lib/undo.ts`, Ctrl+Z / Ctrl+Y,
depth 50) is a stack of *inverse commands* the shell replays.

| Covered | Not covered |
| --- | --- |
| steps, notes, every note operation | mixer: faders, pan, mute, solo, sends |
| clips: paint, move, resize, delete | insert chains and their parameters |
| lane mute and rename | patterns and lanes being created or destroyed |

**A gesture is one entry.** `startPatternGesture` snapshots, every `editPattern`
in between paints and pushes (debounced 50 ms), and `commitPatternGesture` turns
the pair into a single entry — so a forty-note drag is one Ctrl+Z, not four
hundred.

Two mechanisms, because the two kinds of edit differ:

- **Patterns**: a pattern is a few kilobytes and `set_pattern` is a whole-pattern
  replacement, so the inverse of *any* content edit is one command. No diff.
- **The arrangement**: clips are created by the engine, which allocates ids and
  never reuses them — an inverse addressing the old id would work once and then
  point at nothing. So a playlist entry stores the arrangement **by value** and
  `restorePlaylist` reconciles: remove surplus clips, fix the lane count, fix
  names and mutes, add what is missing. Undo → redo → undo therefore lands on
  the same arrangement every time even though every id underneath has changed.

**Structural edits clear the history**, and the status bar says so ("Undo
history cleared by removing Kick"). Removing a track renumbers every pattern's
channels; removing a pattern takes its clips; changing a pattern's length moves
its content. No entry below those could be replayed, and a half-applied undo is
worse than none.

A failed inverse also clears the stack: the engine is then in a state no entry
below describes.

---

## 6. The one engine change

`open_audio_file` — a `#[tauri::command]` that raises the native file dialog,
loads the chosen file and answers with the `AudioSourceDto`, or `null` when the
user cancelled.

It exists for the same reason the project dialogs do: the dialog plugin only
exists inside the app, so a webview raising one directly would be a blank screen
in a plain browser and the mock would have nothing to stand in for.
`load_audio_source(path)` is unchanged and is still the door for a test or a
headless run.

Its filter is `mydaw_audio_file::AUDIO_EXTENSIONS` — a new exported constant,
also used by the decoder crate's own round-trip test, so the dialog can never
offer a format this build has no decoder for. (MP3 is absent on purpose:
`symphonia-bundle-mp3` has no released 0.6.)

---

## 7. Known gaps

- **A mixer track cannot be renamed or coloured.** The engine has
  `rename_bus` / `set_bus_color` but no track equivalent; a track's name is set
  when it is created and thereafter follows its source. Buses get both.
- **Audio clips play into the master bus.** There is no per-lane routing, and a
  clip's tooltip says so rather than implying a fader somewhere affects it.
- **Mixer and parameter changes are not undoable** (§5).
- **Pattern miniatures in the picker are drawn for the active pattern only.**
  `PatternSummaryDto` carries counts, not content, and fetching every pattern to
  draw a 62-pixel box would be a lot of IPC for a thumbnail.
- **The playlist has no lane-height resize.** It is in the mock; it is pure
  chrome, and lanes are a fixed 48 px.
- **No CPU meter, no LUFS, no recording, no loop points, no time signature** —
  §3.

---

## 8. How this was verified

### Gates

```sh
pnpm --dir ui build                                    # tsc --noEmit + vite: clean
cargo build -p mydaw-app --features custom-protocol    # clean
cargo test -p mydaw-app --lib                          # 282 passed
cargo clippy -p mydaw-app -p mydaw-audio-file \
      -p mydaw-plugin-sequencer --all-targets -- -D warnings   # clean
```

### The IPC surface, checked mechanically

Every `invoke` in `backend.ts` was cross-checked against every
`#[tauri::command]` in `app/src/commands.rs` and the `generate_handler!` list:
**87 invocations, all matching a registered command argument for argument.**
The eleven registered commands the shell does not call are the seven dev-bench
ones, `debug_capture`, `get_insert_readouts` (superseded by
`get_chain_readouts`, which it does call) and the VST3 state pair.

### Audible, through the sound card

The commands the mixer and the playlist now issue are the ones these hardware
tests drive, so their numbers are this shell's evidence too.

`cargo test -p mydaw-app --test routing_audio -- --ignored` (Focusrite USB
Audio @ 44 100 Hz, max block 4096, **0 dropped samples** in every capture):

| Claim | Measured |
| --- | --- |
| A send reaches a bus and the bus's EQ colours it | shelf band **−42.74 → −29.13 dBFS = +13.60 dB**, and only +5.97 dB below the corner — a 7.63 dB difference that is the shelf |
| Pre-fader vs post-fader with the fader down | pre **−42.75 dB**, post **−200.00 dB** (silence) in the hat's band |
| A compressor reports gain reduction | crest **22.26 → 19.58 dB**; `gr` **14.10 dB**, `gr_max` 14.14 dB — the same readout the inspector's meter polls |
| EQ + compressor + delay on one bus | clean at the device's own 4096-frame block: 0 dropped, 0 non-finite, 0 clipped |

`cargo test -p mydaw-app --test audio_clip_audio -- --ignored`:

| Claim | Measured |
| --- | --- |
| An audio clip plays at the bar it was placed on | bar 3 is song sample 244 246; the device is playing source frame 40 707 where the file says 40 707 (r = 0.964) — **error +0 samples (+0.000 ms)** |
| …and not before it | hat band bars 0–3 **−51.95 dBFS**, bars 3–5 **−40.05 dBFS** (+11.90 dB) |
| Seeking into the middle of a clip | asked for source frame 54 884, got 54 884 (r = 1.000) — **error +0 samples** |
| A file at a foreign rate keeps its pitch | 440 Hz recorded at 32 kHz on a 44.1 kHz device → **440.00 Hz (+0.0 cents)** |

### The shell itself

Driven with real pointer and keyboard events against the built bundle
(`pnpm dev`, `MockBackend`), verifying among others: a step toggles and Ctrl+Z
turns it back off ("Undo Toggle step" in the readout); the roll draws a note
(`F4 · 001.2.2`, velocity 99) and reports it in the note fields; Chord adds two
notes and one Ctrl+Z removes both; a pattern clip paints onto a lane; `+ Bus`
then a click on a send cell routes `Kick → Bus 1` ("0.0 dB, post-fader"); an EQ
added from the inspector renders its 20 parameters as 14 knobs and 6 switches
with the engine's own labels ("24 dB/oct"); dragging a `Freq` knob moves it
**during** the drag and lands on 245 Hz with the readout to match; the light
theme swaps every token.

Three real defects were found and fixed this way: the frame loop swallowed
painter exceptions in silence, the roll only repainted on an animation frame
(so it never drew in a window the compositor had parked), and an insert knob did
not move under the pointer because it read the engine's copy, which is only
refetched on commit.

**Not verified: the real app's rendered window.** Screen-capture permission was
declined, so there are no screenshots of the Tauri build. What is known about it
is that it boots clean (device open, sampler loaded, zero warnings), that it
embeds this exact bundle, and that its IPC surface matches command for command.
