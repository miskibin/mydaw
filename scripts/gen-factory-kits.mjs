#!/usr/bin/env node
// Regenerates the WAVE Factory Vol. 1 entries in assets-manifest.json.
//
// This is the *generator*, not the installer. It runs rarely -- when a pinned
// commit or the curated file list changes -- and its output,
// assets-manifest.json, is committed. Day to day you want
// scripts/fetch-assets.ps1, which only reads that file.
//
// What it does:
//
//   1. Downloads every curated file from its upstream repository at a PINNED
//      commit, straight from raw.githubusercontent.com. Nothing is converted:
//      the bytes on disk are the bytes upstream published, which is what makes
//      a SHA-256 pin meaningful.
//   2. Hashes and sizes each one.
//   3. Emits one instrument object per kit -- the same shape the Salamander
//      piano already uses -- and merges them into assets-manifest.json,
//      leaving every instrument this script does not own untouched.
//
// Node rather than PowerShell because the curation below is a data structure
// with two hundred rows in it, and because `ui/` already puts Node on every
// contributor's machine. The *installer* stays PowerShell: it is what CI and an
// offline install run, and it has to work on a clone with no node_modules.
//
// Usage:
//   node scripts/gen-factory-kits.mjs            # fetch what is missing, emit
//   node scripts/gen-factory-kits.mjs --force    # re-download everything
//   node scripts/gen-factory-kits.mjs --dry-run  # curation report, no network

