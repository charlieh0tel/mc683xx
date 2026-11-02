//! An image to load: segments of bytes at addresses, and an entry
//! point.

use mc683xx_target::address::Address;

/// Bytes in the CPU32's 32-bit address space (CPU32RM 5.3).
const ADDRESS_SPACE: u64 = 1 << 32;

/// A run of bytes to load at an address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// Where the first byte goes.
    address: Address,
    /// The bytes, in address order.
    data: Vec<u8>,
}

impl Segment {
    /// `data` at `address`.
    #[must_use]
    pub const fn new(address: Address, data: Vec<u8>) -> Self {
        Self { address, data }
    }

    /// Where the first byte goes.
    #[must_use]
    pub const fn address(&self) -> Address {
        self.address
    }

    /// The bytes, in address order.
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

/// An image: segments that neither overlap nor touch, in address
/// order, none empty, and an entry point, where the program starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// The segments, in address order, apart and not empty.
    segments: Vec<Segment>,
    /// Where the program starts, if the image says.
    entry: Option<Address>,
}

/// Segments that do not make an image.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ImageError {
    /// Two segments share bytes.
    #[error("the segments at {first} and {second} overlap")]
    Overlap {
        /// The lower segment's address.
        first: Address,
        /// The higher one's.
        second: Address,
    },
    /// A segment runs past the top of the 32-bit address space.
    #[error("the segment at {address} runs past the top of the address space")]
    Wraps {
        /// The segment's address.
        address: Address,
    },
    /// No segment has a byte.
    #[error("there are no bytes to load")]
    Empty,
}

/// `spans`, `place` giving each one's address and length, laid out as
/// an image lays out its segments: sorted by address, empty ones
/// dropped, and those that touch grouped into runs, each run's spans in
/// address order.  It looks at no span's bytes, so a loader can check
/// its spans before it copies them.
///
/// # Errors
/// Two spans overlap, one runs past the top of the address space, or
/// there are no bytes.
pub(crate) fn arrange<T>(
    spans: impl IntoIterator<Item = T>,
    place: impl Fn(&T) -> (Address, usize),
) -> Result<Vec<Vec<T>>, ImageError> {
    let mut sorted: Vec<T> = spans.into_iter().filter(|span| place(span).1 > 0).collect();
    sorted.sort_by_key(|span| place(span).0);
    // each run's address, the address one past its end, and its spans
    let mut runs: Vec<(Address, u64, Vec<T>)> = Vec::with_capacity(sorted.len());
    for span in sorted {
        let (address, length) = place(&span);
        let start = u64::from(address.0);
        let end = start + length as u64;
        if end > ADDRESS_SPACE {
            return Err(ImageError::Wraps { address });
        }
        match runs.last_mut() {
            Some((first, last_end, _)) if *last_end > start => {
                return Err(ImageError::Overlap {
                    first: *first,
                    second: address,
                });
            }
            Some((_, last_end, run)) if *last_end == start => {
                *last_end = end;
                run.push(span);
            }
            _ => runs.push((address, end, vec![span])),
        }
    }
    if runs.is_empty() {
        return Err(ImageError::Empty);
    }
    Ok(runs.into_iter().map(|(_, _, run)| run).collect())
}

impl Image {
    /// The image of `segments` and `entry`: segments sorted by address,
    /// empty ones dropped, and those that touch joined.
    ///
    /// # Errors
    /// Two segments overlap, one runs past the top of the address
    /// space, or there are no bytes.
    pub fn new(
        segments: impl IntoIterator<Item = Segment>,
        entry: Option<Address>,
    ) -> Result<Self, ImageError> {
        let segments = arrange(segments, |segment| (segment.address, segment.data.len()))?
            .into_iter()
            .map(|run| {
                let address = run[0].address;
                Segment::new(
                    address,
                    run.into_iter().flat_map(|segment| segment.data).collect(),
                )
            })
            .collect();
        Ok(Self { segments, entry })
    }

    /// `data` at `address`, a raw binary, with no entry point.
    ///
    /// # Errors
    /// `data` is empty or runs past the top of the address space.
    pub fn from_binary(address: Address, data: Vec<u8>) -> Result<Self, ImageError> {
        Self::new([Segment::new(address, data)], None)
    }

    /// The segments, in address order.
    #[must_use]
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// Where the program starts, if the image says.
    #[must_use]
    pub const fn entry(&self) -> Option<Address> {
        self.entry
    }

    /// Sets the entry point, or clears it so a load leaves RPC alone.
    pub const fn set_entry(&mut self, entry: Option<Address>) {
        self.entry = entry;
    }

    /// The bytes in all the segments.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.segments.iter().map(|segment| segment.data.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use mc683xx_target::address::Address;

    use super::Image;
    use super::ImageError;
    use super::Segment;

    #[test]
    fn segments_are_sorted_and_touching_ones_joined() {
        let image = Image::new(
            [
                Segment::new(Address(0x104), vec![5, 6]),
                Segment::new(Address(0x100), vec![1, 2, 3, 4]),
                Segment::new(Address(0x200), vec![]),
                Segment::new(Address(0x300), vec![9]),
            ],
            Some(Address(0x100)),
        )
        .unwrap();
        assert_eq!(
            image.segments(),
            [
                Segment::new(Address(0x100), vec![1, 2, 3, 4, 5, 6]),
                Segment::new(Address(0x300), vec![9]),
            ]
        );
        assert_eq!(image.bytes(), 7);
        assert_eq!(image.entry(), Some(Address(0x100)));
    }

    #[test]
    fn overlapping_wrapping_and_empty_images_are_refused() {
        assert_eq!(
            Image::new(
                [
                    Segment::new(Address(0x100), vec![0; 4]),
                    Segment::new(Address(0x103), vec![0]),
                ],
                None,
            ),
            Err(ImageError::Overlap {
                first: Address(0x100),
                second: Address(0x103),
            })
        );
        assert_eq!(
            Image::new(
                [
                    Segment::new(Address(0x100), vec![0; 4]),
                    Segment::new(Address(0x104), vec![0; 2]),
                    Segment::new(Address(0x105), vec![0]),
                ],
                None,
            ),
            Err(ImageError::Overlap {
                first: Address(0x100),
                second: Address(0x105),
            })
        );
        assert_eq!(
            Image::from_binary(Address(0xffff_fffe), vec![0; 3]),
            Err(ImageError::Wraps {
                address: Address(0xffff_fffe),
            })
        );
        assert!(Image::from_binary(Address(0xffff_fffe), vec![0; 2]).is_ok());
        assert_eq!(
            Image::from_binary(Address(0), vec![]),
            Err(ImageError::Empty)
        );
    }
}
