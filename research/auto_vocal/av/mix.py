"""Automatic vocal mix chain: gate -> HPF -> matching EQ -> de-esser -> compressor ->
(saturation) -> level vs beat -> tempo-synced delay + reverb sends.

Every stage maps to an effect the app already has (Eq, Comp/Fet, DeEss, Sat, Delay, Reverb,
in crates/engine/src/{dsp,vocal}.rs). Parameters are chosen from measurements of the vocal
and the beat against targets learned from professional mixes (layer3_eval.py).
"""
from __future__ import annotations

from dataclasses import dataclass, field

import numpy as np
from scipy.signal import butter, firwin2, oaconvolve, sosfilt

from . import mixfeat


@dataclass
class Targets:
    shape: np.ndarray            # target vocal tonal balance (24 bands, mean-removed dB)
    spread: float = 9.0          # target dynamic spread, dB
    sib: float = -12.0           # target sibilance p95, dB
    delta_lufs: float = -1.0     # vocal LUFS minus beat LUFS
    max_eq_db: float = 9.0
    eq_strength: float = 1.0     # fraction of the measured shape difference that is corrected
    eq_cut_only: bool = False    # only cut (remove excess), never boost
    rev_send_db: float | None = None   # reverb send (wet/dry energy, dB); None = off
    rev_t60: float = 1.0
    delay_send_db: float | None = None  # tempo-synced delay send; None = off
    comp_mode: str = "target"    # "target": ratio/threshold chosen to hit `spread`; "fixed3": 3:1 at the median level


@dataclass
class Settings:
    """What the chain decided; reported next to every sample."""
    gate_thr_db: float = 0.0
    eq_gains_db: list = field(default_factory=list)
    deess_thr_db: float = 0.0
    comp_ratio: float = 1.0
    comp_thr_db: float = 0.0
    gain_db: float = 0.0
    bpm: float = 0.0
    delay_s: float = 0.0
    delay_send_db: float | None = None
    reverb_t60: float = 0.0
    reverb_send_db: float | None = None


def _env_db(x, sr, ms=1.0):
    n = max(1, int(sr * ms / 1000))
    m = len(x) // n
    return 10 * np.log10((x[: m * n].reshape(m, n) ** 2).mean(1) + 1e-12), n


def _smooth_gain(g_db, n_per_block, sr, attack_ms, release_ms):
    """One-pole attack/release smoothing of a per-block gain (dB, <= 0), block = n samples."""
    bt = n_per_block / sr
    a_at = np.exp(-bt / (attack_ms / 1000))
    a_re = np.exp(-bt / (release_ms / 1000))
    out = np.empty_like(g_db)
    s = 0.0
    for i, g in enumerate(g_db):
        a = a_at if g < s else a_re
        s = a * s + (1 - a) * g
        out[i] = s
    return out


def _apply_gain(x, g_db, n):
    g = np.repeat(10 ** (g_db / 20), n)
    g = np.concatenate([g, np.full(len(x) - len(g), g[-1] if len(g) else 1.0)])
    return x * g


def gate(x, sr, s: Settings, range_db=18.0):
    e, n = _env_db(x, sr, 5.0)
    floor = np.percentile(e, 10)
    top = np.percentile(e, 95)
    thr = min(floor + 10.0, top - 30.0)
    s.gate_thr_db = float(thr)
    g = np.where(e < thr, np.maximum(-(thr - e), -range_db), 0.0)  # 1:2 downward expander, capped
    return _apply_gain(x, _smooth_gain(g, n, sr, 2.0, 120.0), n)


def hpf(x, sr, f=80.0):
    return sosfilt(butter(4, f, "highpass", fs=sr, output="sos"), x)


