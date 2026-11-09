"""Automatic vocal mix chain: gate -> HPF -> matching EQ -> dynamics -> (saturation) ->
reverb send -> level vs beat. Dynamics = rider -> fast peak stage -> slow stage -> de-esser
("serial", the product default) or a single compressor after the de-esser (older modes).

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
    rider_strength: float = 0.7  # serial mode: word-vs-phrase evening (s)
    rider_mu: float = 0.2        # serial mode: phrase-vs-take nudge (mu)
    fet_peak_gr: float = 4.0     # serial mode: dB of reduction on the loudest 1 % of blocks
    opto_avg_gr: float = 3.0     # serial mode: mean dB of reduction over sung blocks
    # "serial": rider + FET + opto; "target": one compressor aimed at `spread`;
    # "fixed3": one 3:1 compressor at the median level
    comp_mode: str = "target"


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
    rider_gain_db: list = field(default_factory=list)
    rider_curve: tuple = ()
    fet_thr_db: float = 0.0
    fet_gr: dict = field(default_factory=dict)
    opto_thr_db: float = 0.0
    opto_gr: dict = field(default_factory=dict)


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

def k_weight(x, sr):
    """ITU-R BS.1770 K-weighting (pre-filter + RLB high-pass), as in pyloudnorm."""
    import pyloudnorm as pyln
    m = pyln.Meter(sr)
    y = np.asarray(x, np.float64)
    for f in m._filters.values():
        y = f.apply_filter(y)
    return y


RIDER_STYLE = {  # time constants (s) per style: rap words are shorter than sung notes
    "natural": {"fast": 0.21, "phrase": 2.0},
    "hard": {"fast": 0.12, "phrase": 1.0},
}


def rider_curve(x, sr, strength=0.7, mu=0.2, style="natural", deadband=0.6, max_boost=5.0, max_cut=6.0,
                cut_ms=80.0, boost_ms=300.0):
    """Two-time-scale vocal rider (offline). Returns (gain dB per 10 ms frame, frame times s,
    confidence 0..1, activity mask).

    On K-weighted power every 10 ms: L_fast (~word, 210 ms sung / 120 ms rap) and L_phrase
    (2 s / 1 s, averaged over confident vocal frames only). Desired gain =
    strength * deadband(L_phrase - L_fast) + mu * (T - L_phrase), T = median L_fast of the
    take: words are evened toward their phrase, phrases only nudged toward the take, so
    verse/chorus dynamics survive. Confidence = smoothstep(SNR 6->14 dB) x activity: low
    confidence can block a BOOST but never a cut; idle frames glide back to 0 dB. Range
    +5 / -6 dB. Asymmetric slew (toward cut 80 ms, toward boost 300 ms), run forwards and
    backwards and averaged: an offline look-ahead of the same order.

    This is deliberately a local-vs-phrase error with confidence-limited positive gain, not
    the "activity detector -> long-term level -> target gain -> direction-dependent ramp"
    AGC structure claimed in US7995775B2.
    """
    from scipy.ndimage import uniform_filter1d
    tc = RIDER_STYLE.get(style, RIDER_STYLE["natural"])
    hop = int(0.01 * sr)
    pk = uniform_filter1d(k_weight(x, sr) ** 2, hop)[hop // 2::hop] + 1e-14
    nf = len(pk)
    p_fast = uniform_filter1d(pk, max(1, int(tc["fast"] / 0.01)))
    l_fast = 10 * np.log10(p_fast + 1e-14)
    top = np.percentile(l_fast, 95)
    floor = min(np.percentile(l_fast, 10), top - 35)
    snr = l_fast - floor
    u = np.clip((snr - 6.0) / 8.0, 0.0, 1.0)
    active = l_fast > top - 30
    conf = u * u * (3 - 2 * u) * active
    w = max(1, int(tc["phrase"] / 0.01))
    num = uniform_filter1d(pk * conf, w)
    den = uniform_filter1d(conf, w)
    l_phrase = 10 * np.log10(np.where(den > 1e-3, num / np.maximum(den, 1e-9), p_fast) + 1e-14)
    confident = conf > 0.5
    if confident.sum() < 20:
        return np.zeros(nf), np.arange(nf) * 0.01 + 0.005, conf, active
    T = float(np.median(l_fast[confident]))
    e = l_phrase - l_fast
    db = np.sign(e) * np.maximum(np.abs(e) - deadband, 0.0)
    g = strength * db + mu * (T - l_phrase)
    g = np.where(g > 0, g * conf, g)          # low confidence blocks boost, not cut
    g = np.where(active, g, 0.0)              # idle: back to 0 dB
    g = np.clip(g, -max_cut, max_boost)

    def slew(seq):
        a_c, a_b = np.exp(-0.01 / (cut_ms / 1000)), np.exp(-0.01 / (boost_ms / 1000))
        out, st = np.empty_like(seq), 0.0
        for k, v in enumerate(seq):
            a = a_c if v < st else a_b
            st = a * st + (1 - a) * v
            out[k] = st
        return out
    gs = 0.5 * (slew(g) + slew(g[::-1])[::-1])
    return gs, np.arange(nf) * 0.01 + 0.005, conf, active


def rider(x, sr, s: Settings, strength=0.7, mu=0.2, style="natural"):
    g, t, conf, active = rider_curve(x, sr, strength, mu, style)
    s.rider_gain_db = [round(float(np.percentile(g[conf > 0.5], q)), 1) for q in (5, 50, 95)] if (conf > 0.5).any() else []
    s.rider_curve = (g, t, conf, active)
    gs = np.interp(np.arange(len(x)) / sr, t, g)
    return x * 10 ** (gs / 20)

def _static_gr(e, thr, ratio, knee=6.0):
    over = e - thr
    return np.where(over > knee / 2, over * (1 - 1 / ratio),
                    np.where(over > -knee / 2, (1 - 1 / ratio) * (over + knee / 2) ** 2 / (2 * knee), 0.0))


def fet(x, sr, s: Settings, ratio=4.0, peak_gr=4.0, thr=None):
    """Fast peak stage (FET-like): 4:1, 1 ms blocks, attack 1 ms, release 60 ms; threshold
    set so the loudest 1 % of sung blocks get ~peak_gr dB of reduction."""
    n = int(0.001 * sr)
    m = len(x) // n
    e = 20 * np.log10(np.abs(x[: m * n]).reshape(m, n).max(1) + 1e-9)
    sung = e > np.percentile(e, 95) - 30
    if thr is None:  # frozen thresholds let the workload test compare with/without rider
        thr = np.percentile(e[sung], 99) - peak_gr / (1 - 1 / ratio)
    g = _smooth_gain(-_static_gr(e, thr, ratio, 3.0), n, sr, 1.0, 60.0)
    s.fet_thr_db = float(thr)
    s.fet_gr = {"p99_db": round(float(np.percentile(-g[sung], 99)), 1), "max_db": round(float(-g.min()), 1),
                "mean_sung_db": round(float(np.mean(-g[sung])), 2), "p95_sung_db": round(float(np.percentile(-g[sung], 95)), 2)}
    return _apply_gain(x, g, n)


def opto(x, sr, s: Settings, ratio=2.5, avg_gr=3.0, thr=None):
    """Slow stage (opto-like): 2.5:1, soft knee, 10 ms RMS, attack 10 ms, release 250 ms;
    threshold searched so the mean reduction over sung blocks is ~avg_gr dB."""
    n = int(0.01 * sr)
    m = len(x) // n
    e = 10 * np.log10((x[: m * n].reshape(m, n) ** 2).mean(1) + 1e-14)
    sung = e > np.percentile(e, 95) - 25
    if thr is None:
        lo, hi = np.percentile(e[sung], 1) - 20, np.percentile(e[sung], 99)
        for _ in range(30):
            thr = (lo + hi) / 2
            if np.mean(_static_gr(e[sung], thr, ratio)) > avg_gr:
                lo = thr
            else:
                hi = thr
    g = _smooth_gain(-_static_gr(e, thr, ratio), n, sr, 10.0, 250.0)
    s.opto_thr_db = float(thr)
    s.opto_gr = {"mean_sung_db": round(float(np.mean(-g[sung])), 2), "p95_db": round(float(np.percentile(-g[sung], 95)), 1)}
    return _apply_gain(x, g, n)


def auto_mix(v, sr, beat, t: Targets, bpm=None, style="natural", fx=False, stages=("gate", "hpf", "eq", "deess", "comp", "level")):
    s = Settings()
    x = np.asarray(v, np.float64)
    if "gate" in stages:
        x = gate(x, sr, s)
    if "hpf" in stages:
        x = hpf(x, sr)
    if "eq" in stages:
        x = match_eq(x, sr, t.shape, s, t.max_eq_db, t.eq_strength, t.eq_cut_only)
    if t.comp_mode == "serial":
        # rider -> fast peak stage -> slow stage, then the de-esser (owner 11-09)
        if "comp" in stages:
            x = rider(x, sr, s, t.rider_strength, t.rider_mu, "hard" if style == "hard" else "natural")
            x = fet(x, sr, s, peak_gr=t.fet_peak_gr)
            x = opto(x, sr, s, avg_gr=t.opto_avg_gr)
            s.comp_ratio, s.comp_thr_db = 2.5, s.opto_thr_db
        if "deess" in stages:
            x = deess(x, sr, t.sib, s)
    else:
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
