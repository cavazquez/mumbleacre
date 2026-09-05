//! Windows named-pipe endpoints used by `ACRE2Arma`.
//!
//! This module intentionally has no knowledge of Mumble.  A control worker can
//! poll these endpoints, translate messages with [`crate::AcreSession`], and
//! keep every blocking Mumble API call out of the audio callback.

use std::ptr;

use thiserror::Error;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_MORE_DATA, ERROR_NO_DATA, ERROR_PIPE_CONNECTED,
    ERROR_PIPE_LISTENING, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_NOWAIT, PIPE_READMODE_MESSAGE,
    PIPE_TYPE_MESSAGE,
};

use crate::{ACRE_MAX_MESSAGE_BYTES, AcreCodecError, AcreMessage};

/// Pipe created by the voice adapter and read by `ACRE2Arma`.
pub const ACRE_FROM_TS_PIPE: &str = r"\\.\pipe\acre_comm_pipe_fromTS";

/// Pipe created by the voice adapter and written by `ACRE2Arma`.
pub const ACRE_TO_TS_PIPE: &str = r"\\.\pipe\acre_comm_pipe_toTS";

const MAX_DRAIN_CHUNKS: usize = 64;

/// Pair of message-mode named pipes expected by ACRE2 v2.14.0.1064.
///
/// The historical names contain `TS` because they are part of ACRE's private
/// compatibility boundary, not because `TeamSpeak` is used at runtime.
pub struct AcrePipePair {
    from_arma: NamedPipe,
    to_arma: NamedPipe,
}

impl AcrePipePair {
    /// Creates both server endpoints before `ACRE2Arma` opens either client end.
    ///
    /// `FILE_FLAG_FIRST_PIPE_INSTANCE` turns a concurrent `TeamSpeak` plugin or
    /// stale adapter into a clear creation error instead of connecting to an
    /// unknown existing pipe.
    pub fn create() -> Result<Self, AcrePipeError> {
        Ok(Self {
            from_arma: NamedPipe::create(ACRE_FROM_TS_PIPE)?,
            to_arma: NamedPipe::create(ACRE_TO_TS_PIPE)?,
        })
    }

    /// Attempts to complete both client connections without blocking.
    ///
    /// Returns `true` once both endpoints are connected.  A caller should poll
    /// it from its control worker while observing its own shutdown signal.
    pub fn try_connect(&mut self) -> Result<bool, AcrePipeError> {
        let from_connected = self.from_arma.try_connect()?;
        let to_connected = self.to_arma.try_connect()?;
        Ok(from_connected && to_connected)
    }

    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.from_arma.connected && self.to_arma.connected
    }

    /// Polls one message written by `ACRE2Arma`, if available.
    pub fn try_read_from_arma(&mut self) -> Result<Option<AcreMessage>, AcrePipeError> {
        match self.to_arma.try_read()? {
            Some(bytes) => AcreMessage::parse(&bytes)
                .map(Some)
                .map_err(AcrePipeError::Codec),
            None => Ok(None),
        }
    }

    /// Writes one NUL-terminated stock-plugin-style RPC reply to `ACRE2Arma`.
    ///
    /// The caller receives [`AcrePipeError::WouldBlock`] rather than waiting
    /// when the client-side buffer is full.  This makes backpressure explicit
    /// and keeps the pipe usable from a dedicated control worker.
    pub fn write_to_arma(&mut self, message: &AcreMessage) -> Result<(), AcrePipeError> {
        self.from_arma.write(&message.encode_nul_terminated())
    }

    /// Drops both current client connections while retaining the server pipe
    /// instances for a later reconnect.
    pub fn disconnect(&mut self) {
        self.from_arma.disconnect();
        self.to_arma.disconnect();
    }
}

struct NamedPipe {
    name: &'static str,
    handle: HANDLE,
    connected: bool,
}

// SAFETY: a Windows HANDLE is process-scoped and can be used from a different
// thread. `NamedPipe` is moved once into the dedicated control worker and is
// never shared (`Sync` is intentionally not implemented).
unsafe impl Send for NamedPipe {}

