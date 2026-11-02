//! ELF executables: an [`Image`] from an ELF32 big-endian file for the
//! 68000 family.  Offsets and values are the System V ABI's (the
//! generic ABI, "ELF Header" and "Program Header"), with `EM_68K` the
//! machine.

use std::ops::Range;

use mc683xx_target::address::Address;

use crate::image::Image;
use crate::image::ImageError;
use crate::image::Segment;

/// `e_ident[EI_MAG0..EI_MAG3]`.
const MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
/// `e_ident[EI_CLASS]`'s index.
const EI_CLASS: usize = 4;
/// `e_ident[EI_DATA]`'s index.
const EI_DATA: usize = 5;
/// `e_ident[EI_VERSION]`'s index.
const EI_VERSION: usize = 6;
/// 32-bit objects.
const ELFCLASS32: u8 = 1;
/// Big-endian (two's complement, most significant byte first).
const ELFDATA2MSB: u8 = 2;
/// The current version, the only one.
const EV_CURRENT: u8 = 1;
/// Motorola 68000 family.
const EM_68K: u16 = 4;
/// `e_machine`'s offset.
const E_MACHINE: usize = 18;
/// `e_entry`'s offset.
const E_ENTRY: usize = 24;
/// `e_phoff`'s offset.
const E_PHOFF: usize = 28;
/// `e_phentsize`'s offset.
const E_PHENTSIZE: usize = 42;
/// `e_phnum`'s offset.
const E_PHNUM: usize = 44;
/// Bytes in an ELF32 header.
const HEADER_BYTES: usize = 52;
/// Bytes in an ELF32 program header.
const PROGRAM_HEADER_BYTES: usize = 32;
/// A loadable segment's `p_type`.
const PT_LOAD: u32 = 1;
/// `p_type`'s offset in a program header.
const P_TYPE: usize = 0;
/// `p_offset`'s.
const P_OFFSET: usize = 4;
/// `p_paddr`'s.
const P_PADDR: usize = 12;
/// `p_filesz`'s.
const P_FILESZ: usize = 16;

/// Why a file does not make an image.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ParseElfError {
    /// The file does not start with ELF's magic number.
    #[error("not an ELF file")]
    NotElf,
    /// The file ends inside its header or its program headers.
    #[error("the file ends inside its {what}")]
    Truncated {
        /// The ELF header, or the program headers.
        what: &'static str,
    },
    /// Not a 32-bit file.
    #[error("ELF class {class}, not 32-bit (ELFCLASS32, 1)")]
    Class {
        /// `e_ident[EI_CLASS]`.
        class: u8,
    },
    /// Not big-endian.
    #[error("ELF data encoding {encoding}, not big-endian (ELFDATA2MSB, 2)")]
    Encoding {
        /// `e_ident[EI_DATA]`.
        encoding: u8,
    },
    /// Not the current ELF version.
    #[error("ELF version {version}, not 1")]
    Version {
        /// `e_ident[EI_VERSION]`.
        version: u8,
    },
    /// Not for the 68000 family.
    #[error("machine {machine}, not the 68000 family (EM_68K, 4)")]
    Machine {
        /// `e_machine`.
        machine: u16,
    },
    /// Program headers of a size other than ELF32's.
    #[error("program headers of {size} bytes, not 32")]
    ProgramHeaderSize {
        /// `e_phentsize`.
        size: u16,
    },
    /// A loadable segment's bytes lie past the file's end.
    #[error("program header {index}'s bytes lie past the end of the file")]
    Segment {
        /// The program header, from 0.
        index: usize,
    },
    /// The segments do not make an image.
    #[error(transparent)]
    Image(#[from] ImageError),
}

/// The big-endian `u16` at `offset` in `bytes`, which holds it.
fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([bytes[offset], bytes[offset + 1]])
}

/// The big-endian `u32` at `offset` in `bytes`, which holds it.
fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

