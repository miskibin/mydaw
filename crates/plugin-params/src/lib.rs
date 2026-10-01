use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

pub const MAX_PARAMETERS: usize = 16384;
pub const MAX_CHANGES_PER_BLOCK: usize = 256;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ParameterInfo {
    pub id: u32,
    pub name: String,
    pub module: String,
    pub value: f64,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub display: String,
    /// VST3 uses normalized 0..1; CLAP uses plugin-defined plain values.
    pub value_domain: String,
    pub read_only: bool,
    pub stepped: bool,
    pub automatable: bool,
    pub pending: bool,
}
impl ParameterInfo {
    pub fn check(&self, value: f64) -> Result<(), String> {
        if self.read_only {
            return Err(format!("{} is read-only", self.name));
        }
        if !value.is_finite() || value < self.min || value > self.max {
            return Err(format!(
                "{} must be a finite {} value in {}..={}",
                self.name, self.value_domain, self.min, self.max
            ));
        }
        if self.stepped && self.value_domain == "plain" && value.fract() != 0.0 {
            return Err(format!("{} requires an integer", self.name));
        }
        Ok(())
    }
}

#[derive(Debug)]
struct Slot {
    id: AtomicU32,
    input: AtomicU64,
    observed: AtomicU64,
    desired: AtomicU64,
    applied: AtomicU64,
}
const EMPTY_ID: u32 = u32::MAX;
const SLOT_CAPACITY: usize = MAX_PARAMETERS * 2;
/// Allocated once on the owner thread. Producers may be GUI callbacks, CLI
/// setters or the audio processor. Neither publishing nor draining allocates
/// or takes a lock. Values coalesce within a block; automation is a separate
/// future event stream and must not be simulated with this mailbox.
#[derive(Debug)]
pub struct ParameterBank {
    registration: std::sync::Mutex<()>,
    slots: Vec<Slot>,
    pending_input: Vec<AtomicU64>,
    pending_output: Vec<AtomicU64>,
    changes: AtomicU64,
    registered: AtomicUsize,
    input_words: [AtomicU64; SLOT_CAPACITY / 4096],
    output_words: [AtomicU64; SLOT_CAPACITY / 4096],
}
impl ParameterBank {
    pub fn new(mut parameters: Vec<(u32, f64)>) -> Result<Self, String> {
        if parameters.len() > MAX_PARAMETERS {
            return Err(format!(
                "plugin exposes more than {MAX_PARAMETERS} parameters"
            ));
        }
        parameters.sort_unstable_by_key(|(id, _)| *id);
        if parameters.windows(2).any(|pair| pair[0].0 == pair[1].0)
            || parameters.iter().any(|(_, v)| !v.is_finite())
        {
            return Err("plugin exposed duplicate IDs or invalid parameter values".into());
        }
        let words = SLOT_CAPACITY / 64;
        let bank = Self {
            registration: std::sync::Mutex::new(()),
            slots: (0..SLOT_CAPACITY)
                .map(|_| Slot {
                    id: AtomicU32::new(EMPTY_ID),
                    input: AtomicU64::new(0),
                    observed: AtomicU64::new(0),
                    desired: AtomicU64::new(0),
                    applied: AtomicU64::new(0),
                })
                .collect(),
            pending_input: (0..words).map(|_| AtomicU64::new(0)).collect(),
            pending_output: (0..words).map(|_| AtomicU64::new(0)).collect(),
            changes: AtomicU64::new(0),
            registered: AtomicUsize::new(0),
            input_words: std::array::from_fn(|_| AtomicU64::new(0)),
            output_words: std::array::from_fn(|_| AtomicU64::new(0)),
        };
        for (id, value) in parameters {
            bank.register(id, value)?;
        }
        Ok(bank)
    }
    fn index(&self, id: u32) -> Option<usize> {
        if id == EMPTY_ID {
            return None;
        }
        let start = parameter_hash(id);
        for offset in 0..128 {
            let index = (start + offset) & (SLOT_CAPACITY - 1);
            match self.slots[index].id.load(Ordering::Acquire) {
                current if current == id => return Some(index),
                EMPTY_ID => return None,
                _ => {}
            }
        }
        None
    }
    /// Owner-thread only. The immutable-size bank accepts new IDs after a
    /// preset/rescan without swapping memory under an active audio processor.
    pub fn register(&self, id: u32, value: f64) -> Result<(), String> {
        let _guard = self
            .registration
            .lock()
            .map_err(|_| "parameter registration lock is poisoned")?;
        if self.contains(id) {
            return Ok(());
        }
        if id == EMPTY_ID || !value.is_finite() {
            return Err("invalid parameter ID/value".into());
        }
        if self.registered.load(Ordering::Acquire) >= MAX_PARAMETERS {
            return Err("parameter registration limit reached".into());
        }
        let start = parameter_hash(id);
        for offset in 0..128 {
            let slot = &self.slots[(start + offset) & (SLOT_CAPACITY - 1)];
            if slot.id.load(Ordering::Acquire) == EMPTY_ID {
                slot.input.store(value.to_bits(), Ordering::Relaxed);
                slot.observed.store(value.to_bits(), Ordering::Relaxed);
                slot.id.store(id, Ordering::Release);
                self.registered.fetch_add(1, Ordering::Relaxed);
                return Ok(());
            }
        }
        Err("parameter hash probe limit reached".into())
    }
    pub fn contains(&self, id: u32) -> bool {
        self.index(id).is_some()
    }
    pub fn value(&self, id: u32) -> Option<f64> {
        self.index(id)
            .map(|i| f64::from_bits(self.slots[i].observed.load(Ordering::Acquire)))
    }
    pub fn is_pending(&self, id: u32) -> bool {
        self.index(id).is_some_and(|i| {
            self.slots[i].desired.load(Ordering::Acquire)
                != self.slots[i].applied.load(Ordering::Acquire)
        })
    }
    pub fn acknowledge(&self, id: u32, value: f64) {
        if let Some(index) = self.index(id) {
            let slot = &self.slots[index];
            let desired = slot.desired.load(Ordering::Acquire);
            if slot.input.load(Ordering::Acquire) == value.to_bits() {
                slot.applied.store(desired, Ordering::Release);
            }
        }
    }
    pub fn publish(&self, id: u32, value: f64) -> bool {
        let Some(index) = self.index(id) else {
            return false;
        };
        if !value.is_finite() {
            return false;
        }
        self.slots[index]
            .input
            .store(value.to_bits(), Ordering::Release);
        self.slots[index]
            .observed
            .store(value.to_bits(), Ordering::Release);
        let sequence = self.changes.fetch_add(1, Ordering::Relaxed) + 1;
        self.slots[index].desired.store(sequence, Ordering::Release);
        self.pending_input[index / 64].fetch_or(1 << (index % 64), Ordering::Release);
        self.input_words[index / 4096].fetch_or(1 << ((index / 64) % 64), Ordering::Release);
        true
    }
    pub fn reflect(&self, id: u32, value: f64) {
        let Some(index) = self.index(id) else {
            return;
        };
        if !value.is_finite() {
            return;
        }
        let previous = self.slots[index]
            .observed
            .swap(value.to_bits(), Ordering::AcqRel);
        if previous != value.to_bits() {
            self.pending_output[index / 64].fetch_or(1 << (index % 64), Ordering::Release);
            self.output_words[index / 4096].fetch_or(1 << ((index / 64) % 64), Ordering::Release);
            self.changes.fetch_add(1, Ordering::Relaxed);
        }
    }
    pub fn change_count(&self) -> u64 {
        self.changes.load(Ordering::Acquire)
    }
    pub fn drain_input(&self, output: &mut [(u32, f64)]) -> usize {
        self.drain(&self.pending_input, &self.input_words, output, true)
    }
    pub fn drain_output(&self, output: &mut [(u32, f64)]) -> usize {
        self.drain(&self.pending_output, &self.output_words, output, false)
    }
    fn drain(
        &self,
        pending: &[AtomicU64],
        summaries: &[AtomicU64],
        output: &mut [(u32, f64)],
        input: bool,
    ) -> usize {
        let mut count = 0;
        for (summary_index, summary) in summaries.iter().enumerate() {
            if count == output.len() {
                break;
            }
            let mut word_mask = summary.swap(0, Ordering::AcqRel);
            while word_mask != 0 && count < output.len() {
                let word_bit = word_mask.trailing_zeros() as usize;
                let word_index = summary_index * 64 + word_bit;
                let word = &pending[word_index];
                let mut mask = word.swap(0, Ordering::AcqRel);
                while mask != 0 && count < output.len() {
                    let bit = mask.trailing_zeros() as usize;
                    let slot = &self.slots[word_index * 64 + bit];
                    output[count] = (
                        slot.id.load(Ordering::Acquire),
                        f64::from_bits(if input {
                            slot.input.load(Ordering::Acquire)
                        } else {
                            slot.observed.load(Ordering::Acquire)
                        }),
                    );
                    count += 1;
                    mask &= mask - 1;
                }
                if mask != 0 {
                    word.fetch_or(mask, Ordering::Release);
                } else {
                    word_mask &= word_mask - 1;
                }
            }
            if word_mask != 0 {
                summary.fetch_or(word_mask, Ordering::Release);
            }
        }
        count
    }
}

