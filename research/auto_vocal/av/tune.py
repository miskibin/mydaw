"""Scale-aware vocal pitch correction: F0 tracking, note segmentation, an adaptive
correction curve, and TD-PSOLA resynthesis (formants follow the waveform, as in the
app's `pitch::Psola`).

Styles
  natural : each note's centre is moved toward the target note by a strength that adapts
            to how off-key the take is (in-tune singers are left almost untouched);
            vibrato, scoops and drift inside a note are kept; 40 ms ramps between notes.
  hard    : every frame snaps to the nearest scale note (with hysteresis), zero retune
            time: flat notes and stepped transitions, the "trap" tune sound.
"""
from __future__ import annotations

from dataclasses import dataclass

import numpy as np

MAJOR = [0, 2, 4, 5, 7, 9, 11]
MINOR = [0, 2, 3, 5, 7, 8, 10]
HOP_S = 0.01
SCALE_GATE = 0.75  # min share of sung note time on the beat's scale to snap to that scale


def hz2midi(hz):
    hz = np.asarray(hz, float)
    with np.errstate(divide="ignore", invalid="ignore"):
        return np.where(hz > 0, 69 + 12 * np.log2(hz / 440.0), np.nan)


def midi2hz(m):
    return 440.0 * 2 ** ((np.asarray(m, float) - 69) / 12)


def track_f0(y, sr, model="full"):
    """torchcrepe (MIT code and weights), 10 ms hop, Viterbi; unvoiced where periodicity <= 0.21."""
    import torch
    import torchcrepe
    import soxr
    y16 = soxr.resample(y, sr, 16000) if sr != 16000 else y
    f0, per = torchcrepe.predict(torch.tensor(y16, dtype=torch.float32)[None], 16000, 160, 50, 1100, model,
                                 decoder=torchcrepe.decode.viterbi, batch_size=512, device="cpu",
                                 return_periodicity=True)
    per = torchcrepe.filter.median(per, 3)[0].numpy()
    f0 = f0[0].numpy()
    return np.where(per > 0.21, f0, 0.0)


def scale_pcs(root: int | None, minor: bool):
    if root is None:
        return list(range(12))
    return sorted(((p + root) % 12) for p in (MINOR if minor else MAJOR))


def snap(m, pcs):
    """Nearest MIDI note whose pitch class is in pcs."""
    base = np.floor(m)
    cands = np.stack([base + k for k in range(-2, 4)], -1)
    ok = np.isin(np.mod(cands, 12).astype(int), pcs)
    d = np.where(ok, np.abs(cands - m[..., None]), np.inf)
    return np.take_along_axis(cands, d.argmin(-1)[..., None], -1)[..., 0]


@dataclass
class Note:
    a: int
    b: int  # frame range [a, b)
    centre: float  # MIDI


def segment_notes(midi, min_len=6, jump=0.6):
    """Voiced runs split where the smoothed pitch leaves the running note centre by > jump."""
    from scipy.ndimage import median_filter
    v = ~np.isnan(midi)
    sm = midi.copy()
    sm[v] = median_filter(np.nan_to_num(midi), 5)[v]
    notes = []
    i, n = 0, len(midi)
    while i < n:
        if not v[i]:
            i += 1
            continue
        a = i
        ref = sm[i]
        out = 0
        while i < n and v[i]:
            if abs(sm[i] - ref) > jump:
                out += 1
                if out >= 3:
                    i -= 2
                    break
            else:
                out = 0
                ref = 0.9 * ref + 0.1 * sm[i] if i - a < 15 else np.median(sm[a:i + 1])
            i += 1
        b = i
        if b - a >= min_len:
            seg = midi[a:b]
            lo, hi = a + (b - a) // 5, b - (b - a) // 5
            notes.append(Note(a, b, float(np.nanmedian(midi[lo:hi] if hi > lo else seg))))
    return notes


def intonation(notes, pcs):
    """Per-note deviation (cents) of the note centre from its nearest scale note."""
    if not notes:
        return np.array([])
    c = np.array([n.centre for n in notes])
    return 100 * (c - snap(c, pcs))


def estimate_tuning(midi):
    """Global tuning of a take (cents vs A440), circular mean of per-frame offsets."""
    m = midi[~np.isnan(midi)]
    if len(m) == 0:
        return 0.0
    ang = 2 * np.pi * (m - np.round(m))
    return float(100 * np.angle(np.exp(1j * ang).mean()) / (2 * np.pi))


def correction_curve(midi, pcs, style="natural", strength=None, tuning_cents=0.0):
    """Returns (target MIDI per frame — NaN where unvoiced/untouched —, info). The note grid
    is shifted by tuning_cents (the beat's tuning in the product; 0 = A440)."""
    sh = tuning_cents / 100.0
    midi = midi - sh
    n = len(midi)
    target = np.full(n, np.nan)
    notes = segment_notes(midi)
    dev = intonation(notes, pcs)
    D = float(np.median(np.abs(dev))) if len(dev) else 0.0
    if style == "hard":
        from scipy.ndimage import median_filter
        v = ~np.isnan(midi)
        sm = midi.copy()
        sm[v] = median_filter(np.nan_to_num(midi), 3)[v]
        cur = None
        for i in range(n):
            if not v[i]:
                cur = None
                continue
            s = float(snap(np.array([sm[i]]), pcs)[0])
            # hysteresis: keep the held note until the voice is 15 ct past the midpoint
            if cur is not None and s != cur and abs(sm[i] - cur) < abs(s - cur) / 2 + 0.15:
                s = cur
            cur = s
            target[i] = s
        return target + sh, {"median_dev_cents": D, "strength": 1.0, "notes": len(notes)}
    # natural: adaptive strength from how far off the take is
    s = strength if strength is not None else float(np.clip((D - 8.0) / 25.0, 0.0, 1.0))
    offset = np.full(n, np.nan)
    allp = list(range(12))
    for nt in notes:
        t_scale = float(snap(np.array([nt.centre]), pcs)[0])
        # a clearly chromatic note (far from every scale note) is kept as sung: snap chromatically
        t = t_scale if abs(t_scale - nt.centre) <= 0.6 else float(snap(np.array([nt.centre]), allp)[0])
        offset[nt.a:nt.b] = (t - nt.centre) * s
    # 40 ms ramps at note edges (offset 0 outside notes)
    o = np.nan_to_num(offset)
    k = max(1, int(0.04 / HOP_S))
    w = np.hanning(2 * k + 1)
    w /= w.sum()
    o = np.convolve(np.pad(o, k, mode="edge"), w, mode="valid")
    v = ~np.isnan(midi)
    target[v] = midi[v] + o[v]
    return target + sh, {"median_dev_cents": D, "strength": s, "notes": len(notes)}


