# Mixer v2: buses, sends and insert chains

**Status:** implemented.
**Crates:** `crates/rt-core`, `crates/ipc-bridge`, `plugins/audio-io`,
`plugins/effects` (unchanged), `app`.
**Supersedes:** the routing half of step 5 (multitrack mixer), and §3 of
`2026-08-19-param-system-effects-v1.md` — the insert-chain wiring that document
specified and did not build.

§7 is the command contract; the rest is why it looks like that.

---

## 1. What changed, in one paragraph

The mixer had sixteen strips and one destination. It now has sixteen tracks,
**eight buses**, an **insert chain on every one of them and on the master**, and
a matrix of **send taps** with a level and a pre/post flag each. Every strip
still has exactly one main output, into the master; a send is an *additional
copy*, not a reroute. Nothing about a fader, a pan, a mute or a solo changed, and
a project with no buses renders bit-identically to what it rendered before.

---

## 2. The model

```text
Project
  tracks:  [TrackDef]     id, name, source, gain_db, pan, muted, soloed, inserts
  buses:   [BusDef]       id, name, color?, gain_db, pan, muted, soloed, inserts
  sends:   [SendDef]      id, source: StripId, dest: bus, level, pre
  master_gain_db, master_inserts: [InsertDef]

InsertDef { id, type_id, params: Arc<ParamBlock> }
StripId   = Track(index) | Bus(index)
```

**One main out, any number of taps.** Every strip — track or bus — has one
implicit main output into the master. There is no command to reroute it, and
that is the design rather than an omission:

- a mixer whose main paths are settled is one whose signal flow can be read off
  the strip list, rather than traced through a matrix;
- the things users actually want — a reverb, a parallel compressor, a drum
  group — are *taps*, not reroutes;
- "wet only", the one case a reroute would buy, is a **pre-fader tap with the
  fader down**, which is exactly how it is done on a desk and is measured in §10.

Consequently a send's destination is always a **bus**. The master is never one,
because every strip already reaches it.

**Ids, not indices, for anything a user or a file can name.** A track index is a
note channel and a bus index is what a send points at, so both are *positions*
and both move under deletion. Every bus, send and insert therefore carries a
`u32` that is allocated once, never reused, and survives a save — and **every
send command addresses a send by id**, so a send survives a bus being deleted
above it. Removing a strip renumbers what is left and drags the sends with it
(`Project::remove_bus`, `Project::retarget_sends_after_removing_track`); a send
left pointing at whatever moved into the gap would silently reroute somebody's
reverb.

**A bus is a track without a source.** It sums, it has a fader, a pan, a mute, a
solo and its own insert chain. `BusDef` and `TrackDef` share field names
wherever they share meaning, so one UI component can draw either.

### 2.1 Solo is one scope

`Project::bus_effective_mute` resolves a bus against `any_soloed() ||
any_bus_soloed()`. Soloing a track therefore silences every bus that is not
itself soloed. Two independent solo systems would produce a soloed track that is
inaudible because its group is not, which nobody would understand.

Solo stays resolved on the non-RT side into an effective mute per strip, exactly
as it was for tracks: the audio thread reads one bool per strip and never learns
that solo exists.

### 2.2 Pre-fader means before the fader, pan *and* mute

```text
source -> inserts -> [pre-fader tap] -> strip(gain, pan, mute) -> [post-fader tap]
                                                               \-> master sum
```

So a pre-fader send is unaffected by everything on the strip, and a post-fader
send follows the fader — which is what keeps a reverb in proportion while a
fader is ridden. A bus's pre-fader tap is taken after **its** inserts, on the
same terms.

---

## 3. RT topology

`crates/rt-core/src/graph.rs`:

```text
   track i:  SourceSlot -> insert.. -> gate ─┬─ pre tap ──▶ bus[d].input
                                             │
                                             └─ strip ─┬─▶ master sum
                                                       └─ post tap ──▶ bus[d].input

   bus b (in topological order):
        bus[b].input -> insert.. ─┬─ pre tap ──▶ bus[d].input   (d later than b)
                                  └─ strip ─┬─▶ master sum
                                            └─ post tap ──▶ bus[d].input

   master sum -> master insert chain -> InsertSlot (hosted) -> master fader
```

### 3.1 Two taps per send, and why

