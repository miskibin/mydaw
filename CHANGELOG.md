# Changelog

What changed in each release of WAVE. The newest version comes first; its section is
shown in the app after an update and is published as the release notes (see
`docs/releasing.md`). Write for musicians: what you can do now, not how it works inside.

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
