//! Loading a VST3 plugin, activating it, and running it inside the RT graph.
//!
//! [`crate::scan`] finds plugins; this module makes one of them *process audio*.
//!
//! # The instantiation sequence, and why it is in this order
//!
//! VST3 does not have a "create a plugin" call. It has a factory that makes
//! *classes*, and a plugin is one or two of them wired together by the host:
//!
//! ```text
//!  factory->setHostContext(host)          [IPluginFactory3 only; harmless if absent]
//!  factory->createInstance(cid, IComponent)          the processor half
//!  component->initialize(host context)
//!  component->getControllerClassId(&ccid)
//!      ccid == cid  -> single-component: query IEditController off the component
//!      otherwise    -> dual-component:   createInstance(ccid, IEditController)
//!                                        controller->initialize(host context)
//!                                        connect the two IConnectionPoints
//!  controller->setComponentHandler(host)
//!  component->getState(stream); controller->setComponentState(stream)
//!  component -> queryInterface(IAudioProcessor)      the thing that renders
//!  read the bus layout
//! ```
//!
//! Both shapes are in the wild and this host drives both. The
//! `getState`/`setComponentState` step is what makes an editor open showing the
//! processor's actual values rather than the class defaults, and skipping it is
//! the classic reason a freshly loaded plugin's GUI shows the wrong knobs.
//!
//! # Activation, which VST3 is strict about
//!
//! ```text
//!  setBusArrangements(stereo)   } only legal while the component is inactive
//!  setupProcessing(32-bit, rate, max block)
//!  activateBus(audio in/out, event in)
//!  component->setActive(true)
//!  processor->setProcessing(true)
//!  ... process() x N on the audio thread ...
//!  processor->setProcessing(false)
//!  component->setActive(false)
//! ```
//!
//! # The teardown problem, and what we do about it
//!
//! Identical to the CLAP host's, and solved the same way. MYDAW retires graphs
//! through `basedrop`: the audio thread drops the graph and the *collector
//! thread* runs the destructors later. Releasing a plugin's COM objects there --
//! and, worse, unloading its DLL there -- is not something a VST3 plugin expects.
//!
//! So [`Vst3Node`] deactivates nothing on drop. It sends its
//! `ComPtr<IAudioProcessor>` back through a channel to the [`LoadedVst3Plugin`]
//! that created it, and *that* -- on the main thread -- calls `setProcessing`,
//! `setActive` and eventually `terminate`.
//!
//! The one ordering rule that follows: **the `LoadedVst3Plugin` must outlive
//! every `Vst3Node` it created.** If it does not, the module is deliberately
//! leaked rather than unloaded from under a live COM object -- safe, but a leak.
//! [`LoadedVst3Plugin::shutdown`] exists so shutdown code can wait for the node
//! instead of racing the collector.

// The VST3 bindings type their enum constants as `DefaultEnumType`, which the
// crate defines as `c_int` on Windows and `c_uint` everywhere else. So
// `MediaTypes_::kAudio as i32` is a no-op on Windows -- which is what clippy
// sees -- and a genuine narrowing conversion on every other target. Removing the
// casts would break the non-Windows build, so the lint is silenced for this
// module rather than obeyed.
#![allow(clippy::unnecessary_cast)]

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mydaw_rt_core::{NoteKind, ProcessCtx, RtNode, CHANNELS};
use vst3::Steinberg::Vst::{
    AudioBusBuffers, AudioBusBuffers__type0, BusDirections_, BusInfo, Event, Event__type0,
    IAudioProcessor, IAudioProcessorTrait, IComponent, IComponentHandler, IComponentTrait,
    IConnectionPoint, IConnectionPointTrait, IEditController, IEditControllerTrait, IEventList,
    MediaTypes_, NoteOffEvent, NoteOnEvent, ProcessContext, ProcessContext_, ProcessData,
    ProcessModes_, ProcessSetup, SpeakerArr, SpeakerArrangement, SymbolicSampleSizes_,
};
use vst3::Steinberg::{kResultOk, kResultTrue};
use vst3::Steinberg::{
    IBStream, IBStreamTrait, IPluginBaseTrait, IPluginFactory3, IPluginFactory3Trait,
    IPluginFactoryTrait, TUID,
};
use vst3::{ComPtr, ComWrapper};

use crate::host::{
    host_context_ptr, HostEventList, MemoryStream, NoParameterChanges, Vst3GuiRequests,
    Vst3HostContext, Vst3HostRequests, MAX_STATE_BYTES,
};
use crate::module::{ClassId, Vst3Module, Vst3ModuleError};
use crate::scan::{read_classes, Vst3PluginInfo};

/// How long [`LoadedVst3Plugin::shutdown`] waits for a retired node to come back.
///
/// The node is normally dropped by the `basedrop` collector thread, which sweeps
/// every 250 ms, so this is roughly eight sweeps of slack. Waiting only ever
/// happens on a shutdown path, never while audio is running.
pub const RETIRE_TIMEOUT: Duration = Duration::from_secs(2);

/// How long one blocking receive waits before the deadline is re-checked.
const RETIRE_POLL: Duration = Duration::from_millis(25);

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Everything that can go wrong between "here is a path" and "audio is flowing".
///
/// Every variant is reachable from plugin-controlled data. None of them panics:
/// a hostile or simply broken plugin must produce an `Err`, because a panic
/// across an FFI boundary is undefined behaviour and a panic in the DAW is a lost
/// session.
#[derive(Debug, thiserror::Error)]
pub enum Vst3HostError {
    #[error("plugin parameter operation failed: {0}")]
    Parameter(String),
    /// The module would not open. Carries the loader's own reason.
    #[error(transparent)]
    Module(#[from] Vst3ModuleError),

    /// The module loaded, but its factory exports no class with that id.
    #[error("VST3 module {path} contains no plugin class {cid}")]
    ClassNotFound { path: PathBuf, cid: ClassId },

    /// `IPluginFactory::createInstance` refused, or returned nothing.
    #[error("could not create VST3 plugin {cid}: the factory returned {result:#x}")]
    CreateInstance { cid: ClassId, result: i32 },

    /// `IComponent::initialize` refused. On this machine that is the usual
    /// answer from an unlicensed plugin.
    #[error("VST3 plugin '{name}' refused to initialise (result {result:#x})")]
    Initialize { name: String, result: i32 },

    /// The component exposes no `IAudioProcessor`, so it cannot render anything.
    #[error("VST3 plugin '{name}' exposes no IAudioProcessor")]
    NoAudioProcessor { name: String },

    /// The plugin cannot process 32-bit float, which is the only sample format
    /// the Phase 1 engine has.
    #[error("VST3 plugin '{name}' cannot process 32-bit float samples")]
    NoFloat32 { name: String },

    /// The audio configuration is not one any plugin can be activated with.
    #[error("invalid audio configuration: {message}")]
    InvalidAudioConfiguration { message: String },

    /// The plugin's buses are not something the Phase 1 engine can drive.
    #[error("unsupported bus layout: {message}")]
    UnsupportedBusLayout { message: String },

    /// `setupProcessing`, `setBusArrangements`, `setActive` or `setProcessing`
    /// refused.
    #[error("VST3 plugin '{name}' refused while {stage} (result {result:#x})")]
    Activate {
        name: String,
        stage: &'static str,
        result: i32,
    },

    /// A node from this plugin is still in a graph, so it cannot be activated
    /// again. Retryable: the `basedrop` collector is simply behind.
    #[error(
        "VST3 plugin '{name}' is already active; its previous audio node has not been retired"
    )]
    AlreadyActive { name: String },

    /// Plugin code panicked through the FFI boundary. Caught, reported, and the
    /// plugin is not used.
    #[error("VST3 plugin panicked while {stage}")]
    Panicked { stage: &'static str },

    /// `getState` / `setState` refused, or produced nothing.
    #[error("VST3 plugin '{name}' refused to {direction} its state (result {result:#x})")]
    State {
        name: String,
        direction: &'static str,
        result: i32,
    },

    /// A state blob larger than [`MAX_STATE_BYTES`].
    #[error("the state blob is {size} bytes, past the {MAX_STATE_BYTES} byte limit")]
    StateTooLarge { size: usize },
}

