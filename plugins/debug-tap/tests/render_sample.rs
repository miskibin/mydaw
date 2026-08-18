//! Renders reference images so a human (or a vision model) can look at the plot
//! the harness produces without booting the DAW.
//!
//! `#[ignore]`d: it writes files, and `cargo test --workspace` should not leave
//! artefacts behind. Run it deliberately:
//!
//! ```sh
//! cargo test -p mydaw-plugin-debug-tap --test render_sample -- --ignored --nocapture
//! ```
//!
//! Output goes to `MYDAW_DEBUG_TAP_SAMPLE_DIR` if set, otherwise
//! `<temp>/mydaw-debug-tap-samples`.

use std::f64::consts::TAU;
use std::path::PathBuf;

use mydaw_plugin_debug_tap::Capture;

const SAMPLE_DIR_ENV: &str = "MYDAW_DEBUG_TAP_SAMPLE_DIR";
const SR: f64 = 48_000.0;

fn output_dir() -> PathBuf {
    let dir = std::env::var_os(SAMPLE_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("mydaw-debug-tap-samples"));
    std::fs::create_dir_all(&dir).expect("sample directory");
    dir
}

/// Sum of partials, so the spectrum has something to show.
fn tone(partials: &[(f64, f32)], frames: usize) -> Capture {
    let mut samples = Vec::with_capacity(frames * 2);
    for i in 0..frames {
        let value: f32 = partials
            .iter()
            .map(|(frequency, amplitude)| {
                ((TAU * frequency * i as f64 / SR).sin() as f32) * *amplitude
            })
            .sum();
        samples.push(value);
        samples.push(value * 0.5);
    }
    Capture::new(samples, SR, 0)
}

#[test]
#[ignore = "writes reference PNGs for visual inspection"]
fn write_reference_images() {
    let dir = output_dir();
    let cases: Vec<(&str, Capture)> = vec![
        ("sine-440", tone(&[(440.0, 0.5)], (SR * 2.0) as usize)),
        (
            "harmonics",
            tone(
                &[(220.0, 0.4), (440.0, 0.2), (880.0, 0.1), (3_520.0, 0.05)],
                (SR * 1.0) as usize,
            ),
        ),
        ("silence", Capture::new(vec![0.0; 96_000], SR, 0)),
        ("one-frame", Capture::new(vec![0.4, -0.4], SR, 0)),
        ("empty", Capture::new(Vec::new(), SR, 0)),
    ];

    for (name, capture) in cases {
        let png = dir.join(format!("{name}.png"));
        let rendered = capture.render_png(&png).expect("render");
        println!("{}", capture.summary_text());
        println!(
            "wrote {} and {}",
            rendered.png.display(),
            rendered.axes_json.display()
        );
    }
    println!("sample directory: {}", dir.display());
}
