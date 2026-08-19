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
| Inspector insert card | `set_insert_param` (per frame, on its four curated controls) and `set_insert_bypass` |
| Insert card → popout → the full editor | `set_insert_param` for every parameter, plus `set_insert_bypass`, `move_insert`, `remove_insert` (§4.10) |
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

A cell is always 13 × 22 px and the lane scrolls. A 64-step pattern stretched to
fit a narrow window is 64 slivers, and a grid you cannot hit is not a grid.

**The beat is shown by colour and only the bar by a gap** — the first cell of
each beat takes a lighter unlit fill (`--stepbeat`) and the stronger border,
and cells sit 1.5 px apart inside a bar with 5 px between bars. That is the
mock's own lane, and the arithmetic falls out of it: a bar is 235.5 px, a
64-step lane is exactly 937 px, and a roll-mode preview is drawn at the same
width so the two kinds of row line up down the rack. The first cut spent 4 px on
every beat and 9 px on every bar, which pulled the lane into ragged clusters and
made every row a different width from the mock's.

### 4.3.1 Where in the loop we are

The rack draws the playing position the way FL does: a wash the width of one
cell over the step that is sounding, plus a hairline at the exact position.
**Two elements laid over the whole stack of rows, not a class per cell** — one
`translateX` a frame instead of one per row per frame, and the roll-mode
previews get the moving line for free because they share the lane's coordinate
system. The hairline interpolates between one cell's left edge and the next
rather than from a pixels-per-tick constant, so it stays inside the cell the
wash is on even across the wider bar gap.

It is on the frame loop only while the transport runs, and painted **once** more
when it stops — where you stopped is the thing you wanted to see, and a marker
that kept moving after the transport stopped would be the dishonest one.

What it claims to know differs by mode, because the truth does:

| Mode | Shown |
| --- | --- |
| PAT | Always. The transport is looping this pattern; the position inside the loop *is* the answer. |
| SONG | Only when **exactly one unmuted clip** of the active pattern is under the playhead — then there is one true answer. Nothing, otherwise. |

The active pattern can be on the timeline in a dozen places, in none, or twice
at once. A rack lit up from a clip the user is not looking at would be a lie
told sixty times a second, so in the ambiguous cases the rack stays dark.

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

**The wheel is the third gesture, and it obeys the same two rules.**

Every continuous control adjusts under the wheel: a fader by half a decibel a
notch, a pan by a hundredth of its width, a send by two per cent of the signal,
an insert parameter by a fiftieth of its range — normalised, so a `log` frequency
climbs the same musical interval at 100 Hz as at 8 kHz. Shift is fine control
here exactly as it is mid-drag, except where a fifth of a notch would round away
before the readout could show it; pan and the send levels take the smallest step
they actually have instead, because a modifier that changes nothing visible is a
modifier that looks broken. Switches step to the neighbouring position and stop
at the ends: a click has a *next*, but a wheel has a direction, and wrapping a
direction means rolling gently downwards lands on the top.

Only the **sign** of `deltaY` is read. The same flick of the same finger is 100
on a Windows mouse, three lines on a Linux one and a stream of 1.5s on a
trackpad, and multiplying a fader's gain by that number is how one gesture ends
at −60 dB. `deltaX` is the fallback, because Chrome reports a shift-held wheel as
horizontal and reading `deltaY` alone would leave fine adjustment dead. Every one
of these listeners is registered by hand as non-passive: a passive
`preventDefault()` is a silent no-op, and whether a framework's handler ends up
passive is the browser's decision, not the author's — not a thing to leave to
chance on a fader.

The value moves on every notch; only the whole-mixer refetch waits for the
gesture to go quiet, because a wheel has no release to commit on. And the mixer's
strip row scrolls itself sideways one strip per notch, since Chrome only turns a
vertical wheel into horizontal scrolling on macOS — it sees only the gaps between
controls, and declines the gesture entirely when there is nothing to scroll.

Three defects the wheel work turned up in the pointer half, all fixed: `startDrag`
multiplied the *whole* travel by the fine factor rather than the pixels walked
while shift was down, so pressing shift mid-drag yanked the value backwards; the
mixer's pan wrote through the engine with no local value, so notches arriving
faster than the round trip collapsed into one (five moved it by one); and the
send cell's click-to-route fired twice on a double-click, silently discarding the
send's level and its PRE flag.

**The send cell has no double-click reset**, and its tooltip no longer promises
one: its click *is* the route toggle, so the first half of a double-click removes
the send before there is anything left to reset. Unrouting and re-routing is the
reset — a new send starts at 0 dB, post-fader.

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

### 4.8 A pattern clip draws its pattern, and a lane can be resized

An audio clip has a waveform. A pattern clip used to be a coloured rectangle,
which says where a clip is and nothing about *which* one — so it now draws a
miniature of the pattern's real content: a mark per note, laid out by pitch
across a fixed `C2`–`B6` window so a bass line sits low in its clip and a lead
sits high, and a mark per switched-on step on its own thin row per channel, the
way a drum lane reads everywhere. Every mark is in its channel's own colour,
capped at 120 per clip, and skipped entirely below 22 px of clip where nothing
could be read anyway.