// ---------------------------------------------------------------------------
// Bus layout
// ---------------------------------------------------------------------------

/// What a plugin says about its buses.
///
/// Read once, on the main thread, before activation. The VST3 equivalent of
/// `ClapPortLayout`, and it answers the same question: may this plugin go in this
/// slot?
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Vst3BusLayout {
    pub audio_input_buses: u32,
    pub audio_output_buses: u32,
    /// Channel count of audio input bus 0.
    pub main_input_channels: u32,
    /// Channel count of audio output bus 0.
    pub main_output_channels: u32,
    /// Event input buses. Zero means the plugin takes no notes.
    pub event_input_buses: u32,
    /// Event output buses. Recorded for completeness; MYDAW does not read a
    /// plugin's event output yet.
    pub event_output_buses: u32,
}

impl Vst3BusLayout {
    /// Whether this layout is the stereo-in/stereo-out shape of an insert
    /// effect. Anything wider is a Phase 2 mixer feature.
    pub fn is_stereo_effect(&self) -> bool {
        self.audio_input_buses >= 1
            && self.audio_output_buses >= 1
            && self.main_input_channels == CHANNELS as u32
            && self.main_output_channels == CHANNELS as u32
    }

    /// Whether this layout is an instrument: no audio input bus, stereo out.
    ///
    /// A note input is not required. A plugin that generates without being
    /// played is still an instrument as far as the graph is concerned; it just
    /// never receives events.
    pub fn is_instrument(&self) -> bool {
        self.audio_input_buses == 0
            && self.audio_output_buses >= 1
            && self.main_output_channels == CHANNELS as u32
    }

    /// Whether the Phase 1 graph can drive this plugin at all.
    pub fn is_supported(&self) -> bool {
        self.is_stereo_effect() || self.is_instrument()
    }

    /// Whether the plugin takes audio input, and therefore whether
    /// [`Vst3Node`] hands it the bus.
    pub fn has_audio_input(&self) -> bool {
        self.audio_input_buses >= 1
    }

    /// Whether the plugin accepts note events on event bus 0.
    pub fn has_note_input(&self) -> bool {
        self.event_input_buses >= 1
    }
}

// ---------------------------------------------------------------------------
// Loaded plugin
// ---------------------------------------------------------------------------

/// A VST3 plugin instance living on the main thread.
///
/// Not `Send`, by construction: it owns COM pointers and a loaded module, and
/// every lifecycle call VST3 defines (`initialize`, `setActive`, `terminate`) is
/// main-thread work in the same sense CLAP means it.
///
/// # Field order is the teardown protocol
/// Rust drops fields in declaration order, so every COM pointer is declared
/// before `module`: releasing a COM object after its DLL has been unloaded is a
/// use-after-free, and this is what makes that unrepresentable rather than merely
/// avoided.
pub struct LoadedVst3Plugin {
    /// Our own reference to the processor. The node gets a second one.
    processor: Option<ComPtr<IAudioProcessor>>,
    controller: Option<ComPtr<IEditController>>,
    /// The two halves' connection points, when the plugin is dual-component.
    /// Held so they can be disconnected in the right order at teardown.
    component_connection: Option<ComPtr<IConnectionPoint>>,
    controller_connection: Option<ComPtr<IConnectionPoint>>,
    component: Option<ComPtr<IComponent>>,
    /// The `IHostApplication` + `IComponentHandler` the plugin was given.
    ///
    /// Never read again -- it is a **keep-alive**. Both halves of the plugin hold
    /// raw pointers into this object, so it has to outlive them, and it is
    /// declared after `component` so that drop order gives it that.
    _context: ComWrapper<Vst3HostContext>,
    requests: Arc<Vst3HostRequests>,
    gui_requests: Arc<Vst3GuiRequests>,
    info: Vst3PluginInfo,
    layout: Vst3BusLayout,
    /// Whether the controller is the component (single-component design).
    single_component: bool,
    /// Whether `setActive(true)` has been called and not yet undone.
    active: bool,
    /// True between `create_node` and a successful retire.
    node_outstanding: bool,
    retire_tx: Sender<ComPtr<IAudioProcessor>>,
    retire_rx: Receiver<ComPtr<IAudioProcessor>>,
    /// Declared last: see the type docs.
    module: Vst3Module,
}

impl LoadedVst3Plugin {
    /// Load `path` and instantiate the plugin class `cid`.
    ///
    /// Runs third-party code throughout, all of it inside [`catch_unwind`].
    ///
    /// # Errors
    /// Any [`Vst3HostError`]; none of them is fatal to the caller.
    pub fn load(path: &Path, cid: ClassId) -> Result<Self, Vst3HostError> {
        catch_unwind(AssertUnwindSafe(|| Self::load_inner(path, cid))).map_err(|_| {
            Vst3HostError::Panicked {
                stage: "being instantiated",
            }
        })?
    }