/// `length` bytes from `start` in a file of `file_bytes`, if they fit.
fn within(start: u32, length: usize, file_bytes: usize) -> Option<Range<usize>> {
    let start = usize::try_from(start).ok()?;
    let end = start.checked_add(length)?;
    (end <= file_bytes).then_some(start..end)
}

impl Image {
    /// The image an ELF32 big-endian 68000-family file gives: each
    /// `PT_LOAD` segment's file bytes at its physical address
    /// (`p_paddr`), and `e_entry` the entry point unless it is zero,
    /// which the generic ABI gives for "no entry point".  A segment's
    /// memory past its file bytes (`.bss`) is not written, as gdb's
    /// `load` leaves it: the program's startup clears it.
    ///
    /// # Errors
    /// The file is not ELF32, big-endian, version 1 and for the 68000
    /// family; it is truncated; or its loadable segments overlap or
    /// have no bytes.
    pub fn from_elf(file: &[u8]) -> Result<Self, ParseElfError> {
        if file.get(..MAGIC.len()) != Some(&MAGIC) {
            return Err(ParseElfError::NotElf);
        }
        if file.len() < HEADER_BYTES {
            return Err(ParseElfError::Truncated { what: "ELF header" });
        }
        let class = file[EI_CLASS];
        if class != ELFCLASS32 {
            return Err(ParseElfError::Class { class });
        }
        let encoding = file[EI_DATA];
        if encoding != ELFDATA2MSB {
            return Err(ParseElfError::Encoding { encoding });
        }
        let version = file[EI_VERSION];
        if version != EV_CURRENT {
            return Err(ParseElfError::Version { version });
        }
        let machine = u16_at(file, E_MACHINE);
        if machine != EM_68K {
            return Err(ParseElfError::Machine { machine });
        }
        let count = usize::from(u16_at(file, E_PHNUM));
        let size = u16_at(file, E_PHENTSIZE);
        if count > 0 && usize::from(size) != PROGRAM_HEADER_BYTES {
            return Err(ParseElfError::ProgramHeaderSize { size });
        }
        let headers = within(
            u32_at(file, E_PHOFF),
            count * PROGRAM_HEADER_BYTES,
            file.len(),
        )
        .ok_or(ParseElfError::Truncated {
            what: "program headers",
        })?;
        let mut segments = Vec::new();
        for (index, header) in file[headers]
            .as_chunks::<PROGRAM_HEADER_BYTES>()
            .0
            .iter()
            .enumerate()
        {
            if u32_at(header, P_TYPE) != PT_LOAD {
                continue;
            }
            let length = usize::try_from(u32_at(header, P_FILESZ))
                .map_err(|_| ParseElfError::Segment { index })?;
            let bytes = within(u32_at(header, P_OFFSET), length, file.len())
                .ok_or(ParseElfError::Segment { index })?;
            segments.push(Segment::new(
                Address(u32_at(header, P_PADDR)),
                file[bytes].to_vec(),
            ));
        }
        let entry = u32_at(file, E_ENTRY);
        Ok(Self::new(segments, (entry != 0).then_some(Address(entry)))?)
    }
}

#[cfg(test)]
mod tests {
    use mc683xx_target::address::Address;

    use super::ParseElfError;
    use crate::image::Image;
    use crate::image::ImageError;
    use crate::image::Segment;

    /// A program header: type, offset, virtual and physical address,
    /// file and memory size.
    type Header = (u32, u32, u32, u32, u32, u32);