A send compiles into **two** `TrackStrip`s — one reading the pre-fader buffer,
one reading the post-fader one — and `MixerParams::send_targets` hands the level
to exactly one of them and zero to the other. Flipping pre/post is therefore a
10 ms crossfade between two taps of the same signal rather than a route switched
on a block boundary, which is the difference between a routing change and a
click. It costs one extra mono ramp per send, which is nothing next to the
alternative.

### 3.2 The post-fader scratch, and the path that did not change

A strip with **no** taps still goes through `TrackStrip::mix_into` — multiply and
accumulate in one pass, straight into the master sum, exactly as before. A strip
*with* taps has more than one consumer for its post-fader signal, so it is
produced once into a single shared scratch buffer (`TrackStrip::apply_into`) and
handed to each. The two are bit-identical against a silent destination, which is
asserted in `mixer.rs`; keeping the no-tap branch is what makes
`a_graph_with_no_sends_is_bit_identical_to_one_compiled_before_buses_existed`
true.

One scratch buffer for the whole graph, not one per strip: a strip's post-fader
signal is produced, consumed and finished with before the next strip starts.

### 3.3 Bus order

`bus_order(bus_count, sends)` is Kahn's algorithm over the bus-to-bus send edges,
run once at compile time. Ties break by index, so a rebuild produces the same
order and a compiled graph is reproducible. The result is always a permutation of
`0..bus_count`.

Compilation then **drops every tap whose destination is not later in that
order** — which for an acyclic routing is none of them, and for a cycle is
exactly the edges that close it. The audio thread therefore never has to check
for a cycle and never has to reason about buffer staleness.

### 3.4 What the audio thread does per block

1. clear the master bus and every bus input;
2. for each track: render, run pre-fader taps, then either accumulate into the
   master or produce into the scratch and fan out;
3. for each bus **in `bus_order`**: run its inserts on its input, run its
   pre-fader taps, then its strip, the same two ways;
4. master insert chain, hosted insert slot, master fader.

Everything in it is arithmetic over buffers allocated in `compile()`. Measured:
`crates/rt-core/tests/mixer_rt_alloc.rs` renders 32 blocks of **sixteen tracks,
eight buses and every one of the 156 possible taps** — with send knobs, pre/post
flags and bus controls moving while it renders — under a counting global
allocator, and observes zero allocations.

---

## 4. Cycle rules

| where | what happens |
| --- | --- |
| `Project::add_send` | **Refused**, with the loop written out: `there is no send…` / `that would feed a bus into itself: Reverb -> Drums -> Reverb` |
| `Project::add_send`, source == dest | `a bus cannot send to itself` |
| `ProjectFile::to_project` | a cycle in a **hand-edited file** has its closing edge dropped, with a warning, so the project still opens |
| `GraphSpec::compile` | any tap running backwards in the chosen order is dropped — the last line of defence, and unreachable through the command surface |

The error names the whole path because "that would be a cycle" is not something a
user can act on and `bus 1 -> bus 3 -> bus 1` is. The walk is
`Project::cycle_through`: an iterative depth-first search from the proposed
destination looking for the proposed source, carrying its path.

---

## 5. Caps

| constant | value | what it bounds |
| --- | --- | --- |
| `MAX_TRACKS` | 16 | tracks; also the note channels a `NoteEvent` can address |
| `MAX_BUSES` | **8** | buses |
| `MAX_SEND_SOURCES` | 24 | rows of the send matrix (`MAX_TRACKS + MAX_BUSES`) |
| `MAX_SEND_LEVEL` | **1.25** | linear send level; `max_send_db()` is **+1.94 dB** |
| `MAX_INSERTS` | **8** | effects on one chain |
| `SILENCE_DB` .. `MAX_GAIN_DB` | −60 .. +6 | every fader, track and bus alike |

**Why eight buses.** Every piece of per-bus state the audio thread touches is a
`[_; MAX_BUSES]`, so nothing on the RT side ever grows: the parameter block, the
send matrix, the peak block and the meter frame are all fixed arrays. Eight is
the number of aux/group buses a sixteen-channel mixer needs before the *routing*
rather than the count becomes the limitation, and it keeps the send matrix at
24 × 8 = 192 cells — under two kilobytes, and small enough that publishing the
whole thing on every project change is free.

