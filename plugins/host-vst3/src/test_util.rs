//! Helpers for tests that need a **real** VST3 plugin.
//!
//! Unlike the CLAP host, this crate ships no fixture plugin: writing one would
//! mean writing a VST3 *plugin* as well as a host, and step 7 is a spike whose
//! whole point is that it works against the plugins a musician actually has
//! installed. So the integration tests are skip-if-absent -- they scan the
//! machine, and if it has nothing they print why and pass.
//!
//! That is a deliberate trade with a real cost: **CI proves nothing about
//! hosting**, only about the pure logic (path resolution, classification, the
//! host-side COM objects, error taxonomy), which is what the unit tests cover.
//! The step 7 spec records which plugins this was actually verified against, and
//! that table is the evidence the skip-if-absent tests cannot be.
//!
//! # Overrides
//! - `MYDAW_VST3_FX` / `MYDAW_VST3_INSTRUMENT`: a substring of the plugin name to
//!   prefer, so a developer chasing one plugin's behaviour need not edit code.
//! - `MYDAW_VST3_PATH`: an extra search root, on top of the standard ones.
//!
//! # The block list is used, and that matters
//!
//! [`installed_plugins`] scans with the user's persistent
//! [`BlockList`](crate::BlockList). Without it, a machine whose collection
//! includes licence-protected plugins has its loader lock wedged by the first of
//! them, and every test here then skips for entirely the wrong reason. Populate
//! it once with `cargo run -p mydaw-plugin-host-vst3 --example scan_report`.

use std::path::PathBuf;
use std::sync::OnceLock;

use crate::hosting::{LoadedVst3Plugin, Vst3BusLayout};
use crate::scan::Vst3PluginInfo;
use crate::Vst3HostService;

/// Every VST3 plugin installed on this machine, scanned at most once per
/// process.
///
/// A scan opens every module, which on a well-stocked machine takes seconds; a
/// test binary with several tests in it must not pay that more than once.
pub fn installed_plugins() -> &'static [Vst3PluginInfo] {
    static SCANNED: OnceLock<Vec<Vst3PluginInfo>> = OnceLock::new();
    SCANNED.get_or_init(|| {
        let mut roots = crate::scan::standard_search_paths();
        if let Some(extra) = std::env::var_os("MYDAW_VST3_PATH") {
            roots.extend(std::env::split_paths(&extra));
        }
        // The *persistent* block list, deliberately: see the module docs.
        Vst3HostService::with_search_paths_and_blocklist(roots, crate::BlockList::load()).scan()
    })
}

/// The sentence a skipped test prints.
pub fn missing_plugins_message() -> String {
    format!(
        "SKIPPED: no VST3 plugins were found on this machine. Looked in: {}. \
         Set MYDAW_VST3_PATH to add a search root. The hosting path is proven by hand against \
         real plugins; see docs/superpowers/specs/2026-08-19-step7-vst3-host.md.",
        crate::scan::standard_search_paths()
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join("; ")
    )
}

/// One attempt to bring a plugin up, and what happened.
///
/// Collected rather than logged so a test can print a compatibility table --
/// which, for this spike, *is* the deliverable.
#[derive(Debug)]
pub struct LoadAttempt {
    pub name: String,
    pub vendor: String,
    pub subcategories: String,
    pub path: PathBuf,
    /// `Ok` with the layout when the plugin loaded *and* activated.
    pub outcome: Result<Vst3BusLayout, String>,
}

impl LoadAttempt {
    pub fn succeeded(&self) -> bool {
        self.outcome.is_ok()
    }

    /// One line for a compatibility table.
    pub fn row(&self) -> String {
        match &self.outcome {
            Ok(layout) => format!(
                "  OK   {:<34} {:<18} in {}x{} out {}x{} ev {}  [{}]",
                truncate(&self.name, 34),
                truncate(&self.vendor, 18),
                layout.audio_input_buses,
                layout.main_input_channels,
                layout.audio_output_buses,
                layout.main_output_channels,
                layout.event_input_buses,
                self.subcategories
            ),
            Err(why) => format!(
                "  FAIL {:<34} {:<18} {}",
                truncate(&self.name, 34),
                truncate(&self.vendor, 18),
                why
            ),
        }
    }
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        text.to_owned()
    } else {
        text.chars()
            .take(width.saturating_sub(1))
            .collect::<String>()
            + "…"
    }
}

