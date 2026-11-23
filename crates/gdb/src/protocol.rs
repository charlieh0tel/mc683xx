//! What both stubs answer gdb alike: errors, the target description,
//! a connection as gdbstub's, and gdbstub's failures as a session's.

use std::fmt::Display;

use gdbstub::arch::Arch;
use gdbstub::conn::ConnectionExt;
use gdbstub::stub::DisconnectReason;
use gdbstub::stub::GdbStubError;

use crate::Ending;
use crate::SessionError;
use crate::arch::Cpu32;
use crate::connection::Connection;

/// gdb's error for a register it cannot read or write: EIO.
pub(crate) const EIO: u8 = 5;
/// gdb's error for memory it cannot read or write: EFAULT.
pub(crate) const EFAULT: u8 = 14;
/// gdb's error for a packet the stub refuses: EINVAL.
pub(crate) const EINVAL: u8 = 22;
/// The error a `qXfer` read answers for an annex there is none of
/// (the gdb manual, General Query Packets, `qXfer`).
const INVALID_ANNEX: u8 = 0;
/// The one annex of the target description.
const TARGET_XML: &[u8] = b"target.xml";

/// A [`Connection`] as gdbstub's, which the public API does not name.
pub(crate) struct Adapted<C>(pub(crate) C);

impl<C: Connection> gdbstub::conn::Connection for Adapted<C> {
    type Error = C::Error;

    fn write(&mut self, byte: u8) -> Result<(), C::Error> {
        self.0.write_all(&[byte])
    }

    fn write_all(&mut self, bytes: &[u8]) -> Result<(), C::Error> {
        self.0.write_all(bytes)
    }

    fn flush(&mut self) -> Result<(), C::Error> {
        self.0.flush()
    }

    fn on_session_start(&mut self) -> Result<(), C::Error> {
        self.0.start()
    }
}

impl<C: Connection> ConnectionExt for Adapted<C> {
    fn read(&mut self) -> Result<u8, C::Error> {
        self.0.read()
    }

    fn peek(&mut self) -> Result<Option<u8>, C::Error> {
        self.0.peek()
    }
}

/// How gdbstub's session ended, as ours: gdb's detach or kill, or
/// gdbstub's error as ours.
pub(crate) fn ended<E: Display, C: Display>(
    served: Result<DisconnectReason, GdbStubError<E, C>>,
) -> Result<Ending, SessionError<E, C>> {
    let error = match served {
        Ok(DisconnectReason::Kill) => return Ok(Ending::Killed),
        Ok(_) => return Ok(Ending::Detached),
        Err(error) => error,
    };
    let text = error.to_string();
    if error.is_connection_error() {
        return Err(error
            .into_connection_error()
            .map_or(SessionError::Protocol(text), |(error, _)| {
                SessionError::Connection { error }
            }));
    }
    Err(error
        .into_target_error()
        .map_or(SessionError::Protocol(text), |error| SessionError::Target {
            error,
        }))
}

/// `length` bytes of the architecture's description at `offset`, into
/// `buf`, for a read of `annex`: the bytes read, or the errno for an
/// annex other than `target.xml`.
pub(crate) fn description(
    annex: &[u8],
    offset: u64,
    length: usize,
    buf: &mut [u8],
) -> Result<usize, u8> {
    if annex != TARGET_XML {
        return Err(INVALID_ANNEX);
    }
    let xml = Cpu32::target_description_xml()
        .unwrap_or_default()
        .trim()
        .as_bytes();
    let start = usize::try_from(offset).map_or(xml.len(), |offset| offset.min(xml.len()));
    let end = start.saturating_add(length.min(buf.len())).min(xml.len());
    let data = &xml[start..end];
    buf[..data.len()].copy_from_slice(data);
    Ok(data.len())
}
