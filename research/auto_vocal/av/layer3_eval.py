"""Layer 3 — mix-fit validation on MUSDB18 (professional mixes; research-only data).

1. Targets. Per track, descriptors of the professional vocal stem and of the beat
   (drums+bass+other) over the 45 s excerpt (mixfeat.py). Train split (100) -> targets:
   median tonal shape / dynamic spread / sibilance / vocal-minus-beat loudness, and ridge
   models predicting the vocal's shape and loudness offset *from the beat* ("carving").
   Test split (50): does beat conditioning predict the pro vocal better than the median?
2. Paired restore. Each test vocal stem is "un-mixed" by a random, known degradation
   (tilt + resonance EQ, proximity boom, 1.6x dynamic expansion, sibilance shelf, room
   noise, ±10 dB level), then processed blind by each chain. Error vs the pro stem:
   shape MAE (dB over 24 bands), |Δspread|, |Δsib|, |Δ(vocal−beat) LUFS|, and the per-band
   vocal-to-beat ratio MAE (level and EQ together). Chains: untouched, fixed generic
   preset, auto (median targets), auto+beat (ridge targets).
3. Real raw vocals (Vocadito, CC BY 4.0, phone/home recordings): descriptors before/after
   the chain, as percentiles of the pro-vocal distribution (50 = typical pro).

What this can show: whether the chain moves measurable vocal statistics toward what
engineers produced on these songs. What it cannot show: perceived quality or taste; the
degradation is synthetic; MUSDB vocal stems already contain reverb/FX; one pro mix per
song is one valid answer, not the only one.
"""
from __future__ import annotations

import json

import numpy as np
from scipy.signal import firwin2, oaconvolve, sosfilt, butter

from . import mix, mixfeat
from .common import CACHE, DATA, RESULTS, load, save_json

EXC = DATA / "musdb_excerpts"
SR = 44100


def load_exc(p):
    z = np.load(p)
    return z["voc"].astype(np.float32), z["acc"].astype(np.float32)


def pro_features():
    out = CACHE / "l3_pro_feats.json"
    have = json.loads(out.read_text()) if out.exists() else {}
    for p in sorted(EXC.glob("*.npz")):
        if p.stem in have:
            continue
        v, a = load_exc(p)
        if np.sqrt(np.mean(v ** 2)) < 1e-4:
            continue
        f = mixfeat.features(v, SR, a)
        have[p.stem] = {k: (val.tolist() if isinstance(val, np.ndarray) else val) for k, val in f.items()}
        out.write_text(json.dumps(have))
    return {k: {kk: (np.array(vv) if isinstance(vv, list) else vv) for kk, vv in d.items()} for k, d in have.items()}


def fit_targets(F):
    from sklearn.linear_model import RidgeCV
    tr = [k for k in F if k.startswith("train__")]
    te = [k for k in F if k.startswith("test__")]
    X = lambda ks: np.stack([F[k]["acc_shape"] for k in ks])  # noqa: E731
    Ys = lambda ks: np.stack([F[k]["shape"] for k in ks])  # noqa: E731
    Yd = lambda ks: np.array([F[k]["delta_lufs"] for k in ks])  # noqa: E731
    alphas = np.logspace(-1, 4, 20)
    r_shape = RidgeCV(alphas=alphas).fit(X(tr), Ys(tr))
    r_delta = RidgeCV(alphas=alphas).fit(X(tr), Yd(tr))
    med = {"shape": np.median(Ys(tr), 0), "spread": float(np.median([F[k]["spread"] for k in tr])),
           "sib": float(np.median([F[k]["sib"] for k in tr])), "delta_lufs": float(np.median(Yd(tr)))}
    pred = {
        "shape_mae_median_target": float(np.mean(np.abs(Ys(te) - med["shape"]))),
        "shape_mae_ridge_from_beat": float(np.mean(np.abs(Ys(te) - r_shape.predict(X(te))))),
        "delta_mae_median_target": float(np.mean(np.abs(Yd(te) - med["delta_lufs"]))),
        "delta_mae_ridge_from_beat": float(np.mean(np.abs(Yd(te) - r_delta.predict(X(te))))),
        "pro_spread_iqr": np.percentile([F[k]["spread"] for k in tr], [25, 50, 75]).tolist(),
        "pro_sib_iqr": np.percentile([F[k]["sib"] for k in tr], [25, 50, 75]).tolist(),
        "pro_delta_iqr": np.percentile(Yd(tr), [25, 50, 75]).tolist(),
        "n_train": len(tr), "n_test": len(te),
        "alpha_shape": float(r_shape.alpha_), "alpha_delta": float(r_delta.alpha_),
    }
    # bootstrap CI of the ridge-vs-median shape gain on test tracks
    e_m = np.abs(Ys(te) - med["shape"]).mean(1)
    e_r = np.abs(Ys(te) - r_shape.predict(X(te))).mean(1)
    rng = np.random.default_rng(0)
    bs = [np.mean((e_m - e_r)[rng.integers(0, len(te), len(te))]) for _ in range(2000)]
    pred["shape_gain_ci95"] = np.percentile(bs, [2.5, 97.5]).tolist()
    d_m = np.abs(Yd(te) - med["delta_lufs"])
    d_r = np.abs(Yd(te) - r_delta.predict(X(te)))
    bs = [np.mean((d_m - d_r)[rng.integers(0, len(te), len(te))]) for _ in range(2000)]
    pred["delta_gain_ci95"] = np.percentile(bs, [2.5, 97.5]).tolist()
    return med, r_shape, r_delta, pred, te