    fn load_inner(path: &Path, cid: ClassId) -> Result<Self, Vst3HostError> {
        let module = Vst3Module::open(path)?;
        let info = read_classes(module.factory(), path)
            .into_iter()
            .find(|class| class.cid == cid)
            .ok_or(Vst3HostError::ClassNotFound {
                path: path.to_path_buf(),
                cid,
            })?;

        let requests = Arc::new(Vst3HostRequests::default());
        let _ = requests.parameters.set(Arc::new(
            mydaw_plugin_params::ParameterBank::new(vec![]).map_err(Vst3HostError::Parameter)?,
        ));
        let gui_requests = Arc::new(Vst3GuiRequests::default());
        let context = Vst3HostContext::new(Arc::clone(&requests));
        let context_ptr = host_context_ptr(&context);

        // SAFETY (this whole block): every pointer handed to the plugin outlives
        // the call that receives it (`context` is a field of the value being
        // built, `tuid`/`iid` are locals live across their one use), every
        // out-parameter is a live local, and every returned COM pointer is
        // adopted exactly once through `ComPtr::from_raw`, which is what
        // `createInstance` and `queryInterface` hand out. The whole block is
        // inside `catch_unwind` at the caller.
        let (component, controller, single_component, connections) = unsafe {
            let factory = module.factory();

            // Optional, and worth doing: a plugin that can reach the host during
            // class creation gives better diagnostics than one that cannot.
            if let Some(factory3) = factory.cast::<IPluginFactory3>() {
                let _ = factory3.setHostContext(context_ptr);
            }

            let class_tuid: TUID = cid.to_tuid();
            let mut raw: *mut std::ffi::c_void = std::ptr::null_mut();
            let result = factory.createInstance(class_tuid.as_ptr(), component_iid(), &mut raw);
            let component = ComPtr::<IComponent>::from_raw(raw.cast())
                .filter(|_| result == kResultOk)
                .ok_or(Vst3HostError::CreateInstance { cid, result })?;

            let result = component.initialize(context_ptr);
            if result != kResultOk {
                return Err(Vst3HostError::Initialize {
                    name: info.name.clone(),
                    result,
                });
            }

            // Which shape is this plugin? A component that names *itself* as its
            // controller, or names nothing, is single-component.
            let mut controller_cid: TUID = [0; 16];
            let has_separate = component.getControllerClassId(&mut controller_cid) == kResultOk
                && ClassId::from_tuid(&controller_cid) != cid
                && controller_cid != [0; 16];

            let (controller, single_component) = if has_separate {
                let mut raw: *mut std::ffi::c_void = std::ptr::null_mut();
                let result = factory.createInstance(
                    controller_cid.as_ptr(),
                    edit_controller_iid(),
                    &mut raw,
                );
                match ComPtr::<IEditController>::from_raw(raw.cast())
                    .filter(|_| result == kResultOk)
                {
                    Some(controller) => {
                        if controller.initialize(context_ptr) != kResultOk {
                            // A controller that will not initialise costs the
                            // editor, not the audio. Carry on without one.
                            log::warn!(
                                "VST3 plugin '{}' has a controller that refused to initialise; \
                                 it will have no editor",
                                info.name
                            );
                            (None, false)
                        } else {
                            (Some(controller), false)
                        }
                    }
                    None => (None, false),
                }
            } else {
                // Single-component: the controller *is* the component, already
                // initialised. `cast` addRefs, which is right -- it is a second
                // owning handle to the same object.
                (component.cast::<IEditController>(), true)
            };

            // Connect the two halves, when they are two. A plugin whose editor
            // never hears from its processor is the classic symptom of skipping
            // this.
            let connections = if single_component {
                (None, None)
            } else {
                match (
                    component.cast::<IConnectionPoint>(),
                    controller
                        .as_ref()
                        .and_then(ComPtr::cast::<IConnectionPoint>),
                ) {
                    (Some(from_component), Some(from_controller)) => {
                        let _ = from_component.connect(from_controller.as_ptr());
                        let _ = from_controller.connect(from_component.as_ptr());
                        (Some(from_component), Some(from_controller))
                    }
                    _ => (None, None),
                }
            };

            if let Some(controller) = controller.as_ref() {
                if let Some(handler) = context.as_com_ref::<IComponentHandler>() {
                    let _ = controller.setComponentHandler(handler.as_ptr());
                }
                // Give the editor the processor's current values, or it opens
                // showing the class defaults. Best-effort: a plugin with no
                // state is entitled to refuse.
                transfer_component_state(&component, controller);
            }

            (component, controller, single_component, connections)
        };

        // SAFETY: `component` is live; `queryInterface` for `IAudioProcessor` is
        // exactly what `cast` performs.
        let processor =
            component
                .cast::<IAudioProcessor>()
                .ok_or_else(|| Vst3HostError::NoAudioProcessor {
                    name: info.name.clone(),
                })?;

        // SAFETY: as above; `canProcessSampleSize` takes a plain scalar.
        let can_float32 =
            unsafe { processor.canProcessSampleSize(SymbolicSampleSizes_::kSample32 as i32) };
        if can_float32 != kResultTrue {
            return Err(Vst3HostError::NoFloat32 {
                name: info.name.clone(),
            });
        }

        let layout = read_bus_layout(&component);
        let (retire_tx, retire_rx) = mpsc::channel();
        let (component_connection, controller_connection) = connections;

        Ok(Self {
            processor: Some(processor),
            controller,
            component_connection,
            controller_connection,
            component: Some(component),
            _context: context,
            requests,
            gui_requests,
            info,
            layout,
            single_component,
            active: false,
            node_outstanding: false,
            retire_tx,
            retire_rx,
            module,
        })
    }

    /// Identity of the plugin class that was instantiated.
    pub fn info(&self) -> &Vst3PluginInfo {
        &self.info
    }

    /// Bus layout as reported by the plugin before activation.
    pub fn bus_layout(&self) -> Vst3BusLayout {
        self.layout
    }

    /// Whether the controller and the component are the same object.
    ///
    /// Diagnostics: the two designs fail differently, and knowing which one is in
    /// front of you is the first question when an editor misbehaves.
    pub fn is_single_component(&self) -> bool {
        self.single_component
    }

    /// Latched `IComponentHandler` callbacks.
    pub fn requests(&self) -> &Arc<Vst3HostRequests> {
        &self.requests
    }

    /// Latched `IPlugFrame` callbacks. Serviced by the GUI session, if one is
    /// open; see [`crate::gui`].
    pub fn gui_requests(&self) -> &Arc<Vst3GuiRequests> {
        &self.gui_requests
    }

    /// Whether the instance is currently activated.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Whether a node created from this plugin is still outstanding.
    pub const fn has_outstanding_node(&self) -> bool {
        self.node_outstanding
    }

    /// Whether the plugin has an edit controller, and therefore *may* have an
    /// editor.
    ///
    /// Not a guarantee of a view -- `createView` can still return null -- but its
    /// absence is a guarantee of no view, which is what the UI needs to decide
    /// whether to draw an "Open editor" button at all.
    pub fn controller_present(&self) -> bool {
        self.controller.is_some()
    }

    /// Owner-thread metadata with native normalized values and plugin formatting.
    pub fn parameters(&self) -> Result<Vec<mydaw_plugin_params::ParameterInfo>, String> {
        use vst3::Steinberg::Vst::{ParameterInfo, ParameterInfo_::ParameterFlags_};
        let Some(controller) = self.controller.as_ref() else {
            return Ok(vec![]);
        };
        let bank = self
            .requests
            .parameters
            .get()
            .expect("initialized before plugin creation");
        // SAFETY: controller metadata calls are made on the instance owner thread.
        let count = unsafe { controller.getParameterCount() };
        if count < 0 || count as usize > mydaw_plugin_params::MAX_PARAMETERS {
            return Err("invalid plugin parameter count".into());
        }
        let mut result = Vec::with_capacity(count as usize);
        for index in 0..count {
            let mut info: ParameterInfo = unsafe { std::mem::zeroed() };
            if unsafe { controller.getParameterInfo(index, &mut info) } != kResultOk {
                continue;
            }
            let value = unsafe { controller.getParamNormalized(info.id) };
            bank.register(info.id, value)?;
            let mut display = [0; 128];
            let _ = unsafe { controller.getParamStringByValue(info.id, value, &mut display) };
            let utf16 = |text: &[u16]| {
                String::from_utf16_lossy(
                    &text[..text.iter().position(|v| *v == 0).unwrap_or(text.len())],
                )
            };
            result.push(mydaw_plugin_params::ParameterInfo {
                id: info.id,
                name: utf16(&info.title),
                module: format!("unit {}", info.unitId),
                value,
                min: 0.0,
                max: 1.0,
                default: info.defaultNormalizedValue,
                display: utf16(&display),
                value_domain: "normalized".into(),
                read_only: info.flags & ParameterFlags_::kIsReadOnly != 0,
                stepped: info.stepCount > 0,
                automatable: info.flags & ParameterFlags_::kCanAutomate != 0,
                pending: bank.is_pending(info.id),
            });
        }
        Ok(result)
    }
    pub fn set_parameter(
        &self,
        id: u32,
        value: Option<f64>,
        text: Option<&str>,
    ) -> Result<(), String> {
        let controller = self
            .controller
            .as_ref()
            .ok_or("plugin has no parameter controller")?;
        let metadata = self
            .parameters()?
            .into_iter()
            .find(|p| p.id == id)
            .ok_or("parameter ID is not present")?;
        let value = match (value, text) {
            (Some(value), None) => value,
            (None, Some(text)) => {
                let mut input: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
                let mut value = 0.0;
                if unsafe { controller.getParamValueByString(id, input.as_mut_ptr(), &mut value) }
                    != kResultOk
                {
                    return Err("plugin could not parse parameter text".into());
                }
                value
            }
            _ => return Err("provide exactly one of value or text".into()),
        };
        metadata.check(value)?;
        if unsafe { controller.setParamNormalized(id, value) } != kResultOk {
            return Err("plugin refused parameter value".into());
        }
        if !self
            .requests
            .parameters
            .get()
            .expect("initialized")
            .publish(id, value)
        {
            return Err("parameter event could not be queued".into());
        }
        Ok(())
    }
    pub fn parameter_change_count(&self) -> u64 {
        self.requests
            .parameters
            .get()
            .map_or(0, |b| b.change_count())
    }
    pub fn sync_parameter_feedback(&self) {
        let Some(controller) = self.controller.as_ref() else {
            return;
        };
        let Some(bank) = self.requests.parameters.get() else {
            return;
        };
        let mut values = [(0, 0.0); mydaw_plugin_params::MAX_CHANGES_PER_BLOCK];
        let count = bank.drain_output(&mut values);
        for (id, value) in &values[..count] {
            let _ = unsafe { controller.setParamNormalized(*id, *value) };
        }
    }

