//! Chip-select decode (SIMRM 7; MC68331UM 4.8): which of the twelve
//! selects a bus cycle asserts, from CSPAR0-1, CSBARBT, CSBAR0-10,
//! CSORBT and CSOR0-10, and how each would end the cycle.

use mc683xx_cpu32::bus::PortSize;
use mc683xx_cpu32::function_code::FunctionCode;
use mc683xx_cpu32::size::Size;

use crate::chip_select::ChipSelect;

/// All twelve selects, CSBOOT first.
const SELECTS: usize = ChipSelect::COUNT;

/// Pin assignment field encodings (MC68331UM Table 4-20): a chip
/// select for an 8-bit port.
const PIN_8_BIT: u16 = 0b10;
/// A chip select for a 16-bit port.
const PIN_16_BIT: u16 = 0b11;
/// A discrete output (MC68331UM Table 4-20).
const PIN_DISCRETE: u16 = 0b00;
/// A pin assignment field's bits.
const PIN_FIELD_BITS: u16 = 2;
/// A pin assignment field, unshifted.
const PIN_FIELD_MASK: u16 = 0b11;
/// The selects CSPAR0 assigns: CSBOOT and CS0 to CS5 (MC68331UM Table
/// D-8); CSPAR1 assigns CS6 to CS10 (Table D-9).
const CSPAR0_SELECTS: usize = 7;
/// CSPAR0's implemented bits, 13:0 (MC68331UM D.3.23).
const CSPAR0_IMPLEMENTED: u16 = 0x3fff;
/// CSPAR0 bit 1, the CSBOOT field's high bit: always one.
const CSPAR0_CSBOOT_SELECT: u16 = 0b10;
/// CS3's index among the selects: CS3 to CS9 carry PC0 to PC6
/// (MC68331UM 4.8.1.4).
const PORTC_FIRST_SELECT: usize = 4;
/// Port C's pins, PC0 to PC6.
const PORTC_PINS: usize = 7;
/// CSPAR1's implemented bits, 9:0 (MC68331UM D.3.24).
const CSPAR1_IMPLEMENTED: u16 = 0x03ff;
/// DATA\[7:3\]: CSPAR1's reset pins, DATA3 for CS6 (SIMRM Table 7-7).
const CSPAR1_FIRST_DATA: u32 = 3;
/// The highest data pin CSPAR1's reset reads.
const CSPAR1_LAST_DATA: u32 = 7;
/// DATA pins and the CSPAR0 fields each sets at reset (SIMRM Table
/// 7-6): DATA1 for CS0 to CS2, DATA2 for CS3 to CS5.
const CSPAR0_DATA: [u32; CSPAR0_SELECTS - 1] = [1, 1, 1, 2, 2, 2];
/// DATA0 sets CSBOOT's port size (SIMRM 7.9.3).
const CSBOOT_DATA: u32 = 0;

/// CSBAR's base address field, bits 15:3: ADDR\[23:11\] (MC68331UM
/// D.3.25).
const CSBAR_BASE_MASK: u16 = 0xfff8;
/// The base field's shift to an address: bit 3 is ADDR11.
const CSBAR_BASE_SHIFT: u32 = 8;
/// CSBAR BLKSZ, bits 2:0.
const CSBAR_BLKSZ_MASK: u16 = 0b111;
/// Block sizes by BLKSZ (MC68331UM Table 4-21).
const BLOCK_BYTES: [u32; 8] = [
    2 << 10,
    8 << 10,
    16 << 10,
    64 << 10,
    128 << 10,
    256 << 10,
    512 << 10,
    1 << 20,
];
/// CSBARBT out of reset: base 0, 1 Mbyte (MC68331UM Table 4-24).
const CSBARBT_RESET: u16 = 0x0007;
/// The address lines ADDR\[23:0\] the decode compares.
const ADDRESS_MASK: u32 = 0x00ff_ffff;

