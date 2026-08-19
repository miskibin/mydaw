//! Drain of the insert analysis hop. Non-RT: the FFT lives here.

use mydaw_ipc_bridge::{SpectrumAnalyzer, SpectrumConsumer, SpectrumFrame};
use parking_lot::Mutex;

/// Latest analysed hop, if the UI has armed the tap.
pub struct SpectrumPump {
    rx: Mutex<Option<SpectrumConsumer>>,
    analyzer: Mutex<SpectrumAnalyzer>,
    latest: Mutex<Option<SpectrumFrame>>,
}

impl SpectrumPump {
    pub fn new(consumer: Option<SpectrumConsumer>) -> Self {
        Self {
            rx: Mutex::new(consumer),
            analyzer: Mutex::new(SpectrumAnalyzer::new()),
            latest: Mutex::new(None),
        }
    }

    /// Drain the hop queue and analyse the newest frame.
    pub fn poll(&self) -> Option<SpectrumFrame> {
        let mut rx = self.rx.lock();
        let queue = rx.as_mut()?;
        let mut last = None;
        while let Ok(hop) = queue.pop() {
            last = Some(hop);
        }
        drop(rx);
        if let Some(hop) = last {
            let frame = self.analyzer.lock().analyse(&hop);
            *self.latest.lock() = Some(frame.clone());
            Some(frame)
        } else {
            self.latest.lock().clone()
        }
    }

    pub fn clear(&self) {
        *self.latest.lock() = None;
        if let Some(queue) = self.rx.lock().as_mut() {
            while queue.pop().is_ok() {}
        }
    }
}
