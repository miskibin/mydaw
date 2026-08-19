# Patterns, the playlist, and the PAT/SONG transport

**Status:** implemented. Audio clips render as of the audio-clip step — §10 is
now what was built rather than what was owed.
**Crates:** `crates/rt-core`, `crates/audio-file`, `plugins/sequencer`,
`plugins/instruments`, `plugins/audio-io`, `app`.
**Supersedes the pattern half of:** step 4 (step sequencer), step 9 (piano
roll), step 8's format (version 2).

This is the document the UI is built against. §9 is the command contract; the
rest is why it looks like that.

---

## 1. What changed, in one paragraph

MYDAW had one pattern and one loop. It now has a **set** of named patterns
addressed by stable id, a **playlist** that places them along a song, and a
transport switch that says which of the two is playing. The drum kit was split
so that a rack channel is one instrument. Everything the audio thread sees is a
note; steps are an editing shorthand that the compiler expands.

---

## 2. The model

```text
Project                      SequencerService
  tracks: [TrackDef]           patterns: PatternSet { patterns: [Pattern], active }
    0 Kick     (kick)          playlist: Playlist   { tracks: [lane], clips: [Clip] }
    1 Snare    (snare)         bpm, mode
    2 Hat Closed (hat-closed)
    3 Hat Open (hat-open)     Pattern { id, name, color?, length_steps, channels: [
    4 Piano    (sampler|synth)   ChannelClip { channel, steps: Option<StepRow>, notes } ] }
      ^
      |__ a track index IS a note channel IS a mixer strip IS a pattern channel
```

**One number all the way down.** A rack channel, its mixer strip, the note
channel its events carry and the `channel` on a `ChannelClip` are the same
index. That is why deleting a track has to renumber every pattern
(`Pattern::retarget_after_removing`), and why the engine does it rather than
leaving the arrangement pointing at the wrong instrument.

**Ids, not indices, for anything a user or a file can name.** A pattern, a
playlist lane and a clip each carry a `u64` that is allocated once, never
reused, and survives a save. Positions change under insertion and deletion; a
clip addressed by one would silently repoint.

### 2.1 A pattern is a bag of channel clips

Before: `rows: Vec<PatternRow>` plus `notes: Vec<MelodicNote>`, each entry
carrying its own `channel`. After: one `ChannelClip` per channel the pattern
says anything about. Three things get easier and none get harder:

| question | flat | channel-keyed |
| --- | --- | --- |
| delete a channel | two `retain`s over two lists | one `retain` |
| what does channel 4 play here | two filters | a lookup |
| ghost notes | filter every note by channel | the other clips, right there |

A channel a pattern says nothing about has **no clip**, which is why an empty
pattern is a handful of bytes rather than sixteen empty rows.

A clip may carry a step row **and** notes; they sum. Not a state the UI offers,
but one the format can express, and refusing it would make a mode switch a lossy
round trip.

### 2.2 Steps are notes, written shorthand — the decision

`Pattern::compile` turns every switched-on step into a note at
`index * TICKS_PER_STEP` lasting `STEP_GATE_TICKS` (120 ticks, half a step).
**The audio thread only ever sees notes.**

The alternatives were both worse. Two scheduler paths is what the previous
design had, and it duplicated the loop-wrap, seek and note-off-parking
machinery in two places that had to agree. Steps as the only form cannot
express a note length, so the piano roll would have nothing to write into.
One source of truth where correctness is hardest to recover — the callback —
and two representations only where the *user* is, which is where two
representations are a feature.

The cost is one behaviour change, and it is an improvement: a step's gate used
to be `round(step_len * 0.5)` samples added to the boundary, and is now the
sample `STEP_GATE_TICKS` rounds to. They agree to within a sample, and the new
one is exact on the same tick grid as everything else.

**Display mode is derived, not stored.** `ChannelClip::display_mode()` is `roll`
when the clip has notes and `steps` otherwise — notes win, because a grid cannot
show a note length. It is sent on the wire (`ChannelClipDto.mode`) so the UI
does not re-derive the rule, and ignored on the way in: the content decides.

### 2.3 Time

| unit | where | why |
| --- | --- | --- |
| **tick** (`PPQ` = 960/quarter) | everything musical: note positions, clip starts and lengths | 960 divides by 2,3,4,5,6,8,12,16,24,32,48,60,64,96,120,192,240,480 — a triplet and a quintuplet are both exact integers |
| **step** = 240 ticks | the grid | a sixteenth |
| **bar** = 3840 ticks | the playlist's ruler | 4/4 |
| **sample** | the transport, the meter stream, `transport_seek` | what the device counts |

The design mock stores clip positions in **bars** and note positions in
**sixteenths**, and converts at every boundary. The engine has one tick domain
and converts exactly once, in `tick_boundary`. `SequencerState` carries `ppq`,
`ticksPerStep`, `ticksPerBar` and `sampleRate` so the UI can do the same
conversion from the same numbers.

### 2.4 Bounds

| constant | value | what it bounds |
| --- | --- | --- |
| `PATTERN_LENGTHS` | 16, 32, 64 | steps in a pattern's loop |
| `MAX_PATTERN_CHANNELS` | 16 | channel clips per pattern (= `MAX_TRACKS`) |
| `MAX_NOTES` | 1024 | melodic notes per channel clip |
| `MAX_PATTERNS` | 64 | patterns per project |
| `MAX_PLAYLIST_TRACKS` | 32 | arrangement lanes |
| `MAX_CLIPS` | 512 | clips in the arrangement |
| `MAX_SONG_TICKS` | 3 932 160 (1024 bars) | furthest a clip may run |
| `MAX_PENDING` | 4096 | note-offs the scheduler can owe at once |
| `SONG_QUEUE_CAPACITY` | 64 | in-flight song publications |

