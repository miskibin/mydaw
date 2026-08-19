#!/usr/bin/env bash
# MYDAW Cloud Agent install: idempotent repository bootstrap.
#
# Runs after the repository is checked out. Prepares everything an agent needs
# to build and test the Rust workspace and the SolidJS/Vite UI on Linux. Safe to
# run repeatedly: system packages, the Rust toolchain and JS deps all converge.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

# --- system packages ------------------------------------------------------
# Tauri v2 on Linux needs WebKitGTK + GTK dev headers; cpal needs ALSA; the
# headless audio path is exercised through a PulseAudio null sink (see start.sh).
# Guard on a marker package so this is a fast no-op once the libs are present.
if ! pkg-config --exists webkit2gtk-4.1 2>/dev/null \
   || ! pkg-config --exists alsa 2>/dev/null; then
  echo "install: installing system packages (WebKitGTK, GTK, ALSA, PulseAudio)"
  sudo apt-get update
  sudo apt-get install -y --no-install-recommends \
    build-essential pkg-config file curl wget git \
    libwebkit2gtk-4.1-dev libgtk-3-dev libxdo-dev libssl-dev \
    libayatana-appindicator3-dev librsvg2-dev \
    libasound2-dev libasound2-plugins \
    pulseaudio pulseaudio-utils xvfb
else
  echo "install: system packages already present"
fi

# --- Rust toolchain -------------------------------------------------------
# The lockfile pulls crates that require the edition2024 cargo feature, so the
# workspace needs Rust >= 1.85. Ensure a recent stable with clippy + rustfmt.
if command -v rustup >/dev/null 2>&1; then
  rustup toolchain install stable --profile minimal \
    --component clippy --component rustfmt --no-self-update
  rustup default stable
fi
echo "install: $(rustc --version)"

# --- JavaScript / UI ------------------------------------------------------
# pnpm ships with the base image via corepack; fall back to enabling it.
if ! command -v pnpm >/dev/null 2>&1; then
  corepack enable
fi
pnpm --dir ui install --frozen-lockfile

# The Rust app crate embeds ui/dist via tauri-build, so the UI must be built
# before (or alongside) the workspace build.
pnpm --dir ui build

# --- Rust workspace -------------------------------------------------------
# Warm the build cache so the first agent command is fast.
cargo build --workspace

echo "install: done"
