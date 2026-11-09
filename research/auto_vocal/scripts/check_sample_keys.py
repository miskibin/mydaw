"""Sanity check for the samples: does the beat key fit the PRO vocal's notes? Share of
pro-vocal note time on the estimated scale, and the best-fitting key from the vocal itself."""
import sys

import numpy as np

sys.path.insert(0, ".")
from av import tune  # noqa: E402
from av.layer1_run import PROFILES, profile_key  # noqa: E402
from av.layer3_eval import EXC, load_exc  # noqa: E402
from av.make_samples import PICKS, SR, beat_key, window  # noqa: E402

for src, name, _ in PICKS:
    if src != "musdb":
        continue
    pro, acc = load_exc(EXC / f"{name}.npz")
    a, b = window(pro)
    pro, acc = pro[a:b], acc[a:b]
    root, minor, _ = beat_key(acc)
    m = tune.hz2midi(tune.track_f0(pro, SR, "tiny"))
    pcs = np.mod(np.round(m[~np.isnan(m)]), 12).astype(int)
    hist = np.bincount(pcs, minlength=12).astype(float)
    on = hist[tune.scale_pcs(root, minor)].sum() / hist.sum()
    vr, vm = profile_key(hist, *PROFILES["kk"])
    von = hist[tune.scale_pcs(vr, vm)].sum() / hist.sum()
    print(f"{name}: beat key {root} {'min' if minor else 'maj'} -> {on:.0%} of vocal on scale; "
          f"vocal's own key {vr} {'min' if vm else 'maj'} -> {von:.0%}")
