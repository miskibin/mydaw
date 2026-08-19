# Step 8 — Project save/load

**Status:** landed. **Scope:** a session, written to a file and read back — the
mixer, the pattern, the tempo, and every hosted plugin's own state — with a
policy for what happens when one of those plugins is not on the machine any
more.

Everything MYDAW has built so far is live state that dies with the process. This
step is the first one whose subject is what happens **after the application
exits**, which is also why its headline verification is two processes rather than
one (see [Hardware verification](#hardware-verification)).

---

## The shape of it

| Module | What |
| --- | --- |
| `app/src/project_file.rs` | The `.mydaw` format: the structs, the version gate, the atomic write. Knows nothing about a running session. |
| `app/src/project_host.rs` | Main thread. Gathers a session into a file; rebuilds a session from one. The only thing that talks to both plugin hosts. |
| `app/src/project_bridge.rs` | The `Send + Sync` mailbox a Tauri command posts to, plus the two things a project publishes: which file is open, and which of its plugins are missing. |
| `app/src/project.rs` | `MissingPlugins` — the board the mixer reads to draw a warning on a strip. |

The third mailbox, alongside `ClapHostBridge` and `Vst3HostBridge`, and for the
same reason: saving asks every hosted plugin for its state, loading destroys and
recreates all of them, and a hosted plugin is `!Send` and lives on the main
thread. It is a *third* bridge rather than two more commands on an existing one
because a load touches **both** plugin hosts, and neither existing bridge is
served with the other host in scope — the run loop is the only place that holds
both.

---

## The file format

JSON, pretty-printed, UTF-8, one trailing newline. `format_version` is the first
field.

### Why JSON

A project file is the user's document. It outlives the build that wrote it, it
lands in version control, and when something goes wrong with it the first useful
move is to *look at it*. A binary format would be smaller and would make every
one of those worse. The cost is real — a plugin state blob is base64, a third
larger than the bytes it carries — and it is worth paying, because the
alternative to "a user can read their own project" is "a user files a bug nobody
can reproduce".

### Top level

| Field | Type | Notes |
| --- | --- | --- |
| `format_version` | integer | **First field.** `1`. See [Migration policy](#migration-policy). |
| `bpm` | number | Session tempo. Validated against the transport's range on load. |
| `master_gain_db` | number | The master fader, in dB. |
| `selected_track` | integer | Which track the keyboard piano and the source picker address. Clamped to the track count on load. |
| `tracks` | array | One entry per track. **Order is data**: a track's position *is* its note channel. |
| `master_effect` | object or `null` | The one plugin on the master insert. |
| `pattern` | object | The step grid, in the same shape the webview exchanges — so there is exactly one pattern serialiser in the application rather than two that can drift. It is also the bulkiest part of the file: pretty-printed, a 4×16 grid is around 250 lines even when empty. Kept anyway, because a second encoding of the pattern is a much more expensive kind of ugly than a long file. |

### `tracks[]`

| Field | Type | Notes |
| --- | --- | --- |
| `id` | integer | Stable identity. Restored, not regenerated — a UI list keyed on it would otherwise look like every track being replaced. |
| `name` | string | |
| `source` | object | Tagged on `kind`; see below. |
| `gain_db` | number | Stored in dB, not as a multiplier: `-6.0` is exact and `0.5011872` is a rounding of it. |
| `pan` | number | `-1.0` hard left to `1.0` hard right. |
| `muted` | bool | The button, not the outcome. |
| `soloed` | bool | Solo's *effect* is derived on load, never stored — see [What is not stored](#what-is-not-stored). |
| `state_blob` | string | Base64 of whatever the hosted plugin wrote. Empty for a built-in source, and empty for a plugin with no state extension (a normal answer, not a failure). |

### `tracks[].source`

```json
{ "kind": "internal", "id": "drum-kit" }
{ "kind": "clap", "bundle_path": "C:\\CLAP\\Diva.clap", "plugin_id": "com.u-he.diva", "name": "Diva" }
{ "kind": "vst3", "module_path": "C:\\...\\Serum.vst3", "cid": "0123…", "name": "Serum" }
```

A hosted plugin is recorded by **path *and* id**, because neither alone is
enough: one bundle commonly exports several plugins (the workspace's own fixture
exports two), and the same plugin id can be installed in two places. `name` is
the display name from the scan that placed it — recorded so a strip for a
*missing* plugin can still be labelled with something a user recognises rather
than with a path they have to read.

`cid` is 32 hexadecimal digits, exactly as `get_vst3_plugins` reports it.

### `master_effect`

```json
{ "plugin": { "kind": "clap", … }, "state_blob": "…" }
```

Its own type rather than a `tracks[]` entry with the track fields left out: the
master insert has no fader of its own (that is `master_gain_db`) and no name.
`kind` is `clap` or `vst3` only — there is no built-in master effect to name.

### Byte-identical round trips

`serde_json` writes a struct's fields in declaration order, so the order in
`project_file.rs` **is** the order on disk. Nothing is re-derived on the way
through and every float round trips exactly, so *save → load → save* produces the
same bytes.

That property is not decoration. It is what lets a project file be diffed, and it
is what lets the round-trip test assert "nothing was lost" in one line instead of
field by field for ever. It is pinned by
`re_saving_a_loaded_project_produces_byte_identical_json` (unit) and by
`a_whole_session_survives_a_save_and_a_load_into_a_fresh_engine` (integration,
with a real plugin's real state blob in the file).

### What is not stored

- **Effective mute.** Whether a track is *audible* depends on whether any other
  track is soloed, so it is recomputed from the whole project on load rather than
  stored. Storing it would let a hand-edited file describe a mixer the engine
  could not produce.
- **The compiled pattern, the graph, the mixer atomics.** All projections of the
  above, rebuilt on load.
- **The plugin's editor position and open/closed state.** A window is not a
  session.

---

## Atomic write

A save must never be able to destroy the project it is replacing. Writing in
place cannot promise that: a full disk, a crash or a pulled cable halfway through
leaves a truncated file where a session used to be.

So `write_project_file`:

1. writes the bytes to a **sibling** file, `.<name>.<pid>.tmp`;
2. `sync_all()` — all the way to the device, because a rename that beats its own
   data to disk is exactly the corruption this exists to prevent;
3. `fs::rename` over the original, which is atomic on Windows (`MoveFileEx` with
   `MOVEFILE_REPLACE_EXISTING`) and on POSIX.

Either the old file is there or the new one is; there is no moment when neither
is. A *sibling* rather than `%TEMP%` because a rename is only atomic within one
volume and `%TEMP%` is routinely on another.

The sibling's name is deterministic so that a test can occupy it and make the
write fail on purpose — which is what
`a_failed_save_leaves_the_previous_project_exactly_as_it_was` does, then asserts
the original is unchanged byte for byte.

`ProjectFile::validate` runs before any of this and rejects a non-finite number.
`serde_json` writes NaN as `null` and `null` will not read back as a float, so a
save that let one through would produce a file MYDAW itself could not open.

---

## Migration policy

Two rules, and the second is why the first is safe.

1. **Unknown fields are ignored.** A file written by a later MYDAW that added,
   say, `tracks[].colour` still loads in this build, minus the colour. That is
   serde's default and it is deliberately not overridden.
2. **A newer `format_version` is refused by name.** Ignoring unknown fields is
   safe; *misreading* known ones is not, and a format that renumbered its tracks
   would do exactly that. `ProjectFileError::FutureVersion` says so in a sentence
   a status bar can show.

The version is probed *before* the rest of the file is parsed, so a project from
the future is refused with the version message rather than with whatever type
error its new shape happens to produce first.

### When to bump `format_version`

- **Bump** when the meaning of an existing field changes, when a field is removed
  or renamed, or when a new field is *required* for correct playback.
- **Do not bump** for a new optional field: rule 1 already handles it, and an
  older build opening the file loses only the new thing.

### Deliberately unforgiving cases

An unknown **built-in** source id (`{"kind":"internal","id":"wavetable"}`) and a
`cid` that is not 32 hexadecimal digits both refuse the whole file, naming the
track. These are not the missing-plugin case: a built-in is part of the build,
and a malformed class id is corruption rather than absence. Downgrading either to
a placeholder would mean a re-save silently rewrote the user's project into
something else.

---

## Missing-plugin policy

**A project file records a plugin by path and id, and both can stop resolving
without anything being wrong with the project.** The user opened it on their
laptop, uninstalled the plugin, or the vendor's updater moved the bundle. A DAW
that refused the whole session over one of those would be refusing to open a
user's work because of somebody else's software.

So, when an instance cannot be created:

- the track **loads**, headed by its recorded `Source::Clap` / `Source::Vst3`;
- it renders **silence** — `SilentNode`, the placeholder that has existed since
  step 6 — and not a fallback instrument, because a momentary gap is a far
  smaller lie than a wrong sound;
- a `MissingPluginDto` (name, format, path, the loader's own words) is published
  to `MissingPlugins` and reaches the mixer strip as a warning badge, and the
  status bar as a count;
- **the reference stays in the project**, so a save writes it back out unchanged
  — which is what makes the project still open correctly on the machine that does
  have the plugin.

The rest of the load is unaffected: other tracks, the beat, the tempo and every
fader come back exactly.

Two neighbouring cases are handled the same way and are *not* the same thing:

| Situation | Result |
| --- | --- |
| Plugin binary absent, or refuses to load | Silent placeholder + `missing` badge; reference kept |
| Plugin loads but **refuses its saved state** | Plugin is loaded and playable **at its defaults**, plus a warning sentence. Throwing the track away over a serialiser bug would be a far larger loss. |
| Plugin **refuses to save** its state | Track is written out with an empty blob, plus a warning. The rest of the project is saved. |
| No audio device at all | Nothing is hosted (a plugin is activated *for* an audio configuration); one warning says so. Not marked as missing, because the plugins are not. |

---

## Loading: four phases, and why the order is forced

The obvious implementation — walk the file and set each thing — does not work,
for the reason that has shaped every plugin-facing decision in this codebase: **a
hosted plugin cannot be in two graphs at once**, and the instances loaded now
have nothing to do with the ones the file asks for.

1. **Stop.** The transport stops; sequenced notes end with it.
2. **Strip.** Every placed instance is replaced by a built-in and destroyed, and
   the master insert is emptied. This is exactly what the mixer's own source
   picker does when a user switches a track off a plugin, so it is a path that is
   already proven — and afterwards *nothing is hosted*, which is what makes
   phase 3 an ordinary graph rebuild rather than the re-attach dance a layout
   edit needs.
3. **Rebuild.** The new project is installed and a graph compiled from it. A
   plugin-headed track compiles to `SilentNode`. **Every instrument node in the
   session is new, which is also what flushes a note the user was holding** —
   there is no old voice left anywhere to hang.
4. **Attach.** Each plugin the file names is created, activated, swapped into its
   slot, and given its saved state.

Only phase 4 can partially fail, and it is designed to.

The whole file is validated *before* phase 1, so a project the engine could not
run leaves the session exactly as it was rather than half replaced. Phases 1–3
are `ProjectHost::prepare` and phase 4 is `project_host::attach`; they are split
because the step *between* them is installing the graph, which is the audio
device's job and which an offline test has to do by hand.

### "New project" is a load

`ProjectHost::new_project` builds the default `Project` into a `ProjectFile` and
loads it. One code path, so "New" cannot drift away from "Open" in what it
resets — which is the bug every DAW with two of these has had at least once.

---

## The command surface

| Command | Args | Returns |
| --- | --- | --- |
| `get_project_info` | — | `{ path, name, missingPlugins, warnings }` |
| `new_project` | — | `ProjectInfo` |
| `open_project` | `path?` | `ProjectInfo`, or `null` if the dialog was cancelled |
| `save_project` | `saveAs`, `path?` | `ProjectInfo`, or `null` if cancelled |

**The native file dialog lives in Rust, not in the webview.** The UI's backend
abstraction exists so every panel can run against `MockBackend` in a plain
browser; a panel that reached for `@tauri-apps/api` directly would work in the app
and be a blank screen outside it. It also keeps "which extension is ours" in
exactly one place. `tauri-plugin-dialog` is granted only `dialog:allow-open` and
`dialog:allow-save` — the app never shows a message box, and a permission nothing
uses is a permission that can be misused.

Every command also takes an **optional explicit path**. With one, no dialog is
shown at all, which is what a headless run, a test and one day a command line
need — and what makes the dialog a convenience rather than the only door.

`PROJECT_TIMEOUT` is 60 s, an order of magnitude above the plugin bridges' 5 s,
because a load is not one plugin operation but all of them plus a graph rebuild.

---

## Tests

Everything below is in the normal `cargo test --workspace` run except the two
hardware sessions.

**`app/src/project_file.rs`** (18 tests) — the version is the first field; a
session round trips with every field intact; re-saving is byte-identical and
*stable*, not merely idempotent once; awkward floats survive exactly; Windows
paths survive their backslashes; a future version is refused by name even when
the rest of the file is unreadable; unknown fields are tolerated; empty,
truncated, half, non-JSON and wrong-typed files are reported rather than guessed
at; no tracks / too many tracks; every shape of bad track value names its track;
a non-finite value can never reach a file this build could not reopen; the
atomic write, a *failed* atomic write leaving the original byte-identical, and a
replace that does not leave a tail of the previous file behind.

**`app/src/project_host.rs`** (11 tests) — the mixer, pattern, tempo and
selection restore exactly, including solo resolved into the effective mutes the
audio thread reads; capture and load are inverses; a file the engine cannot run
leaves the session untouched; a project naming an absent plugin still loads and
keeps the reference; New is the default session through the same load; save and
open name the file everywhere the UI looks; a file that is not a project says
which file and why.

**`app/src/project_bridge.rs`** (7 tests) and **`app/src/project.rs`** (2 new) —
the mailbox, ordering, the headless fast-fail, and the missing-plugin board
reaching the strip that asked for it.

**`app/tests/project_persistence.rs`** (4 tests) — the real fixture CLAP plugin
through the real graph:

- a whole session (two built-in tracks, one hosted synth with a non-default
  state, a beat, 147 bpm, faders, pans, mutes, a solo) saved, loaded into a
  *fresh* engine, every field asserted, then re-saved **byte for byte**;
- the plugin's state comes back **as a sound**: the fixture's state is a detune
  in semitones, so the reloaded session plays C5 where a default one plays C4,
  and nothing told it to except the file;
- a project whose plugin is gone still opens, that track is silent and marked,
  the beat and the mixer are intact, the reference survives a re-save, and
  putting the bundle back brings the plugin *and its saved state*;
- loading over a live session destroys both previous instances, rebuilds, plays,
  and leaves the `basedrop` collector holding nothing.

> A note for anyone extending that file: Windows will not delete a mapped DLL,
> and its loader resolves an already-loaded module by **base name**. A bundle any
> test in the binary has opened neither goes away nor fails to load from a path
> that no longer exists. The missing-plugin test therefore points at a copy under
> a name this process has never opened.

### Hardware verification

`app/tests/project_hardware.rs` — two `#[ignore]`d tests, run as two `cargo test`
invocations and therefore **two operating system processes**, which is the only
way to observe what survives an exit.

```sh
cargo test -p mydaw-app --test project_hardware -- --ignored --nocapture --exact \
    session_a_authors_a_project_measures_it_and_saves
cargo test -p mydaw-app --test project_hardware -- --ignored --nocapture --exact \
    session_b_reopens_the_project_in_a_fresh_process_and_measures_the_same_thing
```

**A** boots the shell, programs a kick on every beat at 130 bpm, places the
fixture synth on track 1, loads a +12 semitone state into it, moves a fader,
holds C4, captures 2 s through the real device, and saves the project.
**B** boots a completely fresh process — new kernel, new device, no plugin
anywhere, default session — opens the file through the same command a user's Open
menu sends, holds the same key, and captures the same thing.

Measured on a Focusrite USB interface at 44.1 kHz:

| | A (authored) | B (reopened) | tolerance |
| --- | ---: | ---: | --- |
| peak | −7.06 dBFS | −6.98 dBFS | ±0.5 dB |
| rms | −16.74 dBFS | −16.75 dBFS | ±1.5 dB |
| onsets | 4 | 4 | ±1 |
| onset interval | 461.5 ms | 461.6 ms | ±5 ms (a beat at 130 bpm is 461.5) |
| beat grid, worst error | 0.04 ms | 0.09 ms | < 15 ms |
| dominant partial | 523.2 Hz (−13.9 dB) | 523.2 Hz (−13.9 dB) | ±1 FFT bin (1.35 Hz) |
| kick band | −28.7 dB | −25.3 dB | reported, not compared |

B additionally asserts that C5 **is** sounding and C4 **is not** — because a
session that loaded the plugin and threw its settings away would pass every other
comparison in the file.

The kick band is reported rather than compared for a measurable reason: it is
summed energy over one FFT, and an FFT covers the first 32768 frames — 0.74 s,
which at this tempo is a beat and a half. The number therefore says "one kick fell
in the window" or "two did", a ~3 dB difference that is a property of *when the
tap armed* and not of the session. Where the beats are is asserted where it is
actually knowable: the onset grid, which spans the whole capture.

Both sessions leave a WAV, an annotated PNG and a stats JSON under
`.debug/captures/<timestamp>/` and print the paths.

---

## Limitations, and what they are waiting for

- **One pattern.** The sequencer holds a single 16-step grid, so a project does
  too. Multiple patterns and an arrangement are a sequencer change; the file
  gains a `patterns` array when it does, which is a
  [non-bumping](#when-to-bump-format_version) addition only if the single
  `pattern` field is kept as the first one.
- **No undo history.** Nothing is stored about how the session got where it is.
  Undo is its own step and will not live in the project file.
- **No autosave, and no crash recovery.** A save happens when the user asks. The
  atomic write means a crash cannot corrupt the last save; it does not mean the
  work since it survives.
- **No dirty tracking.** The title shows the project name, not a modified marker.
  Doing it honestly means knowing when a *plugin's* state changed, which needs a
  host-side notification neither format gives cheaply — and a marker that lies is
  worse than none.
- **Paths are absolute.** Moving a project and its plugins to another machine
  works when the plugins are installed in the usual places, and produces
  missing-plugin warnings otherwise. A search path, or paths relative to the
  project, is a later refinement — the format can gain one without a version bump.
- **The plugin editor's window is not part of the session.** Position, size and
  whether it was open are all lost on reload.
- **No per-track effect inserts yet**, so the file has no place for them. The
  graph already carries a per-track insert chain; when the mixer exposes it,
  `tracks[].inserts` is an additive change.
