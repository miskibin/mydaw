//! Two properties every effect in the pack has to hold, checked across all five
//! at once so a sixth cannot quietly skip them.
//!
//! **Determinism.** The same input and the same parameters produce the same
//! samples, bit for bit. That is not an aesthetic preference: it is what makes
//! every spectral assertion in this crate a number rather than a tolerance of a
//! tolerance, and what will make a rendered project reproducible.
//!
//! **Block-size independence.** An effect must produce the same audio whether
//! the device asks for 480 frames at a time or 7. Anything that depends on the
//! block boundary -- a smoother that restarts, a coefficient recomputed per
//! block, a delay that resets -- shows up here and nowhere else.
//!
//! Plus the two claims the pack advertises in its own documentation: a fresh
//! effect is transparent, and a settled bypass is the input.

use std::sync::Arc;

use mydaw_plugin_effects::{EffectRegistry, BYPASS_PARAM};
use mydaw_rt_core::{AudioBuffers, ParamBlock, ProcessCtx, TransportSnapshot, DEFAULT_BPM};

const SR: f64 = 48_000.0;
const MAX_BLOCK: usize = 512;

/// A deterministic, broadband, stereo test signal.
///
/// Noise plus two tones: the noise excites everything, and the tones give the
/// filters and the shapers something with a stable spectrum to work on.
fn signal(frames: usize) -> Vec<[f32; 2]> {
    let mut seed = 0x2545_F491_u32;
    (0..frames)
        .map(|n| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let noise = (seed >> 8) as f32 * (2.0 / 16_777_215.0) - 1.0;
            let t = n as f64 / SR;
            let tone = (std::f64::consts::TAU * 220.0 * t).sin() as f32;
            let high = (std::f64::consts::TAU * 3_100.0 * t).sin() as f32;
            [
                0.3 * tone + 0.1 * noise,
                0.25 * high + 0.1 * noise + 0.1 * tone,
            ]
        })
        .collect()
}

/// Render `input` through a fresh node of `type_id`, in `block`-sized chunks.
///
/// `edit` is called before each chunk with the chunk index, so a test can move
/// parameters at a point in *time* rather than at a point in the buffer -- which
/// is what makes the block-size independence check meaningful.
fn render(
    registry: &EffectRegistry,
    type_id: &str,
    input: &[[f32; 2]],
    block: usize,
    setup: impl Fn(&ParamBlock),
    edit: impl Fn(&ParamBlock, usize),
) -> Vec<[f32; 2]> {
    let params = registry.default_params(type_id).expect("a block");
    // Set *before* the node is built, so it starts already at these settings
    // rather than ramping towards them -- which is also the property a graph
    // rebuild depends on.
    setup(&params);
    let mut node = registry
        .create(type_id, SR, MAX_BLOCK, Arc::clone(&params))
        .expect("a node");
    let mut buffers = AudioBuffers::new(MAX_BLOCK);
    let mut out = Vec::with_capacity(input.len());
    let mut position = 0_u64;

    for (index, chunk) in input.chunks(block).enumerate() {
        edit(&params, index);
        let frames = chunk.len();
        {
            buffers.set_frames(frames);
            let [left, right] = buffers.as_mut_slices();
            for (frame, sample) in chunk.iter().enumerate() {
                left[frame] = sample[0];
                right[frame] = sample[1];
            }
        }
        {
            let transport = TransportSnapshot {
                sample_pos: position,
                playing: true,
                bpm: DEFAULT_BPM,
            };
            let mut ctx = ProcessCtx::new(&mut buffers, SR, frames, transport);
            node.process(&mut ctx);
        }
        let [left, right] = buffers.as_slices();
        for (l, r) in left[..frames].iter().zip(&right[..frames]) {
            out.push([*l, *r]);
        }
        position += frames as u64;
    }
    out
}

/// Move every parameter to 70 % of its range.
fn seventy_percent(params: &ParamBlock) {
    for (index, def) in params.defs().iter().enumerate() {
        if def.id == BYPASS_PARAM {
            continue;
        }
        params.set(index, def.from_normalised(0.7));
    }
}