**Why eight inserts.** A chain is rebuilt into a `Vec<Box<dyn RtNode>>` per
strip, and eight internal effects on one strip is already more DSP than a strip
needs. The limit exists so a webview sending "add" in a loop gets a sentence.

Every cap is checked at the IPC boundary, so exceeding one is a status-bar
sentence rather than a truncated project.

---

## 6. What crosses the RT wall, and what rebuilds

| edit | cost |
| --- | --- |
| bus fader, pan, mute, solo | atomic store; 10 ms ramp |
| send level, send pre/post | atomic store; 10 ms ramp (the flag is a crossfade — §3.1) |
| insert parameter, insert bypass | atomic store; the effect's own smoothing |
| bus name, bus colour | nothing crosses the wall at all |
| **add/remove bus, add/remove send, add/remove/move insert** | **graph rebuild** |

The rebuild goes through exactly the machinery `add_track` already used
(`AppState::rebuild_graph`): with nothing hosted it is a `GraphSpec` and a queue
push on the calling thread; with a hosted plugin loaded it posts
`Vst3Command::Rebuild` or `ClapCommand::Rebuild` and waits, because the new graph
needs a new node for every instance and only a host's main thread can make one.
That is the step-6 re-attach protocol with the project edit taken out of it — one
command rather than one per edit, because the main thread's job is identical for
all of them.

**Nothing is reset by a rebuild.** `MixerParams` is shared by `Arc` and so is
every insert's `ParamBlock`, so a rebuilt node is handed the block the old one
was reading and picks the parameters up where they were left. Send taps are built
already sitting at their level, exactly as strips are. Measured twice: offline in
`graph.rs`, and against a real hosted CLAP instance in
`app/tests/clap_instruments.rs::a_routing_edit_rebuilds_the_graph_and_leaves_every_plugin_and_every_knob_alone`.

**A missing device is not an error** for any of these. A bus, a send and an
insert chain are project data, exactly as a pattern is; the session installs
whatever it holds when a stream comes up.

---

## 7. Command contract

*This section is the UI bible.* Every mixer command returns the whole
`MixerState` unless noted, for the reason the sequencer commands return the whole
`SequencerState`: one shape back means a panel never merges a partial update into
what it already had. Errors are a sentence, shown as-is in the status bar.

### 7.1 `MixerState`

```ts
type MixerState = {
  tracks: Track[];
  buses: Bus[];                 // empty in a project with none
  sends: Send[];                // every tap, from both kinds of source
  master: Master;
  selected: number;             // track the keyboard and source picker address
  maxTracks: number;            // 16
  maxBuses: number;             // 8
  maxInserts: number;           // 8
  minGainDb: number;            // -60
  maxGainDb: number;            // +6
  maxSendLevel: number;         // 1.25
  maxSendDb: number;            // 1.9382
};

type Track = {
  index: number;                // position = note channel
  id: number;                   // stable; key a list on this
  name: string;
  sourceId: string; sourceLabel: string;
  gainDb: number; pan: number;
  muted: boolean; soloed: boolean;
  silenced: boolean;            // actually silent right now, solo resolved
  selected: boolean;
  clap: ClapSlot | null; vst3: Vst3Slot | null; missing: MissingPlugin | null;
  inserts: Insert[];            // internal effects, in signal order
};

type Bus = {
  index: number;                // position = what a send's `dest` names
  id: number;
  name: string;
  color?: string;               // ABSENT when the engine has none
  gainDb: number; pan: number;
  muted: boolean; soloed: boolean;
  silenced: boolean;
  inserts: Insert[];
};

type Send = {
  id: number;                   // EVERY send command addresses this
  sourceKind: "track" | "bus";
  sourceIndex: number;
  dest: number;                 // bus index
  level: number;                // linear, 0..=maxSendLevel; 1.0 is unity
  levelDb: number;              // the same, in dB; -60 at zero
  pre: boolean;
};

type Master = {
  gainDb: number;
  clap: ClapSlot | null; vst3: Vst3Slot | null; missing: MissingPlugin | null;
  inserts: Insert[];            // internal chain, AHEAD of the hosted insert
};

type Insert = {
  index: number;                // position in the chain = what commands take
  id: number;                   // stable across reordering; key a list on this
  typeId: string;               // "effects.eq"
  name: string;                 // display name, from the catalogue
  bypassed: boolean;            // lifted out because every strip draws it
  params: ParamValue[];         // every parameter, in declaration order
  readouts: ParamValue[];       // every meter; empty for effects with none
};

type ParamValue = { id: string; value: number };
```

