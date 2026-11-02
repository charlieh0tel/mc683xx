//! What both stubs answer gdb alike: errors, the target description,
//! and gdbstub's failures as a session's.

use std::fmt::Display;

use gdbstub::arch::Arch;
use gdbstub::stub::GdbStubError;

use crate::arch::Cpu32;
use crate::stub::SessionError;

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

/// gdbstub's error as ours.
pub(crate) fn failure<E: Display, C: Display>(error: GdbStubError<E, C>) -> SessionError<E, C> {
    let text = error.to_string();
    if error.is_connection_error() {
        return error
            .into_connection_error()
            .map_or(SessionError::Protocol(text), |(error, _)| {
                SessionError::Connection(error)
            });
    }
    error
        .into_target_error()
        .map_or(SessionError::Protocol(text), SessionError::Target)
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
