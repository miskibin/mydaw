// C ABI over Signalsmith Stretch for offline, whole-buffer time-stretching.
#include <signalsmith-stretch.h>

#include <cstddef>
#include <new>

extern "C" {

// Stretches `channels` planar input buffers of `in_len` frames into planar output
// buffers of `out_len` frames, keeping pitch, and transposes them by `semitones`
// (0 = none). Returns 0 on success, 1 when the output is too short for the algorithm
// (the caller pads), 2 on allocation failure.
int wave_stretch_exact(float sample_rate, int channels, const float *const *input, size_t in_len, float *const *output, size_t out_len, float semitones) {
    try {
        signalsmith::stretch::SignalsmithStretch<float> s;
        s.presetDefault(channels, sample_rate);
        if (semitones != 0.0f) s.setTransposeSemitones(semitones);
        // `exact` takes channel-indexable buffers; it reads input only
        float **in = const_cast<float **>(input);
        float *const *out = output;
        return s.exact(in, (int)in_len, out, (int)out_len) ? 0 : 1;
    } catch (const std::bad_alloc &) {
        return 2;
    } catch (...) {
        return 2;
    }
}

// Output latency of the default preset, so the caller knows the minimum output length.
size_t wave_stretch_min_output(float sample_rate, int channels) {
    signalsmith::stretch::SignalsmithStretch<float> s;
    s.presetDefault(channels, sample_rate);
    return (size_t)s.outputLatency() * 2;
}
}
