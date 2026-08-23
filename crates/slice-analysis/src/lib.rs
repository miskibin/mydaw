//! Where to cut a break, a loop or a vocal take into playable pieces.
//!
//! Two ways to answer, because the two questions musicians actually ask are
//! different. *"Where are the hits?"* is [`SliceMode::Transient`]: a
//! spectral-flux onset detector with an adaptive threshold, which is what FL
//! Studio's Fruity Slicer and Ableton's Simpler run when you slice by
//! transient. *"Where are the sixteenths?"* is [`SliceMode::Grid`]: the loop
//! divided into equal pieces, which is the honest answer for a quantised loop
//! whose hits are buried under a pad.
//!
//! Both return **absolute frame positions in the file**, ascending, starting at
//! the analysed range's first frame. A slice runs from its own position to the
//! next one, and the last slice runs to the end of the range -- so `n` returned
//! positions are `n` playable slices, never `n - 1`.
//!
//! Offline and bounded: this runs on a blocking task, never on the audio
//! thread, and every loop here is linear in the analysed range.
//!
//! # The transient detector
//!
//! 1. Downmix the range to mono. No resampling: the hop is what sets the time
//!    resolution and the refinement step below works on the original samples.
//! 2. STFT with a 1024-sample Hann window and a 256-sample hop. At 48 kHz that
//!    is a 21 ms window every 5.3 ms -- long enough for a kick's fundamental to
//!    be resolved, short enough to separate a 32nd-note hat roll at 174 BPM.
//! 3. Spectral flux: the sum of *positive* magnitude changes per bin, which
//!    reacts to a new sound arriving and ignores one decaying away. Normalised
//!    by its own maximum, so the threshold below is a fraction of the loudest
//!    change in this file rather than an absolute number that depends on how
//!    hot the file was rendered.
//! 4. Adaptive threshold (Dixon 2006): `delta + lambda * median(window)` over a
//!    +/-8 hop window. A median rather than a mean so one enormous hit does not
//!    raise the bar for the eight quiet frames around it. `sensitivity` moves
//!    both terms, and only downwards -- see [`Sensitivity`].
//! 5. Peak-pick: a local maximum over +/-2 hops that clears the threshold and
//!    is at least [`MIN_GAP_MS`] after the previous accepted onset.
//! 6. Refine to a sample. A hop is 5.3 ms and a slice that starts 5 ms late has
//!    audibly lost its attack, so each accepted hop is walked back to the last
//!    near-silent sample before the local peak. On a clean transient this lands
//!    within a sample or two of the true onset.

use std::ops::Range;

use realfft::RealFftPlanner;

/// STFT window, in samples. See the module note on why 1024/256.
pub const FFT_SIZE: usize = 1_024;
/// STFT hop, in samples.
pub const HOP: usize = 256;
/// Nothing may be sliced finer than this. A slice shorter than a click is not
/// a slice, and two onsets 10 ms apart are one hit that flams.
pub const MIN_GAP_MS: f64 = 30.0;
/// How far either side of a hop the adaptive threshold looks, in hops.
const MEDIAN_HALF_WIDTH: usize = 8;
/// How far either side a peak has to dominate, in hops.
const PEAK_HALF_WIDTH: usize = 2;
/// Below this the range is silence, and silence has no onsets.
const SILENCE_PEAK: f32 = 1.0e-4;
/// Fraction of the local peak that counts as "the attack has started".
const ATTACK_FLOOR: f32 = 0.08;
/// Grid divisions the UI offers: quarters, eighths, sixteenths.
pub const GRID_DIVISIONS: [u32; 3] = [4, 8, 16];

/// How eagerly [`detect_onsets`] cuts, `0.0` (only the obvious hits) to `1.0`
/// (every ghost note it can find).
///
/// Both threshold terms fall as this rises and nothing else changes, so a
/// higher sensitivity can only ever *add* candidate onsets. The one way a
/// count can go down is the minimum-gap rule: an extra onset accepted early
/// can suppress one just behind it. That is a property of any greedy gap
/// filter, and it is why the UI shows the resulting count rather than
/// promising a monotone slider.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sensitivity(f32);

impl Sensitivity {
    pub const DEFAULT: Self = Self(0.5);