Every one is checked at the IPC boundary, so a webview bug is a status-bar
sentence rather than a truncated pattern.

---

## 3. One instrument per channel

A rack channel has one fader, one pan, one insert chain and one row in a
pattern. A four-drum node behind all of them means the snare cannot be turned
down, the hats cannot be panned, and the pattern row has to carry a key per hit
to say *which* drum — the one thing a step grid is not shaped to express.

`plugins/instruments/src/single_drum.rs` adds four factories reusing the kit's
own voices:

| `type_id` | instrument | key means |
| --- | --- | --- |
| `kick` | Kick | pitch, ±12 semitones from key 36 |
| `snare` | Snare | body-tone pitch, ±12 from key 38 |
| `hat-closed` | Hat Closed | nothing |
| `hat-open` | Hat Open | nothing |

Inside a kit the key chooses a drum, so it cannot also mean a pitch. Here there
is nothing to choose, so the key means what it means everywhere else. Clamped to
an octave: a kick at key 108 is 64 times its own frequency, which is not a drum,
and a stray key should sound wrong rather than inaudible. The hats ignore it —
a hi-hat is filtered noise, and "pitching" it moves a filter corner.

`drum-kit` stays registered so a project saved before the split still opens.
Nothing creates one.

**Names are the real ones.** These are oscillators and noise, so they are called
Kick and Snare. The design mock labels its channels "Kick 909" and "Clap 808"
over two generic engines; `Source::engine()` reports the factory kind —
`synth`, `sampler`, `clap`, `vst3`, `tone` — and that is what the UI badges.

**Cross-channel hat choke is not implemented.** Inside the kit a closed hat
chokes the open one, because they are one instrument. Across channels that means
one instrument reaching into another's voice state, which needs a cut-group
concept the engine does not have. Two hat channels ring independently, and
`a_closed_hat_channel_does_not_choke_a_separate_open_hat_channel` asserts it, so
adding cut groups later has a test to flip.

**The starter rack** is Kick / Snare / Hat Closed / Hat Open plus a melodic
channel — the sampled piano when its assets are installed, the poly synth (named
as such) when they are not.

---

## 4. The song scheduler

```text
   PAT   |<-- active pattern -->|<-- active pattern -->|<-- ...   forever

   SONG  |A....|A....|     |A....|          |B..|B..|B..|
         0     4     8     12                          (bars)
```

Both modes go through **one emitter**, `Scheduler::emit_run(pattern, base_ticks,
limit_ticks, ...)`. A pattern clip is "this pattern, from this tick, until this
tick"; pattern mode is the same call with a base of zero and no limit. One loop,
one set of note-off rules, one thing to get right.

**Nothing accumulates.** A position is `round(tick / 240 * step_len)` from the
**absolute** tick. There is no running total to drift, no phase to reset at a
loop point and no state to get wrong after a seek — a clip four hundred bars in
is the same arithmetic as the first bar of the first pattern. `tick_boundary` is
written with the division before the multiplication so that a tick on a step line
reduces *exactly* to `step_boundary`; the identity is asserted over ten thousand
steps at four tempos.

**Clip length is not pattern length.** Longer loops the pattern; shorter trims
it. A note that would ring past the clip is cut off at the clip edge — that is
what a clip boundary means, and letting a tail ring past a clip the user
shortened would be the clip not doing anything.

**Per-block cost** is `O(clips + log n + emitted)`: a linear pass over the clip
list (two comparisons each, bounded at `MAX_CLIPS`), then a binary search into
the pattern's sorted notes. The loop-iteration index is computed by division
rather than counted up from zero, so a clip far down the song costs no more than
one at the start.

**Song end.** SONG mode plays to the end of the last clip and stops — FL's
default, and the only one that does not need a loop region to be meaningful. The
scheduler cannot stop the transport (it has no clock, on purpose), so it reports
`song_ended()` and `run_block` acts. The stop edge on the *next* block flushes
the owed note-offs, which is exactly the path a user pressing Stop takes. A
playlist with **no clips at all** has no end, so it free-runs.

**Seek, stop and a mode switch** all flush every parked note-off at frame 0.
They mean the same thing: the notes still owed belong to a bar that is no longer
under the playhead.

**RT safety.** `CompiledSong` is built out of `Vec`s, allocated by
`Pattern::compile` on the editing thread, moved across by value as a
`basedrop::Owned`, and only ever *indexed* on the audio thread. The rule the
engine keeps is "no allocation on the audio thread", not "no heap": that costs
the callback one bounds check, exactly as a fixed array would, and costs the
editing thread the size of the song rather than the size of the largest song
imaginable. Thirty-two patterns of a thousand notes as
`[[CompiledNote; 1024]; 32]` would be a megabyte of mostly zeroes memcpy'd on
every keystroke in the piano roll. `plugins/sequencer/tests/rt_alloc.rs` measures
the claim with a counting allocator in both modes, across a mode switch, and
across a song swap.

---

## 5. Play mode

`PlayMode` lives in **`rt-core`**, beside the tempo, and for the same reason:
it is a property of *the transport*, not of whatever happens to be reading it.
The playhead, the scheduler, a future metronome and a UI badge all have to agree
on it, and putting it in the sequencer crate would mean either rt-core depending
on the sequencer or the mode travelling by a second route with a second
publication story — which is how a DAW ends up with a sequencer a block ahead of
its metronome.

It reaches the audio thread as `RtCommand::SetPlayMode`, through the same queue
as the tempo, so a mode change lands on a block boundary rather than halfway
through one. The session also holds it (`SequencerService::play_mode`), so it
survives a machine with no sound card and is republished when a stream comes up.

