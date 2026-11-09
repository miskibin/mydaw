"""Diagnose where hard-tune error comes from: tracker, tuning estimate or PSOLA."""
import sys

import numpy as np

sys.path.insert(0, ".")
from av import tune  # noqa: E402
from av.layer2_pitch import vocalset_items  # noqa: E402
from av.layer2_tune_eval import SR, load_vs, measure, note_stats  # noqa: E402

stem = sys.argv[1] if len(sys.argv) > 1 else "f1_scales_breathy_o"
for k, get, rt, rf in vocalset_items(14, seed=1):
    if k == stem:
        break
y = load_vs(k)
t = np.arange(int(len(y) / SR / 0.01)) * 0.01
ann = np.interp(t, rt, rf, left=0, right=0)
near = rf[np.clip(np.searchsorted(rt, t), 0, len(rf) - 1)]
ann = np.where(near > 0, ann, 0.0)
am = tune.hz2midi(ann)
notes = tune.segment_notes(am)
tun0 = tune.estimate_tuning(am)
intended = [round(n.centre - tun0 / 100) + tun0 / 100 for n in notes]
print("tun0", tun0, "notes", len(notes))
m0 = measure(y)
e, _ = note_stats(m0, notes, intended)
print("original", np.round(e[:12]))
for label, f0, tun in (("oracle f0+tun", ann, tun0), ("tiny f0, oracle tun", tune.track_f0(y, SR, "tiny"), tun0),
                       ("full f0, oracle tun", tune.track_f0(y, SR, "full"), tun0)):
    z, info = tune.autotune(y, SR, None, False, "hard", f0=f0, tuning_cents=tun)
    e, _ = note_stats(measure(z), notes, intended)
    tgt = info["target"]
    est = tune.hz2midi(f0)
    print(label, "med|e|", np.nanmedian(np.abs(e)), np.round(e[:12]))
    d = 100 * (est - am)
    print("   tracker vs annotation cents: median |d|", np.nanmedian(np.abs(d)), "voiced", np.mean(~np.isnan(est)))
    tg = np.array([np.nanmedian(tgt[n.a:n.b]) for n in notes])
    print("   target - intended", np.round(100 * (tg - np.array(intended))[:12]))