    /// The plugin's edit controller, if it has one. `[main-thread]`.
    pub(crate) fn controller(&self) -> Option<&ComPtr<IEditController>> {
        self.controller.as_ref()
    }

    /// Activate the plugin for this stream and wrap it in an [`RtNode`].
    ///
    /// Everything the RT path will ever need is allocated here: the scratch
    /// buffers, the event list and the empty parameter list. [`Vst3Node::process`]
    /// allocates nothing.
    ///
    /// # Errors
    /// [`Vst3HostError::InvalidAudioConfiguration`] for a nonsensical rate or
    /// block size, [`Vst3HostError::UnsupportedBusLayout`] for anything that is
    /// not stereo in / stereo out, [`Vst3HostError::AlreadyActive`] while a
    /// previous node is still out, and [`Vst3HostError::Activate`] for a plugin
    /// that refuses.
    pub fn create_node(
        &mut self,
        sample_rate: f64,
        max_block: usize,
    ) -> Result<Vst3Node, Vst3HostError> {
        self.parameters().map_err(Vst3HostError::Parameter)?;
        // Do this first: a node retired since the last call frees the instance.
        self.collect_retired();

        if !(sample_rate.is_finite() && sample_rate > 0.0) {
            return Err(Vst3HostError::InvalidAudioConfiguration {
                message: format!("sample rate {sample_rate} is not a positive, finite number"),
            });
        }
        if max_block == 0 {
            return Err(Vst3HostError::InvalidAudioConfiguration {
                message: "maximum block size cannot be 0".to_owned(),
            });
        }
        let max_samples =
            i32::try_from(max_block).map_err(|_| Vst3HostError::InvalidAudioConfiguration {
                message: format!("maximum block size {max_block} does not fit in int32"),
            })?;
        if !self.layout.is_supported() {
            return Err(Vst3HostError::UnsupportedBusLayout {
                message: format!(
                    "'{}' reports {} audio input bus(es) x {} ch and {} output bus(es) x {} ch; \
                     the Phase 1 graph drives stereo in / stereo out (an effect) or \
                     no input / stereo out (an instrument)",
                    self.info.name,
                    self.layout.audio_input_buses,
                    self.layout.main_input_channels,
                    self.layout.audio_output_buses,
                    self.layout.main_output_channels,
                ),
            });
        }
        if self.active {
            return Err(Vst3HostError::AlreadyActive {
                name: self.info.name.clone(),
            });
        }

        let name = self.info.name.clone();
        let layout = self.layout;
        let component = self.component.clone().expect("live until Drop");
        let processor = self.processor.clone().expect("live until Drop");

        let refused = |stage: &'static str, result: i32| Vst3HostError::Activate {
            name: name.clone(),
            stage,
            result,
        };

        // SAFETY: `component` and `processor` are live handles to the same
        // plugin object, the component is inactive (checked above), and every
        // pointer below is to a live local. This is the activation order the
        // VST3 specification requires; see the module docs.
        catch_unwind(AssertUnwindSafe(|| -> Result<(), Vst3HostError> {
            unsafe {
                let mut stereo: SpeakerArrangement = SpeakerArr::kStereo;
                let result = if layout.has_audio_input() {
                    let mut input = stereo;
                    processor.setBusArrangements(&mut input, 1, &mut stereo, 1)
                } else {
                    processor.setBusArrangements(std::ptr::null_mut(), 0, &mut stereo, 1)
                };
                if result != kResultOk {
                    // Not fatal: a plugin may decline a specific arrangement and
                    // still render its own default, which the bus-layout check
                    // has already confirmed is stereo.
                    log::debug!(
                        "VST3 plugin '{name}' declined the stereo bus arrangement \
                         ({result:#x}); using its own default"
                    );
                }

                let mut setup = ProcessSetup {
                    processMode: if std::env::var_os("MYDAW_RENDER_WORKER").is_some() {
                        ProcessModes_::kOffline as i32
                    } else {
                        ProcessModes_::kRealtime as i32
                    },
                    symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
                    maxSamplesPerBlock: max_samples,
                    sampleRate: sample_rate,
                };
                let result = processor.setupProcessing(&mut setup);
                if result != kResultOk {
                    return Err(refused("setting up processing", result));
                }

                // Buses default to inactive unless flagged; a host that never
                // activates them gets silence and no error.
                if layout.has_audio_input() {
                    let _ = component.activateBus(
                        MediaTypes_::kAudio as i32,
                        BusDirections_::kInput as i32,
                        0,
                        1,
                    );
                }
                let _ = component.activateBus(
                    MediaTypes_::kAudio as i32,
                    BusDirections_::kOutput as i32,
                    0,
                    1,
                );
                if layout.has_note_input() {
                    let _ = component.activateBus(
                        MediaTypes_::kEvent as i32,
                        BusDirections_::kInput as i32,
                        0,
                        1,
                    );
                }

                let result = component.setActive(1);
                if result != kResultOk {
                    return Err(refused("being activated", result));
                }
                let result = processor.setProcessing(1);
                if result != kResultOk {
                    // Undo the activation rather than leave the plugin half-on.
                    let _ = component.setActive(0);
                    return Err(refused("starting processing", result));
                }
                Ok(())
            }
        }))
        .map_err(|_| Vst3HostError::Panicked {
            stage: "being activated",
        })??;