    /// Clamped to `0.0..=1.0`; a non-finite value becomes the default.
    pub fn new(value: f32) -> Self {
        if value.is_finite() {
            Self(value.clamp(0.0, 1.0))
        } else {
            Self::DEFAULT
        }
    }

    pub fn get(self) -> f32 {
        self.0
    }

    /// `lambda`: how far above the local median a peak has to stand.
    fn lambda(self) -> f32 {
        lerp(2.5, 1.02, self.0)
    }

    /// `delta`: the absolute floor, as a fraction of the loudest flux here.
    /// Without it a file that is one hit and a lot of room tone would slice
    /// the room tone, because the median of near-nothing is near-nothing.
    fn delta(self) -> f32 {
        lerp(0.14, 0.006, self.0)
    }
}

impl Default for Sensitivity {
    fn default() -> Self {
        Self::DEFAULT
    }
}

fn lerp(from: f32, to: f32, t: f32) -> f32 {
    from + (to - from) * t
}

/// How a slice table was asked for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SliceMode {
    /// Cut where a new sound arrives.
    Transient(Sensitivity),
    /// Cut the loop into `1 / division` of a bar. `division` is one of
    /// [`GRID_DIVISIONS`]; anything else is clamped to the nearest.
    Grid { division: u32 },
}

/// What a slicing run found, and what it assumed to find it.
#[derive(Debug, Clone, PartialEq)]
pub struct SliceAnalysis {
    /// Absolute frame positions in the file, ascending, first == range start.
    pub frames: Vec<u64>,
    /// Bars the analysed range was taken to be. Grid mode only; `0` otherwise.
    ///
    /// Reported so the window can say *"2 bars @ 130"* instead of leaving the
    /// user to work out why sixteenths came out 32 slices long.
    pub bars: u32,
    /// Whether the request had to be cut short at `max_slices`.
    pub truncated: bool,
}

/// Slice positions for `mode` over `range` of an interleaved stereo buffer.
///
/// `stereo` is interleaved L/R and `frames` is its length in frames; `range` is
/// in frames and is clamped to what is actually there. `bpm` is the project
/// tempo, used by [`SliceMode::Grid`] to guess how many bars the range is.
/// `max_slices` bounds the answer -- the engine's slice table is fixed size and
/// a two-minute break at high sensitivity would otherwise overflow it.
///
/// Always returns at least one position (the range start) for a non-empty
/// range, so the caller never has to special-case "no slices".
pub fn analyze(
    stereo: &[f32],
    frames: usize,
    sample_rate: f64,
    range: Range<usize>,
    bpm: f64,
    mode: SliceMode,
    max_slices: usize,
) -> SliceAnalysis {
    let available = frames.min(stereo.len() / 2);
    let start = range.start.min(available);
    let end = range.end.min(available).max(start);
    let max_slices = max_slices.max(1);
    if end <= start || !sample_rate.is_finite() || sample_rate <= 0.0 {
        return SliceAnalysis {
            frames: vec![start as u64],
            bars: 0,
            truncated: false,
        };
    }
    match mode {
        SliceMode::Transient(sensitivity) => {
            let mut frames = detect_onsets(stereo, start..end, sample_rate, sensitivity);
            let truncated = frames.len() > max_slices;
            frames.truncate(max_slices);
            SliceAnalysis {
                frames,
                bars: 0,
                truncated,
            }
        }
        SliceMode::Grid { division } => {
            let division = nearest_division(division);
            let bars = assumed_bars(end - start, sample_rate, bpm);
            let wanted = (bars as usize).saturating_mul(division as usize).max(1);
            let count = wanted.min(max_slices);
            let span = (end - start) as f64;
            let frames = (0..count)
                .map(|index| start as u64 + (span * index as f64 / wanted as f64).round() as u64)
                .collect();
            SliceAnalysis {
                frames,
                bars,
                truncated: wanted > count,
            }
        }
    }
}

/// The division in [`GRID_DIVISIONS`] closest to `division`.
pub fn nearest_division(division: u32) -> u32 {
    *GRID_DIVISIONS
        .iter()
        .min_by_key(|candidate| candidate.abs_diff(division))
        .expect("GRID_DIVISIONS is not empty")
}

