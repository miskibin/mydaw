//! Step 7's hardware verification: the host, against the plugins actually
//! installed on this machine.
//!
//! Every test here is **opt-in and skip-if-absent**. A normal workspace run must
//! not execute arbitrary installed plugin code: a vendor module can block in
//! initialisation or processing, and these tests also need to run serially.
//! What they prove when explicitly enabled is the whole point of the spike, so
//! they print a compatibility table rather than only asserting: run them with
//!
//! ```text
//! cargo test -p mydaw-plugin-host-vst3 --test hardware -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `--test-threads=1` matters. Several of these load large third-party modules,
//! and running two vendors' initialisation code concurrently is a good way to
//! find out which of them has a static that is not thread-safe.

use mydaw_plugin_debug_tap::analysis::spectrum;
use mydaw_plugin_host_vst3::hosting::Vst3BusLayout;
use mydaw_plugin_host_vst3::test_util::{
    fill_sine, first_workable, installed_plugins, missing_plugins_message, peak_and_finite,
    print_attempts, probe_each, rms, scanned_effects, scanned_instruments,
};
use mydaw_rt_core::{
    AudioBuffers, NoteEvent, NoteEventBuffer, ProcessCtx, RtNode, TransportSnapshot,
};

const SAMPLE_RATE: f64 = 48_000.0;
const MAX_BLOCK: usize = 512;

/// (a) What is installed, and what the scanner made of it.
#[test]
#[ignore = "requires installed VST3 plugins; run the hardware suite explicitly and serially"]
fn the_scan_finds_and_classifies_what_is_installed() {
    let plugins = installed_plugins();
    if plugins.is_empty() {
        eprintln!("{}", missing_plugins_message());
        return;
    }

    eprintln!(
        "\n=== VST3 scan: {} plugin class(es) found ===",
        plugins.len()
    );
    for info in plugins.iter().take(10) {
        eprintln!(
            "  {:<36} {:<20} {:<8} {:<24} {}",
            info.name,
            info.vendor,
            info.kind().id(),
            info.subcategories,
            info.path.display()
        );
    }
    if plugins.len() > 10 {
        eprintln!("  ... and {} more", plugins.len() - 10);
    }
    eprintln!(
        "  classified: {} instrument(s), {} effect(s)",
        scanned_instruments().len(),
        scanned_effects().len()
    );

    // Every scanned class must be usable as a locator: a blank name would still
    // be legal, but a class id that does not round-trip is a bug in this crate
    // rather than in the plugin.
    for info in plugins {
        assert_eq!(
            mydaw_plugin_host_vst3::ClassId::from_hex(&info.cid.to_hex()),
            Some(info.cid),
            "'{}' has a class id that does not round-trip",
            info.name
        );
        assert!(
            info.path.exists(),
            "'{}' names a module that is not there",
            info.name
        );
    }
}

