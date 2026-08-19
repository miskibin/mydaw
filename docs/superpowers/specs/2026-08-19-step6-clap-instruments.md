# Step 6 — CLAP instruments as first-class track sources

**Date:** 2026-08-19
**Status:** implemented
**Predecessors:** [spike 2 — CLAP hosting](2026-08-18-spike2-clap-processing.md),
[spike 3 — plugin GUI](2026-08-18-spike3-plugin-gui.md),
[step 5 — mixer and multi-track](2026-08-19-step5-mixer-multitrack.md)

---

## What changed, in one sentence

Hosted CLAP plugins stopped being **one plugin named by an environment variable** and became
**as many as the project wants, chosen from a picker, placed on tracks and on the master
bus, each with its own editor and its own persistable state** — which also meant deleting
step 5's refusal to change the track layout while one was loaded.

## Why this step, and why now

Steps 2 and 3 proved MYDAW could run *a* CLAP plugin and show *an* editor. Step 5 built the
mixer that plugins belong in, and immediately hit the wall the single-instance model put
there: `MYDAW_CLAP_INSTRUMENT` could take over track 0 and nothing else, `MYDAW_CLAP_FX`
could sit on the master and nothing else, and adding a track while either was loaded was
refused outright (`TrackError::ClapInGraph`). That is not a DAW; it is a demo with a
plugin in it.

Everything after this step also needs the pieces this one lays: a project file (step 8) has
to record *which* plugin is on *which* track and restore its state; automation has to
address a specific instance; per-track effect inserts reuse the same placement machinery.

---

## 1. Instances: `ClapInstanceHost`

`plugins/host-clap/src/instances.rs`. One main-thread owner for every hosted plugin.

```text
 main thread (tao's loop)
 ------------------------
 ClapInstanceHost
   ├── #1 LoadedClapPlugin  ──create_node──▶  ClapNode  ─┐
   │      └── ClapGuiSession ──▶ native window           │  audio thread
   ├── #2 LoadedClapPlugin  ──create_node──▶  ClapNode  ─┤  (the RT graph)
   │      └── ClapGuiSession ──▶ native window           │
   └── #3 …                                             ─┘
           ▲
           └── tick(): pump every editor, then give every plugin its
               main-thread slice (timers, on_main_thread)
```

