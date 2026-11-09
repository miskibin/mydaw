"""Smoke test: PSOLA shifts a real VocalSet take by +40 cents and back; pYIN measures it."""
import sys
import time

import librosa
import numpy as np

sys.path.insert(0, ".")
from av.common import load  # noqa: E402
from av.tune import autotune, hz2midi, psola, track_f0  # noqa: E402

sr = 44100
y = load(r"D:\datasets\vocalset_annotated\audio\FULL\female1\scales\belt\f1_scales_belt_a.wav", sr)[: sr * 12]
t = time.time()
f0 = track_f0(y, sr)
print("crepe s", time.time() - t)


def meas(z):
    f, v, _ = librosa.pyin(z, fmin=80, fmax=1000, sr=sr, frame_length=2048, hop_length=441)
    return hz2midi(np.where(v, f, 0))


m0 = meas(y)
t = time.time()
up = psola(y, sr, f0, np.full(len(f0), 2 ** (40 / 1200)))
print("psola s", time.time() - t)
m1 = meas(up)
n = min(len(m0), len(m1))
d = 100 * (m1[:n] - m0[:n])
print("shift +40 measured median", np.nanmedian(d), "iqr", np.nanpercentile(d, [25, 75]))
idn = psola(y, sr, f0, np.ones(len(f0)))
print("identity rms err rel", np.sqrt(np.mean((idn - y) ** 2)) / np.sqrt(np.mean(y ** 2)))
out, info = autotune(up, sr, None, False, "natural")
print({k: v for k, v in info.items() if k not in ("f0", "target")})
m2 = meas(out)
d2 = 100 * (m2[:n] - m0[:n])
print("after natural median", np.nanmedian(d2), np.nanpercentile(d2, [25, 75]))
