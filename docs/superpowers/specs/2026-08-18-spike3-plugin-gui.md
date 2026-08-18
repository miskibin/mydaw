# Phase 1 Spike 3 — Plugin GUI: a CLAP editor in a native floating window

Date: 2026-08-18
Status: Done. Gate passed.

Spike goal (from the scaffold spec): *"plugin-windows: external plugin GUI in native child window,
idle pump, resize/DPI."*

Per the architecture the editor gets its **own native top-level window** next to MYDAW's, not a
region of the webview: a plugin paints with its own toolkit into an `HWND`, and an `HWND` cannot be
composited into a WebView2 surface. "Child window" in the scaffold spec means the *plugin's* window
is a child of a host window — which is what happens; the host window is a floating one.

## What got built

| Where | What |
| --- | --- |
| `plugins/plugin-windows` | Real now. `PluginWindowService`: main-thread-affine manager for plugin editor windows (`open_window`/`show_window`/`close_window`/`set_client_size`/`client_size`/`dpi`/`has_child_window`/`close_all`), `PluginWindowHandle` with an event channel (`Closed`, `Resized`, `DpiChanged`), typed `PluginWindowError`. Raw Win32 in `win32.rs`; a stub backend keeps every other platform compiling. Registered as a kernel service by `PluginWindowsPlugin`. |
| `plugins/host-clap/src/gui.rs` | `ClapGuiSession` — the CLAP `gui` protocol: `is_api_supported` → `create` → `can_resize`/`get_size` → open window → `set_scale` → `set_parent` → `show`. `pump()` services plugin requests and user actions; `close()` destroys the editor then the window. `decide_resize` is the negotiation rule, pure and unit-tested. `ClapGuiError` (`NoGuiSupport`, `UnsupportedApi`, `Gui{stage}`, `Window`, `Panicked`). |
| `plugins/host-clap/src/timers.rs` | `ClapTimers` — host-side `clap.timer-support` registry: register/unregister, `collect_due(now)` with no backlog replay, period clamped to what this host can deliver. |
| `plugins/host-clap/src/hosting.rs` | The host now *declares* `gui` + `timer-support`. `ClapGuiRequests` latches `request_resize`/`request_show`/`request_hide`/`resize_hints_changed`/`closed` as atomics (they are `[thread-safe]` callbacks). `LoadedClapPlugin::tick_main_thread()` fires due timers and finally acts on the `request_callback` flag that spike 2 only latched. |
| `test-fixtures/clap-gain` | The fixture grew a real `clap.gui` editor: embedded Win32 child window, animated bar, painted size and frame counter, `adjust_size` rounding to a 10 px quantum, `can_resize = true`, preferred 400×250. `MYDAW_FIXTURE_GUI=0` removes the extension, which is how the host's "no editor" path is tested. |
| `app/src/gui_bridge.rs` | `ClapGuiBridge` — `Send + Sync` mailbox between a Tauri command (worker pool) and the main thread, with a bounded wait and a waker. |
| `app/src/gui_host.rs` | `ClapGuiHost` — owns the open session on the main thread; `tick()` serves requests, pumps the editor and gives the plugin its main-thread slice. |
| `app/src/lib.rs` | `RunEvent::MainEventsCleared → gui_host.tick(...)`; `RunEvent::Exit` closes the editor **before** the plugin is unloaded. Two new commands: `open_clap_gui`, `close_clap_gui`. |
| `ui/` | *Open GUI* / *Close GUI* in the CLAP Plugins panel, `Backend::openClapGui`/`closeClapGui`, mock implementations that log instead of opening anything. |

## The pump: which thread, and why

**Chosen: the plugin windows live on Tauri's own (tao's) thread, and the host's per-frame work runs
from `RunEvent::MainEventsCleared`.**

The reasoning, and the two facts it rests on — both verified, not assumed:

