"""Layer 2a — vocal pitch tracking benchmark.

Datasets (reference F0 per frame):
  vocadito     40 solo-singing excerpts, 29 singers, CC BY 4.0 (commercial-safe data).
  mir1k        1000 amateur karaoke clips (16 kHz); vocal channel alone ("clean") and the
               vocal + accompaniment at 0 dB ("mix"). Research-only data.
  vocalset     Annotated VocalSet (CC BY 4.0), trained singers, 14 techniques; a fixed
               stratified subset (--n files drawn, 10 per technique with --n 140;
               only files whose audio is in the release are used).
Methods:
  pyin         librosa.pyin (ISC), 65-1047 Hz.
  crepe        torchcrepe 'full' (MIT code + weights), Viterbi, periodicity > 0.21 voiced.
  crepe_tiny   the same with the 'tiny' model (~1/30 of the compute).
  wave_yin     WAVE's built-in YIN (crates/engine pitch::analyze), via rust_music_bench.
  wave_rmvpe   WAVE's RMVPE path (crates/rmvpe; weights research-only), via rust_music_bench.
Metrics: mir_eval.melody (RPA/RCA at 50 cents, voicing recall / false alarm, OA), averaged
per track; plus the median absolute cents error on frames both call voiced.

usage: python -m av.layer2_pitch vocadito|mir1k|vocalset [--methods ...] [--limit N]
"""
from __future__ import annotations

import argparse
import json
import os
import pathlib
import subprocess
import tempfile

import mir_eval
import numpy as np

from .common import CACHE, DATA, RAW, load, write

SR = 16000
HARNESS = pathlib.Path(r"D:\mydaw-data\cargo_target2\release\music-bench.exe")
RMVPE = pathlib.Path(os.environ.get("WAVE_RMVPE_MODEL", r"C:\Users\skibi\AppData\Local\Temp\rmvpe_probe\rmvpe.onnx"))
VS = pathlib.Path(r"D:\datasets\vocalset_annotated")


def vocadito():
    root = DATA / "vocadito"
    for i in range(1, 41):
        ref = np.loadtxt(root / "Annotations" / "F0" / f"vocadito_{i}_f0.csv", delimiter=",")
        yield f"vocadito_{i}", (lambda i=i: load(root / "Audio" / f"vocadito_{i}.wav", SR)), ref[:, 0], ref[:, 1]


def mir1k(mix: bool):
    root = RAW / "mir1k" / "MIR-1K"
    for w in sorted((root / "Wavfile").glob("*.wav")):
        pv = root / "PitchLabel" / (w.stem + ".pv")
        if not pv.exists():
            continue
        semi = np.loadtxt(pv)
        t = 0.02 + 0.02 * np.arange(len(semi))
        hz = np.where(semi > 0, 440 * 2 ** ((semi - 69) / 12), 0.0)

        def get(w=w):
            x = load(w, SR, mono=False)  # left = accompaniment, right = vocal
            return (x[0] + x[1]) if mix else x[1]
        yield w.stem, get, t, hz


