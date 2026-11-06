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
check its work, then listen to the WAV. The same bounce is available in the
app as **Eksport** (`Ctrl+Shift+R`): song or loop, a 0/2/8 s effect tail, and
44.1 or 48 kHz, written as a 32-bit float stereo WAV.

## Plugins

```sh
wave vst3                       # installed VST3 effects (cached scan; --rescan)
wave vst3-add --insert 2 --path "C:\…\Pro-Q 3.vst3" --cid 72C4DB…
wave vst3-params --fx 1210 --query freq
wave vst3-set --fx 1210 --id 42 --value 0.3
```

## Several apps open

`wave sessions` lists them; pass `--session ID` (a prefix is enough).
