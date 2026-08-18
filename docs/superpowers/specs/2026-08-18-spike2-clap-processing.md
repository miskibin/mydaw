# Phase 1 Spike 2 — CLAP hosting: processing audio through a real plugin

Date: 2026-08-18
Status: Done. Gate passed.

Spike goal (from the scaffold spec): *"host-clap: load real CLAP plugin, process audio through it."*

## What got built

| Where | What |
| --- | --- |
| `test-fixtures/clap-gain` | `mydaw-fixture-clap-gain` — a real CLAP plugin (`cdylib`, `clack-plugin` 0.1.1). Stereo in/out, `audio-ports` extension, every sample × 0.5 (−6.02 dB). No params, no GUI, no state. |
| `plugins/host-clap/src/hosting.rs` | `LoadedClapPlugin` (load → instantiate → read port layout → activate), `ClapNode: RtNode` (the audio processor inside the graph), `ClapHostError`, `ClapHostRequests`, `ClapNodeStats`. |
| `plugins/host-clap/src/test_util.rs` | Locates the built fixture `cdylib` (`target/<profile>/` **or** `deps/`, hash-suffixed or not, `MYDAW_FIXTURE_CLAP_GAIN` override) and stages it as `mydaw-gain.clap`. |
| `plugins/host-clap/src/lib.rs` | Scan cache (`cached_scan` / `refresh`), `load(path, id)`, `instantiate_by_id(id)`, and the `clap-browser` panel contribution. |
| `app/src/graph.rs` | `tone_graph(factory, effect, …)` → `test-tone -> [CLAP effect] -> TransportGate`; `MYDAW_CLAP_FX=<path>;<id>` parsing and loading. |
| `app/src/commands.rs` | `get_clap_plugins(rescan: bool) -> Vec<ClapPluginInfoDto>`. |
| `ui/` | `clap-browser` panel (name / vendor / version / path, empty state, Rescan), `Backend::getClapPlugins`, mock with two entries. |

Result: a CLAP plugin loaded from disk processes audio inside the same RT graph the audio callback
runs, with **zero allocations** in the process path (measured, not asserted by eye).

## Why the workspace builds its own plugin

Testing a CLAP host against whichever plugins the developer happens to have installed is not a test,
it is a coincidence: the assertions depend on a third party's DSP, the machine and the day. CI has no
plugins at all. So the workspace builds `mydaw-fixture-clap-gain`, whose entire behaviour is
`output = input * 0.5`. `0.5` is exact in binary floating point, so the host-side assertions are
equalities, not tolerances.

`plugins/host-clap` and `app` both take it as a **dev-dependency**. That is not (only) about importing
`PLUGIN_ID` and `GAIN`; it is the dependency edge that makes Cargo build the `cdylib` before the tests
run. Verified: without the edge, `cargo build -p mydaw-plugin-host-clap` produces no `.dll`, because
nothing links a dynamic library. With it, the `cdylib` lands in `target/<profile>/deps/` (a dependency's
`cdylib` is not uplifted to `target/<profile>/`, which is why the locator searches both).

If the fixture is missing anyway, hosting tests **skip with a printed reason** rather than fail. To stop
that from rotting into a permanently-green-because-empty suite, one unit test —
`test_util::the_workspace_fixture_is_findable_from_the_test_binary` — fails loudly instead of skipping.

## Threading and teardown — the decision

CLAP pins instance lifecycle (`init`, `activate`, `deactivate`, `destroy`) to the main thread and only
lets the *audio processor* move to the audio thread. `clack` encodes this: `PluginInstance` is `!Send`,
`StartedPluginAudioProcessor` is `Send`.

MYDAW retires graphs through `basedrop`: the audio thread drops an `Owned<CompiledGraph>`, and the
**collector thread** runs the destructors later. That is right for buffers and wrong for a CLAP plugin —
the collector thread is not the main thread, so a naive `Drop` on `ClapNode` would call `deactivate()`
from the wrong thread. Undefined behaviour, and the kind that shows up as a crash in someone else's
plugin three weeks later.

**Chosen approach (the correct one, not the documented-TODO one):**