        self.active = true;
        self.node_outstanding = true;
        Ok(Vst3Node::new(
            processor,
            max_block,
            layout,
            self.retire_tx.clone(),
            Arc::clone(self.requests.parameters.get().expect("initialized")),
        ))
    }

    /// Deactivate any processor a dropped [`Vst3Node`] has handed back.
    ///
    /// Cheap and non-blocking: call it from the main thread whenever convenient.
    /// Returns the number of processors retired (0 or 1 in practice).
    pub fn collect_retired(&mut self) -> usize {
        let mut retired = 0;
        while let Ok(processor) = self.retire_rx.try_recv() {
            self.deactivate(&processor);
            retired += 1;
        }
        retired
    }

    /// Wait (up to `timeout`) for a retired node to come home, so the plugin can
    /// be activated again.
    ///
    /// A VST3 component can only be active once, and MYDAW retires nodes through
    /// `basedrop`: the audio thread drops the node and the *collector thread*
    /// runs its destructor some sweeps later. Re-activating -- which a graph
    /// rebuild and a source swap both do -- therefore has to wait for that.
    ///
    /// Returns `true` when the instance is inactive (i.e. ready to activate).
    pub fn wait_for_retire(&mut self, timeout: Duration) -> bool {
        self.collect_retired();
        if !self.active {
            return true;
        }
        let deadline = Instant::now() + timeout;
        while self.active {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            // A bounded receive rather than a sleep loop: the collector wakes us
            // the instant it drops the node.
            match self.retire_rx.recv_timeout(remaining.min(RETIRE_POLL)) {
                Ok(processor) => self.deactivate(&processor),
                Err(_) => continue,
            }
        }
        !self.active
    }

    /// Wait for an outstanding node, then deactivate. Returns `true` if the
    /// instance ended up inactive.
    ///
    /// This is the shutdown path, and the only one that blocks.
    pub fn shutdown(&mut self) -> bool {
        if self.node_outstanding && self.active {
            self.wait_for_retire(RETIRE_TIMEOUT);
        } else {
            self.collect_retired();
        }

        if self.active {
            // The node never came home. Deactivating now would race whichever
            // thread still holds the processor, so the honest answer is to stop
            // touching it -- and to make sure its DLL is never unloaded out from
            // under it. A leaked module is a bounded cost; a use-after-free in
            // an audio callback is not.
            log::warn!(
                "VST3 plugin '{}' still has an audio node outstanding at teardown; its module \
                 will be leaked rather than unloaded",
                self.info.name
            );
            self.module.leak();
        }
        !self.active
    }

    /// `setProcessing(false)` + `setActive(false)`, on this (main) thread.
    ///
    /// `processor` is the handle the node gave back; it is passed rather than
    /// taken from `self` only so that the caller cannot forget to receive it.
    fn deactivate(&mut self, processor: &ComPtr<IAudioProcessor>) {
        if !self.active {
            self.node_outstanding = false;
            return;
        }
        let Some(component) = self.component.as_ref() else {
            self.active = false;
            self.node_outstanding = false;
            return;
        };
        // SAFETY: the audio thread is provably gone -- it dropped the node,
        // which is what sent this processor here, and no other handle to it
        // exists. Both calls are the documented counterparts of `create_node`'s.
        let outcome = catch_unwind(AssertUnwindSafe(|| unsafe {
            processor.setProcessing(0);
            component.setActive(0);
        }));
        if outcome.is_err() {
            log::error!(
                "VST3 plugin '{}' panicked while being deactivated; its module will be leaked",
                self.info.name
            );
            self.module.leak();
        }
        self.active = false;
        self.node_outstanding = false;
    }

    // ------------------------------------------------------------------ state

    /// Serialise the plugin's component state.
    ///
    /// Only the *component* half is saved. A complete VST3 preset also carries
    /// the controller's own state, which matters for a plugin that keeps
    /// editor-only settings (a scroll position, a skin); step 7 does not, and the
    /// gap is recorded in the spec rather than papered over.
    ///
    /// # Errors
    /// [`Vst3HostError::State`] when the plugin refuses.
    pub fn save_state(&mut self) -> Result<Vec<u8>, Vst3HostError> {
        let Some(component) = self.component.as_ref() else {
            return Ok(Vec::new());
        };
        let stream = MemoryStream::empty();
        let Some(bstream) = stream.as_com_ref::<IBStream>() else {
            return Ok(Vec::new());
        };
        // SAFETY: `stream` outlives the call, and `getState` is documented to
        // write into whatever `IBStream` the host provides.
        let result = catch_unwind(AssertUnwindSafe(|| unsafe {
            component.getState(bstream.as_ptr())
        }))
        .map_err(|_| Vst3HostError::Panicked {
            stage: "saving its state",
        })?;

        if result != kResultOk {
            return Err(Vst3HostError::State {
                name: self.info.name.clone(),
                direction: "save",
                result,
            });
        }
        let controller_state = if let Some(controller) = &self.controller {
            let stream = MemoryStream::empty();
            let bstream = stream
                .as_com_ref::<IBStream>()
                .ok_or_else(|| Vst3HostError::Parameter("state stream unavailable".into()))?;
            // SAFETY: owner thread, stream remains alive for the controller call.
            if unsafe { controller.getState(bstream.as_ptr()) } == kResultOk {
                stream.contents()
            } else {
                vec![]
            }
        } else {
            vec![]
        };
        crate::preset::encode(&stream.contents(), &controller_state)
            .map_err(Vst3HostError::Parameter)
    }

    /// Restore the plugin from a blob produced by [`save_state`](Self::save_state).
    ///
    /// The controller is told too, so an open editor follows the change rather
    /// than showing values the processor no longer has.
    ///
    /// # Errors
    /// [`Vst3HostError::StateTooLarge`] for an implausible blob, and
    /// [`Vst3HostError::State`] when the plugin refuses it.
    pub fn load_state(&mut self, bytes: &[u8]) -> Result<(), Vst3HostError> {
        if bytes.len() > MAX_STATE_BYTES {
            return Err(Vst3HostError::StateTooLarge { size: bytes.len() });
        }
        let Some(component) = self.component.as_ref() else {
            return Ok(());
        };
        let (component_bytes, controller_bytes) =
            crate::preset::decode(bytes).map_err(Vst3HostError::Parameter)?;
        let stream = MemoryStream::reading(component_bytes);
        let Some(bstream) = stream.as_com_ref::<IBStream>() else {
            return Ok(());
        };

        // SAFETY: `stream` outlives both calls. The rewind between them is
        // required: `setState` leaves the cursor at the end, and the controller
        // would otherwise read nothing.
        let result = catch_unwind(AssertUnwindSafe(|| unsafe {
            let result = component.setState(bstream.as_ptr());
            if result == kResultOk {
                if let Some(controller) = self.controller.as_ref() {
                    let mut position = 0_i64;
                    let _ = bstream.seek(0, 0, &mut position);
                    let _ = controller.setComponentState(bstream.as_ptr());
                    if !controller_bytes.is_empty() {
                        let editor_stream = MemoryStream::reading(controller_bytes);
                        if let Some(editor_stream) = editor_stream.as_com_ref::<IBStream>() {
                            let _ = controller.setState(editor_stream.as_ptr());
                        }
                    }
                }
            }
            result
        }))
        .map_err(|_| Vst3HostError::Panicked {
            stage: "restoring its state",
        })?;

        if result != kResultOk {
            return Err(Vst3HostError::State {
                name: self.info.name.clone(),
                direction: "restore",
                result,
            });
        }
        Ok(())
    }
}

impl std::fmt::Debug for LoadedVst3Plugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedVst3Plugin")
            .field("name", &self.info.name)
            .field("cid", &self.info.cid)
            .field("path", &self.info.path)
            .field("layout", &self.layout)
            .field("single_component", &self.single_component)
            .field("active", &self.active)
            .finish()
    }
}

impl Drop for LoadedVst3Plugin {
    fn drop(&mut self) {
        // Last chance to tear down on the right thread, in the order VST3 wants:
        // deactivate, disconnect the halves, terminate both, release, unload.
        self.shutdown();

        // SAFETY: every handle below is live, and this is the only place any of
        // them is terminated. `catch_unwind` because a plugin's `terminate` runs
        // its own teardown, which is as trustworthy as the rest of it.
        let outcome = catch_unwind(AssertUnwindSafe(|| unsafe {
            if let (Some(from_component), Some(from_controller)) = (
                self.component_connection.as_ref(),
                self.controller_connection.as_ref(),
            ) {
                let _ = from_component.disconnect(from_controller.as_ptr());
                let _ = from_controller.disconnect(from_component.as_ptr());
            }
            if let Some(controller) = self.controller.as_ref() {
                let _ = controller.setComponentHandler(std::ptr::null_mut());
                // A single-component plugin's controller *is* its component, and
                // terminating it twice is a double free.
                if !self.single_component {
                    let _ = controller.terminate();
                }
            }
            if let Some(component) = self.component.as_ref() {
                let _ = component.terminate();
            }
        }));
        if outcome.is_err() {
            log::error!(
                "VST3 plugin '{}' panicked during terminate; its module will be leaked",
                self.info.name
            );
            self.module.leak();
        }

        // Explicit rather than left to field order, so that the ordering is
        // visible at the place it matters as well as in the declaration.
        drop(self.controller_connection.take());
        drop(self.component_connection.take());
        drop(self.controller.take());
        drop(self.processor.take());
        drop(self.component.take());
    }
}

