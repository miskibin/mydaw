"""Shared paths, audio I/O and small helpers for the auto-vocal research prototype."""
from __future__ import annotations

import io
import json
import os
import pathlib
import subprocess

import numpy as np
import soundfile as sf

DATA = pathlib.Path(os.environ.get("MYDAW_DATA", r"D:\mydaw-data"))
RAW = DATA / "raw"
CACHE = DATA / "auto_vocal_cache"
CACHE.mkdir(parents=True, exist_ok=True)
HERE = pathlib.Path(__file__).resolve().parent.parent
RESULTS = HERE / "results"
RESULTS.mkdir(exist_ok=True)

NOTE = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"]
_FLAT = {"Db": "C#", "Eb": "D#", "Gb": "F#", "Ab": "G#", "Bb": "A#", "Cb": "B", "Fb": "E",
         "E#": "F", "B#": "C"}


def pc_of(name: str) -> int:
    name = name.strip()
    name = name[0].upper() + name[1:]
    name = _FLAT.get(name, name)
    return NOTE.index(name)


def key_str(root: int, minor: bool) -> str:
    return f"{NOTE[root]} {'minor' if minor else 'major'}"


def load(path, sr=22050, mono=True, offset=0.0, duration=None) -> np.ndarray:
    """Decode any audio (wav/mp3/mp4/au) to float32 via ffmpeg; shape (n,) or (ch, n)."""
    cmd = ["ffmpeg", "-v", "error", "-nostdin"]
    if offset:
        cmd += ["-ss", str(offset)]
    if duration:
        cmd += ["-t", str(duration)]
    cmd += ["-i", str(path), "-f", "f32le", "-ar", str(sr)]
    if mono:
        cmd += ["-ac", "1"]
    cmd += ["-"]
    raw = subprocess.run(cmd, capture_output=True, check=True).stdout
    x = np.frombuffer(raw, np.float32)
    if not mono:
        info = subprocess.run(["ffprobe", "-v", "error", "-select_streams", "a:0", "-show_entries",
                               "stream=channels", "-of", "csv=p=0", str(path)],
                              capture_output=True, text=True).stdout.strip()
        ch = int(info.split("\n")[0] or 2)
        x = x.reshape(-1, ch).T
    return x.copy()


def load_bytes(data: bytes, sr=22050) -> np.ndarray:
    x, s = sf.read(io.BytesIO(data), dtype="float32", always_2d=True)
    x = x.mean(1)
    if s != sr:
        import soxr
        x = soxr.resample(x, s, sr)
    return x


def write(path, x, sr):
    path = pathlib.Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    x = np.asarray(x, np.float32)
    sf.write(str(path), x.T if x.ndim == 2 else x, sr, subtype="PCM_24")


def save_json(name, obj):
    p = RESULTS / name
    p.write_text(json.dumps(obj, indent=1, default=float))
    return p


def db(x, eps=1e-12):
    return 10 * np.log10(np.maximum(x, eps))