/// (b) A real effect processes a real sine, and the output is finite.
///
/// Probes candidates until one both activates *and* puts something on its
/// output, because "it activated" is not evidence that audio flowed -- see
/// [`probe_each`](mydaw_plugin_host_vst3::test_util::probe_each).
#[test]
#[ignore = "requires installed VST3 plugins; run the hardware suite explicitly and serially"]
fn a_real_effect_processes_sixty_four_blocks_of_silence_and_sine() {
    let candidates = scanned_effects();
    if candidates.is_empty() {
        eprintln!("{}", missing_plugins_message());
        return;
    }

    let (winner, attempts) = probe_each(
        &candidates,
        Vst3BusLayout::is_stereo_effect,
        std::env::var("MYDAW_VST3_FX").ok().as_deref(),
        |_info, plugin| run_effect(plugin),
    );
    print_attempts("\n=== VST3 effect probe ===", &attempts);

    let Some((info, _plugin, measured)) = winner else {
        eprintln!(
            "SKIPPED: none of the {} scanned effect(s) both activated and produced output. \
             The table above says why for each; an unlicensed plugin refusing to initialise, \
             or a wet-only processor that is silent without a vocal, are expected outcomes \
             rather than host bugs.",
            candidates.len()
        );
        return;
    };

    eprintln!(
        "\n=== processed through '{}' ({}) ===",
        info.name, info.vendor
    );
    eprintln!("  blocks processed  : {}", measured.blocks);
    eprintln!("  process errors    : {}", measured.errors);
    eprintln!("  peak from silence : {:.6}", measured.silent_peak);
    eprintln!("  sine  in  rms     : {:.6}", measured.input_rms);
    eprintln!("  sine  out rms     : {:.6}", measured.output_rms);
    eprintln!("  sine  out peak    : {:.6}", measured.output_peak);
    eprintln!(
        "  samples changed   : {:.1}%",
        measured.changed_fraction * 100.0
    );
    match &measured.dominant_hz {
        Some((hz, db)) => eprintln!("  dominant out bin  : {hz:.1} Hz @ {db:.1} dBFS"),
        None => eprintln!("  dominant out bin  : (silent)"),
    }
    eprintln!(
        "  verdict           : {}",
        if measured.changed_fraction < 0.01 {
            "passthrough -- the host moved audio through the plugin unaltered"
        } else {
            "the plugin altered the signal the host handed it"
        }
    );

    // Asserted: the host's own contract. Deliberately *not* asserted: what the
    // effect did with the signal, which is a stranger's default preset.
    assert_eq!(measured.blocks, 64);
    assert_eq!(
        measured.errors, 0,
        "'{}' failed a process() call",
        info.name
    );
    assert!(
        measured.output_peak < 8.0,
        "'{}' produced a peak of {}, which is not audio",
        info.name,
        measured.output_peak
    );
    assert!(
        measured.silent_peak < 0.5,
        "'{}' invented {} of signal from pure silence",
        info.name,
        measured.silent_peak
    );
}

/// What one effect run measured.
struct EffectRun {
    blocks: u64,
    errors: u64,
    silent_peak: f32,
    input_rms: f64,
    output_rms: f64,
    output_peak: f32,
    changed_fraction: f64,
    dominant_hz: Option<(f64, f32)>,
}

