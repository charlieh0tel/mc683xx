//! Memory the core serves itself: host RAM and ROM behind page tables.
//!
//! The hot path never leaves the core.  The chip builds the tables from
//! its address decode (the SIM's chip selects); a page with no backing
//! sends its cycles to the [`Bus`](crate::bus::Bus).

use std::collections::BTreeSet;
use std::fmt;
use std::mem;
use std::ops::Range;
use std::ops::RangeInclusive;

use crate::bus::PortSize;
use crate::bus::stall;
use crate::cycles::Cycles;
use mc683xx_target::address::Address;
use mc683xx_target::function_code::FunctionCode;
use mc683xx_target::size::Size;

/// Bytes in a page: the smallest chip-select block, 2K (SIMRM 7.3,
/// Table 7-2), so a page never straddles two blocks.
pub const PAGE_BYTES: u32 = 1 << PAGE_SHIFT;
/// log2 of [`PAGE_BYTES`].
const PAGE_SHIFT: u32 = 11;
/// Clocks in the fastest bus cycle (CPU32RM 8.1.5).
const FASTEST_CLOCKS: u8 = 2;

/// Whether the CPU may write a memory a host wires in: the chip's
/// builder's (`mc683xx_mc68331::chip::Builder::memory`) and a board's.
/// Non-exhaustive: a kind may be added (a write-once memory, say), and
/// [`writable`](Self::writable) answers for each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MemoryKind {
    /// Read-only: a write is ignored, as a ROM ignores one.
    Rom,
    /// Read and written.
    Ram,
}

impl MemoryKind {
    /// Whether the CPU writes it.
    #[must_use]
    pub const fn writable(self) -> bool {
        matches!(self, Self::Ram)
    }
}

/// The two address spaces outside CPU space that the chip-select
/// decode tells apart (SIMRM 7.10.3, the SPACE field).  Program and
/// data share one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Space {
    /// User space: FC2 clear.
    User,
    /// Supervisor space: FC2 set.
    Supervisor,
}

impl Space {
    /// The space a cycle of `function_code` is in.  CPU space never
    /// reaches the page tables.
    #[must_use]
    pub const fn of(function_code: FunctionCode) -> Self {
        if function_code.is_supervisor() {
            Self::Supervisor
        } else {
            Self::User
        }
    }

    /// Its page table's index.
    const fn index(self) -> usize {
        match self {
            Self::User => 0,
            Self::Supervisor => 1,
        }
    }
}

/// What a page lets cycles in one space do: read, write, fetch.
///
/// A cycle a page does not permit is an invalid access, ended in a bus
/// error ([PLAN.md, Memory]).  The host sets them; the chip's page
/// table rebuilds leave them.
///
/// [PLAN.md, Memory]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#memory
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Permissions(u8);

impl Permissions {
    /// No cycle.
    pub const NONE: Self = Self(0);
    /// Operand reads, exception processing's included.
    pub const READ: Self = Self(1);
    /// Operand writes, exception stacking's included.
    pub const WRITE: Self = Self(1 << 1);
    /// Instruction fetches.
    pub const EXECUTE: Self = Self(1 << 2);
    /// Every cycle: a page's permissions until the host sets others.
    pub const ALL: Self = Self(Self::READ.0 | Self::WRITE.0 | Self::EXECUTE.0);

    /// Those of `self` and of `other`.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether every one of `other` is among them.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

/// The hooks a page has, as bits: the core looks hooks up only on a
/// page whose bit is set ([PLAN.md, Hooks]).
///
/// [PLAN.md, Hooks]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#hooks
pub(crate) mod watch {
    /// A code hook covers some of it.
    pub(crate) const CODE: u8 = 1;
    /// A block hook covers some of it.
    pub(crate) const BLOCK: u8 = 1 << 1;
    /// A read hook covers some of it.
    pub(crate) const READ: u8 = 1 << 2;
    /// A write hook covers some of it.
    pub(crate) const WRITE: u8 = 1 << 3;
}

/// A region of host memory the core holds, as [`Memory::add_region`]
/// returns it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct RegionId(usize);

/// `region` and its number: `region 2`.
impl fmt::Display for RegionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "region {}", self.0)
    }
}

/// The bytes a page reads or writes: [`region`](Self::region) from
/// `offset` on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Backing {
    /// The region.
    pub region: RegionId,
    /// The byte offset in it of the page's first byte.
    pub offset: u32,
}

/// One page of an address space: where its reads and writes go.
///
/// It also says what a cycle there costs.  A read or write with no
/// backing goes to the [`Bus`](crate::bus::Bus); ROM is a page that
/// reads from a region and writes to the bus.  A host that lays out a
/// bare CPU's memory itself ([`Memory::map`]) builds one from
/// [`Page::BUS`]: `Page::BUS.reading(backing).writing(backing)`, then
/// [`timed`](Self::timed) for a memory slower or narrower than the
/// fastest 16-bit one.  Non-exhaustive, so a field added later gets a
/// default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct Page {
    /// Where reads go.
    pub read: Option<Backing>,
    /// Where writes go.
    pub write: Option<Backing>,
    /// The port width of the memory behind it.
    pub port: PortSize,
    /// Clocks in each of its bus cycles, wait states included: a
    /// `u8`, not [`Cycles`], to keep the page tables small (PLAN.md,
    /// Memory).
    clocks: u8,
}

impl Page {
    /// A page whose every cycle goes to the [`Bus`](crate::bus::Bus).
    pub const BUS: Self = Self {
        read: None,
        write: None,
        port: PortSize::Bits16,
        clocks: FASTEST_CLOCKS,
    };

    /// The longest bus cycle a page holds.
    pub const MOST_CLOCKS: Cycles = Cycles::from_clocks(u8::MAX as u64);

    /// The page, its reads going to `read`, or to the bus if `None`.
    #[must_use]
    pub const fn reading(self, read: Option<Backing>) -> Self {
        Self { read, ..self }
    }

    /// The page, its writes going to `write`, or to the bus if `None`.
    #[must_use]
    pub const fn writing(self, write: Option<Backing>) -> Self {
        Self { write, ..self }
    }

