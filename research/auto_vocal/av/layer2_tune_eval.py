"""Layer 2b — pitch-correction validation.

A) VocalSet "detune and restore" (paired, known intent). A clean take is built first: every
   annotated note of a trained singer is moved (oracle F0, whole-note offset, vibrato kept) to
   the nearest A440 equal-tempered note, so the intended pitch is exact by construction.
   Every annotated note is then detuned by a random ±15…45 cents (PSOLA, driven
   by the hand-checked annotation F0), then corrected blind: the algorithm tracks pitch
   itself (torchcrepe 'tiny'), snaps chromatically (no beat key) on the A440 grid, which
   stands in for the beat's tuning; "hard_selftune" instead estimates the tuning blind.
   Measured with an independent tracker (pYIN, 4-cent resolution) on the output audio:
     note error   |median pitch of the note − intended note pitch|, cents
     wrong note   share of notes ending > 50 cents from intent
     vibrato      std of the note's pitch around its 150 ms moving average, output / original
     spectral     mean |Δ| of 40-band mel dB vs the clean take over voiced frames. Any pitch
                  change moves harmonics across bands, so this is only an artefact proxy
                  relative to "detuned" (a pure shift of the same size); scripts/
                  diag_artefacts.py compares PSOLA against a phase vocoder at fixed shifts.
   The clean (in-tune) take also goes through "natural" mode: it must barely change.
B) MIR-1K real amateurs over their karaoke backing (product path): key + tuning from the
   accompaniment channel only, vocal corrected to that scale. No intended notes exist, so
   only self-referential numbers: note deviation from the nearest scale note before/after,
   share of notes moved by > 50 cents (re-targeted to another note), spectral distance.

usage: python -m av.layer2_tune_eval vocalset|mir1k [--n N]
"""
from __future__ import annotations

import argparse
import json

import librosa
import numpy as np

from . import tune
from .common import CACHE, RAW, load
from .layer1_run import chroma_features
from .layer2_pitch import vocalset_items

SR = 16000  # hop 160 = exactly the 10 ms analysis grid


def measure(y):
    f, v, _ = librosa.pyin(y, fmin=65, fmax=1047, sr=SR, frame_length=1024, hop_length=160,
                           resolution=0.04)  # 0.02 trips a librosa pYIN bug (2-octave errors)
    return tune.hz2midi(np.where(v, np.nan_to_num(f), 0.0))


def mel_db(y):
    return librosa.power_to_db(librosa.feature.melspectrogram(y=y, sr=SR, n_mels=40, hop_length=160), ref=1.0)


def spec_dist(a, b, voiced):
    A, B = mel_db(a), mel_db(b)
    n = min(A.shape[1], B.shape[1], len(voiced))
    v = voiced[:n]
    if not v.any():
        return float("nan")
    return float(np.mean(np.abs(A[:, :n][:, v] - B[:, :n][:, v])))


def note_stats(meas, notes, intended):
    errs, vib = [], []
    for nt, c in zip(notes, intended):
        seg = meas[nt.a:min(nt.b, len(meas))]
        seg = seg[~np.isnan(seg)]
        if len(seg) < 5:
            errs.append(np.nan)
            vib.append(np.nan)
            continue
        errs.append(100 * (np.median(seg) - c))
        ma = np.convolve(seg, np.ones(15) / 15, mode="same")
        vib.append(float(np.std((seg - ma)[7:-7])) if len(seg) > 20 else np.nan)
    return np.array(errs), np.array(vib)


