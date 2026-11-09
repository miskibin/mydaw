# Built-in Reverb and Limiter v2: design notes and measurements

Code: `crates/engine/src/dsp/reverb_v2.rs`, `crates/engine/src/dsp/limiter_v2.rs`.
Measurements: `cargo run -p wave-engine --example reverb_v2_eval --release -- D:\mydaw-data\reverb_v2_samples`.

## Research summary

**Late reverb.** Every current high-end algorithmic reverb is built on a recirculating
network of delay lines; the feedback delay network (FDN) is the general, analysable form.
- Jot & Chaigne (AES 90th Conv., 1991): FDN with a unitary feedback matrix and per-line
  absorption filters `g = 10^(-3·L/T60(f))`; decay is then set per frequency, independent
  of the "colour" of the network.
- Schlecht & Habets, "Accurate reverberation time control in feedback delay networks"
  (DAFx 2017) and "Feedback delay networks: echo density and mixing time" (IEEE/ACM TASLP
  2020): dense unitary matrices (Hadamard, Householder) and more lines give faster echo
  build-up; mutually prime lengths avoid coinciding modes.
- Välimäki, Parker, Savioja, Smith & Abel, "Fifty years of artificial reverberation"
  (IEEE TASLP 2012): survey; time-varying (modulated) delays are the standard cure for
  metallic ringing in short-decay or small networks (as used in Lexicon/Valhalla designs).
- Dattorro, "Effect design part 1" (JAES 1997): the Griesinger/Lexicon plate: input allpass
  diffusers + modulated tank. We take its input diffusion and modulation, not the tank.
- Early reflections: velvet noise, sparse ±1 pulses one per segment (Järveläinen &
  Karjalainen, AES 2007; Välimäki, Holm-Rasmussen, Alary & Lehtonen, "Late reverberation
  synthesis using filtered velvet noise", Applied Sciences 2017). Cheap, smooth, and
  decorrelated by using different pulse patterns per channel.
- Product practice (manuals of FabFilter Pro-R 2, Valhalla VintageVerb/Room, LiquidSonics
  Seventh Heaven): few macro controls (Space/Decay, Brightness/Character, Distance or
  Pre-delay, Mix), per-band decay (Pro-R "decay rate EQ"), wet EQ, and ducking. Users value
  plates for vocals because the onset is dense with no discrete slap and the tail is
  bright-but-short; a common complaint about cheap reverbs is metallic flutter on short
  decays and a "washy" low end. (Inference from manuals and reviews; no listening tests run.)

**Choice for vocals.** 16-line FDN, orthonormal Hadamard mixing (fast WHT, 64 adds/sample),
incommensurate geometric line lengths, slow per-line delay modulation with cubic reads,
two first-order shelves per line for three-band RT60 (bass / mid / treble), 4 input
allpass diffusers per channel, velvet-noise early reflections (none for Plate), wet HPF/LPF,
stereo outputs from two orthogonal Hadamard rows, M/S width, and a dry-keyed ducker. The
macro set stays usable at every position: decay ≤ 6 s, treble decay always shorter than
mid, bass decay 0.5–1.5× mid, network size follows the decay, the wet is partly
normalised for decay, and the Mix law keeps the dry at unity up to 50 %.

**Limiter.** Audit of the old limiter (1 ms look-ahead, instant gain step, 60 ms release,
sample-peak only, hard clip, latency not reported to PDC) found real defects (table below):
inter-sample overs up to +4.9 dBTP over a −1 dB ceiling, 1.9 % THD on 50 Hz, an
uncompensated 1 ms delay. v2: true-peak detection per ITU-R BS.1770-4 Annex 2 (4× always,
8× near the ceiling, 384-tap Kaiser sinc), sliding-minimum hold + moving-average attack over
the look-ahead (gain ramp lands exactly on the peak; Giannoulis, Massberg & Reiss, JAES 2012
for the gain-computer structure), program-dependent release, stereo link, latency reported.

## Measurements (44.1 kHz; 96 kHz within ±0.05 s / ±0.03)

| reverb | NED ≥ 0.9 at | Wiener flatness | ring peak p99.9 / max | L/R corr | CPU µs / 128 fr |
|---|---|---|---|---|---|
| old (1.0 s) | 139 ms | 0.469 | 12.8 / 15.1 dB | 0.122 | 3.3 |
| new Vocal Plate | 52 ms | 0.488 | 11.7 / 15.2 dB | 0.002 | 33 |
| new Vocal Room | 54 ms | 0.542 | 10.2 / 11.0 dB | 0.046 | 45 |
| new Vocal Hall | 101 ms | 0.490 | 10.1 / 10.9 dB | −0.015 | 44 |
| white noise ref. | – | 0.563 | 10.1 / 11.7 dB | – | – |

RT60 per octave band (measured / designed): Plate 1.0 s within 10 %, Hall 2.0 s within 3 %,
Room 0.6 s within 9 %; Ambience 0.4 s within 19 % at 125–250 Hz (band filter ring-down
dominates such short decays). Old reverb default wet was ~12 dB hotter relative to the dry
than the new Vocal Plate, with a 2.2 s default decay.

| limiter (ceiling −1 dBTP) | old max TP | old overs | new max TP | new overs |
|---|---|---|---|---|
| fs/4 sine 45° | +2.10 | 168000 | −1.05 | 0 |
| synthetic drums +9 dB | +4.86 | 560 | −1.04 | 0 |
| 5 MUSDB mixes +8 dB | −0.20 (worst) | 488 | −1.05 (worst) | 0 |

THD at 6 dB GR: 50 Hz 1.86 % → 0.13 %, 100 Hz 0.63 % → 0.00 %. Loudness at equal gain:
within 0.1 dB RMS on the mixes. CPU 3 → 18 µs per 128 frames (true-peak detection).