# ---------------------------------------------------------------- TD-PSOLA

def _marks(x, sr, f0_s):
    """Analysis pitch marks: in voiced parts one per period at waveform peaks; in unvoiced
    parts every 5 ms. Returns (marks, voiced flag per mark)."""
    from scipy.signal import butter, sosfiltfilt
    sos = butter(2, 900, fs=sr, output="sos")
    xl = sosfiltfilt(sos, x)
    marks, voiced = [], []
    t, n = 0, len(x)
    step_u = int(0.005 * sr)
    while t < n:
        f = f0_s[t]
        if f > 0:
            P = sr / f
            if marks and voiced[-1]:
                lo, hi = int(marks[-1] + 0.8 * P), int(marks[-1] + 1.2 * P)
            else:
                lo, hi = t, int(t + P)
            lo, hi = max(lo, 0), min(hi, n)
            if hi <= lo:
                break
            m = lo + int(np.argmax(xl[lo:hi]))
            marks.append(m)
            voiced.append(True)
            t = m + 1
        else:
            marks.append(t)
            voiced.append(False)
            t += step_u
    return np.array(marks), np.array(voiced)


def psola(x, sr, f0, ratio):
    """Time-preserving TD-PSOLA. f0 (Hz, 0 = unvoiced) and ratio (pitch factor) per 10 ms frame."""
    n = len(x)
    tf = np.arange(len(f0)) * HOP_S * sr
    ts = np.arange(n)
    f0_s = np.interp(ts, tf, f0)
    vmask = np.interp(ts, tf, (f0 > 0).astype(float)) > 0.5
    f0_s = np.where(vmask, f0_s, 0.0)
    r_s = np.interp(ts, tf, np.nan_to_num(ratio, nan=1.0))
    marks, voiced = _marks(x, sr, f0_s)
    if len(marks) < 3:
        return x.copy()
    periods = np.diff(marks, append=marks[-1] + (marks[-1] - marks[-2]))
    periods = np.maximum(periods, 16)
    y = np.zeros(n + 4096)
    wsum = np.zeros(n + 4096)
    ts_ = float(marks[0])
    while ts_ < n:
        k = int(np.clip(np.searchsorted(marks, ts_), 0, len(marks) - 1))
        if k > 0 and abs(marks[k - 1] - ts_) < abs(marks[k] - ts_):
            k -= 1
        P = int(periods[k])
        r = r_s[int(ts_)] if voiced[k] else 1.0
        a = marks[k]
        lo, hi = a - P, a + P
        seg = np.zeros(2 * P)
        s0, s1 = max(lo, 0), min(hi, n)
        seg[s0 - lo:s1 - lo] = x[s0:s1]
        w = np.hanning(2 * P)
        o = int(round(ts_)) - P
        if o >= 0:
            y[o:o + 2 * P] += seg * w
            wsum[o:o + 2 * P] += w
        ts_ += P / max(r, 0.25)
    y = y[:n]
    wsum = wsum[:n]
    return np.where(wsum > 1e-3, y / np.maximum(wsum, 1e-3), 0.0).astype(np.float32)


def autotune(y, sr, key_root=None, minor=False, style="natural", strength=None, f0=None, tuning_cents=0.0):
    """Full chain. Returns (corrected audio, info dict with f0 in/target)."""
    if f0 is None:
        f0 = track_f0(y, sr)
    midi = hz2midi(f0)
    pcs = scale_pcs(key_root, minor)
    if tuning_cents == "self":
        tuning_cents = estimate_tuning(midi)
    # scale gate: when the take's notes do not sit on the beat's scale (a wrong key estimate,
    # or a melody that borrows notes), snapping to that scale would rewrite the melody, so
    # fall back to chromatic snapping. Share = note time whose nearest semitone is on scale.
    v = midi[~np.isnan(midi)] - tuning_cents / 100.0
    on_scale = float(np.isin(np.mod(np.round(v), 12).astype(int), pcs).mean()) if len(v) else 1.0
    gated = key_root is not None and on_scale < SCALE_GATE
    target, info = correction_curve(midi, list(range(12)) if gated else pcs, style, strength, tuning_cents)
    info.update(tuning_cents=tuning_cents, on_scale=on_scale, scale_gated=gated)
    ratio = np.where(np.isnan(target) | np.isnan(midi), 1.0, 2 ** ((target - midi) / 12))
    out = psola(y.astype(np.float64), sr, f0, ratio)
    info.update(f0=f0, target=target)
    return out, info
