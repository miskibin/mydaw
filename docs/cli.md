# Live session CLI

`mydaw` controls the open, **unsaved** project. A human can keep using the piano
roll while an agent reads or edits the same engine. No webview automation is
needed. Run a build of the app that includes session control; older binaries do
not publish a session endpoint.

```powershell
pnpm --dir ui build
cargo build -p mydaw-app --features custom-protocol
cargo install --path crates/cli --locked
mydaw sessions
mydaw context
mydaw catalog --query pattern
mydaw schema patch_notes
```

Cargo installs `mydaw.exe` in `%USERPROFILE%\.cargo\bin`. Start the app normally.
When several apps are open, supply `--session ID` on every command. `sessions`
lists live endpoints without their tokens. `--pretty` formats JSON; stdout is
reserved for the result, and diagnostics go to stderr. Exit codes are 0 for
success, 2 for local/connection/timeout errors and 3 for engine/worker errors.
`status` remains available during slow vendor/file commands: `busy:true` means
the command journal is occupied, and history is temporarily omitted.

## An agent's editing loop

1. Read `context`. Keep the envelope's `revision`, stable track IDs and pattern
   IDs. The default response contains summaries, not every note or plugin state.
2. Fetch music only where needed:
   `mydaw context --args '{"detail":"musical","patternId":1,"trackId":0}'`.
   Read `schema COMMAND` before using an unfamiliar operation. The catalog is
   searchable and paginated; schemas are fetched individually.
3. Write a JSON argument file and apply edits against the revision just read.
   Prefer one batch for one musical intention. A batch produces one shared undo
   entry and returns small acknowledgements and newly created IDs.
4. On `REVISION_CONFLICT`, reread the affected music and decide how to combine
   it with the human's edit. Never blindly retry with a newer revision.
5. Render a short excerpt, inspect its summary and listen to the WAV. Request
   the PNG when wave shape or frequency content would answer a specific question.

For example, an `edit.json` file replacing only track 0's melody in pattern 1:

```json
{
  "operations": [
    {"command":"patch_notes","args":{
      "patternId":1,"trackId":0,"append":false,
      "notes":[
        {"key":60,"start":0,"length":720,"velocity":0.7},
        {"key":67,"start":960,"length":480,"velocity":0.65}
      ]
    }},
    {"command":"set_bpm","args":{"bpm":140}}
  ]
}
```

```powershell
# Replace 7 with the revision returned by your context response.
mydaw apply --file edit.json --if-revision 7 --label "Change melody"
mydaw undo --if-revision 8
mydaw redo --if-revision 9
```

`patch_notes` preserves the other channels and playlist. To add drums, use
`add_track`, retain its `createdTrackIds`, then patch that track with drum notes.
To change the instrument, use `set_track_source`, `set_track_source_clap` or
`set_track_source_vst3`; their schemas expose `trackId` as an alternative to the
GUI's positional `track`. Never supply both. Pattern content still has positional
channels internally; use the stable-ID operations for collaboration.

Timing uses **960 ticks per quarter note**, 240 per sixteenth and 3840 per 4/4
bar. MIDI key 60 is C4. Velocities are 0..1. Musical context returns compact note
tuples `[startTicks,lengthTicks,midiKey,velocity]`; command inputs use named
fields as above. Step hits are `[startTicks,velocity]` with a separate `stepKey`.
The engine currently uses 4/4 and pattern lengths of 16, 32 or 64 steps.

GUI and CLI share Rust history. A full-pattern GUI edit checks the pattern it
was based on; an external update cannot silently replace that base during a
gesture. CLI mutations wait for the GUI gesture to finish. Notes and transport
use the fast path independently of serialized file/plugin work. Note/song edits
and built-in mixer controls have lightweight history. Structural or opaque
plugin-state undo rebuilds the project and stops playback.

Successful serialized edit, file and job commands can be retried with the
**same arguments and original revision** using `--idempotency-key KEY`. The result is reused within that live
session's bounded retry cache. Reusing a key with different arguments is an
error. A timed-out command may have completed: inspect current state and use its
original key rather than creating a second edit. History and retry caches are
session-local, not durable across restarts. Invalid batches are rolled back;
`ROLLBACK_FAILED` explicitly means a vendor refused restoration and the session
must be inspected before further edits.

Live note and transport events bypass the retry cache. Use `patch_notes` for
durable musical edits rather than replaying live note events.

## External instruments and effects

Use `get_clap_plugins` / `get_vst3_plugins` with `{"rescan":false}` to discover
installed plugins. Place one using its returned path and ID. VST3 placement
calls the scan result's `id` a `cid`. Supported instances are stereo effects or
instruments with stereo output, following the existing host's bus restrictions.

```powershell
mydaw call plugin_parameters --args '{"format":"vst3","trackId":0,"query":"Cutoff","limit":8}'
mydaw schema set_plugin_parameter
mydaw call set_plugin_parameter --args '{"format":"vst3","trackId":0,"paramId":42,"value":0.3}' --if-revision 10
```

Parameter IDs come from the plugin, never from inferred GUI coordinates. VST3
values are normalized 0..1; CLAP values use their advertised native range.
Metadata includes display text, defaults, units/ranges, flags and `pending`.
Alternatively supply `text` instead of `value` and let the plugin convert its
own displayed units. Setters wait for processor acknowledgement, then take the
undo checkpoint. Unsupported IDs, read-only parameters and invalid values fail.
The audio callback consumes a bounded, preallocated parameter mailbox and emits
VST3 parameter queues or CLAP parameter events; it does no metadata work.