/// `queryInterface` the component's buses into a [`Vst3BusLayout`].
fn read_bus_layout(component: &ComPtr<IComponent>) -> Vst3BusLayout {
    // SAFETY: `component` is live and initialised. `getBusCount` and `getBusInfo`
    // take plain scalars and one live out-parameter; a plugin that reports
    // nonsense produces a layout the caller then refuses, not a memory error.
    unsafe {
        let count = |media: i32, direction: i32| -> u32 {
            u32::try_from(component.getBusCount(media, direction)).unwrap_or(0)
        };
        let channels = |direction: i32| -> u32 {
            let mut info: BusInfo = std::mem::zeroed();
            if component.getBusInfo(MediaTypes_::kAudio as i32, direction, 0, &mut info)
                == kResultOk
            {
                u32::try_from(info.channelCount).unwrap_or(0)
            } else {
                0
            }
        };

        let audio_input_buses = count(MediaTypes_::kAudio as i32, BusDirections_::kInput as i32);
        let audio_output_buses = count(MediaTypes_::kAudio as i32, BusDirections_::kOutput as i32);
        Vst3BusLayout {
            audio_input_buses,
            audio_output_buses,
            main_input_channels: if audio_input_buses > 0 {
                channels(BusDirections_::kInput as i32)
            } else {
                0
            },
            main_output_channels: if audio_output_buses > 0 {
                channels(BusDirections_::kOutput as i32)
            } else {
                0
            },
            event_input_buses: count(MediaTypes_::kEvent as i32, BusDirections_::kInput as i32),
            event_output_buses: count(MediaTypes_::kEvent as i32, BusDirections_::kOutput as i32),
        }
    }
}

/// Copy the component's state into the controller, so an editor opens showing
/// what the processor actually holds.
///
/// Best effort throughout: a plugin with no state to report is not broken.
///
/// # Safety
/// Both handles must be live and initialised.
unsafe fn transfer_component_state(
    component: &ComPtr<IComponent>,
    controller: &ComPtr<IEditController>,
) {
    let stream = MemoryStream::empty();
    let Some(bstream) = stream.as_com_ref::<IBStream>() else {
        return;
    };
    if component.getState(bstream.as_ptr()) != kResultOk || stream.is_empty() {
        return;
    }
    let mut position = 0_i64;
    let _ = bstream.seek(0, 0, &mut position);
    let _ = controller.setComponentState(bstream.as_ptr());
}

/// `IComponent`'s IID as the `FIDString` `createInstance` wants.
fn component_iid() -> *const vst3::Steinberg::char8 {
    vst3::Steinberg::Vst::IComponent_iid.as_ptr()
}

/// `IEditController`'s IID, likewise.
fn edit_controller_iid() -> *const vst3::Steinberg::char8 {
    vst3::Steinberg::Vst::IEditController_iid.as_ptr()
}

// ---------------------------------------------------------------------------
// The RT node
// ---------------------------------------------------------------------------

/// Counters a [`Vst3Node`] maintains for non-RT observers.
///
/// Atomics rather than logging, because `log::warn!` on the audio thread can
/// allocate and take a lock.
#[derive(Debug, Default)]
pub struct Vst3NodeStats {
    process_calls: AtomicU64,
    process_errors: AtomicU64,
    truncated_blocks: AtomicU64,
    events_sent: AtomicU64,
    events_dropped: AtomicU64,
}

impl Vst3NodeStats {
    /// Blocks handed to the plugin since activation.
    pub fn process_calls(&self) -> u64 {
        self.process_calls.load(Ordering::Relaxed)
    }

    /// Blocks where `IAudioProcessor::process` reported failure. The node passes
    /// audio through unchanged for those blocks (bypass), never garbage.
    pub fn process_errors(&self) -> u64 {
        self.process_errors.load(Ordering::Relaxed)
    }

    /// Blocks larger than the `max_block` the node was activated for. Non-zero
    /// means the tail of those blocks was silenced.
    pub fn truncated_blocks(&self) -> u64 {
        self.truncated_blocks.load(Ordering::Relaxed)
    }

    /// Note events handed to the plugin since activation.
    pub fn events_sent(&self) -> u64 {
        self.events_sent.load(Ordering::Relaxed)
    }

    /// Note events the node could not deliver: more than [`MAX_BLOCK_EVENTS`] in
    /// one block, or a [`NoteKind`] this host does not translate yet.
    ///
    /// Events for a plugin with no event input bus are *not* counted -- that is a
    /// configuration, not a loss.
    pub fn events_dropped(&self) -> u64 {
        self.events_dropped.load(Ordering::Relaxed)
    }
}

/// A VST3 plugin, running inside the audio graph.
///
/// # RT safety
/// [`process`](RtNode::process) allocates nothing. The scratch buffers, the event
/// list and the empty parameter list are built in
/// [`LoadedVst3Plugin::create_node`]; per block the node writes channel pointers
/// into arrays it already owns, builds a `ProcessData` on the stack, copies
/// samples, and calls the plugin. No locks, no syscalls, no logging.
///
/// # Drop
/// Dropping a node does **not** deactivate the plugin: it sends the audio
/// processor back to its [`LoadedVst3Plugin`], which deactivates on the main
/// thread. See the module docs. Consequently a node may safely be dropped by the
/// `basedrop` collector thread, which is exactly what the graph does.
pub struct Vst3Node {
    parameter_bank: Arc<mydaw_plugin_params::ParameterBank>,
    output_parameters: ComWrapper<NoParameterChanges>,
    process_mode: i32,
    /// `None` only after drop has sent it home.
    processor: Option<ComPtr<IAudioProcessor>>,
    /// Stereo scratch. VST3 wants distinct input and output buffers, and the
    /// graph bus is a single in-place buffer, so the node owns both sides.
    input: [Vec<f32>; CHANNELS],
    output: [Vec<f32>; CHANNELS],
    /// Channel-pointer arrays, rewritten per block. Owned so that building
    /// `AudioBusBuffers` never allocates.
    input_channels: [*mut f32; CHANNELS],
    output_channels: [*mut f32; CHANNELS],
    /// This block's notes, in VST3's wire format. See [`HostEventList`].
    events: ComWrapper<HostEventList>,
    /// Handed to every `process` call; see [`NoParameterChanges`].
    parameters: ComWrapper<NoParameterChanges>,
    layout: Vst3BusLayout,
    max_block: usize,
    stats: Arc<Vst3NodeStats>,
    retire_tx: Sender<ComPtr<IAudioProcessor>>,
}

// SAFETY: the channel-pointer arrays hold pointers into this node's own `input`
// and `output` vectors, whose heap allocations do not move when the node moves.
// They are rewritten at the top of every `process` before being read, so they are
// never stale, and no other thread ever sees them: the graph moves the node to
// the audio thread as a whole and it stays there. Every other field is `Send`
// (`ComPtr<IAudioProcessor>` because the interface is declared `Send + Sync`;
// `ComWrapper` for the two host objects because they assert it -- see
// [`HostEventList`]).
unsafe impl Send for Vst3Node {}

impl Vst3Node {
    fn new(
        processor: ComPtr<IAudioProcessor>,
        max_block: usize,
        layout: Vst3BusLayout,
        retire_tx: Sender<ComPtr<IAudioProcessor>>,
        parameter_bank: Arc<mydaw_plugin_params::ParameterBank>,
    ) -> Self {
        Self {
            parameter_bank,
            output_parameters: NoParameterChanges::new(),
            process_mode: if std::env::var_os("MYDAW_RENDER_WORKER").is_some() {
                ProcessModes_::kOffline as i32
            } else {
                ProcessModes_::kRealtime as i32
            },
            processor: Some(processor),
            input: [vec![0.0; max_block], vec![0.0; max_block]],
            output: [vec![0.0; max_block], vec![0.0; max_block]],
            input_channels: [std::ptr::null_mut(); CHANNELS],
            output_channels: [std::ptr::null_mut(); CHANNELS],
            events: HostEventList::new(),
            parameters: NoParameterChanges::new(),
            layout,
            max_block,
            stats: Arc::new(Vst3NodeStats::default()),
            retire_tx,
        }
    }

    /// Counters for this node. Clone the `Arc` to watch it from a non-RT thread.
    pub fn stats(&self) -> &Arc<Vst3NodeStats> {
        &self.stats
    }

    /// Largest block this node was activated for.
    pub fn max_block(&self) -> usize {
        self.max_block
    }

    /// The bus layout this node was built for.
    pub fn bus_layout(&self) -> Vst3BusLayout {
        self.layout
    }

