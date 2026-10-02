# Mixing guide for MYDAW agents

Read this with `mydaw help mixing`, even when the app is closed.

## Start with the sound and balance

1. Choose a representative busy 2–4 bar excerpt. Save a new project version.
2. Run `mydaw schema diagnose`, then diagnose the kick, bass and hats by stable
   track ID. Audition the returned WAVs. Inspect source, processed track, premaster
   and final mix separately. Do not infer a good mix from the final peak alone.
3. If the source already crunches, replace the distorted sample or change the
   instrument patch. A fader or limiter cannot restore the missing transient.
4. Establish the kick/bass relationship, then add clap/snare, hats, percussion
   and accompaniment. Use relative level, duration, timing and frequency content.
   Lower a dominant hat before reaching for compression or EQ. Listen in context.
5. Compare before/after at matched playback level. A louder version is easy to
   mistake for a better one. Export the whole song only after the excerpt works.

No universal kick/hat fader values exist: source level, velocity, processing and
hit density all matter. A hat with a short sharp transient can be distracting
without a large whole-excerpt RMS. Never normalize every track to the same peak.
Changing playback source can change headroom and note-off/release behavior.
Inspect the instrument's actual parameter metadata and render it; do not guess
parameter IDs. For drums, preserve one-shot decay unless a gated sound is wanted.
For a clean premix, several dB of peak headroom is a useful starting point, not a
genre standard. Do not raise a master limiter to repair an unbalanced mix.

## Read the diagnostic report

`source` isolates the played instrument/sample at unity track gain, centered,
with track inserts, sends and master processing removed. Note velocity, pitch,
sample region and the instrument's own patch remain. It is not the original file
at its recorded level. `track` preserves its fader, pan and inserts, through
existing buses/sends, with master inserts/plugin/fader removed. `premaster`
preserves the actual mix's routing with only master processing removed. `mix`
is the current final output. Dry isolation has no bus or sidechain context;
processed solo can also change sidechain and nonlinear behavior. Isolated tracks
do not sum to the mastered mix. Check the full mix after every decision.

The compact JSON provides sample peak, RMS, active-window RMS (50 ms windows
above -60 dBFS), active-window fraction, crest factor, loudest 50 ms window and
its time, samples at/above full scale, nonfinite values, flat-top candidates,
and approximate frequency-band energy shares. RMS includes silence; active RMS
helps compare sounds with different hit density. Neither is perceptual loudness.
Band shares aggregate up to 64 evenly spaced Hann-windowed FFTs per channel;
they are spectral evidence, not exact band loudness or a harshness score.
Zero energy reports null levels. A sustained near-constant peak is a clipping
candidate, also possible with square waves or deliberate distortion.

Float WAV samples above 0 dBFS indicate headroom risk, not proof of irreversible
internal clipping. Zero such samples does not exclude distortion already in a
sample, saturation in a plugin, or earlier clipping followed by attenuation.
True peak estimates reconstructed intersample peaks; sample peak does not.
This native report does not measure LUFS or true peak. Check those on the final
export with a standards-based loudness meter (or FFmpeg's ebur128/loudnorm), and
do not confuse a streaming normalization target with a mixing requirement.

## Efficient commands

Use `mydaw context` for IDs, then write `diagnose.json`:

```json
{"options":{"mode":"song","startBar":8,"bars":2,"tailSeconds":0.5},"trackIds":[4,7],"images":false}
```

Run `mydaw diagnose --file diagnose.json --timeout 180`. One unsaved snapshot
feeds all comparisons. The command creates audition WAVs and a report without
editing notes, faders, solo state, playback or the project revision. Select 1–4
tracks at a time, 1–8 bars; default 2 bars and a 0.5 s release tail. Images are
optional. Use `--no-wait`, `call job_status` and `call cancel_job` for long jobs.
Decoded banks are shared only between this job's stages (512 MiB limit), while
voices and effects are rebuilt. New jobs reload assets; there is no global stale
cache. External plugins still need fresh instances for each comparison.
Large binary audio and images stay in files; read the small report first.

For a source file alone:

```powershell
mydaw analyze --args '{"path":"D:/samples/kick.wav","images":true}'
```

Its `diagnostics` field uses
the same metrics. Compare a short source-file excerpt with the instrument render
to distinguish recorded distortion from playback processing. A spectrogram or
waveform helps answer a specific question; it does not substitute for audition.
If you cannot listen, state the measurement evidence and let the musician judge
the timbre. Do not claim to have listened.

## Sources and why they matter

- FL Studio, Levels, Mixing & Clipping: sampler defaults differ from audio clips;
  balance the channels before mastering, and a peak meter is not loudness.
  https://www.image-line.com/fl-studio-learning/fl-studio-online-manual/html/mixer_levelsandmixing.htm
- Ableton Live 12, Audio Fact Sheet: floating-point summing and recording retain
  headroom, so capture float audio for diagnosis instead of clipping to PCM.
  https://www.ableton.com/en/live-manual/12/audio-fact-sheet/
- iZotope, Gain staging: inspect levels between processing stages; a later
  volume change cannot undo distortion introduced upstream.
  https://www.izotope.com/community/blog/gain-staging-what-it-is-and-how-to-do-it
- iZotope, Clipping in mixing: deliberate clipping has a musical context;
  destructive distortion and excessive drive are not automatically desirable.
  https://www.izotope.com/community/blog/clipping-in-mixing
- Youlean, Measurements Explained: RMS, LUFS and true peak answer different
  questions; short-term and momentary readings reveal changes within a song.
  https://docs.youlean.co/youlean-loudness-meter/getting-started/measurements-explained
