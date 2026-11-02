//! Motorola S-records (M68000PRM Appendix C): an [`Image`] from their
//! text.

use mc683xx_target::address::Address;

use crate::image::Image;
use crate::image::ImageError;
use crate::image::Segment;

/// The character every record starts with (M68000PRM C.1, Table C-1).
const START: u8 = b'S';

/// Why an S-record file does not make an image.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ParseSrecordError {
    /// A line is not a good record.
    #[error("line {line}: {error}")]
    Record {
        /// The line, from 1.
        line: usize,
        /// What is wrong with it.
        error: RecordError,
    },
    /// The records' data do not make an image.
    #[error(transparent)]
    Image(#[from] ImageError),
}

/// What is wrong with one record.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RecordError {
    /// The line does not start with `S`.
    #[error("not an S-record: it does not start with `S`")]
    NotARecord,
    /// The type is not one M68000PRM C.2 lists, or S6.
    #[error("`S{kind}` is not a record type")]
    Type {
        /// The character after the `S`.
        kind: char,
    },
    /// The rest is not pairs of hex digits.
    #[error("not pairs of hex digits")]
    Hex,
    /// The record length is not the pairs that follow it.
    #[error("its record length is {length}, but {pairs} pairs follow it")]
    Length {
        /// The record length field.
        length: u8,
        /// The pairs after it.
        pairs: usize,
    },
    /// The record is too short for its type's address and checksum.
    #[error("too short for its address and checksum")]
    Short,
    /// The checksum does not match the record's bytes.
    #[error("checksum {recorded:#04x}, but the record sums to {computed:#04x}")]
    Checksum {
        /// The checksum field.
        recorded: u8,
        /// The one's complement of the sum of the bytes before it.
        computed: u8,
    },
    /// A count record (S5, S6) does not count the data records before
    /// it.
    #[error("it counts {recorded} data records, but {counted} came before it")]
    Count {
        /// The count in the record.
        recorded: u32,
        /// The data records before it.
        counted: usize,
    },
    /// A record follows the termination record (S7, S8, S9): "each
    /// block of S-records uses only one termination record" (C.2).
    #[error("a record after the termination record")]
    AfterEnd,
}

/// A record's type: the digit after the `S` (M68000PRM C.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// S0, a header: ignored.
    Header,
    /// S1, S2, S3: data at an address of this many bytes.
    Data(usize),
    /// S5, a count of the data records in its 2-byte address field,
    /// or S6, the same in 3 bytes, which M68000PRM does not list but
    /// tools write for files of more than 65,535 records.
    Count(usize),
    /// S7, S8, S9: the end, with an entry address of this many bytes.
    End(usize),
}

impl Kind {
    /// The type `S` and `kind` name.
    const fn from_digit(kind: u8) -> Option<Self> {
        match kind {
            b'0' => Some(Self::Header),
            b'1' => Some(Self::Data(2)),
            b'2' => Some(Self::Data(3)),
            b'3' => Some(Self::Data(4)),
            b'5' => Some(Self::Count(2)),
            b'6' => Some(Self::Count(3)),
            b'7' => Some(Self::End(4)),
            b'8' => Some(Self::End(3)),
            b'9' => Some(Self::End(2)),
            _ => None,
        }
    }

    /// Bytes in the address field.
    const fn address_bytes(self) -> usize {
        match self {
            Self::Header => 2,
            Self::Data(bytes) | Self::Count(bytes) | Self::End(bytes) => bytes,
        }
    }
}

/// One record, decoded.
struct Record {
    /// Its type.
    kind: Kind,
    /// Its address field, as a number.
    address: u32,
    /// Its code/data field.
    data: Vec<u8>,
}

/// The value of hex digit `digit`.
fn hex_digit(digit: u8) -> Option<u8> {
    char::from(digit)
        .to_digit(16)
        .and_then(|value| u8::try_from(value).ok())
}

impl Record {
    /// The record on `line`, which has no line ending.
    fn parse(line: &str) -> Result<Self, RecordError> {
        let text = line.as_bytes();
        if text.first() != Some(&START) {
            return Err(RecordError::NotARecord);
        }
        let kind_digit = *text.get(1).ok_or(RecordError::Short)?;
        let kind = Kind::from_digit(kind_digit).ok_or(RecordError::Type {
            kind: char::from(kind_digit),
        })?;
        let pairs = text[2..].chunks(2);
        let bytes = pairs
            .map(|pair| match pair {
                [high, low] => Some(hex_digit(*high)? << 4 | hex_digit(*low)?),
                _ => None,
            })
            .collect::<Option<Vec<u8>>>()
            .ok_or(RecordError::Hex)?;
        let (&length, rest) = bytes.split_first().ok_or(RecordError::Short)?;
        if usize::from(length) != rest.len() {
            return Err(RecordError::Length {
                length,
                pairs: rest.len(),
            });
        }
        let address_bytes = kind.address_bytes();
        let (&recorded, body) = rest.split_last().ok_or(RecordError::Short)?;
        if body.len() < address_bytes {
            return Err(RecordError::Short);
        }
        // The one's complement of the sum of length, address and data
        // (Table C-1).
        let computed = !bytes[..bytes.len() - 1]
            .iter()
            .fold(0u8, |sum, byte| sum.wrapping_add(*byte));
        if computed != recorded {
            return Err(RecordError::Checksum { recorded, computed });
        }
        let (address, data) = body.split_at(address_bytes);
        Ok(Self {
            kind,
            address: address
                .iter()
                .fold(0, |value, byte| value << 8 | u32::from(*byte)),
            data: data.to_vec(),
        })
    }
}

