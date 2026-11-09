# Auto vocal: research prototype

Goal: record a vocal over a beat, and have WAVE pick the vocal settings automatically
(pitch correction, EQ, de-ess, compression, gate, level against the beat, tempo-synced
delay/reverb, optional saturation) so the vocal fits that beat and that voice. Every
layer was measured on public, labelled data before the next one was built. This is a
Python prototype. No app code was changed: the app's own estimators were run unchanged
through a detached harness (`rust_music_bench/`) and scored next to the alternatives.

All numbers below come from `results/*.json`. The per-track caches sit in
`D:\mydaw-data\auto_vocal_cache`.

## Recommendation in one paragraph

Keep the app's built-in **key** estimator: it was the best of five on both key sets.
Replace or back up the built-in **tempo** with Beat This! (MIT), which was much better on
EDM and on GTZAN. On hip-hop, the built-in tempo already matches it.
For the vocal F0, **CREPE (MIT code and weights)** is the best permissive tracker on amateur
takes. RMVPE is slightly better and the only tracker that survives an accompaniment
bleeding into the take, but its weights are research-only. The app's YIN is behind on
amateur takes, fine on clean studio takes, and fails when the beat leaks in.
**Pitch correction** works in the paired test, with two conditions: the note grid must
come from the beat (blind tuning estimation fails), and a *scale gate* is needed because
rap and many amateur melodies do not stay on the beat's scale. Against a generic preset on the MUSDB paired test, the **mix chain** wins on vocal-to-beat level; loses on dynamic spread vs the pro stem; ties on tonal balance, sibilance, per-band vocal/beat balance (bootstrap 95 % CIs). The loss on dynamic spread is deliberate: the new dynamics make the vocal more even than the MUSDB stems, which contain backing vocals, after the owner heard the first version as uneven (see Dynamics). Both are far better than the untouched raw vocal. Learning the vocal EQ *from the beat* (ridge regression) gave **no**
measurable gain over one median professional target. Data-driven "carving" is not
supported by this evidence, so the chain uses boost-and-cut EQ at 50% strength toward the median pro tonal shape (63 Hz–10 kHz), with the rider + fast + slow dynamics below (EQ chosen on train songs).
Dynamics are a two-time-scale vocal rider, then a fast peak stage and a slow stage,
checked against seven levelling goals and the pro stems. Reverb level is set from the late energy measured in pro
vocal stems. Delay is off.
A lookahead true-peak limiter finishes every file. Style selection (layer 4) was not built. There is no public ground truth
to validate it, so a user toggle (natural / hard) is the honest product.

## Layer 1: beat tempo and key

| set (n) | metric | Krumhansl | Temperley | Albrecht | **WAVE built-in** | learned profile¹ |
| --- | --- | --- | --- | --- | --- | --- |
| GiantSteps Key (476²) | MIREX weighted | 0.617 | 0.458 | 0.546 | **0.767** | 0.725 |
| GTZAN key (837) | MIREX weighted | 0.565 | 0.507 | 0.561 | **0.674** | 0.620 |
| GTZAN hip-hop (81) | MIREX weighted | 0.312 | 0.254 | 0.232 | **0.437** | 0.377 |

| set (n) | metric | librosa | WAVE built-in | **Beat This!** |
| --- | --- | --- | --- | --- |
| GiantSteps Tempo v2 (664) | Acc1 / Acc2 | 0.41 / 0.47 | 0.65 / 0.86 | **0.83 / 0.91** |
| GTZAN tempo (998) | Acc1 / Acc2 | 0.66 / 0.84 | 0.72 / 0.91 | **0.82 / 0.93** |
| GTZAN hip-hop (100) | Acc1 / Acc2 | 0.81 / 0.88 | 0.95 / 0.97 | **0.95 / 0.98** |

¹ A transposition-equivariant logistic regression on mean chroma and bass chroma. It was
trained on the *other* dataset (GTZAN→GiantSteps and back), so it never saw the test data.
In-domain 2-fold cross-validation on GiantSteps gives 0.715. It does not generalise across
genres as well as the app's hand-tuned spectral-peak chroma.
² The Zenodo `keys_gs+` labels. 566 of 604 tracks have audio and an unambiguous
major/minor label. Labels written "X major | Y minor" are excluded.