/// CSOR MODE: synchronized to ECLK (MC68331UM D.3.26).
const CSOR_MODE: u16 = 1 << 15;
/// CSOR BYTE, bits 14:13.
const CSOR_BYTE_SHIFT: u16 = 13;
/// CSOR R/W, bits 12:11.
const CSOR_RW_SHIFT: u16 = 11;
/// CSOR DSACK, bits 9:6.
const CSOR_DSACK_SHIFT: u16 = 6;
/// CSOR DSACK, unshifted.
const CSOR_DSACK_MASK: u16 = 0b1111;
/// CSOR SPACE, bits 5:4.
const CSOR_SPACE_SHIFT: u16 = 4;
/// CSOR IPL, bits 3:1.
const CSOR_IPL_SHIFT: u16 = 1;
/// CSOR IPL, unshifted.
const CSOR_IPL_MASK: u16 = 0b111;
/// CSOR AVEC: autovector an interrupt acknowledge.
const CSOR_AVEC: u16 = 1;
/// A two-bit CSOR field, unshifted.
const CSOR_FIELD_MASK: u16 = 0b11;
/// BYTE: the lower byte, DATA\[7:0\], odd addresses (MC68331UM Table
/// 4-22).
const BYTE_LOWER: u16 = 0b01;
/// BYTE: the upper byte, DATA\[15:8\], even addresses.
const BYTE_UPPER: u16 = 0b10;
/// R/W: reads.
const RW_READ: u16 = 0b01;
/// R/W: writes.
const RW_WRITE: u16 = 0b10;
/// SPACE: CPU space.
const SPACE_CPU: u16 = 0b00;
/// SPACE: user space.
const SPACE_USER: u16 = 0b01;
/// SPACE: supervisor space.
const SPACE_SUPERVISOR: u16 = 0b10;
/// DSACK: fast termination.
const DSACK_FAST: u16 = 0b1110;
/// DSACK: the peripheral asserts it.
const DSACK_EXTERNAL: u16 = 0b1111;
/// CSORBT out of reset: asynchronous, both bytes, read and write, AS,
/// 13 wait states, supervisor and user, any level, no autovector
/// (MC68331UM Table 4-24; SIMRM Table 8-4).
const CSORBT_RESET: u16 = 0x7b70;

/// CPU space type in ADDR\[19:16\] (MC68331UM 4.5.4, Figure 4-11).
const CPU_SPACE_TYPE_SHIFT: u32 = 16;
/// The CPU space type field, unshifted.
const CPU_SPACE_TYPE_MASK: u32 = 0xf;
/// The interrupt acknowledge CPU space type.
const CPU_SPACE_IACK: u32 = 0xf;
/// The level in an interrupt acknowledge's ADDR\[3:1\].
const IACK_LEVEL_SHIFT: u32 = 1;
/// The level, unshifted.
const IACK_LEVEL_MASK: u32 = 0b111;
/// Clocks in a bus cycle with no wait states and fast termination
/// (SIMRM 7.5.1, 7.7; MC68331UM 4.5).
const ZERO_WAIT_CLOCKS: u8 = 3;
/// Clocks in a fast-termination cycle.
const FAST_CLOCKS: u8 = 2;

/// A bus cycle the selects decode.
///
/// Not part of the semver API: plumbing `mc683xx-mc68331` assembles the chip with.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeRequest {
    /// ADDR\[23:0\]; higher bits are ignored.
    pub address: u32,
    /// The cycle's function code.
    pub function_code: FunctionCode,
    /// Whether it writes.
    pub write: bool,
    /// Byte or word.
    pub size: Size,
}

impl DecodeRequest {
    /// The byte lanes of a 16-bit port the cycle uses: a word both, a
    /// byte the upper at an even address, the lower at an odd (SIMRM
    /// Table 7-5).
    const fn lanes(self) -> Lanes {
        match self.size {
            Size::Byte => {
                let odd = self.address & 1 != 0;
                Lanes {
                    upper: !odd,
                    lower: odd,
                }
            }
            _ => Lanes::BOTH,
        }
    }

    /// The interrupt level an interrupt acknowledge cycle carries, if
    /// it is one.
    const fn acknowledge_level(self) -> Option<u8> {
        if self.function_code.is_cpu_space()
            && self.address >> CPU_SPACE_TYPE_SHIFT & CPU_SPACE_TYPE_MASK == CPU_SPACE_IACK
        {
            Some((self.address >> IACK_LEVEL_SHIFT & IACK_LEVEL_MASK) as u8)
        } else {
            None
        }
    }
}

/// The data bus byte lanes a select takes part on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Lanes {
    /// DATA\[15:8\].
    pub upper: bool,
    /// DATA\[7:0\].
    pub lower: bool,
}

