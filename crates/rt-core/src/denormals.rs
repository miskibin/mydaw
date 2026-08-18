//! Denormal (subnormal) float handling for the audio thread.
//!
//! Denormals are catastrophically slow on some x86 microarchitectures: a filter
//! decaying into the denormal range can multiply the cost of a block by an order
//! of magnitude and cause dropouts. Audio code therefore enables
//! **FTZ** (flush-to-zero, denormal *results* become 0) and
//! **DAZ** (denormals-are-zero, denormal *inputs* are treated as 0).
//!
//! The MXCSR register is **per-thread**, so [`disable_denormals`] must be called
//! once from *inside* the audio thread, before the first `process()` call.

/// Flush-to-zero bit in MXCSR.
#[cfg(target_arch = "x86_64")]
const MXCSR_FTZ: u32 = 0x8000;
/// Denormals-are-zero bit in MXCSR.
#[cfg(target_arch = "x86_64")]
const MXCSR_DAZ: u32 = 0x0040;

/// Enable FTZ + DAZ for the **calling thread**.
///
/// Call once from the audio thread before processing starts. Safe to call more
/// than once. On non-x86_64 targets this is a no-op (aarch64 has FTZ in FPCR,
/// wired up when we get there; ARM handles denormals far more gracefully).
#[cfg(target_arch = "x86_64")]
pub fn disable_denormals() {
    use core::arch::asm;
    // We set the bits by hand rather than via `_MM_SET_FLUSH_ZERO_MODE`, which
    // is deprecated in favour of inline assembly, and DAZ has no intrinsic at all.
    let mut csr: u32 = 0;
    unsafe {
        asm!("stmxcsr [{}]", in(reg) &mut csr, options(nostack, preserves_flags));
        csr |= MXCSR_FTZ | MXCSR_DAZ;
        asm!("ldmxcsr [{}]", in(reg) &csr, options(nostack, preserves_flags));
    }
}

/// Enable FTZ + DAZ for the calling thread (no-op on this architecture).
#[cfg(not(target_arch = "x86_64"))]
pub fn disable_denormals() {}

/// Read back the raw MXCSR value. Diagnostics only (used by tests and by the
/// audio-io plugin to assert FTZ/DAZ really took effect on the audio thread).
#[cfg(target_arch = "x86_64")]
pub fn mxcsr() -> u32 {
    use core::arch::asm;
    let mut csr: u32 = 0;
    unsafe {
        asm!("stmxcsr [{}]", in(reg) &mut csr, options(nostack, preserves_flags));
    }
    csr
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::*;

    #[test]
    fn sets_ftz_and_daz_bits() {
        disable_denormals();
        let csr = mxcsr();
        assert_eq!(
            csr & MXCSR_FTZ,
            MXCSR_FTZ,
            "FTZ not set (mxcsr=0x{csr:04x})"
        );
        assert_eq!(
            csr & MXCSR_DAZ,
            MXCSR_DAZ,
            "DAZ not set (mxcsr=0x{csr:04x})"
        );
    }
}
