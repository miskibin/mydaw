# Changelog

What changed in each release of WAVE. The newest version comes first; its section is
shown in the app after an update and is published as the release notes (see
`docs/releasing.md`). Write for musicians: what you can do now, not how it works inside.

## Unreleased

### Added
- **Tune a vocal to the melody you played in the piano roll.** Play a melody, sing along to it,
  then in the Piano roll press **♪ Vocal reference** (only the selected notes, or the whole
  channel) — the vocal's pitch editor opens with that melody as its target. Or open the pitch
  editor, choose *To reference* and pick the melody from *Melodies in the project* (they are
  listed by how well they fit what you sang). The vocal keeps its own timing: singing a little
  early or late is fine, the octave nearest your voice is used (a melody played an octave higher
  just works), in a chord the note nearest your voice is taken, and ad-libs with nothing played
  under them stay as sung. The melody is copied into the vocal, so changing the piano roll later
  does not change it until you press *Update*.
- **Correction you can understand: Strength, Speed and Flatten**, with one-click styles
  (Natural, Pop, Tight, Robot). *Speed* is how fast the voice is pulled onto the note — 0 ms is
  the hard "robot" step, around 50 ms tight pop, 100 ms and more keeps the singer's own scoop. The
  line under the controls says in words what *Correct* will do. A reference has its own Speed too.
- **Play and loop in the pitch editor like in the Playlist.** Click the ruler to move the start
  marker (while playing, playback jumps there), drag it to set the loop, double-click to remove
  the loop; Space or *Play* plays the song so you hear the vocal in the mix, and the view follows
  playback. *Solo* plays the vocal alone, with your edits, from the marker.
- **Pitch Correct one click away.** The pitch editor, the sample window and a sampler channel's
  window have a *Pitch Correct* button that puts the real-time tuner on that sound's mixer insert
  (with the editor's key and scale), or opens it when it is already there. The sample window also
  opens the pitch editor.
- **Main volume knob next to Play and Stop**, like FL Studio: it is the Master fader (drag, scroll,
  double-click for 0 dB, right-click to automate).

### Fixed
- The pitch editor's preview now plays your pitch edits (it used to play the original recording).
- The reference list no longer offers drum hits and one-shots: only recordings in the playlist or
  a comp, at least a second long.

## 0.9.13 — 2026-11-12

### Added
- **Make unique for audio clips**, like FL Studio. Open an audio clip's menu (click or right-click
  the icon in its top-left corner) and choose *Make unique*: the clip gets its own copy of the
  sample ("Guitar #2", same file, nothing is copied on disk) on a new track right below, sent to a
  free mixer insert named after it, so you can put effects on that one clip. Selected clips of the
  same sample on that track go along. Other clips of the sample stay as they are; Ctrl+Z undoes it
  in one step. From the command line: `wave make-unique --clip ID`.
- **One sample window for audio clips and sampler channels**, like FL Studio. Double-click an audio
  clip and you get the same window as a Channel Rack sampler: the waveform (click it to listen,
  drag the markers for the part that plays and the corner handles for the fades), the file, then
  Reverse, Normalize and the knobs. The MIXER picker sits in the header of both, next to the close
  button; scroll over it to step through the mixer inserts.
- **Transpose audio clips ±12 semitones without changing their length.** PITCH and FINE in the
  sample window. It is rendered once in the background (the Playlist shows `+5 st …` meanwhile and
  the clip plays at its own pitch until it is ready) and saved with the song.
- **Fit an audio clip to 1 or 2 beats or 1, 2, 4 or 8 bars** with one click (*Fit to* in the sample
  window), like FL Studio's TIME → Beat/Bar. Keep pitch stretches it at the same pitch; Resample
  changes speed and pitch together. Lengths that would need more than ¼×–4× are greyed out.
- **Icons in Project → Audio**: every sample shows the icon its clips show in the Playlist.

- **Drop a library sample on a Channel Rack channel to replace its sample**, like FL Studio. The
  row lights up and the label says what will be replaced; the channel keeps its steps, notes and
  mixer routing (a synth or WAVE Keys channel becomes a sampler). Drop on empty rack space for a
  new channel.