1. **A Win32 message queue belongs to a thread, not to a library.** `tao` 0.35 runs
   `GetMessageW(hwnd = NULL)` + `DispatchMessageW` (`tao/src/platform_impl/windows/event_loop.rs`),
   which dispatches to *every* window on that thread. Windows created by `mydaw-plugin-windows`, and
   the plugin's own child window created during `clap_plugin_gui->create()`, are therefore pumped by
   tao's loop with no integration whatsoever.
   *Verified* by `plugins/plugin-windows/tests/native_window.rs`, which drives our windows from a
   deliberately foreign `PeekMessage`/`DispatchMessage` loop, and in the application by an editor
   that animates only because host timers are being serviced.
2. **Tauri surfaces the loop's idle point.** `RunEvent::MainEventsCleared` fires each time the event
   loop drains its queue, which is where the host owes the plugin its main-thread work.

The one thing missing from that pair is a heartbeat while the user does nothing: `GetMessage` blocks,
so no messages means no `MainEventsCleared`. Each plugin window therefore carries a `SetTimer` at
`PUMP_INTERVAL_MS` (16 ms) whose `WM_TIMER` exists purely to make the loop turn. It is counted
(`PluginWindowHandle::pump_ticks`) so "is the loop alive" is a number, not a feeling.

**The alternative — a dedicated plugin-UI thread with its own `GetMessage` loop — was not needed, and
would have cost a great deal.** CLAP pins instance lifecycle to the main thread, so `LoadedClapPlugin`
(already `!Send`, already owning the CLAP instance) would have had to *move* to that thread, taking
plugin loading, activation, node creation and teardown with it. The shell's ownership story from
spike 2 (`run()` owns the plugin next to the kernel; the audio graph hands the processor back through
a channel) would have had to be rebuilt around cross-thread marshalling. That is a large amount of
risk to buy protection against a problem that does not exist on this platform.

The seam is kept, though: everything the main thread does per frame is `ClapGuiHost::tick(plugin,
bridge)` plus `PluginWindowService`'s thread-affinity check. Moving both to another thread later means
moving one call and one service, not redesigning the shell.

### What runs per tick

```text
MainEventsCleared
  └─ ClapGuiHost::tick
       ├─ bridge.drain()             open/close requests from the webview, answered on this thread
       ├─ session.pump()             plugin requests (resize/show/hide/closed) + window events
       └─ plugin.tick_main_thread()  due clap.timer-support timers, then on_main_thread if requested
```

## Empirical findings

Three of these cost real debugging time and are the reason the fixture has an editor at all.

**1. `clack-extensions` 0.1.1 discards the plugin's answer to `set_parent` and `set_size`.**
Its FFI bridge is `PluginWrapper::<P>::handle(plugin, |p| Ok(p.main_thread().as_mut().set_parent(…)
.is_ok())).is_some()` — the inner `bool` is computed and thrown away, and `is_some()` is only false if
the plugin *panicked*. A plugin that fails to embed therefore reports **success** to the host.
Consequence for MYDAW: after `show`, the host asks
`PluginWindowService::has_child_window` and logs a warning when a plugin claimed to embed but did
not. That check is what turns "blank window, no explanation" into a log line naming the plugin.

**2. A plugin DLL must not register window classes with `GetModuleHandleW(NULL)`.**
That returns the *host executable's* instance handle, so every copy of every bundle in the process
shares one `(class name, hInstance)` key. The fixture did this and produced a genuine
`STATUS_ACCESS_VIOLATION` in the test suite: two copies of the bundle loaded from different paths,
one unloaded, the other left creating windows against a window procedure in freed memory. The fix is
`GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, <a function in this module>)`, plus the
module handle in the class name.

**3. Windows does not unregister a DLL's window classes when the DLL is unloaded**, and it usually
maps the DLL back to the same base address. The second load therefore sees its own class already
registered and `RegisterClassW` fails with `ERROR_CLASS_ALREADY_EXISTS`. Treating that as failure is
how a plugin ends up working the first time it is loaded and never again — which is exactly what the
fixture did until it was fixed. With the module handle in the class name, "already exists" can only
mean "registered by this same image at this same address", so it is safe to reuse.

**4. Unloading a bundle while one of its windows still exists is fatal.** Hence the shutdown order in
`app/src/lib.rs`: close the editor, *then* stop audio, *then* deactivate and drop the plugin.

**5. `set_parent` needs the window to exist first.** The fixture creates its child window in
`set_parent`, not in `create`, because that is the first moment the parent handle is known — the same
shape most real plugins use.