import { createHash } from 'node:crypto';
import { mkdir, readFile, rename, stat, writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = dirname(dirname(fileURLToPath(import.meta.url)));
const MANIFEST_PATH = join(REPO_ROOT, 'assets-manifest.json');
const INSTRUMENTS_ROOT = join(REPO_ROOT, 'assets', 'instruments');

// ---------------------------------------------------------------- provenance

/**
 * Where the content comes from. Every URL this script emits is pinned to one of
 * these commits, so the bytes behind a checksum cannot change under it.
 *
 * Naming rule for anything user-visible: bare "808", never a manufacturer's
 * name or model number. The provenance belongs here and in the credits file,
 * not on a kit's label.
 */
const SOURCES = {
  tr808: {
    repo: 'https://github.com/tidalcycles/sounds-tr808-fischer',
    commit: '85fbecf1bec32553395625ea659e2a56dfd7c0e1',
    license: 'CC0-1.0',
    license_url: 'https://creativecommons.org/publicdomain/zero/1.0/',
    author: 'Michael Fischer',
    credit:
      'Sampled from a Roland TR-808 by Michael Fischer (Technopolis), 1994. ' +
      'Released to the public domain under CC0-1.0 via the TidalCycles sound library.',
  },
  vcsl: {
    repo: 'https://github.com/sgossner/VCSL',
    commit: 'c1ea7bcc3c7309650ab0da9d15c9cd1fbc4a4c7e',
    license: 'CC0-1.0',
    license_url: 'https://creativecommons.org/publicdomain/zero/1.0/',
    author: 'Versilian Studios LLC',
    credit:
      'Versilian Community Sample Library (VCSL) by Versilian Studios LLC, ' +
      'released to the public domain under CC0-1.0.',
  },
};

const rawBase = (source) => {
  const { repo, commit } = SOURCES[source];
  const path = new URL(repo).pathname.replace(/^\//, '');
  return `https://raw.githubusercontent.com/${path}/${commit}`;
};

/** Percent-encode a repository path segment by segment; `/` must survive. */
const encodePath = (path) => path.split('/').map(encodeURIComponent).join('/');

// -------------------------------------------------------------- kit builders

/**
 * A drum pad: one file, one key, its own recorded pitch.
 *
 * `root_key === key_lo === key_hi` is what makes a pad a pad. A drum map is a
 * table of unrelated sounds, not an instrument to transpose, so every entry
 * plays at the speed it was recorded at.
 */
const pad = (key, path, file, note, extra = {}) => ({
  path,
  file,
  note,
  root_key: key,
  key_lo: key,
  key_hi: key,
  vel_lo: 1,
  vel_hi: 127,
  tune_cents: 0,
  ...extra,
});

/**
 * A drum pad with velocity layers: `layers` is `[path, file, note]` softest
 * first, and the velocity range 1..127 is split evenly between them.
 *
 * Even splits rather than the recorded dynamics: VCSL labels its layers `v1`,
 * `v2`, `v3` without publishing the level each was played at, so any other
 * division would be invented precision.
 */
const layeredPad = (key, layers) => {
  const count = layers.length;
  return layers.map(([path, file, note], index) => ({
    path,
    file,
    note,
    root_key: key,
    key_lo: key,
    key_hi: key,
    vel_lo: index === 0 ? 1 : Math.round((127 * index) / count) + 1,
    vel_hi: index === count - 1 ? 127 : Math.round((127 * (index + 1)) / count),
    tune_cents: 0,
  }));
};

// ------------------------------------------------------------------- 808 Kit
//
// Layout. The General MIDI percussion map is the one a drum pattern written
// anywhere else already speaks, so the voices GM has a slot for sit in it:
//
//   35 kick (tight)     42 closed hat       51 cymbal (short/dark)
//   36 kick (main)      44 open hat (short) 56 cowbell
//   37 rimshot          45 low tom          62 conga (high)
//   38 snare (main)     46 open hat (long)  63 conga (mid)
//   39 clap             47 mid tom          64 conga (low)
//   40 snare (bright)   48 high tom         70 maracas
//                       49 cymbal (long)    75 claves
//
// The machine has more voices than GM has slots -- five kick tones, five snare
// tones -- so the alternates sit on 24..33, the octave below the map, where
// they are reachable without displacing anything a GM pattern expects.
//
// File names carry the two front-panel settings each sample was recorded at:
// `kick-tone50-decay25` is the tone knob at 50% and the decay knob at 25%.
// Upstream spells 100% as `10` (BD5010); this script spells it `100`.

const KIT_808 = [
  // Alternates, an octave below the GM map.
  pad(24, 'bd8/BD0025.WAV', 'kick-tone00-decay25.wav', 'Kick, dark, short'),
  pad(25, 'bd8/BD5000.WAV', 'kick-tone50-decay00.wav', 'Kick, mid, clipped'),
  pad(26, 'bd8/BD7550.WAV', 'kick-tone75-decay50.wav', 'Kick, bright, medium'),
  pad(27, 'bd8/BD1025.WAV', 'kick-tone100-decay25.wav', 'Kick, brightest, short'),
  pad(28, 'sd8/SD0000.WAV', 'snare-tone00-snap00.wav', 'Snare, dark, no snap'),
  pad(29, 'sd8/SD0075.WAV', 'snare-tone00-snap75.wav', 'Snare, dark, snappy'),
  pad(30, 'sd8/SD5075.WAV', 'snare-tone50-snap75.wav', 'Snare, mid, snappy'),
  pad(31, 'sd8/SD1075.WAV', 'snare-tone100-snap75.wav', 'Snare, bright, snappy'),
  pad(32, 'cy8/CY5050.WAV', 'cymbal-tone50-decay50.wav', 'Cymbal, mid'),
  pad(33, 'oh8/OH50.WAV', 'hat-open-decay50.wav', 'Open hat, medium'),
  // The General MIDI percussion map.
  pad(35, 'bd8/BD0000.WAV', 'kick-tone00-decay00.wav', 'Kick, tight'),
  pad(36, 'bd8/BD5050.WAV', 'kick-tone50-decay50.wav', 'Kick'),
  pad(37, 'rs8/RS.WAV', 'rimshot.wav', 'Rimshot'),
  pad(38, 'sd8/SD5025.WAV', 'snare-tone50-snap25.wav', 'Snare'),
  pad(39, 'cp8/CP.WAV', 'clap.wav', 'Clap'),
  pad(40, 'sd8/SD7550.WAV', 'snare-tone75-snap50.wav', 'Snare, bright'),
  pad(42, 'ch8/CH.WAV', 'hat-closed.wav', 'Closed hat'),
  pad(44, 'oh8/OH00.WAV', 'hat-open-decay00.wav', 'Pedal hat'),
  pad(45, 'lt8/LT50.WAV', 'tom-low.wav', 'Low tom'),
  pad(46, 'oh8/OH75.WAV', 'hat-open-decay75.wav', 'Open hat'),
  pad(47, 'mt8/MT50.WAV', 'tom-mid.wav', 'Mid tom'),
  pad(48, 'ht8/HT50.WAV', 'tom-high.wav', 'High tom'),
  pad(49, 'cy8/CY1075.WAV', 'cymbal-tone100-decay75.wav', 'Cymbal, long'),
  pad(51, 'cy8/CY0025.WAV', 'cymbal-tone00-decay25.wav', 'Cymbal, dark and short'),
  pad(56, 'cb8/CB.WAV', 'cowbell.wav', 'Cowbell'),
  pad(62, 'hc8/HC50.WAV', 'conga-high.wav', 'High conga'),
  pad(63, 'mc8/MC50.WAV', 'conga-mid.wav', 'Mid conga'),
  pad(64, 'lc8/LC50.WAV', 'conga-low.wav', 'Low conga'),
  pad(70, 'ma8/MA.WAV', 'maracas.wav', 'Maracas'),
  pad(75, 'cl8/CL.WAV', 'claves.wav', 'Claves'),
];

// ------------------------------------------------------------------ 808 Bass
//
// The long-decay kick, played as a bass instrument -- the sound the machine is
// actually famous for. Only the two longest decay settings are used: 100%
// (3.0 s) and 75% (2.0 s). Everything shorter is a drum, and lives in the kit.
//
// Key layout: five bands across C1..B3, each about half an octave, so no note
// is resampled more than seven semitones from a recorded one.
//
// Tone runs *backwards* against the register, brightest at the bottom. Every
// band plays the same recording at a different speed, and speed is timbre:
// pitching down darkens, pitching up brightens. Putting the brightest tone
// setting where the most downshift happens is what keeps the low C from
// turning to mud and the top B from turning to a click.
//
// Velocity picks the tail: a soft hit gets the 2.0 s decay, a hard one the
// 3.0 s. Not what the front panel does -- decay is a knob, not a dynamic -- but
// it is how the instrument is played, and it is the difference between a bass
// line that breathes and ten copies of one note.
//
// Root pitch: every one of these recordings settles on 49.64 Hz, which is G1
// (MIDI 31) plus 22.5 cents. That they all agree is not luck -- the tone knob
// is a filter and the decay knob is an envelope; neither touches the bridged-T
// oscillator that makes the pitch -- so one root serves the whole kit, and a
// per-file root would be inventing differences the machine does not have.
//
// `tune_cents` is negative because it is a correction, not a measurement: the
// sampler plays `key - root_key + tune_cents/100` semitones from the recording,
// so pulling a recording that is 22.5 cents sharp back into tune costs -22.5.
//
// Measured, not assumed: plugins/sampler/tests/factory_kits.rs re-derives the
// tail's fundamental from the installed files and fails if this drifts. The
// repository's general analyser (`mydaw_pitch_analysis::detect_root_pitch`)
// reports `Polyphonic` for all ten and cannot be used here -- see that test.
const BASS_808_ROOT_KEY = 31;
const BASS_808_TUNE_CENTS = -22.5;

// Tone descends as the register rises: see the note above on why.
const BASS_808_BANDS = [
  { lo: 24, hi: 30, tone: '100', label: 'C1-F#1' },
  { lo: 31, hi: 36, tone: '75', label: 'G1-C2' },
  { lo: 37, hi: 42, tone: '50', label: 'C#2-F#2' },
  { lo: 43, hi: 49, tone: '25', label: 'G2-C#3' },
  { lo: 50, hi: 59, tone: '00', label: 'D3-B3' },
];

const BASS_808_LAYERS = [
  { decay: '75', upstream: '75', vel_lo: 1, vel_hi: 79, tail: '2.0 s' },
  { decay: '100', upstream: '10', vel_lo: 80, vel_hi: 127, tail: '3.0 s' },
];

const BASS_808 = BASS_808_BANDS.flatMap((band) =>
  BASS_808_LAYERS.map((layer) => {
    const upstreamTone = band.tone === '100' ? '10' : band.tone;
    return {
      path: `bd8/BD${upstreamTone}${layer.upstream}.WAV`,
      file: `bass-tone${band.tone}-decay${layer.decay}.wav`,
      note: `${band.label}, ${layer.tail} tail`,
      root_key: BASS_808_ROOT_KEY,
      key_lo: band.lo,
      key_hi: band.hi,
      vel_lo: layer.vel_lo,
      vel_hi: layer.vel_hi,
      tune_cents: BASS_808_TUNE_CENTS,
    };
  }),
);

// ------------------------------------------------------------ Percussion & FX
//
// Hand percussion, on the General MIDI percussion map. GM runs out at 81, so
// the voices it has no name for -- shakers, cajon, tambourine articulations --
// continue on 82..89.
//
// Where VCSL recorded real dynamics (`v1`/`v2`/`v3`, `vl1`..`vl4`) they become
// velocity layers on one pad. Where it recorded round robins of the same
// dynamic they do not: a round robin is not a dynamic, and pretending it is
// makes a pad get louder when it should only get different.

const P = 'Idiophones/Struck Idiophones';
const M = 'Membranophones/Struck Membranophones';

const PERCUSSION = [
  ...layeredPad(39, [
    [`${P}/Claps/SoloClap_vl1.wav`, 'clap-v1.wav', 'Hand clap, soft'],
    [`${P}/Claps/SoloClap_vl2.wav`, 'clap-v2.wav', 'Hand clap'],
    [`${P}/Claps/SoloClap_vl3.wav`, 'clap-v3.wav', 'Hand clap, loud'],
    [`${P}/Claps/SoloClap_vl4.wav`, 'clap-v4.wav', 'Hand clap, hardest'],
  ]),
  ...layeredPad(54, [
    [`${P}/Tambourine 1/Tamb1_Hit_v1_rr1_Mid.wav`, 'tambourine-v1.wav', 'Tambourine, soft'],
    [`${P}/Tambourine 1/Tamb1_Hit_v2_rr1_Mid.wav`, 'tambourine-v2.wav', 'Tambourine'],
  ]),
  ...layeredPad(56, [
    [`${P}/Cowbells/Cowbell1_Hit_v2_rr1_Mid.wav`, 'cowbell-v1.wav', 'Cowbell, soft'],
    [`${P}/Cowbells/Cowbell1_Hit_v3_rr1_Mid.wav`, 'cowbell-v2.wav', 'Cowbell'],
    [`${P}/Cowbells/Cowbell1_Hit_v4_rr1_Mid.wav`, 'cowbell-v3.wav', 'Cowbell, loud'],
  ]),
  pad(58, `${P}/Vibraslap/Legacy/vibraslap_rr1.wav`, 'vibraslap.wav', 'Vibraslap'),
  ...layeredPad(60, [
    [`${M}/Bongos/BongoH_Hit1_v1_rr1_Mid.wav`, 'bongo-high-v1.wav', 'High bongo, soft'],
    [`${M}/Bongos/BongoH_Hit1_v2_rr1_Mid.wav`, 'bongo-high-v2.wav', 'High bongo'],
    [`${M}/Bongos/BongoH_Hit1_v3_rr1_Mid.wav`, 'bongo-high-v3.wav', 'High bongo, loud'],
  ]),
  ...layeredPad(61, [
    [`${M}/Bongos/BongoL_Hit1_v1_rr1_Mid.wav`, 'bongo-low-v1.wav', 'Low bongo, soft'],
    [`${M}/Bongos/BongoL_Hit1_v2_rr1_Mid.wav`, 'bongo-low-v2.wav', 'Low bongo'],
    [`${M}/Bongos/BongoL_Hit1_v3_rr1_Mid.wav`, 'bongo-low-v3.wav', 'Low bongo, loud'],
  ]),
  ...layeredPad(62, [
    [`${M}/Conga/Quinto_HitFM1_v1_rr1_Sum.wav`, 'conga-high-muted-v1.wav', 'Muted high conga, soft'],
    [`${M}/Conga/Quinto_HitFM1_v2_rr1_Sum.wav`, 'conga-high-muted-v2.wav', 'Muted high conga'],
  ]),
  ...layeredPad(63, [
    [`${M}/Conga/Quinto_HitN_v1_rr1_Sum.wav`, 'conga-high-open-v1.wav', 'Open high conga, soft'],
    [`${M}/Conga/Quinto_HitN_v2_rr1_Sum.wav`, 'conga-high-open-v2.wav', 'Open high conga'],
    [`${M}/Conga/Quinto_HitN_v3_rr1_Sum.wav`, 'conga-high-open-v3.wav', 'Open high conga, loud'],
  ]),
  ...layeredPad(64, [
    [`${M}/Conga/Tumba_HitN_v1_rr1_Sum.wav`, 'conga-low-v1.wav', 'Low conga, soft'],
    [`${M}/Conga/Tumba_HitN_v2_rr1_Sum.wav`, 'conga-low-v2.wav', 'Low conga'],
    [`${M}/Conga/Tumba_HitN_v3_rr1_Sum.wav`, 'conga-low-v3.wav', 'Low conga, loud'],
  ]),
  ...layeredPad(67, [
    [`${P}/Agogo Bells/Agogo_High_v1_rr1_Mid.wav`, 'agogo-high-v1.wav', 'High agogo, soft'],
    [`${P}/Agogo Bells/Agogo_High_v2_rr1_Mid.wav`, 'agogo-high-v2.wav', 'High agogo'],
    [`${P}/Agogo Bells/Agogo_High_v3_rr1_Mid.wav`, 'agogo-high-v3.wav', 'High agogo, loud'],
  ]),
  ...layeredPad(68, [
    [`${P}/Agogo Bells/Agogo_Low_v1_rr1_Mid.wav`, 'agogo-low-v1.wav', 'Low agogo, soft'],
    [`${P}/Agogo Bells/Agogo_Low_v2_rr1_Mid.wav`, 'agogo-low-v2.wav', 'Low agogo'],
  ]),
  pad(69, `${P}/Cabasa/Cabasa1_Hit_rr1_Mid.wav`, 'cabasa.wav', 'Cabasa'),
  pad(70, `${P}/Shaker, Large/LShaker_Hit_rr1_Mid.wav`, 'shaker-large.wav', 'Large shaker'),
  pad(73, `${P}/Guiro/Guiro_Hit_rr1_Mid.wav`, 'guiro-short.wav', 'Short guiro'),
  pad(74, `${P}/Guiro/Guiro_Slow_rr2_Mid.wav`, 'guiro-long.wav', 'Long guiro'),
  ...layeredPad(75, [
    [`${P}/Claves/Claves1_Hit_v1_rr1_Mid.wav`, 'claves-v1.wav', 'Claves, soft'],
    [`${P}/Claves/Claves1_Hit_v2_rr1_Mid.wav`, 'claves-v2.wav', 'Claves'],
    [`${P}/Claves/Claves1_Hit_v3_rr1_Mid.wav`, 'claves-v3.wav', 'Claves, loud'],
  ]),
  ...layeredPad(76, [
    [`${P}/Woodblock/wood_click3_vl1.wav`, 'woodblock-high-v1.wav', 'High woodblock, soft'],
    [`${P}/Woodblock/wood_click3_vl2.wav`, 'woodblock-high-v2.wav', 'High woodblock'],
  ]),
  ...layeredPad(77, [
    [`${P}/Woodblock/wood_click_mp.wav`, 'woodblock-low-v1.wav', 'Low woodblock, soft'],
    [`${P}/Woodblock/wood_click_ff.wav`, 'woodblock-low-v2.wav', 'Low woodblock, loud'],
  ]),
  pad(80, `${P}/Triangles/Triangle1_HitM_v1_rr2_Mid.wav`, 'triangle-muted.wav', 'Muted triangle'),
  ...layeredPad(81, [
    [`${P}/Triangles/Triangle1_Hit_v1_rr1_Mid.wav`, 'triangle-open-v1.wav', 'Open triangle, soft'],
    [`${P}/Triangles/Triangle1_Hit_v2_rr1_Mid.wav`, 'triangle-open-v2.wav', 'Open triangle'],
  ]),
  // Past the end of General MIDI.
  //
  // These three come from `Shaker, Large` rather than `Shaker, Small`, whose
  // WAVs upstream carry a `fmt ` chunk symphonia refuses ("malformed fmt_pcm
  // chunk") -- they would install and then silently not load. Nothing here is
  // re-encoded to work around it: a file we cannot ship byte-for-byte is a file
  // we do not ship. `factory_kits.rs` is what caught it, by decoding every
  // installed sample rather than trusting the download.
  pad(82, `${P}/Shaker, Large/LShaker_Shake1U_rr1_Mid.wav`, 'shaker-up.wav', 'Shaker, upstroke'),
  pad(
    83,
    `${P}/Shaker, Large/LShaker_Shake1D_rr2_Mid.wav`,
    'shaker-down.wav',
    'Shaker, downstroke',
  ),
  pad(84, `${P}/Cabasa/Cabasa1_Rub_v2_rr1_Mid.wav`, 'cabasa-rub.wav', 'Cabasa rub'),
  pad(85, `${P}/Tambourine 1/Tamb1_Shake_rr1_Mid.wav`, 'tambourine-shake.wav', 'Tambourine shake'),
  pad(86, `${P}/Tambourine 1/Tamb1_Roll_v2_rr1_Mid.wav`, 'tambourine-roll.wav', 'Tambourine roll'),
  ...layeredPad(87, [
    [`${P}/Cajon/Cajon_hit1_mp_rr1.wav`, 'cajon-bass-v1.wav', 'Cajon bass tone, soft'],
    [`${P}/Cajon/Cajon_hit1_f_rr1.wav`, 'cajon-bass-v2.wav', 'Cajon bass tone'],
    [`${P}/Cajon/Cajon_hit1_fff_rr1.wav`, 'cajon-bass-v3.wav', 'Cajon bass tone, loud'],
  ]),
  ...layeredPad(88, [
    [`${P}/Cajon/Cajon_hit2_mp_rr1.wav`, 'cajon-slap-v1.wav', 'Cajon slap, soft'],
    [`${P}/Cajon/Cajon_hit2_f_rr1.wav`, 'cajon-slap-v2.wav', 'Cajon slap'],
  ]),
  ...layeredPad(89, [
    [`${P}/Cajon/Cajon_hit3_p_rr1.wav`, 'cajon-tap-v1.wav', 'Cajon tap, soft'],
    [`${P}/Cajon/Cajon_hit3_f_rr1.wav`, 'cajon-tap-v2.wav', 'Cajon tap'],
  ]),
];

// -------------------------------------------------------------- Studio Drums
//
// An acoustic kit, on the General MIDI percussion map.
//
// This kit was planned as AVL Drumkits (Glen MacArthur, CC-BY-SA). It is not:
// AVL ships as five zip archives, and this pipeline's unit of trust is one
// URL, one SHA-256, one file on disk. Teaching both installers to unpack an
// archive would mean the bytes we verify are not the bytes we play, and the
// archive names are not commit-pinned, so the checksum could break silently
// when a repack lands. VCSL is CC0, per-file, and pinned to a commit. See
// docs/factory-library.md.

const D = 'Membranophones/Struck Membranophones';
const C = 'Idiophones/Struck Idiophones';

const STUDIO_DRUMS = [
  pad(31, `${D}/Snare Drum, Modern 1/Snare2_stick_v1_rr1_Mid.wav`, 'sticks.wav', 'Sticks'),
  ...layeredPad(36, [
    [`${D}/Bass Drum 1/BDrumNew_hit_v2_rr1_Sum.wav`, 'kick-v1.wav', 'Kick, soft'],
    [`${D}/Bass Drum 1/BDrumNew_hit_v3_rr1_Sum.wav`, 'kick-v2.wav', 'Kick'],
    [`${D}/Bass Drum 1/BDrumNew_hit_v5_rr1_Sum.wav`, 'kick-v3.wav', 'Kick, loud'],
    [`${D}/Bass Drum 1/BDrumNew_hit_v7_rr1_Sum.wav`, 'kick-v4.wav', 'Kick, hardest'],
  ]),
  pad(
    37,
    `${D}/Snare Drum, Modern 2/Snare3M_Xstick_v2_rr1_Mid.wav`,
    'side-stick.wav',
    'Side stick',
  ),
  ...layeredPad(38, [
    [`${D}/Snare Drum, Modern 1/Snare2_HitSN_v3_rr1_Mid.wav`, 'snare-v1.wav', 'Snare, soft'],
    [`${D}/Snare Drum, Modern 1/Snare2_HitSN_v5_rr1_Mid.wav`, 'snare-v2.wav', 'Snare'],
    [`${D}/Snare Drum, Modern 1/Snare2_HitSN_v7_rr1_Mid.wav`, 'snare-v3.wav', 'Snare, loud'],
    [`${D}/Snare Drum, Modern 1/Snare2_HitSN_v9_rr1_Mid.wav`, 'snare-v4.wav', 'Snare, hardest'],
  ]),
  ...layeredPad(40, [
    [`${D}/Snare Drum, Modern 2/Snare3M_HitSN_v2_rr1_Mid.wav`, 'snare2-v1.wav', 'Snare 2, soft'],
    [`${D}/Snare Drum, Modern 2/Snare3M_HitSN_v3_rr1_Mid.wav`, 'snare2-v2.wav', 'Snare 2'],
    [`${D}/Snare Drum, Modern 2/Snare3M_HitSN_v5_rr1_Mid.wav`, 'snare2-v3.wav', 'Snare 2, loud'],
  ]),
  ...layeredPad(42, [
    [`${C}/Hi-Hat Cymbal/HiHat_HitC_v1_rr1_Mid.wav`, 'hat-closed-v1.wav', 'Closed hat, soft'],
    [`${C}/Hi-Hat Cymbal/HiHat_HitC_v2_rr1_Mid.wav`, 'hat-closed-v2.wav', 'Closed hat'],
    [`${C}/Hi-Hat Cymbal/HiHat_HitC_v3_rr1_Mid.wav`, 'hat-closed-v3.wav', 'Closed hat, loud'],
    [`${C}/Hi-Hat Cymbal/HiHat_HitC_v4_rr1_Mid.wav`, 'hat-closed-v4.wav', 'Closed hat, hardest'],
  ]),
  ...layeredPad(43, [
    [`${D}/Tom 2/Stick/TomL_HitS_v2_rr1_Mid.wav`, 'tom-low-v1.wav', 'Low tom, soft'],
    [`${D}/Tom 2/Stick/TomL_HitS_v3_rr1_Mid.wav`, 'tom-low-v2.wav', 'Low tom'],
    [`${D}/Tom 2/Stick/TomL_HitS_v4_rr1_Mid.wav`, 'tom-low-v3.wav', 'Low tom, loud'],
  ]),
  pad(44, `${C}/Hi-Hat Cymbal/HiHat_Close_rr1_Mid.wav`, 'hat-pedal.wav', 'Pedal hat'),
  pad(
    45,
    `${D}/Tom 2/Mallet/TomL_HitM_v3_rr1_Mid.wav`,
    'tom-low-mallet.wav',
    'Low tom, soft mallet',
  ),
  pad(46, `${C}/Hi-Hat Cymbal/HiHat_HitO_rr1_Mid.wav`, 'hat-open.wav', 'Open hat'),
  ...layeredPad(47, [
    [`${D}/Tom 1/Stick/TomH_HitS_v2_rr1_Mid.wav`, 'tom-high-v1.wav', 'High tom, soft'],
    [`${D}/Tom 1/Stick/TomH_HitS_v3_rr1_Mid.wav`, 'tom-high-v2.wav', 'High tom'],
    [`${D}/Tom 1/Stick/TomH_HitS_v4_rr1_Mid.wav`, 'tom-high-v3.wav', 'High tom, loud'],
  ]),
  ...layeredPad(49, [
    [`${C}/Suspended Cymbal 1/susCymb1_hit_mp1.wav`, 'crash-v1.wav', 'Crash, soft'],
    [`${C}/Suspended Cymbal 1/susCymb1_hit_f1.wav`, 'crash-v2.wav', 'Crash'],
    [`${C}/Suspended Cymbal 1/susCymb1_hit_fff1.wav`, 'crash-v3.wav', 'Crash, loud'],
  ]),
  pad(
    50,
    `${D}/Tom 1/Mallet/TomH_HitM_v3_rr1_Mid.wav`,
    'tom-high-mallet.wav',
    'High tom, soft mallet',
  ),
  ...layeredPad(51, [
    [`${C}/Suspended Cymbal 1/susCymb1_hit_stick_pp1.wav`, 'ride-v1.wav', 'Ride, soft'],
    [`${C}/Suspended Cymbal 1/susCymb1_hit_stick_mp1.wav`, 'ride-v2.wav', 'Ride'],
    [`${C}/Suspended Cymbal 1/susCymb1_hit_stick_f1.wav`, 'ride-v3.wav', 'Ride, loud'],
  ]),
  ...layeredPad(53, [
    [`${C}/Suspended Cymbal 1/susCymb1_hit_bell_mf1.wav`, 'ride-bell-v1.wav', 'Ride bell'],
    [`${C}/Suspended Cymbal 1/susCymb1_hit_bell_fff1.wav`, 'ride-bell-v2.wav', 'Ride bell, loud'],
  ]),
  pad(57, `${C}/Hi-Hat Cymbal/HiHat_HitLoose_rr1_Mid.wav`, 'hat-loose.wav', 'Loose hat'),
];

// ------------------------------------------------------------------- Marimba
//
// Wholetone-sampled, three dynamics, five octaves. VCSL records F1, C2, G2,
// B2, F3, C4, G4, B4, F5, C6; each recorded note answers for the keys around
// it, split at the midpoint between neighbours, so nothing is resampled more
// than a minor third.

const MARIMBA_NOTES = [
  { name: 'F1', key: 29 },
  { name: 'C2', key: 36 },
  { name: 'G2', key: 43 },
  { name: 'B2', key: 47 },
  { name: 'F3', key: 53 },
  { name: 'C4', key: 60 },
  { name: 'G4', key: 67 },
  { name: 'B4', key: 71 },
  { name: 'F5', key: 77 },
  { name: 'C6', key: 84 },
];

// Two of the three recorded dynamics. VCSL's `med` sits between two takes that
// already differ by more than a sampler's crossfade can hide, and a low C bar
// rings for eleven seconds: the third layer costs 10 MB of download for the
// least audible third of the dynamic range.
const MARIMBA_DYNAMICS = [
  { upstream: 'soft', label: 'soft' },
  { upstream: 'loud', label: 'loud' },
];

/**
 * Split the keyboard at the midpoints between recorded notes.
 *
 * The two ends stop `reach` semitones past the outermost recording rather than
 * running to 0 and 127: a bar sampled at F1 resampled down to C-1 is not a
 * marimba, it is a rumble, and an instrument that plays nothing outside its
 * range is more honest than one that plays that.
 */
const tileKeys = (notes, reach = 6) =>
  notes.map((note, index) => ({
    ...note,
    key_lo:
      index === 0
        ? Math.max(0, note.key - reach)
        : Math.floor((notes[index - 1].key + note.key) / 2) + 1,
    key_hi:
      index === notes.length - 1
        ? Math.min(127, note.key + reach)
        : Math.floor((note.key + notes[index + 1].key) / 2),
  }));

/** Split `1..=127` evenly between `count` velocity layers. */
const tileVelocity = (index, count) => ({
  vel_lo: index === 0 ? 1 : Math.round((127 * index) / count) + 1,
  vel_hi: index === count - 1 ? 127 : Math.round((127 * (index + 1)) / count),
});

const MARIMBA = tileKeys(MARIMBA_NOTES).flatMap((note) =>
  MARIMBA_DYNAMICS.map((dynamic, index) => ({
    path: `${C}/Marimba/Marimba_hit_Outrigger_${note.name}_${dynamic.upstream}_01.wav`,
    file: `marimba-${note.name}-${dynamic.label}.wav`,
    note: `${note.name}, ${dynamic.label}`,
    root_key: note.key,
    key_lo: note.key_lo,
    key_hi: note.key_hi,
    ...tileVelocity(index, MARIMBA_DYNAMICS.length),
    tune_cents: 0,
  })),
);

// -------------------------------------------------------------- Glockenspiel
//
// Five recorded bars over two and a half octaves, three dynamics each. Only
// the notes VCSL recorded at all three dynamics are used: its G6 exists as
// `soft` and `medium` but the loud take is a G#6, and a layer that jumps a
// semitone when you hit it harder is worse than no layer.

const GLOCKENSPIEL_NOTES = [
  { name: 'G4', key: 67, takes: { soft: '01', medium: '01', loud: '01' } },
  { name: 'C5', key: 72, takes: { soft: '02', medium: '01', loud: '01' } },
  { name: 'G5', key: 79, takes: { soft: '01', medium: '01', loud: '01' } },
  { name: 'C6', key: 84, takes: { soft: '01', medium: '01', loud: '01' } },
  { name: 'C7', key: 96, takes: { soft: '03', medium: '01', loud: '01' } },
];

const GLOCKENSPIEL_DYNAMICS = ['soft', 'loud'];

const GLOCKENSPIEL = tileKeys(GLOCKENSPIEL_NOTES).flatMap((note) =>
  GLOCKENSPIEL_DYNAMICS.map((dynamic, index) => ({
    path: `${C}/Glockenspiel/glock_${dynamic}_${note.name}_${note.takes[dynamic]}.wav`,
    file: `glockenspiel-${note.name}-${dynamic}.wav`,
    note: `${note.name}, ${dynamic}`,
    root_key: note.key,
    key_lo: note.key_lo,
    key_hi: note.key_hi,
    ...tileVelocity(index, GLOCKENSPIEL_DYNAMICS.length),
    tune_cents: 0,
  })),
);

// ---------------------------------------------------------------------- kits

// A drum pad has to survive a short gate. The sampler starts a linear release
// on note-off, and a sixteenth-note kick on a step sequencer would otherwise be
// cut off mid-tail. A release far longer than any sample here means the ramp is
// still near 1.0 when the sound has finished decaying on its own -- 30 s over a
// 3 s tail is a tenth of a decibel. It is the only knob the manifest has for
// "this is a one-shot"; a real `one_shot` flag would be the better answer.
const ONE_SHOT_RELEASE_MS = 30_000;

const KITS = [
  {
    id: '808-kit',
    name: '808 Kit',
    description: '30 analogue drum-machine one-shots on a General MIDI drum map',
    source: 'tr808',
    release_ms: ONE_SHOT_RELEASE_MS,
    // Nothing here is longer than the 4 s cymbal; the cap is a bound, not a cut.
    max_seconds: 6.0,
    fade_out_seconds: 0.25,
    samples: KIT_808,
  },
  {
    id: '808-bass',
    name: '808 Bass',
    description: 'Long-decay analogue kick played chromatically, C1-B3, two tail lengths',
    source: 'tr808',
    // A bass note is played, not triggered: it stops when the key does.
    release_ms: 120,
    max_seconds: 6.0,
    fade_out_seconds: 0.25,
    samples: BASS_808,
  },
  {
    id: 'percussion-fx',
    name: 'Percussion & FX',
    description: 'Hand percussion on a General MIDI map, velocity-layered where recorded',
    source: 'vcsl',
    release_ms: ONE_SHOT_RELEASE_MS,
    max_seconds: 8.0,
    fade_out_seconds: 0.5,
    samples: PERCUSSION,
  },
  {
    id: 'studio-drums',
    name: 'Studio Drums',
    description: 'Acoustic kit on a General MIDI map, up to four velocity layers per pad',
    source: 'vcsl',
    release_ms: ONE_SHOT_RELEASE_MS,
    max_seconds: 8.0,
    fade_out_seconds: 0.5,
    samples: STUDIO_DRUMS,
  },
  {
    id: 'marimba',
    name: 'Marimba',
    description: 'Five octaves, wholetone-sampled, three dynamics',
    source: 'vcsl',
    // A mallet lifts off the bar; the bar keeps ringing.
    release_ms: 1_200,
    max_seconds: 8.0,
    fade_out_seconds: 0.5,
    samples: MARIMBA,
  },
  {
    id: 'glockenspiel',
    name: 'Glockenspiel',
    description: 'Two and a half octaves, three dynamics',
    source: 'vcsl',
    release_ms: 1_200,
    max_seconds: 8.0,
    fade_out_seconds: 0.5,
    samples: GLOCKENSPIEL,
  },
];

// ------------------------------------------------------------------ validate

/** Refuse a curation that cannot mean anything, before a byte is downloaded. */
function checkCuration() {
  const problems = [];
  const ids = new Set();
  for (const kit of KITS) {
    if (ids.has(kit.id)) problems.push(`duplicate kit id '${kit.id}'`);
    ids.add(kit.id);
    if (!SOURCES[kit.source]) problems.push(`${kit.id}: unknown source '${kit.source}'`);
    if (kit.samples.length === 0) problems.push(`${kit.id}: no samples`);

    const files = new Set();
    const zones = new Map();
    for (const sample of kit.samples) {
      const where = `${kit.id}/${sample.file}`;
      // One local name per kit: the installer writes them into one flat
      // directory, and two entries with the same name would race.
      if (files.has(sample.file)) problems.push(`${where}: duplicate file name`);
      files.add(sample.file);
      if (/[\\/]/.test(sample.file)) problems.push(`${where}: not a plain file name`);
      for (const [label, value] of [
        ['root_key', sample.root_key],
        ['key_lo', sample.key_lo],
        ['key_hi', sample.key_hi],
      ]) {
        if (!Number.isInteger(value) || value < 0 || value > 127) {
          problems.push(`${where}: ${label} ${value} is not a MIDI key`);
        }
      }
      if (sample.key_lo > sample.key_hi) problems.push(`${where}: key range inverted`);
      if (sample.vel_lo > sample.vel_hi) problems.push(`${where}: velocity range inverted`);
      if (sample.vel_lo < 1 || sample.vel_hi > 127) problems.push(`${where}: velocity out of range`);
      // Two entries answering for the same key *and* velocity is a silent
      // shadow: `entry_for` takes the first and the second never sounds.
      for (let key = sample.key_lo; key <= sample.key_hi; key += 1) {
        for (let velocity = sample.vel_lo; velocity <= sample.vel_hi; velocity += 1) {
          const slot = key * 128 + velocity;
          if (zones.has(slot)) {
            problems.push(
              `${where}: key ${key} velocity ${velocity} is already covered by ${zones.get(slot)}`,
            );
            velocity = sample.vel_hi;
            key = sample.key_hi;
          } else {
            zones.set(slot, sample.file);
          }
        }
      }
    }
  }
  if (problems.length > 0) {
    throw new Error(`the curation is not usable:\n  ${problems.join('\n  ')}`);
  }
}

// ------------------------------------------------------------------ download

const sha256 = (bytes) => createHash('sha256').update(bytes).digest('hex');

async function sizeOf(path) {
  try {
    const info = await stat(path);
    return info.isFile() ? info.size : null;
  } catch {
    return null;
  }
}

/**
 * Fetch `url` into `path` unless the right file is already there, and return
 * its bytes.
 *
 * `stale` is what makes "already there" trustworthy. Curation edits change
 * which upstream file a local name points at -- three shakers moved from one
 * VCSL folder to another when the first folder turned out to ship a `fmt `
 * chunk symphonia refuses -- and a name-only check would have kept serving the
 * old bytes under the new URL, quietly, for as long as nobody deleted them.
 *
 * Written as `<path>.part` and renamed only once it is whole, so an interrupted
 * run never leaves a truncated sample where the loader would decode it -- the
 * same rule the installer and the in-app downloader follow.
 */
async function fetchInto(url, path, force, stale) {
  if (!force && !stale) {
    const present = await sizeOf(path);
    if (present !== null) return readFile(path);
  }
  const response = await fetch(url);
  if (!response.ok) throw new Error(`GET ${url} -> HTTP ${response.status}`);
  const bytes = Buffer.from(await response.arrayBuffer());
  if (bytes.length === 0) throw new Error(`GET ${url} -> empty body`);
  const part = `${path}.part`;
  await writeFile(part, bytes);
  await rename(part, path);
  return bytes;
}

// --------------------------------------------------------------------- emit

/** One instrument object, in the shape both installers and the loader read. */
function instrumentOf(kit, samples) {
  const source = SOURCES[kit.source];
  return {
    id: kit.id,
    name: kit.name,
    description: kit.description,
    release_ms: kit.release_ms,
    max_seconds: kit.max_seconds,
    fade_out_seconds: kit.fade_out_seconds,
    attribution: source.credit,
    source: {
      repo: source.repo,
      commit: source.commit,
      license: source.license,
      license_url: source.license_url,
      author: source.author,
    },
    samples,
  };
}

async function main() {
  const force = process.argv.includes('--force');
  const dryRun = process.argv.includes('--dry-run');

  checkCuration();
  const total = KITS.reduce((sum, kit) => sum + kit.samples.length, 0);
  console.log(`WAVE Factory Vol. 1: ${KITS.length} kits, ${total} samples`);
  if (dryRun) {
    for (const kit of KITS) {
      const keys = new Set(kit.samples.map((sample) => sample.key_lo));
      console.log(`  ${kit.id.padEnd(14)} ${String(kit.samples.length).padStart(3)} samples, \
${keys.size} zones, ${kit.source}`);
    }
    return;
  }

  // What the last run recorded, so a file whose upstream path changed can be
  // spotted before it is trusted. Read up front because the merge at the end
  // rewrites it.
  const previous = JSON.parse(await readFile(MANIFEST_PATH, 'utf8'));
  const recordedUrl = new Map(
    (Array.isArray(previous.instruments) ? previous.instruments : [previous]).flatMap(
      (instrument) =>
        instrument.samples.map((sample) => [`${instrument.id}/${sample.file}`, sample.url]),
    ),
  );

  const instruments = [];
  let grandTotal = 0;
  for (const kit of KITS) {
    const dir = join(INSTRUMENTS_ROOT, kit.id);
    await mkdir(dir, { recursive: true });
    const base = rawBase(kit.source);
    const samples = [];
    let bytes = 0;
    let index = 0;
    for (const sample of kit.samples) {
      index += 1;
      const url = `${base}/${encodePath(sample.path)}`;
      const key = `${kit.id}/${sample.file}`;
      const stale = recordedUrl.has(key) && recordedUrl.get(key) !== url;
      const content = await fetchInto(url, join(dir, sample.file), force, stale);
      bytes += content.length;
      process.stdout.write(`\r  ${kit.id}: ${index}/${kit.samples.length}   `);
      samples.push({
        file: sample.file,
        root_key: sample.root_key,
        key_lo: sample.key_lo,
        key_hi: sample.key_hi,
        vel_lo: sample.vel_lo,
        vel_hi: sample.vel_hi,
        tune_cents: sample.tune_cents,
        note: sample.note,
        url,
        sha256: sha256(content),
        bytes: content.length,
      });
    }
    grandTotal += bytes;
    const instrument = instrumentOf(kit, samples);

    // The runtime manifest is this instrument's slice of the download
    // manifest, verbatim -- the same file scripts/fetch-assets.ps1 and the
    // in-app downloader write, so a generator run leaves a *working* install
    // rather than a directory of loose wavs. Written last: its presence is
    // what `SamplerRegistry` and `AssetDownloader::is_installed` read as
    // "the samples beside this are complete".
    const runtime = join(dir, 'manifest.json');
    await writeFile(`${runtime}.part`, `${JSON.stringify(instrument, null, 2)}\n`, 'utf8');
    await rename(`${runtime}.part`, runtime);

    console.log(
      `\r  ${kit.id.padEnd(14)} ${String(samples.length).padStart(3)} samples, ` +
        `${(bytes / 1e6).toFixed(1)} MB`,
    );
    instruments.push(instrument);
  }

  // Merge, rather than replace: `assets-manifest.json` also carries the
  // Salamander piano, whose generator is scripts/gen-asset-manifest.ps1.
  const kept = (
    Array.isArray(previous.instruments) ? previous.instruments : [previous]
  ).filter((instrument) => !KITS.some((kit) => kit.id === instrument.id));
  const merged = {
    schema: 2,
    instruments: [...kept, ...instruments],
  };
  await writeFile(MANIFEST_PATH, `${JSON.stringify(merged, null, 2)}\n`, 'utf8');

  console.log('');
  console.log(`Wrote ${MANIFEST_PATH}`);
  console.log(
    `  ${merged.instruments.length} instruments, ` +
      `${merged.instruments.reduce((sum, i) => sum + i.samples.length, 0)} samples`,
  );
  console.log(`  this run fetched ${(grandTotal / 1e6).toFixed(1)} MB into ${INSTRUMENTS_ROOT}`);
}

await main();
