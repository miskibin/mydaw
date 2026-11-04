# Beat workflow

The Browser stays available in the Rack, Playlist, Mixer and Piano Roll.
Use **All**, **Samples** or **Instruments** to narrow it. Search looks inside
unopened folders and matches every word against the folder and file names.
When a library exceeds the search budget, the Browser says so; narrow the query.

| Action | Gesture |
| --- | --- |
| Find a sound | Ctrl+F |
| Preview a sample | Click, or Space on its row |
| Browse adjacent samples | Up / Down |
| Replace the selected channel's sample | Enter, or Shift+Up / Down |
| Add a separate sampler channel | Double-click, +, or Shift+Enter |
| Add an instrument | Double-click, +, or Enter |
| Place audio in the arrangement | Drag a sample onto a Playlist lane |
| Open Playlist / Rack / Piano Roll / Mixer | F5 / F6 / F7 / F9 |
| Toggle Browser | F8 |

Preview has its own stop and volume controls. It does not start the transport or
add files to the project. Its speaker button mutes and restores the previous
volume. **Info** shows the selected file's path and preview metadata; **Audition**
returns to the waveform. The Browser's **⋮** menu controls audition on selection
and hiding the pane. Snap menus choose a grid value directly.
Waveforms cover up to the first 15 seconds; longer
files are marked. The full file is loaded when added to a channel or Playlist.

Paint drum steps in the Rack; right-click a channel for fills and Piano Roll.
Pan and volume remain visible when scrolling long patterns. Enter
toggles a focused step. Knobs also accept arrows, Shift for finer changes,
Home/End, and double-click to reset.

Audio clips (a beat, a vocal take) edit in place, and the audio never moves in
time. Drag a clip's **left** edge to trim its start, its right edge to trim its
end. Press **C** for the slice tool and click a clip to cut it on the grid (Alt
cuts off the grid); right-click a clip for **Split here**, **Split at playhead**
and its own **Volume** in dB, which also shows after the clip's name. Each edit
is one Undo step.

Choose a pattern from the picker on the **left** of Playlist, then paint it into
a lane. Playback follows the last musical editor you activate: the Rack and
Piano Roll play the current pattern; the Playlist plays the arrangement. Click
an editor or focus one of its controls to activate it. Mixer, Settings, Browser
and plugin windows retain the last musical editor's playback scope. Space and
the Play button use that scope, including after external CLI changes; editor
activation also changes scope while playback is running.
Automatic GUI scope changes do not add Undo entries or clear Redo.

**Space** and the main Play button start/stop, returning to the editor's start
point on Stop. **Ctrl+Space** pauses/resumes at the exact engine position instead
of rewinding. Pattern and Playlist have separate start points, initially zero;
clicking or dragging their time ruler remembers a new start point. Switching
between these scopes starts at that scope's start point, including while playing.
Rack and Piano Roll share the pattern start point. Selecting another pattern
resets its start point to zero; opening or creating a project resets both.
**Home** in a musical editor moves its start point to zero. Start points stay on
their musical beats when the tempo changes. Held Space does not repeatedly
toggle the transport. Space starts/stops even when an editor step, button or
knob has keyboard focus; Enter activates that control. Text inputs, menus and
sample audition keep their local Space behavior; Ctrl+Space pauses the transport.

The Rack is a floating window over the Playlist or Mixer. Drag the empty space
or **CHANNEL RACK** label in its toolbar; controls retain their usual gestures.
Drag the bottom-right corner to resize. Close with **×** or Escape, and use F6
to reopen at its remembered position and size for this session. F5 brings the
Playlist forward and hides the Rack; clicking the Playlist behind an open Rack
activates arrangement playback while keeping both visible. F7 opens the Piano
Roll; closing it restores the editor you came from. Window bounds adjust to a
smaller viewport, and a large rack scrolls its channels and steps.

The right inspector opens
from its rail when needed; selecting a Rack channel does not force it open.

Undo and Redo are beside the transport and show the pending action in a tooltip.
Click **File** at the top left for project actions, song looping and
the typing keyboard. Tempo is one field: drag or scroll for whole BPM, hold
Shift for hundredths, or double-click to type a precise value.

The transport shares one row with the native window controls. Drag the clock or
empty space on this bar to move the window; double-click to maximize or restore.
The Browser's section tabs, refresh and options share one row as well. At narrow
window widths the editor buttons become icons, with their names and shortcuts
in tooltips; no second toolbar row is needed.

The local `examples/beat-starter.mydaw` demonstrates a 130 BPM drum pattern,
separate Kick / Clap / Closed Hat channels, a spare synth, conservative levels
and one arrangement clip. It references the 808 factory samples installed on
this machine. Open it with Ctrl+O and use Save As for your own version.

The interaction choices follow the official [FL Studio Browser](https://www.image-line.com/fl-studio-learning/fl-studio-online-manual/html/browser.htm),
[Channel Rack](https://www.image-line.com/fl-studio-learning/fl-studio-online-manual/html/channelrack.htm)
and [transport controls](https://www.image-line.com/fl-studio-learning/fl-studio-online-manual/html/toolbar_panels.htm),
with [keyboard shortcuts](https://www.image-line.com/fl-studio-learning/fl-studio-online-manual/html/basics_shortcuts.htm)
and [workflow](https://www.image-line.com/fl-studio-learning-content/fl-studio-online-manual/html/basics_workflow.htm)
manuals. The explicit search and folder behavior were also checked against
[Ableton's Browser](https://www.ableton.com/en/live-manual/12/working-with-the-browser/).
