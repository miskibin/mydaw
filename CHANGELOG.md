# Changelog

What changed in each release of WAVE. The newest version comes first; its section is
shown in the app after an update and is published as the release notes (see
`docs/releasing.md`). Write for musicians: what you can do now, not how it works inside.

## Unreleased

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
