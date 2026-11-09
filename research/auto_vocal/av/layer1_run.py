"""Layer 1 — beat analysis. Runs every candidate tempo/key method over one dataset and
appends one JSON line per track to results/cache/l1_<dataset>.jsonl (resumable).

usage: python -m av.layer1_run giantsteps_key|giantsteps_tempo|gtzan [--limit N]

Methods
  key  : chroma_cqt + {Krumhansl-Kessler, Temperley, Albrecht-Shanahan} profiles (librosa),
         WAVE's built-in estimator (crates/engine/src/music.rs, via rust_music_bench),
         learned profiles (scored later in layer1_score.py from the cached mean chroma).
  tempo: librosa onset-autocorrelation tempo, WAVE built-in, Beat This! (Foscarin et al.
         2024, MIT) tempo = 60 / median inter-beat interval.
"""
from __future__ import annotations

import argparse
import json
import os
import pathlib
import re
import subprocess
import tarfile
import tempfile
import zipfile

import numpy as np

from .common import CACHE, DATA, RAW, load, load_bytes, pc_of, write

SR = 22050
RUST = pathlib.Path(os.environ.get("CARGO_TARGET_DIR", r"D:\mydaw-data\cargo_target")) / "release" / "music-bench.exe"
GST = DATA / "gs_tempo_ann" / "giantsteps-tempo-dataset-0b7d47ba8cae59d3535a02e3db69e2cf6d0af5bb"
GTZ_KEY = DATA / "gtzan_key" / "dataset-gtzan-key-master" / "gtzan_key" / "genres"
GTZ_TEMPO = DATA / "gtzan_tempo" / "gtzan_tempo_beat-main" / "tempo"
GTZ_PC = ["A", "A#", "B", "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#"]


def gs_key_tracks():
    lab = {}
    with zipfile.ZipFile(RAW / "gs_keys.zip") as z:
        for n in z.namelist():
            if n.startswith("keys_gs+/") and n.endswith(".txt"):
                tid = n.split("/")[1].split(" ")[0]
                txt = z.read(n).decode("utf8", "replace").strip()
                m = re.match(r"([A-G][b#]?)\s+(major|minor)", txt)
                if m:
                    lab[tid] = (pc_of(m.group(1)), m.group(2) == "minor", txt)
    z = zipfile.ZipFile(RAW / "gs_key_audio.zip")
    for n in z.namelist():
        if not n.startswith("audio/") or not n.endswith(".mp3"):
            continue
        tid = n.split("/")[1].split(" ")[0]
        if tid in lab:
            root, minor, txt = lab[tid]
            yield tid, (lambda n=n: _mp3_bytes(z, n)), {"key_root": root, "key_minor": minor, "key_label": txt}


def _mp3_bytes(z, n):
    with tempfile.NamedTemporaryFile(suffix=".mp3", delete=False) as f:
        f.write(z.read(n))
    try:
        return load(f.name, SR)
    finally:
        os.unlink(f.name)


def gs_tempo_tracks():
    for p in sorted((GST / "annotations" / "tempo").glob("*.bpm")):
        tid = p.name.split(".")[0]
        a = DATA / "gs_tempo_audio" / f"{tid}.LOFI.mp3"
        if not a.exists():
            continue
        v1 = float(p.read_text().split()[0])
        t1, t2, s1 = map(float, (GST / "annotations_v2" / "mirex" / f"{tid}.LOFI.mirex").read_text().split())
        yield tid, (lambda a=a: load(a, SR)), {"bpm_v1": v1, "bpm_v2": t1 if s1 >= 0.5 else t2,
                                              "bpm_v2_pair": [t1, t2, s1]}


def gtzan_tracks():
    tf = tarfile.open(RAW / "gtzan" / "data" / "genres.tar.gz", "r:gz")
    for m in tf:
        base = pathlib.Path(m.name).name  # blues.00000.wav (HF mirror) or .au (original)
        if base.startswith("._") or not base.endswith((".au", ".wav")):
            continue
        genre, num, _ = base.split(".")
        ref = {"genre": genre}
        kp = GTZ_KEY / genre / f"{genre}.{num}.lerch.txt"
        if kp.exists():
            k = int(kp.read_text().split()[0])
            if k >= 0:
                ref.update(key_root=pc_of(GTZ_PC[k % 12]), key_minor=k >= 12)
        tp = GTZ_TEMPO / f"gtzan_{genre}_{num}.bpm"
        if tp.exists():
            ref["bpm"] = float(tp.read_text().split()[0])
        data = tf.extractfile(m).read()
        yield f"{genre}.{num}", (lambda d=data: load_bytes(d, SR)), ref


# ---------------------------------------------------------------- methods

PROFILES = {
    "kk": ([6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88],
           [6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17]),
    "temperley": ([5.0, 2.0, 3.5, 2.0, 4.5, 4.0, 2.0, 4.5, 2.0, 3.5, 1.5, 4.0],
                  [5.0, 2.0, 3.5, 4.5, 2.0, 4.0, 2.0, 4.5, 3.5, 2.0, 1.5, 4.0]),
    "albrecht": ([0.238, 0.006, 0.111, 0.006, 0.137, 0.094, 0.016, 0.214, 0.009, 0.080, 0.008, 0.081],
                 [0.220, 0.006, 0.104, 0.123, 0.019, 0.103, 0.012, 0.214, 0.062, 0.022, 0.061, 0.052]),
}


