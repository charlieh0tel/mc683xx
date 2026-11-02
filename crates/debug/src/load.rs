//! Loading an [`Image`] into a halted target: word writes, read back
//! and compared, and RPC set from the entry point.

use std::fmt;
use std::ops::Range;

use mc683xx_target::Cpu32Target;
use mc683xx_target::address::Address;
use mc683xx_target::function_code::FunctionCode;
use mc683xx_target::register::SystemRegister;
use mc683xx_target::size::Size;

use crate::image::Image;

/// The most bytes one memory call carries: it bounds what a failed call
/// reports and what a read back holds at once.  A multiple of a word.
const CHUNK_BYTES: usize = 256;

/// What to do with a segment's byte at an odd start or end, which a
/// word write cannot reach without writing its neighbor (CPU32RM 6.2.3:
/// a word at an odd address is an address error).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum OddEdges {
    /// Refuse the image before writing anything.
    #[default]
    Refuse,
    /// Write it in a byte access.
    Bytes,
}

/// How to load an image.  `LoadOptions::default()`, then set fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct LoadOptions {
    /// The address space written: supervisor data by default.
    pub function_code: FunctionCode,
    /// A segment's odd edge bytes: refused by default.
    pub odd_edges: OddEdges,
    /// Whether to read every byte back and compare: on by default.
    pub verify: bool,
}

impl Default for LoadOptions {
    fn default() -> Self {
        Self {
            function_code: FunctionCode::SUPERVISOR_DATA,
            odd_edges: OddEdges::Refuse,
            verify: true,
        }
    }
}

/// Whether a load read its bytes back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Verification {
    /// Every byte read back as written.
    Passed,
    /// Not read back, as the options asked.
    Skipped,
}

/// A segment a load wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct LoadedSegment {
    /// Its first byte's address.
    pub address: Address,
    /// Its bytes.
    pub bytes: usize,
}

/// What a load did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct LoadReport {
    /// The segments written, in address order.
    pub segments: Vec<LoadedSegment>,
    /// The bytes written.
    pub bytes: usize,
    /// RPC as the load set it: the image's entry point, if it had one.
    pub entry: Option<Address>,
    /// Whether the bytes were read back.
    pub verification: Verification,
}

/// A line a segment, then the totals, the verification and the entry.
impl fmt::Display for LoadReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for segment in &self.segments {
            writeln!(formatter, "{}: {} bytes", segment.address, segment.bytes)?;
        }
        write!(
            formatter,
            "{} bytes in {} segments, {}",
            self.bytes,
            self.segments.len(),
            match self.verification {
                Verification::Passed => "verified",
                Verification::Skipped => "not verified",
            }
        )?;
        match self.entry {
            Some(entry) => write!(formatter, ", RPC {entry}"),
            None => write!(formatter, ", RPC unchanged"),
        }
    }
}

/// Why a load failed.  `E` is the target's error.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LoadError<E> {
    /// A segment has a byte at an odd edge, and the options refuse
    /// those; nothing was written.
    #[error("{address} is a segment's odd edge, which a word write cannot reach")]
    OddEdge {
        /// The byte's address.
        address: Address,
    },
    /// A write failed.
    #[error("writing {bytes} bytes at {address}: {error}")]
    Write {
        /// The call's first address.
        address: Address,
        /// The call's bytes.
        bytes: usize,
        /// The target's error.
        error: E,
    },
    /// A read back failed.
    #[error("reading back {bytes} bytes at {address}: {error}")]
    Read {
        /// The call's first address.
        address: Address,
        /// The call's bytes.
        bytes: usize,
        /// The target's error.
        error: E,
    },
    /// A byte read back is not the byte written.
    #[error("{address}: wrote {wrote:#04x}, read back {read:#04x}")]
    Verify {
        /// The first such byte's address.
        address: Address,
        /// The byte written.
        wrote: u8,
        /// The byte read.
        read: u8,
    },
    /// Writing RPC failed.
    #[error("setting RPC to {entry}: {error}")]
    Entry {
        /// The entry point.
        entry: Address,
        /// The target's error.
        error: E,
    },
}

