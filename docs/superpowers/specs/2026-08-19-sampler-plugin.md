# Sampler plugin — manifest-driven sampled instruments

Status: implemented
Crate: `plugins/sampler` (`mydaw-plugin-sampler`, kernel id `sampler`)
Depends on: step 3 (internal instruments), step 5 (tracks and sources), step 8
(project persistence), step 9 (piano roll), the debug-tap harness

---

## 1. Why

Everything in `plugins/instruments` is synthesised: no assets, no licences,
bit-deterministic, and silent about what a real instrument sounds like. That was
the right trade for step 3 — it made the note path useful with nothing
installed — and it is the wrong one forever. A DAW whose only instrument is a
saw wave is a demo.

This step adds the other half: an instrument that plays *recordings*, and the
supply chain that gets those recordings onto a machine legally, reproducibly,
and without putting 131 MB in git.

Three constraints shaped every decision below:

1. **A checkout has no assets.** The build, the tests and the app must all work
   on a fresh clone. The sampler starts, reports zero instruments, logs a hint,
   and every test that needs samples skips itself.
2. **The content is CC-BY.** Attribution is a licence obligation, not a
   courtesy, so it travels in the manifest, into the runtime, and into
   `assets/ATTRIBUTION.md` beside the files.
3. **The audio thread is still the audio thread.** Decoding, fetching and
   scanning are all non-RT; `process()` does array reads and multiplies.

---

## 2. Architecture

```text
  assets-manifest.json  (committed: URL + SHA-256 + mapping for all 90 files)
        │
        ├── scripts/fetch-assets.ps1 ─┐        (shell, CI, offline)
        └── AssetDownloader ──────────┤        (in-app, on click; include_str!)
                                      │
                                      v
             assets/instruments/salamander-piano/
                 manifest.json  A0v4.flac  C1v4.flac  …
                                      │
                            SamplerRegistry::rescan          (background thread)
                                      │
                            SampleBank (Arc, immutable)
                                      │
                            SamplerFactory  ──create──>  SamplerNode
                              (per instrument)             (32 voices, RT)
```

The two installers read the **same committed file**, which is what makes them
incapable of disagreeing about what "installed" means. The script reads it from
disk; the app has it `include_str!`d, so there is no path to resolve at runtime
and a missing file is a build error.

### Source integration

`Source::Sampler(SamplerSourceRef)` is a new variant in `app/src/source.rs`. It
is **internal, not hosted**: no bundle, no plugin instance, no main-thread
round-trip. `TrackHost::build_node` resolves it through the `SamplerRegistry`
exactly as it resolves `poly-synth` through the `InstrumentRegistry`.

It appears in `get_sources` as `SourceKindDto::Internal`, appended after
`Source::BUILT_IN` rather than compiled into it — because the set is discovered
at runtime and can *grow while the app is running*, which is precisely what
happens when a download finishes.

**No new UI was built for it.** The picker is data-driven and the new entries
simply arrive in the existing list. The single UI addition is one line in the
status bar (§7).

---

## 3. Manifest format

One `manifest.json` per instrument directory. Deliberately **not** SFZ: SFZ is a
macro language with `#define`, `#include`, opcode inheritance across four scope
levels, curves, CC bindings and two dialects. A parser for all of it is a
project; a parser for *some* of it silently plays the wrong sample when it meets
an opcode it skipped. The ambiguity is resolved once, offline, by
`scripts/gen-asset-manifest.ps1`, and the runtime reads a format it fully
implements.

```json
{
  "id": "salamander-piano",
  "name": "Salamander Grand Piano",
  "description": "Yamaha C5 grand, 30 sampled notes x 3 velocity layers",
  "release_ms": 400,
  "max_seconds": 7.0,
  "fade_out_seconds": 1.0,
  "attribution": "Salamander Grand Piano V3 by Alexander Holm, licensed CC-BY-3.0. …",
  "samples": [
    {
      "file": "A0v4.flac",
      "root_key": 21, "key_lo": 21, "key_hi": 22,
      "vel_lo": 1, "vel_hi": 54,
      "tune_cents": 0.0
    }
  ]
}
```

