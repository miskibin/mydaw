"""Vocal levelling metrics (owner's research brief, 2026-11-09). All levels K-weighted.

Paired metrics compare a stage's input with its output on the input's vocal regions:
  m1 loud400  : 400 ms loudness every 100 ms on vocal windows: median, IQR, P90-P10
  m2 words    : per-word level (energy segments of the input, gaps >= 40 ms): IQR, P90-P10
  m3 phrase   : 3 s loudness curve (0.5 s hop) in vs out: Spearman rho, R_macro = sd_out/sd_in
  m4 lift     : rider gain on non-vocal frames: median, P95 (breath/noise lift)
  m5 motion   : P95 |dg/dt| of the rider gain on active frames, dB/s
  m6 workload : P95 gain reduction of the fast and slow stages with frozen thresholds,
                with vs without the rider
  m7 consonant: per word, onset peak (max 5 ms power in its first 40 ms) minus word level;
                median |change| in vs out
"""
from __future__ import annotations

import numpy as np
from scipy.ndimage import uniform_filter1d
from scipy.stats import spearmanr

from . import mix


def _pw(x, sr, win_s, hop_s):
    k = mix.k_weight(x, sr) ** 2
    hop = int(hop_s * sr)
    return uniform_filter1d(k, max(1, int(win_s * sr)))[hop // 2::hop] + 1e-14


def vocal_windows(ref, sr):
    lr = 10 * np.log10(_pw(ref, sr, 0.4, 0.1))
    return lr > np.percentile(lr, 95) - 20


def m1(x, sr, mask=None):
    lv = 10 * np.log10(_pw(x, sr, 0.4, 0.1))
    mask = vocal_windows(x, sr) if mask is None else mask[: len(lv)]
    v = lv[: len(mask)][mask]
    return {"median": float(np.median(v)), "iqr": float(np.subtract(*np.percentile(v, [75, 25]))),
            "p90_p10": float(np.subtract(*np.percentile(v, [90, 10])))}


def words(ref, sr):
    """Energy segmentation of the reference: (start, end) in 5 ms frames."""
    p = _pw(ref, sr, 0.005, 0.005)
    lv = 10 * np.log10(p)
    act = lv > np.percentile(lv, 95) - 20
    segs, a, gap, last = [], None, 0, 0
    for i, v in enumerate(np.append(act, [False] * 9)):
        if v:
            a = i if a is None else a
            gap, last = 0, i
        elif a is not None:
            gap += 1
            if gap >= 8:
                if last - a >= 16:
                    segs.append((a, last + 1))
                a = None
    return segs


def m2(x, sr, segs):
    p = _pw(x, sr, 0.005, 0.005)
    lv = np.array([10 * np.log10(p[a:b].mean()) for a, b in segs if b <= len(p)])
    if len(lv) < 4:
        return {"iqr": float("nan"), "p90_p10": float("nan"), "n": int(len(lv))}
    return {"iqr": float(np.subtract(*np.percentile(lv, [75, 25]))),
            "p90_p10": float(np.subtract(*np.percentile(lv, [90, 10]))), "n": int(len(lv))}


def m7(x_in, x_out, sr, segs):
    pi, po = _pw(x_in, sr, 0.005, 0.005), _pw(x_out, sr, 0.005, 0.005)
    d = []
    for a, b in segs:
        if b > len(pi) or b - a < 16:
            continue
        on = slice(a, a + 8)
        ci = 10 * np.log10(pi[on].max() / pi[a:b].mean())
        co = 10 * np.log10(po[on].max() / po[a:b].mean())
        d.append(abs(co - ci))
    return float(np.median(d)) if d else float("nan")


def m3(x_in, x_out, sr):
    li = 10 * np.log10(_pw(x_in, sr, 3.0, 0.5))
    lo = 10 * np.log10(_pw(x_out, sr, 3.0, 0.5))
    n = min(len(li), len(lo))
    li, lo = li[:n], lo[:n]
    ok = li > np.percentile(li, 90) - 15
    if ok.sum() < 5:
        return {"rho": float("nan"), "r_macro": float("nan")}
    return {"rho": float(spearmanr(li[ok], lo[ok]).correlation), "r_macro": float(np.std(lo[ok]) / max(np.std(li[ok]), 1e-6))}


def rider_stats(curve):
    g, t, conf, active = curve
    non = (conf < 0.2) | ~active
    dg = np.abs(np.diff(g)) / 0.01
    act = active[1:]
    return {"lift_median_db": float(np.median(g[non])) if non.any() else 0.0,
            "lift_p95_db": float(np.percentile(g[non], 95)) if non.any() else 0.0,
            "motion_p95_db_s": float(np.percentile(dg[act], 95)) if act.any() else 0.0,
            "gain_p5_db": float(np.percentile(g[conf > 0.5], 5)) if (conf > 0.5).any() else 0.0,
            "gain_p95_db": float(np.percentile(g[conf > 0.5], 95)) if (conf > 0.5).any() else 0.0}


def workload(x_eq, x_rid, sr, dyn):
    """P95 GR of fast and slow stages, thresholds frozen from the no-rider run."""
    s0 = mix.Settings()
    y0 = mix.fet(x_eq, sr, s0, peak_gr=dyn.get("fet_peak_gr", 4.0))
    mix.opto(y0, sr, s0, avg_gr=dyn.get("opto_avg_gr", 3.0))
    s1 = mix.Settings()
    y1 = mix.fet(x_rid, sr, s1, thr=s0.fet_thr_db)
    mix.opto(y1, sr, s1, thr=s0.opto_thr_db)
    tot0 = s0.fet_gr["p95_sung_db"] + s0.opto_gr["p95_db"]
    tot1 = s1.fet_gr["p95_sung_db"] + s1.opto_gr["p95_db"]
    return {"p95_gr_without_rider_db": float(tot0), "p95_gr_with_rider_db": float(tot1),
            "change_pct": float(100 * (tot1 - tot0) / max(tot0, 1e-6))}