/// How many bars a range of `span` frames is, at `bpm` in 4/4.
///
/// Rounded to a whole bar and floored at one: a loop is a whole number of bars
/// or it is not a loop, and a one-shot that happens to be 0.7 bars long still
/// has to divide into something. Reported to the user rather than hidden, so a
/// wrong guess is visible and the division picker can correct for it.
pub fn assumed_bars(span_frames: usize, sample_rate: f64, bpm: f64) -> u32 {
    if !bpm.is_finite() || bpm <= 0.0 || !sample_rate.is_finite() || sample_rate <= 0.0 {
        return 1;
    }
    let bar_frames = sample_rate * 4.0 * 60.0 / bpm;
    if bar_frames <= 0.0 {
        return 1;
    }
    let bars = (span_frames as f64 / bar_frames).round();
    bars.clamp(1.0, 64.0) as u32
}

/// Onset positions in `range`, in absolute frames, starting with `range.start`.
pub fn detect_onsets(
    stereo: &[f32],
    range: Range<usize>,
    sample_rate: f64,
    sensitivity: Sensitivity,
) -> Vec<u64> {
    let start = range.start;
    let mono = downmix(stereo, range);
    let mut out = vec![start as u64];
    if mono.len() < FFT_SIZE {
        return out;
    }
    let peak = mono.iter().fold(0.0_f32, |acc, s| acc.max(s.abs()));
    if peak < SILENCE_PEAK {
        return out;
    }

    let flux = spectral_flux(&mono);
    if flux.is_empty() {
        return out;
    }
    let loudest = flux.iter().fold(0.0_f32, |acc, value| acc.max(*value));
    if loudest <= 0.0 {
        return out;
    }
    let flux: Vec<f32> = flux.iter().map(|value| value / loudest).collect();

    let min_gap = (MIN_GAP_MS / 1_000.0 * sample_rate).round().max(1.0) as usize;
    let lambda = sensitivity.lambda();
    let delta = sensitivity.delta();
    let mut window: Vec<f32> = Vec::with_capacity(MEDIAN_HALF_WIDTH * 2 + 1);
    let mut last = 0_usize;
    for index in 1..flux.len() {
        let value = flux[index];
        if value <= 0.0 {
            continue;
        }
        let low = index.saturating_sub(PEAK_HALF_WIDTH);
        let high = (index + PEAK_HALF_WIDTH + 1).min(flux.len());
        if flux[low..high].iter().any(|other| *other > value) {
            continue;
        }
        let low = index.saturating_sub(MEDIAN_HALF_WIDTH);
        let high = (index + MEDIAN_HALF_WIDTH + 1).min(flux.len());
        window.clear();
        window.extend_from_slice(&flux[low..high]);
        window.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median = window[window.len() / 2];
        if value < delta + lambda * median {
            continue;
        }
        let onset = refine(&mono, index);
        if onset <= last || onset - last < min_gap {
            continue;
        }
        last = onset;
        out.push((start + onset) as u64);
    }
    out
}

/// Interleaved stereo `range` folded to mono. **Non-RT.**
fn downmix(stereo: &[f32], range: Range<usize>) -> Vec<f32> {
    let start = range.start;
    let end = range.end.min(stereo.len() / 2).max(start);
    (start..end)
        .map(|frame| 0.5 * (stereo[frame * 2] + stereo[frame * 2 + 1]))
        .collect()
}