/// Activate `plugin`, push 32 blocks of silence then 32 of a sine, and measure.
///
/// `Err` means "this plugin did not demonstrate anything", which sends the probe
/// on to the next candidate with the reason recorded in the table.
fn run_effect(plugin: &mut mydaw_plugin_host_vst3::LoadedVst3Plugin) -> Result<EffectRun, String> {
    let mut node = plugin
        .create_node(SAMPLE_RATE, MAX_BLOCK)
        .map_err(|err| err.to_string())?;
    let mut buffers = AudioBuffers::new(MAX_BLOCK);

    // --- 32 blocks of silence: an effect must not invent anything ----------
    let mut silent_peak = 0.0_f32;
    for _ in 0..32 {
        buffers.silence();
        let mut ctx = ProcessCtx::new(
            &mut buffers,
            SAMPLE_RATE,
            MAX_BLOCK,
            TransportSnapshot::default(),
        );
        node.process(&mut ctx);
        let (peak, finite) = peak_and_finite(&buffers);
        if !finite {
            return Err("produced a non-finite sample from silence".to_owned());
        }
        silent_peak = silent_peak.max(peak);
    }

    // --- 32 blocks of a 440 Hz sine ---------------------------------------
    let mut phase = 0.0;
    let mut input_rms_total = 0.0_f64;
    let mut output_rms_total = 0.0_f64;
    let mut output_peak = 0.0_f32;
    let mut changed = 0_usize;
    let mut total = 0_usize;
    let mut last_output = vec![0.0_f32; MAX_BLOCK];

    for _ in 0..32 {
        fill_sine(&mut buffers, 440.0, SAMPLE_RATE, &mut phase);
        let before: Vec<f32> = buffers.as_slices()[0][..MAX_BLOCK].to_vec();
        input_rms_total += f64::from(rms(&buffers));

        let mut ctx = ProcessCtx::new(
            &mut buffers,
            SAMPLE_RATE,
            MAX_BLOCK,
            TransportSnapshot::default(),
        );
        node.process(&mut ctx);

        let (peak, finite) = peak_and_finite(&buffers);
        if !finite {
            return Err("produced a non-finite sample from a sine".to_owned());
        }
        output_peak = output_peak.max(peak);
        output_rms_total += f64::from(rms(&buffers));

        let after = &buffers.as_slices()[0][..MAX_BLOCK];
        for (a, b) in before.iter().zip(after.iter()) {
            if (a - b).abs() > 1e-6 {
                changed += 1;
            }
            total += 1;
        }
        last_output.copy_from_slice(after);
    }

    let blocks = node.stats().process_calls();
    let errors = node.stats().process_errors();
    let dominant = spectrum(&last_output, SAMPLE_RATE)
        .dominant()
        .map(|peak| (peak.frequency_hz, peak.magnitude_dbfs));

    // Retire before judging, so a plugin that is rejected still tears down
    // cleanly and the next candidate starts from a clean slate.
    drop(node);
    plugin.collect_retired();

    // The bar: something audible came out. A plugin whose output is at the
    // denormal floor (a wet-only vocal processor with no vocal, say) is working
    // correctly and still proves nothing about the host, so it is passed over.
    if output_peak < 1e-4 {
        return Err(format!(
            "activated and processed cleanly, but its output stayed at {output_peak:.2e} \
             (wet-only or needs a real source)"
        ));
    }

    Ok(EffectRun {
        blocks,
        errors,
        silent_peak,
        input_rms: input_rms_total / 32.0,
        output_rms: output_rms_total / 32.0,
        output_peak,
        changed_fraction: changed as f64 / total.max(1) as f64,
        dominant_hz: dominant,
    })
}

/// (c) A real instrument, played by MYDAW's own note events.
#[test]
#[ignore = "requires installed VST3 plugins; run the hardware suite explicitly and serially"]
fn a_real_instrument_sounds_when_it_is_played() {
    let candidates = scanned_instruments();
    if candidates.is_empty() {
        eprintln!(
            "SKIPPED: no VST3 *instrument* is installed on this machine ({} plugin class(es) \
             scanned in total).",
            installed_plugins().len()
        );
        return;
    }

    let (winner, attempts) = probe_each(
        &candidates,
        Vst3BusLayout::is_instrument,
        std::env::var("MYDAW_VST3_INSTRUMENT").ok().as_deref(),
        |_info, plugin| play_note(plugin),
    );
    print_attempts("\n=== VST3 instrument probe ===", &attempts);

    let Some((info, _plugin, played)) = winner else {
        eprintln!(
            "SKIPPED: none of the {} scanned instrument(s) made a sound. See the table above; \
             a cloud-backed bridge plugin that is silent without its companion service is an \
             expected outcome rather than a host bug.",
            candidates.len()
        );
        return;
    };

    eprintln!("\n=== played '{}' ({}) ===", info.name, info.vendor);
    eprintln!("  blocks processed  : {}", played.blocks);
    eprintln!("  events sent       : {}", played.events_sent);
    eprintln!("  events dropped    : {}", played.events_dropped);
    eprintln!("  peak before note  : {:.6}", played.before_peak);
    eprintln!("  peak while held   : {:.6}", played.held_peak);
    eprintln!("  peak after release: {:.6}", played.tail_peak);
    match played.dominant_hz {
        Some((hz, db)) => {
            eprintln!("  dominant partial  : {hz:.1} Hz @ {db:.1} dBFS  (A440 was requested)")
        }
        None => eprintln!("  dominant partial  : (silent)"),
    }

    assert_eq!(played.events_sent, 2, "one note-on and one note-off");
    assert_eq!(played.events_dropped, 0);
    assert_eq!(played.errors, 0);
    assert!(
        played.held_peak > 1e-4,
        "the probe should not have accepted a silent instrument"
    );
    assert!(
        played.before_peak <= played.held_peak,
        "'{}' was louder before it was played than while it was held",
        info.name
    );
}

