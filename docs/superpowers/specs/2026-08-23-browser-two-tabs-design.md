# Browser: two tabs, one drum machine

The browser is organised by *who implemented a thing*. The instrument menu is
organised by *what it does*. One of them had to go, and it was the browser.

## What was wrong

Three tabs — Sounds, Plugins, Audio — split on an axis nobody plays music along.

* **"Plugins" did not mean plugins.** Poly Synth is a plugin; so is the sampler,
  the sequencer, every effect. The tab meant *third-party, installed on your
  machine*, and said something else.
* **"Sounds" held three unrelated things**: six synthesized drums, two synths,
  seven sampled instruments. Their only shared property is that Rust knows the id.
* **Three ways to get a kick.** Synthesized `kick`, the `808-kit` sampled
  instrument, and loose wavs in the Packs folder — two of them in the same tab,
  in different groups, with nothing saying which to reach for.
* **Effects were not in the browser at all**, though the browser's own docstring
  claimed it showed "what you can put on a track".
* `drum_samples.rs` had already conceded the point in a comment: the synthesized
  drums exist, and then get rendered to wav so they behave like sample files.

## What it is now

**Two tabs.** Effects stay in the mixer's insert picker, which is where you
reach for them; the browser is for what a channel plays and where audio lives.

| Tab | Holds |
| --- | --- |
| **Instruments** | Poly Synth, Test Tone, every sampled instrument, every scanned CLAP and VST3 — one flat alphabetical list, built-ins first |
| **Files** | The Factory root (read-only, the sampled instruments' own wavs), the Packs drop folder, extra folders, session files |

**Flat, not foldered.** A search box and a format badge per row — `SYNTH`,
`SAMPLER`, `CLAP`, `VST3`, `TONE`. Musical-role folders were rejected because
CLAP and VST3 expose no reliable category, so every installed plugin would land
in "Other" — the old split wearing a different hat.

**The synthesized drums are gone.** `Source::Drum`, `Source::DrumKit`,
`SingleDrum` and `drum_samples.rs` are deleted outright, not hidden. A project
file naming one fails to load with a message that says so. `808-kit` replaces
them: the same machine, sampled, on a General MIDI map.

**The Factory root is new.** `assets/instruments/` is exposed in Files as a
read-only root, so a single 808 kick can still be dragged out as a one-shot or
chopped. One copy of the sounds, both workflows intact.

## The starter project

Four drum channels plus one melodic channel, as before — but the drum channels
are `sampler:808-kit` pinned to GM keys, which is what the sequencer's
`DEFAULT_ROW_KEYS` already assumed:

| Channel | Source | Step key |
| --- | --- | ---: |
| Kick | `sampler:808-kit` | 36 |
| Snare | `sampler:808-kit` | 38 |
| Closed Hat | `sampler:808-kit` | 42 |
| Open Hat | `sampler:808-kit` | 46 |
| Piano / Poly Synth | sampled piano if installed, else `poly-synth` | 60 |

`assets/` is gitignored, so a fresh clone has no samples at all. When `808-kit`
is not installed the starter is the melodic channel alone — deterministic, and
the same rule the piano channel already followed. `Project::starter` takes both
sources as arguments so tests never depend on what is on the disk.

## Testing

* `Project::starter` with and without a kit — track count, names, step keys.
* A project file naming `drum-kit` or `kick` fails to load, with the id in the error.
* The Factory library root lists instrument wavs and refuses writes.
* Browser: one list, built-ins sorted first, badge per row, search filters across
  built-ins and scanned plugins alike.