def profile_key(chroma_mean, maj, mnr):
    best = (-2, 0, False)
    for minor, prof in ((False, maj), (True, mnr)):
        prof = np.asarray(prof, float)
        for r in range(12):
            c = np.corrcoef(chroma_mean, np.roll(prof, r))[0, 1]
            if c > best[0]:
                best = (c, r, minor)
    return best[1], best[2]


def chroma_features(y):
    """One 36-bins/octave CQT (C1..C8) folded to chroma; the three bins per semitone
    absorb tuning offsets up to ±1/3 semitone, so no separate tuning pass is needed
    (librosa.estimate_tuning cost 10 s per 2-min track). Each frame is max-normalised
    so loud sections do not outvote quiet ones; bass chroma = the lowest three octaves."""
    import librosa
    C = np.abs(librosa.cqt(y, sr=SR, hop_length=4096, fmin=librosa.note_to_hz("C1"), n_bins=252,
                           bins_per_octave=36))
    fold = librosa.filters.cq_to_chroma(252, bins_per_octave=36, n_chroma=12, fmin=librosa.note_to_hz("C1"))
    fb = librosa.filters.cq_to_chroma(108, bins_per_octave=36, n_chroma=12, fmin=librosa.note_to_hz("C1"))
    c = fold @ C
    cb = fb @ C[:108]
    norm = lambda m: m / np.maximum(m.max(0, keepdims=True), 1e-9)  # noqa: E731
    # fine tuning: energy-weighted circular mean over the 3 sub-semitone bins
    e = (C ** 2).sum(1).reshape(-1, 3).sum(0)
    tuning = float(np.angle((e * np.exp(2j * np.pi * np.array([0, 1, 2]) / 3)).sum()) / (2 * np.pi))
    return norm(c).mean(1), norm(cb).mean(1), tuning


_beat = None


def beat_this_tempo(y):
    global _beat
    from beat_this.inference import Audio2Beats
    if _beat is None:
        _beat = Audio2Beats(checkpoint_path="final0", device="cpu", dbn=False)
    beats, _ = _beat(y, SR)
    if len(beats) < 3:
        return float("nan")
    return float(60.0 / np.median(np.diff(beats)))


def rust(paths):
    lst = tempfile.NamedTemporaryFile("w", suffix=".txt", delete=False)
    lst.write("\n".join(str(p) for p in paths))
    lst.close()
    out = subprocess.run([str(RUST), lst.name], capture_output=True, text=True).stdout
    os.unlink(lst.name)
    res = {}
    for line in out.splitlines():
        p, bpm, root, minor, tun = line.split("\t")
        res[p] = (float(bpm), int(root), bool(int(minor)), float(tun))
    return res


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("dataset")
    ap.add_argument("--limit", type=int, default=0)
    a = ap.parse_args()
    import librosa
    import torch
    torch.set_num_threads(int(os.environ.get("AV_THREADS", "3")))
    it = {"giantsteps_key": gs_key_tracks, "giantsteps_tempo": gs_tempo_tracks, "gtzan": gtzan_tracks}[a.dataset]
    out = CACHE / f"l1_{a.dataset}.jsonl"
    done = set()
    if out.exists():
        done = {json.loads(line)["id"] for line in out.read_text().splitlines() if line.strip()}
    tmpdir = pathlib.Path(tempfile.mkdtemp(prefix="l1_"))
    n = 0
    with out.open("a") as fo:
        for tid, get, ref in it():
            if tid in done:
                continue
            try:
                y = get()
            except Exception as e:  # noqa: BLE001  (GTZAN jazz.00054 is a known corrupt file)
                print("skip", tid, e, flush=True)
                continue
            if len(y) < SR * 5:
                continue
            rec = {"id": tid, "ref": ref, "dur": len(y) / SR}
            cm, cb, tun = chroma_features(y)
            rec["chroma"] = cm.round(5).tolist()
            rec["bass_chroma"] = cb.round(5).tolist()
            rec["tuning"] = tun
            for name, (maj, mnr) in PROFILES.items():
                rec[f"key_{name}"] = profile_key(cm, maj, mnr)
            onset = librosa.onset.onset_strength(y=y, sr=SR)
            rec["bpm_librosa"] = float(librosa.feature.tempo(onset_envelope=onset, sr=SR)[0])
            if a.dataset != "giantsteps_key":  # no tempo labels there
                rec["bpm_beatthis"] = beat_this_tempo(y)
            wp = tmpdir / f"{tid}.wav"
            write(wp, y, SR)
            r = rust([wp]).get(str(wp))
            wp.unlink()
            if r:
                rec["bpm_wave"], root, minor, _ = r
                rec["key_wave"] = (root, minor)
            fo.write(json.dumps(rec) + "\n")
            fo.flush()
            n += 1
            if a.limit and n >= a.limit:
                break
    print("done", a.dataset, n)


if __name__ == "__main__":
    main()
