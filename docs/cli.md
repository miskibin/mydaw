# `wave` — the command line

The CLI talks to the **running app** and edits the same project you see,
with the same undo history. There is nothing to install: on start the app puts
a `wave` launcher on your PATH (`%LOCALAPPDATA%\Microsoft\WindowsApps\wave.cmd`
on Windows, `~/.local/bin/wave` elsewhere). Settings → CLI shows where it went.
Open a new terminal after the first start and run:

```sh
wave status
wave context --pretty
```

stdout is only JSON (`--pretty` to indent); messages go to stderr. Exit codes:
`0` ok, `1` bad usage, `2` WAVE not running / unreachable, `3` the app refused
(invalid op, revision conflict, missing plugin …).

## Reading

| command | what you get |
| --- | --- |
| `wave status` | transport, revision, file, audio device, CLI path |
| `wave context` | summary: tracks (index, insert, volume, clip count), inserts with effects and parameters, patterns, sources, `nextId` |
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
wave apply --file edit.json --label "Busier hats"
wave undo
```

`edit.json` is an array of ops (see `wave schema`), applied atomically:

```json
[
  {"op": "putPattern", "pattern": {"id": 2, "name": "Hats", "color": "#e0a44a", "len": 4,
    "notes": [{"inst": "hat", "step": 0, "len": 1}, {"inst": "hat", "step": 2, "len": 1}]}},
  {"op": "setLoop", "loop": {"a": 0, "b": 16}}
]
```

Units: playlist time in **beats**; pattern `step`/`len` in **sixteenths**;
fades in seconds; track/insert `vol` is a fader position (0.7937 = 0 dB);
clip `gain` in dB; pan −1..1. New ids: start at `context.nextId`.

## Listening without speakers

```sh
wave render --out bounce.wav --from 0 --to 32
```

renders offline (independent of the audio device, so it works while the
musician keeps playing) and prints peak/RMS/crest/clipping so an agent can
check its work, then listen to the WAV.

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
Exit code 0 means every criterion passed.

## Plugins

```sh
wave vst3                       # installed VST3 effects (cached scan; --rescan)
wave vst3-add --insert 2 --path "C:\…\Pro-Q 3.vst3" --cid 72C4DB…
wave vst3-params --fx 1210 --query freq
wave vst3-set --fx 1210 --id 42 --value 0.3
```

## Several apps open

`wave sessions` lists them; pass `--session ID` (a prefix is enough).
