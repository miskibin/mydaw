# Step 7 — Minimal VST3 host

**Status:** spike, landed. **Scope:** prove scan → load → process → notes → GUI
against the plugins actually installed on a real machine. **Not** parity with the
CLAP path; see [Known gaps](#known-gaps).

VST3 was the last unproven pillar in the Phase 1 risk order. CLAP is MYDAW's
primary format and `mydaw-plugin-host-clap` is the reference implementation; this
step builds the compatibility half over a much older API, deliberately mirroring
that architecture so the two read side by side.

| | CLAP | VST3 |
| --- | --- | --- |
| discovery service | `ClapHostService` | `Vst3HostService` |
| one instance | `LoadedClapPlugin` | `LoadedVst3Plugin` |
| in the graph | `ClapNode` | `Vst3Node` |
| its editor | `ClapGuiSession` | `Vst3GuiSession` |
| many at once | `ClapInstanceHost` | `Vst3InstanceHost` |
| main-thread placement | `ClapHost` | `Vst3Host` |
| command mailbox | `ClapHostBridge` | `Vst3HostBridge` |

Where they differ, the difference is the format's rather than a choice. VST3 has
no safe host binding, so `module` and `host` exist to build one, and every COM
call carries the invariant it relies on next to it.

---

## What the machine was, and what it taught

The development machine has **43 VST3 modules** installed (Antares AVOX and
Auto-Tune suites, Arturia Analog Lab V, Xfer Serum, ACE Studio Bridge), a large
fraction of them PACE/iLok protected. That turned out to matter far more than any
API detail, and two findings changed the design.

### 1. A licence dialog is not a failure, it is a hang

An unlicensed PACE plugin does not return an error from `LoadLibraryW`. It puts up
a modal **"Software Activation"** dialog in a separate process and never returns.
`catch_unwind` sees nothing: nothing panicked, the call simply has not come back.

So `scan::scan_module` gives every module its own worker thread and a
`SCAN_TIMEOUT` (10 s) deadline, and reports `Vst3ScanError::TimedOut` past it.
That leaks the worker thread and the module it wedged — taken deliberately,
because killing a thread part-way through third-party code leaves locks held and
is strictly worse.

### 2. One wedged module poisons the whole process

The important one, and it is not in any documentation we found. A module that
blocks **inside `DllMain`** holds the Windows **loader lock**, which is
per-process. Every subsequent `LoadLibraryW` — for any DLL, from any thread —
then blocks behind it forever.

Measured, on this machine:

| scan mode | wall clock | plugin classes found |
| --- | --- | --- |
| in-process, no block list | 420 s | **1** of 29 |
| one child process per module | 234 s | **29** |
| in-process, block list populated | **3.3 s** | 29 |

A timeout alone is not enough: the first hang made 42 healthy modules look
broken. Three things follow, and all three are implemented:

- **`Vst3HostService::scan_detailed` stops at the first timeout** and reports the
  rest as "not attempted" (`Vst3ScanReport::aborted_at`). Carrying on would have
  produced a confidently wrong plugin list.
- **A persistent block list** (`blocklist.rs`,
  `%LOCALAPPDATA%\MYDAW\vst3-blocklist.txt`) records the culprit, so the next
  scan resumes past it. A user gets through their collection one restart per bad
  plugin instead of never — and after the first pass, scanning is 3 seconds.
- **`examples/scan_report.rs` runs one child process per module by default**,
  which makes the problem impossible: a poisoned loader lock dies with the child.
  This is a working prototype of the phase-2 out-of-process scanner, and
  `--in-process` reproduces exactly what the shipping app does today, so the cost
  of not having it is a number rather than an argument.

### 3. Modules are never unloaded

An in-process scan that called `ExitDll` + `FreeLibrary` after each module
crashed with an access violation at the eleventh: `AVOX ARTICULATOR` and
`AVOX ASPIRE` are separate `.vst3` files sharing an Antares runtime, and freeing
the first tore down state the second then used. Either one alone is fine, which is
what makes this class of bug so easy to ship.

`module::should_unload_modules()` is therefore **`false` by default**
(`MYDAW_VST3_UNLOAD_MODULES=1` overrides, for bug reports). This matches the VST3
SDK's own module wrapper, which caches modules per path for the process lifetime.
The cost is address space — file-backed mappings the OS can page out; the
alternative cost is a crashed DAW.

---

## The VST3 subset implemented

### Discovery (`scan.rs`, `module.rs`)

- Search roots: `C:\Program Files\Common Files\VST3`, `%COMMONPROGRAMFILES%\VST3`,
  `%LOCALAPPDATA%\Programs\Common\VST3`, plus `%VST3_PATH%`. Deduplicated.
- Both module layouts: a flat `.vst3` DLL, and a bundle directory whose binary
  lives at `Contents/x86_64-win/`. A bundle whose binary is not named after it
  also resolves (legal, and unit-tested).
- Entry-point protocol: `LoadLibraryW` → `InitDll` (optional) → `GetPluginFactory`.
- Classes are enumerated through `IPluginFactory2::getClassInfo2` where available
  — it is the only interface carrying vendor, version and `subCategories` — and
  through `IPluginFactory::getClassInfo` otherwise. Only `"Audio Module Class"`
  entries are reported.
- Classification reads `subCategories`: `Instrument` → instrument, `Fx` → effect,
  neither → `other`, **none declared at all** → `unknown`. `unknown` has no CLAP
  equivalent and is a real state (a module with no `IPluginFactory2`); such a
  plugin is offered as an effect and the bus-layout check at load decides.

### Hosting (`hosting.rs`)

Instantiation drives both plugin shapes:

```
factory->setHostContext(host)                 [IPluginFactory3 only]
factory->createInstance(cid, IComponent)
component->initialize(host context)
component->getControllerClassId(&ccid)
    ccid == cid  -> single-component: query IEditController off the component
    otherwise    -> dual-component:   createInstance + initialize + connect
                                      the two IConnectionPoints
controller->setComponentHandler(host)
component->getState(stream); controller->setComponentState(stream)
component -> queryInterface(IAudioProcessor)
```

The `getState`/`setComponentState` step is what makes an editor open showing the
processor's real values rather than class defaults.

Activation follows the order VST3 requires: `setBusArrangements` (stereo) →
`setupProcessing` (32-bit float, rate, max block) → `activateBus` for audio
in/out and event in → `setActive(true)` → `setProcessing(true)`.

### The RT node

`Vst3Node` implements `RtNode`. Everything it needs is allocated at activation:
two stereo scratch buffers, the channel-pointer arrays, a `HostEventList`
pre-grown to `MAX_BLOCK_EVENTS`, and a `NoParameterChanges`. Per block it rewrites
two pointer arrays, builds `ProcessData`/`AudioBusBuffers`/`ProcessContext` on the
stack, copies samples and calls `process`.

**Measured zero-allocation** against real plugins (`tests/rt_alloc.rs`, counting
global allocator, 64 blocks of varying size): 0 allocator events for the audio
path and 0 for the note path.

Notes become VST3 `Event`s with `noteId = -1` (match by pitch — MYDAW tracks no
note ids yet). The `NoteOnEvent` and `NoteOffEvent` union arms have *different
field orders*, so writing the wrong one would silently put the pitch where the
velocity belongs; there is a test for exactly that.

`ProcessContext` marks valid only what MYDAW actually knows — tempo, continuous
time, time signature, playing — and deliberately never claims `kBarPositionValid`
or `kSmpteValid`, because a plugin syncing to a field the host left at zero is
worse off than one told nothing.

### Host-side COM objects (`host.rs`)

MYDAW implements five interfaces *for* the plugin, using the `vst3` crate's
`Class` + `ComWrapper` (no macros; plain trait impls, `&self` methods, interior
mutability):

| object | given to | why it cannot be null |
| --- | --- | --- |
| `Vst3HostContext` | `initialize` on both halves | plugins ask for the host's name and many refuse without one |
| `HostEventList` | `ProcessData::inputEvents` | the only way to play an instrument |
| `NoParameterChanges` | `ProcessData::inputParameterChanges` | nominally optional, and shipping plugins dereference it |
| `PlugFrame` | `IPlugView::setFrame` | how an editor asks for a size |
| `MemoryStream` | `getState` / `setState` | there is no other way to move a state blob |

`HostEventList` is the one place `Send`/`Sync` is asserted by hand; the safety
note is on the type.

The loading thread also enters an **apartment-threaded COM apartment**
(`module::ComApartment`), which every real host does and which a great deal of
plugin init code assumes.

### GUI (`gui.rs`)

`createView("editor")` → `isPlatformTypeSupported(HWND)` → `getSize` → open the
window hidden → `setContentScaleFactor` → **`setFrame` before `attached`** →
`attached(HWND)` → `getSize` again → size and show.

Resize is a negotiation in both directions. VST3 has no `adjust_size`; it has
`checkSizeConstraint`, which the plugin **edits in place**, so the host writes a
rect, calls, and reads it back. Plugin-initiated resizes arrive on `IPlugFrame`
from any thread, are latched in `Vst3GuiRequests`, and applied on the main thread.

### Threading model

Identical discipline to the CLAP host:

```
main thread (tao's loop)                      audio thread
------------------------                      ------------
Vst3Module::open, createInstance   (!Send)
initialize / bus setup / setActive
create_node -> Vst3Node            (Send) ------> process() x N
                                                       |
LoadedVst3Plugin::collect_retired  <--- retire channel -'
  setProcessing(false); setActive(false)
```

`Vst3Node` deactivates nothing on drop — it sends its `ComPtr<IAudioProcessor>`
back through a channel, because `basedrop`'s collector thread is the wrong place
to tear a plugin down. If a node never comes home, the module is **leaked rather
than unloaded** under a live COM object.

Field declaration order *is* the teardown protocol: every type holding both a COM
pointer and a module declares the pointer first and the module last, so releasing
after `FreeLibrary` is unrepresentable rather than merely avoided.

---

## Per-plugin results on this machine

Scanned with `cargo run -p mydaw-plugin-host-vst3 --example scan_report`
(isolated mode). **29 plugin classes opened, 14 modules hung.**

### Opened and classified

| plugin | vendor | kind | subCategories |
| --- | --- | --- | --- |
| ACE Bridge 2 | ACE Studio | instrument | `Instrument\|Synth` |
| Analog Lab V | *(none declared)* | instrument | `Instrument\|Synth` |
| Auto-Tune Slice | Antares | instrument | `Instrument\|Synth` |
| Serum | Xfer Records | instrument | `Instrument\|Synth` |
| Auto-Tune Pro | Antares | audio-effect | `Fx\|Pitch Shift` |
| Auto-Tune EFX+ | Antares | audio-effect | `Fx\|Pitch Shift` |
| Auto-Tune Vocal EQ | Antares | audio-effect | `Fx\|Dynamics\|EQ` |
| Auto-Tune Vocal Compressor | Antares | audio-effect | `Fx\|Dynamics` |
| Vocal Reverb | Antares | audio-effect | `Fx\|Reverb` |
| AVOX WARM | Antares | audio-effect | `Fx\|Delay` |
| AVOX ARTICULATOR / ASPIRE / CHOIR / DUO / MUTATOR / PUNCH / SYBIL / THROAT | Antares | audio-effect | `Fx` |
| Auto-Key 2, Auto-Tune SoundSoap, Auto-Tune Vocodist, Harmony Engine, Mic Mod | Antares | audio-effect | `Fx` |

Note that the same plugin is installed twice on this machine — once in
`VST3\` and once in `VST3\Antares\` — and the two copies behave differently: the
`Antares\` builds open, several of the root-level ones hang. The scanner reports
both, keyed by `(module path, class id)`.

### Hung — expected, not host bugs

All 14 are PACE/iLok protected and blocked on an activation dialog. Block-listed
after the first encounter:

`AVOX ARTICULATOR`, `AVOX CHOIR`, `AVOX THROAT`, `AVOX WARM`, `Auto-Key`,
`Auto-Tune Access`, `Auto-Tune Artist`, `Auto-Tune Pro`, `Auto-Tune Vocal
Compressor`, `Auto-Tune Vocal EQ`, `Auto-Tune Vocodist`, `Mic Mod`,
`Vocal De-Esser`, `Vocal Reverb` — all the root-level `VST3\` copies.

### Hosted end-to-end

| what | plugin | result |
| --- | --- | --- |
| **effect processes audio** | AVOX ASPIRE (Antares) | 64 blocks, 0 process errors. Sine in RMS 0.353455 → out RMS 0.352161, peak 0.500000, dominant bin 438.5 Hz. 100% of samples altered. Silence in → silence out. |
| **instrument plays notes** | **Analog Lab V (Arturia)** | note-on A440 (key 69) from MYDAW's own `NoteEvent` path → **dominant partial 438.8 Hz @ −26.4 dBFS**, peak 0.132 while held, 0.000117 before. 2 events sent, 0 dropped, 0 errors. |
| **state round trip** | AVOX ASPIRE | 881-byte blob, byte-identical after save → load → save |
| **teardown** | AVOX ASPIRE | 3 activate/retire cycles, clean deactivation each time |
| **zero-alloc** | AVOX ASPIRE + ACE Bridge 2 | 0 allocator events over 64 blocks, audio and note paths |
| **editor** | Analog Lab V | window `0x5e07c6`, child HWND `0x170148` embedded, negotiated 1280×886, **plugin-initiated resize to 1280×846 observed** through `IPlugFrame`, closed cleanly |
| **editor** | ACE Bridge 2 | child HWND embedded at 394×104, closed cleanly |

Probing matters here: the *first* plugin that activates is not the same as one
that demonstrates anything. ACE Bridge 2 is a bridge to a cloud service and is
correctly silent without it; several AVOX processors are wet-only and output
~5e-13 with no vocal present. `test_util::probe_each` walks candidates until one
actually produces output, and records why each was passed over.

**Screenshot:** `vst3-editor-analoglabv-window.png` in the run scratchpad — the
MYDAW-titled host window at Analog Lab V's negotiated size. The plugin's own
child window renders through the GPU and `PrintWindow` cannot capture it
headlessly; the *embedding* is asserted programmatically instead
(`GetWindow(GW_CHILD)` in `tests/hardware_gui.rs`), which is the stronger check.

---

## App integration

- `Vst3HostBridge` — a sibling of `ClapHostBridge`, not a generalisation. Same
  shape so merging them later stays contained.
- Commands: `get_vst3_plugins(rescan)`, `get_vst3_master_effects`,
  `set_track_source_vst3(track, modulePath, cid)`,
  `set_master_effect_vst3(modulePath, cid)`, `open_vst3_gui`, `close_vst3_gui`,
  `save_vst3_state`, `load_vst3_state`.
- `Source::Vst3(Vst3SourceRef)` with wire id `vst3:<class id hex>`. The prefix
  differs from `clap:` so a project file records *which format* a track used —
  most vendors ship the same plugin in both.
- UI: a `vst3-browser` panel (kind badges including `Unclassified`), `vst3` slots
  on `TrackDto`/`MasterDto`, VST3 instruments in the source list, and full mock
  backend coverage so the panel is developable in a plain browser.

### The master insert holds one plugin, of either format

`TrackHost::master_insert()` is a single slot. Installing a VST3 node displaces a
CLAP node — but the *node* going away is not the *instance* going away, and
stopping there would leave a CLAP plugin loaded, activated, invisible and
un-freeable with the mixer still drawing it.

So eviction is explicit and symmetric. `Vst3Host::place_master_effect` takes
`&mut ClapHost` and calls `ClapHost::evict_master`; the reverse direction is
wired in the run loop, which is the only place that knows both hosts exist.

### Layout edits rebuild once and re-attach both

A rebuild installs a plugin-free graph, then each host re-activates its instances
into it. Only one host can own that, so `AppState::add_track`/`remove_track` route
to the VST3 host when any VST3 plugin is placed (it re-attaches both) and to the
CLAP host otherwise — preserving step 6's behaviour exactly when no VST3 plugin is
loaded.

---

## Known gaps

Ordered by how much they would cost a user.

1. **No parameters.** `IEditController`'s parameter list is never read and
   `ProcessData::inputParameterChanges` is always an empty `NoParameterChanges`.
   Nothing can be automated, and a project file saves only the opaque state blob.
   The CLAP path has the same gap, so this is a *parity* item rather than a
   VST3-specific one.
2. **No `IConnectionPoint` messaging.** `Vst3HostContext::createInstance` returns
   `kNotImplemented`, so MYDAW provides no `IMessage`/`IAttributeList`. Legal, and
   every plugin must tolerate it, but a dual-component plugin that relies on
   messaging will have an editor that does not follow its processor. Contained
   work: implement two more COM classes.
3. **Component state only.** `save_state` writes `IComponent::getState`; a
   complete VST3 preset also carries `IEditController::getState` (editor-only
   settings such as a scroll position).
4. **In-process scanning.** The block list and the abort-on-hang make it usable;
   a subprocess scanner makes it correct. `scan::scan_module_in_process` is
   already the pure path-in/results-out function such a child would call, and
   `examples/scan_report.rs` already drives it that way.
5. **Modules are never unloaded** (see above). A user who uninstalls a plugin
   mid-session keeps its DLL mapped until restart.
6. **Stereo only.** `Vst3BusLayout::is_supported` accepts stereo-in/stereo-out or
   no-input/stereo-out. Multi-out instruments (Analog Lab V declares 1 output bus;
   ACE Bridge 2 declares 3) are driven through bus 0 only.
7. **`restartComponent` is recorded, not acted on.** Flags accumulate in
   `Vst3HostRequests`; a plugin asking to be re-activated is not.
8. **No CI coverage of hosting.** There is no VST3 fixture plugin, so the
   integration tests are skip-if-absent. CI proves the pure logic — path
   resolution, classification, the host-side COM objects, error taxonomy — and
   this document is the evidence for the rest.
9. **Sample-accurate note timing is bounded by `MAX_BLOCK_EVENTS`**; excess events
   in one block are dropped and counted rather than allocated for.

## CLAP-vs-VST3 parity roadmap

| capability | CLAP | VST3 | next step |
| --- | --- | --- | --- |
| scan + cache | yes | yes (+ block list) | share the block list with CLAP |
| instrument on a track | yes | yes | — |
| effect on the master insert | yes | yes | more insert slots (Phase 2 mixer) |
| editor window | yes | yes | — |
| state save/load | yes | component only | add controller state |
| parameters / automation | no | no | one design, both hosts |
| plugin-requested restart | recorded | recorded | act on it in the shell |
| timers / main-thread callbacks | `clap.timer-support` | n/a (VST3 has no equivalent) | — |
| out-of-process scan | TODO | TODO (prototyped) | promote `scan_report --module` to `mydaw --scan` |

The two hosts are close enough that a shared `PluginHost` trait is now a
mechanical refactor rather than a speculative abstraction — which is the outcome
that makes this spike worth having done second rather than first.

---

## Gates

- `cargo test --workspace`: **817 passed, 0 failed** (baseline before this step:
  504).
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo fmt --all --check`: clean.
- `pnpm --dir ui build`: clean.
- Hardware tests (`--test hardware`, `--test hardware_gui`, `--test rt_alloc`):
  green on this machine, skip-if-absent elsewhere.

Run the hardware set with:

```text
cargo run  -p mydaw-plugin-host-vst3 --example scan_report          # populate the block list once
cargo test -p mydaw-plugin-host-vst3 --test hardware     -- --nocapture --test-threads=1
cargo test -p mydaw-plugin-host-vst3 --test hardware_gui -- --nocapture --test-threads=1
cargo test -p mydaw-plugin-host-vst3 --test rt_alloc     -- --nocapture --test-threads=1
```
