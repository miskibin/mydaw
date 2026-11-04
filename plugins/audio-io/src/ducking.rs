//! Keep Windows from turning everything else down while the input is open.
//!
//! Windows treats a capture stream on the default *communications* microphone
//! as a call and, by default, lowers every other sound by 80 % ("ducking") --
//! the beat goes muffled the moment REC is armed. A recorder is not a call, so
//! the capture session opts out before its stream opens, the documented way
//! (`IAudioSessionControl2::SetDuckingPreference`). Best effort: a failure only
//! means the system setting applies, so it is logged, never reported.

/// Opt this process's default session on capture endpoint `endpoint_id` out of
/// ducking. **Non-RT.** No-op off Windows.
#[cfg(windows)]
pub(crate) fn opt_out_of_ducking(endpoint_id: &str) {
    // SAFETY: plain COM calls on interfaces this function owns, on a thread
    // that initialises COM for itself (and undoes only what it did).
    if let Err(err) = unsafe { opt_out(endpoint_id) } {
        log::debug!("could not opt the input out of Windows ducking: {err}");
    }
}

#[cfg(not(windows))]
pub(crate) fn opt_out_of_ducking(_endpoint_id: &str) {}

#[cfg(windows)]
unsafe fn opt_out(endpoint_id: &str) -> windows::core::Result<()> {
    use windows::core::{Interface, HSTRING};
    use windows::Win32::Media::Audio::{
        IAudioSessionControl2, IAudioSessionManager, IMMDeviceEnumerator, MMDeviceEnumerator,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
    };

    struct Com(bool);
    impl Drop for Com {
        fn drop(&mut self) {
            if self.0 {
                // SAFETY: balances the successful CoInitializeEx below.
                unsafe { CoUninitialize() };
            }
        }
    }
    // S_OK / S_FALSE must be balanced; RPC_E_CHANGED_MODE means the thread is
    // already an STA, which works just as well and is not ours to undo.
    let _com = Com(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok());

    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
    let device = unsafe { enumerator.GetDevice(&HSTRING::from(endpoint_id))? };
    let manager: IAudioSessionManager = unsafe { device.Activate(CLSCTX_ALL, None)? };
    // `None` = the process's default session, the one cpal's stream joins.
    let control = unsafe { manager.GetAudioSessionControl(None, 0)? };
    let control: IAudioSessionControl2 = control.cast()?;
    unsafe { control.SetDuckingPreference(true) }
}