/// How many plugins one probe will try before giving up.
///
/// Bounded because each attempt runs a vendor's initialisation: on a machine
/// with thirty plugins installed, an unbounded probe is a several-minute test.
pub const MAX_PROBE_ATTEMPTS: usize = 12;

/// Try candidates until `probe` is satisfied, recording every attempt.
///
/// The two-stage shape matters, and it is what the first version of these tests
/// got wrong. "The first plugin that loads and activates" is *not* the same as
/// "a plugin that demonstrates the host works": on this machine the first
/// instrument to activate is a bridge to a cloud service that is silent without
/// it, and the first effect to activate outputs its wet signal only. Both are
/// working plugins and neither proves anything.
///
/// So `accepts` filters on the bus layout (can this plugin go in this slot at
/// all) and `probe` decides whether the plugin actually *demonstrated* something
/// -- by returning `Err` with the reason when it did not, which lands in the
/// table like any other failure.
pub fn probe_each<T>(
    candidates: &[Vst3PluginInfo],
    accepts: fn(&Vst3BusLayout) -> bool,
    prefer: Option<&str>,
    mut probe: impl FnMut(&Vst3PluginInfo, &mut LoadedVst3Plugin) -> Result<T, String>,
) -> (
    Option<(Vst3PluginInfo, LoadedVst3Plugin, T)>,
    Vec<LoadAttempt>,
) {
    let mut ordered: Vec<&Vst3PluginInfo> = candidates.iter().collect();
    if let Some(prefer) = prefer {
        let needle = prefer.to_ascii_lowercase();
        ordered.sort_by_key(|info| !info.name.to_ascii_lowercase().contains(&needle));
    }

    let mut attempts = Vec::new();
    let mut winner = None;
    for info in ordered {
        if winner.is_some() || attempts.len() >= MAX_PROBE_ATTEMPTS {
            break;
        }
        let outcome = match LoadedVst3Plugin::load(&info.path, info.cid) {
            Ok(mut plugin) => {
                let layout = plugin.bus_layout();
                if !accepts(&layout) {
                    Err(format!(
                        "bus layout is not what this slot needs (in {}x{}, out {}x{})",
                        layout.audio_input_buses,
                        layout.main_input_channels,
                        layout.audio_output_buses,
                        layout.main_output_channels
                    ))
                } else {
                    match probe(info, &mut plugin) {
                        Ok(value) => {
                            // The plugin comes back with the winner: the state
                            // and teardown tests need the *same* live instance,
                            // not a second load of the same file.
                            winner = Some((info.clone(), plugin, value));
                            Ok(layout)
                        }
                        Err(why) => Err(why),
                    }
                }
            }
            Err(err) => Err(err.to_string()),
        };
        attempts.push(LoadAttempt {
            name: info.name.clone(),
            vendor: info.vendor.clone(),
            subcategories: info.subcategories.clone(),
            path: info.path.clone(),
            outcome,
        });
    }

    (winner, attempts)
}

/// Try every candidate until one loads *and* activates, recording each attempt.
///
/// [`probe_each`] with the weakest possible probe: activate once and retire.
/// Enough for the teardown and state tests, which are about the host rather than
/// about what the plugin sounds like.
pub fn first_workable(
    candidates: &[Vst3PluginInfo],
    accepts: fn(&Vst3BusLayout) -> bool,
    prefer: Option<&str>,
    sample_rate: f64,
    max_block: usize,
) -> (
    Option<(Vst3PluginInfo, LoadedVst3Plugin, ())>,
    Vec<LoadAttempt>,
) {
    probe_each(candidates, accepts, prefer, |_info, plugin| {
        // Activating and immediately retiring is the only way to know a plugin
        // really works: plenty load and then refuse `setActive`.
        let node = plugin
            .create_node(sample_rate, max_block)
            .map_err(|err| err.to_string())?;
        drop(node);
        plugin.collect_retired();
        Ok(())
    })
}

