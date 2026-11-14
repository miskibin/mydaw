# `wave` — the command line

The CLI works in two ways, with the same commands:

* **The running app** (the default): it edits the project you see, with the same undo history.
* **A project file** (`--project FILE`, or `-p`): no app, no window and no audio device. The
  file is loaded, the command runs, and the file is saved back if the command changed it.
  See [Project files without the app](#project-files-without-the-app).

There is nothing to install: on start the app puts a `wave` launcher on your PATH
(`%LOCALAPPDATA%\Microsoft\WindowsApps\wave.cmd` on Windows, `~/.local/bin/wave` elsewhere).
Settings → CLI shows where it went. Open a new terminal after the first start and run:

```sh
wave status
wave context --pretty
```

stdout is only JSON (`--pretty` to indent), with numbers rounded to at most four decimal places
(0.47, not 0.4699999988); messages go to stderr. Exit codes:
`0` ok, `1` bad usage, `2` WAVE not running / unreachable, `3` refused, as
`CODE: message` (`BAD_OPS`, `REVISION_CONFLICT`, `NEEDS_APP`, `FILE_OPEN` …).

## Project files without the app

```sh
wave new song.wave --template sketch --bpm 140      # or --template empty (default); --force replaces a file
wave -p song.wave context                           # any read: nothing is written
wave -p song.wave pattern --id 1 --ch kick --steps "x... x... x... x..."   # saved back to song.wave
wave -p song.wave render --out song.wav             # or --stems --out stems/
wave -p song.wave export-project --out song.zip      # without saving changes to song.wave
wave -p song.wave bpm 150 --save-as song-150.wave   # song.wave stays as it was
wave -p song.wave apply --file ops.json --dry-run   # try it: the reply has dryRun.changed, nothing is saved
```

Every reply on a file has `saved`: the path written, or `null`. Audio that could not be loaded
is printed as a warning on stderr and listed under `loadWarnings`; the command still runs (the
missing sources play silence, as in the app).

- **What is refused.** `play`, `stop`, `pattern --play`, `comp --audition`, `undo`, `redo` and
  `open` answer `NEEDS_APP`: they need the audio device or the app's undo history, which a file
  does not have. Listen by rendering; try edits with `--dry-run` or `--save-as`.
- **A file the app has open.** An edit to a file that a running WAVE has open is refused with
  `FILE_OPEN`: the app keeps its own copy and would overwrite the edit on its next save. Edit it
  in the app (drop `--project`), close it there, or pass `--force`. Reading and rendering it are
  fine.
- **Saving.** The save works as it does in the app: a `(backup)` copy of the previous file, and
  recordings moved next to the project. `save` saves even an unchanged file; `save --path` is
  the same as `--save-as`.
- **What is the same.** Renders and stems are the same bounce as the app's export (the WAVE Keys
  piano is loaded, or the command fails with `KEYS_MISSING` instead of rendering silence;
  pitch-edited clips use the installed RMVPE model as the app does). VST3 effects load from the
  scan cache. `vst3-params` and `vst3-set` work, and the plugin's state is saved in the file.

### Share an editable project

Run `wave export-project --out song.zip` to package the current project in the running app,
including unsaved edits. Use `-p song.wave` to package a saved file. Unzip the archive and open
the `.wave` project with its adjacent `Audio` folder. Plugin settings travel in the project.
Install the plugins separately at the locations stored in the project. A missing audio source
fails the export and any existing output ZIP is preserved.

### Scripts: many commands, one load

```sh
wave -p song.wave script edits.txt      # or `script -` to read stdin
```

```text
# edits.txt: one command per line, quoted like a shell ('…' or "…", no escapes)
bpm 128
pattern --id 1 --ch hat --steps "x.x. x.x. x.x5 x.x9"
apply --json '[{"op":"setLoop","loop":{"a":0,"b":16}}]'
render --out preview.wav
```