| Field | Required | Meaning |
| --- | --- | --- |
| `id` | yes | Stable id and directory name. Part of the source id a project file records. |
| `name` | yes | Label for the source picker. |
| `description` | no | Tooltip. A sample/size summary is substituted when empty. |
| `release_ms` | no (300) | Note-off fade, linear to exactly zero. |
| `max_seconds` | no | Hard cap on decoded length per sample. The memory budget, as data. |
| `fade_out_seconds` | no | Fade baked onto a *truncated* tail. Ignored when nothing was cut. |
| `attribution` | no | Licence credit, carried into the runtime. |
| `samples[].file` | yes | A **plain file name**, in the manifest's own directory. |
| `samples[].root_key` | yes | Key at which the file plays at its recorded pitch. |
| `samples[].key_lo/key_hi` | yes | Key span this entry answers for. |
| `samples[].vel_lo/vel_hi` | no (1/127) | Velocity span. |
| `samples[].tune_cents` | no (0) | Fine detune, folded into the playback ratio. |

### Robustness rules

- **Unknown fields are ignored.** `assets-manifest.json` carries `url`,
  `sha256`, `bytes` and a `source` block that the runtime has no use for, and is
  copied to `manifest.json` **verbatim** rather than filtered — one fewer
  transformation to get wrong. Both installers do the same copy.
- **Every error is typed and names the file**, and a sample error names its
  *index* too, because a piano has ninety of them.
- **A missing sample file is a skipped entry with a warning, not a failed
  load.** A partial download leaves exactly that state, and 89 of 90 samples is
  worth far more than nothing. Notes the missing entry covered fall through to
  the next layer that covers them, so the instrument sounds thin rather than
  gapped. The load fails only when *nothing* survived.
- **`file` must be a bare name.** Checked with `Path::components`, not a scan
  for `..`, so `../x`, `..\x`, `/x` and `C:\x` all fail identically on every
  platform. A manifest arrives from a download; it must not choose what this
  process reads.

---

## 4. DSP

### Pitch

```text
step = 2^((key - root_key + tune_cents/100) / 12) · sample_rate / device_rate
```

All three corrections collapse into one `f64` computed at note-on, off the inner
loop. Nothing is resampled at load: a sample keeps its own rate and the device
ratio rides along, which costs one multiply against either a slow start-up or a
full bank rebuild on a device change.

### Interpolation

Four-point Catmull-Rom. Linear interpolation on a resampled piano is audible as
a dull, grainy top end — it is a lowpass whose corner moves with the pitch
ratio. Catmull-Rom costs three more multiplies per channel per frame, is C¹
continuous, and passes exactly through the sample points, which is what makes
`step == 1.0` **bit-exact** playback rather than an approximation of it (tested).

Every sample is padded with one silent frame before and two after, so the
interpolator's four taps are always in bounds with no branch.

### Velocity

Two jobs, deliberately split:

- **Layer selection** is *timbre*. A piano struck hard is not a loud piano
  struck softly. The manifest's velocity spans pick the recording.
- **Layer gain** is *level*, and trims only **within** the chosen layer:
  `LAYER_FLOOR` (0.5, −6 dB) at the bottom of the span to 1.0 at the top.

The recordings already carry the loudness difference *between* layers. Applying
a global velocity curve on top would count it twice, which is the classic way a
sampled piano ends up with no quiet end. Measured contrast, velocity 0.3 → 1.0
through the device: **12.9 dB**.

### Voices

32 voices. Allocation: a free slot, else the oldest (counted as a steal).

A **retrigger takes a new voice** and puts the old one into a 20 ms choke,
because a repeated note on a piano is a fresh hammer strike while the old string
is still ringing — restarting the slot in place would be a jump to the head of
the sample, i.e. a click. Note-off runs a linear release over `release_ms` to
**exactly 0.0**; "released" has to mean digital silence, not "quiet enough".

`VOICE_GAIN` is 0.15 (−16.5 dB), set by the worst realistic case rather than by
taste: six full-velocity notes whose partials line up sum to roughly five times
one note. A six-note chord was measured clipping at 0.25 during development —
this is the number that fixed it.

### RT safety

`process()` holds an `Arc<SampleBank>` and **borrows** it — never clones it,
since an `Arc::clone` per span per block is a pair of atomic RMWs bought for
nothing. Voices are a fixed array. The (key, velocity) → sample lookup is a
single index into a flat 32 KB table built at load, not a scan over ninety
regions.

Proved by `plugins/sampler/tests/rt_alloc.rs` with a counting global allocator
across twelve block sizes, event-heavy blocks, full-polyphony voice stealing and
a two-second steady state: **0 allocations**.

---

## 5. Memory budget

| | |
| --- | --- |
| Download | 131.4 MB (90 FLAC files) |
| Decoded, worst case | 90 × 7.0 s × 48 kHz × 2 ch × 4 B = **231 MB** |
| Decoded, measured | **222.0 MB** |
| Budget | 400 MB |