impl Lanes {
    /// Both lanes.
    pub const BOTH: Self = Self {
        upper: true,
        lower: true,
    };

    /// The lanes in both.
    #[must_use]
    pub const fn and(self, other: Self) -> Self {
        Self {
            upper: self.upper && other.upper,
            lower: self.lower && other.lower,
        }
    }

    /// The lanes in either.
    #[must_use]
    pub const fn or(self, other: Self) -> Self {
        Self {
            upper: self.upper || other.upper,
            lower: self.lower || other.lower,
        }
    }

    /// Whether these include every lane of `other`.
    #[must_use]
    pub const fn covers(self, other: Self) -> bool {
        (self.upper || !other.upper) && (self.lower || !other.lower)
    }

    /// Whether there are none.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        !self.upper && !self.lower
    }
}

/// How a select would end a cycle it matches (SIMRM 7.5, 7.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Termination {
    /// Internal DSACK after two clocks.
    Fast,
    /// Internal DSACK after this many wait states, 0 to 13.
    WaitStates(u8),
    /// The peripheral asserts DSACK.
    External,
    /// MODE set: the cycle runs on ECLK.
    Eclk,
}

impl Termination {
    /// The most wait states DSACK encodes: the field's values below
    /// fast termination's.
    pub const MOST_WAIT_STATES: u8 = DSACK_FAST as u8 - 1;

    /// The cycle's clocks, if the select ends it: ECLK cycles are
    /// charged an ECLK period and half again, the average wait to
    /// synchronize being half a period (our choice, [PLAN.md]).
    ///
    /// [PLAN.md]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md
    #[must_use]
    pub const fn clocks(self, eclk_divisor: u64) -> Option<u8> {
        match self {
            Self::Fast => Some(FAST_CLOCKS),
            Self::WaitStates(wait) => Some(ZERO_WAIT_CLOCKS + wait),
            Self::External => None,
            Self::Eclk => Some((eclk_divisor + eclk_divisor / 2) as u8),
        }
    }

    /// The clocks of a cycle an external DSACK ends after `wait`
    /// states.
    #[must_use]
    pub const fn external_clocks(wait: u8) -> u8 {
        ZERO_WAIT_CLOCKS.saturating_add(wait)
    }
}

/// A select a cycle matches.
///
/// Not part of the semver API: plumbing `mc683xx-mc68331` assembles the chip with.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Match {
    /// The select.
    pub select: ChipSelect,
    /// Whether its pin is assigned as a chip select and so asserted;
    /// otherwise its logic still ends the cycle (SIMRM 7.3).
    pub asserted: bool,
    /// Its port's width.
    pub port: PortSize,
    /// The lanes of the cycle it takes part on: an 8-bit port takes
    /// whatever the cycle carries.
    pub lanes: Lanes,
    /// How it would end the cycle.
    pub termination: Termination,
    /// Whether it autovectors an interrupt acknowledge (AVEC).
    pub autovector: bool,
}

/// A set of selects, bit `n` for the select of [`ChipSelect::index`] `n`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Selects(u16);

impl Selects {
    /// All twelve.
    pub const ALL: Self = Self((1 << SELECTS) - 1);

    /// Whether it holds the select of index `number`.
    #[must_use]
    pub const fn contains(self, number: usize) -> bool {
        self.0 & 1 << number != 0
    }
}

/// A select's base address and option registers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
struct Registers {
    /// CSBARx.
    base: u16,
    /// CSORx.
    option: u16,
}

impl Registers {
    /// An option register field.
    const fn field(self, shift: u16) -> u16 {
        self.option >> shift & CSOR_FIELD_MASK
    }

    /// The block's first address.
    const fn start(self) -> u32 {
        ((self.base & CSBAR_BASE_MASK) as u32) << CSBAR_BASE_SHIFT
    }

    /// The block's bytes.
    const fn length(self) -> u32 {
        BLOCK_BYTES[(self.base & CSBAR_BLKSZ_MASK) as usize]
    }
}

/// The chip-select registers.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChipSelects {
    /// CSPAR0 and CSPAR1.
    pins: [u16; 2],
    /// CSBOOT's registers, then CS0 to CS10's.
    selects: [Registers; SELECTS],
}

