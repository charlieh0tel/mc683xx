//! The system integration module (SIMRM; MC68331UM 4): configuration
//! and protection, the clock and board time, the chip selects, the I/O
//! ports and the reset status.
//!
//! The chip that holds it routes its register cycles here, decodes the
//! bus from its chip selects, and arbitrates the interrupts it
//! requests.

pub mod chip_selects;
pub mod clock;
pub mod ports;
mod protection;
pub mod reset;

use std::mem;

use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::interrupt::InterruptLevel;
use mc683xx_cpu32::size::Size;

use self::chip_selects::ChipSelects;
use self::clock::ClockSource;
use self::clock::SystemClock;
use self::ports::Change;
use self::ports::PORTC_IMPLEMENTED;
use self::ports::PORTC_RESET;
use self::ports::Port;
use self::ports::PortName;
use self::protection::PeriodicTimer;
use self::protection::ProtectionControl;
use self::protection::Watchdog;
use crate::chip_select::ChipSelect;
use crate::module::Block;
use crate::module::RegisterAccess;
use crate::peripheral::Timed;
use crate::pins::PortLevels;

/// The module space's bytes, at $7FF000 or $FFF000 by MM (SIMRM 1.1).
pub const MODULE_SPACE_BYTES: u32 = 0x1000;
/// The module space's base with MM clear.
const MODULE_BASE_LOW: u32 = 0x7f_f000;
/// The module space's base with MM set.
const MODULE_BASE_HIGH: u32 = 0xff_f000;
/// The SIM's register block, $YFFA00 to $YFFA7F (MC68331UM Table D-1).
pub const BLOCK: Block = Block {
    offset: 0xa00,
    length: 0x80,
};

/// Register offsets in the block (MC68331UM Table D-3), each a word's
/// even address.
const SIMCR: u32 = 0x00;
/// SYNCR.
const SYNCR: u32 = 0x04;
/// RSR, the low byte.
const RSR: u32 = 0x06;
/// PORTE0, the low byte.
const PORTE0: u32 = 0x10;
/// PORTE1, the same latch.
const PORTE1: u32 = 0x12;
/// DDRE.
const DDRE: u32 = 0x14;
/// PEPAR.
const PEPAR: u32 = 0x16;
/// PORTF0.
const PORTF0: u32 = 0x18;
/// PORTF1, the same latch.
const PORTF1: u32 = 0x1a;
/// DDRF.
const DDRF: u32 = 0x1c;
/// PFPAR.
const PFPAR: u32 = 0x1e;
/// SYPCR, the low byte.
const SYPCR: u32 = 0x20;
/// PICR.
const PICR: u32 = 0x22;
/// PITR.
const PITR: u32 = 0x24;
/// SWSR, the low byte.
const SWSR: u32 = 0x26;
/// DREG, the last of the factory test registers from $30.
const DREG: u32 = 0x3a;
/// PORTC, the low byte.
const PORTC: u32 = 0x40;
/// CSPAR0; CSPAR1 follows.
const CSPAR0: u32 = 0x44;
/// CSPAR1.
const CSPAR1: u32 = 0x46;
/// CSBARBT; CSORBT follows, then CSBAR0, CSOR0 and so on to CSOR10.
const CSBARBT: u32 = 0x48;
/// The end of the chip-select registers.
const CHIP_SELECTS_END: u32 = 0x78;
/// Bytes per select: CSBARx and CSORx.
const SELECT_BYTES: u32 = 4;

/// The registers SUPV can open to user mode, "S/U" in MC68331UM Table
/// D-3; the rest are supervisor-only.
const UNRESTRICTED: [u32; 8] = [PORTE0, PORTE1, DDRE, PORTF0, PORTF1, DDRF, DREG, PORTC];

