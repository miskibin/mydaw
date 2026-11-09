"""Artefact proxy at fixed pitch shifts: PSOLA (this prototype / the app's method) vs the
librosa phase vocoder, on 12 steady VocalSet takes. Writes results/layer2_artefacts.json.
Metric: mean |Δ| of 40-band mel dB vs the unshifted take over voiced frames (lower = less
spectral change for the same pitch move)."""
import json
import sys

import librosa
import numpy as np

sys.path.insert(0, ".")
from av import tune  # noqa: E402
from av.common import RESULTS  # noqa: E402
from av.layer2_pitch import vocalset_items  # noqa: E402
from av.layer2_tune_eval import SR, load_vs  # noqa: E402


def mel_db(z):
    return librosa.power_to_db(librosa.feature.melspectrogram(y=z, sr=SR, n_mels=40, hop_length=160), ref=1.0)


res = {}
items = [k for k, *_ in vocalset_items(140, seed=1)
         if not any(t in k for t in ("trill", "spoken", "fast"))][:12]
for k in items:
    y = load_vs(k)
    f0 = tune.track_f0(y, SR, "tiny")
    M0 = mel_db(y)
    v = (f0 > 0)[: M0.shape[1]]
    for cents in (15, 30, 45, 100):
        p = tune.psola(y.astype(np.float64), SR, f0, np.full(len(f0), 2 ** (cents / 1200)))
        pv = librosa.effects.pitch_shift(y, sr=SR, n_steps=cents / 100)
        for name, z in (("psola", p), ("phase_vocoder", pv)):
            M = mel_db(z)
            n = min(M.shape[1], len(v))
            res.setdefault(f"{name}_{cents}ct", []).append(float(np.abs(M[:, :n] - M0[:, :n])[:, v[:n]].mean()))
    print(k, flush=True)
out = {k: {"median": float(np.median(v)), "n": len(v)} for k, v in res.items()}
(RESULTS / "layer2_artefacts.json").write_text(json.dumps(out, indent=1))
print(json.dumps(out, indent=1))