def _fir(x, gains_db):
    f = np.concatenate([[0], mixfeat.CENTRES, [SR / 2]])
    gg = np.concatenate([[gains_db[0]], gains_db, [gains_db[-1]]])
    h = firwin2(2047, f / (SR / 2), 10 ** (gg / 20))
    return oaconvolve(x, h)[1023:1023 + len(x)]


def degrade(v, rng):
    c = mixfeat.CENTRES
    lg = np.log10(c / 1000)
    g = rng.uniform(-6, 6) * lg / 1.0  # tilt, ±6 dB per decade around 1 kHz
    f0, gp = 10 ** rng.uniform(np.log10(150), np.log10(5000)), rng.uniform(-8, 8)
    g += gp * np.exp(-0.5 * (np.log2(c / f0) / 0.5) ** 2)
    if rng.random() < 0.5:
        g += 6.0 * (c < 150)
    if rng.random() < 0.7:
        g += rng.uniform(4, 8) * (c > 6000)
    x = _fir(v.astype(np.float64), g)
    # 1.6x dynamic expansion around the median 50 ms level
    n = int(0.05 * SR)
    m = len(x) // n
    e = 10 * np.log10((x[: m * n].reshape(m, n) ** 2).mean(1) + 1e-12)
    on = e > np.percentile(e, 95) - 35
    gd = np.where(on, 0.6 * (e - np.median(e[on])), 0.0)
    gd = np.convolve(gd, np.ones(4) / 4, "same")
    x = mix._apply_gain(x, gd, n)
    # room noise ~ 55 dB under the voice, then a random level
    pk = np.sqrt(np.mean(x ** 2))
    noise = sosfilt(butter(1, 2000, "lowpass", fs=SR, output="sos"), rng.standard_normal(len(x)))
    x = x + noise / np.sqrt(np.mean(noise ** 2)) * pk * 10 ** (-45 / 20)
    x *= 10 ** (rng.uniform(-10, 10) / 20)
    return x.astype(np.float32), {"eq": g.round(1).tolist()}


def fixed_preset(x, beat, delta):
    """A typical generic vocal preset: HPF, mud cut, presence/air lift, 4:1 at the median."""
    s = mix.Settings()
    y = mix.hpf(np.asarray(x, np.float64), SR)
    c = mixfeat.CENTRES
    g = -2.0 * ((c > 200) & (c < 500)) + 3.0 * ((c > 2500) & (c < 6000)) + 2.0 * (c > 10000)
    y = _fir(y, g)
    w, h = int(0.4 * SR), int(0.1 * SR)
    rms = 20 * np.log10(np.sqrt(np.stack([y[i:i + w] ** 2 for i in range(0, len(y) - w, h)]).mean(1)) + 1e-9)
    thr = np.percentile(rms[rms > np.percentile(rms, 95) - 35], 50)
    e, n = mix._env_db(y, SR, 1.0)
    over = np.maximum(e - thr, 0)
    y = mix._apply_gain(y, mix._smooth_gain(-over * 0.75, n, SR, 5, 100), n)
    import pyloudnorm as pyln
    m = pyln.Meter(SR)
    y *= 10 ** (((m.integrated_loudness(np.asarray(beat, np.float64)) + delta) - m.integrated_loudness(y)) / 20)
    return y.astype(np.float32), s


def errors(fz, fp):
    return {"shape_mae": float(np.mean(np.abs(fz["shape"] - fp["shape"]))),
            "spread_err": float(abs(fz["spread"] - fp["spread"])),
            "sib_err": float(abs(fz["sib"] - fp["sib"])),
            "delta_err": float(abs(fz["delta_lufs"] - fp["delta_lufs"])),
            "var_mae": float(np.mean(np.abs(fz["var"] - fp["var"])))}