/// SIMCR (MC68331UM D.3.1): EXOFF, FRZSW, FRZBM, SHEN, SUPV and IARB,
/// which a write sets.
const SIMCR_WRITABLE: u16 = 0xe38f;
/// SIMCR SLVEN: factory test mode, DATA11 held low at reset.
const SIMCR_SLVEN: u16 = 1 << 11;
/// SIMCR SUPV: the "S/U" registers are supervisor-only.
const SIMCR_SUPV: u16 = 1 << 7;
/// SIMCR MM: the module space is at $FFF000; written once.
const SIMCR_MM: u16 = 1 << 6;
/// SIMCR IARB, bits 3:0.
const SIMCR_IARB_MASK: u16 = 0xf;
/// SIMCR out of reset but SLVEN: SUPV, MM, IARB $F (MC68331UM D.3.1).
/// SIMRM Table 8-4 also sets FRZSW and FRZBM; the 68331's manual
/// governs.
const SIMCR_RESET: u16 = 0x00cf;
/// DATA11, whose level at reset sets SLVEN (MC68331UM 4.2.4).
const SLVEN_DATA: u16 = 1 << 11;
/// DATA8, whose level at reset sets PEPAR (MC68331UM D.3.8).
const PEPAR_DATA: u16 = 1 << 8;
/// DATA9, whose level at reset sets PFPAR (MC68331UM D.3.11).
const PFPAR_DATA: u16 = 1 << 9;
/// PEPAR's AVEC bit, PE2 (MC68331UM Table D-4).
const PEPAR_AVEC: u8 = 1 << 2;

/// A reset's cause, as RSR records it (MC68331UM D.3.4).  The chip
/// raises neither LOC nor TST: its synthesizer never loses its
/// reference (PLAN.md, Clock synthesizer) and the test submodule is
/// factory test only; a host's reset scopes may name them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum ResetCause {
    /// RESET driven by the board: EXT.
    External,
    /// Power-on: POW.
    PowerOn,
    /// The software watchdog timed out: SW.
    Watchdog,
    /// The halt monitor, on a double bus fault: HLT.
    HaltMonitor,
    /// The `reset` instruction: SYS.
    System,
    /// Loss of the clock's reference: LOC.
    LossOfClock,
    /// The test submodule, in system test: TST.
    Test,
}

impl ResetCause {
    /// Every cause, in RSR's bit order, high to low.
    pub const ALL: [Self; 7] = [
        Self::External,
        Self::PowerOn,
        Self::Watchdog,
        Self::HaltMonitor,
        Self::LossOfClock,
        Self::System,
        Self::Test,
    ];

    /// Its RSR bit (MC68331UM D.3.4).
    #[must_use]
    pub const fn status(self) -> u8 {
        match self {
            Self::External => 1 << 7,
            Self::PowerOn => 1 << 6,
            Self::Watchdog => 1 << 5,
            Self::HaltMonitor => 1 << 4,
            Self::LossOfClock => 1 << 2,
            Self::System => 1 << 1,
            Self::Test => 1,
        }
    }
}

/// The pins the SIM reads as a reset releases (SIMRM 8.6; MC68331UM
/// 4.6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Configuration {
    /// DATA\[15:0\]'s levels; the weak pull-ups leave them high.
    pub data: u16,
    /// The clock source, by MODCLK.
    pub clock: ClockSource,
}

impl Default for Configuration {
    fn default() -> Self {
        Self {
            data: u16::MAX,
            clock: ClockSource::default(),
        }
    }
}

/// What a register write changed that the chip must act on.
///
/// Not part of the semver API: plumbing `mc683xx-mc68331` assembles the chip with.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Written {
    /// The address decode or its timing: a chip-select register, MM,
    /// the bus monitor in SYPCR, or ECLK's rate in SYNCR.
    pub decode: bool,
}

/// The SIM's state.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Sim {
    /// The reset pins.
    configuration: Configuration,
    /// SIMCR.
    simcr: u16,
    /// Whether MM's one write has happened.
    mm_written: bool,
    /// The clock synthesizer and board time.
    clock: SystemClock,
    /// RSR.
    reset_status: u8,
    /// Port E.
    port_e: Port,
    /// Port F.
    port_f: Port,
    /// PORTC.
    port_c: u8,
    /// SYPCR.
    protection: ProtectionControl,
    /// The software watchdog.
    watchdog: Watchdog,
    /// The periodic interrupt timer.
    timer: PeriodicTimer,
    /// The chip-select registers.
    chip_selects: ChipSelects,
    /// A reset the SIM asks for, not yet carried out.
    reset_request: Option<ResetCause>,
    /// The interrupt mask LPSTOP stored, while the MCU is stopped
    /// (MC68331UM 4.3.4).
    low_power: Option<u8>,
    /// What ports C, E and F drive, each change with its time, until
    /// the host takes them.
    output_changes: Vec<(Cycles, Change)>,
}

