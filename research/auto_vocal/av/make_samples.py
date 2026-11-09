"""Before/after listening pairs -> D:\\mydaw-data\\auto_vocal_samples.

MUSDB18 tracks: professional vocal stems have no raw take, so the "raw" vocal is simulated
(per-note detune ±15–45 ct + the layer-3 degradation: EQ/boom/expanded dynamics/sibilance/
room noise/level). MIR-1K: a real amateur karaoke take over its backing (16 kHz source).
The full chain runs blind on the raw vocal and the beat: beat key/tuning/tempo (layer 1),
pitch correction (layer 2), mix fit (layer 3), tempo-synced FX. Every file is normalised
to -14 LUFS through one -1 dBTP lookahead true-peak limiter, so "louder" never passes for
"better". Reverb level comes from fx_calib.py; the delay is off (no data-backed setting).
"""
from __future__ import annotations

import json
import os
import subprocess
import tempfile

import numpy as np

from . import mix, tune
from .common import CACHE, DATA, NOTE, RAW, RESULTS, load, write
from .layer1_run import beat_this_tempo, chroma_features
from .layer3_eval import EXC, degrade, load_exc
from .limiter import master

SR = 44100
OUT = DATA / "auto_vocal_samples"
HARNESS = r"D:\mydaw-data\cargo_target2\release\music-bench.exe"

PICKS = [  # (source, name, style)
    ("musdb", "test__Little Chicago's Finest - My Own", "hard"),
    ("musdb", "test__M.E.R.C. Music - Knockout", "hard"),
    ("musdb", "test__Cristina Vane - So Easy", "natural"),
    ("musdb", "test__Signe Jakobsen - What Have You Done To Me", "natural"),
    ("musdb", "test__Side Effects Project - Sing With Me", "hard"),
    ("musdb", "test__Juliet's Rescue - Heartbeats", "natural"),
    ("musdb", "test__Speak Softly - Like Horses", "natural"),
    ("mir1k", "Ani_1", "natural"),
    ("mir1k", "amy_1", "natural"),
]


def targets():
    from .layer3_eval import fit_targets, pro_features
    F = pro_features()
    med, r_shape, r_delta, _, _ = fit_targets(F)
    return med, r_shape, r_delta


def beat_key(acc):
    """WAVE's built-in key estimator (best on GiantSteps/GTZAN in layer 1)."""
    import soxr
    with tempfile.TemporaryDirectory() as td:
        p = os.path.join(td, "beat.wav")
        write(p, soxr.resample(acc, SR, 22050), 22050)
        lst = os.path.join(td, "l.txt")
        open(lst, "w").write(p)
        out = subprocess.run([HARNESS, lst], capture_output=True, text=True).stdout.strip().split("\t")
    root, minor = int(out[2]), bool(int(out[3]))
    return (root if root >= 0 else None), minor, float(out[4])


def window(v, secs=25):
    n = int(secs * SR)
    if len(v) <= n:
        return 0, len(v)
    e = np.convolve(v ** 2, np.ones(SR) / SR, "same")[::SR]
    w = np.convolve(e, np.ones(secs), "valid")
    s = int(np.argmax(w)) * SR
    return s, s + n


def simulate_raw(v, rng):
    f0 = tune.track_f0(v, SR, "tiny")
    notes = tune.segment_notes(tune.hz2midi(f0))
    ratio = np.ones(len(f0))
    for nt in notes:
        ratio[nt.a:nt.b] = 2 ** (rng.uniform(15, 45) * rng.choice([-1, 1]) / 1200)
    ratio = np.convolve(np.pad(ratio, 3, mode="edge"), np.ones(7) / 7, mode="valid")
    det = tune.psola(v.astype(np.float64), SR, f0, ratio)
    raw, _ = degrade(det, rng)
    return raw


