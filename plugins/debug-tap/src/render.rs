//! The picture: waveform on top, log-frequency spectrum underneath.
//!
//! ```text
//!  +---------------------------------------------------------------+
//!  | WAVEFORM DBFS  L=BLUE R=ORANGE  <frames> FRAMES <s> <rate>HZ   |
//!  |   0 -------------------------------------------  +----------+ |
//!  |  -6 ... min/max envelope of the whole capture ... | ZOOM: a  | |
//!  |   0 ============================================= | few      | |
//!  |     0.00S      0.50S      1.00S      1.50S        | cycles   | |
//!  |                                                   +----------+ |
//!  | SPECTRUM DBFS VS HZ (LOG)  PEAK <f>HZ AT <db>DB                |
//!  |   0 ---------------------------------------------------------- |
//!  | -40 ... magnitude trace per channel, 20 Hz .. 20 kHz log-x  ... |
//!  |-120 20   50  100  200  500  1K   2K   5K   10K  20K            |
//!  +---------------------------------------------------------------+
//! ```
//!
//! Two waveform views on purpose. The envelope answers "how loud, for how long,
//! with what shape"; it cannot answer "is this a sine or a square wave", because
//! at 1 100 pixel columns for 96 000 frames every column spans a whole cycle.
//! The zoom panel shows a few cycles of the dominant tone, which is where the
//! shape lives.
//!
//! Everything is drawn by hand into an RGB byte buffer and handed to the `png`
//! crate. That is a deliberate trade: a plotting library would be a large
//! dependency tree for one chart whose exact look matters (dark background,
//! high-contrast traces, baked-in tick labels) more than its flexibility.
//!
//! Alongside the PNG the renderer writes an **axis sidecar** JSON with the pixel
//! coordinates and values of every gridline, so a reader that wants exact
//! numbers does not have to squint at the image.
//!
//! Robustness is a hard requirement here: this runs on whatever the graph
//! produced, which may be silence, a single frame, or a buffer full of NaN.
//! Every mapping clamps and no path indexes without a bound.

use std::io::BufWriter;
use std::path::{Path, PathBuf};

use crate::analysis::{dbfs, spectrum_with_planner, SpectralPeak, Spectrum};
use crate::capture::Capture;
use crate::error::{ensure_parent_dir, CaptureIoError};
use crate::font::{glyph, text_width, GLYPH_ADVANCE, GLYPH_HEIGHT};

/// Default image width.
pub const PNG_WIDTH: u32 = 1200;
/// Default image height.
pub const PNG_HEIGHT: u32 = 600;

/// Smallest image the renderer will produce; smaller requests are raised to it.
pub const MIN_WIDTH: u32 = 320;
/// Smallest image height.
pub const MIN_HEIGHT: u32 = 200;

/// Low end of the spectrum frequency axis.
pub const SPECTRUM_MIN_HZ: f64 = 20.0;
/// High end of the spectrum frequency axis.
pub const SPECTRUM_MAX_HZ: f64 = 20_000.0;
/// Bottom of the spectrum magnitude axis, in dBFS.
pub const SPECTRUM_FLOOR_DB: f64 = -120.0;
/// Top of the spectrum magnitude axis, in dBFS. A little above full scale, so a
/// clipping peak is still inside the plot rather than flattened against the top.
pub const SPECTRUM_CEILING_DB: f64 = 6.0;

type Rgb = [u8; 3];

const BACKGROUND: Rgb = [13, 17, 23];
const PLOT_BACKGROUND: Rgb = [18, 23, 31];
const GRID: Rgb = [42, 49, 58];
const GRID_MAJOR: Rgb = [70, 80, 92];
const AXIS: Rgb = [120, 132, 146];
const TEXT: Rgb = [200, 210, 222];
const TEXT_DIM: Rgb = [145, 156, 170];
const LEFT_TRACE: Rgb = [88, 166, 255];
const RIGHT_TRACE: Rgb = [247, 129, 97];
const MARKER: Rgb = [63, 185, 80];
const WARNING: Rgb = [255, 84, 214];

/// Text scale. 5x7 glyphs at 2x are 10x14 pixels: legible after the downscaling
/// a vision model applies, without eating the plot.
const TEXT_SCALE: usize = 2;

const MARGIN_LEFT: u32 = 58;
const MARGIN_RIGHT: u32 = 16;
const TITLE_HEIGHT: u32 = 22;
const AXIS_HEIGHT: u32 = 20;

/// Where a rendered capture ended up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedPng {
    pub png: PathBuf,
    /// JSON describing the axes: tick values and their pixel positions.
    pub axes_json: PathBuf,
    pub width: u32,
    pub height: u32,
}

/// Path of the axis sidecar that accompanies `png`.
pub fn axes_sidecar_path(png: &Path) -> PathBuf {
    let mut name = png
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "capture".to_owned());
    name.push_str(".axes.json");
    match png.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
        _ => PathBuf::from(name),
    }
}

/// Render `capture` at the default size.
pub fn render_png(capture: &Capture, path: &Path) -> Result<RenderedPng, CaptureIoError> {
    render_png_sized(capture, path, PNG_WIDTH, PNG_HEIGHT)
}

/// Render `capture` at an explicit size (raised to [`MIN_WIDTH`]/[`MIN_HEIGHT`]).
pub fn render_png_sized(
    capture: &Capture,
    path: &Path,
    width: u32,
    height: u32,
) -> Result<RenderedPng, CaptureIoError> {
    let width = width.max(MIN_WIDTH);
    let height = height.max(MIN_HEIGHT);

    let mut canvas = Canvas::new(width as usize, height as usize, BACKGROUND);
    let layout = Layout::new(width, height);

    // The spectrum is computed first: the waveform zoom needs the dominant
    // frequency to decide how many cycles to show.
    let spectra = channel_spectra(capture);
    let dominant = loudest_peak(&spectra);

    draw_waveform(
        &mut canvas,
        capture,
        &layout,
        dominant.map(|peak| peak.frequency_hz),
    );
    draw_spectrum(&mut canvas, capture, &spectra, &layout);

    ensure_parent_dir(path)?;
    write_png(&canvas, path)?;

    let axes_json = axes_sidecar_path(path);
    write_axes(capture, &spectra, &layout, dominant, &axes_json)?;

    Ok(RenderedPng {
        png: path.to_path_buf(),
        axes_json,
        width,
        height,
    })
}