`color` is **absent** rather than `null` when there is none — the engine has no
palette, and inventing one would put a design decision in a file format.

A send is drawn from both ends ("sends from this strip", "what feeds this bus")
by filtering the one `sends` list. It is deliberately not split per strip: two
copies of a relationship can disagree.

```jsonc
// a fragment of a real MixerState
{
  "buses": [
    { "index": 0, "id": 0, "name": "Drums", "color": "#ff6b35",
      "gainDb": -2.5, "pan": 0.25, "muted": false, "soloed": false,
      "silenced": false,
      "inserts": [
        { "index": 0, "id": 4, "typeId": "effects.compressor",
          "name": "Compressor", "bypassed": false,
          "params": [ { "id": "bypass", "value": 0.0 },
                      { "id": "threshold", "value": -20.0 }, /* … */ ],
          "readouts": [ { "id": "gr", "value": 12.6 },
                        { "id": "gr_max", "value": 14.14 } ] }
      ] }
  ],
  "sends": [
    { "id": 0, "sourceKind": "track", "sourceIndex": 0, "dest": 0,
      "level": 0.4, "levelDb": -7.9588, "pre": true }
  ],
  "maxBuses": 8, "maxInserts": 8, "maxSendLevel": 1.25, "maxSendDb": 1.9382
}
```

### 7.2 The catalogue

```ts
type EffectInfo = {
  typeId: string; name: string; description: string;
  params: ParamDef[]; readouts: ReadoutDef[];
};

type ParamDef = {
  id: string; name: string;
  unit: string;                 // "dB" | "Hz" | "ms" | "%" | ":1" | ""
  min: number; max: number;
  defaultValue: number;         // `default` is reserved in too many languages
  curve: "linear" | "log" | "db";
  step: number | null;          // quantisation in the parameter's own unit
  smoothingMs: number;          // 0 marks a SWITCH: draw it as one, never animate
};

type ReadoutDef = { id: string; name: string; unit: string };
```

`curve` is how a knob should lay its range out; the stored value is always the
real value in the parameter's own unit. `step: 1.0` with `smoothingMs: 0` is how
a switch or an enumeration is declared.

```jsonc
{ "typeId": "effects.eq", "name": "EQ",
  "params": [
    { "id": "bell1.freq", "name": "Bell 1 Freq", "unit": "Hz",
      "min": 20.0, "max": 20000.0, "defaultValue": 800.0,
      "curve": "log", "step": null, "smoothingMs": 10.0 },
    { "id": "hp.slope", "name": "HP Slope", "unit": "",
      "min": 0.0, "max": 1.0, "defaultValue": 1.0,
      "curve": "linear", "step": 1.0, "smoothingMs": 0.0 }
  ],
  "readouts": [] }
```

### 7.3 Buses

| command | arguments | returns | rebuilds |
| --- | --- | --- | --- |
| `add_bus` | `name?: string` | `MixerState` | yes (`async`) |
| `remove_bus` | `bus: number` | `MixerState` | yes (`async`) |
| `rename_bus` | `bus, name: string` | `MixerState` | no |
| `set_bus_color` | `bus, color?: string \| null` | `MixerState` | no |
| `set_bus_gain` | `bus, db: number` | `MixerState` | no |
| `set_bus_pan` | `bus, pan: number` | `MixerState` | no |
| `set_bus_mute` | `bus, muted: boolean` | `MixerState` | no |
| `set_bus_solo` | `bus, soloed: boolean` | `MixerState` | no |

`add_bus` with no name numbers by **position** — "Bus 3" after two deletions is
the third strip, which is what the user is looking at.

`remove_bus` takes **every send at either end of it**, and renumbers the sends
naming a later bus. See §2 for why.

### 7.4 Sends

| command | arguments | returns | rebuilds |
| --- | --- | --- | --- |
| `add_send` | `sourceKind: "track" \| "bus", sourceIndex, dest` | `MixerState` | yes (`async`) |
| `remove_send` | `sendId: number` | `MixerState` | yes (`async`) |
| `set_send_level` | `sendId, level: number` | `void` | no |
| `set_send_pre` | `sendId, pre: boolean` | `void` | no |