impl Sim {
    /// The SIM out of a power-on reset at `now`.
    #[must_use]
    pub fn new(configuration: Configuration, now: Cycles) -> Self {
        let modclk = configuration.clock.modclk();
        let protection = ProtectionControl::new(modclk);
        let clock = SystemClock::new(configuration.clock, now);
        let tick = clock.tick(now);
        Self {
            configuration,
            simcr: Self::simcr_reset(configuration),
            mm_written: false,
            clock,
            reset_status: ResetCause::PowerOn.status(),
            port_e: Port::new(Self::assignment(configuration, PEPAR_DATA)),
            port_f: Port::new(Self::assignment(configuration, PFPAR_DATA)),
            port_c: PORTC_RESET,
            protection,
            watchdog: Watchdog::new(protection, tick),
            timer: PeriodicTimer::new(modclk),
            chip_selects: ChipSelects::new(configuration.data),
            reset_request: None,
            low_power: None,
            output_changes: Vec::new(),
        }
    }

    /// SIMCR out of reset.
    const fn simcr_reset(configuration: Configuration) -> u16 {
        if configuration.data & SLVEN_DATA == 0 {
            SIMCR_RESET | SIMCR_SLVEN
        } else {
            SIMCR_RESET
        }
    }

    /// A port's pin assignment out of reset, from `data_pin`: all bus
    /// or interrupt functions if it is high.
    const fn assignment(configuration: Configuration, data_pin: u16) -> u8 {
        if configuration.data & data_pin != 0 {
            u8::MAX
        } else {
            0
        }
    }

    /// A reset at `now` for `cause`: every register to its reset value
    /// but the port latches (SIMRM 8.8), RSR to the cause.  For the
    /// `reset` instruction use `system_reset`.
    pub fn reset(&mut self, cause: ResetCause, now: Cycles) {
        let before = self.all_outputs();
        let output_changes = mem::take(&mut self.output_changes);
        let configuration = self.configuration;
        let port_e = self
            .port_e
            .reset(Self::assignment(configuration, PEPAR_DATA));
        let port_f = self
            .port_f
            .reset(Self::assignment(configuration, PFPAR_DATA));
        *self = Self {
            reset_status: cause.status(),
            port_e,
            port_f,
            output_changes,
            ..Self::new(configuration, now)
        };
        self.record_outputs(before, now);
    }

    /// Whether a snapshot's SIM, `self`, fits the chip whose SIM is
    /// `current`: the same reset configuration, and registers a run
    /// could leave (a multiplier SYNCR gives, implemented bits only).
    ///
    /// # Errors
    ///
    /// [`StateError::Mismatch`] for another reset configuration or
    /// clock source, [`StateError::Invalid`] for registers no run could
    /// leave.
    ///
    /// Not part of the semver API: plumbing `mc683xx-mc68331` assembles the chip with.
    #[doc(hidden)]
    pub fn check_snapshot(&self, current: &Self) -> Result<(), StateError> {
        if self.configuration != current.configuration
            || self.clock.source() != current.clock.source()
        {
            return Err(StateError::mismatch("SIM configuration"));
        }
        if !self.clock.is_valid() {
            return Err(StateError::invalid("SYNCR"));
        }
        if self.port_c & !PORTC_IMPLEMENTED != 0 || !self.timer.is_valid() {
            return Err(StateError::invalid("SIM registers"));
        }
        if self
            .low_power
            .is_some_and(|mask| InterruptLevel::new(mask).is_none())
        {
            return Err(StateError::invalid("LPSTOP's interrupt mask"));
        }
        Ok(())
    }

    /// The `reset` instruction: RSR to SYS; the SIM's configuration
    /// stays (SIMRM 8.2.6).
    pub const fn system_reset(&mut self) {
        self.reset_status = ResetCause::System.status();
    }

    /// A reset the SIM asks for (the watchdog's), taken.
    ///
    /// Not part of the semver API: plumbing `mc683xx-mc68331` assembles the chip with.
    #[doc(hidden)]
    pub const fn take_reset(&mut self) -> Option<ResetCause> {
        self.reset_request.take()
    }

    /// The clock and board time.
    #[must_use]
    pub const fn clock(&self) -> &SystemClock {
        &self.clock
    }

