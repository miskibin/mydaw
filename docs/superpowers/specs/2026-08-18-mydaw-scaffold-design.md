# MYDAW — Microkernel DAW: Scaffold & Architecture Design

Date: 2026-08-18
Status: Approved (user)

## Goal

FL Studio-style DAW. Rust engine + Tauri v2 + SolidJS/TS UI. CLAP hosting first, VST3 later.
Core principle: **everything is a plugin** (VSCode/harness style) over a tiny microkernel.

## Decisions (locked)

| Decision | Choice |
|---|---|
| Plugin depth | Full harness — microkernel + ALL features as plugins (transport, mixer, hosts, UI panels) |
| Plugin linking | Static, compile-time. Plugin = crate in workspace, explicit registration in `app/main.rs`. External CLAP/VST3 load dynamically as audio plugins only |
| Kernel model | Typed service registry + pub/sub event bus (non-RT) + RT audio graph behind lock-free wall |
| MVP scope | Phase 0 scaffold + Phase 1 = 5 risk spikes. No sequencing features until spikes pass |
| UI stack | SolidJS + TS + Vite; Canvas/WebGL renderers for heavy views (later phases) |
| Platform | Windows-first (WASAPI + WebView2). Code stays cross-platform-clean (cpal, no gratuitous Win32) |

## Workspace layout

```
D:\MYDAW\
├── Cargo.toml                 # workspace
├── crates/
│   ├── kernel/                # microkernel: Plugin trait, registry, extension points, typed event bus, lifecycle
│   ├── rt-core/               # RT primitives: AudioGraph, RtNode trait, rtrb command queue, arc-swap graph publish, basedrop collector, transport clock
│   └── ipc-bridge/            # kernel <-> webview: panel manifest, commands, throttled meter channel
├── plugins/
│   ├── audio-io/              # cpal WASAPI device service + RT thread (audio_thread_priority)
│   ├── host-clap/             # clack-host wrapper -> RtNode
│   ├── host-vst3/             # vst3 0.3 bindings wrapper -> RtNode (later phase, stub now)
│   ├── plugin-windows/        # native child windows for external plugin GUIs (tao), idle pump (stub now)
│   ├── test-tone/             # sine gen RtNode — spike 1 vehicle
│   └── metering/              # peak/RMS taps -> IPC channel
├── app/                       # Tauri v2 shell: boots kernel, registers plugins, hosts webview
├── ui/                        # SolidJS + TS + Vite; panel registry mirror
├── docs/superpowers/specs/
└── .github/workflows/         # Windows CI: build + clippy + test
```

## Plugin model

```rust
trait Plugin {
    fn manifest(&self) -> PluginManifest;          // id, name, deps
    fn register(&mut self, ctx: &mut KernelCtx);   // provide services, subscribe events, RT node factories, UI panel contributions
    fn start(&mut self, ctx: &KernelCtx);
    fn stop(&mut self);
}
```

- Registration explicit in `app/main.rs` — no linkme/inventory magic. Debuggable.
- `KernelCtx` exposes: `services.provide::<T>()` / `get::<T>()`, `bus.publish/subscribe` (typed events),
  `rt.register_node_factory()`, `ui.contribute_panel(manifest)`.
- Start order = topological sort of manifest deps. Cycle = boot error.

## RT wall (hard rules)

- RT thread touches ONLY: pre-published graph (`arc_swap::ArcSwap` load), `rtrb` queues, atomics.
  Zero alloc, zero lock, zero syscall, zero logging in callback.
- Graph mutation: build new graph off-thread -> atomic swap -> old graph freed via `basedrop::Collector` on non-RT thread.
- UI -> RT: rtrb command queue. RT -> UI: rtrb meter/position queue -> ipc-bridge throttles 30-60 Hz -> Tauri Channel, small packets.
- Playhead: JS interpolates locally, corrected by sparse Rust updates.
- Enforcement: `assert_no_alloc` wraps process callback in debug/test builds; CI runs it.
- Denormals: FTZ/DAZ enabled on RT thread (`no_denormals` or manual).

## Key crates (from research)

cpal (audio I/O), rtrb (SPSC), arc-swap, basedrop, audio_thread_priority, clack-host (CLAP),
vst3 0.3 (VST3, later), tao (native windows), symphonia/hound (later), assert_no_alloc (dev).

## Phases

- **Phase 0 — scaffold** (this spec): workspace, kernel + registry + bus, rt-core skeleton,
  Tauri+Solid shell boots and shows kernel/plugin status panel, plugin skeletons, CI.
  Deliverable: compiles, boots, all tests green. Plays nothing yet.
- **Phase 1 — spikes** (order = risk):
  1. audio-io + test-tone: RT loop, sine out, no dropouts, assert_no_alloc clean
  2. host-clap: load real CLAP plugin, process audio through it
  3. plugin-windows: external plugin GUI in native child window, idle pump, resize/DPI
  4. IPC perf: measure meter/playhead latency+throughput on WebView2; gate: >20-30 ms sustained -> rethink webview
  5. host-vst3: minimal load + process via vst3 crate
- **Phase 2+** — transport/timeline, graph+PDC, step sequencer, piano roll, mixer, project save.
  Each gets own spec.

## Error handling

- Internal plugin panic = bug -> crash loud in dev builds.
- External CLAP/VST3 crash isolation deferred; architecture keeps seam (hosts behind trait, out-of-proc scanning planned).
- Kernel boot errors (dep cycle, missing service) = typed errors, fail fast with clear message.

## Testing

- Unit tests per crate (registry, bus, topo-sort, rtrb plumbing, graph swap).
- Integration: kernel boots full plugin set; graph swap under simulated load.
- CI: Windows runner — cargo build, clippy -D warnings, cargo test, ui build (pnpm).