def vocalset_items(n=56, seed=0):
    ann = VS / "Annotated VocalSet"
    csvs = sorted(ann.glob("raw */csv/*/*.csv"))
    by_tech = {}
    for c in csvs:
        by_tech.setdefault(c.parent.name, []).append(c)
    rng = np.random.default_rng(seed)
    pick = []
    per = max(1, n // len(by_tech))
    for tech in sorted(by_tech):
        cs = by_tech[tech]
        pick += [cs[i] for i in rng.choice(len(cs), min(per, len(cs)), replace=False)]
    audio = {p.stem: p for p in (VS / "audio").rglob("*.wav")}
    for c in pick:
        if c.stem not in audio:
            continue
        rows = []
        for line in c.read_text().splitlines()[1:]:
            f = [v.strip() for v in line.split(",")]
            try:
                rows.append((float(f[0]), float(f[1])))
            except ValueError:
                pass
        r = np.array(rows)
        yield c.stem, (lambda a=audio[c.stem]: load(a, SR)), r[:, 0], r[:, 1]


# ---------------------------------------------------------------- methods

def m_pyin(y):
    import librosa
    f0, vflag, _ = librosa.pyin(y, fmin=65, fmax=1047, sr=SR, frame_length=1024, hop_length=160)
    t = librosa.times_like(f0, sr=SR, hop_length=160)
    return t, np.where(vflag, np.nan_to_num(f0), 0.0)


def m_crepe(y, model="full"):
    import torch
    import torchcrepe
    x = torch.tensor(y)[None]
    f0, per = torchcrepe.predict(x, SR, 160, 50, 1100, model, decoder=torchcrepe.decode.viterbi,
                                 batch_size=512, device="cpu", return_periodicity=True)
    per = torchcrepe.filter.median(per, 3)
    f0 = f0[0].numpy()
    per = per[0].numpy()
    t = np.arange(len(f0)) * 0.01
    return t, np.where(per > 0.21, f0, 0.0)


def m_wave(ys: dict, det: str):
    """Batch through the Rust harness: {id: audio} -> {id: (t, hz)}."""
    tmp = pathlib.Path(tempfile.mkdtemp(prefix="l2_"))
    lines = []
    for k, y in ys.items():
        wp = tmp / f"{k}.wav"
        write(wp, y, SR)
        lines.append(f"{wp}\t{tmp / (k + '.txt')}")
    lst = tmp / "list.txt"
    lst.write_text("\n".join(lines))
    subprocess.run([str(HARNESS), "--pitch", det, str(lst)], check=True, capture_output=True)
    out = {}
    for k in ys:
        midi = np.array([float(v) for v in (tmp / f"{k}.txt").read_text().split()])
        hz = np.where(np.isnan(midi), 0.0, 440 * 2 ** ((np.nan_to_num(midi) - 69) / 12))
        out[k] = (np.arange(len(hz)) / 200.0, hz)
        (tmp / f"{k}.txt").unlink()
        (tmp / f"{k}.wav").unlink()
    lst.unlink()
    tmp.rmdir()
    return out


def score(rt, rf, et, ef):
    m = mir_eval.melody.evaluate(rt, rf, et, ef)
    ev = np.interp(rt, et, ef, left=0, right=0)
    # interpolating across a voicing edge gives junk: require the nearest est frame voiced
    near = ef[np.clip(np.searchsorted(et, rt), 0, len(ef) - 1)]
    both = (rf > 0) & (ev > 0) & (near > 0)
    cents = np.abs(1200 * np.log2(ev[both] / rf[both])) if both.any() else np.array([np.nan])
    return {"rpa": m["Raw Pitch Accuracy"], "rca": m["Raw Chroma Accuracy"], "vr": m["Voicing Recall"],
            "vfa": m["Voicing False Alarm"], "oa": m["Overall Accuracy"], "med_cents": float(np.median(cents))}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("dataset", choices=["vocadito", "mir1k", "mir1k_mix", "vocalset"])
    ap.add_argument("--methods", default="pyin,crepe,crepe_tiny,wave_yin,wave_rmvpe")
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--every", type=int, default=1, help="take every Nth item (subsample)")
    ap.add_argument("--n", type=int, default=56, help="VocalSet: files drawn (stratified by technique)")
    a = ap.parse_args()
    import torch
    torch.set_num_threads(int(os.environ.get("AV_THREADS", "3")))
    methods = a.methods.split(",")
    it = {"vocadito": vocadito, "mir1k": lambda: mir1k(False), "mir1k_mix": lambda: mir1k(True),
          "vocalset": lambda: vocalset_items(a.n)}[a.dataset]()
    out = CACHE / f"l2_{a.dataset}.jsonl"
    done = set()
    if out.exists():
        done = {(r["id"], r["method"]) for r in map(json.loads, out.read_text().splitlines())}
    batch = {}
    refs = {}
    n = 0
    with out.open("a") as fo:
        def flush():
            for det, name in (("yin", "wave_yin"), (str(RMVPE), "wave_rmvpe")):
                if name not in methods:
                    continue
                todo = {k: y for k, y in batch.items() if (k, name) not in done}
                if not todo:
                    continue
                for k, (et, ef) in m_wave(todo, det).items():
                    fo.write(json.dumps({"id": k, "method": name, **score(*refs[k], et, ef)}) + "\n")
            fo.flush()
            batch.clear()

        for i, (k, get, rt, rf) in enumerate(it):
            if i % a.every:
                continue
            if all((k, m) in done for m in methods):
                continue
            y = get()
            refs[k] = (rt, rf)
            for name, fn in (("pyin", m_pyin), ("crepe", m_crepe), ("crepe_tiny", lambda z: m_crepe(z, "tiny"))):
                if name in methods and (k, name) not in done:
                    et, ef = fn(y)
                    fo.write(json.dumps({"id": k, "method": name, **score(rt, rf, et, ef)}) + "\n")
            batch[k] = y
            if len(batch) >= 20:
                flush()
            n += 1
            if a.limit and n >= a.limit:
                break
        flush()
    print("done", a.dataset, n)


if __name__ == "__main__":
    main()