    /// The page, the memory behind it `port` wide, each of its bus
    /// cycles `clocks` long, wait states included.
    ///
    /// # Panics
    ///
    /// If `clocks` is more than [`Page::MOST_CLOCKS`]: no select's timing
    /// comes near it, an ECLK cycle, the longest, being 24 at most.
    #[must_use]
    pub const fn timed(self, port: PortSize, clocks: Cycles) -> Self {
        assert!(
            clocks.clocks() <= Self::MOST_CLOCKS.clocks(),
            "more clocks than a page holds"
        );
        Self {
            port,
            clocks: clocks.clocks() as u8,
            ..self
        }
    }

    /// Clocks in each of its bus cycles, wait states included.
    #[must_use]
    pub const fn clocks(&self) -> Cycles {
        Cycles::from_clocks(self.clocks as u64)
    }
}

/// What can go wrong building the page tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum MemoryError {
    /// An address width the tables cannot hold.
    #[error("{0} address bits: must be {min} to 32", min = PAGE_SHIFT + 1)]
    AddressBits(u32),
    /// A range that does not start and end on a page.
    #[error("{start}, {length:#x} bytes: not whole pages")]
    Unaligned {
        /// Its start.
        start: Address,
        /// Its length.
        length: u32,
    },
    /// A range past the end of the address space or of its region.
    #[error("{start}, {length:#x} bytes: out of range")]
    OutOfRange {
        /// Its start.
        start: Address,
        /// Its length.
        length: u32,
    },
}

/// A byte a host's cycle-free read or write cannot reach, the bare
/// machine's or the chip's: not memory the core serves (nothing mapped
/// there, or a peripheral's).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("no memory at {0}")]
pub struct Unserved(pub Address);

/// What a memory's regions hold, for tests that it frees what it
/// unmaps.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Held {
    /// The regions not removed.
    pub(crate) regions: usize,
    /// The regions' slots, removed ones included.
    pub(crate) slots: usize,
    /// The bytes they have allocated.
    pub(crate) bytes: usize,
}

/// log2 of the pages in a slab, the unit the tables are allocated in:
/// 1024 pages, 2 Mbytes of address space.
const SLAB_SHIFT: u32 = 10;
/// Pages in a slab.
const SLAB_PAGES: usize = 1 << SLAB_SHIFT;
/// A page number's bits within its slab.
const SLAB_MASK: usize = SLAB_PAGES - 1;

/// The page instruction fetch last read on the fast path, what it maps
/// to and what a word there costs: fetch looks no page table up while
/// the PC stays on it ([PLAN.md, Memory]).  It holds the mapping, not
/// the bytes, so fetch always reads memory as it is now; any change to
/// the tables drops it.
///
/// [PLAN.md, Memory]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#memory
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FetchPage {
    /// The space.
    space: Space,
    /// The page number, on the address lines.
    page: usize,
    /// The region its reads come from.
    region: usize,
    /// The byte offset in the region of the page's first byte.
    offset: usize,
    /// A word's stall there.
    stall: Cycles,
}

/// [`SLAB_PAGES`] pages of both spaces: their entries, the fast path's
/// copies of them, their permissions and their [`watch`] bits.  A slab
/// is allocated when a map or a protection first reaches it, so a wide
/// address space costs only what the host maps ([PLAN.md, Memory]).
///
/// [PLAN.md, Memory]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#memory
struct Slab {
    /// The entries of each [`Space`], by [`Space::index`].
    tables: [[Page; SLAB_PAGES]; 2],
    /// The entries the fast path serves: `tables`', but a page with a
    /// read or write hook or less than [`Permissions::ALL`] serves
    /// nothing, so its cycles take the slow path ([PLAN.md, Hooks]).
    ///
    /// [PLAN.md, Hooks]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#hooks
    fast: [[Page; SLAB_PAGES]; 2],
    /// Each page's permissions, by [`Space::index`].
    permissions: [[Permissions; SLAB_PAGES]; 2],
    /// Each page's [`watch`] bits, in both spaces.
    watched: [u8; SLAB_PAGES],
}

impl Slab {
    /// Every page on the bus, with every permission and no hook.  Built
    /// on the stack, about 230 Kbytes, then moved: a debug build on a
    /// small-stack thread needs a bigger one ([PLAN.md, Memory]).
    ///
    /// [PLAN.md, Memory]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#memory
    fn new() -> Box<Self> {
        Box::new(Self {
            tables: [[Page::BUS; SLAB_PAGES]; 2],
            fast: [[Page::BUS; SLAB_PAGES]; 2],
            permissions: [[Permissions::ALL; SLAB_PAGES]; 2],
            watched: [0; SLAB_PAGES],
        })
    }

    /// Brings page `index` of space `space`'s fast table up to date.
    fn refresh(&mut self, space: usize, index: usize) {
        let entry = self.tables[space][index];
        let slow = self.watched[index] & (watch::READ | watch::WRITE) != 0
            || self.permissions[space][index] != Permissions::ALL;
        self.fast[space][index] = if slow {
            Page {
                read: None,
                write: None,
                ..entry
            }
        } else {
            entry
        };
    }

    /// Whether any page has a hook or less than [`Permissions::ALL`].
    fn is_special(&self) -> bool {
        self.watched.iter().any(|&bits| bits != 0)
            || self
                .permissions
                .iter()
                .flatten()
                .any(|&permissions| permissions != Permissions::ALL)
    }
}

/// A slab as a snapshot holds it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SlabState {
    /// Its number: its first page's, over [`SLAB_PAGES`].
    index: usize,
    /// Its entries, by [`Space::index`].
    tables: [Vec<Page>; 2],
    /// Its permissions, by [`Space::index`].
    permissions: [Vec<Permissions>; 2],
}

/// Memory as a snapshot holds it: the regions and the slabs
/// allocated, not the hooks' bits.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct MemoryState {
    /// The regions.
    regions: Vec<Vec<u8>>,
    /// The slabs allocated, in order.
    slabs: Vec<SlabState>,
}