impl ChipSelects {
    /// The registers out of reset, with `data` the levels of DATA\[15:0\]
    /// as the reset releases (SIMRM 7.9).
    #[must_use]
    pub fn new(data: u16) -> Self {
        let high = |pin: u32| u16::from(data & 1 << pin != 0);
        let mut cspar0 = CSPAR0_CSBOOT_SELECT | high(CSBOOT_DATA);
        for (field, &pin) in CSPAR0_DATA.iter().enumerate() {
            cspar0 |= (high(pin) << 1 | 1) << (PIN_FIELD_BITS * (field as u16 + 1));
        }
        let mut cspar1 = 0;
        for field in 0..=(CSPAR1_LAST_DATA - CSPAR1_FIRST_DATA) {
            // a pin is a chip select only if its data pin and every
            // higher one in DATA[7:3] are high (SIMRM Table 7-7)
            let select = (CSPAR1_FIRST_DATA + field..=CSPAR1_LAST_DATA).all(|pin| high(pin) != 0);
            cspar1 |= (u16::from(select) << 1 | 1) << (PIN_FIELD_BITS * field as u16);
        }
        let mut selects = [Registers::default(); SELECTS];
        selects[0] = Registers {
            base: CSBARBT_RESET,
            option: CSORBT_RESET,
        };
        Self {
            pins: [cspar0, cspar1],
            selects,
        }
    }

    /// CSPAR`register`, 0 or 1.
    #[must_use]
    pub const fn pin_assignment(&self, register: usize) -> u16 {
        self.pins[register]
    }

    /// CSPAR`register` written.
    pub(crate) const fn write_pin_assignment(&mut self, register: usize, value: u16) {
        self.pins[register] = if register == 0 {
            value & CSPAR0_IMPLEMENTED | CSPAR0_CSBOOT_SELECT
        } else {
            value & CSPAR1_IMPLEMENTED
        };
    }

    /// `select`'s base address register.
    #[must_use]
    pub const fn base(&self, select: ChipSelect) -> u16 {
        self.selects[select.index()].base
    }

    /// `select`'s option register.
    #[must_use]
    pub const fn option(&self, select: ChipSelect) -> u16 {
        self.selects[select.index()].option
    }

    /// `select`'s base address register written.
    pub(crate) const fn write_base(&mut self, select: ChipSelect, value: u16) {
        self.selects[select.index()].base = value;
    }

    /// `select`'s option register written.
    pub(crate) const fn write_option(&mut self, select: ChipSelect, value: u16) {
        self.selects[select.index()].option = value;
    }

    /// `select`'s pin assignment field.
    const fn pin(&self, select: usize) -> u16 {
        let (register, field) = if select < CSPAR0_SELECTS {
            (0, select)
        } else {
            (1, select - CSPAR0_SELECTS)
        };
        self.pins[register] >> (PIN_FIELD_BITS * field as u16) & PIN_FIELD_MASK
    }

    /// The port C pins assigned as discrete outputs, PC0 to PC6 on CS3
    /// to CS9 (MC68331UM 4.8.1.1, 4.8.1.4), a mask.
    #[must_use]
    pub fn discrete_outputs(&self) -> u8 {
        (0..PORTC_PINS)
            .filter(|&pin| self.pin(PORTC_FIRST_SELECT + pin) == PIN_DISCRETE)
            .fold(0, |mask, pin| mask | 1 << pin)
    }

    /// The block of each enabled select outside CPU space: its first
    /// address and its length.
    pub fn blocks(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.selects
            .iter()
            .filter(|registers| {
                registers.field(CSOR_BYTE_SHIFT) != 0
                    && registers.field(CSOR_SPACE_SHIFT) != SPACE_CPU
            })
            .map(|registers| {
                let length = registers.length();
                (registers.start() & !(length - 1) & ADDRESS_MASK, length)
            })
    }