    /// Whether this node forwards [`ProcessCtx::events`] to its plugin.
    pub fn delivers_notes(&self) -> bool {
        self.layout.has_note_input()
    }

    /// Translate this block's note events into VST3's wire format.
    ///
    /// Returns how many were dropped. RT-safe: see [`HostEventList`].
    fn fill_events(&self, events: &[mydaw_rt_core::NoteEvent], frames: usize) -> usize {
        self.events.reset();
        if !self.layout.has_note_input() || events.is_empty() {
            return 0;
        }

        // `frames >= 1` is guaranteed by the caller. VST3 requires every event
        // offset to be inside the block; clamping is defensive, since the engine
        // already guarantees it.
        let last_frame = (frames - 1) as i32;
        let mut dropped = 0;
        for event in events {
            let offset = (event.frame_offset as i32).min(last_frame);
            let wire = match event.kind {
                NoteKind::NoteOn { key, velocity } => {
                    note_event(offset, event.channel, key, velocity, true)
                }
                // Release velocity 0.0: MYDAW's note model has no release
                // velocity yet, and 0.0 is what hosts send when they have
                // nothing to say about it.
                NoteKind::NoteOff { key } => note_event(offset, event.channel, key, 0.0, false),
                // A `NoteKind` added after this code was written. Counted, not
                // guessed at: silently reinterpreting an unknown event is how a
                // host makes a plugin sound wrong in a way nobody can trace.
                #[allow(unreachable_patterns)]
                _ => {
                    dropped += 1;
                    continue;
                }
            };
            if !self.events.push(wire) {
                dropped += 1;
            }
        }
        dropped
    }
}

/// Build one VST3 note event.
///
/// `note_id` is `-1`, VST3's "no id": MYDAW does not track per-note ids, so a
/// note-off has to match by pitch, which is what `-1` asks a plugin to do.
fn note_event(offset: i32, channel: u8, key: u8, velocity: f32, on: bool) -> Event {
    // SAFETY: `Event` is a plain `#[repr(C)]` aggregate of scalars and a union of
    // such aggregates; an all-zero one is a valid (if meaningless) value, and
    // every field that matters is written below.
    let mut event: Event = unsafe { std::mem::zeroed() };
    event.busIndex = 0;
    event.sampleOffset = offset;
    event.ppqPosition = 0.0;
    event.flags = 0;
    event.r#type = if on {
        vst3::Steinberg::Vst::Event_::EventTypes_::kNoteOnEvent as u16
    } else {
        vst3::Steinberg::Vst::Event_::EventTypes_::kNoteOffEvent as u16
    };
    event.__field0 = if on {
        Event__type0 {
            noteOn: NoteOnEvent {
                channel: i16::from(channel),
                pitch: i16::from(key),
                tuning: 0.0,
                velocity,
                length: 0,
                noteId: -1,
            },
        }
    } else {
        Event__type0 {
            noteOff: NoteOffEvent {
                channel: i16::from(channel),
                pitch: i16::from(key),
                velocity,
                noteId: -1,
                tuning: 0.0,
            },
        }
    };
    event
}

impl RtNode for Vst3Node {
    fn process(&mut self, ctx: &mut ProcessCtx) {
        let frames = ctx.frames.min(self.max_block);
        if frames == 0 {
            return;
        }
        if ctx.frames > self.max_block {
            // Cannot grow the buffers here. Silence what we cannot process
            // rather than let stale samples through as a buzz.
            self.stats.truncated_blocks.fetch_add(1, Ordering::Relaxed);
            for channel in ctx.buffers.as_mut_slices() {
                channel[frames..].fill(0.0);
            }
        }

        let dropped = self.fill_events(ctx.events, frames);
        if dropped > 0 {
            self.stats
                .events_dropped
                .fetch_add(dropped as u64, Ordering::Relaxed);
        }
        let sent = self.events.len() as u64;
        self.parameters.fill(&self.parameter_bank);
        self.output_parameters.clear();

        {
            let bus = ctx.buffers.as_slices();
            for (scratch, source) in self.input.iter_mut().zip(bus) {
                scratch[..frames].copy_from_slice(&source[..frames]);
            }
        }
        // Written every block rather than cached: a cached pointer would make
        // this struct self-referential for no gain, and two stores are free.
        for (slot, channel) in self.input_channels.iter_mut().zip(self.input.iter_mut()) {
            *slot = channel.as_mut_ptr();
        }
        for (slot, channel) in self.output_channels.iter_mut().zip(self.output.iter_mut()) {
            *slot = channel.as_mut_ptr();
        }

        let Some(processor) = self.processor.as_ref() else {
            // Retired node still in a live graph: pass audio through untouched.
            return;
        };

        let mut input_bus = AudioBusBuffers {
            numChannels: CHANNELS as i32,
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: self.input_channels.as_mut_ptr(),
            },
        };
        let mut output_bus = AudioBusBuffers {
            numChannels: CHANNELS as i32,
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: self.output_channels.as_mut_ptr(),
            },
        };
        let mut context = process_context(ctx);

        // An instrument declares no audio input bus, so it must be handed none.
        // Passing it the bus "just in case" would contradict its own answer,
        // which is a host bug even when a lenient plugin tolerates it.
        let (inputs, num_inputs) = if self.layout.has_audio_input() {
            (&raw mut input_bus, 1)
        } else {
            (std::ptr::null_mut(), 0)
        };

        // `as_com_ref` is pointer arithmetic over a fixed interface list -- no
        // allocation, no refcount traffic, and the wrappers outlive the call.
        let events_ptr = self
            .events
            .as_com_ref::<IEventList>()
            .map_or(std::ptr::null_mut(), |list| list.as_ptr());
        let parameters_ptr = self
            .parameters
            .as_com_ref::<vst3::Steinberg::Vst::IParameterChanges>()
            .map_or(std::ptr::null_mut(), |changes| changes.as_ptr());

        let mut data = ProcessData {
            processMode: self.process_mode,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
            numSamples: frames as i32,
            numInputs: num_inputs,
            numOutputs: 1,
            inputs,
            outputs: &raw mut output_bus,
            inputParameterChanges: parameters_ptr,
            // Output parameter changes are discarded: MYDAW records no
            // automation yet, and a null here is what the specification expects
            // from a host that does not.
            outputParameterChanges: self
                .output_parameters
                .as_com_ref::<vst3::Steinberg::Vst::IParameterChanges>()
                .map_or(std::ptr::null_mut(), |c| c.as_ptr()),
            inputEvents: events_ptr,
            outputEvents: std::ptr::null_mut(),
            processContext: &raw mut context,
        };

        // SAFETY: the plugin is active and processing (`create_node` did both and
        // nothing has deactivated it -- the node holds the only handle). Every
        // pointer in `data` refers to a live local or to this node's own buffers,
        // each channel array has `CHANNELS` entries of at least `frames` samples,
        // and `numSamples <= maxSamplesPerBlock` from `setupProcessing`.
        let result = unsafe { processor.process(&mut data) };
        if result == kResultOk {
            self.parameters.acknowledge(&self.parameter_bank);
            self.output_parameters.reflect(&self.parameter_bank);
        }

        self.stats.process_calls.fetch_add(1, Ordering::Relaxed);
        if sent > 0 {
            self.stats.events_sent.fetch_add(sent, Ordering::Relaxed);
        }
        if result == kResultOk {
            let bus = ctx.buffers.as_mut_slices();
            for (destination, processed) in bus.into_iter().zip(self.output.iter()) {
                destination[..frames].copy_from_slice(&processed[..frames]);
            }
        } else {
            // The plugin failed: its output buffers hold nothing trustworthy.
            // Leave the bus as it was -- i.e. bypass -- and count it.
            self.stats.process_errors.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Build this block's `ProcessContext`.
///
/// Only the fields MYDAW actually knows are marked valid. A host that sets a
/// validity flag for a field it left at zero is worse than one that sets none:
/// a plugin syncing to the "bar position" would sit at bar 1 forever.
fn process_context(ctx: &ProcessCtx) -> ProcessContext {
    // SAFETY: `ProcessContext` is a plain `#[repr(C)]` aggregate of scalars and
    // one small struct; zeroing it is a valid starting value.
    let mut context: ProcessContext = unsafe { std::mem::zeroed() };
    context.sampleRate = ctx.sample_rate;
    context.projectTimeSamples = ctx.transport.sample_pos as i64;
    context.continousTimeSamples = ctx.transport.sample_pos as i64;
    context.tempo = ctx.transport.bpm;
    context.timeSigNumerator = 4;
    context.timeSigDenominator = 4;
    context.state = ProcessContext_::StatesAndFlags_::kTempoValid as u32
        | ProcessContext_::StatesAndFlags_::kContTimeValid as u32
        | ProcessContext_::StatesAndFlags_::kTimeSigValid as u32;
    if ctx.transport.playing {
        context.state |= ProcessContext_::StatesAndFlags_::kPlaying as u32;
    }
    context
}

impl std::fmt::Debug for Vst3Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vst3Node")
            .field("live", &self.processor.is_some())
            .field("max_block", &self.max_block)
            .field("layout", &self.layout)
            .field("stats", &self.stats)
            .finish()
    }
}

