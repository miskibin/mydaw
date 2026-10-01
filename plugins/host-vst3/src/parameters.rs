//! Preallocated COM parameter lists; no allocation in a processing callback.
use mydaw_plugin_params::{ParameterBank, MAX_CHANGES_PER_BLOCK};
use std::cell::Cell;
use vst3::Steinberg::Vst::{
    IParamValueQueue, IParamValueQueueTrait, IParameterChanges, IParameterChangesTrait, ParamID,
    ParamValue,
};
use vst3::Steinberg::{int32, kInvalidArgument, kResultOk};
use vst3::{Class, ComWrapper};

const POINTS: usize = 16;
struct ValueQueue {
    id: Cell<u32>,
    count: Cell<usize>,
    points: [Cell<(i32, f64)>; POINTS],
}
impl ValueQueue {
    fn new() -> Self {
        Self {
            id: Cell::new(0),
            count: Cell::new(0),
            points: std::array::from_fn(|_| Cell::new((0, 0.0))),
        }
    }
}
impl Class for ValueQueue {
    type Interfaces = (IParamValueQueue,);
}
// SAFETY: COM callbacks and Cell access occur exclusively inside the node's
// processing call. The plugin must not retain these block-local interfaces.
unsafe impl Send for ValueQueue {}
unsafe impl Sync for ValueQueue {}
impl IParamValueQueueTrait for ValueQueue {
    unsafe fn getParameterId(&self) -> ParamID {
        self.id.get()
    }
    unsafe fn getPointCount(&self) -> int32 {
        self.count.get() as i32
    }
    unsafe fn getPoint(&self, index: int32, offset: *mut int32, value: *mut ParamValue) -> i32 {
        if index < 0 || index as usize >= self.count.get() || offset.is_null() || value.is_null() {
            return kInvalidArgument;
        }
        let (time, point) = self.points[index as usize].get();
        unsafe {
            *offset = time;
            *value = point;
        }
        kResultOk
    }
    unsafe fn addPoint(&self, offset: int32, value: ParamValue, index: *mut int32) -> i32 {
        let count = self.count.get();
        if count >= POINTS || offset < 0 || !value.is_finite() || index.is_null() {
            return kInvalidArgument;
        }
        self.points[count].set((offset, value));
        self.count.set(count + 1);
        unsafe {
            *index = count as i32;
        }
        kResultOk
    }
}
pub struct ParameterChanges {
    queues: Vec<ComWrapper<ValueQueue>>,
    count: Cell<usize>,
}
impl ParameterChanges {
    pub fn new() -> ComWrapper<Self> {
        ComWrapper::new(Self {
            queues: (0..MAX_CHANGES_PER_BLOCK)
                .map(|_| ComWrapper::new(ValueQueue::new()))
                .collect(),
            count: Cell::new(0),
        })
    }
    pub fn clear(&self) {
        for queue in &self.queues[..self.count.get()] {
            queue.count.set(0);
        }
        self.count.set(0);
    }
    pub fn fill(&self, bank: &ParameterBank) {
        self.clear();
        let mut values = [(0, 0.0); MAX_CHANGES_PER_BLOCK];
        let count = bank.drain_input(&mut values);
        for (queue, (id, value)) in self.queues.iter().zip(&values[..count]) {
            queue.id.set(*id);
            queue.count.set(1);
            queue.points[0].set((0, *value));
        }
        self.count.set(count);
    }
    pub fn reflect(&self, bank: &ParameterBank) {
        for queue in &self.queues[..self.count.get()] {
            if queue.count.get() > 0 {
                bank.reflect(queue.id.get(), queue.points[queue.count.get() - 1].get().1);
            }
        }
    }
    pub fn acknowledge(&self, bank: &ParameterBank) {
        for queue in &self.queues[..self.count.get()] {
            if queue.count.get() > 0 {
                bank.acknowledge(queue.id.get(), queue.points[queue.count.get() - 1].get().1);
            }
        }
    }
}
impl Class for ParameterChanges {
    type Interfaces = (IParameterChanges,);
}
// SAFETY: same exclusive processing-thread ownership as ValueQueue above.
unsafe impl Send for ParameterChanges {}
unsafe impl Sync for ParameterChanges {}
impl IParameterChangesTrait for ParameterChanges {
    unsafe fn getParameterCount(&self) -> int32 {
        self.count.get() as i32
    }
    unsafe fn getParameterData(&self, index: int32) -> *mut IParamValueQueue {
        if index < 0 || index as usize >= self.count.get() {
            return std::ptr::null_mut();
        }
        self.queues[index as usize]
            .as_com_ref::<IParamValueQueue>()
            .map_or(std::ptr::null_mut(), |q| q.as_ptr())
    }
    unsafe fn addParameterData(
        &self,
        id: *const ParamID,
        index: *mut int32,
    ) -> *mut IParamValueQueue {
        if id.is_null() || index.is_null() {
            return std::ptr::null_mut();
        }
        let id = unsafe { *id };
        let found = self.queues[..self.count.get()]
            .iter()
            .position(|q| q.id.get() == id);
        let position = match found {
            Some(index) => index,
            None => {
                let position = self.count.get();
                if position == MAX_CHANGES_PER_BLOCK {
                    return std::ptr::null_mut();
                }
                self.queues[position].id.set(id);
                self.queues[position].count.set(0);
                self.count.set(position + 1);
                position
            }
        };
        unsafe {
            *index = position as i32;
            self.getParameterData(position as i32)
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn changes_reach_processor_and_output_feedback() {
        let bank = ParameterBank::new(vec![(17, 0.2)]).unwrap();
        bank.publish(17, 0.7);
        let changes = ParameterChanges::new();
        changes.fill(&bank);
        unsafe {
            assert_eq!(changes.getParameterCount(), 1);
            let queue =
                vst3::ComRef::<IParamValueQueue>::from_raw(changes.getParameterData(0)).unwrap();
            assert_eq!(queue.getParameterId(), 17);
            let mut time = -1;
            let mut value = 0.0;
            assert_eq!(queue.getPoint(0, &mut time, &mut value), kResultOk);
            assert_eq!((time, value), (0, 0.7));
            changes.clear();
            let mut index = 0;
            let queue = vst3::ComRef::<IParamValueQueue>::from_raw(
                changes.addParameterData(&17, &mut index),
            )
            .unwrap();
            assert_eq!(queue.addPoint(8, 0.4, &mut index), kResultOk);
        }
        changes.reflect(&bank);
        assert_eq!(bank.value(17), Some(0.4));
    }
}
