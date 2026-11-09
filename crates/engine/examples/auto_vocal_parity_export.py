"""Exports the auto-vocal research test set for the Rust parity check (auto_vocal_parity.rs).

Run from the research checkout (branch `auto-vocal`, folder research/auto_vocal) with its
venv:  python <this file> D:/mydaw-data/auto_vocal_parity

It rebuilds exactly the 50 degraded MUSDB test songs of `av.dyn_calib` (same seeds), the
pre-dynamics signal (gate -> HPF -> EQ, which the app chain does not port yet), and writes
per song: raw.wav, eq.wav, py_after.wav (float32, 44.1 kHz) and the Python metrics in
python.json.
"""
from __future__ import annotations

import json
import pathlib
import sys

import numpy as np
import soundfile as sf

sys.path.insert(0, str(pathlib.Path.cwd()))
from av import leveling as L  # noqa: E402
from av import mix  # noqa: E402
from av.common import RESULTS  # noqa: E402
from av.dyn_calib import dynamics, pre_dynamics  # noqa: E402
from av.layer3_eval import EXC, SR, degrade, fit_targets, load_exc, pro_features  # noqa: E402


def main(out):
    out = pathlib.Path(out)
    out.mkdir(parents=True, exist_ok=True)
    medt, *_ = fit_targets(pro_features())
    cfg = json.loads((RESULTS / "layer3_chain_config.json").read_text())
    t = mix.Targets(shape=medt["shape"], spread=medt["spread"], sib=medt["sib"],
                    eq_strength=cfg["eq_strength"], eq_cut_only=cfg["eq_cut_only"])
    dyn = {k: cfg[k] for k in ("rider_strength", "rider_mu", "fet_peak_gr", "opto_avg_gr")}
    rng = np.random.default_rng(41)
    rows = []
    for i, p in enumerate(sorted(EXC.glob("test__*.npz"))):
        v, _ = load_exc(p)
        raw, _ = degrade(v.astype(np.float64), np.random.default_rng(rng.integers(1 << 30)))
        x_eq = pre_dynamics(raw, t)
        xr, y, s = dynamics(x_eq, t, dyn)
        mask_r, segs_r = L.vocal_windows(raw, SR), L.words(raw, SR)
        g, _, conf, active = s.rider_curve
        r = {"name": p.stem, "i": i,
             "raw_m1": L.m1(raw, SR, mask_r), "raw_m2": L.m2(raw, SR, segs_r),
             "after_m1": L.m1(y, SR, mask_r), "after_m2": L.m2(y, SR, segs_r),
             "rider_m1": L.m1(xr, SR, mask_r), "rider_m2": L.m2(xr, SR, segs_r),
             "eq_m1": L.m1(x_eq, SR, mask_r), "eq_m2": L.m2(x_eq, SR, segs_r),
             "rider": L.rider_stats(s.rider_curve),
             "fet_gr": s.fet_gr, "opto_gr": s.opto_gr, "fet_thr": s.fet_thr_db, "opto_thr": s.opto_thr_db,
             "deess_thr": s.deess_thr_db, "sib_target": float(t.sib)}
        d = out / f"{i:02d}"
        d.mkdir(exist_ok=True)
        sf.write(d / "raw.wav", np.asarray(raw, np.float32), SR, subtype="FLOAT")
        sf.write(d / "eq.wav", np.asarray(x_eq, np.float32), SR, subtype="FLOAT")
        sf.write(d / "py_after.wav", np.asarray(y, np.float32), SR, subtype="FLOAT")
        sf.write(d / "py_rider.wav", np.asarray(xr, np.float32), SR, subtype="FLOAT")
        np.save(d / "py_rider_gain.npy", np.stack([g, conf, active.astype(float)]))
        (d / "python.json").write_text(json.dumps(r, default=float))
        rows.append(r)
        print(i, p.stem, round(r["raw_m1"]["iqr"], 2), round(r["after_m1"]["iqr"], 2), flush=True)
    keys = ("raw_m1", "raw_m2", "after_m1", "after_m2", "rider_m1", "rider_m2", "eq_m1", "eq_m2")
    med = {k: {m: float(np.nanmedian([r[k][m] for r in rows])) for m in ("iqr", "p90_p10")} for k in keys}
    for k in ("lift_median_db", "lift_p95_db", "motion_p95_db_s"):
        med[k] = float(np.median([r["rider"][k] for r in rows]))
    med["fet_p99_db"] = float(np.median([r["fet_gr"]["p99_db"] for r in rows]))
    med["opto_mean_db"] = float(np.median([r["opto_gr"]["mean_sung_db"] for r in rows]))
    med["sib_target"] = float(t.sib)
    (out / "python_medians.json").write_text(json.dumps(med, indent=1))
    print(json.dumps(med, indent=1))


if __name__ == "__main__":
    main(sys.argv[1])