Wire names: `pat` and `song`. `pattern` is accepted as a synonym for `pat` on
the way in, because both spellings are obvious and a mode switch failing over a
synonym would be an infuriating bug.

---

## 6. Routing

| clip kind | goes to |
| --- | --- |
| pattern clip | wherever its pattern's channels go — the rack channels, and therefore their mixer strips |
| audio clip | the **master bus**, summed after every track strip and before the master insert and fader |

A playlist lane has a name and a mute and **nothing else**: no fader, no pan, no
insert chain, no output. It exists so overlapping clips can be drawn on separate
rows and muted in groups, which is what FL's playlist tracks are. That is why a
project can have five rack channels and twelve lanes without either number
meaning anything to the other — and why an audio clip goes to the master rather
than to "strip *n*, because the lane happens to be *n*-th", which would invent a
relationship the model does not have. Per-clip gain covers the common case. Real
audio-track routing is a later step and is a change to the graph rather than to
the playlist.

**A muted lane is resolved at compile time** — its clips are simply not in the
`CompiledSong` — so the callback never has to ask. Muting silences a lane; it
does not shorten the song, so the playhead still runs to the end of a muted
lane's last clip.

**Where the clip player actually sits.** A master insert already runs on the
summed bus before the fader, so the audio-clip node is compiled in as the
**first** master insert and *adds* to what the tracks produced. That is the
routing above, with no new concept in the graph and no change to
`CompiledGraph::process`:

```text
  track 0 ─▶ [strip] ─┐
  track 1 ─▶ [strip] ─┼─▶ Σ ─▶ AudioClipNode ─▶ InsertSlot ─▶ [master fader] ─▶ device
  track n ─▶ [strip] ─┘         (clips sum in)   (master FX)
```

Like the insert slot, the node is in **every** graph and is transparent when
there is nothing to play: gaining a first audio clip must not be a graph
rebuild. And because a rebuild *does* make a new node with an empty queue, the
shell's `AudioClipBus` remembers the last arrangement and replays it into every
node it opens — the arrangement is not the graph's to lose.

---

## 7. Project format version 3

```jsonc
{
  "format_version": 3,
  "bpm": 130.0,
  "master_gain_db": 0.0,
  "selected_track": 0,
  "tracks": [ /* unchanged from version 2 */ ],
  "master_effect": null,
  "play_mode": "pat",
  "patterns": [
    { "id": 1, "name": "Pattern 1", "color": "#4f8ef7", "steps": 16,
      "channels": [
        { "channel": 0, "mode": "steps",
          "steps": { "key": 36, "label": "Kick", "steps": [ {"on": true, "velocity": 0.78}, … ] },
          "notes": [] },
        { "channel": 4, "mode": "roll", "notes": [ {"key": 60, "start": 0, "length": 960, "velocity": 0.78} ] }
      ] }
  ],
  "active_pattern": 1,
  "playlist": {
    "tracks": [ { "id": 1, "name": "Track 1", "muted": false } ],
    "clips":  [ { "id": 1, "kind": "pattern", "track": 0, "start": 0, "length": 15360, "pattern_id": 1 } ]
  },
  "audio_sources": [ { "id": 1, "path": "C:\\takes\\vox.wav" } ]
}
```

`audio_sources` is a table rather than a path on each clip, because two clips of
the same take are the common case and a path repeated per clip is a path that can
disagree with itself. A file that has moved is a **warning** on load, not a
refusal — exactly as a missing plugin is.

**No version bump was needed for audio clips.** The table and the clip fields
have been in version 3 since this document was written; what changed is that
something now fills them. A save writes **every loaded source**, including one
the user has loaded and not yet placed — dropping it would make "load, save,
reload" lose a file the user chose. A load calls `restore_audio_sources`, which
retains exactly the ids the file names, re-decodes anything new, keeps anything
already loaded from the same path (re-opening a project must not re-decode its
takes), and puts a **silent placeholder** under the id of anything it cannot
read. Committed fixture: `app/tests/fixtures/v3-audio.mydaw`, whose second
source names a file that will never exist on any machine.

### Migration

Version 1 and 2 carried one unnamed pattern (`"pattern"`) and no arrangement.
`parse_project` turns it into `patterns: [ … ]` with id 1 and the name
"Pattern 1", and an empty playlist — which is exactly what those files meant,
because there was nowhere else for a pattern to be placed.

The interesting part is a channel that had **more than one row**, which is what
a drum-kit track looked like: the first row keeps the grid and the rest become
*notes*. That is the same translation `Pattern::compile` does, so the migrated
pattern produces the same events, note for note. The v1 fixture is a real
drum-kit project: three rows on channel 0 and one on channel 1 become two grids
(6 hits) and eight notes, and `pattern.compile().note_count() == 14` — the
fourteen events version 1 played.

Fixtures are committed (`app/tests/fixtures/v1.mydaw`, `v2.mydaw`) rather than
generated: the point of a migration test is to read the *format*, and a fixture
this build produced would only prove that this build agrees with itself.

Policy, unchanged: **migrate on load, write current on save.** A parsed file
always carries `FORMAT_VERSION`, so byte-identity holds from the second save.

---

## 8. What the UI reads: `SequencerState`

Every sequencer command returns this whole shape, so a panel never merges a
partial update into what it already had. The IPC spike measured a message this
size as free.