/// What one instrument run measured.
struct NoteRun {
    blocks: u64,
    errors: u64,
    events_sent: u64,
    events_dropped: u64,
    before_peak: f32,
    held_peak: f32,
    tail_peak: f32,
    dominant_hz: Option<(f64, f32)>,
}

/// Activate `plugin`, hold A440 for a second, release it, and measure.
fn play_note(plugin: &mut mydaw_plugin_host_vst3::LoadedVst3Plugin) -> Result<NoteRun, String> {
    let mut node = plugin
        .create_node(SAMPLE_RATE, MAX_BLOCK)
        .map_err(|err| err.to_string())?;
    if !node.delivers_notes() {
        drop(node);
        plugin.collect_retired();
        return Err("declares no event input bus, so it can never be played".to_owned());
    }

    let mut buffers = AudioBuffers::new(MAX_BLOCK);

    // Silence before the note: an instrument nobody has played must be quiet,
    // and proving that first is what makes the sound afterwards evidence.
    let mut before_peak = 0.0_f32;
    for _ in 0..8 {
        buffers.silence();
        let mut ctx = ProcessCtx::new(
            &mut buffers,
            SAMPLE_RATE,
            MAX_BLOCK,
            TransportSnapshot::default(),
        );
        node.process(&mut ctx);
        before_peak = before_peak.max(peak_and_finite(&buffers).0);
    }

    // A440 (MIDI key 69), held for about a second, captured whole.
    let mut on = NoteEventBuffer::new(8);
    on.push(NoteEvent::note_on(0, 69, 0.9));
    let blocks = (SAMPLE_RATE as usize).div_ceil(MAX_BLOCK);
    let mut capture: Vec<f32> = Vec::with_capacity(blocks * MAX_BLOCK);
    let mut held_peak = 0.0_f32;

    for block in 0..blocks {
        buffers.silence();
        let slice = if block == 0 { on.as_slice() } else { &[] };
        let mut ctx = ProcessCtx::with_events(
            &mut buffers,
            SAMPLE_RATE,
            MAX_BLOCK,
            TransportSnapshot::default(),
            slice,
        );
        node.process(&mut ctx);
        let (peak, finite) = peak_and_finite(&buffers);
        if !finite {
            drop(node);
            plugin.collect_retired();
            return Err("produced a non-finite sample".to_owned());
        }
        held_peak = held_peak.max(peak);
        capture.extend_from_slice(&buffers.as_slices()[0][..MAX_BLOCK]);
    }

    // Release, then let the tail decay.
    let mut off = NoteEventBuffer::new(8);
    off.push(NoteEvent::note_off(0, 69));
    for block in 0..blocks {
        buffers.silence();
        let slice = if block == 0 { off.as_slice() } else { &[] };
        let mut ctx = ProcessCtx::with_events(
            &mut buffers,
            SAMPLE_RATE,
            MAX_BLOCK,
            TransportSnapshot::default(),
            slice,
        );
        node.process(&mut ctx);
    }
    let tail_peak = peak_and_finite(&buffers).0;

    let run = NoteRun {
        blocks: node.stats().process_calls(),
        errors: node.stats().process_errors(),
        events_sent: node.stats().events_sent(),
        events_dropped: node.stats().events_dropped(),
        before_peak,
        held_peak,
        tail_peak,
        dominant_hz: spectrum(&capture, SAMPLE_RATE)
            .dominant()
            .map(|peak| (peak.frequency_hz, peak.magnitude_dbfs)),
    };

    drop(node);
    plugin.collect_retired();

    if run.held_peak <= 1e-4 {
        return Err(
            "took the note without error but stayed silent (a bridge plugin with no service, \
             or an empty default preset)"
                .to_owned(),
        );
    }
    Ok(run)
}

