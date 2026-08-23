# WAVE Factory Vol. 1

Six sampled instruments, downloaded on demand and pinned by SHA-256, alongside
the Salamander piano that came before them. 161 samples, 70.5 MiB, all CC0.

Everything here is *data*: the curation lives in
[`scripts/gen-factory-kits.mjs`](../scripts/gen-factory-kits.mjs), the result in
[`assets-manifest.json`](../assets-manifest.json), and the rules the result has
to obey in [`plugins/sampler/tests/factory_kits.rs`](../plugins/sampler/tests/factory_kits.rs).
No sample is re-encoded, resampled or normalised on the way in: the bytes on
disk are the bytes upstream published, which is the only thing that makes a
checksum pin mean anything.

## The kits

| Id | Name | Samples | Download | Source |
| --- | --- | ---: | ---: | --- |
| `808-kit` | 808 Kit | 30 | 2.1 MiB | tidalcycles/sounds-tr808-fischer |
| `808-bass` | 808 Bass | 10 | 2.1 MiB | tidalcycles/sounds-tr808-fischer |
| `percussion-fx` | Percussion & FX | 55 | 14.4 MiB | sgossner/VCSL |
| `studio-drums` | Studio Drums | 36 | 21.9 MiB | sgossner/VCSL |
| `marimba` | Marimba | 20 | 21.1 MiB | sgossner/VCSL |
| `glockenspiel` | Glockenspiel | 10 | 8.9 MiB | sgossner/VCSL |

## Licences and provenance