impl Drop for Vst3Node {
    fn drop(&mut self) {
        let Some(processor) = self.processor.take() else {
            return;
        };
        // Runs on a non-RT thread: either the graph builder (an install that was
        // rejected) or the `basedrop` collector. Never inside the audio callback.
        if self.retire_tx.send(processor).is_err() {
            // The owning `LoadedVst3Plugin` is already gone, which means it has
            // already leaked the module rather than unload it under this handle.
            // Releasing here is therefore safe, and is what dropping the
            // `ComPtr` the send gave back does.
            log::warn!(
                "VST3 node retired after its plugin owner was dropped; the plugin module leaks"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo_effect() -> Vst3BusLayout {
        Vst3BusLayout {
            audio_input_buses: 1,
            audio_output_buses: 1,
            main_input_channels: 2,
            main_output_channels: 2,
            event_input_buses: 0,
            event_output_buses: 0,
        }
    }

    #[test]
    fn a_stereo_layout_is_accepted_and_anything_else_is_not() {
        let stereo = stereo_effect();
        assert!(stereo.is_stereo_effect());
        assert!(stereo.is_supported());
        assert!(stereo.has_audio_input());
        assert!(!stereo.has_note_input());

        let mono = Vst3BusLayout {
            main_output_channels: 1,
            ..stereo
        };
        assert!(!mono.is_stereo_effect());
        assert!(!mono.is_supported(), "a mono output is still not drivable");

        let no_output = Vst3BusLayout {
            audio_output_buses: 0,
            main_output_channels: 0,
            ..stereo
        };
        assert!(!no_output.is_supported());
    }

    #[test]
    fn an_instrument_layout_is_supported_and_is_not_an_effect() {
        let instrument = Vst3BusLayout {
            audio_input_buses: 0,
            audio_output_buses: 1,
            main_input_channels: 0,
            main_output_channels: 2,
            event_input_buses: 1,
            event_output_buses: 0,
        };
        assert!(!instrument.is_stereo_effect());
        assert!(instrument.is_instrument());
        assert!(instrument.is_supported());
        assert!(
            !instrument.has_audio_input(),
            "it must not be handed the bus"
        );
        assert!(instrument.has_note_input());

        // A generator with no event bus is still an instrument as far as the
        // graph is concerned; it just never receives events.
        let generator = Vst3BusLayout {
            event_input_buses: 0,
            ..instrument
        };
        assert!(generator.is_instrument());
        assert!(!generator.has_note_input());

        let mono = Vst3BusLayout {
            main_output_channels: 1,
            ..instrument
        };
        assert!(!mono.is_supported());
    }

    #[test]
    fn a_note_on_carries_the_key_velocity_and_offset_the_engine_gave_it() {
        let event = note_event(37, 3, 69, 0.75, true);
        assert_eq!(event.sampleOffset, 37);
        assert_eq!(
            u32::from(event.r#type),
            vst3::Steinberg::Vst::Event_::EventTypes_::kNoteOnEvent as u32
        );
        // SAFETY: the event was built as a note-on, so this union arm is the
        // live one.
        unsafe {
            assert_eq!(event.__field0.noteOn.pitch, 69);
            assert_eq!(event.__field0.noteOn.channel, 3);
            assert!((event.__field0.noteOn.velocity - 0.75).abs() < f32::EPSILON);
            assert_eq!(
                event.__field0.noteOn.noteId, -1,
                "MYDAW tracks no note ids, so a note-off must match by pitch"
            );
        }
    }

    #[test]
    fn a_note_off_uses_the_note_off_arm_of_the_union() {
        // The two structs have *different field orders*, so writing the wrong
        // arm would put the pitch where the velocity belongs -- silently.
        let event = note_event(0, 0, 42, 0.0, false);
        assert_eq!(
            u32::from(event.r#type),
            vst3::Steinberg::Vst::Event_::EventTypes_::kNoteOffEvent as u32
        );
        // SAFETY: the event was built as a note-off.
        unsafe {
            assert_eq!(event.__field0.noteOff.pitch, 42);
            assert_eq!(event.__field0.noteOff.noteId, -1);
        }
    }

    #[test]
    fn the_process_context_only_marks_valid_what_the_host_actually_knows() {
        use mydaw_rt_core::{AudioBuffers, TransportSnapshot};

        let mut buffers = AudioBuffers::new(64);
        let transport = TransportSnapshot {
            sample_pos: 4_800,
            playing: true,
            bpm: 128.0,
            ..TransportSnapshot::default()
        };
        let ctx = ProcessCtx::new(&mut buffers, 48_000.0, 64, transport);
        let context = process_context(&ctx);

        assert!((context.sampleRate - 48_000.0).abs() < f64::EPSILON);
        assert_eq!(context.projectTimeSamples, 4_800);
        assert!((context.tempo - 128.0).abs() < f64::EPSILON);
        assert_ne!(
            context.state & ProcessContext_::StatesAndFlags_::kPlaying as u32,
            0
        );
        assert_ne!(
            context.state & ProcessContext_::StatesAndFlags_::kTempoValid as u32,
            0
        );
        // Never claimed, because MYDAW has no bar position or SMPTE to give.
        assert_eq!(
            context.state & ProcessContext_::StatesAndFlags_::kBarPositionValid as u32,
            0,
            "a validity flag for a field left at zero would strand a plugin at bar 1"
        );
        assert_eq!(
            context.state & ProcessContext_::StatesAndFlags_::kSmpteValid as u32,
            0
        );
    }

    #[test]
    fn a_stopped_transport_is_reported_as_stopped() {
        use mydaw_rt_core::{AudioBuffers, TransportSnapshot};

        let mut buffers = AudioBuffers::new(16);
        let ctx = ProcessCtx::new(&mut buffers, 44_100.0, 16, TransportSnapshot::default());
        let context = process_context(&ctx);
        assert_eq!(
            context.state & ProcessContext_::StatesAndFlags_::kPlaying as u32,
            0
        );
    }

    #[test]
    fn loading_a_path_that_does_not_exist_is_an_error_not_a_panic() {
        let path = std::env::temp_dir().join("mydaw-definitely-not-a-module-91cd.vst3");
        assert!(!path.exists());
        let err = LoadedVst3Plugin::load(&path, ClassId([0; 16])).unwrap_err();
        assert!(matches!(err, Vst3HostError::Module(_)), "got {err:?}");
    }

    #[test]
    fn the_retire_timeout_is_several_collector_sweeps() {
        // The collector sweeps every 250 ms, so anything shorter would make an
        // ordinary shutdown leak the module.
        assert!(RETIRE_TIMEOUT >= Duration::from_millis(500));
    }
}
