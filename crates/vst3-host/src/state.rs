//! The state blob WAVE stores for a VST3 effect.
//!
//! A complete VST3 preset is two streams: the component's (`IComponent::
//! getState`, what the processor needs) and the controller's
//! (`IEditController::getState`, editor-only settings). They travel as one
//! opaque blob in a small versioned envelope:
//!
//! ```text
//! 16 bytes magic | u32 LE component length | u32 LE controller length | component | controller
//! ```
//!
//! A blob without a known magic is a bare component state (what the old host
//! saved before it stored controller state), and is loaded as such. The old
//! host's envelope (magic kept byte-for-byte as `LEGACY_MAGIC`, spelled in hex
//! because it carries the app's former name) is read too, so its project files
//! keep their plugin settings.

use crate::host::MAX_STATE_BYTES;
use crate::Vst3Error;

const MAGIC: &[u8; 16] = b"WAVE-VST3-STATE1";
const LEGACY_MAGIC: &[u8; 16] = b"\x4d\x59\x44\x41\x57-VST-STATE1";
const HEADER: usize = 24;

pub(crate) fn encode(component: &[u8], controller: &[u8]) -> Result<Vec<u8>, Vst3Error> {
    let total = component.len().saturating_add(controller.len()).saturating_add(HEADER);
    if total > MAX_STATE_BYTES {
        return Err(Vst3Error::InvalidState(format!("the plugin state is {total} bytes, past the {MAX_STATE_BYTES} byte limit")));
    }
    let mut bytes = Vec::with_capacity(total);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&(component.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&(controller.len() as u32).to_le_bytes());
    bytes.extend_from_slice(component);
    bytes.extend_from_slice(controller);
    Ok(bytes)
}

/// Split a stored blob into `(component, controller)` streams.
pub(crate) fn decode(bytes: &[u8]) -> Result<(&[u8], &[u8]), Vst3Error> {
    if bytes.len() > MAX_STATE_BYTES {
        return Err(Vst3Error::InvalidState(format!("the stored state is {} bytes, past the {MAX_STATE_BYTES} byte limit", bytes.len())));
    }
    if !(bytes.starts_with(MAGIC) || bytes.starts_with(LEGACY_MAGIC)) {
        return Ok((bytes, &[]));
    }
    let header = bytes.get(16..HEADER).ok_or_else(|| Vst3Error::InvalidState("truncated state header".to_owned()))?;
    let component = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    let controller = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
    if component.saturating_add(controller).saturating_add(HEADER) != bytes.len() {
        return Err(Vst3Error::InvalidState("the state lengths do not match its size".to_owned()));
    }
    Ok((&bytes[HEADER..HEADER + component], &bytes[HEADER + component..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_streams_round_trip_through_the_envelope() {
        let encoded = encode(b"processor", b"editor").unwrap();
        assert!(encoded.starts_with(MAGIC));
        assert_eq!(decode(&encoded).unwrap(), (&b"processor"[..], &b"editor"[..]));
        let empty = encode(b"", b"").unwrap();
        assert_eq!(decode(&empty).unwrap(), (&b""[..], &b""[..]));
    }

    #[test]
    fn a_bare_blob_is_a_component_state_and_the_old_envelope_still_reads() {
        assert_eq!(decode(b"legacy").unwrap(), (&b"legacy"[..], &b""[..]));
        let mut old = LEGACY_MAGIC.to_vec();
        old.extend_from_slice(&3_u32.to_le_bytes());
        old.extend_from_slice(&2_u32.to_le_bytes());
        old.extend_from_slice(b"abcde");
        assert_eq!(decode(&old).unwrap(), (&b"abc"[..], &b"de"[..]));
    }

    #[test]
    fn a_truncated_or_inconsistent_envelope_is_an_error_not_a_panic() {
        let encoded = encode(b"processor", b"editor").unwrap();
        assert!(decode(&encoded[..encoded.len() - 1]).is_err());
        assert!(decode(&encoded[..20]).is_err());
        let mut lying = encoded.clone();
        lying[16] = 0xFF;
        assert!(decode(&lying).is_err());
    }
}