The window is fixed rather than fitted per pattern on purpose: a fitted range
would centre every clip's content and say nothing about register.

**Lane heights are draggable** — the grip is the bottom edge of the lane
*header*, not of the lane, because the lane is a drawing surface and a five-pixel
band of it that paints nothing when clicked would be a hole in the paint tool.
30–160 px, double-click for 48. Heights are keyed by lane **id**, not index: an
index would slide onto the neighbour the moment a lane below it was removed. And
because lanes no longer share one height, dragging a clip between lanes asks the
document which lane is under the pointer instead of dividing a delta by 48.

### 4.9 Selection survives editing

`withNotes` deliberately does **not** re-order the note array. The roll
addresses the note it is dragging by index, and sorting on every edit would make
a marquee of forty notes point at different notes halfway through the drag.

---

### 4.10 The inspector's cards are previews, and the editor is the effect

An EQ has twenty parameters. Drawn as twenty knobs in a 306 px column — which is
what the first cut did — the panel stops being an inspector and becomes a wall,
and the four other effects in the chain are pushed off the bottom of it. So a
card is a **preview**: the mock's 29 px head with the bypass dot, the name and an
accent popout glyph, over a body of at most four controls chosen for that effect.
The picks are in `lib/effectLayout.ts` with the reason for each written next to
it, taken from `2026-08-19-effects-ux-research.md`: threshold/ratio/attack/release
for the compressor, drive first for the saturator, and so on. An effect the table
has never heard of gets its first four parameters rather than nothing.

The EQ's card is the exception and draws **no knobs at all** — a magnitude curve
fills it, because four of an EQ's twenty parameters is an arbitrary quarter of an
EQ whereas the curve is the whole of it. The curve is the shell's own evaluation
of the RBJ sections the engine declares, transcribed from
`plugins/effects/src/biquad.rs`; `eq::magnitude_db_at` exists on the Rust side and
evaluates the very coefficients the audio path runs, but nothing on the command
surface returns it. So this is a drawing of the same formula and not a
measurement of the sound, and the tooltip on every curve says exactly that. It is
redrawn when a parameter changes and never on the frame tick.

The popout opens a **fullscreen editor**, a second layer over the three views
alongside the piano roll: back button, Escape, every parameter in grids grouped
by the prefix the engine puts on an id (`low.*`, `bell1.*`), 44 px knobs, the
EQ's curve large above its bands, and the effect's meters underneath. Bypass,
move in chain and remove live in its header rather than on the card, which is
what keeps the card the shape the mock draws — §2's table is split accordingly.
It sits *inside* the view row rather than
replacing it, because the inspector behind it is what polls `get_chain_readouts`
— unmounting it would have frozen the gain-reduction bar the editor is showing.
Opening it closes the roll and opening the roll closes it, so Escape is never
ambiguous about which layer it is leaving.

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

## 6. The engine changes

### 6.1 `open_audio_file`

A `#[tauri::command]` that raises the native file dialog,
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

### 6.2 `get_pattern_content(patternId)`

The content of one pattern, by id, or a sentence naming the id it does not have.

`SequencerStateDto` carries every pattern as a `PatternSummaryDto` — counts, no
content — plus the active pattern in full, which is everything the rack and the
roll need. §4.8's clip miniatures need more: most clips on a timeline are not the
pattern being edited. One pattern at a time, asked for by the view that wants it
and cached there, rather than fattening every sequencer reply with the content of
every pattern for the sake of a few boxes of marks — a pattern is a few
kilobytes, and the whole set on every step toggle would not be.

The cache is invalidated by the only two things that can make it stale: a pattern
that *was* the active one and has since stopped being it, and a summary whose
step count or event counts moved under it. Only the active pattern can be edited,
and the active pattern is never read from the cache.

### 6.3 A fix: `add_track` skipped its pattern sync on the hosted paths

`AppState::add_track` has three branches — VST3 host, CLAP host, plain — and
`sync_channel_added`'s own doc comment said it was "called after the rack grew,
whichever of the three paths did it". Only the plain one called it; the two
hosted branches returned early. So once *any* hosted plugin was placed anywhere
in the project — one CLAP instrument on one channel is enough — every channel
added afterwards got no grid in any pattern, and the rack drew it as a row with
the mode button and the delete cross and **no lane between them**. That is the
row the user screenshotted.

Both branches now sync, and a regression test
(`a_new_channel_gets_a_grid_in_every_pattern_and_not_only_the_active_one`) checks
the invariant across more than one pattern, which is where it fails invisibly:
the rack only draws the active one.

The shell was made to survive it regardless. `lib/pattern.ts`'s `stepCells` draws
a full lane for all three of the shapes a channel can arrive in — no clip, a clip
whose `steps` is `null` (`ChannelClip::melodic` makes one, and
`PatternSet::add_channel` will not retrofit a grid onto a clip that already
exists), or a row shorter than the pattern — and `withToggledStep` creates the row
the click landed on. A lane that is drawn has to be a lane that can be clicked.

---

## 7. Known gaps

