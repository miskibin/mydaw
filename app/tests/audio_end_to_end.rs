//! End-to-end check of the shell's audio path on a machine with a real device.
//!
//! Everything except the webview: boot the kernel, open the output device,
//! install the test-tone graph, start the transport and watch meter frames come
//! back through the pump. It makes an audible 440 Hz tone for about a second,
//! which is why it is `#[ignore]`d -- `cargo test --workspace` must stay silent
//! and must pass on machines with no sound card.
//!
//! ```sh
//! cargo test -p mydaw-app --test audio_end_to_end -- --ignored --nocapture
//! ```

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::sleep;
use std::time::Duration;

use mydaw_app::{boot_kernel, AppState, MeterFrameDto};
use parking_lot::Mutex;

/// Frames collected from the pump, newest last.
#[derive(Default)]
struct Collected {
    frames: Mutex<Vec<MeterFrameDto>>,
    count: AtomicU64,
}

#[test]
#[ignore = "opens the audio device and makes an audible tone"]
fn play_produces_a_tone_and_stop_silences_it() {
    let kernel = boot_kernel().expect("kernel boots");
    let state = AppState::from_kernel(&kernel).expect("services present");

    let info = match state.start_audio() {
        Ok(info) => info,
        Err(err) => {
            eprintln!("no audio device ({err}); nothing to verify on this machine");
            return;
        }
    };
    eprintln!(
        "device: {} @ {} Hz, {} ch",
        info.device_name, info.sample_rate, info.channels
    );

    let collected = Arc::new(Collected::default());
    let sink = Arc::clone(&collected);
    state.meters().subscribe(Box::new(move |frame| {
        sink.count.fetch_add(1, Ordering::Relaxed);
        sink.frames.lock().push(frame);
        true
    }));

    // --- stopped: the gate must keep the device silent -------------------
    sleep(Duration::from_millis(300));
    let idle: Vec<MeterFrameDto> = collected.frames.lock().drain(..).collect();
    assert!(!idle.is_empty(), "the pump must emit frames while stopped");
    assert!(
        idle.iter().all(|f| !f.playing && f.peak_l == 0.0),
        "a stopped transport must produce silence, got {:?}",
        idle.first()
    );

    // --- playing: tone audible, meters move, playhead advances -----------
    state.set_playing(true).expect("transport starts");
    sleep(Duration::from_millis(700));
    let playing: Vec<MeterFrameDto> = collected.frames.lock().drain(..).collect();

    assert!(
        playing.len() >= 10,
        "expected a ~30 Hz stream over 700 ms, got {} frames",
        playing.len()
    );
    let loudest = playing.iter().fold(0.0_f32, |acc, f| acc.max(f.peak_l));
    assert!(
        loudest > 0.1,
        "the test tone should peak near -14 dBFS, got {loudest}"
    );
    let last = playing.last().unwrap();
    assert!(last.playing, "frames must report the transport as running");
    assert!(
        last.sample_pos > playing[0].sample_pos,
        "the playhead must advance while playing"
    );

    // --- stopped again: silence returns, position is kept ----------------
    state.set_playing(false).expect("transport stops");
    sleep(Duration::from_millis(300));
    let after: Vec<MeterFrameDto> = collected.frames.lock().drain(..).collect();
    let tail = after.last().expect("frames keep arriving after stop");
    assert!(!tail.playing);
    assert_eq!(tail.peak_l, 0.0, "stop must silence the output");
    assert!(
        tail.sample_pos >= last.sample_pos,
        "stop keeps the playhead where it is; it does not rewind"
    );

    eprintln!(
        "{} frames total, loudest peak {loudest:.3}, final position {} frames",
        collected.count.load(Ordering::Relaxed),
        tail.sample_pos
    );

    state.meters().shutdown();
    state.audio().stop();
}