// ---------------------------------------------------------------------------
// layout
// ---------------------------------------------------------------------------

/// An inclusive-exclusive pixel rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rect {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
}

impl Rect {
    fn width(&self) -> i32 {
        (self.x1 - self.x0).max(1)
    }

    fn height(&self) -> i32 {
        (self.y1 - self.y0).max(1)
    }
}

/// Fraction of the waveform row given to the zoom panel.
const ZOOM_FRACTION: f64 = 0.26;
/// Pixels between the envelope panel and the zoom panel.
const ZOOM_GAP: i32 = 16;
/// Below this the zoom panel is dropped rather than drawn uselessly small.
const MIN_ZOOM_WIDTH: i32 = 140;

struct Layout {
    width: u32,
    height: u32,
    /// The whole-capture envelope view.
    wave: Rect,
    /// A few cycles blown up, so a sine can be told from a square wave. `None`
    /// on images too narrow to spare the room.
    wave_zoom: Option<Rect>,
    spectrum: Rect,
}

impl Layout {
    fn new(width: u32, height: u32) -> Self {
        let half = (height / 2) as i32;
        let x0 = MARGIN_LEFT as i32;
        let x1 = (width - MARGIN_RIGHT) as i32;
        let y0 = TITLE_HEIGHT as i32;
        let y1 = half - AXIS_HEIGHT as i32;

        let zoom_width = ((x1 - x0) as f64 * ZOOM_FRACTION) as i32;
        let (wave, wave_zoom) = if zoom_width >= MIN_ZOOM_WIDTH {
            let split = x1 - zoom_width;
            (
                Rect {
                    x0,
                    y0,
                    x1: split - ZOOM_GAP,
                    y1,
                },
                Some(Rect {
                    x0: split,
                    y0,
                    x1,
                    y1,
                }),
            )
        } else {
            (Rect { x0, y0, x1, y1 }, None)
        };

        Self {
            width,
            height,
            wave,
            wave_zoom,
            spectrum: Rect {
                x0,
                y0: half + TITLE_HEIGHT as i32,
                x1,
                y1: height as i32 - AXIS_HEIGHT as i32,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// waveform panel
// ---------------------------------------------------------------------------

/// Amplitude gridlines, in dB below full scale.
const WAVE_GRID_DB: [f64; 4] = [0.0, -6.0, -12.0, -24.0];

/// Map an amplitude in [-1, 1] onto the panel, clamping anything outside it.
///
/// A NaN amplitude lands on the zero line: `f64::clamp` propagates NaN, and an
/// unguarded `as i32` would turn that into pixel 0, i.e. a full-scale spike at
/// the top of the plot -- exactly the wrong picture of a broken signal.
fn amplitude_to_y(amplitude: f64, rect: &Rect) -> i32 {
    let mid = (rect.y0 + rect.y1) as f64 / 2.0;
    let half = rect.height() as f64 / 2.0;
    let clamped = if amplitude.is_nan() {
        0.0
    } else {
        amplitude.clamp(-1.0, 1.0)
    };
    (mid - clamped * half).round() as i32
}

/// Draw one channel trace into `rect`, over frames `[start, start + len)`.
///
/// Two regimes, because they need different pictures: more frames than pixel
/// columns gives the familiar min/max envelope, fewer gives a connected
/// polyline through the individual samples (an envelope there would be a row of
/// disconnected dots).
///
/// Columns containing a NaN or an infinity are struck through in [`WARNING`], so
/// a broken signal is impossible to miss. Returns whether anything non-zero was
/// drawn.
fn draw_wave_trace(
    canvas: &mut Canvas,
    capture: &Capture,
    rect: &Rect,
    channel: usize,
    colour: Rgb,
    start: usize,
    len: usize,
) -> bool {
    let frames = capture.frames();
    if len == 0 || channel >= capture.channels || start >= frames {
        return false;
    }
    let len = len.min(frames - start);
    let columns = rect.width() as usize;
    let sample_at = |frame: usize| capture.samples[frame * capture.channels + channel];
    let mut audible = false;

    if len <= columns {
        let mut previous: Option<(i32, i32)> = None;
        for index in 0..len {
            let x = if len == 1 {
                rect.x0 + rect.width() / 2
            } else {
                rect.x0 + (index * (columns - 1) / (len - 1)) as i32
            };
            let sample = sample_at(start + index);
            if !sample.is_finite() {
                canvas.vline(x, rect.y0, rect.y1, WARNING);
                previous = None;
                continue;
            }
            if sample != 0.0 {
                audible = true;
            }
            let y = amplitude_to_y(sample as f64, rect);
            match previous {
                Some((previous_x, previous_y)) => canvas.line(previous_x, previous_y, x, y, colour),
                None => canvas.set(x, y, colour),
            }
            // Two pixels tall, so a lone sample is visible at all.
            canvas.set(x, y + 1, colour);
            previous = Some((x, y));
        }
        return audible;
    }

    for column in 0..columns {
        let x = rect.x0 + column as i32;
        let first = start + column * len / columns;
        let last = (start + ((column + 1) * len / columns).max(column * len / columns + 1))
            .min(start + len);

        let mut minimum = f32::INFINITY;
        let mut maximum = f32::NEG_INFINITY;
        let mut bad = false;
        for frame in first..last {
            let sample = sample_at(frame);
            if sample.is_finite() {
                minimum = minimum.min(sample);
                maximum = maximum.max(sample);
            } else {
                bad = true;
            }
        }
        if bad {
            canvas.vline(x, rect.y0, rect.y1, WARNING);
        }
        if !minimum.is_finite() || !maximum.is_finite() {
            continue;
        }
        if minimum != 0.0 || maximum != 0.0 {
            audible = true;
        }
        let top = amplitude_to_y(maximum as f64, rect);
        let bottom = amplitude_to_y(minimum as f64, rect);
        canvas.vline(x, top.min(bottom), top.max(bottom) + 1, colour);
    }
    audible
}

/// Frames to show in the zoom panel: about four cycles of the dominant tone, or
/// 5 ms when there is no tone to lock onto.
fn zoom_window_frames(capture: &Capture, dominant_hz: Option<f64>) -> usize {
    let frames = capture.frames();
    if frames == 0 {
        return 0;
    }
    let sample_rate = if capture.sample_rate.is_finite() && capture.sample_rate > 0.0 {
        capture.sample_rate
    } else {
        48_000.0
    };
    let seconds = match dominant_hz {
        Some(hz) if hz.is_finite() && hz > 0.0 => (4.0 / hz).clamp(0.0005, 0.05),
        _ => 0.005,
    };
    // `clamp` is not usable here: a one-frame capture would make `min > max`
    // and panic.
    ((seconds * sample_rate).round() as usize)
        .max(1)
        .min(frames)
}

/// Length of the zoom window in milliseconds.
fn zoom_window_ms(capture: &Capture, window: usize) -> f64 {
    if capture.sample_rate.is_finite() && capture.sample_rate > 0.0 {
        window as f64 / capture.sample_rate * 1_000.0
    } else {
        0.0
    }
}

/// Amplitude gridlines and the zero line, in `rect`.
fn draw_wave_grid(canvas: &mut Canvas, rect: &Rect) {
    for db in WAVE_GRID_DB {
        let amplitude = 10.0_f64.powf(db / 20.0);
        let colour = if db == 0.0 { GRID_MAJOR } else { GRID };
        for signed in [amplitude, -amplitude] {
            canvas.hline(rect.x0, rect.x1, amplitude_to_y(signed, rect), colour);
        }
    }
    canvas.hline(rect.x0, rect.x1, amplitude_to_y(0.0, rect), AXIS);
    canvas.rect_outline(rect, GRID_MAJOR);
}

fn draw_waveform(
    canvas: &mut Canvas,
    capture: &Capture,
    layout: &Layout,
    dominant_hz: Option<f64>,
) {
    let rect = &layout.wave;
    canvas.fill_rect(rect, PLOT_BACKGROUND);
    draw_wave_grid(canvas, rect);

    // Labelled on the upper half only: the lower half mirrors it, and at this
    // panel height a -24 dB label on both sides collides with the zero line.
    for db in WAVE_GRID_DB {
        let amplitude = 10.0_f64.powf(db / 20.0);
        canvas.text_right_of(
            rect.x0 - 6,
            amplitude_to_y(amplitude, rect),
            &format!("{db:.0}"),
            TEXT_DIM,
        );
    }

    if let Some(zoom) = &layout.wave_zoom {
        canvas.fill_rect(zoom, PLOT_BACKGROUND);
        draw_wave_grid(canvas, zoom);
    }

    let title = format!(
        "WAVEFORM DBFS  L=BLUE R=ORANGE  {} FRAMES  {:.3}S  {:.0}HZ",
        capture.frames(),
        capture.duration_seconds(),
        capture.sample_rate,
    );
    canvas.text(rect.x0, 4, &title, TEXT);
    if capture.dropped > 0 {
        canvas.text_right_aligned(
            (layout.width - MARGIN_RIGHT) as i32,
            4,
            &format!("DROPPED {}", capture.dropped),
            WARNING,
        );
    }

    draw_time_axis(canvas, capture, rect);

    let frames = capture.frames();
    if frames == 0 {
        canvas.text_centred(
            (rect.x0 + rect.x1) / 2,
            (rect.y0 + rect.y1) / 2 - 7,
            "NO SAMPLES CAPTURED",
            WARNING,
        );
        return;
    }

    // Right first, so the left channel stays visible on top of it.
    let mut audible = false;
    for (channel, colour) in [(1_usize, RIGHT_TRACE), (0_usize, LEFT_TRACE)] {
        audible |= draw_wave_trace(canvas, capture, rect, channel, colour, 0, frames);
    }

    if let Some(zoom) = &layout.wave_zoom {
        let window = zoom_window_frames(capture, dominant_hz);
        let start = frames.saturating_sub(window) / 2;
        for (channel, colour) in [(1_usize, RIGHT_TRACE), (0_usize, LEFT_TRACE)] {
            draw_wave_trace(canvas, capture, zoom, channel, colour, start, window);
        }
        canvas.text_centred(
            (zoom.x0 + zoom.x1) / 2,
            zoom.y1 + 5,
            &format!("ZOOM {:.2}MS", zoom_window_ms(capture, window)),
            TEXT_DIM,
        );
    }

    if !audible {
        canvas.text_centred((rect.x0 + rect.x1) / 2, rect.y0 + 10, "SILENCE", TEXT_DIM);
    }
}

/// Second markers along the bottom of the waveform panel.
fn time_ticks(duration: f64) -> Vec<f64> {
    if !(duration.is_finite() && duration > 0.0) {
        return Vec::new();
    }
    // Aim for 4-10 labelled ticks whatever the length.
    const STEPS: [f64; 9] = [0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0];
    let step = STEPS
        .iter()
        .copied()
        .find(|step| duration / step <= 10.0)
        .unwrap_or(duration / 8.0);
    if step <= 0.0 {
        return Vec::new();
    }
    let count = (duration / step).floor() as usize;
    (0..=count.min(40)).map(|i| i as f64 * step).collect()
}

fn draw_time_axis(canvas: &mut Canvas, capture: &Capture, rect: &Rect) {
    let duration = capture.duration_seconds();
    for seconds in time_ticks(duration) {
        let fraction = seconds / duration;
        let x = rect.x0 + (fraction * rect.width() as f64).round() as i32;
        if x >= rect.x1 {
            continue;
        }
        canvas.vline(x, rect.y0, rect.y1, GRID);
        canvas.vline(x, rect.y1, rect.y1 + 4, AXIS);
        canvas.text_centred(x, rect.y1 + 5, &format!("{seconds:.2}S"), TEXT_DIM);
    }
}

// ---------------------------------------------------------------------------
// spectrum panel
// ---------------------------------------------------------------------------

/// Labelled frequency gridlines.
const SPECTRUM_TICKS: [(f64, &str); 10] = [
    (20.0, "20"),
    (50.0, "50"),
    (100.0, "100"),
    (200.0, "200"),
    (500.0, "500"),
    (1_000.0, "1K"),
    (2_000.0, "2K"),
    (5_000.0, "5K"),
    (10_000.0, "10K"),
    (20_000.0, "20K"),
];

/// Unlabelled minor frequency gridlines.
const SPECTRUM_MINOR: [f64; 12] = [
    30.0, 40.0, 70.0, 150.0, 300.0, 400.0, 700.0, 1_500.0, 3_000.0, 4_000.0, 7_000.0, 15_000.0,
];

const SPECTRUM_DB_TICKS: [f64; 7] = [0.0, -20.0, -40.0, -60.0, -80.0, -100.0, -120.0];

fn hz_to_x(hz: f64, rect: &Rect) -> i32 {
    let low = SPECTRUM_MIN_HZ.log10();
    let high = SPECTRUM_MAX_HZ.log10();
    let value = if hz.is_nan() {
        SPECTRUM_MIN_HZ
    } else {
        hz.max(1e-6)
    };
    let fraction = ((value.log10() - low) / (high - low)).clamp(0.0, 1.0);
    rect.x0 + (fraction * rect.width() as f64).round() as i32
}

fn x_to_hz(x: i32, rect: &Rect) -> f64 {
    let low = SPECTRUM_MIN_HZ.log10();
    let high = SPECTRUM_MAX_HZ.log10();
    let fraction = ((x - rect.x0) as f64 / rect.width() as f64).clamp(0.0, 1.0);
    10.0_f64.powf(low + fraction * (high - low))
}

fn db_to_y(db: f64, rect: &Rect) -> i32 {
    if db.is_nan() {
        return rect.y1;
    }
    let fraction =
        ((SPECTRUM_CEILING_DB - db) / (SPECTRUM_CEILING_DB - SPECTRUM_FLOOR_DB)).clamp(0.0, 1.0);
    rect.y0 + (fraction * rect.height() as f64).round() as i32
}

fn channel_spectra(capture: &Capture) -> Vec<Spectrum> {
    let mut planner = realfft::RealFftPlanner::<f64>::new();
    (0..capture.channels)
        .map(|channel| {
            spectrum_with_planner(&capture.channel(channel), capture.sample_rate, &mut planner)
        })
        .collect()
}

/// The loudest dominant peak across all channels.
fn loudest_peak(spectra: &[Spectrum]) -> Option<SpectralPeak> {
    spectra
        .iter()
        .filter_map(Spectrum::dominant)
        .max_by(|a, b| {
            a.magnitude_dbfs
                .partial_cmp(&b.magnitude_dbfs)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

fn draw_spectrum(canvas: &mut Canvas, capture: &Capture, spectra: &[Spectrum], layout: &Layout) {
    let rect = &layout.spectrum;
    canvas.fill_rect(rect, PLOT_BACKGROUND);

    for hz in SPECTRUM_MINOR {
        canvas.vline(hz_to_x(hz, rect), rect.y0, rect.y1, GRID);
    }
    for (hz, label) in SPECTRUM_TICKS {
        let x = hz_to_x(hz, rect);
        canvas.vline(x, rect.y0, rect.y1, GRID_MAJOR);
        canvas.vline(x, rect.y1, rect.y1 + 4, AXIS);
        canvas.text_centred(x, rect.y1 + 5, label, TEXT_DIM);
    }
    for db in SPECTRUM_DB_TICKS {
        let y = db_to_y(db, rect);
        canvas.hline(
            rect.x0,
            rect.x1,
            y,
            if db == 0.0 { GRID_MAJOR } else { GRID },
        );
        canvas.text_right_of(rect.x0 - 6, y, &format!("{db:.0}"), TEXT_DIM);
    }
    canvas.rect_outline(rect, GRID_MAJOR);

    let dominant = loudest_peak(spectra);
    let title = match dominant {
        Some(peak) => format!(
            "SPECTRUM DBFS VS HZ (LOG)  PEAK {:.1}HZ AT {:.1}DB  BIN {:.2}HZ",
            peak.frequency_hz,
            peak.magnitude_dbfs,
            spectra.first().map(|s| s.bin_hz).unwrap_or(0.0),
        ),
        None => "SPECTRUM DBFS VS HZ (LOG)  NO TONAL PEAK".to_owned(),
    };
    canvas.text(rect.x0, rect.y0 - 18, &title, TEXT);

    if spectra.iter().all(Spectrum::is_empty) {
        canvas.text_centred(
            (rect.x0 + rect.x1) / 2,
            (rect.y0 + rect.y1) / 2 - 7,
            "NO SPECTRUM: CAPTURE TOO SHORT",
            WARNING,
        );
        return;
    }

    // Nyquist: everything to the right of it is empty by definition, so say so.
    let nyquist = capture.sample_rate / 2.0;
    if nyquist > SPECTRUM_MIN_HZ && nyquist < SPECTRUM_MAX_HZ {
        let x = hz_to_x(nyquist, rect);
        canvas.dashed_vline(x, rect.y0, rect.y1, AXIS, 4);
        canvas.text_centred(x, rect.y0 + 2, "NYQ", TEXT_DIM);
    }

    for (channel, colour) in [(1_usize, RIGHT_TRACE), (0_usize, LEFT_TRACE)] {
        let Some(spectrum) = spectra.get(channel) else {
            continue;
        };
        if spectrum.is_empty() {
            continue;
        }
        let mut previous: Option<(i32, i32)> = None;
        for x in rect.x0..rect.x1 {
            let low = x_to_hz(x, rect);
            let high = x_to_hz(x + 1, rect);
            let Some(magnitude) = column_magnitude(spectrum, low, high) else {
                continue;
            };
            let y = db_to_y(dbfs(magnitude) as f64, rect);
            match previous {
                Some((previous_x, previous_y)) => canvas.line(previous_x, previous_y, x, y, colour),
                None => canvas.set(x, y, colour),
            }
            previous = Some((x, y));
        }
    }

    if let Some(peak) = dominant {
        let x = hz_to_x(peak.frequency_hz, rect);
        canvas.dashed_vline(x, rect.y0, rect.y1, MARKER, 6);
        let label = format!("{:.0}HZ", peak.frequency_hz);
        let offset = (text_width(&label) * TEXT_SCALE) as i32 / 2 + 6;
        let anchor = if x + 2 * offset < rect.x1 {
            x + offset
        } else {
            x - offset
        };
        canvas.text_centred(anchor, rect.y0 + 14, &label, MARKER);
    }
}

/// Largest magnitude among the bins covering `[low, high)`, or the nearest bin
/// when the column is narrower than one bin (the low-frequency end).
fn column_magnitude(spectrum: &Spectrum, low: f64, high: f64) -> Option<f32> {
    if spectrum.bin_hz <= 0.0 || spectrum.magnitudes.is_empty() {
        return None;
    }
    let last = spectrum.magnitudes.len() - 1;
    let first_bin = (low / spectrum.bin_hz).ceil().max(0.0) as usize;
    let last_bin = (high / spectrum.bin_hz).floor().max(0.0) as usize;

    if first_bin > last_bin || first_bin > last {
        // Narrower than a bin: take the closest one, so the trace stays
        // continuous instead of dropping out below a few hundred hertz.
        let nearest = (((low + high) / 2.0) / spectrum.bin_hz).round().max(0.0) as usize;
        return spectrum.magnitudes.get(nearest.min(last)).copied();
    }

    let mut peak = 0.0_f32;
    for bin in first_bin..=last_bin.min(last) {
        peak = peak.max(spectrum.magnitudes[bin]);
    }
    Some(peak)
}

// ---------------------------------------------------------------------------
// sidecar
// ---------------------------------------------------------------------------

fn write_axes(
    capture: &Capture,
    spectra: &[Spectrum],
    layout: &Layout,
    dominant: Option<SpectralPeak>,
    path: &Path,
) -> Result<(), CaptureIoError> {
    let wave = &layout.wave;
    let spectrum = &layout.spectrum;
    let duration = capture.duration_seconds();

    let wave_y: Vec<serde_json::Value> = WAVE_GRID_DB
        .iter()
        .flat_map(|db| {
            let amplitude = 10.0_f64.powf(db / 20.0);
            [amplitude, -amplitude].into_iter().map(move |signed| {
                serde_json::json!({
                    "label": format!("{db:.0} dB"),
                    "amplitude": signed,
                    "y": amplitude_to_y(signed, wave),
                })
            })
        })
        .collect();

    let wave_x: Vec<serde_json::Value> = time_ticks(duration)
        .into_iter()
        .map(|seconds| {
            let fraction = if duration > 0.0 {
                seconds / duration
            } else {
                0.0
            };
            serde_json::json!({
                "label": format!("{seconds:.3} s"),
                "seconds": seconds,
                "x": wave.x0 + (fraction * wave.width() as f64).round() as i32,
            })
        })
        .collect();

    let spectrum_x: Vec<serde_json::Value> = SPECTRUM_TICKS
        .iter()
        .map(|(hz, label)| {
            serde_json::json!({
                "label": label,
                "hz": hz,
                "x": hz_to_x(*hz, spectrum),
            })
        })
        .collect();

    let spectrum_y: Vec<serde_json::Value> = SPECTRUM_DB_TICKS
        .iter()
        .map(|db| {
            serde_json::json!({
                "label": format!("{db:.0} dBFS"),
                "dbfs": db,
                "y": db_to_y(*db, spectrum),
            })
        })
        .collect();

    let zoom = layout.wave_zoom.map(|zoom| {
        let window = zoom_window_frames(capture, dominant.map(|peak| peak.frequency_hz));
        serde_json::json!({
            "plot": { "x0": zoom.x0, "y0": zoom.y0, "x1": zoom.x1, "y1": zoom.y1 },
            "startFrame": capture.frames().saturating_sub(window) / 2,
            "frames": window,
            "milliseconds": zoom_window_ms(capture, window),
        })
    });

    let document = serde_json::json!({
        "image": { "width": layout.width, "height": layout.height },
        "capture": {
            "frames": capture.frames(),
            "sampleRate": capture.sample_rate,
            "durationSeconds": duration,
            "droppedSamples": capture.dropped,
        },
        "waveform": {
            "plot": { "x0": wave.x0, "y0": wave.y0, "x1": wave.x1, "y1": wave.y1 },
            "yAxis": { "unit": "amplitude", "min": -1.0, "max": 1.0, "ticks": wave_y },
            "xAxis": { "unit": "seconds", "min": 0.0, "max": duration, "ticks": wave_x },
            "zoom": zoom,
        },
        "spectrum": {
            "plot": { "x0": spectrum.x0, "y0": spectrum.y0, "x1": spectrum.x1, "y1": spectrum.y1 },
            "xAxis": {
                "unit": "hz", "scale": "log10",
                "min": SPECTRUM_MIN_HZ, "max": SPECTRUM_MAX_HZ, "ticks": spectrum_x,
            },
            "yAxis": {
                "unit": "dbfs", "scale": "linear",
                "min": SPECTRUM_FLOOR_DB, "max": SPECTRUM_CEILING_DB, "ticks": spectrum_y,
            },
            "fftSize": spectra.first().map(|s| s.size).unwrap_or(0),
            "binHz": spectra.first().map(|s| s.bin_hz).unwrap_or(0.0),
            "window": "hann",
            "dominantHz": dominant.map(|peak| peak.frequency_hz),
        },
        "series": [
            { "channel": "L", "color": "#58a6ff" },
            { "channel": "R", "color": "#f78166" },
        ],
    });

    ensure_parent_dir(path)?;
    let text = serde_json::to_string_pretty(&document).map_err(|source| CaptureIoError::Json {
        path: path.to_path_buf(),
        source,
    })?;
    std::fs::write(path, text).map_err(|source| CaptureIoError::Io {
        path: path.to_path_buf(),
        source,
    })
}

// ---------------------------------------------------------------------------
// canvas
// ---------------------------------------------------------------------------

/// A plain RGB8 pixel buffer with the handful of primitives the plot needs.
struct Canvas {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

impl Canvas {
    fn new(width: usize, height: usize, background: Rgb) -> Self {
        let mut pixels = Vec::with_capacity(width * height * 3);
        for _ in 0..width * height {
            pixels.extend_from_slice(&background);
        }
        Self {
            width,
            height,
            pixels,
        }
    }

    fn set(&mut self, x: i32, y: i32, colour: Rgb) {
        if x < 0 || y < 0 || x as usize >= self.width || y as usize >= self.height {
            return;
        }
        let index = (y as usize * self.width + x as usize) * 3;
        self.pixels[index..index + 3].copy_from_slice(&colour);
    }

    fn fill_rect(&mut self, rect: &Rect, colour: Rgb) {
        for y in rect.y0..rect.y1 {
            for x in rect.x0..rect.x1 {
                self.set(x, y, colour);
            }
        }
    }

    fn rect_outline(&mut self, rect: &Rect, colour: Rgb) {
        self.hline(rect.x0, rect.x1, rect.y0, colour);
        self.hline(rect.x0, rect.x1, rect.y1 - 1, colour);
        self.vline(rect.x0, rect.y0, rect.y1, colour);
        self.vline(rect.x1 - 1, rect.y0, rect.y1, colour);
    }

    fn hline(&mut self, x0: i32, x1: i32, y: i32, colour: Rgb) {
        for x in x0.min(x1)..x0.max(x1) {
            self.set(x, y, colour);
        }
    }

    fn vline(&mut self, x: i32, y0: i32, y1: i32, colour: Rgb) {
        for y in y0.min(y1)..y0.max(y1) {
            self.set(x, y, colour);
        }
    }

    fn dashed_vline(&mut self, x: i32, y0: i32, y1: i32, colour: Rgb, period: i32) {
        let period = period.max(2);
        for y in y0.min(y1)..y0.max(y1) {
            if (y - y0).rem_euclid(period) < period / 2 {
                self.set(x, y, colour);
            }
        }
    }

    /// Bresenham, so a steep spectrum slope stays a connected line.
    fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, colour: Rgb) {
        let dx = (x1 - x0).abs();
        let dy = -(y1 - y0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut error = dx + dy;
        let (mut x, mut y) = (x0, y0);
        loop {
            self.set(x, y, colour);
            if x == x1 && y == y1 {
                return;
            }
            let doubled = 2 * error;
            if doubled >= dy {
                error += dy;
                x += sx;
            }
            if doubled <= dx {
                error += dx;
                y += sy;
            }
        }
    }

    /// Draw `text` with its top-left corner at `(x, y)`.
    fn text(&mut self, x: i32, y: i32, text: &str, colour: Rgb) {
        let scale = TEXT_SCALE as i32;
        let mut pen = x;
        for character in text.chars() {
            for (row_index, row) in glyph(character).iter().enumerate() {
                for (column, cell) in row.chars().enumerate() {
                    if cell != '#' {
                        continue;
                    }
                    for dy in 0..scale {
                        for dx in 0..scale {
                            self.set(
                                pen + column as i32 * scale + dx,
                                y + row_index as i32 * scale + dy,
                                colour,
                            );
                        }
                    }
                }
            }
            pen += GLYPH_ADVANCE as i32 * scale;
        }
    }

    /// Draw `text` centred horizontally on `x`, kept inside the image so an edge
    /// label is never half cut off.
    fn text_centred(&mut self, x: i32, y: i32, text: &str, colour: Rgb) {
        let width = (text_width(text) * TEXT_SCALE) as i32;
        let start = (x - width / 2).clamp(0, (self.width as i32 - width).max(0));
        self.text(start, y, text, colour);
    }

    /// Draw `text` ending at `x`.
    fn text_right_aligned(&mut self, x: i32, y: i32, text: &str, colour: Rgb) {
        let width = (text_width(text) * TEXT_SCALE) as i32;
        self.text((x - width).max(0), y, text, colour);
    }

    /// Draw `text` ending at `x`, vertically centred on `y`. Used for the
    /// left-hand axis labels.
    fn text_right_of(&mut self, x: i32, y: i32, text: &str, colour: Rgb) {
        let height = (GLYPH_HEIGHT * TEXT_SCALE) as i32;
        self.text_right_aligned(x, y - height / 2, text, colour);
    }
}

fn write_png(canvas: &Canvas, path: &Path) -> Result<(), CaptureIoError> {
    let file = std::fs::File::create(path).map_err(|source| CaptureIoError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let writer = BufWriter::new(file);

    let mut encoder = png::Encoder::new(writer, canvas.width as u32, canvas.height as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);

    let mut header = encoder
        .write_header()
        .map_err(|source| CaptureIoError::Png {
            path: path.to_path_buf(),
            source,
        })?;
    header
        .write_image_data(&canvas.pixels)
        .map_err(|source| CaptureIoError::Png {
            path: path.to_path_buf(),
            source,
        })?;
    header.finish().map_err(|source| CaptureIoError::Png {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "mydaw-debug-tap-png-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("scratch directory");
            Self(path)
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn sine_capture(frequency: f64, amplitude: f32, frames: usize) -> Capture {
        let sample_rate = 48_000.0;
        let mut samples = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let phase = std::f64::consts::TAU * frequency * i as f64 / sample_rate;
            let value = (phase.sin() as f32) * amplitude;
            samples.push(value);
            samples.push(value);
        }
        Capture::new(samples, sample_rate, 0)
    }

    /// Decode a PNG back to (width, height, RGB bytes).
    fn decode(path: &Path) -> (u32, u32, Vec<u8>) {
        let file = std::io::BufReader::new(std::fs::File::open(path).expect("open"));
        let decoder = png::Decoder::new(file);
        let mut reader = decoder.read_info().expect("png header");
        let mut buffer = vec![0; reader.output_buffer_size().expect("size")];
        let info = reader.next_frame(&mut buffer).expect("png data");
        buffer.truncate(info.buffer_size());
        (info.width, info.height, buffer)
    }

    #[test]
    fn a_normal_capture_renders_at_the_documented_size() {
        let dir = TempDir::new("normal");
        let path = dir.join("capture.png");
        let rendered = sine_capture(440.0, 0.5, 24_000)
            .render_png(&path)
            .expect("render");

        assert_eq!(rendered.width, PNG_WIDTH);
        assert_eq!(rendered.height, PNG_HEIGHT);
        assert!(rendered.png.is_file());
        assert!(rendered.axes_json.is_file());
        assert!(
            std::fs::metadata(&path).unwrap().len() > 1_000,
            "an empty-looking PNG means nothing was drawn"
        );

        let (width, height, pixels) = decode(&path);
        assert_eq!((width, height), (PNG_WIDTH, PNG_HEIGHT));
        assert_eq!(pixels.len(), (PNG_WIDTH * PNG_HEIGHT * 3) as usize);
        assert!(
            pixels.as_chunks::<3>().0.contains(&LEFT_TRACE),
            "the left channel trace colour should appear somewhere"
        );
        assert!(
            pixels.as_chunks::<3>().0.contains(&MARKER),
            "the dominant-frequency marker should be drawn"
        );
    }

    #[test]
    fn the_axis_sidecar_describes_the_gridlines_in_pixels_and_values() {
        let dir = TempDir::new("sidecar");
        let path = dir.join("capture.png");
        let rendered = sine_capture(1_000.0, 0.25, 24_000)
            .render_png(&path)
            .expect("render");

        let text = std::fs::read_to_string(&rendered.axes_json).expect("sidecar");
        let json: serde_json::Value = serde_json::from_str(&text).expect("valid json");

        assert_eq!(json["image"]["width"], 1_200);
        assert_eq!(json["spectrum"]["window"], "hann");
        assert_eq!(json["spectrum"]["fftSize"], 16_384);
        assert!((json["spectrum"]["dominantHz"].as_f64().unwrap() - 1_000.0).abs() < 1.0);

        let ticks = json["spectrum"]["xAxis"]["ticks"].as_array().unwrap();
        assert_eq!(ticks.len(), SPECTRUM_TICKS.len());
        assert_eq!(ticks[0]["hz"], 20.0);
        assert_eq!(ticks[0]["x"], MARGIN_LEFT);

        let y_ticks = json["spectrum"]["yAxis"]["ticks"].as_array().unwrap();
        assert_eq!(y_ticks.len(), SPECTRUM_DB_TICKS.len());
        assert!(y_ticks[0]["y"].as_i64().unwrap() < y_ticks[1]["y"].as_i64().unwrap());

        let zoom = &json["waveform"]["zoom"];
        assert!(zoom["frames"].as_u64().unwrap() > 0);
        assert!(zoom["milliseconds"].as_f64().unwrap() > 0.0);
    }

    #[test]
    fn the_sidecar_path_sits_next_to_the_image() {
        assert_eq!(
            axes_sidecar_path(Path::new("/tmp/a/capture.png")),
            PathBuf::from("/tmp/a/capture.axes.json")
        );
        assert_eq!(
            axes_sidecar_path(Path::new("capture.png")),
            PathBuf::from("capture.axes.json")
        );
    }

    #[test]
    fn silence_renders_without_panicking_and_says_so() {
        let dir = TempDir::new("silence");
        let path = dir.join("capture.png");
        Capture::new(vec![0.0; 48_000], 48_000.0, 0)
            .render_png(&path)
            .expect("render");
        let (width, height, _) = decode(&path);
        assert_eq!((width, height), (PNG_WIDTH, PNG_HEIGHT));
    }

    #[test]
    fn a_single_frame_capture_renders() {
        let dir = TempDir::new("oneframe");
        let path = dir.join("capture.png");
        Capture::new(vec![0.4, -0.4], 48_000.0, 0)
            .render_png(&path)
            .expect("render");
        assert!(std::fs::metadata(&path).unwrap().len() > 0);
        let (width, height, _) = decode(&path);
        assert_eq!((width, height), (PNG_WIDTH, PNG_HEIGHT));
    }

    #[test]
    fn a_single_orphan_sample_renders_as_an_empty_capture() {
        let dir = TempDir::new("onesample");
        let path = dir.join("capture.png");
        let capture = Capture::new(vec![0.4], 48_000.0, 0);
        assert_eq!(capture.frames(), 0);
        capture.render_png(&path).expect("render");
        let (width, height, _) = decode(&path);
        assert_eq!((width, height), (PNG_WIDTH, PNG_HEIGHT));
    }

    #[test]
    fn an_empty_capture_renders() {
        let dir = TempDir::new("empty");
        let path = dir.join("capture.png");
        Capture::new(Vec::new(), 48_000.0, 0)
            .render_png(&path)
            .expect("render");
        let (_, _, pixels) = decode(&path);
        assert!(pixels.as_chunks::<3>().0.contains(&WARNING));
    }

    #[test]
    fn nan_and_infinity_render_as_warning_columns_instead_of_panicking() {
        let dir = TempDir::new("nan");
        let path = dir.join("capture.png");
        let mut capture = sine_capture(440.0, 0.5, 24_000);
        capture.samples[100] = f32::NAN;
        capture.samples[101] = f32::INFINITY;
        capture.samples[9_000] = f32::NEG_INFINITY;

        capture.render_png(&path).expect("render");
        let (_, _, pixels) = decode(&path);
        assert!(
            pixels.as_chunks::<3>().0.contains(&WARNING),
            "a NaN column must be visibly marked"
        );
    }

    #[test]
    fn a_capture_too_short_for_an_fft_still_renders_a_labelled_frame() {
        let dir = TempDir::new("short");
        let path = dir.join("capture.png");
        sine_capture(440.0, 0.5, 20)
            .render_png(&path)
            .expect("render");
        let (width, height, _) = decode(&path);
        assert_eq!((width, height), (PNG_WIDTH, PNG_HEIGHT));
    }

    #[test]
    fn a_nonsense_sample_rate_does_not_break_the_renderer() {
        let dir = TempDir::new("badrate");
        let path = dir.join("capture.png");
        Capture::new(vec![0.1; 4_096], f64::NAN, 0)
            .render_png(&path)
            .expect("render");
        assert!(std::fs::metadata(&path).unwrap().len() > 0);
    }

    #[test]
    fn a_tiny_requested_size_is_raised_to_the_minimum_and_drops_the_zoom_panel() {
        let dir = TempDir::new("tiny");
        let path = dir.join("capture.png");
        let rendered =
            render_png_sized(&sine_capture(440.0, 0.5, 4_096), &path, 10, 10).expect("render");
        assert_eq!(rendered.width, MIN_WIDTH);
        assert_eq!(rendered.height, MIN_HEIGHT);
        assert!(
            Layout::new(MIN_WIDTH, MIN_HEIGHT).wave_zoom.is_none(),
            "there is no room for a zoom panel at the minimum size"
        );
        let (width, height, _) = decode(&path);
        assert_eq!((width, height), (MIN_WIDTH, MIN_HEIGHT));
    }

    #[test]
    fn the_default_layout_reserves_room_for_both_waveform_panels() {
        let layout = Layout::new(PNG_WIDTH, PNG_HEIGHT);
        let zoom = layout.wave_zoom.expect("the default size has a zoom panel");
        assert!(layout.wave.x1 < zoom.x0, "the panels must not overlap");
        assert!(zoom.width() >= MIN_ZOOM_WIDTH);
        assert_eq!(zoom.x1, (PNG_WIDTH - MARGIN_RIGHT) as i32);
        assert!(layout.wave.y1 < layout.spectrum.y0);
    }

    #[test]
    fn clipped_and_out_of_range_samples_stay_inside_the_plot() {
        let dir = TempDir::new("clip");
        let path = dir.join("capture.png");
        Capture::new(vec![9.0, -9.0, 0.0, 0.0], 48_000.0, 0)
            .render_png(&path)
            .expect("render");

        // Nothing may be drawn outside the image; decoding proves the buffer is
        // exactly the declared size, and `set` clamps by construction.
        let (width, height, pixels) = decode(&path);
        assert_eq!(pixels.len(), (width * height * 3) as usize);
    }

    #[test]
    fn the_frequency_axis_is_logarithmic_and_round_trips() {
        let rect = Rect {
            x0: 58,
            y0: 0,
            x1: 1_184,
            y1: 200,
        };
        assert_eq!(hz_to_x(SPECTRUM_MIN_HZ, &rect), rect.x0);
        assert_eq!(hz_to_x(SPECTRUM_MAX_HZ, &rect), rect.x1);
        // 632 Hz is the geometric midpoint of 20 Hz and 20 kHz.
        let middle = hz_to_x(632.45, &rect);
        assert!(
            (middle - (rect.x0 + rect.x1) / 2).abs() <= 2,
            "log midpoint landed at {middle}"
        );
        for hz in [20.0, 100.0, 1_000.0, 10_000.0, 20_000.0] {
            let back = x_to_hz(hz_to_x(hz, &rect), &rect);
            assert!(
                (back / hz - 1.0).abs() < 0.02,
                "{hz} Hz round-tripped to {back} Hz"
            );
        }
        // Out-of-range input clamps instead of drawing off-canvas.
        assert_eq!(hz_to_x(0.0, &rect), rect.x0);
        assert_eq!(hz_to_x(1e9, &rect), rect.x1);
        assert_eq!(hz_to_x(f64::NAN, &rect), rect.x0);
    }

    #[test]
    fn the_magnitude_axis_puts_full_scale_near_the_top_and_clamps() {
        let rect = Rect {
            x0: 0,
            y0: 100,
            x1: 100,
            y1: 300,
        };
        assert_eq!(db_to_y(SPECTRUM_CEILING_DB, &rect), rect.y0);
        assert_eq!(db_to_y(SPECTRUM_FLOOR_DB, &rect), rect.y1);
        assert_eq!(db_to_y(1e9, &rect), rect.y0);
        assert_eq!(db_to_y(f64::NEG_INFINITY, &rect), rect.y1);
        assert_eq!(db_to_y(f64::NAN, &rect), rect.y1);
        assert!(db_to_y(0.0, &rect) > rect.y0 && db_to_y(0.0, &rect) < rect.y1);
    }

    #[test]
    fn amplitudes_map_symmetrically_around_the_zero_line() {
        let rect = Rect {
            x0: 0,
            y0: 0,
            x1: 10,
            y1: 200,
        };
        assert_eq!(amplitude_to_y(1.0, &rect), 0);
        assert_eq!(amplitude_to_y(-1.0, &rect), 200);
        assert_eq!(amplitude_to_y(0.0, &rect), 100);
        assert_eq!(amplitude_to_y(5.0, &rect), 0, "clamped, not off-canvas");
        assert_eq!(amplitude_to_y(f64::INFINITY, &rect), 0);
        assert_eq!(amplitude_to_y(f64::NEG_INFINITY, &rect), 200);
        assert_eq!(
            amplitude_to_y(f64::NAN, &rect),
            100,
            "a NaN must sit on the zero line, not spike to full scale"
        );
    }

    #[test]
    fn the_zoom_window_shows_a_few_cycles_of_the_dominant_tone() {
        let capture = sine_capture(440.0, 0.5, 48_000);
        let frames = zoom_window_frames(&capture, Some(440.0));
        // Four cycles of 440 Hz at 48 kHz is 436 frames.
        assert!(
            (400..=470).contains(&frames),
            "expected about four cycles, got {frames} frames"
        );

        // No tone: fall back to 5 ms.
        assert_eq!(zoom_window_frames(&capture, None), 240);
        // Absurd input does not produce an absurd window.
        for hz in [Some(0.0), Some(f64::NAN), Some(-1.0), Some(1e9)] {
            let frames = zoom_window_frames(&capture, hz);
            assert!((1..=48_000).contains(&frames), "{hz:?} gave {frames}");
        }
        // Never longer than the capture.
        assert_eq!(
            zoom_window_frames(&sine_capture(440.0, 0.5, 10), Some(20.0)),
            10
        );
        assert_eq!(
            zoom_window_frames(&sine_capture(440.0, 0.5, 1), Some(440.0)),
            1,
            "a one-frame capture must not make the window bounds cross"
        );
        assert_eq!(
            zoom_window_frames(&Capture::new(Vec::new(), 48_000.0, 0), None),
            0
        );
    }

    #[test]
    fn time_ticks_stay_readable_at_every_length() {
        assert!(time_ticks(0.0).is_empty());
        assert!(time_ticks(f64::NAN).is_empty());
        for duration in [0.05, 0.5, 2.0, 10.0, 60.0] {
            let ticks = time_ticks(duration);
            assert!(
                (2..=41).contains(&ticks.len()),
                "{duration}s produced {} ticks",
                ticks.len()
            );
            assert_eq!(ticks[0], 0.0);
            assert!(*ticks.last().unwrap() <= duration + 1e-9);
        }
    }

    #[test]
    fn the_canvas_never_writes_outside_its_buffer() {
        let mut canvas = Canvas::new(4, 4, BACKGROUND);
        canvas.set(-1, 0, MARKER);
        canvas.set(0, -1, MARKER);
        canvas.set(4, 0, MARKER);
        canvas.set(0, 4, MARKER);
        canvas.line(-50, -50, 50, 50, MARKER);
        canvas.text(-100, -100, "OUT OF RANGE", MARKER);
        canvas.text_centred(2, 0, "WIDER THAN THE CANVAS", MARKER);
        canvas.text_right_aligned(-40, 0, "LEFT OF THE CANVAS", MARKER);
        assert_eq!(canvas.pixels.len(), 4 * 4 * 3);
        // The diagonal still painted the pixels that are inside.
        assert_eq!(&canvas.pixels[0..3], &MARKER);
    }
}