* MIREX weighted score = 1 for the exact key, 0.5 for a fifth, 0.3 for the relative
  key, 0.2 for the parallel key (mir_eval). Acc1 = within 4 %, Acc2 = also allows
  ×2, ×3, ×½ and ×⅓.
* Limits: GiantSteps is EDM and GTZAN is 30 s excerpts of older pop. Key labels on hip-hop
  are weak: every method scores below 0.45 there, and many such beats are modal or have
  no tonal centre. That motivates the scale gate in layer 2. Beat This! is trained on
  public beat datasets, and its authors evaluate on GTZAN as a held-out set. Check
  their dataset list before quoting the GTZAN number externally.
* Licences: librosa ISC; Beat This! code MIT (`beat-this` 1.1.0). The checkpoint is
  downloaded from JKU; confirm its licence before shipping. GiantSteps/GTZAN audio:
  evaluation only.

## Layer 2: vocal pitch tracking and correction

### 2a. F0 tracking (mir_eval melody metrics, mean per track)

| tracker | Vocadito RPA / VFA / cents (n=40) | MIR-1K clean RPA / VFA / cents (n=40) | MIR-1K mix RPA / VFA / cents (n=100) | VocalSet RPA / VFA / cents (n=50) |
| --- | --- | --- | --- | --- |
| pYIN | 0.966 / 0.41 / 5.6 | 0.965 / 0.33 / 8.3 | 0.259 / 0.57 / 844.5 | 0.849 / 0.19 / 17.5 |
| CREPE-full | 0.984 / 0.37 / 7.8 | 0.968 / 0.27 / 10.9 | – | – |
| CREPE-tiny | 0.979 / 0.38 / 8.5 | 0.970 / 0.28 / 11.4 | 0.793 / 0.49 / 13.7 | 0.836 / 0.09 / 22.2 |
| WAVE YIN | 0.930 / 0.28 / 7.9 | 0.939 / 0.20 / 9.6 | 0.132 / 0.34 / 1197.8 | 0.872 / 0.05 / 11.5 |
| WAVE RMVPE (research-only weights) | 0.983 / 0.32 / 3.1 | 0.971 / 0.19 / 8.3 | 0.946 / 0.57 / 9.1 | 0.830 / 0.07 / 21.9 |

RPA = raw pitch accuracy (within 50 cents); VFA = voicing false alarm; OA = overall
accuracy; cents = median |error| on frames both call voiced. Vocadito (CC BY 4.0) has
amateur-ish solo singing. MIR-1K clean (40 clips, every 10th) has amateur karaoke vocals at 16 kHz.
"MIR-1K mix" is the same vocal with its accompaniment at 0 dB (100 clips). There, pYIN and
YIN lock onto the accompaniment (median errors of whole octaves). That matters only if
the beat leaks into the vocal mic, for example from speakers instead of headphones. Annotated VocalSet has
trained singers in 14 techniques. CREPE-full was left out of the last two sets to bound
CPU time; CREPE-tiny stands in. Licences: torchcrepe MIT (code and weights); pYIN ISC;
RMVPE weights research-only (already flagged in `crates/rmvpe`).

### 2b. Correction (`av/tune.py`)

The algorithm: CREPE-tiny F0 → note segmentation → a target grid from the beat's key and
tuning → a **natural** curve (each note's centre moves toward its target by a strength
that adapts to how far off the take is; vibrato and scoops kept; 40 ms ramps) or a
**hard** curve (every frame snaps with hysteresis, zero retune time) → TD-PSOLA. The
**scale gate** works like this: if less than 75 % of the sung time sits on the beat's
scale, it snaps chromatically instead, so it never rewrites a melody that leaves the scale.

Paired test, Annotated VocalSet, 26 takes / 993 notes of steady techniques.
A clean take is made first: each annotated note is moved onto the nearest A440 note
(oracle F0, whole-note offset, vibrato kept), so the intended pitch is known exactly.
Then every note is detuned by a random ±15–45 cents. The result is corrected blind and
measured with an independent tracker (pYIN, 4-cent bins) on the output audio:

| condition | note error, median cents | wrong note (>50 ct) | within 10 ct | vibrato kept (ratio) | mel-dB change |
| --- | --- | --- | --- | --- | --- |
| clean take (floor) | 14.0 | 0% | 39% | 1.00 | 0.00 |
| detuned input | 28.0 | 10% | 15% | 1.00 | 1.68 |
| natural, adaptive strength | 14.8 | 12% | 32% | 1.00 | 1.75 |
| natural, strength 1 | 12.9 | 11% | 44% | 1.01 | 1.81 |
| hard | 9.2 | 9% | 55% | 1.01 | 2.09 |
| hard, tuning estimated blind | 19.2 | 16% | 20% | 0.85 | 2.28 |
| natural on the in-tune clean take | 12.1 | 0% | 39% | 1.00 | 0.05 |

* The "clean" row is the **measurement floor**. Note medians include onsets and glides,
  so even an exactly tuned take measures about 14 cents. Hard mode measures below that
  floor because it also flattens the glides.
* The adaptive strength works as intended: median 0.64 on detuned takes, 0.002 on the
  clean takes (an in-tune singer is left alone). On vibrato takes, natural mode keeps
  the vibrato extent (ratio ≈ 1.0).
* **Blind tuning fails** (hard_selftune: 19 cents and 16 % wrong notes). Over a beat, the
  grid must come from the beat. That is what the product does: tuning comes from the
  beat's chroma.
* Ornamented techniques (trills, lip trills) are not helped. Those notes are not steady
  pitches. Both styles leave their errors at about the detuned level.
* The artefact proxy (40-band mel dB distance over voiced frames) mostly reflects the
  pitch change itself. At fixed shifts of 15/30/45/100 cents, PSOLA changes the spectrum
  *less* than librosa's phase vocoder (3.3 vs 3.5, 3.7 vs 3.9, 4.1 vs 4.5, 5.9 vs 6.3 dB; `results/layer2_artefacts.json`).
  This is not a listening test.