def run_vocalset(n):
    out = CACHE / "l2b_vocalset.jsonl"
    done = {json.loads(line)["id"] for line in out.read_text().splitlines()} if out.exists() else set()
    rng = np.random.default_rng(7)
    with out.open("a") as fo:
        for k, get, rt, rf in vocalset_items(n, seed=1):
            delta_seed = rng.integers(1 << 30)
            if k in done or "spoken" in k:
                continue
            y = load_vs(k)
            # annotation f0 on the 10 ms grid
            t = np.arange(int(len(y) / SR / 0.01)) * 0.01
            ann = np.interp(t, rt, rf, left=0, right=0)
            near = rf[np.clip(np.searchsorted(rt, t), 0, len(rf) - 1)]
            ann = np.where(near > 0, ann, 0.0)
            amidi = tune.hz2midi(ann)
            notes = tune.segment_notes(amidi)
            if len(notes) < 3:
                continue
            # 1) clean take: every annotated note moved (oracle F0, whole-note offset, so vibrato
            #    and scoops stay) onto the A440 equal-tempered note nearest it -> intent is exact
            ratio_c = np.ones(len(t))
            intended = []
            for nt in notes:
                tgt = float(round(nt.centre))
                intended.append(tgt)
                ratio_c[nt.a:nt.b] = 2 ** ((tgt - nt.centre) / 12)
            ratio_c = np.convolve(np.pad(ratio_c, 3, mode="edge"), np.ones(7) / 7, mode="valid")
            clean = tune.psola(y.astype(np.float64), SR, ann, ratio_c)
            ann_c = ann * ratio_c
            # 2) detune every note of the clean take by a known ±15…45 cents
            r2 = np.random.default_rng(delta_seed)
            deltas = r2.uniform(15, 45, len(notes)) * r2.choice([-1, 1], len(notes))
            ratio = np.ones(len(t))
            for nt, d in zip(notes, deltas):
                ratio[nt.a:nt.b] = 2 ** (d / 1200)
            ratio = np.convolve(np.pad(ratio, 3, mode="edge"), np.ones(7) / 7, mode="valid")
            det = tune.psola(clean, SR, ann_c, ratio)
            voiced = ~np.isnan(amidi)
            rec = {"id": k, "notes": len(notes), "orig_tuning_cents": tune.estimate_tuning(amidi)}
            conds = {"clean": clean, "detuned": det}
            f0_det = tune.track_f0(det, SR, "tiny")
            f0_c = tune.track_f0(clean, SR, "tiny")
            # the A440 grid stands in for the beat's tuning (tuning_cents=0); hard_selftune
            # instead estimates the tuning blind from the detuned take
            for style, strength, src, f0, tun in (
                    ("natural", None, det, f0_det, 0.0), ("natural_full", 1.0, det, f0_det, 0.0),
                    ("hard", None, det, f0_det, 0.0), ("hard_selftune", None, det, f0_det, "self"),
                    ("natural_on_clean", None, clean, f0_c, 0.0)):
                z, info = tune.autotune(src, SR, None, False, "hard" if style.startswith("hard") else "natural",
                                        strength, f0=f0, tuning_cents=tun)
                conds[style] = z
                rec[f"{style}_strength"] = info["strength"]
            m_ref = measure(clean)
            _, vib0 = note_stats(m_ref, notes, intended)
            for cname, z in conds.items():
                m = measure(z)
                e, vib = note_stats(m, notes, intended)
                rec[cname] = {"abs_err_med": float(np.nanmedian(np.abs(e))), "abs_err_mean": float(np.nanmean(np.abs(e))),
                              "wrong": float(np.nanmean(np.abs(e) > 50)), "within10": float(np.nanmean(np.abs(e) <= 10)),
                              "vib_ratio": float(np.nanmedian(vib / vib0)),
                              "spec": spec_dist(clean, z, voiced)}
            fo.write(json.dumps(rec) + "\n")
            fo.flush()
            print(k, {c: round(rec[c]["abs_err_med"], 1) for c in conds}, flush=True)


_VS_AUDIO = None


def load_vs(stem):
    global _VS_AUDIO
    from .layer2_pitch import VS
    if _VS_AUDIO is None:
        _VS_AUDIO = {p.stem: p for p in (VS / "audio").rglob("*.wav")}
    return load(_VS_AUDIO[stem], SR)