```ts
type SequencerState = {
  bpm: number;
  sampleRate: number;        // 0 when no device is open
  playMode: "pat" | "song";
  ppq: number;               // 960
  ticksPerStep: number;      // 240
  ticksPerBar: number;       // 3840
  patternSet: PatternSet;
  playlist: Playlist;
};

type PatternSet = {
  patterns: PatternSummary[];   // picker rows, in order
  activeId: number;             // always names one of them
  active: Pattern;              // that one, in full
  maxPatterns: number;
};

type PatternSummary = {
  id: number; name: string; color?: string;
  steps: number;         // 16 | 32 | 64
  lengthTicks: number;   // steps * 240
  channelCount: number; hitCount: number; noteCount: number;
};

type Pattern = {
  id: number; name: string; color?: string;
  steps: number;                 // 16 | 32 | 64
  channels: ChannelClip[];
};

type ChannelClip = {
  channel: number;               // rack channel = mixer strip = note channel
  mode: "steps" | "roll";        // derived; ignored on the way in
  steps?: StepRow;               // absent when the channel has no grid
  notes: MelodicNote[];
};

type StepRow = { key: number; label: string; steps: Step[] };  // exactly `Pattern.steps` cells
type Step = { on: boolean; velocity: number };                 // velocity 0..1, kept when off
type MelodicNote = { key: number; start: number; length: number; velocity: number };  // ticks

type Playlist = {
  tracks: PlaylistTrack[];
  clips: Clip[];
  endTicks: number;              // where SONG mode stops; 0 when empty
  maxTracks: number; maxClips: number; maxTicks: number;
};

type PlaylistTrack = { id: number; name: string; muted: boolean };

type Clip = {
  id: number;
  kind: "pattern" | "audio";
  track: number;                 // lane index
  start: number; length: number; // ticks
  patternId?: number;            // pattern clips only
  sourceId?: number;             // audio clips only
  offsetFrames?: number;         // audio clips only, in the file's own frames
  gainDb?: number;               // audio clips only
};
```

One `Clip` shape for both kinds rather than a discriminated union: the UI draws
them with the same geometry and only the fill differs, and a union would make
every consumer narrow a type to read `start`. The fields the other kind does not
have are simply absent from the JSON.

`color` is **absent** rather than `null` when the engine has none — the engine
has no palette, and inventing one would put a design decision in a file format.

---

## 9. Command contract

Every command below returns the whole `SequencerState` unless noted, and every
one **succeeds with no audio device open**: patterns and arrangements are project
data, and a user on a machine with no sound card must still be able to write a
beat. Errors are a sentence, shown as-is in the status bar.

### Patterns

| command | arguments | notes |
| --- | --- | --- |
| `get_pattern` | — | the whole state; call once at startup |
| `set_pattern` | `pattern: Pattern` | whole-pattern replacement. `pattern.id` names which; `0` or absent means the active one |
| `add_pattern` | `name?: string` | grid on every rack channel. **Does not switch to it** |
| `clone_pattern` | `patternId, name?` | copy lands directly after the original |
| `remove_pattern` | `patternId` | **and every clip that played it**. Refused for the last pattern |
| `rename_pattern` | `patternId, name` | blank refused |
| `set_pattern_color` | `patternId, color?` | `null` clears |
| `set_pattern_length` | `patternId, steps` | 16/32/64. Non-destructive — see below |
| `set_active_pattern` | `patternId` | what the grid, the roll and PAT mode address |

`set_pattern_length` shortening is **not** destructive: cells and notes past the
new end are kept and simply not played, so lengthening again gives them back.
The one exception is a note that would then hang past the bar line, which is
refused (a half-applied edit is worse than a rejected one). Note that a round
trip through `set_pattern` *does* lose the tail — the wire only carries the cells
that play — so a UI that wants the tail back must use `set_pattern_length`.

### Transport

| command | arguments | returns |
| --- | --- | --- |
| `set_play_mode` | `mode: "pat" \| "song"` | `SequencerState` |
| `transport_play` / `transport_stop` | — | `void` |
| `transport_seek` | `samplePos: number` | `void` |
| `set_bpm` | `bpm: number` | `number` (the stored value) |

`transport_seek` takes **samples**, because that is what the transport counts and
what the meter stream reports back; converting in two places would let the
playhead the UI draws and the playhead the engine plays disagree by a rounding.
`SequencerState` carries `ppq`, `ticksPerStep` and `sampleRate` so the UI can
convert once, itself, from the same numbers.

`set_bpm` **rejects** out-of-range values rather than clamping: a user who typed
5000 should be told, not silently given 999.

### The playlist

| command | arguments | notes |
| --- | --- | --- |
| `add_playlist_track` | `name?` | |
| `remove_playlist_track` | `track: number` | **and every clip on it**; lanes below shift up. Refused for the last lane |
| `rename_playlist_track` | `track, name` | blank refused |
| `set_playlist_track_mute` | `track, muted` | silences it; does not shorten the song |
| `add_pattern_clip` | `patternId, track, start, length?` | ticks. `length` absent means four bars |
| `remove_clip` | `clipId` | |
| `move_clip` | `clipId, track, start` | |
| `resize_clip` | `clipId, length` | zero refused |

Removing a lane takes its clips because a clip's lane is its *position*, not a
reference: one left behind would have to move somewhere, and every somewhere is
a silent rewrite of the arrangement.

### Audio clips and the files behind them

| command | arguments | returns |
| --- | --- | --- |
| `load_audio_source` | `path: string` | `AudioSource` |
| `get_audio_sources` | — | `AudioSource[]` |
| `unload_audio_source` | `sourceId` | `AudioSource[]` |
| `get_audio_waveform` | `sourceId, targetBins?` | `Waveform` |
| `add_audio_clip` | `sourceId, track, start, length?, offsetFrames?, gainDb?` | `SequencerState` |
| `set_clip_gain` | `clipId, gainDb` | `SequencerState` |
| `set_clip_offset` | `clipId, offsetFrames` | `SequencerState` |

