# WAVE Shell v2 — UI redesign

**Status:** implemented
**Design source:** `.design/wave-shell-v2.html` (a dc-runtime React mock)
**Scope:** `ui/` in full, plus one new Tauri command (`get_stream_info`)

The old shell — a transport bar, a left dock of cards and a tabbed workspace —
is gone. In its place is the layout the mock describes: transport across the
top, a collapsible browser on the left, an arrangement above a two-tab bottom
panel in the middle, a collapsible inspector on the right, and a status bar
underneath.

The rule this document exists to record is: **everything drawn is either wired
to the engine or visibly disabled.** The mock shows a DAW that is further along
than MYDAW is, and rather than fake the difference the shell renders the missing
parts at half opacity with a tooltip saying what is coming. A control that looks
live is live.

---

## 1. Design → implementation map

| Mock element | Implementation | Wired to |
| --- | --- | --- |
| Shell frame, `--bg/--txt`, Geist | `ui/src/App.tsx`, `ui/src/styles/wave.css` | — |
| `DARK` / `LIGHT` palettes, `acc()`, `alpha()`, `mix()` | `ui/src/theme/tokens.ts` | — |
| Theme + accent state, CSS custom properties | `ui/src/theme/ThemeProvider.tsx` | `localStorage` |
| Every SVG icon (paths copied verbatim) | `ui/src/ui/icons.tsx` | — |
| Transport bar | `ui/src/shell/TransportBar.tsx` | see below |
| — play / pause | play button | `transport_play` / `transport_stop` |
| — stop | square button | `transport_stop` |
| — `01:23:45` timecode | `mm:ss.mmm`, painted on the frame loop | `subscribe_meters` → `Playhead` |
| — `045.3.00` bars/beats | `bar.beat.hundredths` | same position + `bpm` |
| — `128.00 BPM` | `ui/src/ui/NumberField.tsx`: drag **and** double-click-to-type | `set_bpm` |
| — `4/4 SIG` | static label (the engine is fixed at 4/4) | truthful, not a control |
| — `PEAK −1.2` | max of `peakL`/`peakR` as dBFS | `subscribe_meters` |
| — theme toggle | sun/moon button | `localStorage` |
| Browser pane, 250 px / 32 px rail | `ui/src/shell/BrowserPane.tsx` | — |
| — search field | client-side filter over the visible list | — |
| — Sounds tab | internal instruments (and any future sampler kinds) | `get_sources` → `set_track_source` |
| — Devices tab | CLAP + VST3, grouped by what the engine can do with them, format badges, rescan | `get_clap_plugins`, `get_vst3_plugins`, `set_track_source_clap/_vst3`, `set_master_effect_clap/_vst3` |
| — Projects tab | New / Open / Save / Save As | `new_project`, `open_project`, `save_project` |
| — preview footer | the selected track's current source + the device rate | `get_tracks`, `get_stream_info` |
| Arrangement header | `ui/src/shell/Arrangement.tsx` | — |
| — track rows (colour bar, name, device, S, M) | real rows | `get_tracks`, `set_selected_track`, `set_track_mute`, `set_track_solo` |
| — `+ Track` (not in the mock) | added: the engine has `add_track` and the mock has no affordance for it | `add_track` |
| — clip lanes | rendered empty and disabled | — |
| — playhead over the lanes | **real**, runs across one time round the loop | `subscribe_meters` → `Playhead` |
| Bottom panel, 300 px / F7 maximise | `ui/src/shell/BottomPanel.tsx` | — |
| — MIXER tab | `ui/src/panels/mixer/MixerView.tsx` | — |
| — strip fader | pointer drag, `scale.ts` dB curve reused unchanged | `set_track_gain` |
| — strip pan knob | pointer drag, double-click centres | `set_track_pan` |
| — strip meter | DOM fill painted on the frame loop | `subscribe_meters` (`tracks[]`) |
| — strip mute, selection | | `set_track_mute`, `set_selected_track` |
| — master strip | fader + **two** meters | `set_master_gain`, `subscribe_meters` |
| — EDITOR tab, Notes/Steps toggle | `ui/src/panels/editor/` | — |
| — Steps | `StepGrid.tsx`; click toggles, shift-click accents, current step lit | `set_pattern` (debounced) |
| — Notes | `RollView.tsx`; the previous shell's canvas renderer, re-skinned | `set_pattern` (debounced) |
| — pattern length | select in the tab bar | `set_pattern` |
| Inspector, 330 px / 32 px rail | `ui/src/shell/Inspector.tsx` | — |
| — selection header | name / kind / colour | `get_tracks` |
| — INSTRUMENT card + pop-out | opens the plugin's **native** window | `open_clap_gui` / `open_vst3_gui` |
| — missing-plugin badge | from `TrackDto.missing` / `MasterDto.missing` | `get_tracks` |
| — MASTER INSERT card (master selected) | name, format, pop-out, replace, remove | `set_master_effect_*`, `clear_master_effect` |
| — `+ Device ⌘K` | `ui/src/shell/DevicePalette.tsx` (Ctrl+K) | `set_master_effect_clap/_vst3` |
| Status bar | `ui/src/shell/StatusBar.tsx` | — |
| — BUFFER / LATENCY / RATE / DEVICE | real, from the open stream | **new** `get_stream_info` |
| — in-app floating plugin windows | **not built** — see §4 | — |