/// The core's memory: the host's regions and a page table per space.
///
/// Each page's permissions and hooks are beside it.  The tables are
/// two-level: a directory of slabs, each allocated as it is first
/// mapped or protected; a page in no slab is on the bus with every
/// permission.
pub struct Memory {
    /// The address lines the chip has: an address is masked to them,
    /// so the upper lines alias.
    pub(crate) address_mask: u32,
    /// The host's RAM and ROM, by [`RegionId`]; a freed one is empty.
    regions: Vec<Vec<u8>>,
    /// The slots of `regions` freed, for [`add_region`](Self::add_region)
    /// to reuse, lowest first, so the same maps and unmaps give the same
    /// [`RegionId`]s.
    free: BTreeSet<usize>,
    /// The pages on the address lines.
    pages: usize,
    /// The slabs, by number; [`None`] until one is mapped or protected.
    slabs: Vec<Option<Box<Slab>>>,
    /// The [`watch`] bits of each slab not allocated: those of the hooks
    /// covering any of it.
    unallocated: Vec<u8>,
    /// The hooks' address ranges with their [`watch`] bits, on the
    /// address lines, for a slab allocated after they were set.
    watches: Vec<(RangeInclusive<Address>, u8)>,
    /// Whether any page has a hook or less than [`Permissions::ALL`]:
    /// while not, no cycle or instruction looks either up.
    special: bool,
    /// The page instruction fetch is on, while the tables keep it.
    fetch_page: Option<FetchPage>,
}

impl fmt::Debug for Memory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Memory")
            .field("address_mask", &self.address_mask)
            .field(
                "regions",
                &self.regions.iter().map(Vec::len).collect::<Vec<_>>(),
            )
            .field("slabs", &self.slabs.iter().flatten().count())
            .finish_non_exhaustive()
    }
}

/// Sets in `watched`, the bits of the pages from `first` on, each of
/// `ranges`' bits over the pages its addresses reach, on lines `mask`.
fn watch_pages(
    watched: &mut [u8],
    first: usize,
    ranges: &[(RangeInclusive<Address>, u8)],
    mask: u32,
) {
    for (range, bit) in ranges {
        let (start, end) = range_pages(range, mask);
        let from = start.max(first);
        let to = end.min(first + watched.len() - 1);
        if from <= to {
            for bits in &mut watched[from - first..=to - first] {
                *bits |= bit;
            }
        }
    }
}

/// The first and last pages `range` reaches on lines `mask`.
fn range_pages(range: &RangeInclusive<Address>, mask: u32) -> (usize, usize) {
    let page = |address: Address| (address.0.min(mask) >> PAGE_SHIFT) as usize;
    (page(*range.start()), page(*range.end()))
}

impl Memory {
    /// Memory on `address_bits` address lines, every page on the bus.
    ///
    /// # Errors
    ///
    /// [`MemoryError::AddressBits`] for fewer than 12 address lines, a
    /// page and one more, or more than 32.
    pub fn new(address_bits: u32) -> Result<Self, MemoryError> {
        if !(PAGE_SHIFT + 1..=u32::BITS).contains(&address_bits) {
            return Err(MemoryError::AddressBits(address_bits));
        }
        let address_mask = u32::MAX >> (u32::BITS - address_bits);
        let pages: usize = 1 << (address_bits - PAGE_SHIFT);
        let slabs = pages.div_ceil(SLAB_PAGES);
        Ok(Self {
            address_mask,
            regions: Vec::new(),
            free: BTreeSet::new(),
            pages,
            slabs: (0..slabs).map(|_| None).collect(),
            unallocated: vec![0; slabs],
            watches: Vec::new(),
            special: false,
            fetch_page: None,
        })
    }

    /// `address` on the chip's address lines.
    #[must_use]
    pub const fn mask(&self, address: Address) -> Address {
        Address(address.0 & self.address_mask)
    }

    /// Takes `bytes` as a region the core serves, in the lowest slot a
    /// removed region left, if one did.
    pub fn add_region(&mut self, bytes: Vec<u8>) -> RegionId {
        if let Some(slot) = self.free.pop_first() {
            self.regions[slot] = bytes;
            return RegionId(slot);
        }
        self.regions.push(bytes);
        RegionId(self.regions.len() - 1)
    }

    /// Frees region `id`'s bytes; [`add_region`](Self::add_region) reuses
    /// its slot.  No page may be backed by it any more.
    pub(crate) fn remove_region(&mut self, id: RegionId) {
        self.fetch_page = None;
        self.regions[id.0] = Vec::new();
        self.free.insert(id.0);
    }

    /// Keeps `keep` of region `id`'s bytes and frees the rest: what was
    /// at `keep.start` is at offset zero now, so a page still backed by
    /// the region must be mapped again.
    pub(crate) fn trim_region(&mut self, id: RegionId, keep: Range<usize>) {
        self.fetch_page = None;
        let bytes = &mut self.regions[id.0];
        bytes.truncate(keep.end);
        bytes.drain(..keep.start);
        bytes.shrink_to_fit();
    }

    /// What the regions hold: the live ones, the slots, and the bytes
    /// allocated.
    #[cfg(test)]
    pub(crate) fn held(&self) -> Held {
        Held {
            regions: self.regions.len() - self.free.len(),
            slots: self.regions.len(),
            bytes: self.regions.iter().map(Vec::capacity).sum(),
        }
    }

    /// A region's bytes.
    ///
    /// # Panics
    ///
    /// If `id` is not one of this memory's regions.
    #[must_use]
    pub fn region(&self, id: RegionId) -> &[u8] {
        &self.regions[id.0]
    }

    /// A region's bytes, to change.
    ///
    /// # Panics
    ///
    /// If `id` is not one of this memory's regions.
    pub fn region_mut(&mut self, id: RegionId) -> &mut [u8] {
        &mut self.regions[id.0]
    }

    /// Every page of both spaces back on the bus.
    pub fn clear(&mut self) {
        self.fetch_page = None;
        for slab in self.slabs.iter_mut().flatten() {
            for table in slab.tables.iter_mut().chain(&mut slab.fast) {
                table.fill(Page::BUS);
            }
        }
    }

