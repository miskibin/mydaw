"""Layer 3 chain design on the TRAIN split only (40 MUSDB train tracks): the paired restore
test of layer3_eval.py run over a small grid of chain options. The best configuration by
mean normalised error is written to results/layer3_chain_config.json, and layer3_eval.py
then reports it once on the untouched test split.

Grid: EQ strength {0, 0.5, 1} x {boost+cut, cut only}; compressor {spread target, fixed 3:1}.
Score = mean over tracks of shape_mae/2.7 + spread_err/3.4 + sib_err/3.5 + delta_err/6.0
(each error divided by the untouched-raw test-set median, so the four count equally).
"""
from __future__ import annotations

import itertools
import json

import numpy as np

from . import mix, mixfeat
from .common import RESULTS, save_json
from .layer3_eval import EXC, SR, degrade, errors, fit_targets, load_exc, pro_features

NORM = {"shape_mae": 2.7, "spread_err": 3.4, "sib_err": 3.5, "delta_err": 6.0}


def main():
    F = pro_features()
    med, *_ = fit_targets(F)
    tr = sorted(k for k in F if k.startswith("train__"))[::2][:40]
    rng = np.random.default_rng(23)
    data = []
    for k in tr:
        v, a = load_exc(EXC / f"{k}.npz")
        raw, _ = degrade(v, np.random.default_rng(rng.integers(1 << 30)))
        data.append((k, raw, a, mixfeat.features(v, SR, a)))
    grid = list(itertools.product((0.0, 0.5, 1.0), (False, True), ("target", "fixed3")))
    res = []
    for eq_s, cut, comp in grid:
        if eq_s == 0.0 and cut:
            continue
        t = mix.Targets(shape=med["shape"], spread=med["spread"], sib=med["sib"], delta_lufs=med["delta_lufs"],
                        eq_strength=eq_s, eq_cut_only=cut, comp_mode=comp)
        errs = [errors(mixfeat.features(mix.auto_mix(raw, SR, a, t)[0], SR, a), fp) for _, raw, a, fp in data]
        m = {key: float(np.mean([e[key] for e in errs])) for key in NORM}
        score = float(np.mean([sum(e[key] / NORM[key] for key in NORM) / len(NORM) for e in errs]))
        res.append({"eq_strength": eq_s, "eq_cut_only": cut, "comp_mode": comp, "score": score, **m})
        print(res[-1], flush=True)
    best = min(res, key=lambda r: r["score"])
    save_json("layer3_chain_grid_train.json", {"n_train_tracks": len(data), "grid": res, "best": best})
    (RESULTS / "layer3_chain_config.json").write_text(json.dumps(
        {k: best[k] for k in ("eq_strength", "eq_cut_only", "comp_mode")}, indent=1))
    print("best", best)


if __name__ == "__main__":
    main()
