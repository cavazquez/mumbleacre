//! Unofficial ACRE2 backend for Mumble. No independent radio simulation.
#![cfg_attr(not(windows), allow(dead_code))]
#[cfg(windows)]
mod acre_adapter;
mod acre_audio_snapshot;
mod acre_babel;
mod acre_direct;
mod acre_intercom;
mod acre_monitor;
mod acre_radio;
mod acre_radio_render;
#[cfg(windows)]
mod acre_sounds;
mod audio;
mod capture;
mod control;
mod ffi;
mod publication;
#[cfg(windows)]
mod runtime;
#[cfg(test)]
mod test_alloc;
#[cfg(windows)]
mod timer;

#[cfg(windows)]
type SharedLog = std::sync::Arc<std::sync::Mutex<mumbleacre_logging::EventLog>>;
#[cfg(windows)]
fn write_event(
    log: Option<&SharedLog>,
    level: mumbleacre_logging::LogLevel,
    event: &str,
    message: &str,
) {
    // Pipe and sound workers never call Mumble APIs.
    if let Some(log) = log {
        let _ = log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .write(level, event, message);
    }
}
