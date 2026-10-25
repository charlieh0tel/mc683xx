//! Memory with every byte defined, for differential tests: what was
//! written, over a background derived from a seed.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::random::mix_at;

/// The address lines the 68331 drives, ADDR[23:0] (MC68331UM 4.4.1.1).
/// The oracles alias above them, as the core does on the chip.
pub const ADDRESS_MASK: u32 = 0x00ff_ffff;

/// A 24-bit address space whose unwritten bytes come from a seed, so a
/// test needs no image and both sides of a comparison read the same
/// byte wherever they read.  Writes are kept in address order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SparseMemory {
    /// What the unwritten bytes derive from.
    seed: u64,
    /// The bytes written, by address.
    written: BTreeMap<u32, u8>,
}

impl SparseMemory {
    /// Memory whose every byte is the seed's background.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self {
            seed,
            written: BTreeMap::new(),
        }
    }

    /// The byte at `address`.
    #[must_use]
    pub fn read_byte(&self, address: u32) -> u8 {
        let address = address & ADDRESS_MASK;
        self.written
            .get(&address)
            .copied()
            .unwrap_or_else(|| self.background(address))
    }

    /// Writes the byte at `address`.
    pub fn write_byte(&mut self, address: u32, value: u8) {
        self.written.insert(address & ADDRESS_MASK, value);
    }

    /// The big-endian word at `address`.
    #[must_use]
    pub fn read_word(&self, address: u32) -> u16 {
        u16::from_be_bytes([
            self.read_byte(address),
            self.read_byte(address.wrapping_add(1)),
        ])
    }

    /// Writes the big-endian word at `address`.
    pub fn write_word(&mut self, address: u32, value: u16) {
        let [high, low] = value.to_be_bytes();
        self.write_byte(address, high);
        self.write_byte(address.wrapping_add(1), low);
    }

    /// Each address either side wrote whose bytes differ: the address,
    /// `self`'s byte (expected) and `actual`'s.
    #[must_use]
    pub fn differences(&self, actual: &Self) -> Vec<(u32, u8, u8)> {
        let mut addresses: Vec<u32> = self
            .written
            .keys()
            .chain(actual.written.keys())
            .copied()
            .collect();
        addresses.sort_unstable();
        addresses.dedup();
        addresses
            .into_iter()
            .map(|address| (address, self.read_byte(address), actual.read_byte(address)))
            .filter(|(_, expected, actual)| expected != actual)
            .collect()
    }

    /// The unwritten byte at `address`.
    const fn background(&self, address: u32) -> u8 {
        mix_at(self.seed, address as u64) as u8
    }
}

/// What a region of a `RegionMemory` holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Contents {
    /// Read only, and shared: a ROM image every call of a run maps
    /// without a copy.
    ReadOnly(Arc<[u8]>),
    /// Writable, a call's own.
    Writable(Vec<u8>),
}

/// A run of addresses a `RegionMemory` maps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    /// Its first address.
    pub start: u32,
    /// Its bytes, from `start`.
    pub contents: Contents,
}

impl Region {
    /// Its bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        match &self.contents {
            Contents::ReadOnly(bytes) => bytes,
            Contents::Writable(bytes) => bytes,
        }
    }

    /// Whether it holds `address`.
    fn holds(&self, address: u32) -> bool {
        address
            .checked_sub(self.start)
            .is_some_and(|offset| (offset as usize) < self.bytes().len())
    }
}

/// An access a `RegionMemory` refused: to an address no region holds,
/// or a write to a read-only one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fault {
    /// The address.
    pub address: u32,
    /// Whether it was a write.
    pub write: bool,
}

/// A byte of a device register a routine polls, which reads as some
/// bits of what was last written there and others set: a device that is
/// always ready.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Polled {
    /// Its address.
    pub address: u32,
    /// The bits a read gives as last written.
    pub kept: u8,
    /// The bits a read gives set.
    pub set: u8,
}

/// A 24-bit address space of regions, as a board maps memory: what the
/// regions hold, and nothing elsewhere.  A refused access reads zero,
/// changes nothing and is kept, the first of them, for the caller to
/// end its run on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegionMemory {
    /// The regions, apart.
    regions: Vec<Region>,
    /// The polled bytes, in writable regions.
    polled: Vec<Polled>,
    /// The first access refused.
    fault: Option<Fault>,
}

impl RegionMemory {
    /// Memory of `regions`, which must not overlap.
    #[must_use]
    pub const fn new(regions: Vec<Region>) -> Self {
        Self {
            regions,
            polled: Vec::new(),
            fault: None,
        }
    }

    /// `self`, its reads of `polled`'s bytes as they say.
    #[must_use]
    pub fn with_polled(self, polled: Vec<Polled>) -> Self {
        Self { polled, ..self }
    }

    /// The polled bytes.
    #[must_use]
    pub fn polled(&self) -> &[Polled] {
        &self.polled
    }

    /// The regions.
    #[must_use]
    pub fn regions(&self) -> &[Region] {
        &self.regions
    }

    /// The first access refused, if one was.
    #[must_use]
    pub const fn fault(&self) -> Option<Fault> {
        self.fault
    }

    /// The byte at `address`, if a region holds it, without a fault.
    #[must_use]
    pub fn peek(&self, address: u32) -> Option<u8> {
        let address = address & ADDRESS_MASK;
        self.regions
            .iter()
            .find(|region| region.holds(address))
            .map(|region| region.bytes()[(address - region.start) as usize])
    }