def paired(F, med, r_shape, r_delta, te):
    out = CACHE / "l3_paired.jsonl"
    done = {json.loads(line)["id"] for line in out.read_text().splitlines()} if out.exists() else set()
    rng = np.random.default_rng(11)
    with out.open("a") as fo:
        for k in te:
            seed = rng.integers(1 << 30)
            if k in done:
                continue
            v, a = load_exc(EXC / f"{k}.npz")
            fp = mixfeat.features(v, SR, a)
            raw, _ = degrade(v, np.random.default_rng(seed))
            t_med = mix.Targets(shape=med["shape"], spread=med["spread"], sib=med["sib"], delta_lufs=med["delta_lufs"])
            t_beat = mix.Targets(shape=r_shape.predict(fp["acc_shape"][None])[0], spread=med["spread"],
                                 sib=med["sib"], delta_lufs=float(r_delta.predict(fp["acc_shape"][None])[0]))
            rec = {"id": k}
            outs = {"untouched": raw, "fixed_preset": fixed_preset(raw, a, med["delta_lufs"])[0],
                    "auto": mix.auto_mix(raw, SR, a, t_med)[0], "auto_beat": mix.auto_mix(raw, SR, a, t_beat)[0]}
            cfg = RESULTS / "layer3_chain_config.json"
            if cfg.exists():  # the configuration chosen on the train split (layer3_tune.py)
                c = json.loads(cfg.read_text())
                t_tuned = mix.Targets(shape=med["shape"], spread=med["spread"], sib=med["sib"],
                                      delta_lufs=med["delta_lufs"], **c)
                outs["auto_tuned"] = mix.auto_mix(raw, SR, a, t_tuned)[0]
            for name, z in outs.items():
                rec[name] = errors(mixfeat.features(z, SR, a), fp)
            # how far apart are pro mixes themselves? another song's pro stats as the "answer"
            other = te[(te.index(k) + 1) % len(te)]
            fo_ = F[other]
            rec["other_pro_song"] = {"shape_mae": float(np.mean(np.abs(fo_["shape"] - fp["shape"]))),
                                     "spread_err": float(abs(fo_["spread"] - fp["spread"])),
                                     "sib_err": float(abs(fo_["sib"] - fp["sib"])),
                                     "delta_err": float(abs(fo_["delta_lufs"] - fp["delta_lufs"]))}
            fo.write(json.dumps(rec) + "\n")
            fo.flush()
            print(k, {n: round(rec[n]["shape_mae"], 2) for n in outs}, flush=True)


def raw_vocals(F, med):
    pro = {f: np.array([F[k][f] for k in F]) for f in ("spread", "sib")}
    pro_shape = np.stack([F[k]["shape"] for k in F])
    pct = lambda f, v: float((pro[f] < v).mean() * 100)  # noqa: E731
    cfg = RESULTS / "layer3_chain_config.json"
    c = json.loads(cfg.read_text()) if cfg.exists() else {}
    t = mix.Targets(shape=med["shape"], spread=med["spread"], sib=med["sib"], **c)
    rows = []
    for i in range(1, 41):
        v = load(DATA / "vocadito" / "Audio" / f"vocadito_{i}.wav", SR)
        z, s = mix.auto_mix(v, SR, None, t)
        r = {"id": i}
        for tag, x in (("before", v), ("after", z)):
            f = mixfeat.features(x, SR)
            r[tag] = {"spread": f["spread"], "spread_pct": pct("spread", f["spread"]), "sib": f["sib"],
                      "sib_pct": pct("sib", f["sib"]),
                      "shape_mae_to_pro_median": float(np.mean(np.abs(f["shape"] - np.median(pro_shape, 0))))}
        r["settings"] = {"comp_ratio": s.comp_ratio, "eq": s.eq_gains_db}
        rows.append(r)
        print(i, r["before"]["spread"], r["after"]["spread"], flush=True)
    return rows


def summarize():
    rows = [json.loads(line) for line in (CACHE / "l3_paired.jsonl").read_text().splitlines()]
    names = [n for n in ["untouched", "fixed_preset", "auto", "auto_beat", "auto_tuned", "other_pro_song"] if n in rows[0]]
    out = {}
    for n in names:
        out[n] = {m: float(np.median([r[n][m] for r in rows if m in r[n]]))
                  for m in ("shape_mae", "spread_err", "sib_err", "delta_err", "var_mae") if m in rows[0][n]}
    out["n"] = len(rows)
    # paired bootstrap (4000 resamples) of per-song error differences: positive = first chain better
    rng = np.random.default_rng(0)
    boot = {}
    for a, b in (("auto_tuned", "fixed_preset"), ("auto_tuned", "untouched"), ("auto_beat", "auto")):
        if a not in rows[0] or b not in rows[0]:
            continue
        for m in ("shape_mae", "spread_err", "sib_err", "delta_err", "var_mae"):
            d = np.array([r[b][m] - r[a][m] for r in rows])
            bs = [d[rng.integers(0, len(d), len(d))].mean() for _ in range(4000)]
            boot[f"{a}_vs_{b}_{m}"] = {"mean_gain": float(d.mean()), "ci95": np.percentile(bs, [2.5, 97.5]).tolist(),
                                       "win_rate": float((d > 0).mean())}
    out["bootstrap"] = boot
    return out


def main():
    F = pro_features()
    med, r_shape, r_delta, pred, te = fit_targets(F)
    save_json("layer3_targets.json", {"median_targets": {k: (v.tolist() if isinstance(v, np.ndarray) else v)
                                                          for k, v in med.items()},
                                      "prediction_test": pred, "band_centres_hz": mixfeat.CENTRES.round(0).tolist()})
    print(json.dumps(pred, indent=1))
    paired(F, med, r_shape, r_delta, te)
    save_json("layer3_paired_summary.json", summarize())
    save_json("layer3_vocadito.json", raw_vocals(F, med))
    print("done")


if __name__ == "__main__":
    main()
