# WAVE Keys

A sampled grand piano as a Channel Rack channel (`kind: "keys"`). Add it from the Channel
Rack's **+** menu (*WAVE Keys — fortepian*) or with `wave channels --keys`; it is played from
the piano roll, the typing keyboard and a MIDI keyboard (with the sustain pedal) like every
other pitched channel.

Project format: `{"kind": "keys", "keys": {"release": 350, "dynamics": 100}}`; both fields are
optional. Projects without a Keys channel load unchanged.

| knob | range | what it does |
| --- | --- | --- |
| Wybrzmienie (`release`) | 40 ms – 3 s, default 350 ms | how long a note rings after its key is let go (−60 dB at the end) |
| Dynamika (`dynamics`) | 0 – 100 %, default 100 % | how much the velocity changes the level; the timbre always follows the velocity |

```sh
wave channels --keys
wave channels --ch "WAVE Keys" --param release --value 600
wave channels --ch "WAVE Keys" --param dynamics --value 70
```

The Keys window (click the channel's name) has a keyboard you can click (a key sounds while held): lower on a key is a
harder hit, dragging slides across keys, and keys held on a MIDI keyboard light up. Sampler
channels have the same keyboard, with a dot on the root key.

## The recordings

Salamander Grand Piano V3 (a Yamaha C5, recorded by Alexander Holm, **CC-BY-3.0**): 30 notes,
every minor third from A0 to C8, at 3 of the 16 recorded dynamics (layers 4, 9 and 14), 48 kHz
24-bit FLAC, 138 MB. They are pinned by URL and SHA-256 in `assets-manifest.json` (kit
`salamander-piano`) and never re-encoded.

CC-BY is an attribution licence: a song that uses the piano should credit the recordings. The
Keys window shows the credit, and `THIRD_PARTY_NOTICES.md` carries it for the app.

Where the app finds them, in this order:

1. **inside the app**: `keys/` in the bundled resources. `node scripts/fetch-keys.mjs`
   downloads them into `app/resources/keys` (checksummed, not in Git) and `tauri build` ships
   that folder; the release workflow runs the script, so installers always contain the piano;
2. **the sample library**: `<data>/WAVE/Library/salamander-piano`, for builds made without
   step 1 (a plain `cargo build`). The Keys window then offers **Pobierz fortepian**, which
   installs the library kit; the channel starts playing when the download ends.

Nothing is loaded until a project has a Keys channel. Then the files are decoded on a few
threads (about a second) into memory, about 280 MB, and stay there. Until that ends the
channel is silent and the window says so; an offline bounce (`wave render`, Eksport) waits
for it instead of rendering a silent piano.

## How a note plays (`crates/engine/src/keys.rs`, voices in `synth.rs`)

- the recording nearest to the key, resampled by at most two semitones (cubic interpolation);
- the layer its velocity falls in (thirds of the velocity range); inside a layer the level
  follows the velocity by `dynamics` (`(velocity / layer centre)^(1.2 · dynamics)`);
- it rings for the note's length, then fades exponentially over `release`; striking a key
  again damps that key's previous note the same way;
- recordings are cut at 9 s with a 1.5 s fade (the longest a held note can ring);
- voices come from the engine's shared pool of 96, oldest stolen first.

Real time: the bank is an `Arc` built off the audio thread and carried by the timeline; a
voice only reads it. No allocation, lock or I/O in the callback.

Not there (yet): half-pedalling and sympathetic resonance, release samples, more than three
dynamics. They are what a larger sample set would add.