| Decision | Why |
| --- | --- |
| `ClapInstanceId(u64)`, opaque, never reused | Not the CLAP id — one session commonly hosts two instances of the same plugin. Not a track index — removing a track renumbers every index after it while the instances behind them do not move. A destroyed id never coming back is what stops a stale UI from silently addressing a different plugin. |
| A `Vec`, not a `HashMap` | A session has a handful of plugins and the host is walked in full on every `tick` anyway. Iteration order is creation order, so logs read in the order things happened. |
| `!Send`, by construction | It owns `LoadedClapPlugin` (CLAP pins instance lifecycle to the main thread) and `ClapGuiSession` (a window handle belonging to the loop's thread). It lives in `run()`, next to the kernel. |
| One `tick()` for everything | Editors, `clap.timer-support` timers and `on_main_thread` for *every* instance, from `RunEvent::MainEventsCleared`. Spike 3's pump, generalised — the mechanism was never the limit. |

**Teardown order is not negotiable**, and `destroy` does all three parts in it:

1. destroy the plugin's editor — a window whose procedure lives in an unloaded bundle
   crashes the process on its next message (spike 3, finding 4);
2. retire the `ClapNode` — the audio thread has to have dropped it and the `basedrop`
   collector has to have run its destructor, which is what hands the processor back;
3. deactivate and destroy the instance, on this thread.

`LoadedClapPlugin::wait_for_retire` is step 5's ad-hoc `CLAP_RETIRE_WAIT` loop, moved into
the crate that owns the problem and turned into a bounded *receive* rather than a sleep
poll: the collector wakes it the instant it drops the node, so a source swap costs one
sweep rather than one sweep plus a poll interval.

## 2. `clap.state`

`plugins/host-clap/src/state.rs`. `save_state(instance) -> Vec<u8>` and
`load_state(instance, &[u8])`, straight over the extension's `clap_ostream`/`clap_istream`.
MYDAW does not interpret a byte — that is the point of the extension and the reason a
project file can store a preset for a plugin it knows nothing about.

| Case | Answer | Why |
| --- | --- | --- |
| Plugin has no `state` extension | `save` → **empty blob**; `load` → **no-op**, `Ok` | Per the specification, no extension means no state. That is a state, not a failure. The gain fixture is deliberately such a plugin, so the path is exercised rather than assumed. |
| Plugin writes forever | `ClapStateError::TooLarge` at `MAX_STATE_BYTES` (64 MiB) | `clap_ostream->write` is a loop the *plugin* drives; the only way a host bounds it is by failing a write. A DAW must not OOM because a plugin's `save` has a bug. |
| Blob larger than the limit arrives | `BlobTooLarge`, before the plugin sees it | Reachable from a corrupted or hostile project file. |
| Plugin rejects a blob | `ClapStateError::Load`, and the plugin is left as it was | A blob from a different plugin is the ordinary case. |
| Plugin calls `mark_dirty` | latched on an `AtomicBool`, cleared on read | Step 8 turns it into "the project has unsaved changes". Saving on every knob movement would write a megabyte per gesture. |

`load_state` clears the dirty flag afterwards: some plugins fire `mark_dirty` from inside
`load`, and carrying that into the session would mark a freshly-opened project as modified.

### The fixture grew a real state

`dev.mydaw.fixture.synth` now implements `clap.state`, and its state is **a detune in
semitones** (`f32`, clamped to ±24, blob = `b"MFS1"` + little-endian f32, eight bytes).
Detune rather than gain because a pitch change is a *spectral peak*: "the state took
effect" becomes a number an FFT reports, not a level that could equally be a fader. Held
notes retune on the next block, so a `load` is audible without re-triggering.

Both fixture descriptors also gained CLAP `features` — `[audio-effect, utility, stereo]`
and `[instrument, synthesizer, stereo]` — which is what makes the picker's filter testable.

## 3. Placement, and the two questions about a plugin

`app/src/clap_host.rs`. `ClapHost` owns the instance host *and remembers where each
instance sits*: `tracks[i]` is the instance heading track `i`, plus one optional master
insert. Placement is stored there rather than derived from either side, because track
indices move when the layout changes and instance ids do not.

**Two questions, two answers, deliberately:**

| Question | Answered by | Cost |
| --- | --- | --- |
| What goes in this picker? | the descriptor's `features` (`ClapPluginKind`) | free — already scanned |
| May this actually be used *here*? | the port layout (`ClapPortLayout`) | one instantiation |

A picker that classified by port layout would have to instantiate every installed plugin to
draw a dropdown — running dozens of vendors' initialisation code to populate a menu. So the
list is filtered by what the plugin *claims*, and the placement is checked against what it
*declared to the host*, with an error that names the layout it actually reported. A
descriptor that lies is caught at the moment it matters, and the instance is destroyed
rather than left hosted where nobody can see or free it.

Track sources take instruments (`input_ports == 0`, stereo out); the master insert takes
stereo-in/stereo-out effects. Both are refused with a sentence, both tested.

## 4. The master insert became a slot

`app/src/insert.rs`. Step 5 pushed the CLAP effect into the master chain *only when one was
configured*, which made "add an effect" a graph rebuild — the exact operation a live CLAP
node cannot survive.

`InsertSlot` is therefore **unconditional**: compiled into every graph, transparent when
empty, and swapped through an `rtrb` + `basedrop` channel exactly like `SourceSlot`. One
branch and one wait-free pop per block buys inserting, replacing and removing a master
plugin with no rebuild at all. It differs from `SourceSlot` in one line — an empty source
slot *silences*, an empty insert slot *passes through* — and that line is a test.

## 5. Structural edits, which step 5 refused

The circle step 5 documented is real: a rebuild compiles a whole new graph, which needs new
CLAP nodes; the old nodes are not retired until the old graph is dropped; the old graph
cannot be dropped until the new one is installed. Step 5 answered it by refusing the edit.

Step 6 breaks it by conceding the one thing that is actually cheap:

```text
1.  install the new graph with NO hosted plugin in it at all
        CLAP-headed track  -> SilentNode
        master insert slot -> empty
2.  the old graph is now unreachable: the audio thread drops it, the
    basedrop collector runs its destructors, every processor comes home
3.  activate each instance again and swap its node into the new graph's slot
```

`ClapHost::reattach_all` is that third step; `create_node` does the waiting, bounded by
`ACTIVATE_WAIT` (500 ms ≈ two collector sweeps).

**The cost is a few milliseconds of silence from the hosted plugins only.** Every other
track keeps playing, because a rebuild has never interrupted those (step 5 §5). Measured on
this machine: the level before and after adding a track was identical to within 0.03 of
full scale, and the editor stayed open across it.

### Deviation: the instance is kept, not recreated

The brief asked for *save state → new instance → load state → new node*. What is
implemented is *save nothing → deactivate → reactivate → new node*, and the reasoning is
worth stating because it is a deliberate departure:

- CLAP requires state to survive `deactivate`/`activate`, so a round trip through the
  serialiser is reaching a state the instance is **already in**;
- recreating re-runs the bundle's initialisation (slow, and it runs vendor code again);
- recreating **closes the plugin's editor**, so adding a track would blow away every open
  editor window — a visible regression for an invisible operation;
- some plugins implement `save`/`load` imperfectly, so a round trip that is not needed is a
  fidelity risk taken for nothing.

The save/load path is implemented and tested anyway, because step 8's project loader is
exactly the case that *does* need it: `state_survives_being_moved_into_a_brand_new_instance`
covers save → destroy → create → load, byte-wise and acoustically.

## 6. One bridge, not two

`app/src/clap_bridge.rs`. Spike 3 had `ClapGuiBridge`; step 5 added `ClapSourceBridge`: two
mailboxes with the same shape, drained at two points in the same event-loop pass, and
therefore two independent orderings for operations that are emphatically not independent.
"Make this track a synth" then "open that synth's editor" must not be reordered, or the
second addresses a plugin that does not exist yet. One queue makes that automatic instead
of something the run loop has to get right by hand.

```text
  webview                 command (worker pool)         main thread (tao loop)
  -------                 ---------------------         ----------------------
  setTrackSourceClap  ->  bridge.request(..) ---------> drain() in MainEventsCleared
                            blocks on a reply channel     ClapHost creates the instance,
                                                          activates it, swaps the node in
                          <----------------------------- reply(Done)
  mixer state         <-  Ok(())                         board.publish(..)
```

Every command names a `ClapTarget` (`{"slot":"track","index":n}` or `{"slot":"master"}`),
because with many instances "which plugin" stopped being rhetorical.

Two smaller decisions with outsized effect:

- **`set_served`.** The bridge knows whether a main thread is serving it. Without that, every
  CLAP command in a headless build — a test, a `MYDAW_DEBUG_CAPTURE` run, a window that
  failed to open — would block for the full five-second timeout before failing. It also
  makes the *right* error message possible ("hosted CLAP plugins need MYDAW's main window,
  which is not running in this session") instead of a timeout.
- **The status board.** Answers describe one operation; the *state* — which track holds
  which plugin, whether it has an editor, whether that editor is open — is published to a
  `Send + Sync` board after every change. `get_tracks` runs after every fader move and reads
  it with no round trip at all.

## 7. `Source` carries its plugin

`Source` stopped being `Copy`:

```rust
pub enum Source { TestTone, PolySynth, DrumKit, Clap(ClapSourceRef) }
pub struct ClapSourceRef { bundle_path: PathBuf, plugin_id: String, name: String }
```

Bundle path **and** id, because neither alone is enough: one bundle commonly exports several
plugins (the workspace's own fixture exports two), and the same plugin id can be installed
in two places. That pair is exactly what a project file records.

The wire id is `clap:<plugin id>` — the bundle path is deliberately *not* in it, because an
id lands in a UI list and a Windows path in it is a separator minefield. The path travels as
its own argument on the one command that needs it. `Source::parse` therefore **refuses** a
bare `clap:` id rather than guessing a bundle from a previous scan.

A CLAP-headed track compiles to `SilentNode`, never to a fallback instrument: a track the
user set to a synthesiser must not answer with a sine wave because the host was briefly
busy. A momentary gap is a far smaller lie than a wrong sound.

## 8. Environment variables, retired

`MYDAW_CLAP_FX` and `MYDAW_CLAP_INSTRUMENT` are **gone**, along with `ClapPluginSpec`, its
parser and its loaders. They were spike affordances: a way to get one plugin into the graph
before there was a UI for choosing one. There is a UI now, and it does strictly more — any
number of instruments, on any track, changeable while the engine runs. Keeping the variables
would have meant maintaining a second, weaker path into the same machinery for no user.

`CLAP_PATH` — the CLAP specification's own search-path override, and the way a developer
points MYDAW at a build directory — is untouched.

## 9. UI

- **Mixer strip:** a source `<select>` per track with `Built in` / `CLAP instruments`
  optgroups, sized so a long plugin name ellipsises rather than widening the strip. It
  replaces the old static source line (a `<select>` cannot live inside the header `<button>`,
  and a duplicated name wastes strip height). Below it, an **Editor** button, present only
  when the placed plugin reports `hasEditor` — a plugin without an editor is normal, and a
  button that always fails is worse than no button.
- **Master strip:** the same two controls, with a "No insert" option that clears the slot.
- **CLAP browser:** now a *browser*. Each row badges its kind and carries its raw features
  in a tooltip; the old single "Open GUI" button (which meant "the one env-var plugin") is
  gone, replaced by a line pointing at the mixer.
- **`canEditLayout` is deleted** from the wire, along with the panel's locked-layout hint
  and the guard it put on the remove button.
- **Mock backend** grew four plugins across both kinds, a set of open editors keyed by
  target (so "two at once" is rehearsable in a plain browser), and state stubs.

---

## 10. Verification

### Gates

| Gate | Result |
| --- | --- |
| `cargo test --workspace` | **721 passed, 0 failed, 12 ignored** (670 before this step) |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo fmt --all --check` | clean |
| `pnpm --dir ui build` (`tsc --noEmit && vite build`) | clean |

New coverage, by claim:

| Claim | Where |
| --- | --- |
| two instances of one plugin coexist and activate independently | `host-clap/tests/instances.rs` |
| state round-trips byte-for-byte **and** changes the pitch | `host-clap/tests/instances.rs`, `app/tests/clap_instruments.rs` |
| state survives save → destroy → create → load (the project-load path) | `host-clap/tests/instances.rs` |
| a plugin with no `state` extension saves empty and loads as a no-op | `host-clap/tests/instances.rs` |
| an oversized or malformed blob is refused, and leaves the plugin as it was | `host-clap/tests/instances.rs` |
| `features` and `ClapPortLayout` agree on instrument vs effect | `host-clap/tests/instances.rs` |
| **two editors open, animating and closing independently** | `host-clap/tests/fixture_gui.rs` |
| destroy under load retires the node and the collector ends up empty | `host-clap/tests/instances.rs`, `app/tests/clap_instruments.rs` |
| two hosted synths on two tracks, each hearing only its own channel | `app/tests/clap_instruments.rs` |
| a rebuild re-attaches every instance **with its state**, at the same level | `app/tests/clap_instruments.rs` |
| a hosted effect processes the master sum and can be cleared | `app/tests/clap_instruments.rs` |
| a plugin in the wrong slot is refused and not left hosted | `app/tests/clap_instruments.rs` |
| base64 round-trips every blob length and rejects non-base64 | `app/src/clap_bridge.rs` |
| an empty insert slot is transparent; a source slot is silent | `app/src/insert.rs` |
| a CLAP command with no main thread fails at once, not after 5 s | `app/src/state.rs`, `app/src/clap_bridge.rs` |

### Hardware verification

`cargo test -p mydaw-app --test clap_hardware -- --ignored --nocapture --test-threads=1`

Speakers (Focusrite USB Audio) @ 44 100 Hz, 2 ch, max block 4096. Kick-only pattern at
130 BPM on track 0 (drum kit), hosted fixture synths on tracks 1 and 2, five captures of
1.5 s, 32768-point FFT (1.35 Hz/bin), **0 dropped samples in every capture**. Placement,
layout edits and state all went through `ClapCommand` — **no environment variables**.

| Capture | L peak (dBFS) | kick band | dominant partials (L) |
| --- | --- | --- | --- |
| beat only | 0.3522 (−9.07) | −19.94 dB | 52.5, 83.1, 28.1 Hz |
| beat + hosted A4 on track 1 | 0.5381 (−5.38) | −20.48 dB | **440.0**, 52.6, 83.4 Hz |
| chord split across two instances | 0.7428 (−2.58) | −19.90 dB | **329.6, 392.0, 261.6**, 52.2 Hz |
| track 1's state set to +12 semitones | 0.9024 (−0.89) | −19.93 dB | **659.2, 523.2, 392.0**, 52.2 Hz |
| the saved state loaded back | 0.8070 (−1.86) | −20.14 dB | **329.6, 392.0, 261.6**, 52.1 Hz |

Read as claims:

1. **A plugin picked from the list plays on the track it was put on.** 440.0 Hz appears in
   the same capture as the beat, and the kick moves 0.54 dB — measurement noise on a
   transient, not a level change.
2. **A chord really is split across two instances.** C4 (261.6), E4 (329.6) and G4 (392.0)
   are all present; C4 and E4 come from track 1's instance and G4 from track 2's. The track
   that carries G4 only exists because a track was *added* with a hosted plugin already in
   the graph — the edit step 5 refused.
3. **State is audible, isolated and restorable.** +12 semitones on track 1 moved its C4 and
   E4 to 523.2 and 659.2 Hz while track 2's 392.0 Hz stayed exactly where it was — the state
   belongs to one instance. Loading the saved blob back restored 261.6 / 329.6 / 392.0, and
   a fresh `save` returned the same eight bytes.

`cargo test -p mydaw-app --test gui_audio_smoke -- --ignored --nocapture` runs the same
machinery with the editor open: two hosted plugins (synth on track 0, gain on the master),
editor window `0x3f07b2` with the plugin's child at `0x4405fc`, a drag negotiated down to
610×430 and repainted, **440.0 Hz at −20.00 dBFS** (the synth's −13.98 through the gain's
−6.02), **0 dropped blocks** — and adding a track mid-flight left the peak at 0.1000 with
the editor still open.

WAV, annotated PNG and `stats.json` for each capture are written under
`.debug/captures/<timestamp>/` and the paths are printed.

---

## 11. Limitations, and what step 7+ inherits

- **No parameters.** No `clap.params`, no automation, no parameter events, and therefore no
  way to change a plugin's settings except through its own editor. This is the largest
  remaining gap and the next obvious one to close: `ClapNode`'s `EventBuffer` is where
  parameter events would go in, and `OutputEvents::void()` is where a knob moved in an
  editor would have to come back out.
- **No modulation, no note expressions.** `NoteKind` carries key and velocity; CLAP's
  polyphonic modulation and expression events are neither sent nor received.
- **Audio-effect inserts on *tracks* are not placeable.** The graph has carried a per-track
  insert chain since step 5 and the master's slot mechanism generalises to it directly, but
  there is no UI, no placement model and no project field for it yet.
- **A plugin's note *output* is discarded.** `ClapNode` drains none, so an arpeggiator or a
  note-effect has nowhere to send what it produces. `ClapPluginKind::NoteEffect` is listed
  in the browser precisely so a user is not left wondering where theirs went.
- **Nothing is persisted.** `save_clap_state` / `load_clap_state` exist and are tested, and
  `TrackDef` records bundle + id, but there is no project file to put them in (step 8).
  Editor size and position are still forgotten on close.
- **A rebuild silences hosted plugins briefly.** See §5. Bounded by `ACTIVATE_WAIT`; in
  practice one collector sweep.
- **The UI's editor state is refreshed, not pushed.** The board is republished whenever
  something changes and `get_tracks` reads it, so closing a plugin's *native* window
  updates the button on the next mixer refresh rather than instantly. A push channel from
  the shell to the webview is the fix, and it is the same one automation will want.
- **`request_restart` is still only recorded.** A plugin asking to be deactivated and
  reactivated is reported in `MainThreadWork` and ignored — even though step 6 now has
  exactly the machinery (`reattach_all`) that would honour it.
- **No crash isolation, still.** A plugin that segfaults takes the process; one that blocks
  in its window procedure freezes the loop, webview included. Out-of-process scanning
  remains the Phase 2 TODO the crate has documented since spike 2.
- **Only Windows** for editors. The macOS/X11 backends of `mydaw-plugin-windows` are still
  the `unsupported` module's four functions.