Real amateur takes over their backing (MIR-1K, 60 clips) follow the product path:
key from the accompaniment with the app's estimator, tuning from its chroma.
The scale gate tripped on 65% of takes (median 69% of sung time on the beat's scale), so those takes were snapped chromatically. The median note deviation from the grid in use went from 24 cents to 14 cents (natural, strength 0.68) and 6 cents (hard). Notes moved by more than 50 cents: 0% natural, 5% hard.
No "intended note" exists here, so these numbers are self-referential. They show that the
pitch moved onto the grid, not that the melody is right.

## Layer 3: mix fit (MUSDB18, professional mixes)

Descriptors (`av/mixfeat.py`) are: tonal *shape* (24 one-third-octave bands, mean
removed), dynamic *spread* (p95−p10 of 400 ms RMS), *sibilance* (p95 of the 5–10 kHz vs
0.1–5 kHz energy), and *vocal minus beat* loudness (BS.1770). The targets are medians over
the 100 MUSDB train songs: spread 12.0 dB, sibilance −6.0 dB, vocal 1.2 LU below the beat.

**Can the beat predict the pro vocal?** No, not beyond the median. On the 50 test songs,
a ridge model from the beat's spectrum gave a vocal-shape MAE of 4.32 dB, against 4.29 dB
for the median target (gain CI [−0.18, +0.13] dB). The loudness offset came out at 2.62
vs 2.72 LU (CI [−0.06, +0.24]). Different songs' pro vocals differ by 5.6 dB in shape,
far more than any beat signal explains with 100 songs.

**Paired restore test** (50 test songs). Each pro vocal stem is "un-mixed" by a random,
known degradation: tilt and resonance EQ, proximity boom, 1.6× dynamic expansion,
sibilance shelf, room noise and ±10 dB level. It is then processed blind. The error is
measured against the pro stem (median over songs; lower is better):

| chain | shape MAE dB | spread err dB | sibilance err dB | vocal−beat err LU | band vocal/beat MAE dB |
| --- | --- | --- | --- | --- | --- |
| untouched raw | 2.70 | 3.41 | 3.47 | 6.05 | 6.85 |
| generic preset (HPF, -2 dB mud, +3 dB presence, +2 dB air, 4:1, level) | 2.46 | 1.40 | 2.88 | 2.31 | 3.78 |
| auto, full matching EQ to median target | 2.96 | 3.06 | 2.74 | 2.28 | 3.82 |
| auto + beat-conditioned targets | 3.02 | 2.56 | 2.94 | 2.28 | 4.10 |
| **auto chain as shipped** (EQ from the train grid, rider + fast + slow dynamics) | 2.37 | 3.99 | 3.04 | 2.30 | 3.62 |
| another song's pro vocal (spread between pro mixes) | 5.57 | 5.80 | 6.47 | 3.20 | – |

Paired bootstrap, chosen auto chain vs generic preset (mean gain per song, 95 % CI): shape +0.21 [-0.10, 0.56]; sibilance +0.38 [-0.14, 0.92]; band ratio -0.01 [-0.36, 0.36]; spread -2.00 [-2.45, -1.53]; level +0.01 [0.00, 0.02]. Beat-conditioned vs median targets: band ratio -0.02 [-0.19, 0.15] dB. The configuration was picked on 40 *train*
songs from a 10-point grid (`results/layer3_chain_grid_train.json`) and reported once on
the test songs. Full matching EQ to a population target is *worse* than doing nothing:
it pulls every vocal toward an average that no individual pro mix has.

Raw amateur vocals (Vocadito, 40 phone/home takes), against the pro-vocal distribution: dynamic spread median 11.7 → 7.5 dB (percentile 46 → 17); sibilance -9.3 → -6.2 dB (percentile 29 → 45); tonal-shape distance to the pro median 9.3 → 6.6 dB.

What this can and cannot show: it shows that the chain moves measurable statistics
toward what engineers produced on these songs. The comparison with the fixed preset is in
the bootstrap line above. Excluding the EQ above 10 kHz (the AAC-coded MUSDB
stems bias the "air" target low) cost some score on sibilance against the first,
cut-only configuration, but it no longer dulls every vocal by 9 dB above 10 kHz. It does not show perceived quality. The degradation is synthetic, and one pro
mix per song is one valid answer. The EQ never boosts a band that the take does not
contain, meaning a band more than 45 dB under its loudest one, such as anything above
8 kHz in a 16 kHz recording. This guard was added after the first MIR-1K render.

**Dynamics: rider, fast and slow compression** (`av/mix.py`, `av/leveling.py`,
`av/dyn_calib.py`). The owner heard the first chain as uneven: loud and quiet words were
left, and the limiter was doing the levelling. The single 3:1 compressor is now replaced by
rider → fast peak stage (FET-like 4:1, ~4 dB on the loudest 1 % of 1 ms blocks) → slow
stage (opto-like 2.5:1, 10/250 ms, ~3 dB mean) → de-esser → (mix bus) true-peak limiter.

The rider follows the owner's research brief (Waves Vocal Rider, Sound Radix POWAIR,
WebRTC AGC2, DAFx 2022). On K-weighted level every 10 ms it keeps two time scales:
L_fast (210 ms sung / 120 ms rap) and L_phrase (2 s / 1 s, averaged over confident vocal
frames only). The gain is s · deadband(L_phrase − L_fast, ±0.6 dB) + μ · (T − L_phrase),
with T the take's median. That evens words toward their phrase and only nudges phrases, so
verse/chorus dynamics survive. Vocal confidence (SNR 6→14 dB smoothstep × activity) can
block a *boost* but never a *cut*, and idle frames glide back to 0 dB, which is the rule
against lifting breaths and noise. The range is +5 / −6 dB. The slew is asymmetric (80 ms
toward cut, 300 ms toward boost), run forwards and backwards and averaged, which gives an
offline look-ahead. Patent note: this is a local-vs-phrase error with confidence-limited
positive gain. It deliberately avoids the "activity detector → long-term level → target
gain → direction-dependent ramp" AGC structure claimed in US7995775B2. Keep it that way
when porting.

Calibration: s × μ grid on 40 real Vocadito takes plus 30 degraded MUSDB *train* songs. The pick meets the most goals (5/7 by median), then lands closest to the pro p25. Chosen: s = 0.5, μ = 0.2. The grid is in `results/dynamics_calibration.json`.

| rider-stage metric (goal) | MUSDB test, 50 degraded | Vocadito, 40 real takes |
| --- | --- | --- |
| m1: 400 ms loudness IQR −25…45 % | -20 % (P90−P10 -21 %) ✗ | -22 % (P90−P10 -22 %) ✗ |
| m2: word P90−P10 −2…5 dB | -2.2 dB (IQR -1.3) ✓ | -2.0 dB (IQR -1.1) ✓ |
| m3: phrase ρ > 0.9, R_macro 0.7–1.0 | ρ 0.99, R 0.86 ✓ | ρ 0.99, R 0.85 ✓ |
| m4: lift on non-vocal frames median < 0.25, P95 < 1 dB | +0.01 / +0.92 dB ✓ | +0.03 / +0.98 dB ✓ |
| m5: gain motion P95 < 12 dB/s | 9.0 dB/s ✓ | 9.7 dB/s ✓ |
| m6: downstream P95 GR −20 % or more | -12 % ✗ | -12 % ✗ |
| m7: consonant peak change < 1 dB | 0.80 dB ✓ | 0.54 dB ✓ |

