# Releasing WAVE

A release is one commit; the workflow does the rest. Installed copies find it by themselves
(Settings → General → Updates, or the button beside the menus) and show its notes.

## Each release

1. Bump `version` in `app/tauri.conf.json` (the version the app, the installers and the updater use).
2. Add a section to the top of `CHANGELOG.md` (or rename `## Unreleased`, where changes made
   since the last release collect, and drop its reminder line): `## 0.9.4 — 2026-12-01`, then `### Added`,
   `### Changed`, `### Fixed` with bullets written for musicians. The release workflow stops
   when the section is missing or empty, so a release cannot go out without notes.
3. Push to `main` with `[release]` in the commit message (or push a `v*` tag, or run the
   workflow by hand). `tauri-action` builds Windows (with ASIO) and Linux one after the other,
   signs the installers, creates the GitHub release and uploads `latest.json`.

## What a release contains

* Windows: `WAVE_<v>_x64-setup.exe` (NSIS). No `.msi`: it was the same app a second time, and
  the updater installs the NSIS one anyway (`updaterJsonPreferNsis`).
* Linux: `.AppImage` (any distribution; the most downloaded), `.deb` and `.rpm`.
* A `.sig` beside each (the updater's signature) and `latest.json`.

The bundle list is `bundles` in the matrix of `.github/workflows/release.yml`. Every installer
carries the WAVE Keys piano, which `scripts/fetch-keys.mjs` cuts to what the app plays before
the build (138 MB of downloads become about 23 MB; see `docs/keys.md`). The AppImage is the
largest because it also carries its own WebKit and GTK.

## What the updater uses

* `latest.json` on the latest release, found at
  `https://github.com/miskibin/wave-DAW/releases/latest/download/latest.json`. Pre-releases
  are not "latest", so they never reach users.
* The `notes` in `latest.json` are the release body above the first `---` line: the CHANGELOG
  section. The part under the rule (install tips) stays on the release page.
* Every installer is signed with a minisign key. The app refuses a download whose signature
  does not match `plugins.updater.pubkey` in `app/tauri.conf.json`.
* After an update, the first start shows the notes of every version since the last one
  shown (`seenVersion` in `settings.json`), read from the `CHANGELOG.md` bundled into the UI.

## The signing key

* The **private** key lives in the repository secret `TAURI_SIGNING_PRIVATE_KEY` (Settings →
  Secrets and variables → Actions). It has no password; if you add one, also set
  `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. Keep a backup outside GitHub: **without it no update
  can be published to existing installs**, and replacing the key means every user installs by hand once.
* Local builds do not need it: `createUpdaterArtifacts` is switched on only by
  `app/tauri.release.conf.json`, which the release workflow passes with `--config`.
* A new pair: `pnpm --dir ui dlx @tauri-apps/cli signer generate -w <file>`; put the contents of
  `<file>.pub` into `plugins.updater.pubkey`.

## First release with the updater

Copies older than 0.9.3 have no updater: install 0.9.3 by hand once. To test the whole round
trip, publish two consecutive versions and update the first from the second.
