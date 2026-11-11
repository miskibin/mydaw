#!/usr/bin/env python3
"""Builds the factory sample library that ships inside the app.

Takes CC0 one-shots from the kits pinned in assets-manifest.json (download them first, e.g.
with the app's Library tab, or point --kits at a folder of `<kit id>/<file>`), then for
every pick: trims silence before the hit and the dead tail, fades the end, normalises the
peak to -1 dBFS and writes 16-bit FLAC. Also writes the table the engine embeds
(crates/engine/src/factory_files.rs).

    python3 scripts/make-factory-library.py --kits <folder>

Needs ffmpeg and numpy.
"""
import argparse, os, subprocess, sys
import numpy as np

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "crates", "engine", "library")
SR = 44100

# (output file, display name, group, kit, source file, max seconds, root key)
PICKS = [
    ("kick-808.flac", "808 Kick", "Kicks", "808-kit", "kick-tone50-decay50.wav", 1.2, 60),
    ("kick-808-short.flac", "808 Kick Short", "Kicks", "808-kit", "kick-tone00-decay25.wav", 0.5, 60),
    ("kick-808-tight.flac", "808 Kick Tight", "Kicks", "808-kit", "kick-tone50-decay00.wav", 0.25, 60),
    ("kick-808-bright.flac", "808 Kick Bright", "Kicks", "808-kit", "kick-tone100-decay25.wav", 0.5, 60),
    ("kick-studio.flac", "Studio Kick", "Kicks", "studio-drums", "kick-v4.wav", 0.9, 60),
    ("kick-studio-soft.flac", "Studio Kick Soft", "Kicks", "studio-drums", "kick-v3.wav", 0.9, 60),
    ("kick-cajon.flac", "Cajon Kick", "Kicks", "percussion-fx", "cajon-bass-v3.wav", 0.9, 60),
    ("snare-808.flac", "808 Snare", "Snares", "808-kit", "snare-tone50-snap75.wav", 0.5, 60),
    ("snare-808-bright.flac", "808 Snare Bright", "Snares", "808-kit", "snare-tone100-snap75.wav", 0.5, 60),
    ("snare-808-dark.flac", "808 Snare Dark", "Snares", "808-kit", "snare-tone00-snap75.wav", 0.5, 60),
    ("rimshot-808.flac", "808 Rimshot", "Snares", "808-kit", "rimshot.wav", 0.25, 60),
    ("snare-studio.flac", "Studio Snare", "Snares", "studio-drums", "snare-v4.wav", 1.2, 60),
    ("snare-studio-2.flac", "Studio Snare 2", "Snares", "studio-drums", "snare2-v3.wav", 1.0, 60),
    ("sidestick-studio.flac", "Studio Side Stick", "Snares", "studio-drums", "side-stick.wav", 0.6, 60),
    ("cajon-slap.flac", "Cajon Slap", "Snares", "percussion-fx", "cajon-slap-v2.wav", 0.8, 60),
    ("clap-808.flac", "808 Clap", "Claps", "808-kit", "clap.wav", 1.0, 60),
    ("clap-hand.flac", "Hand Clap", "Claps", "percussion-fx", "clap-v4.wav", 0.45, 60),
    ("clap-hand-soft.flac", "Hand Clap Soft", "Claps", "percussion-fx", "clap-v3.wav", 0.6, 60),
    ("hat-808-closed.flac", "808 Closed Hat", "Hats", "808-kit", "hat-closed.wav", 0.25, 60),
    ("hat-808-open.flac", "808 Open Hat", "Hats", "808-kit", "hat-open-decay75.wav", 0.75, 60),
    ("hat-808-pedal.flac", "808 Pedal Hat", "Hats", "808-kit", "hat-open-decay00.wav", 0.25, 60),
    ("hat-studio-closed.flac", "Studio Closed Hat", "Hats", "studio-drums", "hat-closed-v4.wav", 0.5, 60),
    ("hat-studio-open.flac", "Studio Open Hat", "Hats", "studio-drums", "hat-open.wav", 2.5, 60),
    ("hat-studio-pedal.flac", "Studio Pedal Hat", "Hats", "studio-drums", "hat-pedal.wav", 0.5, 60),
    ("cymbal-808.flac", "808 Cymbal", "Cymbals", "808-kit", "cymbal-tone50-decay50.wav", 2.5, 60),
    ("crash-studio.flac", "Studio Crash", "Cymbals", "studio-drums", "crash-v3.wav", 5.0, 60),
    ("ride-studio.flac", "Studio Ride", "Cymbals", "studio-drums", "ride-v3.wav", 3.0, 60),
    ("ride-bell-studio.flac", "Studio Ride Bell", "Cymbals", "studio-drums", "ride-bell-v2.wav", 2.5, 60),
    ("tom-808-low.flac", "808 Low Tom", "Toms", "808-kit", "tom-low.wav", 1.0, 60),
    ("tom-808-mid.flac", "808 Mid Tom", "Toms", "808-kit", "tom-mid.wav", 1.0, 60),
    ("tom-808-high.flac", "808 High Tom", "Toms", "808-kit", "tom-high.wav", 1.0, 60),
    ("tom-studio-low.flac", "Studio Low Tom", "Toms", "studio-drums", "tom-low-v3.wav", 1.8, 60),
    ("tom-studio-high.flac", "Studio High Tom", "Toms", "studio-drums", "tom-high-v3.wav", 1.6, 60),
    ("cowbell-808.flac", "808 Cowbell", "Percussion", "808-kit", "cowbell.wav", 0.8, 60),
    ("conga-808-high.flac", "808 High Conga", "Percussion", "808-kit", "conga-high.wav", 0.5, 60),
    ("conga-808-low.flac", "808 Low Conga", "Percussion", "808-kit", "conga-low.wav", 0.5, 60),
    ("maracas-808.flac", "808 Maracas", "Percussion", "808-kit", "maracas.wav", 0.25, 60),
    ("claves-808.flac", "808 Claves", "Percussion", "808-kit", "claves.wav", 0.25, 60),
    ("shaker.flac", "Shaker", "Percussion", "percussion-fx", "shaker-large.wav", 0.7, 60),
    ("tambourine.flac", "Tambourine", "Percussion", "percussion-fx", "tambourine-v2.wav", 1.0, 60),
    ("bongo-high.flac", "High Bongo", "Percussion", "percussion-fx", "bongo-high-v3.wav", 0.8, 60),
    ("bongo-low.flac", "Low Bongo", "Percussion", "percussion-fx", "bongo-low-v3.wav", 0.9, 60),
    ("woodblock.flac", "Woodblock", "Percussion", "percussion-fx", "woodblock-low-v2.wav", 0.5, 60),
    ("triangle.flac", "Triangle", "Percussion", "percussion-fx", "triangle-open-v2.wav", 3.0, 60),
    ("cabasa.flac", "Cabasa", "Percussion", "percussion-fx", "cabasa.wav", 0.6, 60),
    # tonal: the root key is measured (see `pitch`) so the piano plays them in tune
    ("bass-808.flac", "808 Bass", "Tonal", "808-bass", "bass-tone100-decay100.wav", 3.0, None),
    ("bass-808-short.flac", "808 Bass Short", "Tonal", "808-bass", "bass-tone75-decay75.wav", 1.2, None),
    ("marimba.flac", "Marimba", "Tonal", "marimba", "marimba-C4-loud.wav", 2.5, 60),
    ("marimba-low.flac", "Marimba Low", "Tonal", "marimba", "marimba-C2-loud.wav", 3.5, 36),
    ("glockenspiel.flac", "Glockenspiel", "Tonal", "glockenspiel", "glockenspiel-C6-loud.wav", 3.0, 84),
]


