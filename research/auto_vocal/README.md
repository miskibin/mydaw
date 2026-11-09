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
rap and many amateur melodies do not stay on the beat's scale. The **mix chain** beats
a generic preset on tonal balance (bootstrap CI > 0). It ties on sibilance, per-band
vocal-to-beat balance, dynamics and level (CIs include 0), and both are far better than
the untouched raw vocal. Learning the vocal EQ *from the beat* (ridge regression) gave **no**
measurable gain over one median professional target. Data-driven "carving" is not
supported by this evidence, so the chain uses boost-and-cut EQ at 50% strength toward the median pro tonal shape (63 Hz–10 kHz), with a fixed 3:1 compressor at the median level (chosen on train songs).
Reverb level is set from the late energy measured in pro vocal stems. Delay is off.
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
| auto, full matching EQ to median target | 2.96 | 3.06 | 2.74 | 2.29 | 3.75 |
| auto + beat-conditioned targets | 2.92 | 2.56 | 2.94 | 2.28 | 4.09 |
| **auto, configuration chosen on train split** | 2.36 | 1.34 | 3.16 | 2.30 | 3.51 |
| another song's pro vocal (spread between pro mixes) | 5.57 | 5.80 | 6.47 | 3.20 | – |

Paired bootstrap, chosen auto chain vs generic preset (mean gain per song, 95 % CI): shape +0.32 [0.04, 0.61]; sibilance +0.31 [-0.23, 0.86]; band ratio +0.20 [-0.11, 0.51]; spread +0.02 [-0.39, 0.41]; level +0.01 [-0.00, 0.02]. Beat-conditioned vs median targets: band ratio -0.02 [-0.19, 0.15] dB. The configuration was picked on 40 *train*
songs from a 10-point grid (`results/layer3_chain_grid_train.json`) and reported once on
the test songs. Full matching EQ to a population target is *worse* than doing nothing:
it pulls every vocal toward an average that no individual pro mix has.

Raw amateur vocals (Vocadito, 40 phone/home takes), against the pro-vocal distribution: dynamic spread median 11.7 → 9.2 dB (percentile 46 → 26); sibilance -9.3 → -6.6 dB (percentile 29 → 42); tonal-shape distance to the pro median 9.3 → 6.4 dB.

What this can and cannot show: it shows that the chain moves measurable statistics
toward what engineers produced on these songs, and that it beats a fixed preset on
tonal balance (it ties on the rest). Excluding the EQ above 10 kHz (the AAC-coded MUSDB
stems bias the "air" target low) cost some score on sibilance against the first,
cut-only configuration, but it no longer dulls every vocal by 9 dB above 10 kHz. It does not show perceived quality. The degradation is synthetic, and one pro
mix per song is one valid answer. The EQ never boosts a band that the take does not
contain, meaning a band more than 45 dB under its loudest one, such as anything above
8 kHz in a 16 kHz recording. This guard was added after the first MIR-1K render.

**Reverb, delay and limiter** (`av/fx_calib.py`, `av/limiter.py`). The first sample round
used fixed tempo rules (a −14 dB noise-tail reverb with T60 up to 2.5 s, plus a −16 dB
delay). The owner judged it far too wet, so the level is now taken from data.
The *tail ratio* is the energy 80–300 ms after a phrase end, relative to the last 100 ms of the phrase. It is counted only where the tail is still decaying, so a constant bed of backing vocals is excluded. Over 67 measurable MUSDB pro vocal stems it has a median of -15.7 dB (IQR -20.9 to -12.2). Dry takes (Vocadito and VocalSet) mostly have no decaying tail at all. After the chain, a short plate (pre-delay 30 ms, wet filtered 300 Hz–7 kHz) was added at sends from −33 to −12 dB with T60 0.8 and 1.2 s (median tail ratio per send at T60 1.2 s: -33 dB → -35.2, -30 dB → -32.3, -27 dB → -29.3, -24 dB → -26.4, -21 dB → -23.5, -18 dB → -20.5, -15 dB → -16.2, -12 dB → -13.1). A stem's tail holds *all* of its printed FX (delay throws, doubles, room), so the median overstates what a reverb alone contributes. The target is therefore the pro **25th percentile** (-20.9 dB), which gives **send -18 dB, T60 1.2 s**. That is about 1.6 % wet energy, and that setting is used. Matching the pro median instead would need -15 dB. Limits: only a few dry takes have long enough phrase gaps to measure (see `send_curve_measurable_share`), so the curve is coarse; a density-dependent rule (less reverb on a dense beat) was not validated. The **delay is off**: echo level in the pro stems was not measured, so no data-backed setting exists.
The limiter reads true peak on a 4× oversampled signal. It applies its gain over a 5 ms
lookahead with an 80 ms release, and its ceiling is −1 dBTP. Every listening file (before,
after, reference) goes through the same limiter at −14 LUFS, and the gain reduction is
listed per file in `index.txt`. On the 9 'after' files the maximum gain reduction was 2.3–7.5 dB. When active, the mean was 0.4–1.3 dB. Every file measured at most -1.0 dBTP. Saturation (hard style only) is a style choice and was not
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
deviation before/after, scale gate, EQ moves, de-esser, compressor, level, reverb and
limiter gain reduction. Seven MUSDB songs have a *simulated* raw vocal (the pro stem,
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
   above 10 kHz), `DeEss`, `Comp` (3:1 at the median 400 ms level), `Sat`, `Reverb` (send
   from the calibration; a better reverb plugin is being built separately) and `Limiter`
   (it needs true-peak lookahead).
   The analysis is offline over the recorded take and the beat, so nothing new runs on
   the audio thread. Store the median pro targets as constants.