def app_key(acc):
    import os
    import subprocess
    import tempfile
    from .common import write
    with tempfile.TemporaryDirectory() as td:
        p = os.path.join(td, "beat.wav")
        write(p, acc, SR)
        lst = os.path.join(td, "l.txt")
        with open(lst, "w") as fh:
            fh.write(p)
        out = subprocess.run([r"D:\mydaw-data\cargo_target2\release\music-bench.exe", lst], capture_output=True,
                             text=True).stdout.strip().split("\t")
    root = int(out[2])
    return (root if root >= 0 else None), bool(int(out[3]))


def run_mir1k(n):
    out = CACHE / "l2b_mir1k.jsonl"
    done = {json.loads(line)["id"] for line in out.read_text().splitlines()} if out.exists() else set()
    root = RAW / "mir1k" / "MIR-1K"
    files = sorted((root / "Wavfile").glob("*.wav"))[::max(1, 1000 // n)][:n]
    with out.open("a") as fo:
        for w in files:
            if w.stem in done:
                continue
            x = load(w, SR, mono=False)
            acc, voc = x[0], x[1]
            _, _, tun = chroma_features(librosa.resample(acc, orig_sr=SR, target_sr=22050))  # tuning, layer-1 features
            root_pc, minor = app_key(acc)  # WAVE's built-in key estimator, the layer-1 winner
            if root_pc is None:
                continue
            pcs = tune.scale_pcs(root_pc, minor)
            f0 = tune.track_f0(voc, SR, "tiny")
            midi_in = measure(voc)
            notes = tune.segment_notes(midi_in)
            if len(notes) < 3:
                continue
            rec = {"id": w.stem, "key": [int(root_pc), bool(minor)], "tuning": tun * 100, "notes": len(notes)}
            notes_t = tune.segment_notes(midi_in - tun)
            rec["input"] = {"dev_med_scale": float(np.median(np.abs(tune.intonation(notes_t, pcs)))),
                            "dev_med_chromatic": float(np.median(np.abs(tune.intonation(notes_t, list(range(12))))))}
            for style in ("natural", "hard"):
                z, info = tune.autotune(voc, SR, root_pc, minor, style, f0=f0, tuning_cents=tun * 100)
                m = measure(z)
                cent = [np.nanmedian(m[nt.a:nt.b]) if np.isfinite(m[nt.a:nt.b]).sum() >= 5 else np.nan
                        for nt in notes]
                cent = np.array(cent)
                ok = ~np.isnan(cent)
                c_in = np.array([nt.centre for nt in notes])
                grid = list(range(12)) if info["scale_gated"] else pcs
                dev_out = 100 * (cent[ok] - tun - tune.snap(cent[ok] - tun, grid))
                rec[style] = {"dev_med": float(np.median(np.abs(dev_out))),
                              "dev_in_same_grid": rec["input"]["dev_med_chromatic" if info["scale_gated"] else "dev_med_scale"],
                              "moved_gt50": float(np.mean(np.abs(cent[ok] - c_in[ok]) > 0.5)),
                              "strength": info["strength"], "scale_gated": bool(info["scale_gated"]),
                              "on_scale": info["on_scale"],
                              "spec": spec_dist(voc, z, ~np.isnan(midi_in))}
            fo.write(json.dumps(rec) + "\n")
            fo.flush()
            print(w.stem, rec["input"], rec["natural"]["dev_med"], rec["hard"]["dev_med"], rec["hard"]["scale_gated"], flush=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("what", choices=["vocalset", "mir1k"])
    ap.add_argument("--n", type=int, default=0)
    a = ap.parse_args()
    import torch
    torch.set_num_threads(2)
    if a.what == "vocalset":
        run_vocalset(a.n or 70)
    else:
        run_mir1k(a.n or 100)


if __name__ == "__main__":
    main()
