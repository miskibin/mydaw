use crate::hosting::LoadedClapPlugin;
use clack_extensions::params::{ParamInfoBuffer, ParamInfoFlags, PluginParams};
use clack_host::events::event_types::ParamValueEvent;
use clack_host::events::io::{EventBuffer, OutputEventBuffer, OutputEvents, TryPushError};
use clack_host::events::{Match, Pckn, UnknownEvent};
use clack_host::utils::{ClapId, Cookie};
use mydaw_plugin_params::{ParameterBank, ParameterInfo, MAX_CHANGES_PER_BLOCK, MAX_PARAMETERS};
use std::ffi::CString;

pub(crate) fn value_event(id: ClapId, value: f64) -> ParamValueEvent {
    ParamValueEvent::new(
        0,
        id,
        Pckn::new(Match::All, Match::All, Match::All, Match::All),
        value,
        Cookie::empty(),
    )
}
pub(crate) struct ParameterOutput<'a>(pub &'a ParameterBank);
impl OutputEventBuffer for ParameterOutput<'_> {
    fn try_push(&mut self, event: &UnknownEvent) -> Result<(), TryPushError> {
        if let Some(value) = event.as_event::<ParamValueEvent>() {
            if let Some(id) = value.param_id() {
                self.0.reflect(id.get(), value.value());
            }
        }
        Ok(())
    }
}
impl LoadedClapPlugin {
    /// Metadata and displayed values, queried on the plugin's owner thread.
    pub fn parameters(&mut self) -> Result<Vec<ParameterInfo>, String> {
        let mut handle = self.instance.plugin_handle();
        let Some(extension) = handle.get_extension::<PluginParams>() else {
            return Ok(vec![]);
        };
        let count = extension.count(&mut handle) as usize;
        if count > MAX_PARAMETERS {
            return Err("plugin parameter count exceeds host limit".into());
        }
        let bank = self
            .requests
            .parameters
            .get()
            .expect("initialized before plugin creation");
        let mut result = Vec::with_capacity(count);
        let mut info_buffer = ParamInfoBuffer::default();
        for index in 0..count {
            let Some(info) = extension.get_info(&mut handle, index as u32, &mut info_buffer) else {
                return Err("plugin failed to describe a parameter".into());
            };
            let value = extension
                .get_value(&mut handle, info.id)
                .ok_or("plugin failed to read a parameter")?;
            bank.register(info.id.get(), value)?;
            let mut display = [0u8; 256];
            let display = extension
                .value_to_text(&mut handle, info.id, value, &mut display)
                .map(|s| String::from_utf8_lossy(s).into_owned())
                .unwrap_or_else(|_| value.to_string());
            result.push(ParameterInfo {
                id: info.id.get(),
                name: String::from_utf8_lossy(info.name).into_owned(),
                module: String::from_utf8_lossy(info.module).into_owned(),
                value,
                min: info.min_value,
                max: info.max_value,
                default: info.default_value,
                display,
                value_domain: "plain".into(),
                read_only: info.flags.contains(ParamInfoFlags::IS_READONLY),
                stepped: info.flags.contains(ParamInfoFlags::IS_STEPPED),
                automatable: info.flags.contains(ParamInfoFlags::IS_AUTOMATABLE),
                pending: bank.is_pending(info.id.get()),
            });
        }
        Ok(result)
    }
    pub fn set_parameter(
        &mut self,
        id: u32,
        value: Option<f64>,
        text: Option<&str>,
    ) -> Result<(), String> {
        let metadata = self
            .parameters()?
            .into_iter()
            .find(|p| p.id == id)
            .ok_or("parameter ID is not present")?;
        let clap_id = ClapId::from_raw(id).ok_or("invalid parameter ID")?;
        let value = match (value, text) {
            (Some(v), None) => v,
            (None, Some(text)) => {
                let mut handle = self.instance.plugin_handle();
                let extension = handle
                    .get_extension::<PluginParams>()
                    .ok_or("plugin has no parameter extension")?;
                let text = CString::new(text).map_err(|_| "parameter text contains a NUL")?;
                extension
                    .text_to_value(&mut handle, clap_id, &text)
                    .ok_or("plugin could not parse this value")?
            }
            _ => return Err("supply exactly one of value or text".into()),
        };
        metadata.check(value)?;
        let bank = self.requests.parameters.get().expect("initialized");
        if !bank.publish(id, value) {
            return Err("parameter event could not be queued".into());
        }
        self.flush_inactive_parameters();
        Ok(())
    }
    /// Called on the owner thread; active processors receive the same queue in process().
    pub fn flush_inactive_parameters(&mut self) {
        let Some(mut handle) = self.instance.inactive_plugin_handle() else {
            return;
        };
        let Some(extension) = handle.get_extension::<PluginParams>() else {
            return;
        };
        let bank = self.requests.parameters.get().expect("initialized");
        let mut changes = [(0, 0.0); MAX_CHANGES_PER_BLOCK];
        let count = bank.drain_input(&mut changes);
        let mut events = EventBuffer::new();
        for &(id, value) in &changes[..count] {
            if let Some(id) = ClapId::from_raw(id) {
                events.push(&value_event(id, value));
            }
        }
        let mut feedback = ParameterOutput(bank);
        extension.flush(
            &mut handle,
            &events.as_input(),
            &mut OutputEvents::from_buffer(&mut feedback),
        );
        for &(id, value) in &changes[..count] {
            bank.acknowledge(id, value);
        }
    }
    pub fn parameter_change_count(&self) -> u64 {
        self.requests
            .parameters
            .get()
            .map_or(0, |b| b.change_count())
    }
}