`save_plugin_preset` and `load_plugin_preset` use explicit paths and the host's
opaque state format. These files belong to that plugin and are not vendor
`.vstpreset`/`.fxp` files; do not load a different plugin's state. Large blobs stay
in files. Project format 5 stores component and controller state for VST3;
versions 1..4, including legacy raw VST3 component blobs, remain readable.

Some vendor browsers, account dialogs and hidden controls expose no parameter
API. The fallback is the existing `open_clap_gui` / `open_vst3_gui` command and
the native editor, followed by rereading state. CLI does not pretend to automate
those vendor screens. While an editor is open, CLI operations that replace or
snapshot plugin state, including undo, are rejected. Native editor changes
reported by the plugin create a history barrier because no reliable inverse is
available; close the editor and reread the revision before resuming CLI edits.
Plugins that do not report changes or save state cannot provide full round-trip
guarantees. A failed state checkpoint prevents an undoable edit from starting.

## Render, analysis and cancellation

```powershell
mydaw render --args '{"options":{"mode":"pattern","patternId":1,"bars":4,"tailSeconds":2}}'
mydaw render --args '{"options":{"mode":"song","startBar":8,"bars":4,"trackId":0}}'
mydaw analyze --args '{"path":"D:/beats/excerpt.wav","images":true}'
```

Both commands wait for completion by default. `--no-wait` returns a `jobId`;
use `call job_status` or `call cancel_job` with `{"jobId":"..."}`. Workers use
independent processes and a snapshot of the unsaved project; live transport and
devices are untouched. Rendering uses the real engine graph, sample files,
routing, built-in effects and real CLAP/VST3 instances, including the master.
Workers load only the sampled banks referenced by the project, synchronously,
so library discovery cannot race the render and create a silent drum track.
Plugins receive offline mode where available. The live app must have a loaded
plugin instance to snapshot its settings; placing/controlling external plugins
uses the running audio engine. A device-free worker can then render independently.
Missing plugins/assets, refused
state restoration, unsupported realtime-only CLAP plugins and non-finite audio
produce errors rather than a successful partial render.

Ranges are zero-based bars. Earlier bars warm voices and effects before capture;
note scheduling stops after the requested body and the graph keeps processing
the requested release/effect tail. A track render solos that track through its
routing and master. The engine currently has **no plugin delay compensation**
or sample-accurate plugin automation; these capabilities are reported as false.
Render is the current graph's audio, so plugins introducing latency can affect
alignment. Those engine features are separate follow-up work.

Results contain paths to float stereo `audio.wav`, `analysis.json` and an optional
annotated `waveform-spectrum.png`, plus small level/spectrum summaries and a
64-point envelope. Analysis accepts the app's WAV/FLAC/Ogg/MP3 formats, including
PCM/mono input, which is decoded to stereo. Peak/RMS, clipping, spectral peaks
and the note representation provide evidence for musical decisions; they do not
measure subjective quality or reconstruct a polyphonic score. Audio files and
images are not inserted into every JSON reply.

Work is bounded: 2 concurrent workers, 180 seconds of rendered/decoded audio
and worker wall time, 0..30 seconds of tail, render rates 44.1/48/96 kHz, 256 MiB
analysis input, 16 cached job statuses, 50 history entries/64 MiB, 128 batch
operations and 16 MiB IPC frames. Artifacts remain on disk after cache eviction
or app exit and can be deleted when no longer needed.

## Architecture and maintenance

```text
Solid UI -> SessionClient --\
                            SessionControl -> typed app commands -> engine
mydaw CLI -> local IPC ----/         |                              |
                         shared revision/history          bounded RT queues
                                    |
                              project snapshot -> worker -> WAV/JSON/PNG
```

`app/build.rs` reads the existing `#[tauri::command]` functions and generates
argument structs, JSON schemas, validators, descriptions and dispatch. Adding or
removing a command changes the CLI catalog at build time. It is not a second
handwritten musical API. Add argument documentation to DTOs so generated schemas
describe musical units. Review the command's classification in
`app/src/control.rs`: reads, transient operations, song edits, mixer edits,
full-project edits and project-opening barriers have different history costs.
Unknown classifications default to full-project edits. `apply` permits only
undoable operations; file/device/window side effects cannot enter a transaction.

`crates/control-protocol` has no engine/webview/plugin dependency. It defines the
versioned request/reply envelope and framing; `crates/cli` is a small client.
GUI musical calls go through `control_call`; direct invoke handlers remain only
for meter subscription and the IPC bench. A future MCP adapter can translate to
this same envelope without a separate command implementation.

Endpoints use authenticated Windows named pipes or Unix local sockets. Discovery
files live in the user's `%LOCALAPPDATA%/WAVE Studio/sessions` (Unix XDG runtime
directory when available). `MYDAW_CONTROL_DIR` selects an isolated directory for
tests. Tokens stay in those files and are not printed; Unix directories are
private. No TCP port is opened. Connections, framing time and response size are
bounded. The protocol rejects unknown envelope and top-level argument fields.

## Verification

Run the repository gates and the real native collaboration test:

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
pnpm --dir ui test
pnpm --dir ui build
cargo build -p mydaw-app --features custom-protocol -p mydaw-cli -p mydaw-fixture-clap-gain
./scripts/cli-smoke.ps1
# Optional local integration test, requires installed Serum:
./scripts/cli-smoke.ps1 -InstalledVst3
```

The script launches and stops its own app copy, never an existing session. It
verifies melody preservation, revision conflicts, idempotency, rollback, shared
undo/redo, CLAP parameter/state restoration, real rendered artifacts and optional
Serum control/render. `report.json` records artifacts and context byte/latency
measurements. Rust tests check rendered fixture pitch and master gain objectively;
RT allocation tests include simultaneous CLAP parameter changes and notes.