### Truncation

Salamander's samples run to ~16 s each; all 90 undecimated would be ~518 MB.
`max_seconds: 7.0` caps them, with `fade_out_seconds: 1.0` baked in at load so
the cut is not a step discontinuity. A sample that ends on its own is **not**
faded — it already decayed. Verified through the device: the 6.0–7.0 s window
declines monotonically and nothing sounds past 7.05 s.

The musical cost is real and worth stating: a note held longer than 7 seconds
with no note-off stops early. At 130 BPM that is over four bars of one note.

### `f32`, not `i16`

Halving the footprint would mean eight integer conversions per output frame
(four taps × two channels) inside the voice loop, on the audio thread, to save
memory the budget does not need. Memory is cheap here; the audio callback is
not.

### Load time

~9 s to decode 90 FLAC files, on a background thread started at plugin boot. The
window opens immediately; the piano appears in the picker when it is ready. The
same mechanism is what lets a finished download appear **without a restart**.

---

## 6. Licensing

Nothing in this table is committed. `assets/` is gitignored; what *is* committed
is `assets-manifest.json`, which pins every file's URL and SHA-256.

| What | Source | Licence | Ships in repo? | Obligation |
| --- | --- | --- | --- | --- |
| Salamander Grand Piano V3 samples (90 FLAC) | [sfzinstruments/SalamanderGrandPiano](https://github.com/sfzinstruments/SalamanderGrandPiano) @ `3382bf9` | **CC-BY-3.0**, Alexander Holm | No — fetched | Attribution on any redistribution |
| The SFZ mapping it was derived from | same repo, `Data/*.txt` | CC-BY-3.0 | No — read by the generator | — |
| `assets-manifest.json` (URLs, checksums, key/velocity mapping) | ours | MIT OR Apache-2.0 | Yes | — |
| `scripts/gen-asset-manifest.ps1`, `scripts/fetch-assets.ps1` | ours | MIT OR Apache-2.0 | Yes | — |
| `plugins/sampler` | ours | MIT OR Apache-2.0 | Yes | — |

CC-BY-3.0 is an **attribution** licence. MYDAW does not redistribute the samples
— it downloads them from the source named above — but anything exported that
contains their sound does carry the obligation. The credit is therefore in three
places: the manifest (`attribution`), the runtime (`InstrumentInfo::attribution`,
printed by the hardware tests), and `assets/ATTRIBUTION.md`, written beside the
files by whichever installer put them there.

The upstream repo's `LICENSE` file was read and verified as CC-BY-3.0 before the
commit was pinned.

**VSCO-2-CE percussion (CC0) was considered and skipped.** The piano was the
priority, it is done, and a second instrument would have added a second mapping
to derive for no new mechanism — the manifest format, the fetcher and the
registry are all already plural.

---

## 7. The in-app downloader

`scripts/fetch-assets.ps1` covers CI and offline installs. It does not cover a
user who cloned the repo and opened the app, so the same install runs from
inside the application.

### Consent

**Nothing downloads on its own.** A fresh install starts `AssetState::Idle` with
`installed: false`; the status bar offers a button, and pressing it *is* the
consent. Pulling 131 MB on first launch, on a connection the app knows nothing
about, would be doing something the user did not ask for.

### Atomicity and idempotence

Every file lands as `<name>.part`, is hashed as it is written, and is renamed
onto its final name **only after** the SHA-256 matches. A file that fails its
checksum is **deleted**, not kept — a nearly-right file is worse than a missing
one, because the loader would decode it. `manifest.json` is written **last**, so
its presence means the samples beside it are complete.

A file already present at its recorded size is skipped, which makes a retry
resume rather than restart. The full hash is checked when a file is *fetched*,
not on every status query: re-hashing 131 MB to answer "is it installed?" would
make the status command cost half a second, and the atomic rename is what makes
the size check sufficient.

### Status

```ts
{ installed, instrumentReady, state, totalBytes, downloadedBytes,
  filesDone, fileCount, instrumentId, instrumentName, error }
```

`installed` (files on disk) and `instrumentReady` (decoded and playable) are
**different facts**, and the gap between them is the ~9 s decode. The shell shows
"Loading…" in that window rather than pretending the picker is ready.

### UI

One line in `ui/src/shell/StatusBar.tsx`, reusing existing classes:

- not installed → `Download Salamander Grand Piano (131 MB)` (a `link-button`)
- transferring → `Downloading Salamander Grand Piano… 47%`
- decoding → `Loading Salamander Grand Piano…`
- ready → **gone**, permanently

Polled at 500 ms only while something is in flight; an installed, loaded session
polls nothing. When `instrumentReady` flips, the shell calls the existing
`app.refresh()` so the new instrument reaches the source picker with no restart.

The mock (browser) backend reports the content as installed, hiding the line
rather than offering a button that could not work.

---

## 8. Persistence

**Free**, and that is the design rather than a coincidence.

A `sampler:` id is *complete*: it names an instrument directory and nothing else.
Unlike `clap:` and `vst3:`, there is no second half (a bundle path, a class id)
that must travel beside it. So `Source::parse` rebuilds a sampled source from an
id alone, and `SourceEntry::capture` falls into the existing `Internal { id }`
arm with no new variant:

```json
{ "kind": "internal", "id": "sampler:salamander-piano" }
```

A project saved with a piano and opened on a machine without the assets keeps
its source and renders `SilentNode` with a warning — the same doctrine as a
plugin that will not load, for the same reason: a track set to a piano must not
answer with a sine wave. Install the content and it plays.

The display name is deliberately *not* persisted; it belongs to the registry.

---

## 9. Verification

### Offline (no assets, runs everywhere)

`plugins/sampler/tests/voices.rs` — 18 tests against synthetic instruments built
on disk through the shipping loader. Pitch (C4-rooted sample at A4 measures
739.99 Hz), bit-exact playback at the root key, interpolation continuity across
six ratios, layer boundaries at exactly the manifest's velocities, release to
digital silence, release timing, voice stealing, retrigger choke, frame offsets,
chord headroom.

`plugins/sampler/tests/rt_alloc.rs` — 5 tests, counting allocator, **0
allocations** on the audio thread.

`plugins/sampler/src/download.rs` — the whole installer state machine against an
injected fetcher: success, checksum mismatch (file discarded, nothing left on
disk, nothing looks installed), resume, a truncated file replaced, the
background thread's transitions. No network, so it runs in CI.

### Against the network

`plugins/sampler/tests/live_download.rs`, `#[ignore]`d. Fetches the single
smallest pinned file over real HTTPS and checks its SHA-256 — which is the only
way to prove the TLS stack is wired, the pinned URLs resolve, and the bytes
upstream still match what was recorded. Verified: `C8v4.flac`, 306 364 bytes,
`1f8ff28f…`. A failure here means the supply chain broke, not the sampler.

### With assets (skip-if-absent)

`plugins/sampler/tests/piano_assets.rs` — 8 tests. 90/90 samples loaded, 0
skipped, 222.0 MB, key range A0–C8, every key answers at every velocity band,
pitch within **3.6 cents** worst case, decay, truncation fade, release to
silence, C major chord.

### Through the sound card

`app/tests/sampler_audio.rs`, `#[ignore]`d, assets required. Focusrite USB Audio
@ 44 100 Hz — note the device rate differs from the samples' 48 kHz, so this also
exercises the rate term in the playback step.

**C major chord** (C4 E4 G4, velocity 1.0):

| Key | Expected | Measured | Level |
| --- | --- | --- | --- |
| 60 | 261.63 Hz | 261.08 Hz | −49.7 dBFS |
| 64 | 329.63 Hz | 329.66 Hz | −42.3 dBFS |
| 67 | 392.00 Hz | 391.35 Hz | −36.4 dBFS |

Decay, RMS over fifths of a 2.5 s capture:
`0.02762 → 0.00875 → 0.00409 → 0.00298 → 0.00224` (−31.2 → −53.0 dBFS),
monotone. Peak 0.1272 (−17.9 dBFS), **0 clipped**, 0 non-finite, 0 dropped.
Silence before the first note and after the release, both exactly 0.0.

**Velocity contrast** (C4):

| Velocity | RMS | dBFS |
| --- | --- | --- |
| 0.3 | 0.00295 | −50.6 |
| 1.0 | 0.01297 | −37.7 |

**12.9 dB** of contrast, and the two select different velocity layers.

**Melody from the piano roll** — the step-9 machinery unchanged, with track 1
re-headed to the piano through `set_track_source`, over the drum grid at 130 BPM.
Kick onsets: 10, contiguous, worst grid error **0.010 ms**.

| Window | t (s) | Key | Expected | Measured | Partial | Cents |
| --- | --- | --- | --- | --- | --- | --- |
| 0 | 0.4117 | 67 | 392.00 | 391.60 | ×1 | −1.8 |
| 1 | 0.8732 | 72 | 523.25 | 524.08 | ×1 | +2.7 |
| 2 | 1.3347 | 60 | 261.63 | 523.31 | ×2 | +0.2 |
| 3 | 1.7963 | 64 | 329.63 | 329.40 | ×1 | −1.2 |
| 4 | 2.2578 | 67 | 392.00 | 391.60 | ×1 | −1.8 |
| 5 | 2.7194 | 72 | 523.25 | 524.08 | ×1 | +2.7 |
| 6 | 3.1809 | 60 | 261.63 | 523.31 | ×2 | +0.2 |
| 7 | 3.6424 | 64 | 329.63 | 329.40 | ×1 | −1.2 |

Worst pitch-class deviation **2.7 cents**. Capture peak 0.4104 (−7.7 dBFS), 0
clipped, 0 dropped.

**Windows 2 and 6 landed on the second harmonic**, and that is a property of the
instrument, not a fault. A struck piano string's second partial is routinely
louder than its fundamental — on this recording of C4, 523 Hz sits ~13 dB above
261 Hz. The assertion is therefore on *pitch class* (deviation folded into
±600 cents), which still catches every wrong note: no note in this pattern is an
octave from any other right answer, and a semitone is 100 cents. The partial
each measurement landed on is printed rather than hidden.

PNGs, WAVs and stats JSON for every capture are under `.debug/captures/`.

---

## 10. Limitations

Stated rather than hidden. Each is a deliberate omission with a reason.

- **No disk streaming.** Everything is in RAM. 222 MB for one piano is fine;
  a 4 GB orchestral library is not, and would need a streaming engine with a
  pre-roll buffer and a disk thread — a project, not a step.
- **No loops.** A sample plays once and the voice ends. Sustained instruments
  (strings, pads) need loop points and crossfades.
- **No round-robin.** One recording per (key, velocity) region, so a repeated
  note is bit-identical — the "machine gun" effect. Salamander has no
  round-robin variations to use anyway.
- **Single mic position.** Salamander ships release samples (`rel*`), string
  resonance (`harmL*`), hammer noise and pedal noise. None are installed; they
  are separate mechanisms (release triggers, sympathetic layers), not more
  samples.
- **7-second cap.** See §5.
- **Three velocity layers of sixteen.** A budget decision. The layer boundaries
  are audible as steps under a slow crescendo.
- **No per-instrument parameters.** No tuning, no envelope, no filter, no
  level. Same reason as step 3: parameters need a parameter queue, smoothing and
  automation, and a knob wired to a field with no smoothing clicks.
- **No sample-start offset.** Salamander's SFZ carries a CC-controlled
  `offset_oncc98` per region; the default CC value is 0, so ignoring it is
  correct for the default state and wrong only for a control MYDAW does not
  expose.
- **Mono content costs double.** It is duplicated to stereo at load, to keep one
  code path in the voice. Every shipped instrument is stereo.
- **The downloader has no cancel button.** It can be left to finish or the app
  can be closed; `.part` files are overwritten on the next run.

---

## 11. Files

| Path | What |
| --- | --- |
| `assets-manifest.json` | Committed. URLs, SHA-256s and the full key/velocity mapping. Read by both installers. |
| `scripts/gen-asset-manifest.ps1` | Regenerates the above from the pinned SFZ. Run rarely. |
| `scripts/fetch-assets.ps1` | Installs and verifies. Run by users and CI. |
| `plugins/sampler/src/manifest.rs` | The format, its validation and its typed errors. |
| `plugins/sampler/src/decode.rs` | symphonia → interleaved stereo `f32`, with the truncation policy. |
| `plugins/sampler/src/bank.rs` | `SampleBank`, the flat lookup table, the fade. |
| `plugins/sampler/src/node.rs` | `SamplerNode`, voices, pitch, interpolation. |
| `plugins/sampler/src/registry.rs` | Runtime instrument discovery, `ArcSwap` hot-reload, id interning. |
| `plugins/sampler/src/download.rs` | The in-app installer. |
| `plugins/sampler/tests/live_download.rs` | The one network test, `#[ignore]`d. |
| `app/src/source.rs` | `Source::Sampler`, `SamplerSourceRef`, the picker DTO. |
| `app/src/tracks.rs` | `build_node` for a sampled source. |
| `app/src/state.rs` | Registry wiring, `asset_status`, `start_asset_download`. |
| `ui/src/shell/StatusBar.tsx` | The one UI line. |