def match_eq(x, sr, target_shape, s: Settings, max_db=9.0, strength=1.0, cut_only=False):
    fx_ = mixfeat.features(x, sr)
    cur = fx_["shape"]
    empty = fx_["band_db"] < fx_["band_db"].max() - 45  # no content there (e.g. a 16 kHz source)
    g = np.clip(strength * (target_shape - cur), -max_db, max_db)
    if cut_only:
        g = np.minimum(g, 0.0)
    g = np.convolve(np.pad(g, 1, mode="edge"), [0.25, 0.5, 0.25], "valid")
    g[mixfeat.CENTRES < 100] = np.minimum(g[mixfeat.CENTRES < 100], 0)  # never boost rumble
    # above 10 kHz the targets come from AAC-coded MUSDB stems (encoder low-pass), so the
    # pro "air" level is biased low: leave that region to the de-esser
    g[mixfeat.CENTRES > 10000] = 0.0
    g[empty] = np.minimum(g[empty], 0.0)  # never boost a band the take does not contain
    if not cut_only:
        g -= np.average(g, weights=np.ones_like(g))  # tonal change only; level is set later
    s.eq_gains_db = [round(float(v), 1) for v in g]
    f = np.concatenate([[0], mixfeat.CENTRES, [sr / 2]])
    gg = np.concatenate([[g[0]], g, [g[-1]]])
    h = firwin2(2047, f / (sr / 2), 10 ** (gg / 20))
    y = oaconvolve(x, h)[1023:1023 + len(x)]
    return y


def deess(x, sr, target_sib, s: Settings):
    """Split-band: the 5 kHz+ band is turned down in frames whose HF/LF ratio exceeds the
    level that puts the take's p95 sibilance at target_sib."""
    hi = sosfilt(butter(4, 5000, "highpass", fs=sr, output="sos"), x)
    lo = x - hi
    n = int(sr * 0.005)
    m = len(x) // n
    eh = 10 * np.log10((hi[: m * n].reshape(m, n) ** 2).mean(1) + 1e-12)
    el = 10 * np.log10((lo[: m * n].reshape(m, n) ** 2).mean(1) + 1e-12)
    r = eh - el
    loud = el > np.percentile(el, 95) - 40
    p95 = np.percentile(r[loud], 95) if loud.any() else 0
    over = p95 - target_sib
    thr = p95 - max(over, 0) - 3.0
    s.deess_thr_db = float(thr)
    g = np.where(loud & (r > thr), -(r - thr), 0.0)
    g = np.maximum(g, -12.0)
    g = _smooth_gain(g, n, sr, 1.0, 40.0)
    return lo + _apply_gain(hi, g, n)


def _spread_after(rms_db, thr, ratio, knee=6.0):
    over = rms_db - thr
    red = np.where(over > knee / 2, over * (1 - 1 / ratio),
                   np.where(over > -knee / 2, (1 - 1 / ratio) * (over + knee / 2) ** 2 / (2 * knee), 0))
    out = rms_db - red
    on = rms_db > np.percentile(rms_db, 95) - 35
    return np.percentile(out[on], 95) - np.percentile(out[on], 10)


def compress(x, sr, target_spread, s: Settings, mode="target"):
    w, h = int(0.4 * sr), int(0.1 * sr)
    idx = np.arange(0, len(x) - w, h)
    rms = 20 * np.log10(np.sqrt(np.stack([x[i:i + w] ** 2 for i in idx]).mean(1)) + 1e-9)
    on = rms > np.percentile(rms, 95) - 35
    if mode == "fixed3":
        ratio, thr = 3.0, float(np.percentile(rms[on], 50))
    else:
        best = (1e9, 1.0, 0.0)
        for q in (30, 40, 50, 60, 70):
            thr = np.percentile(rms[on], q)
            for ratio in (1.0, 1.5, 2.0, 3.0, 4.0, 6.0, 8.0):
                d = abs(_spread_after(rms, thr, ratio) - target_spread)
                if d < best[0] - 0.25:
                    best = (d, ratio, thr)
        _, ratio, thr = best
    s.comp_ratio, s.comp_thr_db = float(ratio), float(thr)
    if ratio <= 1.0:
        return x
    e, n = _env_db(x, sr, 1.0)
    over = e - thr
    knee = 6.0
    red = np.where(over > knee / 2, over * (1 - 1 / ratio),
                   np.where(over > -knee / 2, (1 - 1 / ratio) * (over + knee / 2) ** 2 / (2 * knee), 0))
    g = _smooth_gain(-red, n, sr, 5.0, 100.0)
    return _apply_gain(x, g, n)


