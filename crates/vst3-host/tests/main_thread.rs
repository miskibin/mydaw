//! With an executor installed, plugin work runs on the "UI" thread and the
//! effect/handle work from anywhere -- the shape of the Tauri integration,
//! with a plain thread and a channel standing in for `run_on_main_thread`.
//! One test per binary: the executor is process-global.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;

use wave_engine::ProcessCtx;
use wave_vst3::{MainThreadJob, Vst3Error};

#[test]
fn plugin_work_is_marshalled_to_the_ui_thread() {
    let jobs_run = Arc::new(AtomicUsize::new(0));
    let (installed_tx, installed_rx) = mpsc::channel();
    let counter = Arc::clone(&jobs_run);
    // The "UI thread": installs the executor from itself, then pumps jobs.
    let ui = std::thread::spawn(move || {
        let (job_tx, job_rx) = mpsc::channel::<MainThreadJob>();
        let job_tx = std::sync::Mutex::new(job_tx);
        assert!(wave_vst3::set_main_thread_executor(move |job| {
            let _ = job_tx.lock().unwrap().send(job);
        }));
        assert!(wave_vst3::on_main_thread());
        installed_tx.send(()).unwrap();
        // Exits when the executor is gone (never, here) or after a quiet spell.
        while let Ok(job) = job_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            counter.fetch_add(1, Ordering::SeqCst);
            job();
        }
    });
    installed_rx.recv().unwrap();
    assert!(
        !wave_vst3::on_main_thread(),
        "the test thread is not the UI thread"
    );
    assert!(
        !wave_vst3::set_main_thread_executor(|_| {}),
        "installed once"
    );

    // Garbage still fails cleanly through the executor.
    let err = wave_vst3::instantiate("/nowhere/x.vst3", &"00".repeat(16), None, 48_000.0, 64)
        .err()
        .expect("must fail");
    assert!(
        matches!(err, Vst3Error::Load { .. } | Vst3Error::UnsupportedPlatform),
        "got {err:?}"
    );
    assert!(
        jobs_run.load(Ordering::SeqCst) >= 1,
        "instantiation ran as a UI-thread job"
    );

    // Editor calls are refused off the UI thread.
    let windows = wave_plugin_windows::Service::new(); // owned by *this* thread
    let Some(bundle) = std::env::var_os("WAVE_VST3_TEST_BUNDLE") else {
        eprintln!("skipped the hosting half: WAVE_VST3_TEST_BUNDLE is not set");
        return;
    };
    let root = std::path::Path::new(&bundle)
        .parent()
        .unwrap()
        .to_path_buf();
    let worker = std::path::Path::new(env!("CARGO_BIN_EXE_wave-vst3-scan-worker"));
    let (effects, _, _) = wave_vst3::testing::scan_roots(&[root], Some(worker));
    let info = effects
        .into_iter()
        .find(|effect| std::path::Path::new(&effect.path) == std::path::Path::new(&bundle))
        .expect("fixture listed");

    let before = jobs_run.load(Ordering::SeqCst);
    let (mut effect, handle) =
        wave_vst3::instantiate(&info.path, &info.cid, None, 48_000.0, 64).unwrap();
    let id = handle.params()[0].id;
    handle.set_param(id, 0.5).unwrap();
    assert!(matches!(
        handle.open_editor(&windows),
        Err(Vst3Error::WrongThread)
    ));
    assert!(matches!(handle.close_editor(), Err(Vst3Error::WrongThread)));

    // The effect moves to an "audio thread" and processes there.
    let audio = std::thread::spawn(move || {
        let ctx = ProcessCtx {
            sample_rate: 48_000.0,
            ..ProcessCtx::default()
        };
        let (mut l, mut r) = (vec![1.0_f32; 64], vec![1.0_f32; 64]);
        effect.process(&mut l, &mut r, &ctx);
        effect.prepare(96_000.0, 64); // re-activation goes through the UI thread
        effect.process(&mut l, &mut r, &ctx);
        (effect, l[0])
    });
    let (effect, sample) = audio.join().unwrap();
    assert!(
        (sample - 0.25).abs() < 1e-6,
        "0.5 gain applied twice: {sample}"
    );
    assert_eq!(handle.params()[0].value, 0.5);
    assert!(!handle.get_state().unwrap().is_empty());

    drop(effect);
    let jobs_before_teardown = jobs_run.load(Ordering::SeqCst);
    drop(handle); // teardown is posted to the UI thread
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while jobs_run.load(Ordering::SeqCst) == jobs_before_teardown
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let after = jobs_run.load(Ordering::SeqCst);
    // instantiate, params, set_param's controller update, prepare, params,
    // get_state, teardown: all on the UI thread.
    assert!(
        after - before >= 7,
        "only {} UI-thread jobs",
        after - before
    );
    drop(ui); // detach; it exits after its quiet spell
}
