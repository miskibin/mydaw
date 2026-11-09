"""Fetch MUSDB18 (Zenodo 1117372; research/non-commercial use) track by track over HTTP
range requests (no 4.4 GB zip on disk), decode the vocal stem and the accompaniment
(drums + bass + other) and keep the 45 s with the most vocal activity as float16 mono
at 44.1 kHz in D:\\mydaw-data\\musdb_excerpts\\<split>__<name>.npz.
"""
from __future__ import annotations

import pathlib
import subprocess
import tempfile

import numpy as np
from remotezip import RemoteZip

from .common import DATA

URL = "https://zenodo.org/records/1117372/files/musdb18.zip?download=1"
OUT = DATA / "musdb_excerpts"
OUT.mkdir(parents=True, exist_ok=True)
SR = 44100
SECONDS = 45


def stream(path, idx):
    raw = subprocess.run(["ffmpeg", "-v", "error", "-nostdin", "-i", str(path), "-map", f"0:a:{idx}", "-f", "f32le",
                          "-ac", "1", "-ar", str(SR), "-"], capture_output=True, check=True).stdout
    return np.frombuffer(raw, np.float32)


def main():
    with RemoteZip(URL) as z:
        infos = [i for i in z.infolist() if i.filename.endswith(".stem.mp4")]
        for i in infos:
            split, name = i.filename.split("/")[0], pathlib.Path(i.filename).name.replace(".stem.mp4", "")
            dst = OUT / f"{split}__{name}.npz"
            if dst.exists():
                continue
            with tempfile.TemporaryDirectory() as td:
                p = z.extract(i, td)
                drums, bass, other, voc = (stream(p, k) for k in (1, 2, 3, 4))
            n = min(map(len, (drums, bass, other, voc)))
            acc = drums[:n] + bass[:n] + other[:n]
            voc = voc[:n]
            # pick the 45 s window with the most vocal energy (1 s resolution)
            e = np.convolve((voc[: n // SR * SR].reshape(-1, SR) ** 2).mean(1), np.ones(SECONDS), "valid")
            s = int(np.argmax(e)) * SR if len(e) else 0
            seg = slice(s, s + SECONDS * SR)
            np.savez_compressed(dst, voc=voc[seg].astype(np.float16), acc=acc[seg].astype(np.float16),
                                sr=SR, start=s / SR)
            print(split, name, flush=True)
    print("done")


if __name__ == "__main__":
    main()