/// Every scanned plugin whose class claims to be an audio effect.
pub fn scanned_effects() -> Vec<Vst3PluginInfo> {
    installed_plugins()
        .iter()
        .filter(|info| info.is_audio_effect())
        .cloned()
        .collect()
}

/// Every scanned plugin whose class claims to be an instrument.
pub fn scanned_instruments() -> Vec<Vst3PluginInfo> {
    installed_plugins()
        .iter()
        .filter(|info| info.is_instrument())
        .cloned()
        .collect()
}

/// Print a compatibility table for a run of attempts.
pub fn print_attempts(heading: &str, attempts: &[LoadAttempt]) {
    eprintln!("{heading} ({} attempted)", attempts.len());
    for attempt in attempts {
        eprintln!("{}", attempt.row());
    }
}

/// A sine at `hz`, written into both channels at `amplitude`.
///
/// The test signal for "did this effect do anything": a pure tone in means any
/// change out is the plugin's doing.
pub fn fill_sine(
    buffers: &mut mydaw_rt_core::AudioBuffers,
    hz: f64,
    sample_rate: f64,
    phase: &mut f64,
) {
    let step = std::f64::consts::TAU * hz / sample_rate;
    let frames = buffers.frames();
    for frame in 0..frames {
        let value = (*phase).sin() as f32 * 0.5;
        for channel in buffers.as_mut_slices() {
            channel[frame] = value;
        }
        *phase += step;
    }
}

/// Peak absolute sample, and whether every sample is finite.
pub fn peak_and_finite(buffers: &mydaw_rt_core::AudioBuffers) -> (f32, bool) {
    let mut peak = 0.0_f32;
    let mut finite = true;
    for channel in buffers.as_slices() {
        for sample in &channel[..buffers.frames()] {
            if !sample.is_finite() {
                finite = false;
            } else {
                peak = peak.max(sample.abs());
            }
        }
    }
    (peak, finite)
}

/// Root mean square of both channels together.
pub fn rms(buffers: &mydaw_rt_core::AudioBuffers) -> f32 {
    let mut sum = 0.0_f64;
    let mut count = 0_usize;
    for channel in buffers.as_slices() {
        for sample in &channel[..buffers.frames()] {
            if sample.is_finite() {
                sum += f64::from(*sample) * f64::from(*sample);
            }
            count += 1;
        }
    }
    if count == 0 {
        0.0
    } else {
        (sum / count as f64).sqrt() as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_name_is_truncated_with_an_ellipsis_rather_than_breaking_the_table() {
        assert_eq!(truncate("short", 10), "short");
        let long = truncate("a name far longer than the column", 10);
        assert_eq!(long.chars().count(), 10);
        assert!(long.ends_with('…'));
    }

    #[test]
    fn the_skip_message_names_where_it_looked() {
        let message = missing_plugins_message();
        assert!(message.starts_with("SKIPPED:"));
        assert!(message.contains("VST3"), "{message}");
    }

    #[test]
    fn a_sine_fills_both_channels_and_measures_as_expected() {
        let mut buffers = mydaw_rt_core::AudioBuffers::new(4_800);
        let mut phase = 0.0;
        fill_sine(&mut buffers, 440.0, 48_000.0, &mut phase);

        let (peak, finite) = peak_and_finite(&buffers);
        assert!(finite);
        assert!((peak - 0.5).abs() < 0.01, "peak {peak}");
        // A full-scale-0.5 sine has an RMS of 0.5/sqrt(2).
        let measured = rms(&buffers);
        assert!((measured - 0.3535).abs() < 0.01, "rms {measured}");
    }

    #[test]
    fn silence_measures_as_silent_and_finite() {
        let mut buffers = mydaw_rt_core::AudioBuffers::new(64);
        buffers.silence();
        assert_eq!(peak_and_finite(&buffers), (0.0, true));
        assert_eq!(rms(&buffers), 0.0);
    }
}