// Mix high bits as well: vendor IDs often encode modules in the high word.
// Linear probing is capped so adversarial IDs cannot consume an audio block.
fn parameter_hash(mut id: u32) -> usize {
    id ^= id >> 16;
    id = id.wrapping_mul(0x85ebca6b);
    id ^= id >> 13;
    id = id.wrapping_mul(0xc2b2ae35);
    id ^= id >> 16;
    id as usize & (SLOT_CAPACITY - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sparse_high_word_vendor_ids_have_bounded_lookup_and_acknowledgement() {
        let bank = ParameterBank::new((0..MAX_PARAMETERS as u32).map(|n| (n << 15, 0.0)).collect())
            .unwrap();
        let id = ((MAX_PARAMETERS - 1) as u32) << 15;
        assert!(bank.publish(id, 0.5));
        assert!(bank.is_pending(id));
        bank.acknowledge(id, 0.25);
        assert!(bank.is_pending(id));
        bank.acknowledge(id, 0.5);
        assert!(!bank.is_pending(id));
        assert!(!bank.publish(u32::MAX, 0.5));
    }
    #[test]
    fn bounded_drain_preserves_remaining_values_and_coalesces() {
        let bank = ParameterBank::new((0..300).map(|id| (id, 0.0)).collect()).unwrap();
        for id in 0..300 {
            assert!(bank.publish(id, f64::from(id)));
        }
        bank.publish(0, 1000.0);
        let mut buffer = [(0, 0.0); MAX_CHANGES_PER_BLOCK];
        assert_eq!(bank.drain_input(&mut buffer), 256);
        assert_eq!(buffer[0], (0, 1000.0));
        assert_eq!(bank.drain_input(&mut buffer), 44);
        assert_eq!(bank.drain_input(&mut buffer), 0);
    }
    #[test]
    fn processor_feedback_cannot_overwrite_pending_user_input() {
        let bank = ParameterBank::new(vec![(17, 0.0)]).unwrap();
        bank.publish(17, 0.8);
        bank.reflect(17, 0.3);
        let mut buffer = [(0, 0.0); 1];
        assert_eq!(bank.drain_input(&mut buffer), 1);
        assert_eq!(buffer[0], (17, 0.8));
        assert_eq!(bank.drain_output(&mut buffer), 1);
        assert_eq!(buffer[0], (17, 0.3));
        assert!(!bank.publish(99, 1.0));
    }
}