`move_clip`, `resize_clip` and `remove_clip` take an audio clip exactly as they
take a pattern clip — one id space, one set of commands, which is the point of
`Clip` being one shape.

```ts
type AudioSource = {
  id: number;
  path: string;            // where the file was when it was loaded
  name: string;            // its file name, for a UI with no room for a path
  frames: number;          // in the file's own rate; 0 when the file has moved
  sampleRate: number;      // 0 when the file has moved
  durationSeconds: number;
  durationTicks: number;   // how long it is at the session's *current* tempo
  missing: boolean;        // its clips are in the arrangement and silent
  bytes: number;
};

type Waveform = {
  sourceId: number;
  frames: number;
  bins: number;            // may be fewer than asked for; see below
  maxBins: number;         // 4096
  peaks: [number, number][];   // [min, max] per bin, over both channels
};
```

```jsonc
// load_audio_source({ path: "D:\takes\vox-4.wav" })
{ "id": 1, "path": "D:\takes\vox-4.wav", "name": "vox-4.wav",
  "frames": 148676, "sampleRate": 44100, "durationSeconds": 3.371,
  "durationTicks": 7014, "missing": false, "bytes": 1189440 }

// get_audio_waveform({ sourceId: 1, targetBins: 4 })
{ "sourceId": 1, "frames": 148676, "bins": 4, "maxBins": 4096,
  "peaks": [[-0.41, 0.39], [-0.28, 0.30], [-0.19, 0.21], [-0.07, 0.08]] }
```

**`load_audio_source` returns the source, not `SequencerState`**, because
nothing about the arrangement changed: loading a file and placing it are two
actions, and a UI that wants a browser needs the first without the second.
Loading a path that is already loaded returns the source that is already there,
so dragging one loop onto four lanes decodes once.

**`length` absent on `add_audio_clip` means "the whole file at the current
tempo"** — `durationTicks`, which moves with the tempo because a tick does. The
audio does not stretch either way; see §10.

**`offsetFrames` is in the file's own frames**, not ticks, because it is a
position in a recording rather than in the song. Trimming the head of a vocal
and then changing the tempo must not start it on a different syllable.

**`unload_audio_source` is refused while a clip still plays the file**, and the
message says how many — silencing part of an arrangement without saying so is
the failure mode this exists to prevent. Removing the last clip does *not*
unload the file: a user who deletes a clip and draws it again a second later
should not pay for a second decode.

**`targetBins` is a request, not a promise.** A file with less detail than that
gets what it has (there is no information below the finest mip level, and
inventing bins would be drawing a waveform the file does not have), and anything
past `maxBins` is clamped.

### The rack, and what it does to patterns