- **A mixer track cannot be renamed or coloured.** The engine has
  `rename_bus` / `set_bus_color` but no track equivalent; a track's name is set
  when it is created and thereafter follows its source. Buses get both.
- **Audio clips play into the master bus.** There is no per-lane routing, and a
  clip's tooltip says so rather than implying a fader somewhere affects it.
- **Mixer and parameter changes are not undoable** (§5).
- **Pattern miniatures in the picker are still drawn for the active pattern
  only.** `get_pattern_content` (§6.2) now makes the other rows drawable and the
  playlist's clips use it; the picker has not been moved onto it yet.
- **A pattern summary's event count lags its content.** `set_pattern`'s echoed
  state is deliberately dropped (§5's note on the push being debounced), so the
  picker can read `0 events` for a pattern that has two until the next sequencer
  command refreshes the summaries. Pre-existing, and cosmetic.
- **No CPU meter, no LUFS, no recording, no loop points, no time signature** —
  §3.

---

## 8. How this was verified

### Gates

```sh
pnpm --dir ui build                                    # tsc --noEmit + vite: clean
cargo build -p mydaw-app --features custom-protocol    # clean
cargo test -p mydaw-app --lib                          # 283 passed
cargo clippy -p mydaw-app -p mydaw-audio-file \
      -p mydaw-plugin-sequencer --all-targets -- -D warnings   # clean
```

The 283rd is §6.3's regression test. The workspace suite was run as
`cargo test --workspace --exclude mydaw-app` plus `cargo test -p mydaw-app --lib`,
because a running `mydaw-app.exe` holds the link target and `--workspace` cannot
relink the binary while the app is open; the excluded crate's own tests are the
283 above. `mydaw-plugin-host-vst3`'s `hardware` suite failed once under the
parallel run and passes standalone (5 passed) — it opens real installed plugins,
which is not a thing two test binaries can do at the same time.

### The IPC surface, checked mechanically

Every `invoke` in `backend.ts` was cross-checked against every
`#[tauri::command]` in `app/src/commands.rs` and the `generate_handler!` list:
**88 invocations, all matching a registered command argument for argument**
(87 at first ship, plus §6.2's `get_pattern_content`).
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

### The polish pass

The same way, driven headlessly through CDP so the numbers are reproducible
(`scratchpad/rack-shots.mjs`), with the DOM read back after each gesture:

| Claim | Measured |
| --- | --- |
| §4.3's lane is the mock's lane | a 64-step lane is **937 px**, in-bar pitch 14.5, bar gap 5, last cell's right edge at 937 — the mock's own `laneStyle` width |
| A roll-mode preview lines up with a step lane | both **230.5 px wide at x = 394**, to the pixel |
| §4.3.1 follows the transport | at `001.3.1` the column sat at `translateX(262px)` = lane + `cellX(8)`, the hairline 9 px into that cell |
| …and holds where it stopped | pause left the column at the same `translateX(262px)`, visible |
| …and says nothing it cannot know | SONG mode with no clip of the active pattern under the playhead: `display: none`. With exactly one 4-bar clip under it at song bar 2.3.4: the 12th cell, which is where that clip's loop had reached |
| §4.8's clips draw their pattern | ten toggled steps across three channels became **ten marks** at `0 / 25 / 50 / 75 %` in each channel's own colour, and kept drawing after the pattern stopped being the active one (i.e. through `get_pattern_content`) |
| §4.8's lanes resize | dragging one grip 60 px took that lane **48 → 108 px** and left the other three at 48; the clip on it grew 39 → 101 px; double-click put it back to 48; the readout said `Track 1 · Lane height 108 px` |
| §6.3's three broken clip shapes | `stepCells` returns a full 16 cells for a channel with no clip, a clip with `steps: null`, and a one-cell row; `withToggledStep` creates the row and switches the clicked cell on in all three |

Console across the whole run: one 404 for a favicon `index.html` has never
declared, and nothing else.

Three real defects were found and fixed this way: the frame loop swallowed
painter exceptions in silence, the roll only repainted on an animation frame
(so it never drew in a window the compositor had parked), and an insert knob did
not move under the pointer because it read the engine's copy, which is only
refetched on commit.

The polish pass found two more of its own, both invisible to a type checker and
both caught by reading the DOM back rather than by looking at a picture: the
position overlays were revealed by clearing `style.display`, which falls straight
back to the stylesheet's own `display: none` — every transform was correct and
nothing was ever drawn; and the measured lane offset cached the zero it got when
the first paint landed before any row had rendered, pinning the playhead to the
left edge for the life of the view.

**Not verified: the real app's rendered window.** Screen-capture permission is
still not granted (the desktop build is not a Start-menu application, so it
cannot be added to the allowlist), so there are no screenshots of the Tauri
build. What is known about it is that it boots clean (device open, sampler
loaded, zero warnings), that it embeds this exact bundle, and that its IPC
surface matches command for command. §6.3 is therefore verified in two places
that do not need it: the engine half by `cargo test`, the shell half by calling
`lib/pattern.ts`'s own exports against the three broken clip shapes in the page.