A new send is **unity, post-fader**. `set_send_level` and `set_send_pre` return
nothing rather than the whole mixer because a knob drag produces one per frame
and the caller already knows what it asked for.

`level` is linear in `0..=maxSendLevel`. A knob should lay it out on the dB
scale `minGainDb..=maxSendDb`, which is what `levelDb` is for; the engine does
not keep a second copy of that conversion and neither should the UI.

### 7.5 Insert chains

`target` is `"track"`, `"bus"` or `"master"`; `targetIndex` is ignored for the
master. The same three words the routing commands use for a strip.

| command | arguments | returns | rebuilds |
| --- | --- | --- | --- |
| `get_effects` | — | `EffectInfo[]` | — (static; fetch once at startup) |
| `add_insert` | `target, targetIndex, typeId, position?` | `MixerState` | yes (`async`) |
| `remove_insert` | `target, targetIndex, index` | `MixerState` | yes (`async`) |
| `move_insert` | `target, targetIndex, from, to` | `MixerState` | yes (`async`) |
| `set_insert_param` | `target, targetIndex, index, paramId, value` | `number` | no |
| `set_insert_bypass` | `target, targetIndex, index, bypassed` | `MixerState` | no |
| `get_insert_readouts` | `target, targetIndex, index` | `ParamValue[]` | no |
| `get_chain_readouts` | `target, targetIndex` | `InsertReadouts[]` | no |

`set_insert_param` returns **what was actually stored** — clamped into range and
snapped to the parameter's step grid — so a knob that sent 40 kHz to a
20 Hz..20 kHz parameter can put itself on the truth rather than draw where the
engine is not.

`get_insert_readouts` is deliberately **separate from the parameter values**: a
gain-reduction bar wants to move at frame rate, and sending twenty knob positions
sixty times a second to animate one bar is the mistake the `ParamDef`/`ReadoutDef`
split exists to prevent.

```ts
type InsertReadouts = { index: number; id: number; readouts: ParamValue[] };
```

`get_chain_readouts` returns **one entry per insert**, including the ones that
publish nothing, so the answer's length is the chain's length and a UI can index
it directly.

**`gr_max`, not `gr`.** A compressor's `gr_max` is the worst reduction *anywhere
in one block*; `gr` is the last sample's. A whole attack and recovery can fall
between two UI frames, so a meter should draw `gr_max` and hold its own peak —
`app/tests/routing_audio.rs::poll_reduction` is what that looks like.

### 7.6 Error vocabulary

| situation | message shape |
| --- | --- |
| unknown bus / track / send | `there is no bus 7` / `there is no track 7` / `there is no send 7` |
| too many buses | `a project may have at most 8 buses` |
| duplicate tap | `track 3 already sends to bus 1` |
| self-send | `a bus cannot send to itself` |
| cycle | `that would feed a bus into itself: Reverb -> Drums -> Reverb` |
| bad send level | `send level 9 is not a number between 0.0 and 1.25` |
| blank bus name | `a bus must have a name` |
| unknown effect | `'effects.reverb' is not an internal effect` |
| unknown target word | `'aux' is not a place to put an effect; expected 'track', 'bus' or 'master'` |
| unknown send source word | `'aux' is not a place a send can come from; expected 'track' or 'bus'` |
| chain full | `a chain may hold at most 8 effects` |
| no such insert | `bus 1 has no insert 4` |
| no such parameter | `'effects.eq' has no parameter called 'resonance'` |

---

## 8. Meters

`MeterFrame` gains `buses: [TrackMeter; MAX_BUSES]` and `bus_count`, measured
after each bus's strip on exactly the terms the per-track peaks are — so a bus
meter follows its own fader and not the master's. The DTO renders it as a list of
exactly the buses that exist:

```ts
type MeterFrame = {
  peakL: number; peakR: number;      // master, POST master fader
  rmsL: number; rmsR: number;
  samplePos: number; playing: boolean;
  tracks: { peakL: number; peakR: number }[];   // post-strip, pre-master
  buses:  { peakL: number; peakR: number }[];   // post-strip, pre-master
};
```

A separate list rather than more entries in `tracks`, because a bus is a
different strip with a different index space: `buses[0]` is bus 0, not "the track
after the last one". Empty in a project with no buses, so a session that never
opened the routing view pays nothing for it.