    /// The big-endian word at `address`, if regions hold it, without a
    /// fault.
    #[must_use]
    pub fn peek_word(&self, address: u32) -> Option<u16> {
        Some(u16::from_be_bytes([
            self.peek(address)?,
            self.peek(address.wrapping_add(1))?,
        ]))
    }

    /// The byte at `address`; zero and a fault where no region is.
    pub fn read_byte(&mut self, address: u32) -> u8 {
        let Some(byte) = self.peek(address) else {
            self.refuse(address, false);
            return 0;
        };
        let address = address & ADDRESS_MASK;
        self.polled
            .iter()
            .find(|polled| polled.address == address)
            .map_or(byte, |polled| byte & polled.kept | polled.set)
    }

    /// Writes the byte at `address`; a fault where no writable region
    /// is.
    pub fn write_byte(&mut self, address: u32, value: u8) {
        let address = address & ADDRESS_MASK;
        let region = self.regions.iter_mut().find(|region| region.holds(address));
        match region.map(|region| (region.start, &mut region.contents)) {
            Some((start, Contents::Writable(bytes))) => bytes[(address - start) as usize] = value,
            _ => self.refuse(address, true),
        }
    }

    /// The big-endian word at `address`.
    pub fn read_word(&mut self, address: u32) -> u16 {
        u16::from_be_bytes([
            self.read_byte(address),
            self.read_byte(address.wrapping_add(1)),
        ])
    }

    /// Writes the big-endian word at `address`.
    pub fn write_word(&mut self, address: u32, value: u16) {
        let [high, low] = value.to_be_bytes();
        self.write_byte(address, high);
        self.write_byte(address.wrapping_add(1), low);
    }

    /// The first address where `self`'s writable regions and `other`'s
    /// differ, with `self`'s byte (expected) and `other`'s, if one
    /// does.  The two must map the same regions.
    #[must_use]
    pub fn first_difference(&self, other: &Self) -> Option<(u32, u8, u8)> {
        self.regions
            .iter()
            .zip(&other.regions)
            .filter(|(region, _)| matches!(region.contents, Contents::Writable(_)))
            .find_map(|(expected, actual)| {
                let at = expected
                    .bytes()
                    .iter()
                    .zip(actual.bytes())
                    .position(|(expected, actual)| expected != actual)?;
                Some((
                    expected.start + at as u32,
                    expected.bytes()[at],
                    actual.bytes()[at],
                ))
            })
    }

    /// Keeps the first refused access, on the 24 address lines.
    fn refuse(&mut self, address: u32, write: bool) {
        self.fault.get_or_insert(Fault {
            address: address & ADDRESS_MASK,
            write,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::Contents;
    use super::Fault;
    use super::Polled;
    use super::Region;
    use super::RegionMemory;
    use super::SparseMemory;

    #[test]
    fn reads_are_stable_and_writes_stick() {
        let mut memory = SparseMemory::new(7);
        let before = memory.read_word(0x1234);
        assert_eq!(memory.read_word(0x1234), before);
        memory.write_word(0x1234, 0xbeef);
        assert_eq!(memory.read_word(0x1234), 0xbeef);
        assert_eq!(memory.read_byte(0xff00_1235), 0xef);
    }

    #[test]
    fn differences_cover_both_sides_writes() {
        let expected = SparseMemory::new(1);
        let mut actual = expected.clone();
        let old = actual.read_byte(10);
        actual.write_byte(10, !old);
        assert_eq!(expected.differences(&actual), [(10, old, !old)]);
        actual.write_byte(10, old);
        assert!(expected.differences(&actual).is_empty());
    }

    fn regions() -> RegionMemory {
        RegionMemory::new(vec![
            Region {
                start: 0,
                contents: Contents::ReadOnly(Arc::from([0x12, 0x34].as_slice())),
            },
            Region {
                start: 0x100,
                contents: Contents::Writable(vec![0; 4]),
            },
        ])
    }

    #[test]
    fn regions_read_write_and_refuse() {
        let mut memory = regions();
        assert_eq!(memory.read_word(0), 0x1234);
        memory.write_word(0x102, 0xbeef);
        assert_eq!(memory.read_word(0x102), 0xbeef);
        assert_eq!(memory.fault(), None);
        memory.write_byte(1, 0);
        assert_eq!(memory.read_byte(1), 0x34);
        assert_eq!(
            memory.fault(),
            Some(Fault {
                address: 1,
                write: true
            })
        );
        assert_eq!(memory.read_byte(0x104), 0, "past the region");
        assert_eq!(memory.peek_word(0x103), None);
    }

    #[test]
    fn a_polled_byte_reads_its_bits() {
        let mut memory = regions().with_polled(vec![Polled {
            address: 0x101,
            kept: 0x0f,
            set: 0x80,
        }]);
        memory.write_byte(0x101, 0x35);
        assert_eq!(memory.read_byte(0x101), 0x85);
        assert_eq!(memory.peek(0x101), Some(0x35));
    }

    #[test]
    fn first_difference_is_in_writable_regions() {
        let expected = regions();
        let mut actual = expected.clone();
        assert_eq!(expected.first_difference(&actual), None);
        actual.write_byte(0x103, 7);
        actual.write_byte(0x101, 5);
        assert_eq!(expected.first_difference(&actual), Some((0x101, 0, 5)));
    }
}
