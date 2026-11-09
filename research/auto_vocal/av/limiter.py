"""Offline lookahead true-peak limiter and loudness matching for the listening files.

True peak is read on a 4x oversampled signal (ITU-R BS.1770-4 style). The gain needed to
keep every true peak under the ceiling is spread over a 5 ms lookahead (moving minimum,
then a moving average of the same length, so the gain has reached its target before the
peak arrives) and recovers with an 80 ms release. Nothing is clipped.
"""
from __future__ import annotations

import numpy as np
import pyloudnorm as pyln
from scipy.ndimage import minimum_filter1d, uniform_filter1d
from scipy.signal import resample_poly


def true_peak(x):
    """Per-sample true peak: max |x| over the 4 oversampled points of each sample."""
    return np.abs(resample_poly(x, 4, 1)).reshape(-1, 4).max(1)


def limit(x, sr, ceiling_dbtp=-1.0, lookahead_ms=5.0, release_ms=80.0):
    """Returns (limited signal, stats: max/mean gain reduction in dB, share of time limiting)."""
    x = np.asarray(x, np.float64)
    c = 10 ** (ceiling_dbtp / 20)
    tp = true_peak(x)
    need = np.minimum(1.0, c / np.maximum(tp, 1e-12))
    L = max(1, int(sr * lookahead_ms / 1000))
    g = minimum_filter1d(need, 2 * L + 1)
    g = uniform_filter1d(g, L + 1)
    g = np.minimum(g, need)  # the average can never let a peak through
    # release: the gain may fall at once but rises with a one-pole release
    a = np.exp(-1.0 / (sr * release_ms / 1000))
    out = np.empty_like(g)
    s = 1.0
    for i, v in enumerate(g):
        s = v if v < s else a * s + (1 - a) * v
        out[i] = s
    y = x * out
    over = np.max(true_peak(y))
    if over > c:  # inter-sample residue after the gain ride: a final trim, never a clip
        y *= c / over
        out *= c / over
    gr = -20 * np.log10(np.maximum(out, 1e-9))
    return y, {"gr_max_db": float(gr.max()), "gr_mean_db_when_active": float(gr[gr > 0.1].mean()) if (gr > 0.1).any() else 0.0,
               "active_share": float((gr > 0.1).mean()), "true_peak_dbtp": float(20 * np.log10(np.max(true_peak(y)) + 1e-12))}


def master(x, sr, lufs=-14.0, ceiling_dbtp=-1.0):
    """Gain to the loudness target, then the limiter; the pre-gain is refined twice so the
    limited file lands on the target (matched loudness across before/after/reference)."""
    m = pyln.Meter(sr)
    x = np.asarray(x, np.float64)
    gain = lufs - m.integrated_loudness(x)
    for _ in range(3):
        y, st = limit(x * 10 ** (gain / 20), sr, ceiling_dbtp)
        err = lufs - m.integrated_loudness(y)
        if abs(err) < 0.1:
            break
        gain += err
    st["lufs"] = float(m.integrated_loudness(y))
    return y.astype(np.float32), st