impl Image {
    /// The image S-records `text` give: S1, S2 and S3 records' data, at
    /// their addresses; S0 ignored; S5 and S6 counts checked; the S7,
    /// S8 or S9 address the entry point, unless it is zero, which on a
    /// CPU32 holds the reset vector, not code (CPU32RM 6.2.1), and
    /// which tools write for "no entry".  Blank lines are skipped,
    /// and each line's trailing white space, a carriage return among
    /// it.
    ///
    /// # Errors
    /// A line is not a good record, the records' data overlap, or
    /// there are none.
    pub fn from_srecords(text: &str) -> Result<Self, ParseSrecordError> {
        let mut segments = Vec::new();
        let mut entry = None;
        let mut ended = false;
        for (index, line) in text.lines().enumerate() {
            let line = line.trim_end();
            if line.is_empty() {
                continue;
            }
            let error = |error| ParseSrecordError::Record {
                line: index + 1,
                error,
            };
            if ended {
                return Err(error(RecordError::AfterEnd));
            }
            let record = Record::parse(line).map_err(error)?;
            match record.kind {
                Kind::Header => {}
                Kind::Data(_) => segments.push(Segment::new(Address(record.address), record.data)),
                Kind::Count(_) => {
                    if usize::try_from(record.address).ok() != Some(segments.len()) {
                        return Err(error(RecordError::Count {
                            recorded: record.address,
                            counted: segments.len(),
                        }));
                    }
                }
                Kind::End(_) => {
                    ended = true;
                    entry = (record.address != 0).then_some(Address(record.address));
                }
            }
        }
        Ok(Self::new(segments, entry)?)
    }
}

#[cfg(test)]
mod tests {
    use mc683xx_target::address::Address;

    use super::ParseSrecordError;
    use super::RecordError;
    use crate::image::Image;
    use crate::image::ImageError;
    use crate::image::Segment;

    /// M68000PRM C.3's example module.
    const EXAMPLE: &str = "S00600004844521B
S1130000285F245F2212226A000424290008237C2A
S11300100002000800082629001853812341001813
S113002041E900084E42234300182342000824A952
S107003000144ED492
S9030000FC
";

    /// The record error on `line` of `text`.
    fn record_error(text: &str) -> (usize, RecordError) {
        match Image::from_srecords(text) {
            Err(ParseSrecordError::Record { line, error }) => (line, error),
            other => panic!("not a record error: {other:?}"),
        }
    }

    #[test]
    fn the_manuals_example_loads_with_no_entry() {
        let image = Image::from_srecords(EXAMPLE).unwrap();
        assert_eq!(image.segments().len(), 1);
        let segment = &image.segments()[0];
        assert_eq!(segment.address(), Address(0));
        assert_eq!(segment.data().len(), 0x34);
        assert_eq!(segment.data()[..4], [0x28, 0x5f, 0x24, 0x5f]);
        assert_eq!(segment.data()[0x30..], [0x00, 0x14, 0x4e, 0xd4]);
        assert_eq!(image.entry(), None, "S9 address zero");
    }

    #[test]
    fn s2_s3_with_s7_and_s8_entries_and_counts() {
        let image =
            Image::from_srecords("S30900200000DEADBEEF9E\r\nS5030001FB\r\n\r\nS70500200100D9\r\n")
                .unwrap();
        assert_eq!(
            image.segments(),
            [Segment::new(
                Address(0x20_0000),
                vec![0xde, 0xad, 0xbe, 0xef]
            )]
        );
        assert_eq!(image.entry(), Some(Address(0x20_0100)));
        let image = Image::from_srecords("S2061234123471FC\nS604000001FA\nS804001234B5\n").unwrap();
        assert_eq!(image.segments()[0].address(), Address(0x12_3412));
        assert_eq!(image.segments()[0].data(), [0x34, 0x71]);
        assert_eq!(image.entry(), Some(Address(0x1234)));
    }

    #[test]
    fn bad_records_name_their_line() {
        assert_eq!(record_error("\nX1"), (2, RecordError::NotARecord));
        assert_eq!(
            record_error("S4030000FC"),
            (1, RecordError::Type { kind: '4' })
        );
        assert_eq!(record_error("S"), (1, RecordError::Short));
        assert_eq!(record_error("S1030G00FC"), (1, RecordError::Hex));
        assert_eq!(record_error("S1030000F"), (1, RecordError::Hex));
        assert_eq!(
            record_error("S1040000FC"),
            (
                1,
                RecordError::Length {
                    length: 4,
                    pairs: 3
                }
            )
        );
        assert_eq!(record_error("S10200FD"), (1, RecordError::Short));
        assert_eq!(
            record_error("S1030000FD"),
            (
                1,
                RecordError::Checksum {
                    recorded: 0xfd,
                    computed: 0xfc,
                }
            )
        );
        assert_eq!(
            record_error("S1040000AA51\nS5030002FA"),
            (
                2,
                RecordError::Count {
                    recorded: 2,
                    counted: 1,
                }
            )
        );
        assert_eq!(
            record_error("S1040000AA51\nS9030000FC\nS1040002AA4F"),
            (3, RecordError::AfterEnd)
        );
    }

    #[test]
    fn overlapping_or_no_data_is_refused() {
        assert_eq!(
            Image::from_srecords("S1050000AAAAA6\nS1040001AA50"),
            Err(ParseSrecordError::Image(ImageError::Overlap {
                first: Address(0),
                second: Address(1),
            }))
        );
        assert_eq!(
            Image::from_srecords("S00600004844521B\nS9030000FC"),
            Err(ParseSrecordError::Image(ImageError::Empty))
        );
    }

    #[test]
    fn errors_name_the_line() {
        assert_eq!(
            Image::from_srecords("S1030000FD").unwrap_err().to_string(),
            "line 1: checksum 0xfd, but the record sums to 0xfc"
        );
    }
}