```text
 main thread                                  audio thread / collector
 -----------                                  ------------------------
 PluginEntry::load            (!Send)
 PluginInstance::new          (!Send)
 instance.activate()  -> StoppedPluginAudioProcessor (Send)
                                   |
                                   '--> ClapNode ──► start_processing(), process() × N
                                                     │
                       mpsc channel  ◄───────────────'  ClapNode::drop sends the
                                   |                    processor home (no CLAP call)
 stop_processing() + deactivate()  ◄'   LoadedClapPlugin::collect_retired() / shutdown()
```

`ClapNode::drop` makes **no CLAP calls at all**. It sends its `StartedPluginAudioProcessor` back through
an `mpsc` channel to the `LoadedClapPlugin` that created it. The main thread then calls
`stop_processing()` + `deactivate()`. `ClapNode` is therefore safe to drop on the collector thread,
which is exactly what the graph does.

Consequences, all deliberate:

- **The `LoadedClapPlugin` must outlive every `ClapNode` it created.** If it does not, `clack` leaks the
  instance rather than destroying it off-thread (safe, but a leak). `ClapNode::drop` logs a warning when
  the channel is already gone.
- `LoadedClapPlugin` is `!Send`, so it cannot live in `AppState` (Tauri's managed state must be
  `Send + Sync`). It lives in `run()`, on the main thread, next to the kernel — the same reasoning that
  already keeps the kernel out of managed state.
- Shutdown order in `app/src/lib.rs` is: `meters.shutdown()` → `audio.stop()` (drops the stream, which
  retires the graph) → `shutdown_clap()` → `kernel.shutdown()`. `LoadedClapPlugin::shutdown()` waits up
  to `RETIRE_TIMEOUT` (2 s ≈ eight collector sweeps) for the node to come back. Measured on this machine:
  ~160 ms.
- `create_node()` calls `collect_retired()` first, so rebuilding a graph works without the caller having
  to sequence teardown by hand.

## Hostile-input policy

No `unwrap` on anything a plugin controls. Every failure is a `ClapHostError` variant:

- bundle will not load / is not a library → `BundleLoad`
- no `clap_plugin_factory` → `NoPluginFactory`
- bundle has no plugin with that id → `PluginNotFound`
- id with an interior NUL (a project file or a text field can produce one) → `InvalidPluginId`
- `create_plugin`/`init` refused → `Instantiate`
- sample rate ≤ 0, NaN, ∞, or block size 0 → `InvalidAudioConfiguration`, checked **by us** because
  `clack`'s `PluginAudioConfiguration::validate()` *panics* on a zero block size
- not stereo-in/stereo-out, or no `audio-ports` extension → `UnsupportedPortLayout`
- `activate`/`start_processing` refused → `Activate` / `StartProcessing`
- plugin panicked through the FFI boundary → `Panicked { stage }` (via `catch_unwind` around load,
  instantiate, port query, activate and start)

A `clap_plugin->process()` that reports failure is counted (`ClapNodeStats::process_errors`) and the node
**bypasses** for that block — the bus keeps the input rather than whatever the plugin left in its output
buffers. A hard crash (segfault, `abort`) is still fatal in-process; out-of-process scanning remains the
Phase 2 TODO already documented in the crate.

Port negotiation is strict on purpose: a plugin that does not implement `audio-ports` has, per the CLAP
specification, *no audio ports*, so handing it buffers would be a host bug. It is refused with a clear
message instead.

## RT safety

`ClapNode::process` allocates nothing. That is measured by `plugins/host-clap/tests/rt_alloc.rs`, which
installs a counting `GlobalAlloc` armed per-thread and asserts **0** allocator events across 64 blocks of
varying size — and separately across 32 blocks driven through the real `RtGraphSlot::poll_swap` +
`process` path, which is what the audio callback actually calls. The test first proves the instrument
works (a `Vec::with_capacity` must be counted, pure arithmetic must not), because a broken counter would
silently pass everything.