    /// The chip-select registers.
    #[must_use]
    pub const fn chip_selects(&self) -> &ChipSelects {
        &self.chip_selects
    }

    /// RSR.
    #[must_use]
    pub const fn reset_status(&self) -> u8 {
        self.reset_status
    }

    /// The module space's base, by MM.
    #[must_use]
    pub const fn module_base(&self) -> u32 {
        if self.simcr & SIMCR_MM != 0 {
            MODULE_BASE_HIGH
        } else {
            MODULE_BASE_LOW
        }
    }

    /// SIMCR's IARB: the SIM's arbitration priority, for the timer and
    /// the IRQ pins (SIMRM 6.3).
    #[must_use]
    pub const fn arbitration(&self) -> u8 {
        (self.simcr & SIMCR_IARB_MASK) as u8
    }

    /// The bus monitor's period for internal-to-external cycles, in
    /// system clocks, if BME enables it (SIMRM 3.2).
    #[must_use]
    pub const fn bus_monitor(&self) -> Option<u8> {
        self.protection.bus_monitor()
    }

    /// Whether the halt monitor resets the MCU on a double bus fault
    /// (SIMRM 3.3).
    #[must_use]
    pub const fn halt_monitor(&self) -> bool {
        self.protection.halt_monitor()
    }

    /// Port E.
    #[must_use]
    pub const fn port_e(&self) -> &Port {
        &self.port_e
    }

    /// Port F.
    #[must_use]
    pub const fn port_f(&self) -> &Port {
        &self.port_f
    }

    /// PORTC, the latch: the levels of the chip-select pins assigned as
    /// discrete outputs, PC6 to PC0 on CS9 to CS3 (SIMRM 7.10.4).
    /// `outputs(PortName::C)` gives the pins it drives.
    #[must_use]
    pub const fn port_c(&self) -> u8 {
        self.port_c
    }

    /// The pins `port` drives and their levels.
    #[must_use]
    pub fn outputs(&self, port: PortName) -> PortLevels {
        match port {
            PortName::C => {
                let driven = self.chip_selects.discrete_outputs();
                PortLevels {
                    driven,
                    levels: self.port_c & driven,
                }
            }
            PortName::E => self.port_e.outputs(),
            PortName::F => self.port_f.outputs(),
        }
    }

    /// What each port drives, in [`PortName::ALL`]'s order.
    fn all_outputs(&self) -> [PortLevels; PortName::ALL.len()] {
        PortName::ALL.map(|port| self.outputs(port))
    }

    /// Records the ports whose outputs differ from `before` as
    /// changing at `now`.
    fn record_outputs(&mut self, before: [PortLevels; PortName::ALL.len()], now: Cycles) {
        for (port, before) in PortName::ALL.into_iter().zip(before) {
            let outputs = self.outputs(port);
            if outputs != before {
                self.output_changes.push((now, Change { port, outputs }));
            }
        }
    }

    /// The changes in what ports C, E and F drive since the last call,
    /// each with its time: a firmware pulse shorter than a quantum
    /// is both its edges.
    pub fn take_output_changes(&mut self) -> Vec<(Cycles, Change)> {
        mem::take(&mut self.output_changes)
    }

    /// The board drives port E's pins to `pins`.
    pub const fn set_port_e_pins(&mut self, pins: u8) {
        self.port_e.pins = pins;
    }

    /// The board drives port F's pins to `pins`.
    pub const fn set_port_f_pins(&mut self, pins: u8) {
        self.port_f.pins = pins;
    }

    /// Whether IRQ`level`'s pin has its interrupt function (PFPAR,
    /// MC68331UM Table D-5).
    #[must_use]
    pub const fn interrupt_pin(&self, level: InterruptLevel) -> bool {
        level.get() != 0 && self.port_f.assignment & 1 << level.get() != 0
    }

    /// PEPAR: a set bit assigns its port E pin its bus control
    /// function, PE0 DSACK0 and PE1 DSACK1 among them (SIMRM 9.1,
    /// Table 9-1).
    #[must_use]
    pub const fn port_e_assignment(&self) -> u8 {
        self.port_e.assignment
    }

    /// Whether PE2 is the AVEC input (PEPAR, MC68331UM Table D-4).
    #[must_use]
    pub const fn autovector_pin(&self) -> bool {
        self.port_e.assignment & PEPAR_AVEC != 0
    }

