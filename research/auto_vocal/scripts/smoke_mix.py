"""Smoke test: degrade one MUSDB vocal, run the chain, print descriptors and timings."""
import sys
import time

import numpy as np

sys.path.insert(0, ".")
from av import mix, mixfeat  # noqa: E402
from av.layer3_eval import EXC, SR, degrade, fixed_preset, load_exc  # noqa: E402

p = sorted(EXC.glob("*.npz"))[0]
v, a = load_exc(p)
fp = mixfeat.features(v, SR, a)
raw, _ = degrade(v, np.random.default_rng(1))
fr = mixfeat.features(raw, SR, a)
t = mix.Targets(shape=fp["shape"], spread=fp["spread"], sib=fp["sib"], delta_lufs=fp["delta_lufs"])
t0 = time.time()
z, s = mix.auto_mix(raw, SR, a, t)
print("chain s", round(time.time() - t0, 1), s)
fz = mixfeat.features(z, SR, a)
ff = mixfeat.features(fixed_preset(raw, a, fp["delta_lufs"])[0], SR, a)
for name, f in (("pro", fp), ("raw", fr), ("auto(oracle targets)", fz), ("fixed", ff)):
    print(f"{name:22s} shapeMAE={np.mean(np.abs(f['shape'] - fp['shape'])):.2f} spread={f['spread']:.1f} "
          f"sib={f['sib']:.1f} delta={f['delta_lufs']:.1f}")
