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

    /// The address one past the last byte, past the 32-bit bus if the
    /// segment ends at its top.
    fn end(&self) -> u64 {
        u64::from(self.address.0) + self.data.len() as u64
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
        let mut sorted: Vec<Segment> = segments
            .into_iter()
            .filter(|segment| !segment.data.is_empty())
            .collect();
        sorted.sort_by_key(|segment| segment.address);
        let mut joined: Vec<Segment> = Vec::with_capacity(sorted.len());
        for segment in sorted {
            if segment.end() > ADDRESS_SPACE {
                return Err(ImageError::Wraps {
                    address: segment.address,
                });
            }
            match joined.last_mut() {
                Some(last) if last.end() > u64::from(segment.address.0) => {
                    return Err(ImageError::Overlap {
                        first: last.address,
                        second: segment.address,
                    });
                }
                Some(last) if last.end() == u64::from(segment.address.0) => {
                    last.data.extend_from_slice(&segment.data);
                }
                _ => joined.push(segment),
            }
        }
        if joined.is_empty() {
            return Err(ImageError::Empty);
        }
        Ok(Self {
            segments: joined,
            entry,
        })
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