/// Change nothing while rendering.
fn no_edits(_: &ParamBlock, _: usize) {}

/// The saturator's reported latency at its default 4x, in samples.
///
/// Its output is *late*, not different, so a transparency comparison has to line
/// the two up. See `crate::oversample`.
const SATURATOR_LATENCY: usize = mydaw_plugin_effects::oversample::OUTER_HALF_LENGTH
    + mydaw_plugin_effects::oversample::INNER_HALF_LENGTH / 2;

#[test]
fn two_runs_of_the_same_effect_with_the_same_input_are_bit_identical() {
    let registry = EffectRegistry::new();
    let input = signal(24_000);
    for info in registry.effects() {
        let first = render(
            &registry,
            info.type_id,
            &input,
            128,
            seventy_percent,
            no_edits,
        );
        let second = render(
            &registry,
            info.type_id,
            &input,
            128,
            seventy_percent,
            no_edits,
        );
        assert_eq!(first, second, "{} is not deterministic", info.type_id);
        assert!(
            first.iter().all(|f| f[0].is_finite() && f[1].is_finite()),
            "{} produced a non-finite sample",
            info.type_id
        );
    }
}

#[test]
fn the_block_size_the_device_asks_for_does_not_change_the_audio() {
    // The parameters are set before the node exists, so nothing is ramping and
    // nothing is aimed once per block: what is left is the DSP, and it must not
    // care how the device chops the stream up.
    let registry = EffectRegistry::new();
    let input = signal(24_000);
    for info in registry.effects() {
        let reference = render(
            &registry,
            info.type_id,
            &input,
            480,
            seventy_percent,
            no_edits,
        );
        for block in [1_usize, 7, 64, 127, 512] {
            let other = render(
                &registry,
                info.type_id,
                &input,
                block,
                seventy_percent,
                no_edits,
            );
            assert_eq!(reference.len(), other.len());
            let worst = reference
                .iter()
                .zip(&other)
                .map(|(a, b)| (a[0] - b[0]).abs().max((a[1] - b[1]).abs()))
                .fold(0.0_f32, f32::max);
            assert!(
                worst < 1.0e-4,
                "{} differs by {worst} between block sizes 480 and {block}",
                info.type_id
            );
        }
    }
}

#[test]
fn every_effect_is_bit_transparent_at_its_defaults_or_documents_why_not() {
    // Four of the five are a genuine pass-through. The delay is the exception
    // and says so: its mix defaults to 30 %, because a delay whose mix started
    // at zero would be a delay that appears broken.
    let registry = EffectRegistry::new();
    // Quiet, at about -40 dBFS: the compressor's default threshold is -18 dB,
    // and "transparent at its defaults" can only mean "for a signal it is not
    // being asked to compress". A compressor that left a signal above its own
    // threshold alone would be broken, not transparent.
    let input: Vec<[f32; 2]> = signal(4_096)
        .into_iter()
        .map(|f| [f[0] * 0.03, f[1] * 0.03])
        .collect();
    for info in registry.effects() {
        let out = render(&registry, info.type_id, &input, 96, |_| {}, no_edits);
        if info.type_id == "effects.delay" {
            // Scaled dry and nothing else: the line is empty for the first
            // repeat's worth of samples, so anything the wet path contributes
            // here would be a bug. Compared to a tolerance rather than to the
            // bit because `30 % * 0.01` is not exactly `0.3` in `f32`.
            let worst = input
                .iter()
                .zip(&out)
                .map(|(a, b)| (a[0] * 0.7 - b[0]).abs().max((a[1] * 0.7 - b[1]).abs()))
                .fold(0.0_f32, f32::max);
            assert!(
                worst < 1.0e-6,
                "the delay's dry path is off by {worst}, or its line is not empty"
            );
        } else if info.type_id == "effects.saturator" {
            // Late, not different: the output has to be lined up with the input
            // by the oversampler's latency before it can be compared at all.
            // Then it is transparent to a tolerance rather than to the bit,
            // because the `tanh` curve bends even at unity drive.
            let worst = input
                .iter()
                .zip(out.iter().skip(SATURATOR_LATENCY))
                .map(|(a, b)| (a[0] - b[0]).abs().max((a[1] - b[1]).abs()))
                .fold(0.0_f32, f32::max);
            assert!(
                worst < 1.0e-3,
                "the saturator changed a quiet signal by {worst}"
            );
        } else {
            assert_eq!(out, input, "{} is not transparent when fresh", info.type_id);
        }
    }
}