**Total strips a frame can carry: 25** — sixteen tracks, eight buses, and the
master, whose level is the flat `peak*`/`rms*` fields. Unchanged at 30 Hz through
the same throttle and the same pump.

---

## 9. `.mydaw` version 4

```jsonc
{
  "format_version": 4,
  "bpm": 130.0,
  "master_gain_db": -1.5,
  "selected_track": 0,
  "tracks": [
    { "id": 0, "name": "Kick", "source": { "kind": "internal", "id": "kick" },
      "gain_db": 0.0, "pan": 0.0, "muted": false, "soloed": false,
      "state_blob": "",
      "inserts": [
        { "id": 0, "type_id": "effects.eq",
          "params": { "bell1.freq": 800.0, "bypass": 0.0, "low.gain": 6.0 /* … */ } }
      ] }
  ],
  "buses": [
    { "id": 0, "name": "Drum Bus", "color": "#ff6b35",
      "gain_db": -2.5, "pan": 0.25, "muted": false, "soloed": false,
      "inserts": [ { "id": 1, "type_id": "effects.compressor", "params": { /* … */ } } ] }
  ],
  "sends": [
    { "id": 0, "source_kind": "track", "source_index": 0, "dest": 0,
      "level": 0.4, "pre": true }
  ],
  "master_effect": null,
  "master_inserts": [ { "id": 2, "type_id": "effects.utility", "params": { /* … */ } } ],
  "play_mode": "pat",
  "patterns": [ /* unchanged from version 3 */ ],
  "active_pattern": 1,
  "playlist": { /* unchanged */ },
  "audio_sources": []
}
```

**Why this is a version bump.** Step 8's rule is *do not* bump for a new optional
field — rule 1 (unknown fields are ignored) already handles it. That is exactly
the problem here: a version 3 build opening a project whose drum bus carries a
compressor would play it back **without the compressor and without the bus**,
silently, and then save that loss back over the user's file. An insert chain and
a routing graph are required for correct playback, which is step 8's own stated
bump condition, so refusing the file by name is the correct outcome.

**Migration from version 3 is nothing.** `buses`, `sends`, `master_inserts` and
`tracks[].inserts` are all `#[serde(default)]`, so a version 3 file parses as a
version 4 one with no routing — which is precisely what such a file meant. Plus
the standard "stamp the current version on load": policy unchanged, **migrate on
load, write current on save**.

`app/tests/fixtures/v3.mydaw` is a real file written by this build *before* the
bump and committed verbatim. The test asserts it still says `"format_version":
3` — so nobody "helpfully" updates it — and that opening and saving it writes
version 4 with `"buses": []`, `"sends": []`, `"master_inserts": []` and
`"inserts": []` present rather than omitted.

### 9.1 Insert entries

`params` is a `BTreeMap`, so the key order on disk is deterministic and the
byte-identical round trip still holds. **Every** parameter is written, not only
the ones that were moved: a default that changes in a later build must not
silently change a saved project. That is also the precedent the v1 → v2
migration set, where `"notes": []` is written explicitly rather than omitted.

Ids are restored rather than regenerated, so a UI list keyed on one does not look
like every effect being replaced after a load.

### 9.2 What is a warning rather than a refusal

| situation | outcome |
| --- | --- |
| `type_id` this build does not have | the effect is left out of the chain, with a warning; **the entry stays in the file** |
| parameter id this build does not have | ignored (`ParamBlock::apply` — rule 1 at parameter granularity) |
| send naming a strip that is not there | dropped, with a warning |
| duplicate send between the same two strips | the later one dropped |
| send closing a cycle (hand-edited file only) | the closing edge dropped |
| bus fader or pan outside the scale | clamped |
| more than `MAX_BUSES` buses | **refused**: `a project may have at most 8 buses; this one has 12` |

The first row is the missing-plugin policy one level down, and it has the same
shape: the project opens, that thing is silent, and a save on this machine writes
the reference back out unchanged.

### 9.3 A note for the parallel audio-clip work

This step takes **version 4**. The audio-clip renderer was in flight at the same
time and will need its own bump for whatever it adds to `audio_sources` and the
clip entries; **it takes version 5**, and neither branch renumbers the other.
`audio_sources` and the audio half of `ClipEntry` are untouched here.

---

## 10. Verification

### Unit and offline

