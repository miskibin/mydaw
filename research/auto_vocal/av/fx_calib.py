"""Choose the reverb send from data: how much late energy do professional vocal stems carry?

tail ratio (dB): at every phrase end that is followed by >= 600 ms without singing and whose
tail is still decaying (300-600 ms at least 3 dB under 80-300 ms), the energy 80-300 ms after
the end relative to the last 100 ms before it (median over phrase ends, >= 3 per take). A
dry vocal shows only its room/noise floor there; reverb and delay raise it.

1. Distribution over the 150 MUSDB vocal stems (professional mixes, FX printed in the stem).
2. Dry takes (Vocadito 40 home takes + 60 studio VocalSet takes) through the chain without FX, then with the plate at
   sends -33…-12 dB and T60 0.8 / 1.2 s. The chosen send is the one whose median tail ratio
   lands on the pro 25th percentile (see the comment at `target`). Written to results/fx_calibration.json and used by make_samples.
"""
from __future__ import annotations

import json

import numpy as np
from scipy.signal import oaconvolve

from . import mix
from .common import DATA, RESULTS, load, save_json
from .layer3_eval import EXC, SR, fit_targets, load_exc, pro_features

HOP = 441  # 10 ms


def tail_ratio(x, sr=SR):
    n = len(x) // HOP
    p = (x[: n * HOP].reshape(n, HOP) ** 2).mean(1) + 1e-14
    d = 10 * np.log10(p)
    top = np.percentile(d, 95)
    # phrase activity is judged 15 dB under the peaks so that a reverb tail (typically 15+ dB
    # down) does not extend the phrase and hide itself
    act = d > top - 15
    # merge gaps shorter than 50 ms
    idx = np.flatnonzero(act)
    if len(idx) < 50:
        return float("nan")
    segs = []
    a = prev = idx[0]
    for i in idx[1:]:
        if i - prev > 5:
            segs.append((a, prev + 1))
            a = i
        prev = i
    segs.append((a, prev + 1))
    vals = []
    for k, (s0, e) in enumerate(segs):
        nxt = segs[k + 1][0] if k + 1 < len(segs) else n
        if e - s0 < 20 or nxt - e < 60 or e + 60 > n:
            continue
        tail, later = p[e + 8:e + 30].mean(), p[e + 30:e + 60].mean()
        # only decaying tails count: a constant bed (backing vocals, doubles, bleed in the
        # stem) is not reverb and would inflate the pro target
        if 10 * np.log10(tail / later) < 3.0:
            continue
        vals.append(10 * np.log10(tail / p[e - 10:e].mean()))
    return float(np.median(vals)) if len(vals) >= 3 else float("nan")


def main():
    pro = []
    for p in sorted(EXC.glob("*.npz")):
        v, _ = load_exc(p)
        r = tail_ratio(v.astype(np.float64))
        if np.isfinite(r):
            pro.append(r)
    pro = np.array(pro)
    F = pro_features()
    med, *_ = fit_targets(F)
    cfg = json.loads((RESULTS / "layer3_chain_config.json").read_text())
    t = mix.Targets(shape=med["shape"], spread=med["spread"], sib=med["sib"], **cfg)
    sends = [-33, -30, -27, -24, -21, -18, -15, -12]
    grid = {f"{t60}": {str(sd): [] for sd in sends} for t60 in (0.8, 1.2)}
    dry = []
    from .layer2_pitch import VS
    srcs = [DATA / "vocadito" / "Audio" / f"vocadito_{i}.wav" for i in range(1, 41)]
    vs = sorted((VS / "audio").rglob("*.wav"))
    srcs += [vs[k] for k in np.random.default_rng(3).choice(len(vs), 60, replace=False)]
    for i, src in enumerate(srcs):
        v = load(src, SR).astype(np.float64)
        z, _ = mix.auto_mix(v, SR, None, t)
        z = z.astype(np.float64)
        r0 = tail_ratio(z)  # NaN for a truly dry take: no decaying tail at all
        dry.append(r0)
        for t60 in (0.8, 1.2):
            ir = mix.plate_ir(SR, t60)
            wet = oaconvolve(z, ir)[: len(z)]
            for sd in sends:
                grid[f"{t60}"][str(sd)].append(tail_ratio(z + 10 ** (sd / 20) * wet))
        print(i, r0, flush=True)
    # target: the pro 25th percentile, not the median. A stem's tail holds ALL its printed
    # FX (delay throws, doubles, room), so the median overstates what the reverb alone does;
    # the median-matched send is reported too.
    target = float(np.percentile(pro, 25))
    target_med = float(np.median(pro))
    curve = {t60: {sd: float(np.nanmedian(v)) for sd, v in g.items()} for t60, g in grid.items()}
    best = {}
    for t60, c in curve.items():
        sd = min(c, key=lambda k: abs(c[k] - target))
        best[t60] = {"send_db": float(sd), "median_tail_db": c[sd]}
    t60_pick = "1.2" if abs(best["1.2"]["median_tail_db"] - target) < abs(best["0.8"]["median_tail_db"] - target) else "0.8"
    out = {"pro_tail_ratio_db": {"median": target_med, "iqr": np.percentile(pro, [25, 75]).tolist(), "n": len(pro)},
           "dry_tail_ratio_db": {"median_where_measurable": float(np.nanmedian(dry)) if np.isfinite(dry).any() else None,
                                 "measurable_share": float(np.isfinite(dry).mean()), "n": len(dry)},
           "send_curve_measurable_share": {t60: {sd: float(np.isfinite(v).mean()) for sd, v in g.items()} for t60, g in grid.items()},
           "send_curve_median_tail_db": curve, "best_per_t60": best,
           "target_tail_db": {"used_p25": target, "median": target_med},
           "median_matched": {t60: {"send_db": float(min(c, key=lambda k: abs(c[k] - target_med)))} for t60, c in curve.items()},
           "chosen": {"t60": float(t60_pick), "send_db": best[t60_pick]["send_db"]},
           "delay": "off: echo level in pro stems was not measured, so no data-backed setting exists"}
    save_json("fx_calibration.json", out)
    print(json.dumps(out, indent=1))


if __name__ == "__main__":
    main()