#[test]
fn every_effect_is_bit_transparent_once_its_bypass_has_settled() {
    let registry = EffectRegistry::new();
    let input = signal(8_192);
    for info in registry.effects() {
        let out = render(
            &registry,
            info.type_id,
            &input,
            96,
            |_| {},
            |params, chunk| {
                if chunk == 0 {
                    // Something audible to bypass, and the bypass engaged from the
                    // very first block so the fade is well behind us by the end.
                    for (index, def) in params.defs().iter().enumerate() {
                        if def.id != BYPASS_PARAM {
                            params.set(index, def.from_normalised(0.8));
                        }
                    }
                    params.set_by_id(BYPASS_PARAM, 1.0);
                }
            },
        );
        // The 10 ms crossfade is 480 frames; everything after it is the input.
        assert_eq!(
            &out[2_048..],
            &input[2_048..],
            "{} is not bit-transparent when bypassed",
            info.type_id
        );
    }
}

#[test]
fn no_parameter_step_produces_a_discontinuity_the_ear_would_hear_as_a_click() {
    // The zipper test, across every continuous parameter of every effect: slam
    // it from one end of its range to the other mid-render and compare the worst
    // sample-to-sample jump against what the *settled* effect produces on its
    // own.
    //
    // Comparing against a fixed number would be wrong twice over. A gain
    // parameter legitimately changes the amplitude, and therefore the slope, of
    // the signal; and a saturator at full drive legitimately produces fast
    // edges, because that is what a clipped waveform is. What must not happen is
    // a jump the *destination setting* cannot itself explain.
    let registry = EffectRegistry::new();
    // A slow sine: its own step is 2*pi*110/48000 of its amplitude per sample,
    // so anything the effect adds stands out.
    let input: Vec<[f32; 2]> = (0..24_000)
        .map(|n| {
            let x = 0.5 * (std::f64::consts::TAU * 110.0 * n as f64 / SR).sin() as f32;
            [x, x]
        })
        .collect();

    let worst_step = |out: &[[f32; 2]]| {
        out.windows(2)
            .map(|pair| (pair[1][0] - pair[0][0]).abs())
            .fold(0.0_f32, f32::max)
    };

    for info in registry.effects() {
        for (target, def) in info.params.iter().enumerate() {
            if def.is_switch() {
                continue;
            }
            let steady = |value: f32| {
                render(
                    &registry,
                    info.type_id,
                    &input,
                    128,
                    |params| {
                        params.set(target, value);
                    },
                    no_edits,
                )
            };
            let slammed = render(
                &registry,
                info.type_id,
                &input,
                128,
                |params| {
                    params.set(target, def.min);
                },
                |params, chunk| {
                    // Halfway through: from one end of the range to the other.
                    let value = if chunk < 94 { def.min } else { def.max };
                    params.set(target, value);
                },
            );

            let settled = worst_step(&steady(def.min)).max(worst_step(&steady(def.max)));
            let moving = worst_step(&slammed);
            // Twice the settled slope, because a full-range ramp legitimately
            // adds its own: 72 dB of gain over 480 frames is 0.15 dB a sample,
            // which is 1.7 % of the amplitude, and at the top of the ramp that
            // lands on top of the signal's own slope. An *unsmoothed* slam would
            // be a step of the whole range in one sample -- tens of times this,
            // not one and a half.
            assert!(
                moving <= settled * 2.0 + 1.0e-4,
                "{}: slamming '{}' produced a jump of {moving}, against {settled}                  when it is sitting still at either end",
                info.type_id,
                def.id
            );
        }
    }
}