`cargo test --workspace`: **1284 passed, 0 failed, 24 ignored** (baseline before
this work: 1229 / 0 / 20, measured on the same machine and the same way — the
four new ignored tests are §"Hardware" below).

The load-bearing ones:

| claim | where |
| --- | --- |
| a send tap adds its source into a bus, and the bus reaches the master | `rt-core/src/graph.rs` |
| **fader at silence: a pre-fader tap still sends, a post-fader one does not** | `rt-core/src/graph.rs` |
| the same, for mute | `rt-core/src/graph.rs` |
| a send level change ramps and never steps | `rt-core/src/graph.rs` |
| flipping pre/post crossfades rather than switching | `rt-core/src/graph.rs` |
| bus order respects every bus-to-bus edge | `rt-core/src/graph.rs` |
| a bus-to-bus tap arrives in the same block | `rt-core/src/graph.rs` |
| a cycle is broken at compile time, and a self-send dropped | `rt-core/src/graph.rs` |
| buses past `MAX_BUSES` dropped at compile time | `rt-core/src/graph.rs` |
| a project with no sends is **bit-identical** to one compiled before buses existed | `rt-core/src/graph.rs` |
| send levels survive a rebuild with no fade-in | `rt-core/src/graph.rs` |
| a strip written into a scratch is bit-identical to one summed into silence | `rt-core/src/mixer.rs` |
| a track's row of the send matrix is never a bus's | `rt-core/src/mixer.rs` |
| **zero allocation at full fan-out**: 16 tracks × 8 buses, 156 taps, 32 blocks | `rt-core/tests/mixer_rt_alloc.rs` |
| the cycle rule, with the loop named | `app/src/project.rs` |
| removing a bus or a track drags and renumbers its sends | `app/src/project.rs` |
| a send is addressed by id and survives a bus deleted above it | `app/src/project.rs` |
| the whole send matrix is republished, so a stale cell cannot be inherited | `app/src/project.rs` |
| one solo scope across tracks and buses | `app/src/project.rs` |
| insert CRUD on all three kinds of chain, and the caps | `app/src/project.rs` |
| v3 → v4 migration against a committed fixture | `app/src/project_file.rs` |
| buses, sends and chains round-trip byte-identically | `app/src/project_file.rs` |
| an unknown effect / parameter / send is a warning, not a refusal | `app/src/project_file.rs` |
| a send reaching a bus through the shell's own builder | `app/tests/mixer_graph.rs` |
| a send level and an insert parameter reaching the running graph **with no rebuild** | `app/tests/mixer_graph.rs` |
| the compressor's `gr` / `gr_max` readouts through the command surface | `app/tests/mixer_graph.rs` |
| **a routing edit re-attaches a hosted CLAP plugin and leaves another chain's knobs alone** | `app/tests/clap_instruments.rs` |

### Hardware

```sh
cargo test -p mydaw-app --test routing_audio -- --ignored --nocapture --test-threads=1
```

Focusrite USB Audio, 44 100 Hz, 2 ch, **max block 4096**, 130 BPM, 1.5 s per
capture (66 150 frames). WAV, annotated PNG and stats JSON under
`app/.debug/captures/`.

**1. A send into a bus carrying a +12 dB shelf from 4 kHz.** Closed hat on every
eighth, tapped pre-fader at unity into the bus; the hat's own main output is
untouched, so the device plays dry + wet. Reference is the same project with the
bus muted, so the only difference between the two captures is the bus.

| | 6–16 kHz | 200 Hz–1 kHz | peak |
| --- | --- | --- | --- |
| dry | −42.75 dB | −72.16 dB | −20.39 dBFS |
| wet + dry | **−29.10 dB** | −66.14 dB | −7.22 dBFS |
| change | **+13.65 dB** | **+6.02 dB** | |

Theory for dry + wet is `1 + 10^(12/20)` = **+14.0 dB** above the corner and
`1 + 1` = **+6.0 dB** below it. Measured 13.65 and 6.02, on a noisy source
through a real converter. 0 dropped, 0 clipped.