    /// The selects `request` matches, CSBOOT first then CS0 to CS10
    /// (SIMRM 7.4, Figure 7-2).
    pub fn matches(&self, request: DecodeRequest) -> impl Iterator<Item = Match> + '_ {
        self.matches_among(request, Selects::ALL)
    }

    /// The selects among `candidates` that `request` matches, in
    /// `matches`' order: with `candidates` the selects whose blocks
    /// hold the cycle's address (`at`), the same as `matches`.
    pub fn matches_among(
        &self,
        request: DecodeRequest,
        candidates: Selects,
    ) -> impl Iterator<Item = Match> + '_ {
        self.selects
            .iter()
            .enumerate()
            .filter(move |&(number, _)| candidates.contains(number))
            .filter_map(move |(number, &registers)| self.matching(number, registers, request))
    }

    /// The selects whose blocks hold `address`, enabled or not.  A
    /// block is at least 2 Kbytes and aligned to its size, so this is
    /// the same for every address of an aligned 2-Kbyte page.
    #[must_use]
    pub fn at(&self, address: u32) -> Selects {
        let bits = self
            .selects
            .iter()
            .enumerate()
            .filter(|(_, registers)| {
                let mask = !(registers.length() - 1) & ADDRESS_MASK;
                (address ^ registers.start()) & mask == 0
            })
            .fold(0, |bits, (number, _)| bits | 1 << number);
        Selects(bits)
    }

    /// Whether select `number` matches `request`, and how.
    fn matching(
        &self,
        number: usize,
        registers: Registers,
        request: DecodeRequest,
    ) -> Option<Match> {
        let byte = registers.field(CSOR_BYTE_SHIFT);
        if byte == 0 {
            return None;
        }
        let space = registers.field(CSOR_SPACE_SHIFT);
        let space_matches = if request.function_code.is_cpu_space() {
            space == SPACE_CPU
        } else if request.function_code.is_supervisor() {
            space & SPACE_SUPERVISOR != 0
        } else {
            space & SPACE_USER != 0
        };
        let mask = !(registers.length() - 1) & ADDRESS_MASK;
        if !space_matches || (request.address ^ registers.start()) & mask != 0 {
            return None;
        }
        let acknowledge = request.acknowledge_level();
        if let Some(level) = acknowledge {
            let ipl = (registers.option >> CSOR_IPL_SHIFT & CSOR_IPL_MASK) as u8;
            if ipl != 0 && ipl != level {
                return None;
            }
        }
        let read_write = registers.field(CSOR_RW_SHIFT);
        let wanted = if request.write { RW_WRITE } else { RW_READ };
        if read_write & wanted == 0 {
            return None;
        }
        let pin = self.pin(number);
        let asserted = pin == PIN_8_BIT || pin == PIN_16_BIT;
        let (port, lanes) = if pin == PIN_8_BIT {
            (PortSize::Bits8, request.lanes())
        } else {
            let selected = Lanes {
                upper: byte & BYTE_UPPER != 0,
                lower: byte & BYTE_LOWER != 0,
            };
            (PortSize::Bits16, selected.and(request.lanes()))
        };
        if lanes.is_empty() {
            return None;
        }
        let autovector = acknowledge.is_some() && registers.option & CSOR_AVEC != 0;
        let dsack = registers.option >> CSOR_DSACK_SHIFT & CSOR_DSACK_MASK;
        let termination = if registers.option & CSOR_MODE != 0 {
            Termination::Eclk
        } else if autovector || dsack == DSACK_FAST {
            Termination::Fast
        } else if dsack == DSACK_EXTERNAL {
            Termination::External
        } else {
            Termination::WaitStates(dsack as u8)
        };
        Some(Match {
            select: ChipSelect::from_index(number).expect("one of twelve"),
            asserted,
            port,
            lanes,
            termination,
            autovector,
        })
    }
}

/// The DSACK pins a peripheral drives on an externally terminated select.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dsack {
    /// DSACK0: an 8-bit port (SIMRM Table 5-2).
    Dsack0,
    /// DSACK1: a 16-bit port.
    Dsack1,
    /// Both: reserved, taken as a 16-bit port.
    Both,
}

impl Dsack {
    /// Whether it drives DSACK0.
    #[must_use]
    pub const fn drives_dsack0(self) -> bool {
        matches!(self, Self::Dsack0 | Self::Both)
    }

    /// Whether it drives DSACK1.
    #[must_use]
    pub const fn drives_dsack1(self) -> bool {
        matches!(self, Self::Dsack1 | Self::Both)
    }
}

#[cfg(test)]
mod tests {
    use super::ChipSelects;
    use super::DecodeRequest;
    use super::Lanes;
    use super::Termination;
    use crate::chip_select::ChipSelect;
    use crate::chip_select::Number;
    use mc683xx_cpu32::bus::PortSize;
    use mc683xx_cpu32::function_code::FunctionCode;
    use mc683xx_cpu32::size::Size;

