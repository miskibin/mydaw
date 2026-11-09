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


def evenness(x, sr):
    """How even the vocal level is, on sung frames only (all values in dB; lower = more even).

    Sung = within 20 dB of the take's p95 level (silence, breaths, noise and most printed
    reverb tails are out).
    mom_iqr / mom_std : IQR / std of the 400 ms momentary RMS level (100 ms hop) over the
                        whole excerpt. This includes deliberate section changes (verse vs
                        chorus).
    mom_local_iqr     : IQR of the 400 ms level minus its median over the sung windows within
                        ±2 s: the phrase-to-phrase and word-to-word unevenness a listener
                        hears, without section-level arrangement changes.
    word_iqr          : IQR of per-"word" levels (a run of sung 10 ms frames, split at gaps
                        >= 40 ms, length >= 80 ms; an energy segment, not a transcribed word).
    word_local_iqr    : IQR of each word's level minus the median of the words within ±3 s.
    """
    x = np.asarray(x, np.float64)
    w, h = int(0.4 * sr), int(0.1 * sr)
    idx = np.arange(0, max(len(x) - w, 1), h)
    rms = 10 * np.log10(np.array([np.mean(x[i:i + w] ** 2) for i in idx]) + 1e-14)
    on = rms > np.percentile(rms, 95) - 20
    m = rms[on]
    t_on = np.flatnonzero(on) * 0.1
    loc = np.array([v - np.median(m[np.abs(t_on - t) <= 2.0]) for v, t in zip(m, t_on)])
    n10 = int(0.01 * sr)
    k = len(x) // n10
    f = 10 * np.log10((x[: k * n10].reshape(k, n10) ** 2).mean(1) + 1e-14)
    act = f > np.percentile(f, 95) - 20
    words, centres, a, gap, last = [], [], None, 0, 0
    for i, v in enumerate(np.append(act, [False] * 4)):
        if v:
            if a is None:
                a = i
            gap = 0
            last = i
        elif a is not None:
            gap += 1
            if gap >= 4:
                if last - a >= 8:
                    words.append(10 * np.log10(np.mean(10 ** (f[a:last + 1] / 10))))
                    centres.append((a + last) / 2 * 0.01)
                a = None
    wl, wc = np.array(words), np.array(centres)
    wloc = np.array([wl[j] - np.median(wl[np.abs(wc - wc[j]) <= 3.0]) for j in range(len(wl))
                     if (np.abs(wc - wc[j]) <= 3.0).sum() >= 3])
    iqr = lambda v: float(np.percentile(v, 75) - np.percentile(v, 25)) if len(v) >= 4 else float("nan")  # noqa: E731
    return {"mom_iqr": iqr(m), "mom_std": float(np.std(m)), "mom_local_iqr": iqr(loc),
            "word_iqr": iqr(wl), "word_local_iqr": iqr(wloc), "n_words": int(len(wl))}