## DPI

Win32 uses **physical** pixels for `CLAP_WINDOW_API_WIN32`, and the plugin is told the scale
separately. The order matters, and it is forced by the platform:

- The host window is created at 96 dpi metrics (`AdjustWindowRectExForDpi`), because which monitor
  the window lands on is not knowable before it exists.
- Immediately after creation the real DPI is read (`GetDpiForWindow`) and the client area re-applied,
  so a 400×250 editor is 400×250 physical pixels on a 150% display too.
- `set_scale(dpi / 96)` is called only once the window exists — before `set_parent`, as the CLAP
  sequence requires. A plugin that queries the OS itself may refuse it; that is logged, not fatal.
- `get_size` is re-read after `set_scale`, since the scale may change the editor's idea of its size.
- `WM_DPICHANGED` moves the window to the rectangle Windows suggests and re-issues `set_scale`.

Untested on this machine: both monitors are 96 dpi. The code paths exist and are unit-tested where
they can be (`scale_for_dpi`, DPI-scaled window metrics); a mixed-DPI machine is on the manual test
plan below.

## Resize is a negotiation

A user drag is a *request*, not an instruction. `decide_resize(current, requested, can_resize,
adjusted)` encodes the rule and is tested on its own:

| Situation | Decision |
| --- | --- |
| The window is already the size the plugin agreed to | `Ignore` — this is the echo of our own `SetWindowPos`, and acting on it starts a resize loop |
| `can_resize() == false` | `RestoreWindow(current)` — and the window has no drag frame in the first place |
| The plugin accepted the size | `Resize { correct_window: false }` |
| The plugin rounded it down | `Resize { correct_window: true }` — `set_size`, then move the window to what was agreed |
| The plugin rounded it back to its current size | `RestoreWindow(current)` |

The plugin's own `request_resize` (which may arrive from any thread) is latched and applied on the
main thread; the resulting `WM_SIZE` is recognised as the host's own doing.

## Verified on this machine

Windows 11, Focusrite USB Audio @ 44 100 Hz, two 1920×1080 monitors at 96 dpi.

**Automated** — `cargo test -p mydaw-app --test gui_audio_smoke -- --ignored`, which opens the editor
through the application's own path while audio plays through the same plugin:

```text
device: Speakers (Focusrite USB Audio) @ 44100 Hz, 2 ch, max block 4096
editor window 0x2770966, plugin child 0xa1d0890
after the resize the plugin reports 610x430
captured 1.20 s, peak 0.1000 (-20.00 dBFS), dominant 440.0 Hz, dropped 0
```

