"""Objective vocal-in-mix descriptors used by layer 3 (all on mono float signals).

shape   24 one-third-octave bands, 63 Hz – 16 kHz: long-term average spectrum (dB) over
        vocal-active frames, minus its mean (tonal balance, level-free)
lufs    integrated loudness (ITU-R BS.1770 via pyloudnorm)
spread  dynamic spread: p95 − p10 of 400 ms RMS (dB) over vocal-active windows
crest   median peak-to-RMS (dB) in 400 ms windows
sib     sibilance: p95 over active frames of [energy 5–10 kHz − energy 0.1–5 kHz] (dB)
"""
from __future__ import annotations

import numpy as np
import pyloudnorm as pyln

EDGES = 63.0 * 2 ** (np.arange(25) / 3.0)  # 24 bands to ~16 kHz
CENTRES = np.sqrt(EDGES[:-1] * EDGES[1:])
NFFT, HOP = 4096, 2048


def _stft_power(x, sr):
    from scipy.signal import stft
    _, _, Z = stft(x, sr, nperseg=NFFT, noverlap=NFFT - HOP, boundary=None, padded=False)
    return np.abs(Z) ** 2  # (freq, frames)


def active_frames(x, sr, rel_db=35.0):
    """Frames (HOP grid) whose RMS is within rel_db of the take's 95th percentile."""
    n = len(x) // HOP
    r = np.sqrt((x[: n * HOP].reshape(n, HOP) ** 2).mean(1) + 1e-12)
    d = 20 * np.log10(r)
    return d > (np.percentile(d, 95) - rel_db)


def band_db(P, sr, frames):
    f = np.fft.rfftfreq(NFFT, 1 / sr)
    m = P[:, frames[: P.shape[1]]].mean(1) if frames[: P.shape[1]].any() else P.mean(1)
    out = np.empty(24)
    for b in range(24):
        sel = (f >= EDGES[b]) & (f < EDGES[b + 1])
        out[b] = 10 * np.log10(m[sel].mean() + 1e-14)
    return out


def features(v, sr, acc=None):
    act = active_frames(v, sr)
    P = _stft_power(v, sr)
    bd = band_db(P, sr, act)
    feats = {"band_db": bd, "shape": bd - bd.mean()}
    meter = pyln.Meter(sr)
    feats["lufs"] = float(meter.integrated_loudness(v.astype(np.float64)))
    w = int(0.4 * sr)
    h = int(0.1 * sr)
    idx = np.arange(0, len(v) - w, h)
    seg = np.stack([v[i:i + w] for i in idx]) if len(idx) else v[None, :w]
    rms = 20 * np.log10(np.sqrt((seg ** 2).mean(1)) + 1e-9)
    pk = 20 * np.log10(np.abs(seg).max(1) + 1e-9)
    on = rms > np.percentile(rms, 95) - 35
    feats["spread"] = float(np.percentile(rms[on], 95) - np.percentile(rms[on], 10))
    feats["crest"] = float(np.median(pk[on] - rms[on]))
    f = np.fft.rfftfreq(NFFT, 1 / sr)
    hf = P[(f >= 5000) & (f < 10000)].sum(0)
    lf = P[(f >= 100) & (f < 5000)].sum(0)
    a = act[: P.shape[1]]
    ratio = 10 * np.log10(hf[a] + 1e-14) - 10 * np.log10(lf[a] + 1e-14)
    feats["sib"] = float(np.percentile(ratio, 95)) if len(ratio) else float("nan")
    if acc is not None:
        Pa = _stft_power(acc, sr)
        feats["acc_band_db"] = band_db(Pa, sr, act)
        feats["acc_shape"] = feats["acc_band_db"] - feats["acc_band_db"].mean()
        feats["acc_lufs"] = float(meter.integrated_loudness(acc.astype(np.float64)))
        feats["delta_lufs"] = feats["lufs"] - feats["acc_lufs"]
        feats["var"] = bd - feats["acc_band_db"]  # vocal-to-accompaniment ratio per band, active frames
    return feats