## 2. What is disabled, and why

Each of these is drawn at 0.5 opacity with `cursor: default` and a tooltip.

| Control | Tooltip | Why |
| --- | --- | --- |
| Rewind | "Return to start — coming soon" | no seek command; `transport_stop` is the only rewind |
| Record | "Record — coming soon" | no recording path at all |
| Loop | "Loop — coming soon" | the pattern always loops; there is no loop *region* to toggle |
| KEY readout | "Key detection — coming soon" | nothing analyses key |
| LUFS (transport and master strip) | "Loudness metering — coming soon" | the meter stream carries peak and RMS, not loudness |
| Avatar chip | "Accounts — coming soon" | no account system |
| Arrangement tools (select/draw/cut) | "Editing tools — coming soon" | nothing to edit in the lanes yet |
| Snap chip | "Arrangement snap — coming soon" | ditto (the *roll* has a real snap control) |
| Section strip | "Song sections — coming soon. These are the pattern's bars." | no song structure; the chips show the pattern's real bars rather than invented section names |
| Clip lanes | "Clips and the arrangement timeline — coming soon" | there is no arrangement timeline, only a looping pattern |
| Per-track record arm | "Record arm — coming soon" | no recording |
| Per-track insert `+` | "Per-track inserts — coming soon. Master insert only for now." | the engine holds one effect, on the master |
| Inspector TRACK EFFECTS / MASTER CHAIN card | "Effect chains — coming soon" | as above |
| Inspector SENDS TO RETURNS card | "Sends and return buses — coming soon" | no return buses |
| `+ Layer` | "Instrument layers — coming soon" | one instrument per track |
| Autosave | "Autosave — coming soon" | reads `AUTOSAVE off`, which is true |
| Group / ⌘G, multi-select | **removed entirely** | there is no bus or group concept to model; a disabled control implying one would be a lie about the data model rather than about a feature |

Two mock features were dropped rather than disabled, because they describe
*data* MYDAW does not have and never pretended to: track groups/buses, and
return tracks. Everything else survives as a disabled control.

## 3. Theming architecture

```
theme/tokens.ts        DARK/LIGHT tables, mix/alpha/accentFor, cssVariables()
        │              (Solid-free: also the canvas renderer's colour source)
        ▼
theme/ThemeProvider    writes --bg … --acc onto document.documentElement,
                       sets data-theme, persists to localStorage
        ▼
styles/wave.css        every component styled from var(--…); only two
                       non-token values (--playhead, --palette-shadow) are
                       keyed on [data-theme]
```

Three consequences worth stating:

- **A theme swap is one write, not a re-render.** Custom properties on `:root`
  cascade to every element; no component subscribes to the theme. That matters
  because the mixer's meters are repainted 60 times a second and must not be in
  the theme's dependency graph.
- **The accent is darkened in light mode** (`mix(accent, '#0b1c3d', 0.28)`),
  copied from the mock's `acc()`. One accent value for both themes gives you
  unreadable 9 px labels on a white panel.
- **The canvas cannot inherit custom properties**, so `RollView.rollColors()` is
  the one mapping from the palette to the roll's colours. The previous shell
  kept a parallel set of `--roll-*` CSS variables; that duplication is gone.

Track colours are assigned by the shell from a fixed ramp keyed on `TrackDto.id`
(stable across removals), because the engine has no opinion about what colour a
track is.

## 4. Deliberate deviations from the mock

1. **No in-app floating plugin windows.** The mock draws EQ curves and knobs in
   draggable panes. A hosted plugin's editor is a native OS window drawn by the
   plugin's own toolkit into an HWND — it cannot be composited into WebView2.
   The pop-out buttons ask Rust to open the real window instead.
2. **No clips.** See §2.
3. **No groups or returns.** See §2.
4. **The piano roll is a canvas, not DOM.** The mock's roll is 24 absolutely
   positioned divs. A four-bar roll over 128 keys is tens of thousands of
   elements; `panels/editor/draw.ts` (carried over unchanged from the previous
   shell) draws it instead. It gains a thin toolbar the mock does not have —
   snap, zoom, note count, clear — because those have nowhere else to live.