    /// The periodic interrupt timer's request level, while one is
    /// pending.
    ///
    /// Not part of the semver API: plumbing `mc683xx-mc68331` assembles the chip with.
    #[doc(hidden)]
    #[must_use]
    pub fn timer_request(&self) -> Option<InterruptLevel> {
        self.timer.request()
    }

    /// The CPU took the timer's interrupt: PIV.
    ///
    /// Not part of the semver API: plumbing `mc683xx-mc68331` assembles the chip with.
    #[doc(hidden)]
    pub fn acknowledge_timer(&mut self) -> u8 {
        self.timer.acknowledge()
    }

    /// The LPSTOP broadcast cycle at `now`, carrying the interrupt mask
    /// (MC68331UM 4.5.4.2): the watchdog's clock stops (4.2.12).
    ///
    /// Not part of the semver API: plumbing `mc683xx-mc68331` assembles the chip with.
    #[doc(hidden)]
    pub fn low_power_stop(&mut self, mask: u8, now: Cycles) {
        self.low_power = Some(mask);
        self.watchdog.stop(self.clock.tick(now));
    }

    /// An interrupt or a reset ends low-power stop at `now`.
    ///
    /// Not part of the semver API: plumbing `mc683xx-mc68331` assembles the chip with.
    #[doc(hidden)]
    pub fn wake(&mut self, now: Cycles) {
        if self.low_power.take().is_some() {
            self.watchdog.start(self.clock.tick(now));
        }
    }

    /// The interrupt mask LPSTOP stored, while the MCU is in low-power
    /// stop.
    #[must_use]
    pub const fn low_power(&self) -> Option<u8> {
        self.low_power
    }

    /// Whether a cycle from user mode may reach the register at
    /// `offset`.
    fn unrestricted(&self, offset: u32) -> bool {
        self.simcr & SIMCR_SUPV == 0 && UNRESTRICTED.contains(&offset)
    }

    /// The chip select whose registers are at `offset`, from CSBARBT,
    /// and whether `offset` is its option register.
    fn select_at(offset: u32) -> (ChipSelect, bool) {
        let index = ((offset - CSBARBT) / SELECT_BYTES) as usize;
        let select = ChipSelect::from_index(index).expect("CSBARBT to CSOR10");
        (select, !(offset - CSBARBT).is_multiple_of(SELECT_BYTES))
    }

    /// The register word at the even `offset`.
    fn read_word(&self, offset: u32) -> u16 {
        match offset {
            SIMCR => self.simcr,
            SYNCR => self.clock.syncr(),
            RSR => self.reset_status.into(),
            PORTE0 | PORTE1 => self.port_e.data().into(),
            DDRE => self.port_e.direction.into(),
            PEPAR => self.port_e.assignment.into(),
            PORTF0 | PORTF1 => self.port_f.data().into(),
            DDRF => self.port_f.direction.into(),
            PFPAR => self.port_f.assignment.into(),
            SYPCR => self.protection.value.into(),
            PICR => self.timer.control,
            PITR => self.timer.timing,
            PORTC => self.port_c.into(),
            CSPAR0 => self.chip_selects.pin_assignment(0),
            CSPAR1 => self.chip_selects.pin_assignment(1),
            CSBARBT..CHIP_SELECTS_END => match Self::select_at(offset) {
                (select, false) => self.chip_selects.base(select),
                (select, true) => self.chip_selects.option(select),
            },
            // SWSR reads zero (MC68331UM D.3.15); the factory test
            // registers and the unused words read zero too
            _ => 0,
        }
    }