On a file the project is loaded once and saved once, at the end. The first line that fails stops
the script before the project is saved. The error names the failing line and command. Renders and
ZIP exports write their own output when their line runs. For ZIP export, omit `--dry-run`.
The reply lists each line's result. Without `--project`, a script runs against the running app line
by line: lines before a failing one stay applied, each as its own undo step. Options of the whole
run (`--project`, `--session`, `--save-as`, `--dry-run`, `--force`, `--pretty`) go before
`script`, not on its lines; a leading `wave` on a line is ignored. Relative paths in a line are
relative to the current folder.

## Reading

| command | what you get |
| --- | --- |
| `wave status` | transport, revision, file, audio device, CLI path |
| `wave context` | summary: tracks (index, insert, volume, clip count), inserts with effects and parameters, patterns, channels, sources, `nextId` |
| `wave channels` | the Channel Rack: id, name, `sample` (+ source and sampler settings)/`synth` (+ preset)/`keys`, volume, pan, mute/solo, mixer insert (`null` = the clip's track), root key, note count |
| `wave pattern [--id N]` | a pattern as one step string per channel (`x` full velocity, `1`–`9` = 10–90 %, `.` empty, a space every beat) plus `notes` for melodic rows, bars, minimum bars, swing % |
| `wave context --full` | the whole project JSON (every clip, note, parameter) |
| `wave catalog` | effect types, parameter ids/ranges/units, presets, command list |
| `wave schema` | every op `apply` accepts, with units and an example |
| `wave analyze [--clip ID] [--exclude-fx ID]` | detect tempo (BPM, ½×/2× candidates) and musical key (top 3 candidates, 12-note chroma, tuning offset) |

## Editing

Each command is one undo step. Add `--if-revision N` (the `revision` from your
last read) and the edit is refused with `REVISION_CONFLICT` if the musician
changed something meanwhile — re-read and decide, don't blindly retry.

```sh
wave bpm 140
wave add-fx --insert 6 --type reverb --preset 3          # → {"fxId":1203,…}
wave set-param --fx 1203 --param mix --value 30
wave preset --fx 1203 --index 4
wave import kick.wav loop.wav --track 3 --beat 16
wave make-unique --clip 5012                           # own copy of the sample on a new track (same routing)
wave apply --file edit.json --label "Busier hats"
wave undo
```

`edit.json` is an array of ops (see `wave schema`), applied atomically:

```json
[
  {"op": "putPattern", "pattern": {"id": 2, "name": "Hats", "color": "#e0a44a", "len": 4, "swing": 0.4,
    "notes": [{"ch": 1002, "step": 0, "len": 1}, {"ch": 1002, "step": 2, "len": 1, "v": 0.6}]}},
  {"op": "setLoop", "loop": {"a": 0, "b": 16}}
]
```

Every note names its channel with `ch` (see `wave channels`).

Units: playlist time in **beats**; pattern `step`/`len` in **sixteenths**;
fades in seconds; track/insert `vol` is a fader position (0.7937 = 0 dB);
clip `gain` in dB; audio clip `pitch` in semitones (±12, cents as the fraction; the
length stays) and `stretch` 0.25–4; pan −1..1. New ids: start at `context.nextId`.

## Patterns and channels

Channels (Channel Rack rows) are shared by all patterns; a note plays channel `ch`.
Steps are sixteenths. A pattern is as long as its notes (the end of the last note rounded
up to a bar, like FL Studio); `--bars N` sets the shortest it may be, for empty bars kept on
purpose (a crash every second bar). Swing 0–100 % (delays every odd 16th by up to half a
step). Each edit is one undo step.

```sh
wave channels                                     # ids, kinds, routing
wave channels --add snare-studio                  # a sampler of a standard-library sample (file or display name, or the start of one: "kick")
wave channels --ch "Studio Snare" --param cut --value 1   # one sampler setting: start, end, reverse, norm, semi, fine, gate, attack, decay, sustain, release, cut
wave channels --ch "Studio Snare" --param fadeOutMs --value 80   # fadeInMs and fadeOutMs in source milliseconds
wave channels --sample 1001                       # a sampler channel playing source 1001
wave channels --synth "808 Czysty"                # a synth channel with a factory preset ("" = blank patch)
wave channels --ch 1005 --preset "Pad Ciepły"     # load a synth preset (ids/names in `wave catalog` → effects.synth)
wave channels --ch 1005 --param flt_cutoff --value 900   # one synth knob
wave channels --keys                              # WAVE Keys: the sampled grand piano (see docs/keys.md)
wave channels --ch "WAVE Keys" --param release --value 600   # its knobs: release (ms), dynamics (%)
wave channels --remove 1004                       # (or a name) — also removes its notes
wave pattern --id 2                               # step strings per channel
wave pattern --id 2 --ch hat --steps "x.x. x.x. x.x5 x.x9"     # replace the hat's notes
wave pattern --id 2 --ch kick --steps "x... x... x... x..." --vel 0.9
wave pattern --id 3 --ch bass --notes '[{"step":0,"len":3,"p":33},{"step":4,"len":2,"p":40,"v":0.8}]'
wave pattern --id 2 --ch hat --clear
wave pattern --id 2 --bars 2 --swing 55 --name "Hats B"
wave pattern --new --name Fill --bars 1           # → its id in the reply
wave pattern --id 2 --play                        # loop it on its own (stops the song); --stop
wave pattern --id 2 --play --from 4 --loop 4:8    # from beat 4, looping beats 4–8 of it
```

`--ch` takes a channel id or name (`kick`, `Hats 2` …). With `apply`, the ops are
`putPattern` / `putChannel` / `removeChannel` (see `wave schema`). When a pattern's
length changes (notes added past its end or removed, `--bars`), clips that show exactly one
whole pass of it follow; clips sized on purpose keep their length.

`--steps` creates explicit triggers. A sampler trigger rings out even in Gate mode.
`--notes` creates held notes by default. Include `"trig":true` on any trigger in a mixed
row. When a row contains held notes or cannot be represented as a step string, the reply's
`notes` array carries `trig` for each event.

## Takes and comping

A track you recorded with a loop set (one take per pass), or recorded over
an existing clip, has a comp clip. Times here are **timeline beats**,
crossfades **milliseconds**; add `--clip ID` when a track has several comps.

```sh
wave takes --track 5                       # takes, comp regions, crossfades
wave comp --track 5 --set '[{"take":2001,"from":32,"to":36},{"take":2002,"from":36,"to":40,"xfadeMs":20}]'
wave comp --track 5 --take 2003            # one whole take
wave comp --track 5 --xfade 10             # default crossfade at region boundaries
wave comp --track 5 --remove-unused        # drop takes the comp does not play
wave comp --track 5 --audition 2002        # hear one take instead of the comp (`off` to stop; not an edit)
wave comp --track 5 --flatten              # render to one audio clip, takes removed (undo brings them back)
```

Each edit is one undo step. Through `apply`, the ops are `putComp` /
`removeComp` with comp-beat times and crossfades in seconds (see `wave schema`).

## Lyrics (the notebook)

`wave context` lists the notebook pages (`id`, `name`, `startBar`, `lines`);
`wave context --full` includes their text. Each line of a page is one bar from
`startBar`, `[…]` lines are headers. Write them with one op that replaces every
page (one undo step):

```sh
wave apply --json '[{"op":"putNotebook","notebook":{"pages":[{"id":3000,"name":"Refren","startBar":17,"text":"[Refren]\nto jest nasz czas\nnasza noc"}]}}]'
```

## Listening without speakers

```sh
wave render --out bounce.wav --from 0 --to 32
```

renders offline (independent of the audio device, so it works while the
musician keeps playing) and prints peak/RMS/crest/clipping so an agent can
check its work, then listen to the WAV. The same bounce is available in the
app as **Eksport** (`Ctrl+Shift+R`): song or loop, a 0/2/8 s effect tail, and
44.1 or 48 kHz. Stereo WAV defaults to 32-bit float. Select 24-bit PCM with
`--wav-bits 24` and add `--dither` for optional triangular dither. Integer WAV
clips samples beyond full scale. The export stats report those peaks before conversion.

Select **Wrap** when exporting a WAV loop to carry the effect tail into its
start. The result is one loop long. In the CLI, use `--wrap-tail` with the
project loop or specify both `--from` and `--to`. This works for stereo mixes
and stems.

### Stems

```sh
wave render --stems --out stems/ --from 0 --to 32     # a folder, created if needed
wave render --stems --out stems/ --no-master          # without Master.wav
wave render --stems --out stems/ --wav-bits 24 --dither
wave render --stems --out stems/ --format mp3 --kbps 192
```

writes one stereo file per mixer insert that has sound, named after the insert
(`Drums.wav`, `Bass.wav`; a repeated name becomes `Bass (2).wav`). In the app, choose
**Export → Output: Stems**, select WAV or MP3, then pick the folder.

- A stem is what the insert plays after its own effects, fader, pan and width, **before the
  master**. The optional `Master` file contains the normal mix with the master's effects.
  Use `--no-master` to leave it out. Nonlinear effects on the master, such as a limiter,
  can make the mix sound different from the sum of the stems.
- Every bounce uses the same range and effect tail. Use WAV for sample-accurate alignment in
  editors that ignore MP3 encoder delay and padding.
- Muted inserts and inserts with nothing routed into them get no file, and neither does one that
  turns out silent in the range (the reply lists those under `skipped`). Solo is ignored, so a
  soloed insert does not silence the other stems; a muted track or channel stays silent.
- WAV is the default. MP3 supports 128, 192, 256 and 320 kbps (default 320).
  Files are staged during export and moved into place after all stems are encoded.
  Existing files with the same names are replaced.
- The reply lists every file with its level statistics (`files`), so an agent can check each stem.

## Tune to Reference

Pulls a vocal take onto another performance of the same melody (a guide vocal,
an acapella, a better take) that is already a source in the project. Nothing is
generated and the take is not time-stretched: the reference is aligned to the
take (content, then pitch shape), brought to the take's octave, and its notes
become the take's target; the singer's vibrato and transitions are kept.
Where the alignment is not trusted (another song, a different part) the take
is left as sung.

```sh
wave tune-ref --clip 412 --reference 7                       # source 7 = the guide vocal
wave tune-ref --clip 412 --reference 7 --correction 80 --vibrato 100 --influence 100 --octave auto
wave pitch-target --clip 412            # score, octave, the reference's notes, low-confidence stretches
wave pitch-target --clip 412 --full     # plus every curve (source, reference, target, confidence)
wave tune-ref --clip 412 --off          # back to manual pitch edits
wave f0-model                           # which pitch detector runs (RMVPE or YIN) and its install state
```

A melody you played in the piano roll can be the reference instead: the notes a
channel plays over the clip in the playlist (a copy, so changing the piano roll
later does not change the vocal; `--melody` again takes them again). The take
keeps its own timing; the octave nearest the voice is used unless `--octave`
says otherwise; stretches nothing is played under stay as sung.

```sh
wave melody-channels --clip 412         # [{channel, name, notes, fit}] — fit = share of the singing it matches
wave tune-ref --clip 412 --melody 1003 --speed 40      # follow the Piano channel, 40 ms to reach each note
```

`--speed` is the retune speed in ms: how long the voice takes to reach each
target note (95 %); 0 or absent = at once.

`--correction` (how far toward the target), `--vibrato` (how much of the
singer's own movement is kept) and `--influence` (100 = the reference's note
centres, 0 = nearest note of `--key`/`--scale`) are percentages. Manual note
edits on the clip still apply on top.

Without a running app, a whole file:

```sh
wave tune-file --take take.wav --reference guide.wav --out tuned.wav --json target.json
```

uses the installed RMVPE model, or `--model rmvpe.onnx`, or `--yin`.

**Pitch detector.** WAVE detects pitch with YIN out of the box. The RMVPE
model (362 MB, from the RVC project; read its licence first) is better on
real voices: Pitch Editor → *Pobierz RMVPE* downloads it after showing the
licence, checks its SHA-256 and switches to it. `WAVE_RMVPE_MODEL=path`
points at a copy you already have.

**Benchmark.** `cargo run --release -p wave-tune-bench -- synth` (synthetic
takes with ±25…400 ct errors and drift against references in other voices,
octaves and tempi, plus unrelated references that must be refused),
`… -- vocalset --root DIR` (Annotated VocalSet: real singers with injected
errors, cross-singer references, mismatched references) and `… -- ab --out DIR`
(a blind A/B listening kit; `ab-score` evaluates the filled-in ratings). Add
`--rmvpe model.onnx` or `--yin`, and `--json out.json` for the full results.
Exit code 0 means every criterion passed; a run with no case, no criterion or a
failed case is a failure. The agent workflow (`pitch suggest` → `editTune` →
render) is measured on real recordings by `agent-vocalset`, `agent-gtsinger`,
`agent-medley`, `estimation` and the three-way `listen` kit; see
[`crates/tune-bench/RESULTS.md`](../crates/tune-bench/RESULTS.md) for the commands,
datasets and the latest results.

## Pitch editing for agents

The pitch editor's notes, for scripts and agents: what a vocal clip sings, how it relates to
another performance, a cautious correction proposal, and note-level edits. Edits are the same
`editTune` op the pitch editor sends, so they are one undo step in the app and save into the
project like any manual edit. Tune to Reference (`tune-ref`) is unchanged and independent:
both can be used on one clip (note edits apply on top of a reference).

```sh
wave pitch inspect --clip 412                     # notes, tuning, range; `base` fingerprint
wave pitch inspect --clip 412 --full --from 10 --to 14   # plus 5 ms frame curves for 4 s
wave pitch compare --clip 412 --source 7          # read-only: against another singer's take
wave pitch suggest --clip 412 --source 7          # a proposal with a reason per note; changes nothing
wave pitch edit --clip 412 --base 9d0cbbba3b9fc299 --note 3 --cents -25
wave pitch edit --clip 412 --file edit.json       # several notes at once; also a `pitch suggest` result or its `op`
wave pitch render --clip 412 --out take.wav       # the clip alone with its edits (--original: without)
```

Times are **seconds of the clip's source**; pitch is **MIDI** (60 = C4, fractional), changes in
**cents**. Notes are addressed by index `i` in the clip's segmentation: its own notes once it has
any edit, else the analysis' (`segmentation`: `edits` / `analysis`).

### `pitch inspect`

```jsonc
{ "clipId": 412, "revision": 31, "base": "9d0cbbba3b9fc299", "segmentation": "analysis",
  "backend": "rmvpe", "rate": 200,
  "clip": { "source": 1008, "start": 0, "len": 65.06, "offset": 0, "stretch": 1, "bpm": 120,
            "region": [0, 32.53], "sourceSeconds": 32.53, "sampleRate": 44100, … },
  "tuning": { "offsetCents": 9.5, "spreadCents": 14.9, "notes": 41 },   // the take vs A440
  "range": { "low": 61.9, "high": 74.1, "lowName": "D4", "highName": "D5" },
  "kinds": { "stable": 24, "vibrato": 17, "glide": 4, "short": 6 },
  "reference": null, "pencil": { "points": 0, "runs": [] },
  "notes": [ { "i": 2, "start": 1.015, "end": 1.835, "beats": [2.03, 3.67], "kind": "vibrato",
               "sung": 74.21, "nearest": "D5", "cents": 21.4, "out": 74.21,
               "shift": 0, "drift": 0, "formant": 0, "gain": 0, "confidence": 0.92, "voiced": 1,
               "levelDb": -18.9, "slopeCents": 22.7, "spreadCents": 44.5, "stepCents": 4.0, "vibrato": [3.2, 37.5] } ],
  "frames": { "rate": 200, "start": 10.0, "f0": [69.02, null, …], "confidence": […], "out": […] }  // --full
}
```

`kind`: `stable` (a held pitch), `vibrato` (held, with a vibrato: `[rate Hz, depth ct]`), `glide`
(a slide through the note's held middle), `short` (< 80 ms: ornament or transition), `unsure`
(breathy, mostly unvoiced or low detector confidence). `sung` is the recording; `out` what it
sounds like with the current edits (and Tune to Reference). `slopeCents` is the pitch change
across the note's central 60 %, `spreadCents` the movement around it, `stepCents` the difference
between the medians of its second and first half (two pitches sung legato as one note: split it).

### `pitch compare`

Read-only. Aligns the take with another source (another singer, another key, other phrasing)
and reports, without deciding anything:

* `related` — the reference sings the same melody: the alignment is trusted **and** at least
  half of the confidently matched notes agree within 50 ct after the interval. `aligned` alone
  means the same material in time (a harmony part aligns but is not related). Only a related
  reference is used by `suggest`.
* `intervals` — the take's interval above the reference, best first: `semitones` (any interval,
  not only octaves) and `cents` (the measured offset, which includes a difference between the two
  singers' tunings), with `support`; `ambiguousInterval` when a second interval is close.
* `notes[]` — per take note: `relation` `match` / `merged` (one take note over several reference
  notes: the reference ornaments) / `split` / `complex` / `extra` (nothing aligned with
  confidence: an ad-lib, or not trusted), the reference notes it covers, `expected` (the
  reference's note in the take's register), `diffCents`, `onsetMs`, `confidence`.
* `referenceNotes[]` with `ornament` (short notes inside a run the take sings as one note),
  `counts` (`matched`, `merged`, `split`, `extra`, `missing`), `sameNotes`, `contour`,
  `coverage`, `unmatched` stretches, `timingMs`, `referenceTuningCents`.

### `pitch suggest`

A conservative proposal, with a decision and a reason for every note; it changes nothing.
Rules: short, unsure and sliding notes, and notes holding two pitches (`stepCents` ≥ 25), are kept; an unrelated reference changes nothing at all;
with a related reference the intended pitch is the reference's note in the take's register,
unless the take is further than `--max-cents` (default 60) from it — a semitone or more off yet
itself in tune is what a deliberate variation looks like, so it is kept; without a reference,
the nearest note of the key (`--key C --scale minor`, or the project's key) or the nearest
semitone, unless the take is near the middle between two notes. Within `--min-cents` (15) a note
is left alone. `--strength 50` goes half the way; `--tuning 8` puts the grid at A = 442 Hz (use the
backing track's `wave analyze` tuning offset). The result's `op` is a ready `editTune` op.

### `pitch edit`

One undo step, refused as a whole if any part is invalid. `--base` (from `inspect`) refuses
the edit with `STALE_NOTES` if the notes were split, merged or re-analysed since;
`--if-revision` refuses it if anything in the project changed. `--max-cents C` refuses a
result that shifts a note further than C (`OVER_LIMIT`).

| flag / JSON field | meaning |
| --- | --- |
| `--note I --shift C` / `{"i": I, "shift": C}` | the note's pitch move, cents (absolute) |
| `--cents C` / `"shiftBy": C` | add C cents to its current move |
| `--target M` / `"target": M` | move the note's centre to MIDI M (turned into `shift`) |
| `--drift %` / `"drift": 0..1` | flatten the movement inside the note toward its centre |
| `--formant C`, `--gain dB`, `--speed MS\|none` | formant cents, level, retune speed |
| `--split I@SEC`, `--merge I`, `--reset I,J`, `--clear` | structure; reset = back to as sung; clear = every note edit and pencil off |
| `"pencil": [[sec, midi], …]`, `"erasePencil": [[from, to]]` | draw / erase pitch |

All indices in one edit name the notes as they were **before** it, whatever order the parts are
in; they apply as clear, reset, notes, split, merge, erasePencil, pencil. A note cannot be both
split and merged, nor edited and merged into its neighbour, in one edit. The reply has the new
`base`.

### An agent's correction workflow

Your recording is clip 412; the original singer (another key, range and phrasing) is source 7;
the instrumental is clip 300.

```sh
wave analyze --clip 300                                # key and tuning offset of the backing
wave pitch inspect --clip 412 > take.json              # what you sang: kinds, cents, tuning, range
wave pitch compare --clip 412 --source 7 > cmp.json    # related? interval? which notes match?
wave pitch suggest --clip 412 --source 7 --key A --scale minor --max-cents 60 > plan.json
```

Then decide, note by note: accept the plan's edits that make sense, drop the ones that touch a
deliberate variation, add your own (`--target`, a split before a scoop you want to keep), and
apply them as one edit:

```sh
jq 'del(.op.edit.notes[] | select(.i == 21))' plan.json > edit.json   # e.g. drop note 21's change
wave pitch edit --clip 412 --file edit.json --if-revision "$(jq .revision plan.json)" --max-cents 80
wave pitch render --clip 412 --out after.wav && wave pitch render --clip 412 --out before.wav --original
wave pitch inspect --clip 412                          # `out` per note: did it land?
wave pitch edit --clip 412 --reset 7                   # put back a note that sounds worse; or `wave undo`
```

What the defaults protect: a different note, an ornament the reference has and you do not (or
the other way round), a slide or vibrato, and a reference that is not the same song are left
as sung. Timing is never changed. `--max-cents` raises the limit when you know the take is far
off; low-level `pitch edit` always lets you override the plan.

## Plugins

```sh
wave vst3                       # installed VST3 effects (cached scan; --rescan)
wave vst3-add --insert 2 --path "C:\…\Pro-Q 3.vst3" --cid 72C4DB…
wave vst3-params --fx 1210 --query freq
wave vst3-set --fx 1210 --id 42 --value 0.3
wave auto-add --fx 1210 --param 42 --points "0:0.2,8:0.8" --name "Reverb decay"
                # automate a hosted plugin parameter: an envelope plus the
                # playlist clip that plays it; ids come from context.nextId
```

## Notes (deterministic transforms and generators)

Every verb is one undo step and deterministic in `--seed`: the same command on the same
pattern produces identical notes, so agents can generate variants, render each and keep
a favourite.

```sh
wave notes humanize --pattern 1 --timing 0.3 --velocity 0.2
wave notes quantize --pattern 1 --grid 1 --amount 1 --scale      # --scale: into the project key
wave notes euclid --pattern 1 --ch 1001 --k 5 --n 16 --rotate 2  # E(k,n) fill for one channel
wave notes strum --pattern 1 --width 0.5 --ramp 0.05
wave notes drums --pattern 2 --bars 2 --seed 42 --voice "1001:4:16:0" --voice "1002:2:16:8"
wave notes melody --pattern 3 --ch 1003 --bars 2 --seed 7 --density 0.5
wave notes humanize --pattern 1 --select "0:16"                  # limit to a step range
wave key C minor                                                  # project key for --scale
wave key --off
```

`--select "0:16@1001"` also filters one channel; `--indices "1,2,5"` selects exact notes.

## Several apps open

`wave sessions` lists them; pass `--session ID` (a prefix is enough).