/// Half-wave-rectified spectral flux, one value per hop.
fn spectral_flux(mono: &[f32]) -> Vec<f32> {
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);
    let mut input = fft.make_input_vec();
    let mut spectrum = fft.make_output_vec();
    let window: Vec<f32> = (0..FFT_SIZE)
        .map(|index| 0.5 - 0.5 * (std::f32::consts::TAU * index as f32 / FFT_SIZE as f32).cos())
        .collect();
    let bins = spectrum.len();
    let mut previous = vec![0.0_f32; bins];
    let mut magnitude = vec![0.0_f32; bins];
    let hops = if mono.len() < FFT_SIZE {
        0
    } else {
        (mono.len() - FFT_SIZE) / HOP + 1
    };
    let mut flux = Vec::with_capacity(hops);
    for hop in 0..hops {
        let offset = hop * HOP;
        for (index, slot) in input.iter_mut().enumerate() {
            *slot = mono[offset + index] * window[index];
        }
        if fft.process(&mut input, &mut spectrum).is_err() {
            break;
        }
        let mut sum = 0.0_f32;
        for (index, bin) in spectrum.iter().enumerate() {
            let value = bin.norm();
            magnitude[index] = value;
            let rise = value - previous[index];
            if rise > 0.0 {
                sum += rise;
            }
        }
        previous.copy_from_slice(&magnitude);
        // The very first hop has nothing to be a change from; counting it
        // would make every file's loudest flux its own first window.
        flux.push(if hop == 0 { 0.0 } else { sum });
    }
    flux
}