    /// Slab `index`, allocated now if it was not, with the hooks'
    /// bits.
    fn slab_mut(&mut self, index: usize) -> &mut Slab {
        let (watches, mask) = (&self.watches, self.address_mask);
        self.slabs[index].get_or_insert_with(|| {
            let mut slab = Slab::new();
            watch_pages(&mut slab.watched, index * SLAB_PAGES, watches, mask);
            slab
        })
    }

    /// Maps `length` bytes from `start` in `space` as `page`, each
    /// page's backing one page further into its region.
    ///
    /// # Errors
    ///
    /// [`MemoryError::Unaligned`] if the range is not whole pages, and
    /// [`MemoryError::OutOfRange`] if it passes the end of the address
    /// space.  A backing that runs past the end of its
    /// region is out of range too.
    ///
    /// # Panics
    ///
    /// If a backing's region is not one of this memory's.
    pub fn map(
        &mut self,
        space: Space,
        start: Address,
        length: u32,
        page: Page,
    ) -> Result<(), MemoryError> {
        let pages = self.pages(start, length)?;
        for backing in [page.read, page.write].into_iter().flatten() {
            let end = u64::from(backing.offset) + u64::from(length);
            if end > self.regions[backing.region.0].len() as u64 {
                return Err(MemoryError::OutOfRange { start, length });
            }
        }
        for (index, number) in pages.clone().enumerate() {
            let advance = |backing: Backing| Backing {
                offset: backing.offset + index as u32 * PAGE_BYTES,
                ..backing
            };
            let entry = Page {
                read: page.read.map(advance),
                write: page.write.map(advance),
                ..page
            };
            let slab = number >> SLAB_SHIFT;
            if entry == Page::BUS && self.slabs[slab].is_none() {
                continue;
            }
            self.slab_mut(slab).tables[space.index()][number & SLAB_MASK] = entry;
        }
        self.refresh(space.index(), pages);
        Ok(())
    }

    /// Brings `pages` of space `index`'s fast table up to date.
    fn refresh(&mut self, index: usize, pages: Range<usize>) {
        self.fetch_page = None;
        for page in pages {
            if let Some(slab) = self.slabs[page >> SLAB_SHIFT].as_deref_mut() {
                slab.refresh(index, page & SLAB_MASK);
            }
        }
    }

    /// Sets the permissions of `length` bytes from `start` in `space`.
    ///
    /// # Errors
    ///
    /// [`MemoryError::Unaligned`] if the range is not whole pages, and
    /// [`MemoryError::OutOfRange`] if it passes the end of the address
    /// space.
    pub fn protect(
        &mut self,
        space: Space,
        start: Address,
        length: u32,
        permissions: Permissions,
    ) -> Result<(), MemoryError> {
        let pages = self.pages(start, length)?;
        for page in pages.clone() {
            let slab = page >> SLAB_SHIFT;
            if permissions == Permissions::ALL && self.slabs[slab].is_none() {
                continue;
            }
            self.slab_mut(slab).permissions[space.index()][page & SLAB_MASK] = permissions;
        }
        self.update_special();
        self.refresh(space.index(), pages);
        Ok(())
    }

    /// The permissions of the page `address` is in.
    #[must_use]
    pub fn permissions(&self, space: Space, address: Address) -> Permissions {
        self.locate(address)
            .map_or(Permissions::ALL, |(slab, index, _)| {
                slab.permissions[space.index()][index]
            })
    }

    /// The page `address` is in, as its table entry maps it:
    /// [`Page::BUS`] if none is mapped.
    #[must_use]
    pub fn page(&self, space: Space, address: Address) -> Page {
        self.locate(address).map_or(Page::BUS, |(slab, index, _)| {
            slab.tables[space.index()][index]
        })
    }

    /// Whether `length` bytes from `start` are whole pages inside the
    /// address space, as [`map`](Self::map) and [`protect`](Self::protect)
    /// need.
    ///
    /// # Errors
    ///
    /// [`MemoryError::Unaligned`] if the range is not whole pages, and
    /// [`MemoryError::OutOfRange`] if it passes the end of the address
    /// space.
    pub fn check(&self, start: Address, length: u32) -> Result<(), MemoryError> {
        self.pages(start, length).map(|_| ())
    }

    /// The pages of `length` bytes from `start`: whole pages inside the
    /// address space.
    fn pages(&self, start: Address, length: u32) -> Result<Range<usize>, MemoryError> {
        if !start.0.is_multiple_of(PAGE_BYTES) || !length.is_multiple_of(PAGE_BYTES) {
            return Err(MemoryError::Unaligned { start, length });
        }
        let first = (start.0 >> PAGE_SHIFT) as usize;
        let count = (length >> PAGE_SHIFT) as usize;
        if first + count > self.pages {
            return Err(MemoryError::OutOfRange { start, length });
        }
        Ok(first..first + count)
    }

    /// Whether a cycle must look up its page's permissions and hooks.
    pub(crate) const fn is_special(&self) -> bool {
        self.special
    }

    /// The [`watch`] bits of the page `address` is in: of its slab, if
    /// that is not allocated.
    pub(crate) fn watched(&self, address: Address) -> u8 {
        let page = self.page_number(address);
        self.slabs[page >> SLAB_SHIFT]
            .as_deref()
            .map_or(self.unallocated[page >> SLAB_SHIFT], |slab| {
                slab.watched[page & SLAB_MASK]
            })
    }