**2. Pre versus post, with the fader at the bottom of its scale.** One bus, one
tap, `gain_db = -60` on the source (`db_to_gain(-60)` is exactly zero, so the
track's own path is off rather than quiet).

| | 6–16 kHz | peak |
| --- | --- | --- |
| pre-fader | **−42.75 dB** | −20.39 dBFS |
| post-fader | **−200.00 dB** (the floor of the measurement) | **−inf dBFS** |

The post-fader capture is **digital silence** — not "quieter", not "mostly
gone". 0 dropped in both.

**3. A compressor on a drum bus.** Kick on the beat, snare on 2 and 4, closed
hats on every sixteenth, an open hat on the off-beats; all four tapped pre-fader
into the bus with their own faders down, so the device plays the compressor's
output and nothing else. Threshold −20 dB (between the hats at ≈ −20 dBFS and
the kick at ≈ −4), ratio 8:1, attack 1 ms, release 60 ms, knee 3 dB. Reference
is the *same* chain with `bypass` on.

| | peak | rms | **crest** |
| --- | --- | --- | --- |
| bypassed | −3.54 dBFS | −25.78 dBFS | **22.25 dB** |
| compressed | −12.32 dBFS | −31.82 dBFS | **19.50 dB** |
| | | | **−2.75 dB** |

Reported through `get_insert_readouts`, polled over 1.5 s the way a meter panel
polls: **`gr` 12.60 dB, `gr_max` 14.14 dB**. 0 dropped, 0 non-finite.

**4. EQ + compressor + delay on one bus.** Shelf +6 dB, low +4 dB, threshold
−30 dB at 6:1, delay feedback 45 % at 40 % mix, fed by a pre-fader tap at 0.8.

| | dropped | non-finite | clipped | peak |
| --- | --- | --- | --- | --- |
| three effects, max block 4096 | **0** | **0** | **0** | −6.10 dBFS |
| after removing the compressor mid-play | **0** | 0 | 0 | −4.53 dBFS |

The second row is the structural rebuild every insert edit goes through, taken
while the transport was running.

---

## 11. Limitations

- **No main-output rerouting.** By design — §2. A track cannot be made to feed a
  bus *instead of* the master; it feeds it *as well*, and "wet only" is a
  pre-fader tap with the fader down.
- **A send's destination is a bus.** The master is not one, because every strip
  already reaches it.
- **Buses carry internal effects only.** A hosted CLAP or VST3 plugin still has
  exactly two homes: the head of a track, and the one master insert slot. Nothing
  in the model prevents extending it; the slot machinery is per-place and would
  have to grow one slot per bus.
- **The master's hosted insert runs last**, after the internal chain. That keeps
  the hosted slot a single unconditional node at a fixed position, which is what
  makes its rebuild-free swap work.
- **No plugin delay compensation**, unchanged. A bus carrying the saturator at 4x
  is 0.79 ms late against one that is not.
- **No sidechain input.** A compressor on a bus detects its own input; there is
  no way to key it from another strip. That needs a second audio path into a
  node, which the `RtNode` contract does not have.
- **Solo is one scope**, which is a choice — §2.1 — and means a soloed bus
  silences every track that is not itself soloed.
- **Stereo only**, like everything else in the engine.

---

## 12. Files

| file | what |
| --- | --- |
| `crates/rt-core/src/mixer.rs` | `StripId`, `SendParams`, the send matrix, bus strips, `PeakBlock` |
| `crates/rt-core/src/graph.rs` | `BusSpec`, `SendSpec`, `bus_order`, the per-block topology |
| `crates/rt-core/tests/mixer_rt_alloc.rs` | zero-alloc at full fan-out |
| `crates/ipc-bridge/src/meter.rs` | per-bus peaks in the meter frame |
| `plugins/audio-io/src/rt.rs` | filling them |
| `app/src/effects.rs` | `InsertDef`, `InsertTarget`, and every effect DTO |
| `app/src/project.rs` | `BusDef`, `SendDef`, `RouteError`, the CRUD and the cycle walk |
| `app/src/tracks.rs` | `build_spec`: chains, buses and taps; the atomic edits |
| `app/src/state.rs` | `rebuild_graph`, and the command layer over all of it |
| `app/src/commands.rs` | §7, as Tauri commands |
| `app/src/project_file.rs` | version 4 and the migration |
| `app/src/clap_host.rs`, `vst3_host.rs` | `Rebuild` |
| `app/tests/mixer_graph.rs` | routing through the shell's own builder |
| `app/tests/clap_instruments.rs` | a routing edit against a real hosted plugin |
| `app/tests/routing_audio.rs` | §10's hardware captures |
| `app/tests/fixtures/v3.mydaw` | the committed migration fixture |