def read(path):
    raw = subprocess.run(["ffmpeg", "-v", "error", "-i", path, "-f", "f32le", "-ac", "2", "-ar", str(SR), "-"], capture_output=True, check=True).stdout
    return np.frombuffer(raw, dtype=np.float32).reshape(-1, 2).astype(np.float64)


def pitch(x):
    """MIDI key (float) of the fundamental, by autocorrelation of the sustained part."""
    m = x.mean(axis=1)
    a = m[int(0.08 * SR):int(0.08 * SR) + 16384]
    a = a - a.mean()
    ac = np.correlate(a, a, "full")[len(a) - 1:]
    lo, hi = int(SR / 400), int(SR / 25)
    lag = lo + int(np.argmax(ac[lo:hi]))
    # parabolic refinement
    y0, y1, y2 = ac[lag - 1], ac[lag], ac[lag + 1]
    lag += 0.5 * (y0 - y2) / (y0 - 2 * y1 + y2)
    return 69 + 12 * np.log2(SR / lag / 440)


def prepare(x, max_s):
    env = np.abs(x).max(axis=1)
    peak = env.max()
    # start half a millisecond before the hit
    first = int(np.argmax(env > peak * 10 ** (-40 / 20)))
    start = max(0, first - int(0.0005 * SR))
    x = x[start:]
    env = env[start:]
    # drop the tail once it stays 60 dB under the peak, and cap the length
    loud = np.nonzero(env > peak * 1e-3)[0]
    end = min(len(x), int(loud[-1]) + int(0.02 * SR), int(max_s * SR))
    x = x[:end].copy()
    fade = max(int(0.02 * SR), int(0.15 * len(x))) if end < len(env) else int(0.01 * SR)
    fade = min(fade, len(x))
    x[-fade:] *= (np.cos(np.linspace(0, np.pi, fade)) * 0.5 + 0.5)[:, None]
    lead = min(int(0.0003 * SR), len(x))
    if start > 0:
        x[:lead] *= np.linspace(0, 1, lead)[:, None]
    x *= 10 ** (-1 / 20) / np.abs(x).max()
    mono = np.abs(x[:, 0] - x[:, 1]).max() < 1e-4
    return (x[:, :1] if mono else x), mono


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--kits", required=True)
    a = ap.parse_args()
    os.makedirs(OUT, exist_ok=True)
    for f in os.listdir(OUT):
        os.remove(os.path.join(OUT, f))
    rows, total = [], 0
    for out, name, group, kit, src, max_s, root in PICKS:
        x = read(os.path.join(a.kits, kit, src))
        if root is None:
            root = int(round(pitch(x)))
        y, mono = prepare(x, max_s)
        dst = os.path.join(OUT, out)
        pcm = (np.clip(y, -1, 1) * 32767).round().astype("<i2").tobytes()
        subprocess.run(["ffmpeg", "-v", "error", "-y", "-f", "s16le", "-ar", str(SR), "-ac", "1" if mono else "2", "-i", "-", "-compression_level", "8", "-bitexact", "-map_metadata", "-1", dst], input=pcm, check=True)
        total += os.path.getsize(dst)
        rows.append((out, name, group, root, len(y) / SR, 1 if mono else 2))
        print(f"{out:28} {len(y) / SR:5.2f} s  {'mono' if mono else 'stereo':6} root {root:3} {os.path.getsize(dst) // 1024:4} K")
    with open(os.path.join(ROOT, "crates", "engine", "src", "factory_files.rs"), "w", newline="\n") as f:
        f.write("// Generated by scripts/make-factory-library.py; do not edit by hand.\n\n")
        f.write("use super::FactoryFile;\n\n")
        f.write("pub(super) const FILES: &[FactoryFile] = &[\n")
        for out, name, group, root, dur, ch in rows:
            f.write(f'    FactoryFile {{ file: "{out}", name: "{name}", group: "{group}", root: {root}, dur: {dur:.4f}, ch: {ch}, bytes: include_bytes!("../library/{out}") }},\n')
        f.write("];\n")
    print(f"{len(rows)} files, {total / 1e6:.2f} MB")


if __name__ == "__main__":
    sys.exit(main())
