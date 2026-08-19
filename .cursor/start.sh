#!/usr/bin/env bash
# MYDAW Cloud Agent start: per-boot runtime reconciliation.
#
# There is no real sound card in the VM, so cpal (WASAPI on Windows, ALSA here)
# has no device to open. We bring up a PulseAudio null sink and point ALSA's
# default device at it, which gives the audio engine a real output device: the
# transport runs, meters move, and MYDAW_DEBUG_CAPTURE can record the bus.
# Idempotent: safe to run on every boot and tolerant of an already-running
# daemon.
set -euo pipefail

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/tmp/xdg-runtime-$(id -u)}"
mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"

# Route ALSA's default PCM/CTL through PulseAudio.
if [ ! -f "$HOME/.asoundrc" ]; then
  cat > "$HOME/.asoundrc" <<'ASOUND'
pcm.!default { type pulse }
ctl.!default { type pulse }
ASOUND
fi

# Start PulseAudio if it is not already up.
if ! pulseaudio --check 2>/dev/null; then
  pulseaudio --start --exit-idle-time=-1
  # Give the daemon a moment to accept clients.
  for _ in 1 2 3 4 5; do
    pulseaudio --check 2>/dev/null && break
    sleep 1
  done
fi

# Ensure a null output sink exists and is the default.
if ! pactl list short sinks 2>/dev/null | grep -q '\bmydaw_out\b'; then
  pactl load-module module-null-sink \
    sink_name=mydaw_out \
    sink_properties=device.description=MyDAW_Null_Output >/dev/null
fi
pactl set-default-sink mydaw_out 2>/dev/null || true

echo "start: PulseAudio null sink ready"
pactl list short sinks 2>/dev/null || true
