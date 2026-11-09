# Built-in synth

Channel Rack channels of kind `synth` play the engine's own polyphonic synth
(`crates/engine/src/poly`). Project format: `{"kind": "synth", "synth": {"preset": "808 Czysty" | null,
"params": {"flt_cutoff": 900, ...}}}`; a missing id plays its default, unknown ids are rejected
by `putChannel`. Projects without synth channels load unchanged (serde defaults).

## UI contract (`ui/src/api`, `ui/src/lib/synth.ts`)

- `Catalog.synth: SynthCatalog = { sections, categories, presets }`. Sections in panel order:
  `osc1, osc2, subnoise, unison, filter, ampenv, modenv, lfo1, lfo2, matrix, voice, fx`, each
  `{ id, name, params: ParamDef[] }` (choices carry `options`). Presets carry the complete
  parameter set. The browser mock serves the generated copy `ui/src/api/synthCatalog.ts`
  (regenerate: `WAVE_UPDATE_UI=1 cargo test -p wave-engine --test synth`; a test fails when stale).
- New channel: `newSynthChannel(catalog.synth, id, presetName | null)` then the `putChannel` op.
  Knob edit: `withSynthParam(...)` + `putChannel` (fast path, no timeline rebuild, held notes
  follow). Preset: `withSynthPreset(...)` + `putChannel`.
- CLI: `wave channels --synth "808 Czysty"`, `--ch ID --preset NAME`, `--ch ID --param ID --value V`.

## Signal flow

Per voice: 2 wavetable oscillators (8 waves × 2 frames, Shape morphs; Square Shape = PWM, built
as the difference of two phase-offset band-limited saws, so PWM is as alias-free as the tables
— no PolyBLEP needed),
unison 1/3/5/7 per oscillator with symmetric detune and stereo width, sub (−1/−2 oct), noise
(colour tilt) → per-voice drive (rational tanh, 2× oversampled) → Simper/Cytomic TPT SVF
(LP12, LP24 = two cascaded, BP, HP, notch; key tracking, env and LFO amounts in octaves) →
amp ADSR × velocity → pan. Envelopes: exponential (Redmon-style overshoot targets) amp and mod
ADSRs. Two LFOs (sine/tri/saw/square/S&H, Hz or tempo sync), 4-slot mod matrix
(sources: mod env, LFO 1/2, velocity, wheel, random, key, LFO 1 × wheel; 14 destinations).
16 voices plus 4 fade slots: a stolen voice fades over 4 ms. Poly / mono / legato with glide
(808 slides). Global: drive (the engine saturator core, triode voicing with auto gain, always
running so its latency never moves), stereo chorus, tempo ping-pong delay, `reverb_v2`, output trim.

Wavetables: harmonics capped at 256 per 2048-sample frame, third-octave mip levels crossfaded
by pitch, so nothing above Nyquist is ever played. Real time: tables are built once off the
audio thread, instances are made by the engine and sent through the command ring, old
instances / patches go back through the garbage ring; no allocation, locks or I/O in render;
denormals flushed in every feedback path. Note events are sample-accurate (sorted per block,
note-offs before note-ons at the same sample).

## Measurements (`cargo run --release -p wave-engine --example synth_lab -- alias|clicks|cpu|levels|demos DIR`)

- Alias SNR (1 s tone, harmonics vs everything else), MIDI 24–108, worst: saw 63.3 dB,
  square 65.4 dB, 25 % pulse 63.7 dB (≥ 80 dB from C4 up). Target > 60 dB to C7: met.
- Clicks (spike/step detector on the 2nd difference): 0 on all 32 presets for note on/off,
  retrigger, 20-note steal, legato+glide, mono+glide and preset change on held notes. The
  detector misses a saw cut exactly on its ramp (by design: it must not flag saw edges), and two
  distorted 808 square-ish waves whose edges coincide in poly overlaps read as a spike, so the
  test leaves that one case out.
- CPU, 16 voices × 2 osc × 7 unison, LP24 + drive, every global effect, 48 kHz / 64-sample
  blocks: mean 16.8 % of one core, p99 block 343 µs, worst 705 µs (budget 1333 µs).
- Loudness (one 2 s note at velocity 100, C3 for 808/basses, C4 otherwise; trims set by
  measurement): −19 LUFS (808, bass, pads) / −18 LUFS (others) within ±0.1, true peak ≤ −3 dBTP.
  Short, peaky presets hit the −3 dBTP ceiling first: Pluck Trap −23.0, Zjazd −23.8, Arp Cyfrowy
  −21.7, Pluck Ciepły −21.0, Arp Puls −20.3, Pluck Szklany −20.0 LUFS (integrated loudness of
  a decaying single note understates how loud they sound in a line).
