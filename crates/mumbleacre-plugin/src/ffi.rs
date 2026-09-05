#![allow(non_snake_case, non_camel_case_types, dead_code)]
#![cfg_attr(not(windows), allow(unused_variables))]
use std::ffi::{CStr, CString, c_char, c_void};
use std::os::raw::c_int;
use std::sync::Mutex;
pub type mumble_plugin_id_t = u32;
pub type mumble_userid_t = u32;
pub type mumble_connection_t = i32;
pub type mumble_channelid_t = i32;
pub type mumble_transmission_mode_t = c_int;
pub type mumble_error_t = c_int;

pub const MUMBLE_STATUS_OK: mumble_error_t = 0;
pub const MUMBLE_EC_GENERIC_ERROR: mumble_error_t = -1;

#[repr(C)]
#[derive(Copy, Clone)]
pub struct mumble_version_t {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

#[repr(C)]
pub struct MumbleStringWrapper {
    pub data: *const c_char,
    pub size: usize,
    pub needs_release: bool,
}

// ---------------------------------------------------------------------------
// Typed prefix of MumbleAPI_v_1_0_x through `playSample` (index 37).
// Layout must exactly match MumblePlugin.h. Every slot is pointer-sized; API
// 1.2 changes only the playSample signature, not the preceding offsets. This
// DLL requests API 1.0, so its API 1.0 signature is the one stored below.
// ---------------------------------------------------------------------------

type MumbleFnFreeMemory =
    unsafe extern "C" fn(caller_id: mumble_plugin_id_t, ptr: *const c_void) -> mumble_error_t;

type MumbleFnGetUserName = unsafe extern "C" fn(
    caller_id: mumble_plugin_id_t,
    connection: mumble_connection_t,
    user_id: mumble_userid_t,
    user_name: *mut *const c_char,
) -> mumble_error_t;

type MumbleFnGetChannelName = unsafe extern "C" fn(
    caller_id: mumble_plugin_id_t,
    connection: mumble_connection_t,
    channel_id: mumble_channelid_t,
    channel_name: *mut *const c_char,
) -> mumble_error_t;

type MumbleFnGetActiveServerConnection = unsafe extern "C" fn(
    caller_id: mumble_plugin_id_t,
    connection: *mut mumble_connection_t,
) -> mumble_error_t;

type MumbleFnIsConnectionSynchronized = unsafe extern "C" fn(
    caller_id: mumble_plugin_id_t,
    connection: mumble_connection_t,
    synchronized: *mut bool,
) -> mumble_error_t;

type MumbleFnGetLocalUserId = unsafe extern "C" fn(
    caller_id: mumble_plugin_id_t,
    connection: mumble_connection_t,
    user_id: *mut mumble_userid_t,
) -> mumble_error_t;

type MumbleFnGetChannelOfUser = unsafe extern "C" fn(
    caller_id: mumble_plugin_id_t,
    connection: mumble_connection_t,
    user_id: mumble_userid_t,
    channel_id: *mut mumble_channelid_t,
) -> mumble_error_t;

type MumbleFnGetUsersInChannel = unsafe extern "C" fn(
    caller_id: mumble_plugin_id_t,
    connection: mumble_connection_t,
    channel_id: mumble_channelid_t,
    users: *mut *mut mumble_userid_t,
    user_count: *mut usize,
) -> mumble_error_t;

type MumbleFnGetLocalUserTransmissionMode = unsafe extern "C" fn(
    caller_id: mumble_plugin_id_t,
    transmission_mode: *mut mumble_transmission_mode_t,
) -> mumble_error_t;

type MumbleFnIsUserLocallyMuted = unsafe extern "C" fn(
    caller_id: mumble_plugin_id_t,
    connection: mumble_connection_t,
    user_id: mumble_userid_t,
    muted: *mut bool,
) -> mumble_error_t;

type MumbleFnGetServerHash = unsafe extern "C" fn(
    caller_id: mumble_plugin_id_t,
    connection: mumble_connection_t,
    server_hash: *mut *const c_char,
) -> mumble_error_t;

type MumbleFnRequestLocalUserTransmissionMode = unsafe extern "C" fn(
    caller_id: mumble_plugin_id_t,
    transmission_mode: mumble_transmission_mode_t,
) -> mumble_error_t;

type MumbleFnRequestLocalMute = unsafe extern "C" fn(
    caller_id: mumble_plugin_id_t,
    connection: mumble_connection_t,
    user_id: mumble_userid_t,
    muted: bool,
) -> mumble_error_t;

type MumbleFnSendData = unsafe extern "C" fn(
    caller_id: mumble_plugin_id_t,
    connection: mumble_connection_t,
    users: *const mumble_userid_t,
    user_count: usize,
    data: *const u8,
    data_length: usize,
    data_id: *const c_char,
) -> mumble_error_t;

type MumbleFnLog =
    unsafe extern "C" fn(caller_id: mumble_plugin_id_t, message: *const c_char) -> mumble_error_t;

/// API 1.0 signature (no volume). Mumble's `PARAM_v1_2` only adds volume on 1.2+.
type MumbleFnPlaySample = unsafe extern "C" fn(
    caller_id: mumble_plugin_id_t,
    sample_path: *const c_char,
) -> mumble_error_t;

/// Opaque placeholder for function pointers we don't call.
type OpaqueFnPtr = *const ();

/// Mumble ships this symbol with a historical typo (`Ovewrite`).
type MumbleFnRequestMicrophoneActivationOverwrite =
    unsafe extern "C" fn(caller_id: mumble_plugin_id_t, activate: bool) -> mumble_error_t;

/// Subset of `MumbleAPI_v10_x0` with exact field ordering from MumblePlugin.h:
///   0: freeMemory
///   1: getActiveServerConnection
///   2: isConnectionSynchronized
///   3: getLocalUserID
///   4: getUserName
///   5: getChannelName
///   8: getChannelOfUser
///   9: getUsersInChannel
///  10: getLocalUserTransmissionMode
///  11: isUserLocallyMuted
///  15: getServerHash
///  18: requestLocalUserTransmissionMode
///  20: requestMicrophoneActivationOvewrite (Mumble typo)
///  21: requestLocalMute
///  35: sendData
///  36: log
///  37: playSample
#[repr(C)]
struct MumbleAPI {
    free_memory: MumbleFnFreeMemory, // index 0
    get_active_server_connection: MumbleFnGetActiveServerConnection, // index 1
    is_connection_synchronized: MumbleFnIsConnectionSynchronized, // index 2
    get_local_user_id: MumbleFnGetLocalUserId, // index 3
    get_user_name: MumbleFnGetUserName, // index 4
    get_channel_name: MumbleFnGetChannelName, // index 5
    _get_all_users: OpaqueFnPtr,     // index 6
    _get_all_channels: OpaqueFnPtr,  // index 7
    get_channel_of_user: MumbleFnGetChannelOfUser, // index 8
    get_users_in_channel: MumbleFnGetUsersInChannel, // index 9
    get_local_user_transmission_mode: MumbleFnGetLocalUserTransmissionMode, // index 10
    is_user_locally_muted: MumbleFnIsUserLocallyMuted, // index 11
    _api_fields_12_to_14: [OpaqueFnPtr; 3], // indices 12..=14
    get_server_hash: MumbleFnGetServerHash, // index 15
    _api_fields_16_to_17: [OpaqueFnPtr; 2], // indices 16..=17
    request_local_user_transmission_mode: MumbleFnRequestLocalUserTransmissionMode, // index 18
    _request_user_move: OpaqueFnPtr, // index 19
    request_microphone_activation_overwrite: MumbleFnRequestMicrophoneActivationOverwrite, // index 20
    request_local_mute: MumbleFnRequestLocalMute, // index 21
    _api_fields_22_to_34: [OpaqueFnPtr; 13],      // indices 22..=34
    send_data: MumbleFnSendData,                  // index 35
    log: MumbleFnLog,                             // index 36
    play_sample: MumbleFnPlaySample,              // index 37
}

#[derive(Clone, Copy)]
struct Api {
    id: u32,
    free: MumbleFnFreeMemory,
    connection: MumbleFnGetActiveServerConnection,
    synchronized: MumbleFnIsConnectionSynchronized,
    local: MumbleFnGetLocalUserId,
    channel: MumbleFnGetChannelOfUser,
    users: MumbleFnGetUsersInChannel,
    channel_name: MumbleFnGetChannelName,
    server_hash: MumbleFnGetServerHash,
    send: MumbleFnSendData,
    mic: MumbleFnRequestMicrophoneActivationOverwrite,
    log: MumbleFnLog,
    play: MumbleFnPlaySample,
}
static API: Mutex<Option<Api>> = Mutex::new(None);
fn api() -> Option<Api> {
    *API.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Context {
    pub connection: i32,
    pub local: u32,
    pub channel: i32,
    pub users: Vec<u32>,
    pub server: String,
    pub name: String,
}
pub fn context() -> Option<Context> {
    let a = api()?;
    let mut connection = 0;
    let mut synchronized = false;
    let mut local = 0;
    let mut channel = 0;
    unsafe {
        if (a.connection)(a.id, &mut connection) != 0
            || (a.synchronized)(a.id, connection, &mut synchronized) != 0
            || !synchronized
            || (a.local)(a.id, connection, &mut local) != 0
            || (a.channel)(a.id, connection, local, &mut channel) != 0
        {
            return None;
        }
        let mut ptr = std::ptr::null_mut();
        let mut count = 0;
        if (a.users)(a.id, connection, channel, &mut ptr, &mut count) != 0 {
            return None;
        }
        if count > 128 || (count > 0 && ptr.is_null()) {
            if !ptr.is_null() {
                (a.free)(a.id, ptr.cast());
            }
            return None;
        }
        let mut users = if count == 0 {
            vec![]
        } else {
            std::slice::from_raw_parts(ptr, count).to_vec()
        };
        if !ptr.is_null() {
            (a.free)(a.id, ptr.cast());
        }
        users.retain(|id| *id != local);
        users.sort_unstable();
        let mut name_ptr = std::ptr::null();
        let name = if (a.channel_name)(a.id, connection, channel, &mut name_ptr) == 0
            && !name_ptr.is_null()
        {
            let s = CStr::from_ptr(name_ptr).to_string_lossy().into_owned();
            (a.free)(a.id, name_ptr.cast());
            s
        } else {
            return None;
        };
        let mut hash_ptr = std::ptr::null();
        let server = if (a.server_hash)(a.id, connection, &mut hash_ptr) == 0 && !hash_ptr.is_null()
        {
            let s = CStr::from_ptr(hash_ptr).to_string_lossy().into_owned();
            (a.free)(a.id, hash_ptr.cast());
            s
        } else {
            return None;
        };
        Some(Context {
            connection,
            local,
            channel,
            users,
            server,
            name,
        })
    }
}
pub fn send(ctx: &Context, bytes: &[u8]) -> bool {
    let Some(a) = api() else { return false };
    ctx.users.is_empty()
        || unsafe {
            (a.send)(
                a.id,
                ctx.connection,
                ctx.users.as_ptr(),
                ctx.users.len(),
                bytes.as_ptr(),
                bytes.len(),
                crate::control::DATA_ID.as_ptr(),
            ) == 0
        }
}
pub fn microphone(active: bool) -> bool {
    let previous = crate::capture::force(active);
    let success = api().is_some_and(|a| unsafe { (a.mic)(a.id, active) == 0 });
    if !success {
        crate::capture::force(previous);
        crate::capture::close();
    }
    success
}

pub fn log(message: &str) {
    if let (Some(a), Ok(s)) = (api(), CString::new(message)) {
        unsafe {
            (a.log)(a.id, s.as_ptr());
        }
    }
}
pub fn play(path: &std::path::Path) -> bool {
    let Some(a) = api() else { return false };
    let Ok(s) = CString::new(path.to_string_lossy().as_bytes()) else {
        return false;
    };
    unsafe { (a.play)(a.id, s.as_ptr()) == 0 }
}

/// # Safety
/// Mumble supplies a valid API 1.0 table, copied before this call returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mumble_registerAPIFunctions(ptr: *const c_void) {
    if ptr.is_null() {
        return;
    }
    let a = unsafe { &*ptr.cast::<MumbleAPI>() };
    *API.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Api {
        id: 0,
        free: a.free_memory,
        connection: a.get_active_server_connection,
        synchronized: a.is_connection_synchronized,
        local: a.get_local_user_id,
        channel: a.get_channel_of_user,
        users: a.get_users_in_channel,
        channel_name: a.get_channel_name,
        server_hash: a.get_server_hash,
        send: a.send_data,
        mic: a.request_microphone_activation_overwrite,
        log: a.log,
        play: a.play_sample,
    });
}
#[unsafe(no_mangle)]
pub extern "C" fn mumble_init(id: u32) -> i32 {
    {
        let mut a = API
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(a) = a.as_mut() else { return -1 };
        a.id = id;
    }
    crate::audio::init();
    crate::capture::init();
    #[cfg(windows)]
    {
        match crate::runtime::start() {
            Ok(()) => 0,
            Err(e) => {
                log(&format!("MumbleACRE: {e}"));
                -1
            }
        }
    }
    #[cfg(not(windows))]
    {
        log("MumbleACRE requires Windows for ACRE2 named pipes");
        -1
    }
}
#[unsafe(no_mangle)]
pub extern "C" fn mumble_shutdown() {
    #[cfg(windows)]
    crate::runtime::stop();
    microphone(false);
    crate::capture::close();
}
fn string(s: &'static CStr) -> MumbleStringWrapper {
    MumbleStringWrapper {
        data: s.as_ptr(),
        size: s.to_bytes().len(),
        needs_release: false,
    }
}
#[unsafe(no_mangle)]
pub extern "C" fn mumble_getName() -> MumbleStringWrapper {
    string(c"MumbleACRE")
}
#[unsafe(no_mangle)]
pub extern "C" fn mumble_getAuthor() -> MumbleStringWrapper {
    string(c"MumbleACRE and RMTFAR contributors")
}
#[unsafe(no_mangle)]
pub extern "C" fn mumble_getDescription() -> MumbleStringWrapper {
    string(c"Unofficial ACRE2 2.14.0.1064 backend for Mumble; managed mission channel required")
}
#[unsafe(no_mangle)]
pub extern "C" fn mumble_getAPIVersion() -> mumble_version_t {
    mumble_version_t {
        major: 1,
        minor: 0,
        patch: 0,
    }
}
#[unsafe(no_mangle)]
pub extern "C" fn mumble_getVersion() -> mumble_version_t {
    mumble_version_t {
        major: 0,
        minor: 1,
        patch: 0,
    }
}
#[unsafe(no_mangle)]
pub extern "C" fn mumble_getFeatures() -> u32 {
    2
}
#[unsafe(no_mangle)]
pub extern "C" fn mumble_deactivateFeatures(_features: u32) -> u32 {
    0
}
#[unsafe(no_mangle)]
pub extern "C" fn mumble_releaseResource(_ptr: *const c_void) {}
/// # Safety
/// Mumble provides sample_count * channel_count writable floats.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mumble_onAudioSourceFetched(
    pcm: *mut f32,
    sample_count: u32,
    channel_count: u16,
    sample_rate: u32,
    is_speech: bool,
    user_id: u32,
) -> bool {
    if !is_speech || pcm.is_null() {
        return false;
    }
    let Some(total) = (sample_count as usize).checked_mul(channel_count as usize) else {
        return false;
    };
    if total > isize::MAX as usize / std::mem::size_of::<f32>() {
        return false;
    }
    let samples = unsafe { std::slice::from_raw_parts_mut(pcm, total) };
    crate::audio::process(user_id, samples, channel_count as usize, sample_rate);
    true
}
/// # Safety
/// Mumble provides valid data and NUL-terminated data_id for this callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mumble_onReceiveData(
    connection: i32,
    sender: u32,
    data: *const u8,
    len: usize,
    data_id: *const c_char,
) -> bool {
    if data_id.is_null() || unsafe { CStr::from_ptr(data_id) } != crate::control::DATA_ID {
        return false;
    }
    if data.is_null() || len == 0 || len > 1024 {
        return true;
    }
    #[cfg(windows)]
    crate::runtime::enqueue(crate::runtime::Input::Peer(
        connection,
        sender,
        unsafe { std::slice::from_raw_parts(data, len) }.to_vec(),
    ));
    true
}
/// # Safety
/// Mumble supplies sample_count * channel_count writable PCM16 samples.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mumble_onAudioInput(
    pcm: *mut i16,
    sample_count: u32,
    channel_count: u16,
    _sample_rate: u32,
    is_speech: bool,
) -> bool {
    if pcm.is_null() {
        return false;
    }
    let Some(total) = (sample_count as usize).checked_mul(channel_count as usize) else {
        return false;
    };
    if total > isize::MAX as usize / std::mem::size_of::<i16>() {
        return false;
    }
    crate::capture::input(
        unsafe { std::slice::from_raw_parts_mut(pcm, total) },
        is_speech,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn api_layout() {
        let p = std::mem::size_of::<usize>();
        assert_eq!(std::mem::offset_of!(MumbleAPI, send_data), 35 * p);
        assert_eq!(std::mem::offset_of!(MumbleAPI, play_sample), 37 * p);
        assert_eq!(std::mem::size_of::<MumbleAPI>(), 38 * p);
    }
    #[test]
    fn speech_without_runtime_is_silent_and_notifications_untouched() {
        let mut pcm = [0.8; 8];
        unsafe {
            assert!(!mumble_onAudioSourceFetched(
                pcm.as_mut_ptr(),
                4,
                2,
                48000,
                false,
                42
            ));
            assert_eq!(pcm, [0.8; 8]);
            assert!(mumble_onAudioSourceFetched(
                pcm.as_mut_ptr(),
                4,
                2,
                48000,
                true,
                42
            ));
        }
        assert_eq!(pcm, [0.0; 8]);
    }
}