/// Walk hop `index` back to the sample the attack actually starts on.
///
/// A rise detected at hop `i` happened somewhere in `[i * HOP, i * HOP + FFT)`
/// -- that is the first analysis window containing it. Inside that span, find
/// the loudest sample and then the last sample before it that is still under
/// [`ATTACK_FLOOR`] of that peak. One hop of slack is searched behind, because
/// the flux maximum is often the hop after the one where the attack first
/// entered the window.
fn refine(mono: &[f32], index: usize) -> usize {
    let centre = index * HOP;
    let from = centre.saturating_sub(HOP);
    let to = (centre + FFT_SIZE).min(mono.len());
    if from >= to {
        return centre.min(mono.len().saturating_sub(1));
    }
    let span = &mono[from..to];
    let mut peak = 0.0_f32;
    let mut peak_at = 0_usize;
    for (offset, sample) in span.iter().enumerate() {
        let level = sample.abs();
        if level > peak {
            peak = level;
            peak_at = offset;
        }
    }
    if peak <= 0.0 {
        return centre;
    }
    let floor = peak * ATTACK_FLOOR;
    let mut onset = peak_at;
    while onset > 0 && span[onset - 1].abs() >= floor {
        onset -= 1;
    }
    from + onset
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 48_000.0;

    /// Interleaved stereo silence with a short decaying click at each position.
    fn clicks(frames: usize, at: &[(usize, f32)]) -> Vec<f32> {
        let mut data = vec![0.0_f32; frames * 2];
        for (position, amplitude) in at {
            for offset in 0..64 {
                let frame = position + offset;
                if frame >= frames {
                    break;
                }
                // A decaying burst of alternating sign: broadband, so every
                // bin of the spectrum moves, which is what flux measures.
                let value = amplitude
                    * (1.0 - offset as f32 / 64.0)
                    * if offset % 2 == 0 { 1.0 } else { -1.0 };
                data[frame * 2] = value;
                data[frame * 2 + 1] = value;
            }
        }
        data
    }

    #[test]
    fn a_click_train_is_found_within_two_milliseconds() {
        let positions = [0_usize, 12_000, 24_000, 36_000, 48_000, 60_000];
        let frames = 72_000;
        let data = clicks(
            frames,
            &positions
                .iter()
                .map(|position| (*position, 0.8))
                .collect::<Vec<_>>(),
        );
        let found = detect_onsets(&data, 0..frames, RATE, Sensitivity::DEFAULT);
        assert_eq!(
            found.len(),
            positions.len(),
            "expected one slice per click, got {found:?}"
        );
        let tolerance = (0.002 * RATE) as i64; // 2 ms
        for (expected, actual) in positions.iter().zip(&found) {
            let error = (*actual as i64 - *expected as i64).abs();
            assert!(
                error <= tolerance,
                "click at {expected} reported at {actual}: {error} frames off, \
                 more than the {tolerance} frame budget"
            );
        }
    }

    #[test]
    fn a_higher_sensitivity_never_finds_fewer_hits() {
        // Six clicks whose amplitudes span 30 dB: a low sensitivity should
        // only take the loud ones, a high one should reach the ghost notes.
        let frames = 96_000;
        let data = clicks(
            frames,
            &[
                (0, 0.9),
                (12_000, 0.05),
                (24_000, 0.7),
                (36_000, 0.03),
                (48_000, 0.8),
                (60_000, 0.04),
                (72_000, 0.6),
                (84_000, 0.03),
            ],
        );
        let counts: Vec<usize> = (0..=10)
            .map(|step| {
                detect_onsets(&data, 0..frames, RATE, Sensitivity::new(step as f32 / 10.0)).len()
            })
            .collect();
        for pair in counts.windows(2) {
            assert!(
                pair[1] >= pair[0],
                "slice count fell as sensitivity rose: {counts:?}"
            );
        }
        assert!(
            counts[0] < counts[10],
            "the slider has to do something: {counts:?}"
        );
        assert!(
            *counts.last().expect("counts") <= 9,
            "silence between clicks must not become slices: {counts:?}"
        );
    }

    #[test]
    fn silence_is_one_slice_and_never_a_divide_by_zero() {
        let data = vec![0.0_f32; 48_000 * 2];
        let found = detect_onsets(&data, 0..48_000, RATE, Sensitivity::new(1.0));
        assert_eq!(found, vec![0]);

        let empty = analyze(
            &[],
            0,
            RATE,
            0..0,
            130.0,
            SliceMode::Grid { division: 16 },
            128,
        );
        assert_eq!(empty.frames, vec![0]);
    }

    #[test]
    fn grid_mode_lands_on_exact_divisions_of_the_assumed_bar() {
        // Two seconds at 120 BPM is exactly one 4/4 bar.
        let frames = 96_000;
        let data = clicks(frames, &[(0, 0.5)]);
        let analysis = analyze(
            &data,
            frames,
            RATE,
            0..frames,
            120.0,
            SliceMode::Grid { division: 16 },
            128,
        );
        assert_eq!(analysis.bars, 1);
        assert_eq!(analysis.frames.len(), 16);
        for (index, frame) in analysis.frames.iter().enumerate() {
            assert_eq!(*frame, index as u64 * 6_000, "sixteenth {index}");
        }

        // Four seconds at the same tempo is two bars, so eighths are sixteen
        // slices and not eight -- the bar is a bar, not the whole file.
        let long = clicks(192_000, &[(0, 0.5)]);
        let two_bars = analyze(
            &long,
            192_000,
            RATE,
            0..192_000,
            120.0,
            SliceMode::Grid { division: 8 },
            128,
        );
        assert_eq!(two_bars.bars, 2);
        assert_eq!(two_bars.frames.len(), 16);
        assert_eq!(two_bars.frames[1], 12_000);
    }

    #[test]
    fn grid_mode_slices_the_selected_region_and_not_the_file() {
        let frames = 192_000;
        let data = clicks(frames, &[(0, 0.5)]);
        let analysis = analyze(
            &data,
            frames,
            RATE,
            96_000..192_000,
            120.0,
            SliceMode::Grid { division: 4 },
            128,
        );
        assert_eq!(analysis.bars, 1);
        assert_eq!(analysis.frames, vec![96_000, 120_000, 144_000, 168_000]);
    }

    #[test]
    fn the_answer_is_bounded_and_says_when_it_was_cut_short() {
        let frames = 192_000;
        let data = clicks(frames, &[(0, 0.5)]);
        let analysis = analyze(
            &data,
            frames,
            RATE,
            0..frames,
            120.0,
            SliceMode::Grid { division: 16 },
            8,
        );
        assert_eq!(analysis.frames.len(), 8);
        assert!(analysis.truncated, "a cut-short answer has to say so");
    }

    #[test]
    fn an_odd_division_is_pulled_to_one_the_engine_offers() {
        assert_eq!(nearest_division(1), 4);
        assert_eq!(nearest_division(7), 8);
        assert_eq!(nearest_division(99), 16);
    }

    #[test]
    fn a_range_past_the_end_of_the_buffer_is_clamped_rather_than_panicking() {
        let frames = 4_000;
        let data = clicks(frames, &[(0, 0.5)]);
        let analysis = analyze(
            &data,
            frames,
            RATE,
            2_000..1_000_000,
            120.0,
            SliceMode::Transient(Sensitivity::DEFAULT),
            128,
        );
        assert_eq!(analysis.frames.first().copied(), Some(2_000));
        assert!(analysis.frames.iter().all(|frame| *frame < frames as u64));
    }
}
