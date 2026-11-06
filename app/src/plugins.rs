//! VST3 hosting glue between the engine and the host crate.
//! (Filled in when the host crate lands; until then every call explains itself.)

use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;
use wave_engine::{Id, NoPlugins, PluginFactory};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParamInfo {
    pub id: u32,
    pub name: String,
    pub value: f64,
    pub display: String,
    pub def: f64,
}

pub struct Plugins;

impl Plugins {
    pub fn new() -> Arc<Self> {
        Arc::new(Plugins)
    }
    pub fn factory(self: &Arc<Self>) -> Arc<dyn PluginFactory> {
        Arc::new(NoPlugins)
    }
    pub fn scan(&self, _rescan: bool) -> Vec<Value> {
        vec![]
    }
    pub fn params(&self, _fx: Id) -> Result<Vec<ParamInfo>, String> {
        Err("VST3 hosting is not in this build".into())
    }
    pub fn set_param(&self, _fx: Id, _id: u32, _v: f64) -> Result<(), String> {
        Err("VST3 hosting is not in this build".into())
    }
    pub fn editor(&self, _app: &tauri::AppHandle, _fx: Id, _open: bool) -> Result<(), String> {
        Err("VST3 hosting is not in this build".into())
    }
    pub fn info(&self, _path: &str, _cid: &str) -> Option<(String, String)> {
        None
    }
}
