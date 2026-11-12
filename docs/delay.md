# Delay

`delay` (`crates/engine/src/dsp/delay.rs`) is a stereo echo with a processing loop: a filter,
lo-fi, a drive stage and diffusion sit inside the feedback path, so every repeat is reshaped
again, as on tape and BBD units. Its control set follows the classic "everything in the loop"
design (Image-Line's Fruity Delay 3 is the reference musicians know). The DSP is our own.

```text
 in ─ Input ─┬─ model routing (+ offset pre-delay) ─ + ─ ring ─ read ─ loop chain ─┬─ Stereo ─ Tone ─ Wet ─┐
             │                                      └──── × Feedback ─────────────┘                       │
             └──────────────────────────────── Dry ───────────────────────────────────────────────── + ─ out
 loop chain: filter (LP/HP/BP, LFO on cutoff) → sample-rate hold → Limit / Sat → DC block → bits → diffusion
```

The chain sits between the read head and both the output and the feedback, so the first echo
is already shaped. With model **Off** there is no ring: the chain processes the input directly
and the effect is a filter / lo-fi / drive unit with no delay.

## Parameters

The window shows Time (with Sync), Feedback, Model, the loop filter's Cutoff, Wet and Dry; everything
else is under *Advanced*.


| id | range | |
|---|---|---|
| `input` | 0–100 % | level into the loop. Automate it to send only some words or hits into the echo; the tails already in the loop keep ringing |
| `sync` | Tempo / ms | **Tempo** (default): `beats` sets the time and follows the song tempo live, tempo automation included. **ms**: `time` sets it freely |
| `beats` | 1/48–4 beats | synced time. The knob snaps to note values (1/128T … 1/1, dotted and triplet), the wheel steps one value; it reads in beats ("1/2 beat", "1 1/2 beats") and the status line adds the note value and the ms at the current tempo |
| `time` | 1–2000 ms | free time |
| `keep` | Off / On | **Off** (Repitch): a new time is reached by gliding, which bends the echoes' pitch like tape; `smooth` sets the glide (0 % = jump, a glitch on purpose; 100 % ≈ 1.5 s). **On**: two read heads crossfade (40 ms) from the old time to the new one, so pitch never bends. The LFO on time still moves pitch |
| `offset` | −100–100 % | Mono model: multiplies one side's loop time (left when negative), so the sides drift apart with every repeat. Stereo model: a pre-delay of that share of the time on one side |
| `pan` | −100–100 % | Ping-pong only: balance on the input, so a centred mono source can start the bounce |
| `model` | Mono / Stereo / Ping-pong / Off | Mono sums L+R into both lines; Stereo keeps two independent loops; Ping-pong crosses the lines every pass (a left source: first echo right, second left …); Off: no delay |
| `spread` | 0–100 % | width of the wet signal (0 = mono) |
| `fb` | 0–125 % | feedback. Above 100 % the loop grows until the drive stage holds it: controlled self-oscillation |
| `ftype`, `cutoff`, `res` | Off/LP/HP/BP, 20 Hz–20 kHz, 0–100 % | loop filter (state-variable, stable under modulation). Resonance above ~75 % can ring the loop on its own even with feedback under 100 % |
| `srate`, `bits` | 500 Hz–48 kHz, 2–24 bit | sample-and-hold and quantiser in the loop; the top of each range is Off |
| `dist`, `level`, `knee`, `sym` | Limit/Sat, −36–0 dB, 0–100 %, 0–100 % | **Limit** clips at the threshold. **Sat** is linear up to the knee, then a tanh curve that never exceeds the threshold: soft knee ≈ valve, hard ≈ solid state. Symmetry lets the negative half run up to 4× higher before it bends (the positive half saturates first: even harmonics). A DC blocker after the stage keeps the asymmetry from piling up an offset |
| `diff`, `dspread` | 0–100 % | four all-passes blended into each repeat: smears the attacks towards a reverb-like cloud |
| `rate`, `mtime`, `mcut` | 0.05–20 Hz, 0–100 %, 0–100 % | sine LFO on the time (±6 ms at 100 %, at most 45 % of the time: wow on long echoes, chorus on short ones) and on the cutoff (±3 octaves) |
| `mix`, `tone`, `dry` | 0–100 %, −100–100 %, 0–100 % | wet level, a post-loop tone filter (left = low-pass, centre = off, right = high-pass), dry level. On a send, set Dry to 0 |

The first delay's ids `time`, `fb` and `mix` are kept, so its automation still applies.

## Presets

Synced echoes (1/8, dotted 1/8, 1/4, 1/8 triplet), three **ping-pong** presets right after the
first one (1/8, dotted 1/8, 1/4: the input is panned 70 % left, so a vocal or any mono source in
the centre bounces left–right too), slapback, a 100 % wet send, tape dub, dub self-oscillation,
lo-fi / bitcrush, an ambient smeared echo, chorus / doubling, and a no-echo filter + drive.

## Real-time behaviour

* No allocation after `prepare`. The ring holds 4 s plus modulation per channel (a synced time
  longer than 4 s at a very slow tempo is clamped).
* Continuous controls glide (20 ms; offset and diffusion spread 80 ms) so automation never
  zippers. The loop filter and the all-pass lengths are updated every 16 samples.
* The loop is bounded by construction: Sat never exceeds its threshold and Limit clips at it;
  every sample written into the loop is clamped to ±4 and a non-finite value becomes 0, so no
  setting can blow up or produce NaN.
* Changing the model dips the wet path for 10 ms and switches at the bottom (no click). The
  ring keeps its content.
* `tail_samples`: the repeats until −140 dB from the feedback × filter-resonance bound; when
  that bound is ≥ 1 the loop may sustain forever and the chain is never skipped.
* Zero latency.

## Old projects

A project saved with the first delay (only `time` 10–1500 ms, `fb` 0–95 %, `mix`) is migrated
on load (`dsp::delay::migrate_params`): free time with the same ms, the same feedback and
wet, no loop filter, a hard-knee Sat at 0 dB (linear below full scale), full dry — the same
echo. Automation of its `time` and `fb` is rescaled to the new ranges, so it lands on the same
values.

## Meters (`Processor::meters`)

`[0]`/`[1]` read heads L/R in ms (with the LFO), `[2]` loop level dBFS, `[3]` level the drive
stage takes off (dB ≤ 0), `[4]` LFO −1..1, `[5]` model in use, `[6]` wet output dBFS, `[7]`
the set time in ms at the current tempo.

## Tests

`dsp::delay::tests`: a synced echo lands on the beat and moves with the tempo; ms mode;
ping-pong alternates sides; stereo offset; 125 % feedback into a resonant band-pass for 20 s
of noise stays finite and within ±4; self-oscillation sustains above 100 % (and the chain is
never parked); keep pitch holds the frequency through a time change while repitch bends it;
model Off has no delay; bit reduction; the drive curve; the tail estimate; the old-project
migration (same echoes, automation remapped). UI: `ui/src/lib/delay.test.ts` (beat labels,
note names, snapping, tap diagram, loop gain).