i.e. with the editor open, animating and being resized: **440.0 Hz, −20.00 dBFS (the tone's −14 dBFS
through the fixture's −6.02 dB), zero dropped blocks.**

**Interactive** — `MYDAW_CLAP_FX=C:\mydaw-smoke\mydaw-gain.clap;dev.mydaw.fixture.gain cargo run -p
mydaw-app --features custom-protocol`, screenshots in the session scratchpad:

| Step | Result |
| --- | --- |
| Boot | `CLAP effect loaded: 'MYDAW Fixture Gain' … audio output: Focusrite @ 44100 Hz` |
| Play | Meters live, playhead running, transport `PLAYING` |
| *Open GUI* in the CLAP Plugins panel | `plugin editor open: 'dev.mydaw.fixture.gain' at 400x250 (scale 1)`; a separate native window titled *MYDAW Fixture Gain — MYDAW* appears (`05-crop-both-windows.png`) |
| Editor liveness | Window text advanced `f=1290 → 1652 → 3200` across captures — the animation is driven **only** by `on_timer`, so this is the host's timer service and pump being alive inside tao's loop |
| Programmatic `SetWindowPos` to a 617×433 client area | Plugin reports **610×430** (its 10 px quantum), window corrected to match, editor repaints `MYDAW fixture GUI 610x430 frame …` |
| Audio during all of the above | Meters unchanged and continuous, playhead advanced 00:01 → 02:05 without a gap |
| Close the editor's window button | `plugin editor closed: 'dev.mydaw.fixture.gain'`; window destroyed, MYDAW still `PLAYING` (`06-crop-closed.png`) |
| Close MYDAW | `shutting down` → `audio output stopped` → `CLAP effect 'dev.mydaw.fixture.gain' deactivated`, exit code 0 |

Gates: `cargo build --workspace`, `cargo test --workspace` (**341 passed, 0 failed, 3 ignored** — was
279), `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --check`,
`pnpm --dir ui build`.

## Known limitations

- **One editor at a time.** `ClapGuiHost` holds a single session, because the shell hosts a single
  CLAP effect (`MYDAW_CLAP_FX`). The window service already handles many windows; the limit is the
  shell's plugin model, and it lifts with the Phase 2 mixer.
- **Nothing is persisted.** Editor size and position are forgotten on close; a project file will store
  them (`gui.set_size` on open is already the code path that would restore one).
- **The UI's open/closed state is optimistic.** If the user closes the *native* window, the panel
  button still says "Close GUI" until it is clicked (which harmlessly reports "closed"). Fixing it
  needs a push channel from the shell to the webview — the same one Phase 2 wants for plugin state.
- **`request_hide` closes the session** rather than hiding the window: `PluginWindowService` has no
  hide, and an editor that asks to disappear is what a user expects to see disappear.
- **`request_restart` is still only recorded.** `tick_main_thread` reports it in `MainThreadWork`;
  nothing rebuilds the graph yet.
- **No keyboard/accelerator forwarding.** Plugin editors that want key events get whatever Windows
  routes to their own child window. `WM_GETDLGCODE`, accelerator translation and focus handoff
  between the webview and the editor are untouched.
- **Mixed-DPI is coded but unproven** (see above), as is any editor that only supports *floating*
  windows: `UnsupportedApi` is returned and the user is told, but nothing floats yet.
- **Only Windows.** The macOS/X11 backends are the `unsupported` module's four functions.
- **No crash isolation.** Unchanged: a plugin that segfaults inside its editor takes the process, and
  a plugin that blocks in its window procedure freezes the whole loop, webview included.

## What real-plugin testing still needs

The fixture proves the *host* is correct; it cannot prove interoperability with vendors' toolkits
(JUCE, iPlug2, VSTGUI, egui). Nothing was downloaded for this spike. The manual plan, for a machine
with real CLAP plugins installed:

1. Install a plugin in `C:\Program Files\Common Files\CLAP` (or point `CLAP_PATH` at it) and start
   MYDAW with `MYDAW_CLAP_FX=<bundle>;<id>`.
2. *Open GUI*: the editor should appear at its own size, fully painted, and animate.
3. Drag the frame: an editor that reports `can_resize()` should follow smoothly with no flicker
   loop; one that does not should have no drag frame at all.
4. Use a knob and confirm the audio changes — that exercises the editor's own event loop inside our
   pump.
5. Move the window to a second monitor at a different scale factor; the editor should stay crisp and
   correctly sized (this is the DPI path that this machine cannot exercise).
6. Close and reopen the editor several times, then close MYDAW: no crash, no leaked window, and
   `CLAP effect '…' deactivated` in the log.
7. Try a plugin with **no** editor: the panel should say "This plugin has no editor." and nothing
   else should change.

## Pointers for the next spikes

- **Spike 4 (IPC perf):** the plugin window's 16 ms `WM_TIMER` now wakes the event loop whenever an
  editor is open, which is exactly the loop the meter channel shares. Measure with an editor open *and*
  closed; if the webview's frame rate moves between the two, the pump interval is the knob.
- **Spike 5 (VST3):** the seam to copy is `ClapGuiSession` + `PluginWindowService`, not the clack
  types. `IPlugView::attached(hwnd, "HWND")` replaces `set_parent`, `IPlugFrame::resizeView` replaces
  `request_resize`, and VST3's `onSize`/`checkSizeConstraint` map onto `decide_resize` unchanged.
- **Phase 2 parameters:** an open editor is the thing that makes parameter output events matter —
  `ClapNode` currently drains none, so a knob moved in an editor cannot yet reach the UI.
- **Phase 2 projects:** `ClapGuiSession::size()` and the window's position are what a project file
  needs to store to reopen an editor where the user left it.
