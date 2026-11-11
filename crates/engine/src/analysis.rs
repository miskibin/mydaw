//! Spectra and scopes for open effect windows. Runs on the housekeeping thread,
//! only for watched effects, ~30 times a second.

use std::sync::Arc;

use realfft::{RealFftPlanner, RealToComplex};

use crate::model::Id;
use crate::rt::Shared;

pub const BANDS: usize = 128;
pub const SCOPE: usize = 256;
const FFT: usize = 2048;

pub struct FxAnalysis {
    pub id: Id,
    pub in_peak: f32,
    pub out_peak: f32,
    pub gr: f32,
    pub meters: [f32; crate::processor::METERS],
    pub spec_in: [f32; BANDS],
    pub spec_out: [f32; BANDS],
    pub scope_in: [f32; SCOPE],
    pub scope_out: [f32; SCOPE],
}

struct History {
    buf: Vec<f32>,
    w: usize,
    peak: f32,
    /// smoothed spectrum (dB)
    smooth: [f32; BANDS],
}

impl History {
    fn new() -> Self {
        Self {
            buf: vec![0.0; FFT],
            w: 0,
            peak: 0.0,
            smooth: [-120.0; BANDS],
        }
    }
    fn feed(&mut self, c: &mut rtrb::Consumer<f32>) {
        let n = c.slots();
        if n == 0 {
            return;
        }
        if let Ok(chunk) = c.read_chunk(n) {
            let (a, b) = chunk.as_slices();
            for &x in a.iter().chain(b.iter()) {
                self.buf[self.w] = x;
                self.w = (self.w + 1) % FFT;
                self.peak = self.peak.max(x.abs());
            }
            chunk.commit_all();
        }
    }
    fn last(&self, out: &mut [f32]) {
        let n = out.len();
        for (i, o) in out.iter_mut().enumerate() {
            *o = self.buf[(self.w + FFT - n + i) % FFT];
        }
    }
}

pub struct Analyzer {
    inputs: Vec<rtrb::Consumer<f32>>,
    outputs: Vec<rtrb::Consumer<f32>>,
    hin: Vec<History>,
    hout: Vec<History>,
    ids: Vec<Id>,
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    scratch: Vec<f32>,
    spec: Vec<realfft::num_complex::Complex<f32>>,
    /// FFT bin range per band.
    bands: Vec<(usize, usize)>,
}

impl Analyzer {
    pub fn new(
        inputs: Vec<rtrb::Consumer<f32>>,
        outputs: Vec<rtrb::Consumer<f32>>,
        sr: f32,
    ) -> Self {
        let n = inputs.len();
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FFT);
        let window = (0..FFT)
            .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / FFT as f32).cos())
            .collect();
        let spec = fft.make_output_vec();
        let bin_hz = sr / FFT as f32;
        let bands = (0..BANDS)
            .map(|k| {
                let f0 = 20.0 * 1000f32.powf(k as f32 / BANDS as f32);
                let f1 = 20.0 * 1000f32.powf((k + 1) as f32 / BANDS as f32);
                let b0 = ((f0 / bin_hz).floor() as usize).clamp(1, FFT / 2);
                let b1 = ((f1 / bin_hz).ceil() as usize).clamp(b0, FFT / 2);
                (b0, b1)
            })
            .collect();
        Self {
            inputs,
            outputs,
            hin: (0..n).map(|_| History::new()).collect(),
            hout: (0..n).map(|_| History::new()).collect(),
            ids: vec![],
            fft,
            window,
            scratch: vec![0.0; FFT],
            spec,
            bands,
        }
    }

    fn spectrum(&mut self, h: &History, smooth: &mut [f32; BANDS]) -> [f32; BANDS] {
        for i in 0..FFT {
            self.scratch[i] = h.buf[(h.w + i) % FFT] * self.window[i];
        }
        let _ = self.fft.process(&mut self.scratch, &mut self.spec);
        let norm = 2.0 / (FFT as f32 * 0.5);
        let mut out = [0f32; BANDS];
        for (k, &(b0, b1)) in self.bands.iter().enumerate() {
            let mut m = 0f32;
            for b in b0..=b1.min(self.spec.len() - 1) {
                m = m.max(self.spec[b].norm());
            }
            let db = 20.0 * (m * norm).max(1e-7).log10();
            // fast attack, slow-ish fall, like a real analyser
            smooth[k] = if db > smooth[k] {
                db
            } else {
                smooth[k] * 0.7 + db * 0.3
            };
            out[k] = smooth[k];
        }
        out
    }

    pub fn analyze(&mut self, watch: &[Id], shared: &Shared) -> Vec<FxAnalysis> {
        if self.ids != watch {
            // slots were reassigned: forget history so windows never show another effect's audio
            for h in self.hin.iter_mut().chain(self.hout.iter_mut()) {
                *h = History::new();
            }
            self.ids = watch.to_vec();
        }
        let mut res = vec![];
        for (slot, &id) in watch.iter().enumerate().take(self.inputs.len()) {
            self.hin[slot].feed(&mut self.inputs[slot]);
            self.hout[slot].feed(&mut self.outputs[slot]);
            let hin = std::mem::replace(&mut self.hin[slot], History::new());
            let hout = std::mem::replace(&mut self.hout[slot], History::new());
            let mut si = hin.smooth;
            let mut so = hout.smooth;
            let spec_in = self.spectrum(&hin, &mut si);
            let spec_out = self.spectrum(&hout, &mut so);
            let mut scope_in = [0f32; SCOPE];
            let mut scope_out = [0f32; SCOPE];
            hin.last(&mut scope_in);
            hout.last(&mut scope_out);
            res.push(FxAnalysis {
                id,
                in_peak: hin.peak,
                out_peak: hout.peak,
                gr: shared.gr(slot),
                meters: shared.fx_meters(slot),
                spec_in,
                spec_out,
                scope_in,
                scope_out,
            });
            self.hin[slot] = History {
                peak: 0.0,
                smooth: si,
                ..hin
            };
            self.hout[slot] = History {
                peak: 0.0,
                smooth: so,
                ..hout
            };
        }
        res
    }
}
