"""Layer 1 scoring: MIREX weighted key score and tempo Accuracy1/Accuracy2 per method.

Learned key profiles ("learned"): a 24-class logistic regression on the cached mean chroma
+ bass chroma, made transposition-equivariant by training on all 12 rotations of every
track. Trained on GTZAN and tested on GiantSteps, and vice versa (no test data seen).
"""
from __future__ import annotations

import json

import mir_eval
import numpy as np

from .common import CACHE, NOTE, key_str, save_json

KEY_METHODS = ["kk", "temperley", "albrecht", "wave", "learned"]
TEMPO_METHODS = ["librosa", "wave", "beatthis"]


def rows(name):
    p = CACHE / f"l1_{name}.jsonl"
    return [json.loads(line) for line in p.read_text().splitlines() if line.strip()] if p.exists() else []


def key_label(r):
    ref = r["ref"]
    if "key_root" not in ref:
        return None
    if "key_label" in ref and "|" in ref["key_label"]:  # ambiguous GiantSteps+ labels
        return None
    return key_str(ref["key_root"], ref["key_minor"])


def feats(r):
    c = np.array(r["chroma"])
    b = np.array(r["bass_chroma"])
    return np.concatenate([c / (c.sum() + 1e-9), b / (b.sum() + 1e-9)])


def rotate(f, k):
    return np.concatenate([np.roll(f[:12], k), np.roll(f[12:], k)])


def train_learned(train_rows):
    from sklearn.linear_model import LogisticRegression
    X, y = [], []
    for r in train_rows:
        lab = key_label(r)
        if lab is None:
            continue
        root, minor = r["ref"]["key_root"], r["ref"]["key_minor"]
        f = feats(r)
        for k in range(12):
            X.append(rotate(f, k))
            y.append(((root + k) % 12) + 12 * int(minor))
    clf = LogisticRegression(C=10.0, max_iter=3000)
    clf.fit(np.log(np.array(X) + 1e-3), y)
    return clf


def predict_learned(clf, r):
    c = int(clf.predict(np.log(feats(r) + 1e-3)[None])[0])
    return key_str(c % 12, c >= 12)


def score_keys(rs, clf, subset=None):
    out = {}
    for m in KEY_METHODS:
        sc, acc, n = [], [], 0
        for r in rs:
            ref = key_label(r)
            if ref is None or (subset and not subset(r)):
                continue
            if m == "learned":
                est = predict_learned(clf, r)
            else:
                k = r.get(f"key_{m}")
                if k is None or k[0] < 0:
                    est = "C major"  # failed estimate counts as wrong-ish, never skipped
                else:
                    est = key_str(k[0], bool(k[1]))
            s = mir_eval.key.evaluate(ref, est)
            sc.append(s["Weighted Score"])
            acc.append(ref == est)
            n += 1
        out[m] = {"weighted": float(np.mean(sc)), "correct": float(np.mean(acc)), "n": n}
    return out


def tempo_ok(est, ref, tol=0.04):
    a1 = abs(est - ref) <= tol * ref
    a2 = any(abs(est - ref * f) <= tol * ref * f for f in (1, 2, 3, 0.5, 1 / 3))
    return a1, a2


def score_tempo(rs, ref_key, subset=None):
    out = {}
    for m in TEMPO_METHODS:
        a1s, a2s = [], []
        for r in rs:
            ref = r["ref"].get(ref_key)
            if ref is None or (subset and not subset(r)):
                continue
            est = r.get(f"bpm_{m}", float("nan"))
            if not np.isfinite(est):
                a1s.append(False)
                a2s.append(False)
                continue
            a1, a2 = tempo_ok(est, ref)
            a1s.append(a1)
            a2s.append(a2)
        if a1s:
            out[m] = {"acc1": float(np.mean(a1s)), "acc2": float(np.mean(a2s)), "n": len(a1s)}
    return out


def main():
    gk, gt, gz = rows("giantsteps_key"), rows("giantsteps_tempo"), rows("gtzan")
    res = {}
    clf_on_gtzan = train_learned(gz) if gz else None
    clf_on_gs = train_learned(gk) if gk else None
    if gk and clf_on_gtzan:
        res["key_giantsteps"] = score_keys(gk, clf_on_gtzan)
    if gz and clf_on_gs:
        res["key_gtzan"] = score_keys(gz, clf_on_gs)
        res["key_gtzan_hiphop"] = score_keys(gz, clf_on_gs, lambda r: r["ref"]["genre"] == "hiphop")
    if gt:
        res["tempo_giantsteps_v2"] = score_tempo(gt, "bpm_v2")
        res["tempo_giantsteps_v1"] = score_tempo(gt, "bpm_v1")
    if gz:
        res["tempo_gtzan"] = score_tempo(gz, "bpm")
        res["tempo_gtzan_hiphop"] = score_tempo(gz, "bpm", lambda r: r["ref"]["genre"] == "hiphop")
    p = save_json("layer1_scores.json", res)
    print(json.dumps(res, indent=1))
    print("->", p, NOTE[0])


if __name__ == "__main__":
    main()