`add_track` and `remove_track` are unchanged in signature and now keep the
patterns in step: adding a channel gives every pattern an empty grid on it (with
the drum's General MIDI key when the channel holds a drum, middle C otherwise),
and removing one drops its clip from every pattern and shifts the channels above
it down. A channel that existed in the mixer and not in the pattern would be a
row nobody could click in.

### Error vocabulary

| situation | message shape |
| --- | --- |
| unknown pattern | `there is no pattern 7` |
| last pattern | `a project must keep at least one pattern` |
| too many | `a project may have at most 64 patterns` |
| bad length | `a pattern must be 16, 32 or 64 steps long, got 24` |
| short step row | `channel clip 0 declares 15 steps; the pattern is 16 long` |
| two clips, one channel | `channel 3 has two clips in the same pattern` |
| note past the bar | `note 0 on channel 4 runs to tick 4000 but the pattern is only 3840 ticks long` |
| unknown clip / lane | `there is no clip 9` / `there is no playlist track 9` |
| clip off the end | `a clip may not run past tick 3932160; this one ends at …` |
| dangling clip | `clip 3 names pattern 99, which is not in this project` |
| audio thread behind | `the song queue is full; the audio thread has not picked up previous edits` — retryable |
| unreadable file | `D:\takes\vox.wav is not an audio file this build can read: …` |
| file too long | `… is 812.4 s long; a source may be at most 600 s` |
| too many files | `a project may hold at most 64 audio sources` |
| unknown source | `there is no audio source 7` |
| source in use | `audio source 1 is still played by 3 clip(s)` |
| gain on a pattern clip | `there is no clip 4` — `set_clip_gain` and `set_clip_offset` are audio-only |

---

## 10. Audio clips

The one item this phase declared rather than delivered. It is delivered now:
files decode into a registry, clips place them on the timeline, a fixed voice
pool renders them into the master bus, and the UI can draw them.

### 10.1 The source registry

`load_audio_source(path)` decodes a **whole file into RAM** as interleaved
stereo `f32` at its own rate, and hands back a stable id that clips reference and
the project file stores. WAV, FLAC and Ogg Vorbis.

Three decisions, each the same one the sampler made and for the same reason:

- **All of it in memory.** Streaming from disk in the callback is a syscall in
  the callback, and the alternative — a prefetch thread with a ring per voice —
  is a lot of machinery for clips that are loops and takes. One file is capped
  at `MAX_SOURCE_SECONDS` (ten minutes, 230 MB at 48 kHz stereo) and a project
  at `MAX_SOURCES` (64), so "how much audio can this hold" has an answer rather
  than being unbounded.
- **Nothing is resampled at load.** A file keeps its own rate and the voice folds
  the device ratio into the step it computes anyway. Resampling at load is slow
  to open and has to be redone on every device change.
- **Padded, so the interpolator needs no bounds check.** One silent frame before
  the content and two after, which is what makes frame 0 and frame *n*−1 legal
  taps without a branch in the inner loop.

**MP3 is absent for a build reason, not a policy one.** `symphonia-bundle-mp3`
has no released 0.6 — only `0.6.0-alpha.2` — so enabling the feature makes the
workspace unresolvable. `decode_stereo` *probes* rather than switching on the
extension, so the day that crate ships, adding `"mp3"` to the workspace feature
list is the whole change.

**A missing file is a warning, not a refusal.** Its id gets a **silent
placeholder** of zero frames, so every clip that referenced it keeps its place in
the arrangement and can be re-pointed at the file once it is found. Deleting the
clips instead would make "I moved my samples folder" a destructive edit.

The decode itself moved to a new crate, `crates/audio-file`: the sampler
(instrument recordings) and the sequencer (playlist clips) needed the same probe
loop, the same channel policy and the same Catmull-Rom kernel, and two copies is
two places for them to drift. `plugins/sampler/src/decode.rs` is now the
sampler's *policy* over it — the manifest's frame cap and the fade that hides the
cut — and nothing else.

### 10.2 The voice pool

`AUDIO_CLIP_VOICES` = 32 voices, fixed, allocated once. A clip voice is four
multiplies and two array reads per frame, so the number is musical rather than
budgetary: thirty-two simultaneous takes is a dense arrangement.

**Sample-accurate at both ends.** A clip's first frame is
`tick_boundary(start_ticks, step_len)` — character for character the arithmetic
the note scheduler uses, so an audio clip on bar three and a kick on bar three
land on the *same sample*. A voice carries the absolute sample it is due at, so a
clip whose tick falls 22 frames into a 512-frame block sounds on frame 22 rather
than on frame 0.

**Nothing accumulates.** The clip's end and the file's end are both recomputed
from the absolute tick and the voice's current position **every block**, so a
tempo change mid-clip moves the fade to where the new tempo puts it rather than
where the old one implied. A clip stops at whichever of the two comes first: a
clip longer than its file ends when the file does, and a shorter one cuts it off.

**Seeking lands inside a clip.** On a seek, a play edge, or a switch into SONG
mode, every clip *covering* the new position starts a voice at

```text
position = offset_frames + (sample_pos − clip_start_sample) × step
```

so dropping the playhead halfway through a vocal continues the vocal.

**No clicks.** Every voice fades in over `CLIP_RAMP_MS` (2 ms) and out over the
same, and a stop, a seek, a steal or a deletion starts the fade rather than
cutting. The fade-out *begins* `ramp` frames before the end, so silence is
reached exactly at the clip's last sample rather than two milliseconds after it.

**A deleted clip has samples to fade out of.** The superseded arrangement is
kept — `retiring` — until no voice is still reading it, because a two-millisecond
fade needs two milliseconds of file. An edit that leaves a clip alone (moving a
*different* clip, changing a gain, changing a length) **rebinds** the voice
rather than restarting it: the comparison is file, start tick and offset, which
are the only three things that can invalidate a read position.

**Pool exhaustion drops the oldest and counts it.** `AudioClipStats::voices_stolen`
is an atomic; dropping the *newest* instead would make an arrangement's most
recent edit the one that does not play, which is the wrong thing to lose.

**Zero allocation, measured.** `plugins/sequencer/tests/rt_alloc.rs` drives the
node through starts, steals, seeks, stops and mode switches at eight block sizes
under a counting allocator, and swaps the arrangement underneath it — the
superseded one goes to the `basedrop` collector, because the last
`Arc<AudioSource>` in it can be a hundred megabytes and `free()` in a callback is
a dropout.

### 10.3 The tempo does not stretch audio

A clip's **position** is in ticks and moves with the tempo; its **content** plays
at `sample_rate / device_rate` source frames per output frame and does not.
Changing 120 → 140 BPM moves the vocal earlier and leaves the singer's pitch
alone. That is what FL, Live and Logic all default to for an un-warped clip.

**There is no time-stretch here**, and adding one is a separate step with its own
algorithm and its own artefacts rather than a flag on this node. The consequence
worth stating: a tempo *increase* shortens a clip's tick length below the audio
it was drawn around, which cuts the tail — the same rule as a clip that outlasts
its file, resolved the same way.

Interpolation is Catmull-Rom, four taps, shared with the sampler. It is exact at
the sample points, so a file at the device's own rate is bit-for-bit playback
rather than an approximation of it.

### 10.4 Waveforms

`get_audio_waveform(sourceId, targetBins?)` returns `[min, max]` pairs. Three mip
levels are built at load — 256, 2 048 and 16 384 frames per bin, costing one pass
over the audio plus a geometric tail — and a query is served from the **coarsest
level that still has at least `targetBins` bins**, folded down by whole bins. So
the answer is always the true min and max of the frames it covers, never a
resample of a resample.

One pair per bin **over both channels**, not one per channel: a clip on a
timeline is drawn as a single envelope at these sizes, and the outer bound never
hides a transient that is on one channel only.

JSON, not a binary blob: the IPC spike measured a 64 kB message as free and
`MAX_WAVEFORM_BINS` (4 096) pairs is roughly 40 kB, for a payload a UI fetches
once per source rather than per frame. A base64 `Float32Array` would be about a
third of the size and would cost every consumer a decode step; the seam to add it
later is `WaveformDto`, and nothing else would change.

### 10.5 What is still not here

- **No cross-channel choke** (cut groups). See §3.
- **No loop region.** SONG mode plays to the end and stops; PAT mode loops
  forever. A loop toggle is a later step.
- **No time signature.** 4/4 is assumed everywhere a bar is mentioned, and
  `PATTERN_LENGTHS` is whole bars for exactly that reason.
- **Playlist lanes have no output.** See §6. An audio clip goes to the master
  bus; per-clip gain covers the common case, and real audio-track routing is a
  change to the graph rather than to the playlist.
- **No time-stretch and no per-clip pitch.** See §10.3.
- **A clip dragged under a running playhead starts at its next boundary**, not
  immediately. Starts happen at clip boundaries and on a seek; an edit keeps
  playing what was already playing. The same rule the pattern scheduler follows.
- **MP3.** See §10.1 — a dependency that has not shipped, not a decision.

---

## 11. Verification

### Unit and offline

`cargo test --workspace`: **1297 passed, 0 failed, 21 ignored** after audio
clips (1229 / 0 / 20 before them; 1113 / 0 / 19 before the phase).

The load-bearing ones:

| claim | where |
| --- | --- |
| a tick on a step line lands on exactly the step boundary, 10 000 steps × 4 tempos | `schedule.rs` |
| every step on its boundary at 2 rates × 4 tempos × 4 block sizes | `schedule.rs` |
| ten minutes at 174.3 BPM with no drift | `schedule.rs` |
| a clip sounds at its own tick and nowhere else, 3 tempos × 4 block sizes | `schedule.rs` |
| a clip longer than its pattern loops it; shorter trims it | `schedule.rs` |
| a note ringing past its clip is cut at the clip edge | `schedule.rs` |
| seeking mid-clip resumes on the grid, forwards and backwards | `schedule.rs` |
| a clip 400 bars in costs no more than one at the start | `schedule.rs` |
| the song ends after its last clip and says so | `schedule.rs` |
| `MAX_PENDING` overflow is counted, not silent | `schedule.rs` |
| zero allocation in both modes, across a mode switch, across a song swap | `tests/rt_alloc.rs` |
| a superseded song is freed by the collector, not the audio thread | `compiled.rs` |
| pattern CRUD, id reuse, cursor behaviour | `set.rs` |
| clip CRUD, lane removal, bounds | `playlist.rs` |
| v1 and v2 migration against committed fixtures | `project_file.rs` |
| a second row on one channel becomes the notes it always meant | `project_file.rs` |
| per-drum pitch reaches the samples (zero-crossing rate doubles an octave up) | `single_drum.rs` |

Audio clips added these, and they are the load-bearing half of §10:

| claim | where |
| --- | --- |
| a clip sounds at the sample its start tick names, 2 rates × 4 tempos × 4 block sizes | `clips.rs` |
| a clip reaches silence *at* its end, and frees its voice | `clips.rs` |
| a clip that outlasts its file ends when the file does | `clips.rs` |
| seeking mid-clip resumes at the right source frame (±1 frame, offset honoured) | `clips.rs` |
| a seek landing before a clip starts it at its own offset, not mid-file | `clips.rs` |
| 441 Hz at 44.1 kHz on a 48 kHz device measures 441 Hz (zero crossings) | `clips.rs` |
| no adjacent-sample step at any start or stop exceeds one ramp increment | `clips.rs` |
| an edit elsewhere rebinds a sounding voice instead of retriggering it | `clips.rs` |
| a deleted clip fades out of the retiring arrangement rather than cutting | `clips.rs` |
| pool exhaustion steals the oldest and counts it | `clips.rs` |
| nothing sounds in PAT mode or with the transport stopped | `clips.rs` |
| the node **sums into** the bus rather than replacing the track mix | `clips.rs`, `app/src/clips.rs` |
| zero allocation across starts, steals, seeks, stops and mode switches, 8 block sizes | `tests/rt_alloc.rs` |
| a superseded arrangement is freed by the collector, not the audio thread | `tests/rt_alloc.rs` |
| a waveform query matches a brute-force scan of the samples | `audio.rs` |
| every coarser mip level bounds the one below it | `audio.rs` |
| a waveform never hides a transient that is on one channel only | `audio.rs` |
| one file loaded twice is one decode | `audio.rs` |
| unloading a referenced source is refused and says how many clips | `audio.rs` |
| WAV decode: mono duplicated, stereo kept apart, cap honoured, garbage refused | `crates/audio-file` |
| a graph rebuild does not lose the arrangement | `app/src/clips.rs` |
| a v3 file's audio clips and source table survive a save and reopen | `project_file.rs` |
| a moved file is a warning and a silent source, not a failed load | `project_host.rs` |
| a save writes a source the user loaded and has not placed | `project_host.rs` |

**FLAC and Ogg have no encoder in this workspace**, so their decode test is
skip-if-absent: point `MYDAW_AUDIO_FIXTURES` at a directory of audio files, or
let it find installed sampler content, and it decodes up to eight of them. With
neither it prints a skip and passes — CI has no sample content and must not fail
for it. Hand-rolling a valid FLAC frame would be testing the fixture rather than
the decoder.

### Hardware

`cargo test -p mydaw-app --test song_mode_audio -- --ignored --nocapture`

Focusrite USB Audio, 44 100 Hz, 2 ch, max block **4096**. 130 BPM: step
5088.462 samples (115.385 ms), bar 1.8462 s. Rack: Kick / Snare / Hat Closed /
Hat Open / Poly Synth. Song: 2 patterns, 3 clips, ends at tick 30 720 (8 bars).

| window | measurement |
| --- | --- |
| bars 0–3 | 12 kick onsets, worst **0.010 ms (0.5 samples)**, mean 0.006 ms, contiguous, 0 dropped |
| bars 0–3, >5 kHz | **−51.95 dBFS** rms — pattern B is in the project and silent |
| seek to bar 5 (sample 407 077) | |
| bars 5–6 | 8 kick onsets, worst **0.010 ms**, contiguous |
| bars 5–6, >5 kHz | **−40.40 dBFS**, **+11.54 dB** — pattern B entered |
| bars 5–6, onsets >5 kHz | 16, on the eighth grid, worst **0.136 ms (6 samples)** |
| whole capture | peak −9.07 dBFS, 0 clipped, 0 non-finite, 0 dropped |

#### Audio clips

`cargo test -p mydaw-app --test audio_clip_audio -- --ignored --nocapture`

Focusrite USB Audio, 44 100 Hz, 2 ch, max block **4096**. 130 BPM: step
5088.462 samples, beat 20 353.846, bar **81 415 samples** (1.8461 s).

The source is **rendered by the engine through the device first** — two bars of
the open hat, captured, trimmed to its first transient (14 154 frames), written
as a WAV and loaded back — so the file under test is one this build made rather
than an asset whose provenance the test would have to take on faith. 148 676
frames, 3.371 s. The arrangement is the kick on every beat for eight bars, that
file at **bar 3** (song sample 244 246), and a 440 Hz tone recorded at **32 000
Hz** at bar 6.

| window | measurement |
| --- | --- |
| hat band (>5 kHz), bars 0–3 | **−51.95 dBFS** rms — the kick's click alone |
| hat band, bars 3–5 | **−40.05 dBFS**, **+11.90 dB** — the clip entered |
| placement, correlated against the file | device is playing source frame **40 707** where the file says 40 707 — **error +0 samples (0.000 ms)**, r = **0.964** |
| timbre | dominant partial **19 346.6 Hz** in the file → **19 144.7 Hz** out of the device, **−18.2 cents** |
| pitch across a rate change | 440 Hz at 32 kHz on a 44.1 kHz device → **440.00 Hz (+0.0 cents)** |
| seek 2.5 beats into the clip (song sample 295 130) | device is playing source frame **54 884**; the seek asked for 54 884 — **error +0 samples**, r = **1.000** |
| every capture | 0 dropped, 0 clipped, 0 non-finite; peaks −7.54 to −5.19 dBFS |

**Placement is measured by cross-correlation, not by onset detection**, and that
is a finding rather than a preference. The kick lands on bar 3 as well, and its
two-millisecond noise click has *the same peak in the hat band as the hat does* —
measured at 0.10326 for both — so no threshold in any band separates them. Band
**energy** separates them (a click does not ring for 350 ms), which is the
+11.90 dB row; **alignment** is the lag at which the device's output matches the
file, which is a sample count and needs no detector at all.

The first attempt did use an onset detector and reported −1.86 ms, then +4.24 ms
after two rounds of "fixing" it. All of that was detector latency — the transient
detector's window differs between the low band and the high band, and a two-pole
120 Hz lowpass carries about 2.6 ms of group delay into any number subtracted
from it. The correlation says the error is zero samples, which is what the
offline tests say it should be. The lesson is the general one: when the question
is "how far apart are these two events", measure both with the *same* instrument,
or measure the waveform itself.

The capture's own origin is knowable because Play is pressed from a second
thread **after** the capture has armed: the engine is silent while stopped, so
the first sound in the capture is song sample 0 and every measurement is relative
to it.

"Is pattern B playing" is measured as **band energy**, not as onsets: the kick's
two-millisecond noise click is broadband, so it registers above 5 kHz too, and no
detector can tell one instrument's transient from another's. An open hat rings
for 350 ms; energy can.

The first hardware run found a real bug. Five song edits inside one 4096-frame
block (93 ms) overflowed the four-deep publish queue and the fifth came back as
an error. The queue only has to absorb a *burst* — the slot keeps the last entry
and drops the rest — so its depth is "how many edits can a hand make in one
block", which is not four. It is 64 now.

---

## 12. Files

| file | what |
| --- | --- |
| `plugins/sequencer/src/pattern.rs` | `Pattern`, `ChannelClip`, `StepRow`, `MelodicNote`, the compile |
| `plugins/sequencer/src/set.rs` | `PatternSet`: ids, the cursor, CRUD |
| `plugins/sequencer/src/playlist.rs` | `Playlist`, lanes, `Clip` |
| `plugins/sequencer/src/compiled.rs` | `CompiledSong`, `CompiledPattern`, the basedrop channel |
| `plugins/sequencer/src/schedule.rs` | `Scheduler`, both modes, the tick arithmetic |
| `plugins/sequencer/src/lib.rs` | `SequencerService`: the session's copy, edit funnels |
| `plugins/instruments/src/single_drum.rs` | the four per-drum instruments |
| `crates/rt-core/src/transport.rs` | `PlayMode`, on the clock |
| `crates/rt-core/src/command.rs` | `RtCommand::SetPlayMode` |
| `plugins/audio-io/src/rt.rs` | the callback: song slot, scheduler, song-end stop |
| `app/src/dto.rs` | every wire shape in §8 |
| `app/src/commands.rs` | every command in §9 |
| `app/src/state.rs` | `edit_patterns` / `edit_playlist`, channel sync, the starter set |
| `app/src/project_file.rs` | version 3 and the migration |
| `app/tests/song_mode_audio.rs` | the hardware capture in §11 |
| `app/tests/fixtures/v1.mydaw`, `v2.mydaw` | committed migration fixtures |
| `crates/audio-file` | decode a file to stereo `f32`, the padding, the Catmull-Rom kernel — shared with the sampler |
| `plugins/sequencer/src/audio.rs` | `AudioSource`, `AudioSourceRegistry`, the waveform mipmaps |
| `plugins/sequencer/src/clips.rs` | `CompiledAudioSong`, the 32-voice pool, the `RtNode` on the master bus |
| `app/src/clips.rs` | `AudioClipBus`: the shell's end, and the replay across a graph rebuild |
| `app/tests/audio_clip_audio.rs` | the audio-clip hardware capture in §11 |
| `app/tests/fixtures/v3-audio.mydaw` | committed audio-clip + source-table fixture |