    /// The register word at the even `offset` written with `value`.
    fn write_word(&mut self, offset: u32, value: u16, now: Cycles) -> Written {
        let low = value as u8;
        let mut written = Written::default();
        match offset {
            SIMCR => {
                let mm = if self.mm_written {
                    self.simcr & SIMCR_MM
                } else {
                    self.mm_written = true;
                    value & SIMCR_MM
                };
                written.decode = mm != self.simcr & SIMCR_MM;
                self.simcr = value & SIMCR_WRITABLE | mm | self.simcr & SIMCR_SLVEN;
            }
            SYNCR => {
                // EDIV sets ECLK cycles' clocks
                self.clock.write_syncr(value, now);
                written.decode = true;
            }
            PORTE0 | PORTE1 => self.port_e.latch = low,
            DDRE => self.port_e.direction = low,
            PEPAR => self.port_e.assignment = low,
            PORTF0 | PORTF1 => self.port_f.latch = low,
            DDRF => self.port_f.direction = low,
            PFPAR => self.port_f.assignment = low,
            SYPCR if !self.protection.written => {
                let old = self.protection;
                self.protection = ProtectionControl {
                    value: low,
                    written: true,
                };
                self.watchdog
                    .control(old, self.protection, self.clock.tick(now));
                // the bus monitor bounds which cycles end well
                written.decode = true;
            }
            PICR => self.timer.write_control(value),
            PITR => self.timer.write_timing(value, self.clock.tick(now)),
            SWSR => self
                .watchdog
                .service(low, self.protection, self.clock.tick(now)),
            PORTC => self.port_c = low & PORTC_IMPLEMENTED,
            CSPAR0 | CSPAR1 => {
                self.chip_selects
                    .write_pin_assignment(((offset - CSPAR0) / 2) as usize, value);
                written.decode = true;
            }
            CSBARBT..CHIP_SELECTS_END => {
                match Self::select_at(offset) {
                    (select, false) => self.chip_selects.write_base(select, value),
                    (select, true) => self.chip_selects.write_option(select, value),
                }
                written.decode = true;
            }
            _ => {}
        }
        written
    }

    /// A read cycle of its registers, `None` if the SIM does not
    /// acknowledge it: from user mode, a supervisor-only register.
    /// Such a cycle goes on to the external bus, as the QSM's does
    /// (QSMRM 3.3; our reading for the SIM, [PLAN.md]).
    ///
    /// [PLAN.md]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md
    ///
    /// Not part of the semver API: plumbing `mc683xx-mc68331` assembles the chip with.
    #[doc(hidden)]
    pub fn read(&mut self, access: RegisterAccess) -> Option<u32> {
        self.peek(access)
    }

    /// What `read` would return, without a read: the SIM's reads have
    /// no side effects, so this is `read` itself, for a debugger's look.
    ///
    /// Not part of the semver API: plumbing `mc683xx-mc68331` assembles the chip with.
    #[doc(hidden)]
    #[must_use]
    pub fn peek(&self, access: RegisterAccess) -> Option<u32> {
        let offset = access.offset & !1;
        if !access.supervisor && !self.unrestricted(offset) {
            return None;
        }
        Some(access.read_value(self.read_word(offset)))
    }

    /// A write cycle of `value` to its registers, as `read`.  A byte
    /// to a word register's half keeps the other half.
    ///
    /// Not part of the semver API: plumbing `mc683xx-mc68331` assembles the chip with.
    #[doc(hidden)]
    pub fn write(&mut self, access: RegisterAccess, value: u32) -> Option<Written> {
        let offset = access.offset & !1;
        if !access.supervisor && !self.unrestricted(offset) {
            return None;
        }
        let word = access.merge(self.read_word(offset), value);
        // a byte to the unused high half of a byte register changes
        // nothing
        if access.upper() && access.size == Size::Byte && Self::byte_register(offset) {
            return Some(Written::default());
        }
        let before = self.all_outputs();
        let written = self.write_word(offset, word, access.now);
        self.record_outputs(before, access.now);
        Some(written)
    }

    /// Whether the register at `offset` is a byte, in the word's low
    /// half.
    const fn byte_register(offset: u32) -> bool {
        matches!(
            offset,
            RSR | PORTE0
                | PORTE1
                | DDRE
                | PEPAR
                | PORTF0
                | PORTF1
                | DDRF
                | PFPAR
                | SYPCR
                | SWSR
                | PORTC
        )
    }
}

impl Timed for Sim {
    fn next_event(&self) -> Option<Cycles> {
        [self.timer.end(), self.watchdog.deadline()]
            .into_iter()
            .flatten()
            .map(|tick| self.clock.at(tick))
            .min()
    }

    fn advance(&mut self, now: Cycles) {
        let tick = self.clock.tick(now);
        self.timer.advance(tick);
        if self
            .watchdog
            .deadline()
            .is_some_and(|deadline| deadline <= tick)
        {
            self.watchdog.expire();
            self.reset_request = Some(ResetCause::Watchdog);
        }
    }
}

#[cfg(test)]
mod tests;