| vocal (K-weighted, vocal regions of the raw take) | 400 ms IQR dB | 400 ms P90−P10 dB | word IQR dB | word P90−P10 dB |
| --- | --- | --- | --- | --- |
| MUSDB test: raw (degraded) | 5.56 | 11.61 | 4.68 | 8.89 |
| MUSDB test: after chain | 2.37 | 5.10 | 1.34 | 2.96 |
| MUSDB test: the songs' pro stems | 3.97 | 8.54 | 2.84 | 7.34 |
| Vocadito: raw | 4.34 | 9.09 | 3.22 | 6.64 |
| Vocadito: after chain | 2.37 | 5.07 | 1.08 | 2.24 |
| all 150 MUSDB pro stems, p25 / median | 3.03 / 3.86 | 6.81 / 8.50 | 1.57 / 2.93 | 3.41 / 7.18 |

Stage gain reduction on the test songs (medians): fast stage 4.8 dB on the loudest 1 % of blocks; slow stage 4.1 dB mean. The MUSDB pro stems are lead + backing + printed FX, so they are *less* even than a mixed lead vocal (harmonies and ad-libs add level swings). Their 25th percentile is the closer reference. The 'word' unit is an energy segment, not a transcribed word, and m6 freezes the stage thresholds from the no-rider run.

**Reverb, delay and limiter** (`av/fx_calib.py`, `av/limiter.py`). The first sample round
used fixed tempo rules (a −14 dB noise-tail reverb with T60 up to 2.5 s, plus a −16 dB
delay). The owner judged it far too wet, so the level is now taken from data. This
calibration was run with the first, single-compressor dynamics. The owner asked to keep
the reverb as it was after the dynamics rework, so `results/fx_calibration.json` is that
run. A rerun of `av.fx_calib` with the new dynamics points to −15 dB / T60 0.8 s, which is
wetter. That setting is not used.
The *tail ratio* is the energy 80–300 ms after a phrase end, relative to the last 100 ms of the phrase. It is counted only where the tail is still decaying, so a constant bed of backing vocals is excluded. Over 67 measurable MUSDB pro vocal stems it has a median of -15.7 dB (IQR -20.9 to -12.2). Dry takes (Vocadito and VocalSet) mostly have no decaying tail at all. After the chain, a short plate (pre-delay 30 ms, wet filtered 300 Hz–7 kHz) was added at sends from −33 to −12 dB with T60 0.8 and 1.2 s (median tail ratio per send at T60 1.2 s: -33 dB → -35.2, -30 dB → -32.3, -27 dB → -29.3, -24 dB → -26.4, -21 dB → -23.5, -18 dB → -20.5, -15 dB → -16.2, -12 dB → -13.1). A stem's tail holds *all* of its printed FX (delay throws, doubles, room), so the median overstates what a reverb alone contributes. The target is therefore the pro **25th percentile** (-20.9 dB), which gives **send -18 dB, T60 1.2 s**. That is about 1.6 % wet energy, and that setting is used. Matching the pro median instead would need -15 dB. Limits: only a few dry takes have long enough phrase gaps to measure (see `send_curve_measurable_share`), so the curve is coarse; a density-dependent rule (less reverb on a dense beat) was not validated. The **delay is off**: echo level in the pro stems was not measured, so no data-backed setting exists.
The limiter reads true peak on a 4× oversampled signal. It applies its gain over a 5 ms
lookahead with an 80 ms release, and its ceiling is −1 dBTP. Every listening file (before,
after, reference) goes through the same limiter at −14 LUFS, and the gain reduction is
listed per file in `index.txt`. On the 9 'after' files the maximum gain reduction was 1.3–7.4 dB. When active, the mean was 0.4–1.3 dB. Every file measured at most -1.0 dBTP. Saturation (hard style only) is a style choice and was not
validated.

## Layer 4: style selection