def saturate(x, drive_db=0.0, mix=0.0):
    if mix <= 0:
        return x
    k = 10 ** (drive_db / 20)
    pk = np.max(np.abs(x)) + 1e-9
    y = np.tanh(k * x / pk) * pk / np.tanh(k)
    return (1 - mix) * x + mix * y


def plate_ir(sr, t60=1.0, predelay=0.03, seed=0):
    """Short plate/room-like tail: dense decaying noise, wet band-limited to 300 Hz - 7 kHz,
    unit energy (so the send level in dB is the wet-to-dry energy ratio for a steady tone)."""
    rng = np.random.default_rng(seed)
    n = int(t60 * 1.2 * sr)
    tt = np.arange(n) / sr
    ir = rng.standard_normal(n) * np.exp(-6.91 * tt / t60)
    ir = sosfilt(butter(2, 300, "highpass", fs=sr, output="sos"), ir)
    ir = sosfilt(butter(2, 7000, "lowpass", fs=sr, output="sos"), ir)
    ir = np.concatenate([np.zeros(int(predelay * sr)), ir])
    return ir / np.sqrt((ir ** 2).sum())


def fx_sends(x, sr, bpm, s: Settings, rev_send_db=None, t60=1.0, delay_send_db=None, delay_beats=0.5):
    """Reverb (short plate, pre-delay 30 ms) and an optional tempo-synced, filtered delay.
    A send of None means off. Levels are chosen from data in fx_calib.py, not by taste."""
    s.bpm = float(bpm or 0.0)
    y = x.copy()
    if rev_send_db is not None:
        s.reverb_t60, s.reverb_send_db = float(t60), float(rev_send_db)
        y = y + 10 ** (rev_send_db / 20) * oaconvolve(x, plate_ir(sr, t60))[: len(x)]
    if delay_send_db is not None and bpm:
        d = 60.0 / bpm * delay_beats
        s.delay_s, s.delay_send_db = float(d), float(delay_send_db)
        wet = sosfilt(butter(2, 300, "highpass", fs=sr, output="sos"), sosfilt(butter(2, 5000, "lowpass", fs=sr, output="sos"), x))
        nd = int(d * sr)
        dl = np.zeros_like(x)
        for k in range(1, 4):
            if nd * k >= len(x):
                break
            dl[nd * k:] += 0.35 ** (k - 1) * wet[: len(x) - nd * k]
        y = y + 10 ** (delay_send_db / 20) * dl
    return y

def auto_mix(v, sr, beat, t: Targets, bpm=None, style="natural", fx=False, stages=("gate", "hpf", "eq", "deess", "comp", "level")):
    s = Settings()
    x = np.asarray(v, np.float64)
    if "gate" in stages:
        x = gate(x, sr, s)
    if "hpf" in stages:
        x = hpf(x, sr)
    if "eq" in stages:
        x = match_eq(x, sr, t.shape, s, t.max_eq_db, t.eq_strength, t.eq_cut_only)
    if "deess" in stages:
        x = deess(x, sr, t.sib, s)
    if "comp" in stages:
        x = compress(x, sr, t.spread, s, t.comp_mode)
    if style == "hard":
        x = saturate(x, 6.0, 0.25)
    if fx:
        x = fx_sends(x, sr, bpm, s, t.rev_send_db, t.rev_t60, t.delay_send_db)
    if "level" in stages and beat is not None:
        import pyloudnorm as pyln
        m = pyln.Meter(sr)
        lv = m.integrated_loudness(x)
        lb = m.integrated_loudness(np.asarray(beat, np.float64))
        g = (lb + t.delta_lufs) - lv
        s.gain_db = float(g)
        x = x * 10 ** (g / 20)
    return x.astype(np.float32), s