    /// An ELF32 big-endian 68000 file entering at `entry`, with
    /// `headers` after its header and then `body`.
    fn elf(entry: u32, headers: &[Header], body: &[u8]) -> Vec<u8> {
        let mut file = vec![0x7f, b'E', b'L', b'F', 1, 2, 1];
        file.resize(16, 0);
        file.extend_from_slice(&2u16.to_be_bytes()); // ET_EXEC
        file.extend_from_slice(&4u16.to_be_bytes()); // EM_68K
        file.extend_from_slice(&1u32.to_be_bytes());
        file.extend_from_slice(&entry.to_be_bytes());
        file.extend_from_slice(&52u32.to_be_bytes()); // e_phoff
        file.extend_from_slice(&0u32.to_be_bytes()); // e_shoff
        file.extend_from_slice(&0u32.to_be_bytes()); // e_flags
        file.extend_from_slice(&52u16.to_be_bytes()); // e_ehsize
        file.extend_from_slice(&32u16.to_be_bytes());
        file.extend_from_slice(&u16::try_from(headers.len()).unwrap().to_be_bytes());
        file.extend_from_slice(&[0; 6]); // section headers
        for &(kind, offset, virtual_address, physical, file_size, memory_size) in headers {
            for field in [
                kind,
                offset,
                virtual_address,
                physical,
                file_size,
                memory_size,
                0,
                2,
            ] {
                file.extend_from_slice(&field.to_be_bytes());
            }
        }
        file.extend_from_slice(body);
        file
    }

    /// Where `elf`'s body starts with `headers` program headers.
    fn body(headers: u32) -> u32 {
        52 + 32 * headers
    }

    #[test]
    fn loadable_segments_at_their_physical_addresses() {
        let file = elf(
            0x20_0100,
            &[
                (1, body(4), 0x20_0000, 0x20_0000, 4, 4),
                // .data run from RAM, loaded after the text
                (1, body(4) + 4, 0x30_0000, 0x20_0004, 2, 8),
                // .bss: no file bytes
                (1, body(4) + 6, 0x30_0008, 0x20_0006, 0, 16),
                // not loadable
                (0x6474_e551, 0, 0, 0, 0, 0),
            ],
            &[1, 2, 3, 4, 5, 6],
        );
        let image = Image::from_elf(&file).unwrap();
        assert_eq!(
            image.segments(),
            [Segment::new(Address(0x20_0000), vec![1, 2, 3, 4, 5, 6])]
        );
        assert_eq!(image.entry(), Some(Address(0x20_0100)));
        let file = elf(0, &[(1, body(1), 0x100, 0x100, 1, 1)], &[7]);
        assert_eq!(Image::from_elf(&file).unwrap().entry(), None);
    }

    #[test]
    fn other_files_are_refused_by_name() {
        let file = elf(0, &[(1, body(1), 0x100, 0x100, 2, 2)], &[7, 8]);
        let changed = |offset: usize, value: u8| {
            let mut file = file.clone();
            file[offset] = value;
            Image::from_elf(&file)
        };
        assert_eq!(changed(0, 0), Err(ParseElfError::NotElf));
        assert_eq!(changed(4, 2), Err(ParseElfError::Class { class: 2 }));
        assert_eq!(changed(5, 1), Err(ParseElfError::Encoding { encoding: 1 }));
        assert_eq!(changed(6, 0), Err(ParseElfError::Version { version: 0 }));
        assert_eq!(changed(19, 3), Err(ParseElfError::Machine { machine: 3 }));
        assert_eq!(
            changed(43, 56),
            Err(ParseElfError::ProgramHeaderSize { size: 56 })
        );
        assert_eq!(
            changed(45, 2),
            Err(ParseElfError::Truncated {
                what: "program headers"
            })
        );
        assert_eq!(
            Image::from_elf(&file[..40]),
            Err(ParseElfError::Truncated { what: "ELF header" })
        );
        assert_eq!(
            Image::from_elf(&file[..file.len() - 1]),
            Err(ParseElfError::Segment { index: 0 })
        );
        assert_eq!(
            Image::from_elf(&elf(0x100, &[(1, body(1), 0, 0, 0, 16)], &[])),
            Err(ParseElfError::Image(ImageError::Empty))
        );
        assert_eq!(
            changed(5, 1).unwrap_err().to_string(),
            "ELF data encoding 1, not big-endian (ELFDATA2MSB, 2)"
        );
    }
}