    /// Sets the [`watch`] bits from the hooks' address ranges, each with
    /// its bit, on the address lines.
    pub(crate) fn set_watched(
        &mut self,
        ranges: impl Iterator<Item = (RangeInclusive<Address>, u8)>,
    ) {
        self.watches = ranges.collect();
        self.unallocated.fill(0);
        let mask = self.address_mask;
        for (range, bit) in &self.watches {
            let (first, last) = range_pages(range, mask);
            if first > last {
                continue;
            }
            for slab in first >> SLAB_SHIFT..=last >> SLAB_SHIFT {
                if self.slabs[slab].is_none() {
                    self.unallocated[slab] |= bit;
                }
            }
        }
        for (number, slab) in self.slabs.iter_mut().enumerate() {
            if let Some(slab) = slab.as_deref_mut() {
                slab.watched.fill(0);
                watch_pages(&mut slab.watched, number * SLAB_PAGES, &self.watches, mask);
            }
        }
        self.update_special();
        self.refresh_all();
    }

    /// Brings every allocated slab's fast tables up to date.
    fn refresh_all(&mut self) {
        self.fetch_page = None;
        for slab in self.slabs.iter_mut().flatten() {
            for space in 0..slab.fast.len() {
                for index in 0..SLAB_PAGES {
                    slab.refresh(space, index);
                }
            }
        }
    }

    /// The regions and the allocated slabs, for a snapshot.
    pub(crate) fn state(&self) -> MemoryState {
        MemoryState {
            regions: self.regions.clone(),
            slabs: self
                .slabs
                .iter()
                .enumerate()
                .filter_map(|(index, slab)| {
                    slab.as_deref().map(|slab| SlabState {
                        index,
                        tables: slab.tables.map(Vec::from),
                        permissions: slab.permissions.map(Vec::from),
                    })
                })
                .collect(),
        }
    }

