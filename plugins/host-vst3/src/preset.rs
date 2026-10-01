//! Versioned host envelope. Legacy projects contain only raw component bytes.
const MAGIC: &[u8; 16] = b"MYDAW-VST-STATE1";
pub(crate) fn encode(component: &[u8], controller: &[u8]) -> Result<Vec<u8>, String> {
    if component
        .len()
        .saturating_add(controller.len())
        .saturating_add(24)
        > crate::host::MAX_STATE_BYTES
    {
        return Err("combined plugin state exceeds 64 MiB".into());
    }
    let mut bytes = Vec::with_capacity(24 + component.len() + controller.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&(component.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&(controller.len() as u32).to_le_bytes());
    bytes.extend_from_slice(component);
    bytes.extend_from_slice(controller);
    Ok(bytes)
}
pub(crate) fn decode(bytes: &[u8]) -> Result<(&[u8], &[u8]), String> {
    if !bytes.starts_with(MAGIC) {
        return Ok((bytes, &[]));
    }
    let lengths = bytes.get(16..24).ok_or("truncated plugin state header")?;
    let a = u32::from_le_bytes(
        lengths[..4]
            .try_into()
            .map_err(|_| "invalid state header")?,
    ) as usize;
    let b = u32::from_le_bytes(
        lengths[4..]
            .try_into()
            .map_err(|_| "invalid state header")?,
    ) as usize;
    if a.saturating_add(b).saturating_add(24) != bytes.len() {
        return Err("invalid plugin state lengths".into());
    }
    Ok((&bytes[24..24 + a], &bytes[24 + a..]))
}
#[cfg(test)]
mod tests {
    #[test]
    fn state_envelope_and_legacy_are_distinct() {
        let encoded = super::encode(b"processor", b"editor").unwrap();
        assert_eq!(
            super::decode(&encoded).unwrap(),
            (&b"processor"[..], &b"editor"[..])
        );
        assert_eq!(
            super::decode(b"legacy").unwrap(),
            (&b"legacy"[..], &b""[..])
        );
        assert!(super::decode(&encoded[..encoded.len() - 1]).is_err());
    }
}
