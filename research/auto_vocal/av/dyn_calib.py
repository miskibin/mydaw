"""Calibrate and validate the vocal levelling (rider -> fast stage -> slow stage -> de-esser).

Metrics: av/leveling.py (owner's brief). Goals for the rider stage: m1 IQR -25…45 % vs its
input, m2 word P90-P10 -2…5 dB, m3 rho > 0.9 and R_macro 0.7-1.0, m4 lift median < +0.25 dB
and P95 < +1 dB, m5 P95 |dg/dt| < 12 dB/s (sung), m6 workload P95 GR -20 % or more,
m7 consonant change < 1 dB.

1. Pro reference: m1/m2 of all 150 MUSDB vocal stems. A MUSDB "vocals" stem is lead +
   backing + printed FX, so it is less even than a mixed lead vocal; its 25th percentile is
   reported as the closest public proxy, the median for context.
2. Grid on the rider's word-evening strength s {0.5, 0.7, 0.85} x phrase nudge mu
   {0.1, 0.2, 0.3}, run on 40 real raw Vocadito takes and 30 degraded MUSDB train songs.
   The pick: the most goals met (median over takes), then the chain output's m1 IQR
   closest to the pro p25.
3. Report: 50 degraded MUSDB test songs and the 40 Vocadito takes, raw -> after (full
   dynamics chain, dry) -> pro stem, plus every rider-stage metric and per-stage GR.
The chosen settings are merged into results/layer3_chain_config.json.
"""
from __future__ import annotations

import itertools
import json

import numpy as np

from . import leveling as L
from . import mix
from .common import DATA, RESULTS, load, save_json
from .layer3_eval import EXC, SR, degrade, fit_targets, load_exc, pro_features


def pre_dynamics(raw, t):
    s = mix.Settings()
    x = mix.gate(np.asarray(raw, np.float64), SR, s)
    x = mix.hpf(x, SR)
    return mix.match_eq(x, SR, t.shape, s, t.max_eq_db, t.eq_strength, t.eq_cut_only)


def dynamics(x_eq, t, dyn):
    s = mix.Settings()
    xr = mix.rider(x_eq, SR, s, dyn["rider_strength"], dyn["rider_mu"])
    y = mix.fet(xr, SR, s, peak_gr=dyn["fet_peak_gr"])
    y = mix.opto(y, SR, s, avg_gr=dyn["opto_avg_gr"])
    y = mix.deess(y, SR, t.sib, s)
    return xr, y, s


def evaluate(raw, t, dyn):
    x_eq = pre_dynamics(raw, t)
    xr, y, s = dynamics(x_eq, t, dyn)
    mask = L.vocal_windows(x_eq, SR)
    segs = L.words(x_eq, SR)
    a, b = L.m1(x_eq, SR, mask), L.m1(xr, SR, mask)
    wa, wb = L.m2(x_eq, SR, segs), L.m2(xr, SR, segs)
    rs = L.rider_stats(s.rider_curve) if s.rider_curve else {}
    mask_r, segs_r = L.vocal_windows(raw, SR), L.words(raw, SR)
    r = {"m1_iqr_change_pct": 100 * (b["iqr"] - a["iqr"]) / max(a["iqr"], 1e-6),
         "m1_p90p10_change_pct": 100 * (b["p90_p10"] - a["p90_p10"]) / max(a["p90_p10"], 1e-6),
         "m2_p90p10_change_db": wb["p90_p10"] - wa["p90_p10"], "m2_iqr_change_db": wb["iqr"] - wa["iqr"],
         **{f"m3_{k}": v for k, v in L.m3(x_eq, xr, SR).items()},
         **{f"m45_{k}": v for k, v in rs.items()},
         **{f"m6_{k}": v for k, v in L.workload(x_eq, xr, SR, dyn).items()},
         "m7_consonant_change_db": L.m7(x_eq, xr, SR, segs),
         # raw -> after on the same (raw) vocal windows and word segments
         "raw_m1": L.m1(raw, SR, mask_r), "raw_m2": L.m2(raw, SR, segs_r),
         "after_m1": L.m1(y, SR, mask_r), "after_m2": L.m2(y, SR, segs_r),
         "gr": {"fast": s.fet_gr, "slow": s.opto_gr}}
    return r


def goals(m):
    return {"m1": -45 <= m["m1_iqr_change_pct"] <= -25, "m2": -5 <= m["m2_p90p10_change_db"] <= -2,
            "m3": m["m3_rho"] > 0.9 and 0.7 <= m["m3_r_macro"] <= 1.0,
            "m4": m["m45_lift_median_db"] < 0.25 and m["m45_lift_p95_db"] < 1.0,
            "m5": m["m45_motion_p95_db_s"] < 12, "m6": m["m6_change_pct"] <= -20, "m7": m["m7_consonant_change_db"] < 1}