    /// Takes a snapshot's regions, page tables and permissions, if they
    /// fit this memory: regions of the same sizes, slabs inside its
    /// address space, each backing inside its region.  What does not
    /// fit, if not.
    pub(crate) fn restore(&mut self, state: MemoryState) -> Result<(), &'static str> {
        let sizes = |regions: &[Vec<u8>]| regions.iter().map(Vec::len).collect::<Vec<_>>();
        if sizes(&state.regions) != sizes(&self.regions) {
            return Err("memory regions");
        }
        let regions = &state.regions;
        let backed = |page: &Page| {
            [page.read, page.write]
                .into_iter()
                .flatten()
                .all(|backing| {
                    regions.get(backing.region.0).is_some_and(|region| {
                        backing.offset as usize + PAGE_BYTES as usize <= region.len()
                    })
                })
        };
        let ascending = state
            .slabs
            .windows(2)
            .all(|pair| pair[0].index < pair[1].index);
        let fits = state.slabs.iter().all(|slab| {
            slab.index < self.slabs.len()
                && slab.tables.iter().all(|table| table.len() == SLAB_PAGES)
                && slab
                    .permissions
                    .iter()
                    .all(|table| table.len() == SLAB_PAGES)
                && slab.tables.iter().flatten().all(backed)
        });
        if !ascending || !fits {
            return Err("page tables");
        }
        self.regions = state.regions;
        self.slabs.iter_mut().for_each(|slab| *slab = None);
        for restored in state.slabs {
            let slab = self.slab_mut(restored.index);
            for (table, entries) in slab.tables.iter_mut().zip(&restored.tables) {
                table.copy_from_slice(entries);
            }
            for (table, entries) in slab.permissions.iter_mut().zip(&restored.permissions) {
                table.copy_from_slice(entries);
            }
        }
        let watches = mem::take(&mut self.watches);
        self.set_watched(watches.into_iter());
        Ok(())
    }

    /// Recomputes `special`.
    fn update_special(&mut self) {
        self.special = self.unallocated.iter().any(|&bits| bits != 0)
            || self.slabs.iter().flatten().any(|slab| slab.is_special());
    }

    /// The number of the page `address` is in.
    const fn page_number(&self, address: Address) -> usize {
        ((address.0 & self.address_mask) >> PAGE_SHIFT) as usize
    }

    /// The slab `address` is in, if allocated, the page's index in it
    /// and the address's offset in the page.
    #[inline]
    fn locate(&self, address: Address) -> Option<(&Slab, usize, usize)> {
        let address = self.mask(address).0;
        let page = (address >> PAGE_SHIFT) as usize;
        let slab = self.slabs[page >> SLAB_SHIFT].as_deref()?;
        Some((slab, page & SLAB_MASK, (address % PAGE_BYTES) as usize))
    }

    /// A byte, word or long the core serves itself, read with no bus
    /// cycle and no stall: what a hook sees of memory.  [`None`] if any
    /// of it is on the bus.  A word or long must be aligned.
    #[must_use]
    pub fn peek(&self, space: Space, address: Address, size: Size) -> Option<u32> {
        self.peek_in(space, address, size, |page| page.read)
    }

    /// What a write of a byte, word or long would replace: the bytes
    /// the pages write to, read with no bus cycle.  [`None`] if any of
    /// it writes to the bus.  A word or long must be aligned.
    pub(crate) fn peek_written(&self, space: Space, address: Address, size: Size) -> Option<u32> {
        self.peek_in(space, address, size, |page| page.write)
    }

    /// A byte, word or long of the bytes `backing` picks of each page,
    /// with no bus cycle: [`None`] if any of it has no such backing.  A
    /// word or long must be aligned.
    fn peek_in(
        &self,
        space: Space,
        address: Address,
        size: Size,
        backing: fn(&Page) -> Option<Backing>,
    ) -> Option<u32> {
        if size == Size::Long {
            let high = self.peek_in(space, address, Size::Word, backing)?;
            let low = self.peek_in(space, address.offset(2), Size::Word, backing)?;
            return Some(high << 16 | low);
        }
        let (slab, index, within) = self.locate(address)?;
        let backing = backing(&slab.tables[space.index()][index])?;
        Some(self.value_at(backing, within, size))
    }

    /// Fills `bytes` from `address` up with no bus cycle, a debugger's
    /// read: from the bytes the pages of supervisor space read,
    /// whatever their permissions.
    ///
    /// # Errors
    ///
    /// [`Unserved`] for the first byte whose page reads from the bus;
    /// `bytes` is then filled up to it.
    pub fn read_bytes(&self, address: Address, bytes: &mut [u8]) -> Result<(), Unserved> {
        let mut done = 0;
        while done < bytes.len() {
            let (region, from, count) = self.span(address, done, bytes.len(), |page| page.read)?;
            bytes[done..done + count].copy_from_slice(&self.region(region)[from..from + count]);
            done += count;
        }
        Ok(())
    }

    /// Writes `bytes` at `address` up with no bus cycle, a debugger's
    /// write: into the bytes each page of supervisor space reads, as
    /// [`read_bytes`](Self::read_bytes) does, a ROM's included, so a read
    /// gives back what was written and the CPU runs it; where a page
    /// reads from the bus, into the bytes it writes (write-only RAM).
    /// Where a page writes to other bytes than it reads, its write
    /// backing is left as it was ([PLAN.md, Memory]).
    ///
    /// # Errors
    ///
    /// [`Unserved`] for the first byte whose page neither reads nor
    /// writes host memory; then nothing is written.
    ///
    /// [PLAN.md, Memory]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#memory
    pub fn write_bytes(&mut self, address: Address, bytes: &[u8]) -> Result<(), Unserved> {
        let mut spans = Vec::new();
        let mut done = 0;
        while done < bytes.len() {
            let span = self.span(address, done, bytes.len(), |page| page.read.or(page.write))?;
            spans.push((span, done));
            done += span.2;
        }
        for ((region, from, count), done) in spans {
            self.region_mut(region)[from..from + count].copy_from_slice(&bytes[done..done + count]);
        }
        Ok(())
    }

    /// Where a debugger's access of `length` bytes from `address` finds
    /// its bytes from `done` on, within one page of supervisor space: the
    /// region of the backing `backing` picks of the page, the index in
    /// it, and how many of the bytes left the page holds.
    fn span(
        &self,
        address: Address,
        done: usize,
        length: usize,
        backing: fn(&Page) -> Option<Backing>,
    ) -> Result<(RegionId, usize, usize), Unserved> {
        let at = address.offset(done as u32);
        let backing = backing(&self.page(Space::Supervisor, at)).ok_or(Unserved(at))?;
        let within = self.mask(at).0 % PAGE_BYTES;
        let count = ((PAGE_BYTES - within) as usize).min(length - done);
        Ok((backing.region, (backing.offset + within) as usize, count))
    }

    /// A byte or word read the fast path serves, with its stall, or
    /// [`None`] if it takes the slow path or goes to the bus.  A word is
    /// aligned, so it never leaves a page.
    #[inline]
    pub(crate) fn read(&self, space: Space, address: Address, size: Size) -> Option<(u32, Cycles)> {
        let (slab, index, within) = self.locate(address)?;
        self.read_in(&slab.fast[space.index()][index], within, size)
    }

    /// A byte or word read served here, hooked page or not, with its
    /// stall, or [`None`] if it goes to the bus.
    pub(crate) fn read_backed(
        &self,
        space: Space,
        address: Address,
        size: Size,
    ) -> Option<(u32, Cycles)> {
        let (slab, index, within) = self.locate(address)?;
        self.read_in(&slab.tables[space.index()][index], within, size)
    }

    /// A read from `page`, `within` bytes into it.
    #[inline]
    fn read_in(&self, page: &Page, within: usize, size: Size) -> Option<(u32, Cycles)> {
        let value = self.value_at(page.read?, within, size);
        Some((value, stall(size, page.port, page.clocks())))
    }

    /// The byte or word `within` bytes into the page `backing` backs.
    #[inline]
    fn value_at(&self, backing: Backing, within: usize, size: Size) -> u32 {
        let bytes = &self.regions[backing.region.0][backing.offset as usize + within..];
        match size {
            Size::Byte => u32::from(bytes[0]),
            _ => u32::from(u16::from_be_bytes([bytes[0], bytes[1]])),
        }
    }

    /// An even long's two word reads, high word first, with their stall,
    /// if the fast path serves both from one page: [`None`] if it takes
    /// the slow path, goes to the bus or crosses a page, with no read
    /// made.
    #[inline]
    pub(crate) fn read_long(&self, space: Space, address: Address) -> Option<(u32, Cycles)> {
        let (slab, index, within) = self.locate(address)?;
        let page = &slab.fast[space.index()][index];
        let (backing, word) = (page.read?, stall(Size::Word, page.port, page.clocks()));
        let at = backing.offset as usize + Self::long_within(within)?;
        let bytes = &self.regions[backing.region.0][at..at + 4];
        Some((
            u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            word + word,
        ))
    }

    /// An even long's two word writes, high word first, with their
    /// stall, if the fast path serves both on one page, as
    /// [`read_long`](Self::read_long).
    #[inline]
    pub(crate) fn write_long(
        &mut self,
        space: Space,
        address: Address,
        value: u32,
    ) -> Option<Cycles> {
        let (slab, index, within) = self.locate(address)?;
        let page = slab.fast[space.index()][index];
        let (backing, word) = (page.write?, stall(Size::Word, page.port, page.clocks()));
        let at = backing.offset as usize + Self::long_within(within)?;
        self.regions[backing.region.0][at..at + 4].copy_from_slice(&value.to_be_bytes());
        Some(word + word)
    }

    /// `within`, if a long that far into a page ends in it.
    const fn long_within(within: usize) -> Option<usize> {
        if within <= PAGE_BYTES as usize - 4 {
            Some(within)
        } else {
            None
        }
    }

    /// The word an instruction fetch reads at `address`, even, with its
    /// stall, if the fast path serves it: [`read`](Self::read) of a word,
    /// through the cached page while the PC stays on it.  Always inlined:
    /// the run loop's opcode fetch is its hottest caller.
    #[inline(always)]
    pub(crate) fn fetch(&mut self, space: Space, address: Address) -> Option<(u16, Cycles)> {
        let address = self.mask(address).0;
        let page = (address >> PAGE_SHIFT) as usize;
        let cached = match self.fetch_page {
            Some(cached) if cached.page == page && cached.space == space => cached,
            _ => self.fetch_miss(space, page)?,
        };
        let at = cached.offset + (address % PAGE_BYTES) as usize;
        let bytes = &self.regions[cached.region][at..at + 2];
        Some((u16::from_be_bytes([bytes[0], bytes[1]]), cached.stall))
    }

    /// Caches `page` of `space` for fetch, if the fast path serves it.
    #[inline]
    fn fetch_miss(&mut self, space: Space, page: usize) -> Option<FetchPage> {
        let slab = self.slabs[page >> SLAB_SHIFT].as_deref()?;
        let entry = slab.fast[space.index()][page & SLAB_MASK];
        let backing = entry.read?;
        let cached = FetchPage {
            space,
            page,
            region: backing.region.0,
            offset: backing.offset as usize,
            stall: stall(Size::Word, entry.port, entry.clocks()),
        };
        self.fetch_page = Some(cached);
        Some(cached)
    }

    /// A byte or word write the fast path serves, with its stall, or
    /// [`None`] if it takes the slow path or goes to the bus.
    #[inline]
    pub(crate) fn write(
        &mut self,
        space: Space,
        address: Address,
        size: Size,
        value: u32,
    ) -> Option<Cycles> {
        let (slab, index, within) = self.locate(address)?;
        let page = slab.fast[space.index()][index];
        self.write_in(page, within, size, value)
    }

    /// Whether the tables serve writes at `address`, hooked page or not.
    pub(crate) fn write_served(&self, space: Space, address: Address) -> bool {
        self.locate(address)
            .is_some_and(|(slab, index, _)| slab.tables[space.index()][index].write.is_some())
    }

    /// A byte or word write served here, hooked page or not, with its
    /// stall, or [`None`] if it goes to the bus.
    pub(crate) fn write_backed(
        &mut self,
        space: Space,
        address: Address,
        size: Size,
        value: u32,
    ) -> Option<Cycles> {
        let (slab, index, within) = self.locate(address)?;
        let page = slab.tables[space.index()][index];
        self.write_in(page, within, size, value)
    }

    /// A write to `page`, `within` bytes into it.
    #[inline]
    fn write_in(&mut self, page: Page, within: usize, size: Size, value: u32) -> Option<Cycles> {
        let (backing, cost) = (page.write?, stall(size, page.port, page.clocks()));
        let bytes = &mut self.regions[backing.region.0][backing.offset as usize + within..];
        match size {
            Size::Byte => bytes[0] = value as u8,
            _ => bytes[..2].copy_from_slice(&(value as u16).to_be_bytes()),
        }
        Some(cost)
    }
}