    /// DATA\[15:0\] left high by the pull-ups.
    const PULLED_UP: u16 = 0xffff;

    fn cs(number: u8) -> ChipSelect {
        ChipSelect::Numbered(Number::new(number).unwrap())
    }

    fn read(address: u32, size: Size) -> DecodeRequest {
        DecodeRequest {
            address,
            function_code: FunctionCode::SUPERVISOR_PROGRAM,
            write: false,
            size,
        }
    }

    fn selected(selects: &ChipSelects, request: DecodeRequest) -> Vec<ChipSelect> {
        selects.matches(request).map(|found| found.select).collect()
    }

    #[test]
    fn reset_pins_follow_the_data_bus() {
        let selects = ChipSelects::new(PULLED_UP);
        assert_eq!(selects.pin_assignment(0), 0x3fff);
        assert_eq!(selects.pin_assignment(1), 0x03ff);
        // DATA0 low: an 8-bit boot ROM; DATA5 low: CS8 to CS6 address
        // lines (SIMRM Tables 7-6, 7-7)
        let selects = ChipSelects::new(!0b10_0001);
        assert_eq!(selects.pin_assignment(0), 0x3ffe);
        assert_eq!(selects.pin_assignment(1), 0x03d5);
    }

    #[test]
    fn csboot_covers_the_first_megabyte_at_reset() {
        let selects = ChipSelects::new(PULLED_UP);
        let found: Vec<_> = selects.matches(read(0, Size::Word)).collect();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].select, ChipSelect::Boot);
        assert_eq!(found[0].port, PortSize::Bits16);
        assert_eq!(found[0].termination, Termination::WaitStates(13));
        assert_eq!(found[0].termination.clocks(8), Some(16));
        assert!(selected(&selects, read(0xf_fffe, Size::Word)).len() == 1);
        assert!(selected(&selects, read(0x10_0000, Size::Word)).is_empty());
        let user = DecodeRequest {
            function_code: FunctionCode::USER_DATA,
            ..read(0x100, Size::Byte)
        };
        assert_eq!(selected(&selects, user), [ChipSelect::Boot]);
        let cpu = DecodeRequest {
            function_code: FunctionCode::CPU_SPACE,
            ..read(0x100, Size::Byte)
        };
        assert!(selected(&selects, cpu).is_empty());
    }

    #[test]
    fn byte_lane_selects_split_a_word() {
        let mut selects = ChipSelects::new(PULLED_UP);
        // RAM on three selects: CS0 reads, CS2 the upper byte's writes,
        // CS3 the lower's
        for (number, option) in [(0, 0x6830), (2, 0x5030), (3, 0x3030)] {
            selects.write_base(cs(number), 0x1003);
            selects.write_option(cs(number), option);
        }
        let write = |address, size| DecodeRequest {
            write: true,
            ..read(address, size)
        };
        assert_eq!(selected(&selects, read(0x10_0000, Size::Word)), [cs(0)]);
        assert_eq!(
            selected(&selects, write(0x10_0000, Size::Word)),
            [cs(2), cs(3)]
        );
        assert_eq!(selected(&selects, write(0x10_0000, Size::Byte)), [cs(2)]);
        assert_eq!(selected(&selects, write(0x10_ffff, Size::Byte)), [cs(3)]);
        assert!(selected(&selects, write(0x11_0000, Size::Byte)).is_empty());
        let lanes: Vec<_> = selects
            .matches(write(0x10_0000, Size::Word))
            .map(|found| found.lanes)
            .collect();
        assert_eq!(
            lanes,
            [
                Lanes {
                    upper: true,
                    lower: false
                },
                Lanes {
                    upper: false,
                    lower: true
                }
            ]
        );
    }

    #[test]
    fn an_8_bit_port_takes_any_byte() {
        let mut selects = ChipSelects::new(PULLED_UP);
        // CS5 an 8-bit port (CSPAR0 field 6 %10)
        selects.write_pin_assignment(0, 0x2fff);
        selects.write_base(cs(5), 0x3003);
        selects.write_option(cs(5), 0x7b30);
        let found: Vec<_> = selects.matches(read(0x30_2001, Size::Byte)).collect();
        assert_eq!(found[0].select, cs(5));
        assert_eq!(found[0].port, PortSize::Bits8);
        assert_eq!(found[0].termination, Termination::WaitStates(12));
        assert_eq!(found[0].termination.clocks(8), Some(15));
    }

    #[test]
    fn an_acknowledge_select_matches_its_level() {
        let mut selects = ChipSelects::new(PULLED_UP);
        // CS10 for acknowledges: CPU space, IPL 2, the lower byte,
        // external
        selects.write_base(cs(10), 0xfff8);
        selects.write_option(cs(10), 0x2bc4);
        let acknowledge = |level: u32| DecodeRequest {
            function_code: FunctionCode::CPU_SPACE,
            ..read(0xff_fff1 | level << 1, Size::Byte)
        };
        let found: Vec<_> = selects.matches(acknowledge(2)).collect();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].termination, Termination::External);
        assert!(!found[0].autovector);
        assert!(selects.matches(acknowledge(3)).next().is_none());
        // AVEC at any level: fast termination
        selects.write_option(cs(10), 0x7801);
        let found: Vec<_> = selects.matches(acknowledge(5)).collect();
        assert!(found[0].autovector);
        assert_eq!(found[0].termination, Termination::Fast);
    }

    #[test]
    fn a_pin_given_another_function_still_decodes() {
        let mut selects = ChipSelects::new(PULLED_UP);
        // CS3 as FC0 (%01)
        selects.write_pin_assignment(0, 0x3dff);
        selects.write_base(cs(3), 0x5000);
        selects.write_option(cs(3), 0x7bb0);
        let found: Vec<_> = selects.matches(read(0x50_0000, Size::Word)).collect();
        assert_eq!(found[0].select, cs(3));
        assert!(!found[0].asserted);
        assert_eq!(found[0].termination, Termination::Fast);
    }

    #[test]
    fn space_and_direction_must_match() {
        let mut selects = ChipSelects::new(PULLED_UP);
        selects.write_base(cs(1), 0x2000);
        // read only, user space only
        selects.write_option(cs(1), 0x6810);
        assert!(selected(&selects, read(0x20_0000, Size::Word)).is_empty());
        let user = DecodeRequest {
            function_code: FunctionCode::USER_DATA,
            ..read(0x20_0000, Size::Word)
        };
        assert_eq!(selected(&selects, user), [cs(1)]);
        let write = DecodeRequest {
            write: true,
            ..user
        };
        assert!(selected(&selects, write).is_empty());
        // FC 3, reserved, is user space by FC2 (PLAN.md)
        let reserved = DecodeRequest {
            function_code: FunctionCode::new(3),
            ..user
        };
        assert_eq!(selected(&selects, reserved), [cs(1)]);
    }

    #[test]
    fn blocks_are_the_enabled_selects() {
        let mut selects = ChipSelects::new(PULLED_UP);
        selects.write_base(cs(4), 0x5000);
        selects.write_option(cs(4), 0x7830);
        let blocks: Vec<_> = selects.blocks().collect();
        assert_eq!(blocks, [(0, 1 << 20), (0x50_0000, 2 << 10)]);
    }

    #[test]
    fn the_selects_at_a_page_match_as_all_do() {
        let mut selects = ChipSelects::new(PULLED_UP);
        // CS4 a 2-Kbyte block at $500000, CS5 a 64-Kbyte one at $510000
        selects.write_base(cs(4), 0x5000);
        selects.write_option(cs(4), 0x7830);
        selects.write_base(cs(5), 0x5103);
        selects.write_option(cs(5), 0x7830);
        let at = selects.at(0x50_0000);
        assert!(at.contains(cs(4).index()) && !at.contains(cs(5).index()));
        for address in [
            0, 0x7_fffe, 0x50_0000, 0x50_07fe, 0x50_0800, 0x51_8000, 0xff_fff5,
        ] {
            for write in [false, true] {
                let request = DecodeRequest {
                    address,
                    function_code: FunctionCode::SUPERVISOR_DATA,
                    write,
                    size: Size::Word,
                };
                let among: Vec<_> = selects
                    .matches_among(request, selects.at(address))
                    .collect();
                let all: Vec<_> = selects.matches(request).collect();
                assert_eq!(among, all, "{address:#x}");
            }
        }
    }
}