def main():
    """usage: python -m av.make_samples [--only 8,9]  (re-render some samples, keep the others)"""
    import torch
    torch.set_num_threads(3)
    OUT.mkdir(parents=True, exist_ok=True)
    med, r_shape, r_delta = targets()
    import sys
    only = {int(v) for v in sys.argv[sys.argv.index("--only") + 1].split(",")} if "--only" in sys.argv else None
    prev = json.loads((CACHE / "samples_index.json").read_text()) if only else []
    index = []
    rng = np.random.default_rng(5)
    for i, (src, name, style) in enumerate(PICKS, 1):
        if only and i not in only:
            index.append(prev[i - 1])
            continue
        if src == "musdb":
            pro, acc = load_exc(EXC / f"{name}.npz")
            a, b = window(pro)
            pro, acc = pro[a:b].astype(np.float64), acc[a:b].astype(np.float64)
            raw = simulate_raw(pro, rng)
            title = name.split("__", 1)[1]
            lic = "MUSDB18 (Rafii et al. 2017), track licence per MUSDB18 terms: non-commercial research/evaluation only."
        else:
            x = load(RAW / "mir1k" / "MIR-1K" / "UndividedWavfile" / f"{name}.wav", SR, mono=False)
            acc, raw = x[0].astype(np.float64), x[1].astype(np.float64)
            a, b = window(raw)
            acc, raw, pro = acc[a:b], raw[a:b], None
            title = f"MIR-1K {name} (real amateur karaoke take)"
            lic = "MIR-1K (Hsu & Jang 2010): research use; 16 kHz source, so nothing above 8 kHz exists."
        root, minor, tun = beat_key(acc.astype(np.float32))
        import soxr
        bpm = beat_this_tempo(soxr.resample(acc, SR, 22050).astype(np.float32))
        _, _, tun_c = chroma_features(soxr.resample(acc, SR, 22050).astype(np.float32))
        tuned, info = tune.autotune(raw, SR, root, minor, style, tuning_cents=tun_c * 100)
        fa = med  # targets
        from . import mixfeat
        # chain configuration chosen on the MUSDB train split (layer3_tune.py); median pro
        # targets (beat-conditioned ridge targets did not beat them on the test split)
        cfg = json.loads((RESULTS / "layer3_chain_config.json").read_text())
        # reverb send/T60 from fx_calib.py (matched to the late energy of pro vocal stems); delay off
        fxc = json.loads((RESULTS / "fx_calibration.json").read_text())["chosen"]
        t = mix.Targets(shape=fa["shape"], spread=fa["spread"], sib=fa["sib"], delta_lufs=fa["delta_lufs"],
                        rev_send_db=fxc["send_db"], rev_t60=fxc["t60"], **cfg)
        proc, s = mix.auto_mix(tuned, SR, acc, t, bpm=bpm, style=style, fx=True)
        tag = f"{i:02d}_{title.replace(' ', '_').replace('/', '-').replace(chr(39), '')[:40]}"
        # every file: matched to -14 LUFS through the same -1 dBTP lookahead true-peak limiter
        lim = {}
        for kind, sig in (("before", raw + acc), ("after", proc + acc), ("reference_pro_mix", None if pro is None else pro + acc)):
            if sig is None:
                continue
            y, st = master(sig, SR)
            write(OUT / f"{tag}_{kind}.wav", y, SR)
            lim[kind] = {k: round(v, 2) for k, v in st.items()}
        midi_in = tune.hz2midi(info["f0"])
        notes = tune.segment_notes(midi_in - tun_c)
        pcs = list(range(12)) if info["scale_gated"] else tune.scale_pcs(root, minor)
        dev = np.abs(tune.intonation(notes, pcs))
        f_out = tune.track_f0(tuned, SR, "tiny")
        dev_out = np.abs(tune.intonation(tune.segment_notes(tune.hz2midi(f_out) - tun_c), pcs))
        eq = np.array(s.eq_gains_db)
        cen = mixfeat.CENTRES
        big = np.argsort(-np.abs(eq))[:3]
        rec = {
            "sample": tag, "source": title, "licence": lic, "style": style,
            "beat": {"key": f"{NOTE[root]} {'minor' if minor else 'major'}" if root is not None else "none",
                     "tempo_bpm": round(bpm, 1), "tuning_cents": round(tun_c * 100, 1)},
            "pitch": {"median_note_dev_before_ct": round(float(np.median(dev)), 1) if len(dev) else None,
                      "median_note_dev_after_ct": round(float(np.median(dev_out)), 1) if len(dev_out) else None,
                      "strength": round(info["strength"], 2), "on_scale": round(info["on_scale"], 2),
                      "scale_gated": bool(info["scale_gated"])},
            "mix": {"gate_thr_db": round(s.gate_thr_db, 1),
                    "eq_biggest_moves": [f"{cen[k]:.0f} Hz {eq[k]:+.1f} dB" for k in sorted(big)],
                    "deess_thr_db": round(s.deess_thr_db, 1), "comp": f"{s.comp_ratio:g}:1 at {s.comp_thr_db:.1f} dB",
                    "vocal_vs_beat_lufs": round(t.delta_lufs, 1),
                    "reverb": f"plate T60 {s.reverb_t60:.1f} s, pre-delay 30 ms, wet 300 Hz-7 kHz, send {s.reverb_send_db:+.0f} dB",
                    "delay": "off",
                    "saturation": "tanh drive 6 dB, 25 % mix" if style == "hard" else "off"},
            "limiter": lim,
        }
        index.append(rec)
        print(json.dumps(rec), flush=True)
    (CACHE / "samples_index.json").write_text(json.dumps(index, indent=1))
    lines = ["WAVE auto-vocal prototype: before/after pairs. Every file is matched to -14 LUFS through the same",
             "lookahead true-peak limiter (ceiling -1 dBTP); its gain reduction is listed per file.",
             "before = raw vocal + beat; after = same vocal through the automatic chain + same beat;",
             "reference_pro_mix = the song's professional vocal stem + beat (MUSDB only).",
             "MUSDB 'raw' vocals are SIMULATED from the pro stem (detuned ±15-45 ct per note, EQ/boom/",
             "expanded dynamics/sibilance/room noise added); MIR-1K takes are real amateur recordings.", ""]
    for r in index:
        lines += [f"{r['sample']}  [{r['style']}]  {r['source']}",
                  f"  licence: {r['licence']}",
                  f"  beat: key {r['beat']['key']}, {r['beat']['tempo_bpm']} BPM, tuning {r['beat']['tuning_cents']} ct",
                  f"  pitch: note deviation from the snap grid (scale; chromatic if gated) {r['pitch']['median_note_dev_before_ct']} -> "
                  f"{r['pitch']['median_note_dev_after_ct']} ct (median), strength {r['pitch']['strength']}; "
                  f"{r['pitch']['on_scale']:.0%} of sung time on the beat scale"
                  + (" -> scale gate: snapped chromatically" if r['pitch']['scale_gated'] else ""),
                  f"  mix: gate {r['mix']['gate_thr_db']} dB; EQ {', '.join(r['mix']['eq_biggest_moves'])}; "
                  f"de-ess thr {r['mix']['deess_thr_db']} dB; comp {r['mix']['comp']}; vocal {r['mix']['vocal_vs_beat_lufs']} LU vs beat",
                  f"  fx: reverb {r['mix']['reverb']}; delay {r['mix']['delay']}; saturation {r['mix']['saturation']}",
                  "  limiter: " + "; ".join(f"{k} GR max {v['gr_max_db']} dB, mean {v['gr_mean_db_when_active']} dB over "
                                            f"{v['active_share']:.0%} of the time, TP {v['true_peak_dbtp']} dBTP, {v['lufs']} LUFS"
                                            for k, v in r['limiter'].items()),
                  ""]
    (OUT / "index.txt").write_text("\n".join(lines), encoding="utf-8")
    print("done")


if __name__ == "__main__":
    main()