#[cfg(test)]
mod tests {
    use super::Backing;
    use super::Memory;
    use super::MemoryError;
    use super::PAGE_BYTES;
    use super::Page;
    use super::Space;
    use super::Unserved;
    use crate::bus::PortSize;
    use crate::cycles::Cycles;
    use mc683xx_target::address::Address;
    use mc683xx_target::size::Size;

    fn ram(memory: &mut Memory, at: u32, bytes: u32) {
        let region = memory.add_region(vec![0; bytes as usize]);
        let backing = Some(Backing { region, offset: 0 });
        let page = Page::BUS
            .reading(backing)
            .writing(backing)
            .timed(PortSize::Bits16, Cycles::from_clocks(2));
        memory
            .map(Space::Supervisor, Address(at), bytes, page)
            .unwrap();
    }

    #[test]
    fn fetch_sees_writes_and_remaps_through_its_page() {
        let mut memory = Memory::new(24).unwrap();
        ram(&mut memory, 0, PAGE_BYTES);
        let at = Address(0x10);
        assert_eq!(memory.fetch(Space::Supervisor, at), Some((0, Cycles::ZERO)));
        memory.write(Space::Supervisor, at, Size::Word, 0x4e71);
        assert_eq!(
            memory.fetch(Space::Supervisor, at),
            Some((0x4e71, Cycles::ZERO))
        );
        assert_eq!(memory.fetch(Space::User, at), None, "user space is the bus");
        ram(&mut memory, 0, PAGE_BYTES);
        assert_eq!(memory.fetch(Space::Supervisor, at), Some((0, Cycles::ZERO)));
        memory.clear();
        assert_eq!(memory.fetch(Space::Supervisor, at), None);
    }

    #[test]
    fn a_debugger_write_lands_where_its_read_reads() {
        let mut memory = Memory::new(24).unwrap();
        let rom = memory.add_region(vec![0; PAGE_BYTES as usize]);
        let ram = memory.add_region(vec![0; PAGE_BYTES as usize]);
        assert_eq!(
            (rom.to_string(), ram.to_string()),
            ("region 0".to_owned(), "region 1".to_owned())
        );
        let backing = |region| Some(Backing { region, offset: 0 });
        // a ROM that reads, the RAM under it that takes the writes
        let page = Page::BUS.reading(backing(rom)).writing(backing(ram));
        memory
            .map(Space::Supervisor, Address(0), PAGE_BYTES, page)
            .unwrap();
        let only_writes = Page::BUS.writing(backing(ram));
        memory
            .map(
                Space::Supervisor,
                Address(PAGE_BYTES),
                PAGE_BYTES,
                only_writes,
            )
            .unwrap();
        memory.write_bytes(Address(0x10), &[0x4e, 0x71]).unwrap();
        let mut read = [0; 2];
        memory.read_bytes(Address(0x10), &mut read).unwrap();
        assert_eq!(read, [0x4e, 0x71]);
        assert_eq!(memory.region(rom)[0x10..0x12], [0x4e, 0x71]);
        assert_eq!(memory.region(ram)[0x10..0x12], [0, 0]);
        // a page whose reads go to the bus takes it in the bytes it
        // writes, with none to read back
        let at = Address(PAGE_BYTES);
        memory.write_bytes(at.offset(0x20), &[0x12, 0x34]).unwrap();
        assert_eq!(memory.region(ram)[0x20..0x22], [0x12, 0x34]);
        assert_eq!(memory.read_bytes(at, &mut read), Err(Unserved(at)));
        // one crossing from the split page into it takes each page's
        let last = Address(PAGE_BYTES - 1);
        memory.write_bytes(last, &[0xaa, 0xbb]).unwrap();
        assert_eq!(memory.region(rom)[PAGE_BYTES as usize - 1], 0xaa);
        assert_eq!(memory.region(ram)[0], 0xbb);
        assert_eq!(memory.region(ram)[PAGE_BYTES as usize - 1], 0);
    }