| Source | Pinned commit | Licence | Recorded by |
| --- | --- | --- | --- |
| [tidalcycles/sounds-tr808-fischer](https://github.com/tidalcycles/sounds-tr808-fischer) | `85fbecf1bec3` | [CC0-1.0](https://creativecommons.org/publicdomain/zero/1.0/) | Michael Fischer (Technopolis), 1994 — sampled from a Roland TR-808 |
| [sgossner/VCSL](https://github.com/sgossner/VCSL) | `c1ea7bcc3c73` | [CC0-1.0](https://creativecommons.org/publicdomain/zero/1.0/) | Versilian Studios LLC |

CC0 is a public-domain dedication: no credit, no royalties, no share-alike, for
you or for anything you make with these sounds. The credits above are given
because the people who recorded them earned them, and are written into
`assets/ATTRIBUTION.md` by whichever installer you run.

**Naming.** The kits are called "808" and nothing else. No manufacturer's name,
model number, logo or trade dress appears in an id, a name, a description or a
file name — provenance is a fact about where a recording came from and belongs
in the credits, which is where the line above puts it.
`factory_kits.rs::kit_names_carry_no_trade_dress` enforces this.

### What happened to AVL Drumkits

The acoustic kit was planned as [AVL Drumkits](https://bandshed.net/avldrumkits/)
by Glen MacArthur (CC-BY-SA). It is VCSL instead. Two reasons, both structural:

* **AVL ships as zip archives**, and this pipeline's unit of trust is one URL,
  one SHA-256, one file on disk. Teaching both installers *and* the in-app
  downloader to unpack an archive would mean the bytes we verify are not the
  bytes we play.
* **The archive URLs are not commit-pinned.** `BLACK_PEARL_2023_repack.zip` was
  itself a repack; the next one lands on the same URL and the checksum breaks
  with no way to tell a supply-chain problem from a maintenance release.

CC-BY-SA would also have put a share-alike obligation on a factory library, for
an acoustic kit VCSL already covers under CC0.

## Key maps

Drum kits sit on the **General MIDI percussion map**, which is what a pattern
written anywhere else already speaks. Every pad is a single key at its recorded
pitch (`key_lo == key_hi == root_key`, `tune_cents == 0`): a drum map is a table
of unrelated sounds, and resampling one is not a transposition.

### 808 Kit

The machine has more voices than GM has slots — five kick tones, five snare
tones — so the alternates sit on 24..33, the octave below the map.

| Key | Pad | | Key | Pad |
| ---: | --- | --- | ---: | --- |
| 24 | Kick, dark, short | | 40 | Snare, bright |
| 25 | Kick, mid, clipped | | 42 | Closed hat |
| 26 | Kick, bright, medium | | 44 | Pedal hat |
| 27 | Kick, brightest, short | | 45 | Low tom |
| 28 | Snare, dark, no snap | | 46 | Open hat |
| 29 | Snare, dark, snappy | | 47 | Mid tom |
| 30 | Snare, mid, snappy | | 48 | High tom |
| 31 | Snare, bright, snappy | | 49 | Cymbal, long |
| 32 | Cymbal, mid | | 51 | Cymbal, dark and short |
| 33 | Open hat, medium | | 56 | Cowbell |
| 35 | Kick, tight | | 62 | High conga |
| 36 | Kick | | 63 | Mid conga |
| 37 | Rimshot | | 64 | Low conga |
| 38 | Snare | | 70 | Maracas |
| 39 | Clap | | 75 | Claves |

File names carry the two front-panel settings each sample was recorded at:
`kick-tone50-decay25` is the tone knob at 50% and the decay knob at 25%.
Upstream spells 100% as `10` (`BD5010`); the kit spells it `100`.

### 808 Bass

The long-decay kick played chromatically over **C1..B3**, in five key bands of
about half an octave each, so no note is resampled more than seven semitones
from a recording.

| Keys | Tone | | Velocity | Tail |
| --- | --- | --- | --- | --- |
| 24..30 (C1..F#1) | 100 | | 1..79 | 2.0 s (decay 75) |
| 31..36 (G1..C2) | 75 | | 80..127 | 3.0 s (decay 100) |
| 37..42 (C#2..F#2) | 50 | | | |
| 43..49 (G2..C#3) | 25 | | | |
| 50..59 (D3..B3) | 0 | | | |

Tone runs *backwards* against the register, brightest at the bottom. Every band
plays the same recording at a different speed, and speed is timbre: pitching
down darkens and pitching up brightens, so the brightest tone setting goes where
the most downshift happens.

**Root pitch: MIDI 31 (G1), `tune_cents` −22.5.** Every one of the ten
recordings settles on **49.64 Hz**, which is G1 plus 22.5 cents — the tone knob
is a filter and the decay knob is an envelope, and neither touches the bridged-T
oscillator that makes the pitch, so one root is correct for the whole kit rather
than a simplification. `tune_cents` is negative because it is a correction: the
sampler plays `key - root_key + tune_cents/100` semitones from the recording, so
pulling something 22.5 cents sharp back into tune costs −22.5.

This number is measured, not assumed. The repository's general analyser,
`mydaw_pitch_analysis::detect_root_pitch`, **declines this material** — it
reports `Polyphonic` for all ten. It is not wrong to: it works at a 12 kHz
analysis rate with a 2048-point frame, which puts a 50 Hz fundamental in bin
eight and its harmonics tens of cents off where a harmonic series says they
should be, and "the partials do not line up" is exactly what its polyphony guard
is for. A sound designed to sweep is not the held note it was written for. So
`factory_kits.rs` asks it first, prints what it says, and then measures the
settled tail with a narrow single-purpose projection; the manifest has to agree
within a third of a semitone. It currently agrees within half a cent.

### Percussion & FX

GM percussion, continuing past 81 for the voices GM has no name for.

| Key | Pad | Layers | | Key | Pad | Layers |
| ---: | --- | ---: | --- | ---: | --- | ---: |
| 39 | Hand clap | 4 | | 74 | Long guiro | 1 |
| 54 | Tambourine | 2 | | 75 | Claves | 3 |
| 56 | Cowbell | 3 | | 76 | High woodblock | 2 |
| 58 | Vibraslap | 1 | | 77 | Low woodblock | 2 |
| 60 | High bongo | 3 | | 80 | Muted triangle | 1 |
| 61 | Low bongo | 3 | | 81 | Open triangle | 2 |
| 62 | Muted high conga | 2 | | 82 | Shaker, upstroke | 1 |
| 63 | Open high conga | 3 | | 83 | Shaker, downstroke | 1 |
| 64 | Low conga | 3 | | 84 | Cabasa rub | 1 |
| 67 | High agogo | 3 | | 85 | Tambourine shake | 1 |
| 68 | Low agogo | 2 | | 86 | Tambourine roll | 1 |
| 69 | Cabasa | 1 | | 87 | Cajon bass tone | 3 |
| 70 | Large shaker | 1 | | 88 | Cajon slap | 2 |
| 73 | Short guiro | 1 | | 89 | Cajon tap | 2 |

Where VCSL recorded real dynamics (`v1`/`v2`/`v3`, `vl1`..`vl4`) they become
velocity layers on one pad, split evenly over 1..127 — evenly, because VCSL does
not publish the level each take was played at and any other division would be
invented precision. Where it recorded round robins of the *same* dynamic they do
not: a round robin is not a dynamic, and pretending it is makes a pad get louder
when it should only get different.

VCSL's `Shaker, Small` folder is not used. Its WAVs carry a `fmt ` chunk
symphonia refuses ("malformed fmt\_pcm chunk"), so they would install and then
silently fail to load. Nothing is re-encoded to work around it — a file we
cannot ship byte-for-byte is a file we do not ship — so keys 82..84 come from
`Shaker, Large` and `Cabasa` instead. `factory_kits.rs` caught this by decoding
every installed sample rather than trusting the download, and still would.

### Studio Drums

| Key | Pad | Layers | | Key | Pad | Layers |
| ---: | --- | ---: | --- | ---: | --- | ---: |
| 31 | Sticks | 1 | | 45 | Low tom, soft mallet | 1 |
| 36 | Kick | 4 | | 46 | Open hat | 1 |
| 37 | Side stick | 1 | | 47 | High tom | 3 |
| 38 | Snare | 4 | | 49 | Crash | 3 |
| 40 | Snare 2 | 3 | | 50 | High tom, soft mallet | 1 |
| 42 | Closed hat | 4 | | 51 | Ride | 3 |
| 43 | Low tom | 3 | | 53 | Ride bell | 2 |
| 44 | Pedal hat | 1 | | 57 | Loose hat | 1 |

### Marimba and Glockenspiel

Pitched, so these transpose. Each recorded note answers for the keys around it,
split at the midpoint between neighbours; the two ends stop six semitones past
the outermost recording rather than running to 0 and 127, because a bar sampled
at F1 resampled down to C-1 is a rumble and an instrument that plays nothing
outside its range is more honest than one that plays that.

* **Marimba** — F1, C2, G2, B2, F3, C4, G4, B4, F5, C6 × soft and loud.
* **Glockenspiel** — G4, C5, G5, C6, C7 × soft and loud. VCSL's G6 exists as
  `soft` and `medium` but its loud take is a G#6, and a layer that jumps a
  semitone when you hit it harder is worse than no layer, so G6 is omitted.

Two of the three recorded dynamics, not three: VCSL's `med` sits between two
takes that already differ by more than a crossfade can hide, and a low marimba C
rings for eleven seconds — the third layer costs 10 MB of download for the least
audible third of the dynamic range.

## One-shots, and what each kit asks of the sampler

A drum pad is triggered, not held: a sixteenth-note kick on a step sequencer
must not be cut off mid-tail by the note-off that follows it. The three drum
kits therefore declare `"one_shot": true`, which
[`SamplerNode::note_off`](../plugins/sampler/src/node.rs) honours by ignoring
note-off entirely — the voice ends when the recording does, and is still
stealable and still choked by a retrigger of the same pad. (Earlier revisions
faked this with `release_ms: 30000`, a ramp so long it had not moved by the time
the sound was over. The flag replaced it.)

Everything else is played rather than triggered, and its `release_ms` is where
its Release knob starts:

| kit | one-shot | release | other defaults |
| --- | --- | --- | --- |
| 808 Kit | yes | 250 ms | — |
| 808 Bass | no | 150 ms | `mono: 1`, `glide: 60` |
| Percussion & FX | yes | 250 ms | — |
| Studio Drums | yes | 250 ms | — |
| Marimba | no | 1200 ms | — |
| Glockenspiel | no | 1200 ms | — |

`default_params` carries the rest of the sampler's knobs (`attack`, `decay`,
`sustain`, `release`, `glide`, `mono`) for a kit that wants them somewhere other
than the sampler's own defaults. They are *starting positions*: the track owns
them from the moment it exists, and the user's edits are theirs. 808 Bass ships
mono with a 60 ms glide because that — one voice, and an overlapping note
sliding to the new pitch instead of stacking on it — is what makes a chromatic
kick a trap 808 rather than a kick played chromatically.

## Regenerating

```sh
node scripts/gen-factory-kits.mjs --dry-run   # curation report, no network
node scripts/gen-factory-kits.mjs             # fetch, hash, emit, install
node scripts/gen-factory-kits.mjs --force     # re-download everything
```

The generator merges into `assets-manifest.json`, leaving the piano (and any
other instrument it does not own) untouched; `scripts/gen-asset-manifest.ps1`
does the same in the other direction. A file already on disk is reused unless
its upstream URL has changed since the last run — which is what stops a curation
edit that repoints a local name from silently keeping the old bytes.

After any change, run the checks that make the table trustworthy:

```sh
cargo test -p mydaw-plugin-sampler
cargo test -p mydaw-plugin-sampler --test live_download -- --ignored   # network
```
