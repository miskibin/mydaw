//! Scanning and hosting a real VST3 plugin, end to end.
//!
//! Needs a stereo effect bundle: set `WAVE_VST3_TEST_BUNDLE` to its `.vst3`
//! path. The reference fixture is the `vst3` crate's own example gain plugin
//! (`examples/gain.rs`, dual-component, one "Gain" parameter, gain = value)
//! built as a cdylib and placed at `<Name>.vst3/Contents/x86_64-linux/<Name>.so`
//! (`x86_64-win/<Name>.vst3` on Windows). Without it the plugin-dependent tests
//! print why and pass vacuously; the garbage-module tests always run.

use std::path::{Path, PathBuf};

use wave_engine::{ProcessCtx, Processor};
use wave_vst3::testing::{instantiate_effect, scan_roots};
use wave_vst3::Vst3Error;

const WORKER: &str = env!("CARGO_BIN_EXE_wave-vst3-scan-worker");

fn fixture() -> Option<PathBuf> {
    let Some(path) = std::env::var_os("WAVE_VST3_TEST_BUNDLE") else {
        eprintln!("skipped: WAVE_VST3_TEST_BUNDLE is not set (no VST3 plugin to host)");
        return None;
    };
    let path = PathBuf::from(path);
    assert!(
        path.exists(),
        "WAVE_VST3_TEST_BUNDLE points at nothing: {}",
        path.display()
    );
    Some(path)
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wave-vst3-it-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The fixture's one effect, as an out-of-process scan reports it.
fn scanned(bundle: &Path) -> wave_vst3::PluginInfo {
    let root = bundle.parent().unwrap().to_path_buf();
    let (effects, children, blocked) = scan_roots(&[root], Some(Path::new(WORKER)));
    assert!(
        blocked.is_empty(),
        "the fixture must not crash its worker: {blocked:?}"
    );
    assert!(
        children >= 1,
        "the module must have been opened in a child process"
    );
    effects
        .into_iter()
        .find(|effect| Path::new(&effect.path) == bundle)
        .expect("the fixture's effect is listed")
}

fn sine(frames: usize) -> (Vec<f32>, Vec<f32>) {
    let l: Vec<f32> = (0..frames).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
    let r: Vec<f32> = l.iter().map(|x| -x).collect();
    (l, r)
}

fn ctx() -> ProcessCtx {
    ProcessCtx {
        sample_rate: 48_000.0,
        bpm: 120.0,
        beat: 0.0,
        playing: true,
    }
}

fn assert_scaled(out: &[f32], input: &[f32], gain: f32) {
    for (o, i) in out.iter().zip(input) {
        assert!((o - i * gain).abs() < 1e-6, "expected {} got {o}", i * gain);
    }
}

#[test]
fn a_garbage_module_is_refused_by_its_worker_without_being_block_listed() {
    let root = scratch("garbage");
    std::fs::write(root.join("garbage.vst3"), b"not a shared library").unwrap();
    let (effects, children, blocked) =
        scan_roots(std::slice::from_ref(&root), Some(Path::new(WORKER)));
    assert!(effects.is_empty());
    assert_eq!(children, 1, "one child for the one unknown module");
    assert!(
        blocked.is_empty(),
        "a refusal is cached, not block-listed: only crashes/hangs are"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_worker_binary_refuses_to_run_by_hand() {
    let status = std::process::Command::new(WORKER)
        .env_remove(wave_plugin_scan::worker::FORMAT_ENV)
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(2));
}

#[test]
fn out_of_process_and_in_process_scans_agree() {
    let Some(bundle) = fixture() else { return };
    let info = scanned(&bundle);
    assert_eq!(info.cid.len(), 32);
    assert!(!info.name.is_empty());
    let (in_process, children, _) = scan_roots(&[bundle.parent().unwrap().to_path_buf()], None);
    assert_eq!(children, 0);
    assert!(in_process.contains(&info), "{in_process:?} vs {info:?}");
}

#[test]
fn an_effect_processes_audio_and_follows_its_parameters() {
    let Some(bundle) = fixture() else { return };
    let info = scanned(&bundle);
    let (mut effect, handle) =
        instantiate_effect(&info.path, &info.cid, None, 48_000.0, 64).unwrap();
    assert_eq!(handle.info().cid, info.cid);

    let params = handle.params();
    assert!(!params.is_empty(), "the fixture lists a parameter");
    let gain = params[0].clone();
    assert_eq!(gain.value, 1.0, "{params:?}");
    assert_eq!(gain.def, 1.0);

    // Unity gain.
    let (input_l, input_r) = sine(64);
    let (mut l, mut r) = (input_l.clone(), input_r.clone());
    effect.process(&mut l, &mut r, &ctx());
    assert_scaled(&l, &input_l, 1.0);
    assert_scaled(&r, &input_r, 1.0);

    // A parameter move reaches the processor at the next block, and the
    // controller (what the UI and an editor read) agrees.
    assert!(!handle.take_state_changed(), "instantiation is not an edit");
    handle.set_param(gain.id, 0.25).unwrap();
    handle.set_param(gain.id, 0.5).unwrap();
    assert!(handle.take_state_changed());
    let (mut l, mut r) = (input_l.clone(), input_r.clone());
    effect.process(&mut l, &mut r, &ctx());
    assert_scaled(&l, &input_l, 0.5);
    assert_scaled(&r, &input_r, 0.5);
    assert_eq!(handle.params()[0].value, 0.5);
    assert!(
        handle.params()[0].display.starts_with("0.5"),
        "{:?}",
        handle.params()[0]
    );

    // Blocks longer than the activated maximum are processed in chunks.
    let (input_l, input_r) = sine(200);
    let (mut l, mut r) = (input_l.clone(), input_r.clone());
    effect.process(&mut l, &mut r, &ctx());
    assert_scaled(&l, &input_l, 0.5);

    // Re-preparing at a new rate and a larger block re-activates the plugin.
    effect.prepare(44_100.0, 512);
    let (mut l, mut r) = (input_l.clone(), input_r.clone());
    effect.process(&mut l, &mut r, &ctx());
    assert_scaled(&l, &input_l, 0.5);
    assert_scaled(&r, &input_r, 0.5);

    assert!(matches!(
        handle.set_param(gain.id, 1.5),
        Err(Vst3Error::InvalidParamValue(_))
    ));
    assert!(matches!(
        handle.set_param(gain.id, f64::NAN),
        Err(Vst3Error::InvalidParamValue(_))
    ));

    // Teardown in either order is clean.
    drop(effect);
    assert_eq!(
        handle.params()[0].value,
        0.5,
        "the instance lives while a handle does"
    );
    drop(handle);
}

#[test]
fn state_round_trips_through_a_new_instance() {
    let Some(bundle) = fixture() else { return };
    let info = scanned(&bundle);
    let (_effect, handle) = instantiate_effect(&info.path, &info.cid, None, 48_000.0, 128).unwrap();
    let state = handle.get_state().unwrap();
    assert!(
        state.starts_with(b"WAVE-VST3-STATE1"),
        "the versioned envelope"
    );

    let (mut restored, restored_handle) =
        instantiate_effect(&info.path, &info.cid, Some(&state), 48_000.0, 128).unwrap();
    assert_eq!(
        restored_handle.get_state().unwrap(),
        state,
        "byte-identical after save -> load -> save"
    );
    assert!(
        !restored_handle.take_state_changed(),
        "restoring is not an edit"
    );
    let (input_l, input_r) = sine(128);
    let (mut l, mut r) = (input_l.clone(), input_r.clone());
    restored.process(&mut l, &mut r, &ctx());
    assert_scaled(&l, &input_l, 1.0);

    let err = instantiate_effect(
        &info.path,
        &info.cid,
        Some(b"WAVE-VST3-STATE1\x01"),
        48_000.0,
        128,
    )
    .expect_err("a truncated envelope is refused");
    assert!(matches!(err, Vst3Error::InvalidState(_)), "got {err:?}");
}

#[test]
fn hosting_errors_are_typed() {
    let Some(bundle) = fixture() else { return };
    let info = scanned(&bundle);
    let err = instantiate_effect(&info.path, &"00".repeat(16), None, 48_000.0, 64)
        .expect_err("no such class");
    assert!(
        matches!(err, Vst3Error::ClassNotFound { .. }),
        "got {err:?}"
    );
    let err = instantiate_effect(&info.path, &info.cid, None, 0.0, 64).expect_err("no such rate");
    assert!(
        matches!(err, Vst3Error::InvalidAudioConfig(_)),
        "got {err:?}"
    );

    // The fixture has no editor; on platforms without a window backend the
    // window service says so first.
    let (_effect, handle) = instantiate_effect(&info.path, &info.cid, None, 48_000.0, 64).unwrap();
    assert!(handle.has_editor(), "it has a controller");
    let windows = wave_plugin_windows::Service::new();
    let err = handle.open_editor(&windows).unwrap_err();
    assert!(
        matches!(
            err,
            Vst3Error::NoEditor | Vst3Error::UnsupportedEditorApi { .. } | Vst3Error::Window(_)
        ),
        "got {err:?}"
    );
    assert!(!handle.is_editor_open());
    handle.close_editor().unwrap();
}