5. **The clock reads `mm:ss.mmm`**, not the mock's `01:23:45`. That is the
   precision the engine reports.
6. **`+ Track` and a master Remove/Replace** were added: the commands exist and
   the mock, being a picture of a finished project, has no affordance for
   creating one.
7. **CPU% is absent from the status bar.** MYDAW has no DSP load figure;
   DEVICE / RATE took its place.
8. **`Ctrl+K`, not `⌘K`.** This is a Windows app.

## 5. Dependencies added

Two, both fonts. No component library was added.

| Package | Why |
| --- | --- |
| `@fontsource-variable/geist` | The design is specified in Geist. The mock loads it from Google Fonts, which cannot be used here: the app must work offline, and a remote font in a Tauri webview is a CSP hole for the sake of a typeface. The variable build is one 29 kB woff2 for the Latin subset. |
| `@fontsource-variable/geist-mono` | Same, for the monospaced readouts, which are most of the design's numbers. |

Considered and rejected:

- **corvu** (dialog/popover primitives) — the only floating surface is the device
  palette, which is one absolutely positioned div with Escape and arrow-key
  handling. A primitive library for one popover is more API than code.
- **@tanstack/solid-virtual** — the device lists are the only long ones, and a
  large collection is a few hundred rows of two spans each. Virtualisation is
  worth reaching for if that ever becomes a real complaint; it is not one now.
- **Any styled component kit** — the design tokens are the design; a kit with its
  own theme would have to be fought.

## 6. Reactivity notes

- Meter frames still never touch a Solid signal. They land in `MeterBus`, and
  `ui/src/ui/frameLoop.ts` runs **one** `requestAnimationFrame` loop for the
  whole shell; the clock, the peak readout, every meter fill and both playheads
  paint by writing to the DOM directly. The loop does not run when nothing is
  subscribed.
- The step highlight is computed on the frame but *published* only when the step
  number changes — sixteen times a bar rather than sixty times a second.
- Track rows, mixer strips and step cells use `<Index>`, not `<For>`. Every mixer
  command answers with a whole new `MixerState`, so a keyed `For` rebuilds every
  strip on every pointer move of a fader drag. `Index` keys on position, which is
  what a strip's identity actually is.
- Drags listen on `window` rather than using `setPointerCapture` on the control,
  because the control can be re-rendered mid-gesture.

## 7. Backend change

One new command, because the status bar's BUFFER and LATENCY had nothing real to
read:

```rust
#[tauri::command]
pub fn get_stream_info(state: State<'_, AppState>) -> StreamInfoDto
```

`StreamInfoDto` (in `app/src/dto.rs`) carries the device name, sample rate,
channel count, block size and the derived latency in milliseconds, and answers
with `running: false` rather than an error when no stream is open — "no device
yet" is a state a status bar has to draw.

## 8. The downloadable-asset row

`AssetStatus` in `ui/src/api/types.ts` and the two optional `Backend` methods
(`getAssetStatus`, `startAssetDownload`) are the UI's half of a contract whose
Rust side is being written on another branch. `MockBackend` implements them, so
the row — states, progress bar, failure text — is real and reviewable in
`pnpm dev`. `TauriBackend` leaves them undefined and the browser hides the row
rather than invoking a command that is not registered. Landing the sampler
branch means implementing two methods on `TauriBackend`; nothing in the
component changes.

## 9. Removed

- `panels/registry.ts`, `PanelFrame`, `UnknownPanel`, `panels/types.ts` — the
  dock they served no longer exists. Plugin-contributed panels do not currently
  have a home in the new shell; when they need one it should be a deliberate
  design decision rather than a card list.
- `panels/kernel-status`, `panels/clap-browser`, `panels/vst3-browser`,
  `panels/meters`, `panels/sequencer`, `panels/mixer/MixerPanel` — replaced by
  the browser, the mixer view and the status bar.
- `dev/IpcBenchPanel.tsx` — the manual bench panel had no host. The **automatic**
  matrix (`MYDAW_IPC_BENCH=1`) still runs: it is driven from `state.tsx` and
  imports `dev/ipcBench.ts` lazily, untouched.
- `styles/theme.css`, `styles/app.css` — replaced by `styles/wave.css`.

Kept unchanged: `lib/playhead.ts`, `lib/pattern.ts`, `lib/keyboard.ts`,
`panels/mixer/scale.ts`, `panels/editor/draw.ts` (moved from `panels/piano-roll/`).