def medians(rows):
    keys = [k for k, v in rows[0].items() if isinstance(v, float)]
    out = {k: float(np.nanmedian([r[k] for r in rows])) for k in keys}
    for grp in ("raw_m1", "after_m1", "raw_m2", "after_m2"):
        out[grp] = {k: float(np.nanmedian([r[grp][k] for r in rows])) for k in ("iqr", "p90_p10")}
    return out


def main():
    F = pro_features()
    medt, *_ = fit_targets(F)
    cfg = json.loads((RESULTS / "layer3_chain_config.json").read_text())
    t = mix.Targets(shape=medt["shape"], spread=medt["spread"], sib=medt["sib"],
                    eq_strength=cfg["eq_strength"], eq_cut_only=cfg["eq_cut_only"])
    pro = []
    for p in sorted(EXC.glob("*.npz")):
        v, _ = load_exc(p)
        v = v.astype(np.float64)
        if np.sqrt(np.mean(v ** 2)) > 1e-4:
            pro.append({"m1": L.m1(v, SR), "m2": L.m2(v, SR, L.words(v, SR))})
    pro_ref = {q: {f"m1_{k}": float(np.nanpercentile([r["m1"][k] for r in pro], q)) for k in ("iqr", "p90_p10")}
               | {f"m2_{k}": float(np.nanpercentile([r["m2"][k] for r in pro], q)) for k in ("iqr", "p90_p10")}
               for q in (25, 50)}
    print("pro", pro_ref, flush=True)

    voc = [load(DATA / "vocadito" / "Audio" / f"vocadito_{i}.wav", SR).astype(np.float64) for i in range(1, 41)]
    rng = np.random.default_rng(31)
    train = sorted(p.stem for p in EXC.glob("train__*.npz"))[1::3][:30]
    mus = [degrade(load_exc(EXC / f"{k}.npz")[0], np.random.default_rng(rng.integers(1 << 30)))[0] for k in train]
    grid = []
    for s_, mu in itertools.product((0.5, 0.7, 0.85), (0.1, 0.2, 0.3)):
        dyn = {"comp_mode": "serial", "rider_strength": s_, "rider_mu": mu, "fet_peak_gr": 4.0, "opto_avg_gr": 3.0}
        rows = [evaluate(r, t, dyn) for r in voc + mus]
        m = medians(rows)
        g = goals(m)
        grid.append({"rider_strength": s_, "rider_mu": mu, "goals_met": int(sum(g.values())), "goals": g,
                     "after_m1_iqr": m["after_m1"]["iqr"], "medians": m})
        print({"s": s_, "mu": mu, "met": sum(g.values()), "fail": [k for k, v in g.items() if not v],
               "after_iqr": round(m["after_m1"]["iqr"], 2)}, flush=True)
    tgt = pro_ref[25]["m1_iqr"]
    best = max(grid, key=lambda r: (r["goals_met"], -abs(r["after_m1_iqr"] - tgt)))
    dyn = {"comp_mode": "serial", "rider_strength": best["rider_strength"], "rider_mu": best["rider_mu"],
           "fet_peak_gr": 4.0, "opto_avg_gr": 3.0}

    rng = np.random.default_rng(41)
    test = []
    for p in sorted(EXC.glob("test__*.npz")):
        v, _ = load_exc(p)
        v = v.astype(np.float64)
        raw, _ = degrade(v, np.random.default_rng(rng.integers(1 << 30)))
        r = evaluate(raw, t, dyn)
        r["pro_m1"], r["pro_m2"] = L.m1(v, SR), L.m2(v, SR, L.words(v, SR))
        test.append(r)
    vrows = [evaluate(r, t, dyn) for r in voc]
    tm = medians(test)
    for grp in ("pro_m1", "pro_m2"):
        tm[grp] = {k: float(np.nanmedian([r[grp][k] for r in test])) for k in ("iqr", "p90_p10")}
    gr = {"fast_p99_db": float(np.median([r["gr"]["fast"]["p99_db"] for r in test])),
          "slow_mean_db": float(np.median([r["gr"]["slow"]["mean_sung_db"] for r in test]))}
    vm = medians(vrows)
    out = {"pro": pro_ref, "n_pro": len(pro), "grid": grid, "chosen": dyn, "test_50": tm, "test_goals": goals(tm),
           "vocadito_40": vm, "vocadito_goals": goals(vm), "test_gr_median": gr}
    save_json("dynamics_calibration.json", out)
    cfg = {k: v for k, v in cfg.items() if k not in ("rider_strength", "rider_mu", "fet_peak_gr", "opto_avg_gr", "comp_mode")}
    cfg.update(dyn)
    (RESULTS / "layer3_chain_config.json").write_text(json.dumps(cfg, indent=1))
    print(json.dumps({k: v for k, v in out.items() if k != "grid"}, indent=1, default=float))


if __name__ == "__main__":
    main()