impl NamedPipe {
    fn create(name: &'static str) -> Result<Self, AcrePipeError> {
        let wide_name = wide_nul(name);
        let buffer_bytes = win32_byte_count(ACRE_MAX_MESSAGE_BYTES, name)?;
        let handle = unsafe {
            CreateNamedPipeW(
                wide_name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_NOWAIT,
                1,
                buffer_bytes,
                buffer_bytes,
                0,
                ptr::null(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(AcrePipeError::System {
                pipe: name,
                operation: "CreateNamedPipeW",
                code: unsafe { GetLastError() },
            });
        }
        Ok(Self {
            name,
            handle,
            connected: false,
        })
    }

    fn try_connect(&mut self) -> Result<bool, AcrePipeError> {
        if self.connected {
            return Ok(true);
        }
        let connected = unsafe { ConnectNamedPipe(self.handle, ptr::null_mut()) } != 0;
        if connected {
            self.connected = true;
            return Ok(true);
        }

        match unsafe { GetLastError() } {
            ERROR_PIPE_CONNECTED => {
                self.connected = true;
                Ok(true)
            }
            ERROR_PIPE_LISTENING => Ok(false),
            code => Err(AcrePipeError::System {
                pipe: self.name,
                operation: "ConnectNamedPipe",
                code,
            }),
        }
    }

    fn try_read(&mut self) -> Result<Option<Vec<u8>>, AcrePipeError> {
        if !self.connected {
            return Err(AcrePipeError::NotConnected { pipe: self.name });
        }

        let mut buffer = [0_u8; ACRE_MAX_MESSAGE_BYTES];
        let buffer_bytes = win32_byte_count(buffer.len(), self.name)?;
        let mut bytes_read = 0_u32;
        let read = unsafe {
            ReadFile(
                self.handle,
                buffer.as_mut_ptr(),
                buffer_bytes,
                &raw mut bytes_read,
                ptr::null_mut(),
            )
        };
        if read != 0 {
            return Ok((bytes_read != 0).then(|| buffer[..bytes_read as usize].to_vec()));
        }

        match unsafe { GetLastError() } {
            ERROR_NO_DATA => Ok(None),
            ERROR_BROKEN_PIPE => {
                self.disconnect();
                Err(AcrePipeError::Disconnected { pipe: self.name })
            }
            ERROR_MORE_DATA => {
                self.drain_oversized_message()?;
                Err(AcrePipeError::MessageTooLong { pipe: self.name })
            }
            code => Err(AcrePipeError::System {
                pipe: self.name,
                operation: "ReadFile",
                code,
            }),
        }
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), AcrePipeError> {
        if !self.connected {
            return Err(AcrePipeError::NotConnected { pipe: self.name });
        }
        let byte_count = win32_byte_count(bytes.len(), self.name)?;
        let mut bytes_written = 0_u32;
        let written = unsafe {
            WriteFile(
                self.handle,
                bytes.as_ptr(),
                byte_count,
                &raw mut bytes_written,
                ptr::null_mut(),
            )
        };
        if written != 0 && bytes_written as usize == bytes.len() {
            return Ok(());
        }

        let code = unsafe { GetLastError() };
        match code {
            ERROR_NO_DATA => Err(AcrePipeError::WouldBlock { pipe: self.name }),
            ERROR_BROKEN_PIPE => {
                self.disconnect();
                Err(AcrePipeError::Disconnected { pipe: self.name })
            }
            _ if written != 0 => Err(AcrePipeError::ShortWrite {
                pipe: self.name,
                expected: bytes.len(),
                actual: bytes_written as usize,
            }),
            _ => Err(AcrePipeError::System {
                pipe: self.name,
                operation: "WriteFile",
                code,
            }),
        }
    }

    fn drain_oversized_message(&mut self) -> Result<(), AcrePipeError> {
        let mut discard = [0_u8; ACRE_MAX_MESSAGE_BYTES];
        let discard_bytes = win32_byte_count(discard.len(), self.name)?;
        for _ in 0..MAX_DRAIN_CHUNKS {
            let mut bytes_read = 0_u32;
            let read = unsafe {
                ReadFile(
                    self.handle,
                    discard.as_mut_ptr(),
                    discard_bytes,
                    &raw mut bytes_read,
                    ptr::null_mut(),
                )
            };
            if read != 0 {
                break;
            }
            if unsafe { GetLastError() } != ERROR_MORE_DATA {
                break;
            }
        }
        Ok(())
    }

    fn disconnect(&mut self) {
        if self.connected {
            let _ = unsafe { DisconnectNamedPipe(self.handle) };
            self.connected = false;
        }
    }
}

impl Drop for NamedPipe {
    fn drop(&mut self) {
        self.disconnect();
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

fn wide_nul(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn win32_byte_count(byte_count: usize, pipe: &'static str) -> Result<u32, AcrePipeError> {
    if byte_count > ACRE_MAX_MESSAGE_BYTES {
        return Err(AcrePipeError::MessageTooLong { pipe });
    }
    u32::try_from(byte_count).map_err(|_| AcrePipeError::MessageTooLong { pipe })
}

/// I/O failure at the private ACRE named-pipe boundary.
#[derive(Debug, Error)]
pub enum AcrePipeError {
    #[error("ACRE pipe {pipe} is not connected")]
    NotConnected { pipe: &'static str },
    #[error("ACRE pipe {pipe} was disconnected")]
    Disconnected { pipe: &'static str },
    #[error("ACRE pipe {pipe} has backpressure")]
    WouldBlock { pipe: &'static str },
    #[error("ACRE pipe {pipe} sent a message larger than {ACRE_MAX_MESSAGE_BYTES} bytes")]
    MessageTooLong { pipe: &'static str },
    #[error("ACRE pipe {pipe} wrote {actual} of {expected} bytes")]
    ShortWrite {
        pipe: &'static str,
        expected: usize,
        actual: usize,
    },
    #[error("{operation} failed for ACRE pipe {pipe} (Win32 error {code})")]
    System {
        pipe: &'static str,
        operation: &'static str,
        code: u32,
    },
    #[error(transparent)]
    Codec(#[from] AcreCodecError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_the_pipe_names_hard_coded_by_acre2_arma() {
        assert_eq!(ACRE_FROM_TS_PIPE, r"\\.\pipe\acre_comm_pipe_fromTS");
        assert_eq!(ACRE_TO_TS_PIPE, r"\\.\pipe\acre_comm_pipe_toTS");
        assert_eq!(wide_nul(ACRE_FROM_TS_PIPE).last(), Some(&0));
    }
    #[test]
    fn windows_message_io_reconnect_and_exclusive_ownership() {
        use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, OPEN_EXISTING,
        };
        struct Client(HANDLE);
        impl Drop for Client {
            fn drop(&mut self) {
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
        fn open(name: &str) -> Client {
            let name = wide_nul(name);
            let handle = unsafe {
                CreateFileW(
                    name.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    ptr::null(),
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    ptr::null_mut(),
                )
            };
            assert_ne!(handle, INVALID_HANDLE_VALUE);
            Client(handle)
        }
        let mut server = AcrePipePair::create().unwrap();
        assert!(
            AcrePipePair::create().is_err(),
            "another backend must not own these pipes"
        );
        for _ in 0..2 {
            let reader = open(ACRE_FROM_TS_PIPE);
            let writer = open(ACRE_TO_TS_PIPE);
            assert!(server.try_connect().unwrap());
            let ping = b"ping:";
            let mut written = 0;
            assert_ne!(
                unsafe {
                    WriteFile(
                        writer.0,
                        ping.as_ptr(),
                        ping.len() as u32,
                        &mut written,
                        ptr::null_mut(),
                    )
                },
                0
            );
            assert_eq!(
                server.try_read_from_arma().unwrap().unwrap().procedure(),
                "ping"
            );
            let reply = AcreMessage::parse(b"pong:1.000000,").unwrap();
            server.write_to_arma(&reply).unwrap();
            let mut bytes = [0u8; 128];
            let mut read = 0;
            assert_ne!(
                unsafe {
                    ReadFile(
                        reader.0,
                        bytes.as_mut_ptr(),
                        bytes.len() as u32,
                        &mut read,
                        ptr::null_mut(),
                    )
                },
                0
            );
            assert_eq!(&bytes[..read as usize], reply.encode_nul_terminated());
            drop(reader);
            drop(writer);
            server.disconnect();
            assert!(!server.is_connected());
        }
    }
}