    #[test]
    fn a_range_refused_prints_its_start_as_an_address() {
        let unaligned = MemoryError::Unaligned {
            start: Address(0x10),
            length: 0x20,
        };
        assert_eq!(
            unaligned.to_string(),
            "0x00000010, 0x20 bytes: not whole pages"
        );
    }

    #[test]
    fn a_debugger_write_to_a_page_all_on_the_bus_writes_nothing() {
        let mut memory = Memory::new(24).unwrap();
        ram(&mut memory, 0, PAGE_BYTES);
        let at = Address(PAGE_BYTES);
        assert_eq!(memory.write_bytes(at, &[1]), Err(Unserved(at)));
        let mut read = [0; 1];
        assert_eq!(memory.read_bytes(at, &mut read), Err(Unserved(at)));
        // one crossing into it writes nothing, the served page's
        // byte included
        let last = Address(PAGE_BYTES - 1);
        assert_eq!(memory.write_bytes(last, &[1, 2]), Err(Unserved(at)));
        memory.read_bytes(last, &mut read).unwrap();
        assert_eq!(read, [0]);
    }

    #[test]
    fn ram_reads_back_big_endian() {
        let mut memory = Memory::new(24).unwrap();
        ram(&mut memory, 0x10_0000, 2 * PAGE_BYTES);
        let at = Address(0x10_0800);
        assert_eq!(
            memory.write(Space::Supervisor, at, Size::Word, 0x1234),
            Some(Cycles::ZERO)
        );
        assert_eq!(
            memory.read(Space::Supervisor, at.offset(1), Size::Byte),
            Some((0x34, Cycles::ZERO))
        );
    }

    #[test]
    fn unmapped_and_other_space_go_to_the_bus() {
        let mut memory = Memory::new(24).unwrap();
        ram(&mut memory, 0, PAGE_BYTES);
        assert_eq!(memory.read(Space::User, Address(0), Size::Word), None);
        assert_eq!(
            memory.read(Space::Supervisor, Address(PAGE_BYTES), Size::Word),
            None
        );
    }

    #[test]
    fn upper_address_lines_alias() {
        let mut memory = Memory::new(24).unwrap();
        ram(&mut memory, 0, PAGE_BYTES);
        memory.write(Space::Supervisor, Address(0xff00_0002), Size::Word, 0xbeef);
        assert_eq!(
            memory.read(Space::Supervisor, Address(2), Size::Word),
            Some((0xbeef, Cycles::ZERO))
        );
    }

    #[test]
    fn maps_must_be_whole_pages_inside_their_region() {
        let mut memory = Memory::new(24).unwrap();
        let region = memory.add_region(vec![0; PAGE_BYTES as usize]);
        let page = Page {
            read: Some(Backing { region, offset: 0 }),
            ..Page::BUS
        };
        assert!(matches!(
            memory.map(Space::User, Address(1), PAGE_BYTES, page),
            Err(MemoryError::Unaligned { .. })
        ));
        assert!(matches!(
            memory.map(Space::User, Address(0), 2 * PAGE_BYTES, page),
            Err(MemoryError::OutOfRange { .. })
        ));
    }

    #[test]
    fn thirty_two_lines_allocate_only_what_is_mapped() {
        let mut memory = Memory::new(32).unwrap();
        let high = 0x3140_0000;
        ram(&mut memory, high, PAGE_BYTES);
        assert_eq!(memory.slabs.iter().flatten().count(), 1);
        memory.write(Space::Supervisor, Address(high + 4), Size::Word, 0x4e75);
        assert_eq!(
            memory.read(Space::Supervisor, Address(high + 4), Size::Word),
            Some((0x4e75, Cycles::ZERO))
        );
        assert_eq!(
            memory.read(Space::Supervisor, Address(0xffff_f000), Size::Word),
            None
        );
        assert_eq!(
            memory.permissions(Space::User, Address(0)),
            super::Permissions::ALL
        );
    }

    #[test]
    fn a_slab_mapped_after_its_hooks_takes_their_bits() {
        let mut memory = Memory::new(32).unwrap();
        let hooked = Address(0x0040_0800);
        memory.set_watched([(hooked..=hooked, super::watch::WRITE)].into_iter());
        assert!(memory.is_special());
        assert_eq!(memory.watched(Address(0x0040_1000)), super::watch::WRITE);
        ram(&mut memory, 0x0040_0000, 2 * PAGE_BYTES);
        assert_eq!(memory.watched(hooked), super::watch::WRITE);
        assert_eq!(memory.watched(Address(0x0040_0000)), 0);
        assert_eq!(
            memory.write(Space::Supervisor, hooked, Size::Byte, 1),
            None,
            "a hooked page takes the slow path"
        );
        assert_eq!(
            memory.write(Space::Supervisor, Address(0x0040_0000), Size::Byte, 1),
            Some(Cycles::ZERO)
        );
    }

    #[test]
    fn a_state_restores_its_slabs() {
        let mut memory = Memory::new(32).unwrap();
        ram(&mut memory, 0x8000_0000, PAGE_BYTES);
        memory.write(Space::Supervisor, Address(0x8000_0000), Size::Word, 0x1234);
        let state = memory.state();
        memory.clear();
        let backing = Some(Backing {
            region: super::RegionId(0),
            offset: 0,
        });
        let page = Page {
            read: backing,
            ..Page::BUS
        };
        memory
            .map(Space::Supervisor, Address(0), PAGE_BYTES, page)
            .unwrap();
        memory.restore(state).unwrap();
        assert_eq!(
            memory.peek(Space::Supervisor, Address(0x8000_0000), Size::Word),
            Some(0x1234)
        );
        assert_eq!(memory.peek(Space::Supervisor, Address(0), Size::Word), None);
    }
}