- **The standard library grew from 50 to 148 sounds.** More 808 (long, boomy, deep and punchy
  kicks, four more snares, short and long open hats, deeper and brighter toms, a mid conga) and
  six 808 basses, one of them driven so it carries on small speakers. More acoustic drums
  (snares with and without wires, rimshot, cross stick, rope and field drums, group claps, floor
  tom, cymbal bell, choke) and hand percussion: congas, darbuka, frame drum, agogo, cowbells,
  guiro, vibraslap, sleigh bells, log drums, anvil, brake drum.
- **Orchestra.** Three orchestra hits (open, minor, major) built from strings, brass and
  timpani, a strings stab, a brass stab and pizzicato, all on C so they follow the notes you
  play; single notes of tremolo strings, cello, contrabass, trumpet, horn, flute and harp;
  timpani, concert bass drum, gong, tubular bell and an orchestral crash.
- **More tonal one-shots**, each tuned to its key: vibraphone, xylophone, balafon, kalimba,
  hand chime, harpsichord, upright piano, FM piano and clav, pipe organ, tenor sax.
- **FX.** Cymbal swells (short and long) and a bass-drum swell for build-ups, timpani and snare
  rolls, wind chimes, a bell tree and a zap.
- **Search in the Library.** Type in the field above the list: several words must all match
  the name or the group ("808 bass", "orchestra hit"). It looks in the standard library, your
  installed kits and your folders at once.
- **Browse samples with the keyboard.** ↑ and ↓ move through the list and play each sample;
  Enter adds the one you are on as a sampler channel.

### Changed
- **Play, stop and record come first in the toolbar**, right after the project name.
- **The sample window has no Preview, Trim, Pitch… or Tempo / key… buttons any more.** Click the
  waveform to listen. *Trim to the part that plays*, *Edit pitch…* and *Detect tempo and key…* are
  in the clip's menu (the icon in its corner).
- **PAT / SONG switches what is playing**, like FL Studio: while the song plays, PAT (or L) loops
  the current pattern straight away, and SONG goes back to the song, with no Stop in between.
- **The Piano roll zooms out further**: until the whole pattern and the empty bars after it fit.
- **The Library tab is easier to read.** Groups are rows with their own icon, colour and file
  count that open and close with a click anywhere on the row; the open/closed arrows are
  drawn clearly instead of the tiny grey marks. Every sample carries its group's icon, your
  own folders get icons from their names (kicks, snares, hats, bass, vocals …), and the
  standard library has its own section above the kits you can download.
