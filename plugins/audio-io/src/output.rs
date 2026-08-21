//! Output host and buffer-size request: WASAPI by default, ASIO when asked.
//!
//! cpal's WASAPI host is always shared-mode. ASIO is a separate host, compiled
//! only with the `asio` feature (LLVM/Clang + the Steinberg SDK). Selecting
//! ASIO without that host, or without a driver, must still make sound: we fall
//! back to WASAPI and put the reason on [`crate::StreamInfo::note`].

use cpal::BufferSize;

/// Which output API the open stream is actually using.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioBackend {
    /// Windows shared WASAPI (or the platform default host off Windows).
    Wasapi,
    /// An ASIO driver. Exclusive: other apps typically lose the device.
    Asio,
}

impl AudioBackend {
    /// Wire name the UI and `audio.json` persist.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wasapi => "wasapi",
            Self::Asio => "asio",
        }
    }
}

/// What the next [`crate::AudioIoService::start`] should ask the device for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OutputRequest {
    /// Prefer an ASIO host. Falls back to WASAPI when ASIO is missing.
    pub prefer_asio: bool,
    /// Callback / ring-buffer size in frames. `None` = the host's default.
    pub buffer_frames: Option<u32>,
}

/// Whether this build linked cpal's ASIO host.
pub fn asio_compiled() -> bool {
    cfg!(all(windows, feature = "asio"))
}

/// cpal buffer setting for an optional frame count. `0` and `None` are the
/// host default, not a zero-length callback.
pub fn buffer_size_of(frames: Option<u32>) -> BufferSize {
    match frames {
        Some(n) if n > 0 => BufferSize::Fixed(n),
        _ => BufferSize::Default,
    }
}

/// Sentence explaining an ASIO miss, or empty when there is nothing to explain.
pub fn fallback_note(prefer_asio: bool, opened: AudioBackend) -> String {
    if !prefer_asio || opened == AudioBackend::Asio {
        return String::new();
    }
    if asio_compiled() {
        "ASIO was requested, but no ASIO device opened. Using WASAPI so you still hear sound. Install your interface's ASIO driver, or ASIO4ALL."
            .to_owned()
    } else {
        "ASIO was requested, but this WAVE build has no ASIO host. Using WASAPI — other apps keep playing, and the buffer size still applies."
            .to_owned()
    }
}

/// Host to open, the backend it represents, and a fallback sentence if any.
pub struct HostChoice {
    pub host: cpal::Host,
    pub backend: AudioBackend,
    pub note: String,
}

/// Pick WASAPI (default host) or ASIO. Never fails: a missing ASIO host is a
/// WASAPI stream with a note, not silence.
pub fn choose_host(prefer_asio: bool) -> HostChoice {
    if prefer_asio {
        if let Some(host) = try_asio_host() {
            return HostChoice {
                host,
                backend: AudioBackend::Asio,
                note: String::new(),
            };
        }
        return HostChoice {
            host: cpal::default_host(),
            backend: AudioBackend::Wasapi,
            note: fallback_note(true, AudioBackend::Wasapi),
        };
    }
    HostChoice {
        host: cpal::default_host(),
        backend: AudioBackend::Wasapi,
        note: String::new(),
    }
}

fn try_asio_host() -> Option<cpal::Host> {
    #[cfg(all(windows, feature = "asio"))]
    {
        use cpal::traits::HostTrait;
        let host = cpal::host_from_id(cpal::HostId::Asio).ok()?;
        let has_output = host.default_output_device().is_some()
            || host
                .output_devices()
                .ok()
                .and_then(|mut devices| devices.next())
                .is_some();
        has_output.then_some(host)
    }
    #[cfg(not(all(windows, feature = "asio")))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_default_is_cpal_default() {
        assert_eq!(buffer_size_of(None), BufferSize::Default);
        assert_eq!(buffer_size_of(Some(0)), BufferSize::Default);
        assert_eq!(buffer_size_of(Some(256)), BufferSize::Fixed(256));
    }

    #[test]
    fn a_wasapi_choice_has_no_note() {
        assert!(fallback_note(false, AudioBackend::Wasapi).is_empty());
        assert!(fallback_note(true, AudioBackend::Asio).is_empty());
    }

    #[test]
    fn an_asio_miss_explains_wasapi() {
        let note = fallback_note(true, AudioBackend::Wasapi);
        assert!(note.contains("WASAPI"), "{note}");
        assert!(
            note.contains("ASIO"),
            "the musician asked for ASIO and must be told why they did not get it: {note}"
        );
    }

    #[test]
    fn backend_wire_names_are_stable() {
        assert_eq!(AudioBackend::Wasapi.as_str(), "wasapi");
        assert_eq!(AudioBackend::Asio.as_str(), "asio");
    }

    #[test]
    fn choosing_wasapi_opens_the_default_host() {
        let choice = choose_host(false);
        assert_eq!(choice.backend, AudioBackend::Wasapi);
        assert!(choice.note.is_empty());
    }

    #[test]
    fn preferring_asio_either_opens_it_or_explains_the_fallback() {
        let choice = choose_host(true);
        if choice.backend == AudioBackend::Asio {
            assert!(choice.note.is_empty());
        } else {
            assert_eq!(choice.backend, AudioBackend::Wasapi);
            assert!(!choice.note.is_empty());
        }
    }
}
