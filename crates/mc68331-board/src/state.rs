//! The board's snapshot format.

#[cfg(doc)]
use crate::Board;

/// The format of [`Board::save`]'s bytes.  The chip's and the peripherals'
/// snapshots inside carry their own.
pub const VERSION: u32 = 23;