- **Pitched samples show their key** (G#1, C4 …) in the Library, the key a sampler channel
  made from them is rooted at.
- The installer is about 11 MB larger: the new sounds ship inside WAVE and need no download.

### Fixed
- **A new pattern plays at once.** Making or picking a pattern while a pattern loops could keep
  playing the old one until Stop.
- **808 drums get a drum icon.** "808 Kick", "808 Snare" and the like showed the bass icon.

## 0.9.12 — 2026-11-12

### Added
- **Export stems.** *Export → Output: Stems* writes every mixer track that has sound as its own
  WAV, named after the track, with one folder pick, like FL Studio's *Split mixer tracks*: no more
  soloing and bouncing one track at a time. All files have the same length and start together, so
  they line up in another project or DAW. A stem is the track after its own effects and fader,
  before the master; *Master* adds `Master.wav`, the whole mix with the master effects. Muted
  and empty tracks are skipped, solo is ignored, and a cancelled export leaves no half-written
  files. The same from the command line: `wave render --stems --out folder/`.
- **Stretch audio clips in the Playlist.** Drag an audio clip's edge to make it longer or shorter.
  Trim | Stretch (Shift+M) decides what the edge does: in Stretch mode the clip plays slower or
  faster (¼× to 4×) and keeps its pitch. The stretch is rendered once, in the background, when
  you let go; the Playlist shows which clips are still working.
- **Warp: draw how time and volume move over every bar.** A new effect for stutters, rolls,
  half-time, tape stops and starts, reverses, scratches (time) and gates, trance gates and
  sidechain pumps (volume). Each effect holds 16 time and 16 volume slots, ready-made and
  editable: click a slot to edit and play it, drag points on the map (the slope is the speed,
  the dashed line is "stopped", the grey lines are "reversed"), right-click a point for its
  shape or a musical speed (×½, −7 st, stop, reverse). Change slots over the song with an
  automation clip, or live; Trigger sync waits for the next 1/16 … bar, One-shot plays one bar
  and returns, Hold loops the previous or the next bar. Jumps are de-clicked, and drum hits
  keep their attack.
- **A library of 86 ready-made Warp maps.** Right-click a slot → *Library*: stutters, rolls and
  build-ups, half-time, tape stops and brakes, reverses, scratches, glitches (time), and
  sidechain pumps, gates, trance gates, fades, mutes and tremolos (volume).
- **Three new automation curve shapes:** pulse (square), wave and half sine (tape stop / start).

### Changed
- **The Delay is now a full echo unit.** Its time is set in beats ("1/2 beat", "1 1/2 beats",
  triplets and dotted) and follows the song tempo, even tempo changes; or in ms. Inside the
  feedback loop: a resonant filter (LP/HP/BP), sample-rate and bit reduction, a saturator or
  limiter you can drive into self-oscillation (feedback up to 125 %), diffusion that smears
  repeats into a cloud, and an LFO for tape wow, chorus or filter sweeps. Mono, stereo with an
  offset, ping-pong, or no echo at all (a filter / lo-fi / drive unit). Repitch glides the
  pitch when the time changes, like tape; Keep pitch never bends it. Projects with the old
  Delay sound the same.
- **WAVE opens its window right away.** It no longer waits for your last project to load before
  showing anything: the window appears at once and the project comes in while the interface
  starts, so a big session is ready sooner and there is no more clicking the icon again.
- **A much smaller download.** The installers no longer carry the parts of the piano recordings
  that never play (each note rings at most 9 seconds), so the WAVE Keys piano takes about 23 MB
  instead of 138 MB. It sounds the same.
- **One Windows installer.** Releases have only `setup.exe` now. If you installed WAVE from the
  `.msi`, uninstall it and install the `setup.exe` once; updates then arrive by themselves.

## 0.9.11 — 2026-11-12

### Added
- **Your work survives a crash, even before the first save.** A project you never saved is
  copied aside at every autosave (Settings → General → Autosave, every 2 minutes by default).
  If WAVE closes unexpectedly, the next start offers it back: *Recover*, *Delete copy* or
  *Not now*. It opens as an unsaved project, so Ctrl+S asks where to put it.
- **The previous save is kept.** Every save first keeps the file it replaces as
  "Song (backup).wave" next to your project. It opens like any project.
- **JACK on Linux.** Settings → Audio → Driver → JACK uses JACK or PipeWire's JACK at the rate and
  buffer the server runs at. The ALSA audio thread now asks for real-time priority, so small
  buffers drop out less.

### Changed
- **Songs can be four times longer:** 512 bars instead of 128 (almost 12 minutes at 174 BPM).
- **VST3 on Linux says what it can do.** Plugin windows are not available on Linux yet; the
  effect window shows a note instead of a button that failed, and you edit the plugin with
  its parameter list.

## 0.9.10 — 2026-11-12

### Added
- **Automation you edit right on the clip, with curves.** No more double-click to start
  editing: drag any point straight away (Shift locks the value, Ctrl the time, Alt skips the
  snap). Drag the dot halfway between two points to bend that stretch; right-click a point for
  *Single curve*, *Double curve (S)*, *Hold* or *Stairs*, or to type in an exact value in the
  control's own unit ("-6 dB", "1.2k"). Right-click empty space to add a point, double-click a
  point to delete it, Ctrl+drag to select several and move or delete them together. Values show
  as the knob shows them while you drag. What you see is exactly what plays.
- **Piano roll: To new pattern (Ctrl+Shift+X).** Select notes and move them into a new pattern
  in the same place. The playlist keeps playing them: every clip of the old pattern gets a clip
  of the new one on a free track below. One undo step.
- **Stacked clips are visible.** Where clips on a track overlap, the header is hatched; a clip
  that hides others completely shows "×2", "×3"…, and clicking it says so.
- **Audio comes back by itself.** If the audio device or driver stops (PipeWire/ALSA restart,
  an interface unplugged), WAVE reopens it and tells you, instead of freezing the transport
  until you change the audio settings.

### Changed
- **Picking or making a pattern from the toolbar opens the Channel Rack.** Browsing to a
  shorter pattern no longer shrinks the rack, and it reopens as wide as you left it.
- **A closed hand while you drag.** Clips, notes and windows show the grabbing cursor for the
  whole drag (it used to stay an open hand).

### Fixed
- **Opening a saved project failed** with "undefined is not an object
  (…hasOwnProperty…)". Missing audio files are now listed when a project opens.
- **Cancelling the save dialog no longer loses work.** "Save" in the unsaved-changes question
  followed by Cancel used to go on and discard the project.
- The mixer no longer leaves a grabbing cursor behind after you reorder effects.

## 0.9.9 — 2026-11-11

### Added
- **Move around like in FL Studio.** Middle-button drag pans the playlist and the Piano roll;
  the view scrolls by itself when you drag a clip, a note or a selection box to its edge;
  Page Up / Page Down zoom at the pointer, Shift+1 / 2 / 3 are zoom presets, Shift+4 shows
  everything, Ctrl + right-drag zooms to a box and Ctrl + right-click goes back.
- **Piano roll:** it follows the playhead; Alt + wheel over a note changes its velocity;
  Shift or Ctrl pressed while dragging locks the pitch or the time; Ctrl+Q quantizes and
  Ctrl+L makes notes legato; double-click a note for its properties; Shift+I inverts the
  selection and Ctrl+click on a piano key selects every note of that pitch. New notes take
  the velocity of the last note you touched, like its length.
- **Channel Rack:** Ctrl+click on a channel's light solos it, Alt + Up / Down moves the
  channel, Ctrl+C / X / V copy steps between channels and patterns, and the wheel over the
  title bar switches pattern.

### Changed
- **The playlist deletes with a right-click, like FL** (right-drag deletes what it crosses).
  A clip's menu now opens from the icon in its top-left corner.
- **A beat plays on the first Space.** New installs start in PAT mode, and in SONG mode an
  empty playlist plays the pattern you are editing instead of silence.
- **Notes sound while you draw them** (Settings can switch that off).
- **The Channel Rack sizes itself and has room to breathe:** taller rows, bigger steps and a
  margin around the grid. Its width follows the pattern's length in both directions and only
  scrolls once it spans the screen; it grows when you add a channel.
- **The Piano roll opens over the whole work area**, like FL's. Double-click its title bar to
  switch between that and a smaller window. Both windows reopen where you left them.
- A double-click on a pattern always opens the Channel Rack (it used to open the Piano roll
  instead when only one channel played the pattern).
- **Split by channel adds the tracks it needs** instead of stacking clips on the last track.
- **Playlist tracks in a new project are plain lanes**, as in FL: none is tied to a mixer
  insert any more (they play into the master). Send a track to an insert from its header when
  you want to; existing projects keep their routing.
- Stop works like Space; pressing it again while stopped goes back to the start of the song.
- Paste on the playlist lands right after what you copied, and keeps chaining.
- A project with nothing on the playlist opens on the Channel Rack.
- Tool, snap, zoom, follow and the panel layout are remembered between sessions.
- Selecting a channel selects its mixer insert; mixer strips also name the channels routed to them.
- Slate theme: much higher contrast.

### Fixed
- **Interface scale:** at 110 % and above the bottom of the window (the mixer) was cut off.
- **Autosave never ran** until its setting was changed once.
- **A sample could show another sample's waveform** after opening or creating a project.
- Escape closes Export, Settings and the channel window; a rename is kept when you click
  elsewhere; the delete button of an effect no longer fires on the way to the bypass light.
- Keys typed in the Piano roll no longer switch the playlist's tool behind it, and the typing
  keyboard no longer turns itself back on at start.

## 0.9.8 — 2026-11-11

### Added
- **Menus: File, Edit, View, Help.** New, open, save, export and settings live in File; undo,
  redo and the clipboard in Edit; the browser, mixer, Channel Rack, Piano roll, follow and
  zoom in View; shortcuts and updates in Help.
- **Piano roll scrollbars that zoom.** Drag a bar to scroll, drag either end of it to zoom
  time or key height. The keys are taller by default.
- **Shift+drag clones notes** in the Piano roll: the selection (or the note under the pointer)
  is copied and the copies move. Shift+click still adds a note to the selection.
- **Hear what you click.** Holding a key on the Piano roll keyboard plays it, and sliding
  across the keys follows; a click on the waveform in a sampler channel's window plays the sample.
- **Slate theme**: a lighter dark theme (Settings -> Appearance).
- Icons for every instrument and preset in the add-channel menu.

### Changed
- **The status bar at the bottom is gone**, so the playlist and mixer get its space. Messages
  and the tool hint now sit in the toolbar under the menus; the transport is centred.
- **Bigger Channel Rack steps** with more room between them and a gap after every beat.
- A right-click on the mixer sends the floating windows behind, like a click on the playlist.
- **Patterns in the browser are bigger, coloured blocks** with their own right-click menu:
  open, rename, clone, split by channel, add or remove a bar, colour, delete.
- A right-click anywhere in the Piano roll clears the selection.

### Fixed
- **Piano roll: a note dragged past the end of the pattern no longer jumps around.** The
  pattern still grows to fit, but the view keeps its scale while you drag.
- **Trimming, fading, splitting and clip volume work on the whole selection.** Drag the edge
  or the fade handle of one selected clip and every selected clip follows; *Split here* and the
  clip volume buttons in the clip menu do the same.
- **A new selection box on the playlist replaces the old selection** (hold Shift to add to it).
  With the pencil, Ctrl+drag used to keep adding, so a selection never went away. Ctrl+click
  on a clip toggles it with the select tool too.
- **Short clips can be moved again.** A clip a few pixels wide was all trim zone; now narrow
  clips keep a body to grab, and a plain click on an edge or a fade handle changes nothing.
- **After Space, the keyboard stays in the Piano roll or Channel Rack.** Delete, Ctrl+C / V and
  the tool keys used to go to the playlist after a play / stop and could delete a clip there.
- Piano roll: picking a grid, key or channel from the lists no longer leaves the arrow keys
  and Delete stuck in that list; switching pattern clears the note selection.
- Channel Rack: painting steps into the greyed extra bar no longer shifts the grid under the
  pointer.

## 0.9.7 — 2026-11-11

### Added
- **A standard library of samples, built in.** Fifty drum, percussion and tonal one-shots
  (808 and acoustic kicks, snares, claps, hats, cymbals, toms, congas, shakers, an 808 bass,
  marimba, glockenspiel) are in the Library tab from the first start: no download. Drag one onto
  a track for an audio clip, onto the Channel Rack for a new sampler channel, or onto an open
  sampler to swap its sample. The "+ Channel" menu lists them too.
- **A real sampler in the channel window.** Drag the start and end markers on the waveform to
  pick the part that plays; Reverse and Normalize; transpose and fine tune; a volume envelope
  (attack, decay, sustain, release) drawn over the waveform; One-shot (drums: the sample rings
  out) or Gate (bass and melodies: it stops with the note); and cut groups, so a closed hi-hat
  silences the open one.
- **EQ: a band bank beside the graph.** Each band has a shape button, a gain slider and
  FREQ and BW knobs next to the curve; bands that are switched off no longer clutter the graph.
  New presets that only cut: below 20 Hz and above 18 kHz, low cut 80 Hz, low cut 150 Hz
  (vocal), high cut 10 kHz.

### Changed
- **Kick, Clap, Hat and Bass are samples now.** The built-in synthesised instruments, which had
  no waveform and no settings, are gone. A new project starts with Kick, Clap, Hat and Snare
  samplers from the standard library, and you can see and edit each of them. Songs saved with
  the old instruments open with the closest library samples and sound different.
- Drum samples start exactly as recorded: the sampler no longer fades in the first 2 ms, which
  dulled the attack.
- Projects that use library samples open on any computer: they no longer store where the
  library is installed.
- WAVE starts with an empty project (eight tracks, the starter kit and an empty Pattern 1)
  instead of a demo beat.
- A click on the playlist's time ruler switches from PAT to SONG; a pattern loop that is playing
  carries on as the song from there. PAT is shown amber and SONG green.

### Fixed
- A click on a channel's mini piano roll now brings the Piano roll to the front instead of
  leaving it behind the Channel Rack.

## 0.9.6 — 2026-11-11

### Added
- **Patterns like FL Studio.** The current pattern is picked in the toolbar (menu, mouse
  wheel, Num 1-9, + / -); F4 jumps to the next empty one, F2 renames, Shift+Ctrl+C clones.
- **Channel Rack (F6) and Piano roll (F7) are windows of their own.** The rack has compact step
  buttons, a greyed extra bar (click there to make the pattern longer), a graph editor (G) for
  each step's velocity and key, and *Replace* to swap a channel's instrument while keeping its
  notes and routing. The effect picker moved to Shift+F9.
- **Piano roll for every channel**, drums included. Notes of the pattern's other channels show
  as ghost notes (double right-click one to switch to it), and drawing past the end makes the
  pattern longer: its length follows the notes.

### Changed
- **Much smoother editing in big songs.** Drawing and dragging notes in the Piano roll and the
  Channel Rack, turning a channel's knobs and moving a track fader no longer redraw the whole
  playlist (about 3x the frame rate on a 64-track, 1400-clip song).
- **Zooming the playlist stays fluid** on heavy songs: the view follows the wheel at once and
  sharpens when you stop.
- Resizing or opening the mixer no longer redraws the playlist on every frame.

### Fixed
- **Lower CPU after a phrase ends.** Effects fading out on silence (reverb and delay tails,
  filters) could cost more than the music itself; that part of the load is about a quarter lower.
- Dragging one clip on a big song sometimes redrew the whole playlist on every step.

## 0.9.5 — 2026-11-11

### Fixed
- **Chords from the computer keyboard and MIDI keyboard.** Keys you play live now sound
  through the instrument's own mixer track, exactly like the piano roll. Before, they went
  through the track armed for recording (in the starter project the Vocal track, with its
  compressor), which squashed chords and made them sound unnatural.
- **Shift+click selects notes in the piano roll with any tool.** With the pencil, Shift+click
  used to move the clicked note and drop the rest of the selection, so Ctrl+C copied only one
  note. Shift+click now adds or removes a note, Shift+drag adds a box of notes.

## 0.9.4 — 2026-11-11

### Fixed
- **Chords sound like their notes.** Playing several keys at once on the Synth or WAVE Keys
  no longer changes the sound: every note in a chord now sounds exactly as it does alone.
  Before, each new key pulled the others down and the tone pumped and thinned out.
- **Drive on chords.** Poly synth presets with drive (plucks, Keys Dance, Future Chord, arps,
  Brass and more) now saturate each note on its own, so chords stay clean instead of turning
  into a gritty mush.
- **Glide on chords.** With glide on a poly patch, the notes of a chord no longer slide from
  one another: they all glide from the note you played before the chord.

### Good to know
- Poly synth patches and WAVE Keys now play 12 dB quieter, so a full chord fits without
  clipping. Turn the channel up if a single line feels quiet.
- In older projects, pick a drive preset again to get its new per-note drive.

## 0.9.3 — 2026-11-10

### Added
- **Updates inside the app.** WAVE checks GitHub for a new version a few seconds after it
  starts, then every few hours while it stays open. When one is found, a small
  **Update 0.9.x** button appears in the status bar: it shows what is new, downloads the
  installer in the background (you keep working) and only restarts when you say so.
- **What's new after every update.** The first time you start a new version, its release
  notes open once, including the versions you skipped. Settings → General → Updates opens
  them again at any time.
- Settings → General → **Updates**: current version, **Check for updates**, and a switch for
  the automatic check.

### Good to know
- This is the first version that can update itself: install it by hand once, later versions
  arrive in the app.
- Updates are signed; WAVE refuses an installer whose signature does not match.

## 0.9.2 — 2026-11-10

### Added
- **Automate every knob.** Right-click any knob for *Automate* on the playlist, *Select
  automation clips* and *Reset to default*. New automation targets: channel volume and pan
  (Channel Rack), every synth parameter, the WAVE Keys panel, and an audio clip's own gain
  and pan.

### Fixed
- Changing the song during playback no longer drops automated values for a moment (a full
  0 dB blip on the first block after an edit).

## 0.9.1 — 2026-11-10

### Fixed
- **WAVE Keys and the Synth no longer clip on chords.** Both pass an output limiter at -1 dBFS;
  a single note keeps its level.
- **Smoother playlist drags.** Moving clips, dragging the loop range and rubber-band selection
  only repaint what changes (about 60 fps on a 64-track, 1400-clip song instead of 39).

### Added
- **Half-Speed** effect: a tempo-locked slow-down (1.5x / 2x / 4x per loop window).
- 14 new synth presets, harder basses and wider leads.

## 0.9.0 — 2026-11-10

### Added
- **English interface by default, Polish in Settings.** About 1 280 strings are translated.
- New app icon.
- Windows installer built with ASIO and the WAVE Keys piano included.