/// State survives a save/restore round trip on a real plugin.
#[test]
#[ignore = "requires installed VST3 plugins; run the hardware suite explicitly and serially"]
fn state_round_trips_through_a_real_plugin() {
    let candidates = scanned_effects();
    if candidates.is_empty() {
        eprintln!("{}", missing_plugins_message());
        return;
    }

    let (winner, _) = first_workable(
        &candidates,
        Vst3BusLayout::is_stereo_effect,
        std::env::var("MYDAW_VST3_FX").ok().as_deref(),
        SAMPLE_RATE,
        MAX_BLOCK,
    );
    let Some((info, mut plugin, ())) = winner else {
        eprintln!("SKIPPED: no VST3 effect on this machine loads and activates.");
        return;
    };

    let saved = match plugin.save_state() {
        Ok(bytes) => bytes,
        Err(err) => {
            // A plugin entitled to refuse is not a failing host. Reported, not
            // asserted, because a spike cannot demand a stranger implement
            // `getState`.
            eprintln!("  '{}' declined to save its state: {err}", info.name);
            return;
        }
    };
    eprintln!("\n=== state round trip: '{}' ===", info.name);
    eprintln!("  saved blob        : {} bytes", saved.len());

    plugin
        .load_state(&saved)
        .unwrap_or_else(|err| panic!("'{}' refused its own state blob: {err}", info.name));

    let again = plugin
        .save_state()
        .expect("a plugin that saved once must save twice");
    eprintln!("  re-saved blob     : {} bytes", again.len());
    assert_eq!(
        saved.len(),
        again.len(),
        "'{}' produced a different-sized blob after restoring its own state",
        info.name
    );
    assert_eq!(
        saved, again,
        "'{}' did not round-trip its own state byte for byte",
        info.name
    );

    // A blob past the limit is refused rather than handed to the plugin.
    let huge = vec![0_u8; mydaw_plugin_host_vst3::MAX_STATE_BYTES + 1];
    assert!(plugin.load_state(&huge).is_err());
}

/// Teardown after a full activate/process/retire cycle leaves nothing active.
#[test]
#[ignore = "requires installed VST3 plugins; run the hardware suite explicitly and serially"]
fn a_full_cycle_tears_down_cleanly() {
    let candidates = scanned_effects();
    if candidates.is_empty() {
        eprintln!("{}", missing_plugins_message());
        return;
    }

    let (winner, _) = first_workable(
        &candidates,
        Vst3BusLayout::is_stereo_effect,
        std::env::var("MYDAW_VST3_FX").ok().as_deref(),
        SAMPLE_RATE,
        MAX_BLOCK,
    );
    let Some((info, mut plugin, ())) = winner else {
        eprintln!("SKIPPED: no VST3 effect on this machine loads and activates.");
        return;
    };

    // Three activate/retire cycles: a plugin that can only be activated once is
    // a plugin that breaks on the first source swap, and that is exactly the bug
    // the retire channel exists to prevent.
    for round in 0..3 {
        let mut node = plugin
            .create_node(SAMPLE_RATE, MAX_BLOCK)
            .unwrap_or_else(|err| panic!("round {round}: {err}"));
        assert!(plugin.is_active());

        let mut buffers = AudioBuffers::new(MAX_BLOCK);
        buffers.silence();
        let mut ctx = ProcessCtx::new(
            &mut buffers,
            SAMPLE_RATE,
            MAX_BLOCK,
            TransportSnapshot::default(),
        );
        node.process(&mut ctx);

        drop(node);
        assert_eq!(
            plugin.collect_retired(),
            1,
            "round {round}: the node did not come home"
        );
        assert!(
            !plugin.is_active(),
            "round {round}: the plugin is still active after its node retired"
        );
    }

    assert!(plugin.shutdown());
    eprintln!("'{}' survived three activate/retire cycles", info.name);
}