Everything the plugin needs is allocated in `create_node`: `clack`'s `AudioPorts` descriptor tables
(`with_capacity(2, 1)`, so the per-block `with_*_buffers` calls only refill a `Vec` that never grows) and
two stereo scratch buffers. CLAP wants distinct input and output buffers while the graph bus is a single
in-place buffer, so the node owns both sides and copies in/out — two memcpys per block, no allocation.

Events are `InputEvents::empty()` / `OutputEvents::void()`; `steady_time` is the node's own monotonic
frame counter (the transport's `sample_pos` would violate CLAP's "must increase by at least the block
size" rule while stopped); `transport` is `None`.

## Verified on this machine

Windows 11, Focusrite USB Audio @ 44 100 Hz, 2 ch, `--features custom-protocol` over the built UI.

| Check | Result |
| --- | --- |
| Kernel boots with the CLAP effect loaded | `CLAP effect loaded: 'MYDAW Fixture Gain' (dev.mydaw.fixture.gain)` |
| CLAP Plugins panel, nothing installed | Empty state: "No CLAP plugins found…" — this machine has no CLAP plugins in the standard locations |
| CLAP Plugins panel, with `CLAP_PATH` set | Lists `MYDAW Fixture Gain / MYDAW / 0.1.0` + bundle path, "1 plugin" |
| Play, tone only | Meter peak 0.200 (−14 dBFS) |
| Play, fixture in the chain | Meter peak 0.1000 (−20 dBFS); audibly quieter; meter bar 12 px lower on a 2.045 px/dB scale = −5.9 dB, i.e. −6.02 within a pixel |
| Stop | Meters fall to silence, playhead holds |
| Close window | Process exits; `CLAP effect 'dev.mydaw.fixture.gain' deactivated` |

Gates: `cargo build --workspace`, `cargo test --workspace` (**160 passed, 0 failed, 1 ignored** — was 115),
`cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --check`, `pnpm --dir ui build`.

## Known limitations

- **No parameters.** No `params` extension, no automation, no parameter events. The fixture has none on
  purpose so the tests stay deterministic.
- **No events at all.** Empty input events, void output events — no MIDI, no notes, no transport event.
  Instruments are therefore untestable through this path today.
- **No plugin GUI.** That is spike 3 (`plugins/plugin-windows`).
- **Stereo in / stereo out only.** Mono, multi-out and side-chain ports are refused with
  `UnsupportedPortLayout`; real routing is the Phase 2 mixer.
- **One node per loaded plugin.** CLAP allows one activation at a time, so a second `create_node` while
  a node is live returns `Activate`. Multiple instances mean multiple `LoadedClapPlugin`s.
- **`request_restart` / `request_process` / `request_callback` are latched, not acted on.** The atomics
  are recorded (`ClapHostRequests`) but nothing polls them yet; a plugin asking to be reactivated is
  currently ignored.
- **Insertion is an environment variable.** `MYDAW_CLAP_FX` is a spike affordance. The browser panel
  lists plugins; it cannot yet insert one.
- **No process-level crash isolation.** Unchanged from Phase 0, and unchanged by this spike.
- **`ProcessStatus::Sleep` is counted, not honoured.** The node keeps calling `process()`, which is always
  legal but wastes CPU on a plugin that wanted to idle.

## Pointers for the next spikes

- **Spike 3 (plugin GUI):** `LoadedClapPlugin` already owns the main-thread instance and has
  `requests()`; the `gui` extension hangs off `instance.plugin_handle()`, and `request_callback` →
  `instance.call_on_main_thread_callback()` is the idle pump the window will need.
- **Spike 5 (VST3):** the seam to copy is `LoadedClapPlugin` + `ClapNode` + the retire channel, not the
  `clack` types. The teardown story is format-independent: VST3 has the same "lifecycle on one thread,
  processing on another" split, and the same `basedrop` collector problem.
- **Phase 2 parameters:** the `RtCommand` queue needs a parameter variant, and `ClapNode` needs a real
  `EventBuffer` for input events plus a way to drain output events back to the UI. Both fit where the
  empty `InputEvents`/`OutputEvents` are today.
- **Phase 2 graph:** `ClapPortLayout` is the place where multi-port and mono support will land, and
  `create_node`'s stereo check is the single line that has to relax.