Not built. No public dataset labels which vocal treatment suits which beat, so any
"genre → preset" mapping would be unvalidated taste. The layer 1 data shows that
genre-level beat facts are already strongly informative for hip-hop tempo (Acc1
0.95), but not for key. The product should offer **natural / hard** as a user choice,
and could default to hard when the scale gate trips on a rap vocal.

## Listening samples

`D:\mydaw-data\auto_vocal_samples\` holds 9 pairs, each about 25 s: `_before`, `_after` and
(MUSDB) `_reference_pro_mix`. Each file is matched to −14 LUFS through the −1 dBTP
true-peak limiter. `index.txt` lists what the chain decided per sample: key, tempo, pitch
deviation before/after, scale gate, EQ moves, de-esser, rider range, gain reduction of
the fast and slow stages, evenness raw → after → pro, level, reverb and limiter gain
reduction. Seven MUSDB songs have a *simulated* raw vocal (the pro stem,
detuned and degraded). Two MIR-1K songs are *real* amateur karaoke takes (16 kHz
source). All are non-commercial research data: for internal listening only.

## Layout

| path | what |
| --- | --- |
| `av/layer1_run.py`, `av/layer1_score.py` | beat tempo and key: runs every method per track, then scores them |
| `rust_music_bench/` | runs the app's `wave_engine::music` (tempo/key) and `pitch::analyze_with` (YIN / RMVPE) on WAV lists. Detached workspace |
| `av/layer2_pitch.py` | F0 tracker benchmark |
| `av/tune.py` | the pitch corrector: segmentation, adaptive natural/hard curve, scale gate, TD-PSOLA |
| `av/layer2_tune_eval.py`, `av/layer2_score.py` | correction validation (VocalSet paired, MIR-1K real) |
| `av/mixfeat.py`, `av/mix.py` | descriptors, and the chain (gate, HPF, EQ, de-esser, compressor, saturation, level, FX) |
| `av/fx_calib.py`, `av/limiter.py` | the reverb send from the late energy of pro stems; the lookahead true-peak limiter |
| `av/musdb_fetch.py`, `av/layer3_tune.py`, `av/layer3_eval.py` | MUSDB18 excerpts over HTTP range requests, the train-split chain choice, the test evaluation |
| `av/make_samples.py` | listening pairs |
| `scripts/` | downloads, background launcher, diagnostics |

Reproduce: create `.venv` (Python 3.12, CPU torch) and install `numpy scipy librosa
soundfile mir_eval mirdata pyloudnorm scikit-learn onnxruntime torch torchcrepe beat-this
huggingface_hub remotezip ruff`. Next run the `scripts/download*` files and build the
harness (`cargo build --release` in `rust_music_bench`, `CARGO_TARGET_DIR=D:\mydaw-data\cargo_target2`).
Then run `python -m av.layer1_run <set>`, `av.layer1_score`, `scripts/run_layer2a*.ps1`,
`av.layer2_tune_eval vocalset|mir1k`, `av.layer2_score`, `av.layer3_eval`,
`av.layer3_tune`, `av.layer3_eval` again, `av.fx_calib` and `av.make_samples`. About 8 GB is
downloaded to `D:\mydaw-data`. All jobs are CPU-only and resumable.

## Porting into the app (next step after listening)

1. Tempo: add Beat This! as an optional ONNX model next to RMVPE, using the same "download
   on request" pattern. Keep the built-in estimator as the fallback.
2. Tune: the app already has `pitch::Psola`, `snap`, `scale_mask` and the RMVPE/YIN
   estimators. Port the note segmentation, the adaptive natural curve, hard-mode
   hysteresis and the 75 % scale gate. Add CREPE-tiny (MIT, about 2 MB ONNX) as the
   commercial-safe default tracker.
3. Mix: every stage maps onto the existing `Eq` (bands from the measured shape, nothing
   above 10 kHz). Dynamics need a new offline *rider* (a clip-gain curve from `rider_curve`, so nothing
   runs on the audio thread; keep the patent note above), then `Fet` (4:1 peaks), `Comp` (2.5:1 slow) and `DeEss`. Then `Sat`, `Reverb` (send
   from the calibration; a better reverb plugin is being built separately) and `Limiter`
   (it needs true-peak lookahead).
   The analysis is offline over the recorded take and the beat, so nothing new runs on
   the audio thread. Store the median pro targets as constants.
