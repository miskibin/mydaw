"""Aggregate layer 2 caches into results/layer2_scores.json."""
from __future__ import annotations

import json

import numpy as np

from .common import CACHE, save_json

ORNAMENTED = ("trill", "trillo", "lip_trill", "spoken")


def load(name):
    p = CACHE / name
    return [json.loads(line) for line in p.read_text().splitlines() if line.strip()] if p.exists() else []


def pitch_table():
    out = {}
    for ds in ("vocadito", "mir1k", "mir1k_mix", "vocalset"):
        rs = load(f"l2_{ds}.jsonl")
        if not rs:
            continue
        ids = {}
        for r in rs:
            ids.setdefault(r["method"], {})[r["id"]] = r
        common = set.intersection(*(set(v) for v in ids.values()))
        out[ds] = {m: {k: float(np.nanmean([v[i][k] for i in common])) for k in ("rpa", "rca", "vr", "vfa", "oa", "med_cents")}
                   | {"n": len(common)} for m, v in ids.items()}
    return out


def tune_table():
    rs = load("l2b_vocalset.jsonl")
    out = {}
    if rs:
        steady = [r for r in rs if not any(t in r["id"] for t in ORNAMENTED)]
        conds = [c for c in rs[0] if isinstance(rs[0][c], dict)]
        for label, sub in (("steady", steady), ("ornamented", [r for r in rs if r not in steady])):
            if not sub:
                continue
            out[f"vocalset_{label}"] = {
                c: {k: float(np.nanmedian([r[c][k] for r in sub])) for k in ("abs_err_med", "wrong", "within10", "vib_ratio", "spec")}
                for c in conds} | {"n_takes": len(sub), "n_notes": int(sum(r["notes"] for r in sub))}
            vib = [r for r in sub if "vibrato" in r["id"]]
            if vib:
                out[f"vocalset_{label}"]["vibrato_takes_vib_ratio"] = {
                    c: float(np.nanmedian([r[c]["vib_ratio"] for r in vib])) for c in conds}
            out[f"vocalset_{label}"]["natural_strength_on_detuned_median"] = float(np.median([r["natural_strength"] for r in sub]))
            out[f"vocalset_{label}"]["natural_strength_on_clean_median"] = float(np.median([r["natural_on_clean_strength"] for r in sub]))
    rs = load("l2b_mir1k.jsonl")
    if rs:
        out["mir1k_real"] = {"n": len(rs),
                             "input_dev_med_scale": float(np.median([r["input"]["dev_med_scale"] for r in rs])),
                             "input_dev_med_chromatic": float(np.median([r["input"]["dev_med_chromatic"] for r in rs])),
                             "scale_gated_share": float(np.mean([r["hard"]["scale_gated"] for r in rs])),
                             "on_scale_median": float(np.median([r["hard"]["on_scale"] for r in rs])),
                             **{f"{s}_{k}": float(np.median([r[s][k] for r in rs])) for s in ("natural", "hard")
                                for k in ("dev_in_same_grid", "dev_med", "moved_gt50", "strength", "spec")}}
    return out


def main():
    res = {"pitch_tracking": pitch_table(), "correction": tune_table()}
    save_json("layer2_scores.json", res)
    print(json.dumps(res, indent=1))


if __name__ == "__main__":
    main()