/// One memory call's worth of an image: bytes of a segment, in accesses
/// of one size.
#[derive(Debug)]
struct Access<'a> {
    /// The first byte's address.
    address: Address,
    /// The access size.
    size: Size,
    /// The bytes.
    data: &'a [u8],
}

/// `data`'s accesses from `address`, in words, but its odd edges in
/// bytes or refused as `odd_edges` says, each at most [`CHUNK_BYTES`].
fn accesses<E>(
    address: Address,
    data: &[u8],
    odd_edges: OddEdges,
) -> Result<Vec<Access<'_>>, LoadError<E>> {
    let head = usize::from(address.is_odd());
    let tail = (data.len() - head) % 2;
    let words_end = data.len() - tail;
    let pieces: [(Range<usize>, Size); 3] = [
        (0..head, Size::Byte),
        (head..words_end, Size::Word),
        (words_end..data.len(), Size::Byte),
    ];
    if odd_edges == OddEdges::Refuse
        && let Some((edge, _)) = pieces
            .iter()
            .find(|(range, size)| *size == Size::Byte && !range.is_empty())
    {
        return Err(LoadError::OddEdge {
            address: address.offset(edge.start as u32),
        });
    }
    Ok(pieces
        .into_iter()
        .flat_map(|(range, size)| {
            let start = range.start;
            data[range]
                .chunks(CHUNK_BYTES)
                .enumerate()
                .map(move |(index, chunk)| (start + index * CHUNK_BYTES, size, chunk))
        })
        .map(|(offset, size, data)| Access {
            address: address.offset(offset as u32),
            size,
            data,
        })
        .collect())
}

impl Image {
    /// Writes the image to `target`, which is halted, as `options`
    /// say; reads it back and compares unless they say not to; then
    /// sets RPC to the entry point if the image has one.  Nothing
    /// else is touched: no SSP, which the image's reset vector, if
    /// it has one, holds for a reset.
    ///
    /// # Errors
    /// An odd edge the options refuse, before any write; a write or a
    /// read back the target fails; a byte read back that is not the
    /// byte written; or RPC's write.  What was written before the
    /// error stays written.
    pub fn load<T: Cpu32Target>(
        &self,
        target: &mut T,
        options: &LoadOptions,
    ) -> Result<LoadReport, LoadError<T::Error>> {
        let mut planned = Vec::new();
        for segment in self.segments() {
            planned.extend(accesses(
                segment.address(),
                segment.data(),
                options.odd_edges,
            )?);
        }
        for access in &planned {
            target
                .write_memory(
                    access.address,
                    options.function_code,
                    access.size,
                    access.data,
                )
                .map_err(|error| LoadError::Write {
                    address: access.address,
                    bytes: access.data.len(),
                    error,
                })?;
        }
        let verification = if options.verify {
            let mut read = [0; CHUNK_BYTES];
            for access in &planned {
                let read = &mut read[..access.data.len()];
                target
                    .read_memory(access.address, options.function_code, access.size, read)
                    .map_err(|error| LoadError::Read {
                        address: access.address,
                        bytes: access.data.len(),
                        error,
                    })?;
                if let Some((offset, (&wrote, &read))) = access
                    .data
                    .iter()
                    .zip(read.iter())
                    .enumerate()
                    .find(|(_, (wrote, read))| wrote != read)
                {
                    return Err(LoadError::Verify {
                        address: access.address.offset(offset as u32),
                        wrote,
                        read,
                    });
                }
            }
            Verification::Passed
        } else {
            Verification::Skipped
        };
        if let Some(entry) = self.entry() {
            target
                .write_system_register(SystemRegister::Rpc, entry.0)
                .map_err(|error| LoadError::Entry { entry, error })?;
        }
        Ok(LoadReport {
            segments: self
                .segments()
                .iter()
                .map(|segment| LoadedSegment {
                    address: segment.address(),
                    bytes: segment.data().len(),
                })
                .collect(),
            bytes: self.bytes(),
            entry: self.entry(),
            verification,
        })
    }
}
